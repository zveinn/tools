//! Rendering: paint terminal grids, manager overlays, and the name
//! prompt as escape-sequence streams into any `Write` (a client socket
//! buffer on the server, stdout in tests).

use std::io::Write;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    queue,
    style::{Attribute, Color, Print, SetAttribute, SetBackgroundColor, SetForegroundColor},
    terminal::{BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate},
};

use libghostty_vt::{
    Terminal,
    render::{CellIterator, Dirty, RenderState, RowIterator},
    screen::CellWide,
    style::{RgbColor, StyleColor, Underline},
};

use std::collections::HashMap;

use crate::Result;
use crate::agent_status::{AgentActivity, letter_step};
use crate::model::{Layout, Rect, Session, SplitDir, split_rect};

/// One session drawn on the status bar.
pub struct BarSession {
    /// Index into the server's session vec when the session is running.
    /// Clicking the chip switches to it.
    pub index: Option<usize>,
    /// Pin slot when this chip is a configured session. Clicking a chip
    /// that is not running starts that pin.
    pub pin: Option<usize>,
    pub name: String,
    pub activity: Option<AgentActivity>,
    /// The session went working → idle and nobody is viewing it. Drawn
    /// with the `agent_color_highlight` background instead of a dim chip.
    pub finished: bool,
}

/// What a click on the status bar landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarHit {
    /// A running session.
    Session(usize),
    /// A pinned session that is not running.
    Pin(usize),
    /// A tab of the session on screen.
    Tab(usize),
}

/// Where the status bar and the pane area sit on one client's screen.
///
/// `content` is the pane area's size (origin is always the pane area's
/// own top-left; `content_y` is where that lands on the screen). The
/// bar is a contiguous run of screen rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    pub content: (u16, u16),
    pub content_y: u16,
    /// First screen row of the bar, when it is shown.
    pub bar_row: Option<u16>,
    pub bar_rows: u16,
}

/// Screen geometry for one client.
///
/// The status bar takes `bar_rows` once the terminal is at least two
/// rows tall, and always leaves one row for the panes. A bar on top
/// shifts the pane area down. `bar_rows` is the wrapped height the
/// layout wants; this only places that block and caps it.
pub fn chrome(size: (u16, u16), bar_top: bool, bar_rows: u16) -> Chrome {
    let (cols, rows) = size;
    let n = if rows < 2 { 0 } else { bar_rows.min(rows - 1) };
    if n == 0 {
        return Chrome {
            content: size,
            content_y: 0,
            bar_row: None,
            bar_rows: 0,
        };
    }
    if bar_top {
        Chrome {
            content: (cols, rows - n),
            content_y: n,
            bar_row: Some(0),
            bar_rows: n,
        }
    } else {
        Chrome {
            content: (cols, rows - n),
            content_y: 0,
            bar_row: Some(rows - n),
            bar_rows: n,
        }
    }
}

/// The pane area of a client screen: everything except `bar_rows` of
/// status bar. Sessions are laid out and shells sized to this, so it
/// must be used for split and navigation geometry too. Which edge the
/// bar sits on changes the pane origin, not this size.
pub fn content_size(size: (u16, u16), bar_rows: u16) -> (u16, u16) {
    chrome(size, false, bar_rows).content
}

/// Truncate to `max` display characters, ellipsized.
fn fit(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Draw one frame of the viewed session: the active tab's panes at their
/// rectangles, dim divider lines between them, the status bar, and the
/// focused pane's cursor.
///
/// `full` repaints every cell instead of only the rows the panes marked
/// dirty. The caller sets it when something outside the panes owns what
/// is on the client's screen — an overlay that cleared it, a resize —
/// because the panes' dirty state says nothing about that.
pub fn draw_session(
    renderer: &mut Renderer<'static>,
    session: &Session,
    bar_sessions: &[BarSession],
    active: usize,
    out: &mut impl Write,
    size: (u16, u16),
    accent: Color,
    agent_color_highlight: Color,
    tick: usize,
    bar_top: bool,
    synchronized: bool,
    full: bool,
) -> Result<()> {
    let layout = bar_layout(bar_sessions, session, size.0, size.1);
    let screen = chrome(size, bar_top, layout.rows);
    let area = Rect {
        x: 0,
        y: screen.content_y,
        w: screen.content.0,
        h: screen.content.1,
    };
    let tab = &session.tabs[session.active_tab];
    if synchronized {
        queue!(out, BeginSynchronizedUpdate, Hide)?;
    } else {
        queue!(out, Hide)?;
    }
    // Wipe first on a full frame: the pane rects and the bar cover every
    // cell, but this way nothing an overlay left behind can survive a
    // gap. Inside the synchronized update, so it never shows.
    if full {
        queue!(out, SetAttribute(Attribute::Reset), Clear(ClearType::All))?;
    }

    let mut cursor = None;
    let mut focus_rect = None;
    let focused = tab.focused;
    if tab.zoomed
        && let Some(pane) = tab.layout.pane(focused)
    {
        // Fullscreen: only the focused pane, no dividers.
        cursor = renderer.draw_at(&pane.term, out, area, full)?;
    } else {
        tab.layout.for_each(area, &mut |pane, rect| {
            let pane_cursor = renderer.draw_at(&pane.term, out, rect, full)?;
            if pane.id == focused {
                cursor = pane_cursor;
                focus_rect = Some(rect);
            }
            Ok(())
        })?;
        draw_dividers(out, &tab.layout, area, focus_rect, accent)?;
    }

    if let Some(origin) = screen.bar_row {
        draw_bar(
            out,
            &layout,
            bar_sessions,
            active,
            session.active_tab,
            accent,
            agent_color_highlight,
            origin,
            tick,
        )?;
    }

    if let Some((x, y)) = cursor {
        queue!(out, MoveTo(x, y), Show)?;
    }
    if synchronized {
        queue!(out, EndSynchronizedUpdate)?;
    }
    out.flush()?;
    Ok(())
}

/// One chip after layout. `row` is relative to the top of the bar block.
struct Placed {
    kind: ChipKind,
    label: String,
    col: u16,
    row: u16,
}

#[derive(Clone, Copy)]
enum ChipKind {
    Session {
        index: Option<usize>,
        pin: Option<usize>,
    },
    Tab(usize),
}

#[derive(Clone)]
struct Chip {
    kind: ChipKind,
    label: String,
}

/// The status bar's chips. Drawing and click hit-testing share this, so
/// they cannot disagree about where a session or a tab is.
///
/// Sessions sit on the left and tabs on the right of one row when both
/// fit. When they would overlap, the tabs move up a line (and wrap
/// upward, right-aligned, if they still do not fit) and the sessions
/// stay below, left-aligned, wrapping onto further rows the same way.
/// A chip wider than the screen is ellipsized so it still occupies one row.
pub struct BarLayout {
    chips: Vec<Placed>,
    pub rows: u16,
}

pub fn bar_layout(
    items: &[BarSession],
    active: &Session,
    cols: u16,
    screen_rows: u16,
) -> BarLayout {
    let cols = cols as usize;
    let max_rows = if screen_rows < 2 {
        0
    } else {
        (screen_rows - 1) as usize
    };
    if cols == 0 || max_rows == 0 {
        return BarLayout {
            chips: Vec::new(),
            rows: 0,
        };
    }

    let sessions: Vec<Chip> = items
        .iter()
        .map(|item| Chip {
            kind: ChipKind::Session {
                index: item.index,
                pin: item.pin,
            },
            label: clamp_label(session_chip_label(item), cols),
        })
        .filter(|chip| !chip.label.is_empty())
        .collect();
    let tabs: Vec<Chip> = active
        .tabs
        .iter()
        .enumerate()
        .map(|(i, tab)| Chip {
            kind: ChipKind::Tab(i),
            label: clamp_label(tab_chip_label(tab), cols),
        })
        .filter(|chip| !chip.label.is_empty())
        .collect();

    let session_width = width_of(&sessions);
    let tab_width = width_of(&tabs);
    let chips = if session_width + tab_width <= cols {
        place_shared_row(&sessions, &tabs, cols)
    } else if max_rows == 1 {
        place_clipped_row(&sessions, &tabs, cols)
    } else {
        place_stacked(&sessions, &tabs, cols, max_rows)
    };
    let rows = chips
        .iter()
        .map(|chip| chip.row)
        .max()
        .map_or(0, |row| row + 1);
    BarLayout { chips, rows }
}

/// The session or tab whose chip covers `(row, x)` of the bar block.
pub fn bar_hit(
    items: &[BarSession],
    active: &Session,
    cols: u16,
    screen_rows: u16,
    row: u16,
    x: u16,
) -> Option<BarHit> {
    bar_layout(items, active, cols, screen_rows)
        .chips
        .into_iter()
        .find(|chip| {
            chip.row == row && x >= chip.col && (x - chip.col) < chip.label.chars().count() as u16
        })
        .and_then(|chip| match chip.kind {
            ChipKind::Tab(i) => Some(BarHit::Tab(i)),
            ChipKind::Session { index: Some(i), .. } => Some(BarHit::Session(i)),
            ChipKind::Session { pin: Some(p), .. } => Some(BarHit::Pin(p)),
            ChipKind::Session { .. } => None,
        })
}

/// How a run of text is styled. A working session name paints one
/// letter in the highlight color and then puts this style back.
#[derive(Clone, Copy)]
enum TextPaint {
    /// Foreground `color` with reverse video, so the chip background is
    /// that color.
    Reverse(Color),
    Dim,
    /// Plain foreground, with optional bold and dim. Manager rows.
    Fg {
        color: Color,
        bold: bool,
        dim: bool,
    },
}

fn apply_text_paint(out: &mut impl Write, paint: TextPaint) -> Result<()> {
    match paint {
        TextPaint::Reverse(color) => {
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                SetForegroundColor(color),
                SetAttribute(Attribute::Reverse),
            )?;
        }
        TextPaint::Dim => {
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                SetAttribute(Attribute::Dim),
            )?;
        }
        TextPaint::Fg { color, bold, dim } => {
            queue!(
                out,
                SetAttribute(Attribute::Reset),
                SetForegroundColor(color),
            )?;
            if bold {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if dim {
                queue!(out, SetAttribute(Attribute::Dim))?;
            }
        }
    }
    Ok(())
}

/// Print `text`. When `lit` is a character index, that one character is
/// drawn in `highlight` and the surrounding text keeps `paint`.
///
/// The lit cell keeps the chip's background. A reverse chip's background
/// is its foreground color, so the letter sets that color explicitly:
/// leaving reverse on would turn `highlight` into the background instead.
fn print_with_lit(
    out: &mut impl Write,
    text: &str,
    lit: Option<usize>,
    highlight: Color,
    paint: TextPaint,
) -> Result<()> {
    let Some(lit) = lit.filter(|i| text.chars().nth(*i).is_some()) else {
        queue!(out, Print(text))?;
        return Ok(());
    };
    let mut buf = String::new();
    for (i, ch) in text.chars().enumerate() {
        if i == lit {
            if !buf.is_empty() {
                queue!(out, Print(&buf))?;
                buf.clear();
            }
            queue!(out, SetAttribute(Attribute::Reset))?;
            if let TextPaint::Reverse(bg) = paint {
                queue!(out, SetBackgroundColor(bg))?;
            }
            queue!(out, SetForegroundColor(highlight), Print(ch))?;
            apply_text_paint(out, paint)?;
        } else {
            buf.push(ch);
        }
    }
    if !buf.is_empty() {
        queue!(out, Print(&buf))?;
    }
    Ok(())
}

/// Index of the session-name character to light inside a chip label.
/// The label is `" {name} "`, so the walk skips the padding and wraps
/// across whatever of the name is still visible. A clamped label can
/// lose the trailing space; that space is skipped only when it remains.
fn working_lit(label: &str, tick: usize) -> Option<usize> {
    let count = label.chars().count();
    let start = usize::from(label.starts_with(' '));
    let end = if label.ends_with(' ') {
        count.saturating_sub(1)
    } else {
        count
    };
    let len = end.saturating_sub(start);
    if len == 0 {
        None
    } else {
        Some(start + letter_step(tick) % len)
    }
}

/// Sessions on the left, tabs of the open session on the right. The
/// open session and the open tab are accent chips. A session whose
/// agent just finished a turn, and that is not the one on screen, uses
/// `highlight` as its background. A working session lights one letter
/// of its name in that same color, walking left to right.
fn draw_bar(
    out: &mut impl Write,
    layout: &BarLayout,
    items: &[BarSession],
    active_session: usize,
    active_tab: usize,
    accent: Color,
    highlight: Color,
    origin: u16,
    tick: usize,
) -> Result<()> {
    // Blank every row first. Chips are not always flush left, and a
    // shared row has a gap in the middle, so clearing only up to the
    // last chip would leave the previous frame in that gap.
    for row in 0..layout.rows {
        clear_bar_row(out, origin + row)?;
    }
    for chip in &layout.chips {
        queue!(out, MoveTo(chip.col, origin + chip.row))?;
        let (paint, working) = match chip.kind {
            ChipKind::Session { index, .. } => {
                let finished = index.is_some_and(|i| {
                    items.iter().any(|item| {
                        item.index == Some(i)
                            && item.finished
                            && item.activity == Some(AgentActivity::Idle)
                    })
                });
                let working = index.is_some_and(|i| {
                    items.iter().any(|item| {
                        item.index == Some(i) && item.activity == Some(AgentActivity::Working)
                    })
                });
                // The open session keeps the accent chip, so the bar
                // still shows where you are. A just-finished session
                // uses the same reverse trick — highlight foreground, so
                // the background is that color and the text stays the
                // terminal's own background.
                let paint = if index == Some(active_session) {
                    TextPaint::Reverse(accent)
                } else if finished {
                    TextPaint::Reverse(highlight)
                } else {
                    TextPaint::Dim
                };
                (paint, working)
            }
            ChipKind::Tab(i) => {
                let paint = if i == active_tab {
                    TextPaint::Reverse(accent)
                } else {
                    TextPaint::Dim
                };
                (paint, false)
            }
        };
        apply_text_paint(out, paint)?;
        let lit = working.then(|| working_lit(&chip.label, tick)).flatten();
        print_with_lit(out, &chip.label, lit, highlight, paint)?;
        queue!(out, SetAttribute(Attribute::Reset))?;
    }
    Ok(())
}

fn session_chip_label(item: &BarSession) -> String {
    format!(" {} ", item.name)
}

fn tab_chip_label(tab: &crate::model::Tab) -> String {
    // A fullscreened tab advertises it in its label.
    if tab.zoomed {
        format!(" {} [F] ", tab.name)
    } else {
        format!(" {} ", tab.name)
    }
}

fn clamp_label(label: String, cols: usize) -> String {
    if label.chars().count() <= cols {
        label
    } else {
        fit(&label, cols)
    }
}

fn width_of(chips: &[Chip]) -> usize {
    chips.iter().map(|chip| chip.label.chars().count()).sum()
}

/// Both groups fit on one row: sessions flush left, tabs flush right.
fn place_shared_row(sessions: &[Chip], tabs: &[Chip], cols: usize) -> Vec<Placed> {
    let mut chips = place_run(sessions, 0, 0);
    let tab_col = cols.saturating_sub(width_of(tabs)) as u16;
    chips.extend(place_run(tabs, tab_col, 0));
    chips
}

/// One row is all the screen will give. Keep chips from the front of
/// each group and drop the ones that would overlap.
fn place_clipped_row(sessions: &[Chip], tabs: &[Chip], cols: usize) -> Vec<Placed> {
    let sessions = take_fitting(sessions, cols);
    let tabs = take_fitting(tabs, cols.saturating_sub(width_of(&sessions)));
    let mut chips = place_run(&sessions, 0, 0);
    let tab_col = cols.saturating_sub(width_of(&tabs)) as u16;
    chips.extend(place_run(&tabs, tab_col, 0));
    chips
}

/// Tabs move up a line, right-aligned, and wrap upward if they still
/// do not fit. Sessions stay on the rows below, left-aligned.
fn place_stacked(sessions: &[Chip], tabs: &[Chip], cols: usize, max_rows: usize) -> Vec<Placed> {
    let tab_need = row_count(tabs, cols);
    let session_need = row_count(sessions, cols);
    let (tab_rows, session_rows) = allocate_rows(tab_need, session_need, max_rows);
    let mut chips = pack(tabs, cols, true, tab_rows, 0);
    let session_base = chips
        .iter()
        .map(|chip| chip.row)
        .max()
        .map_or(0, |row| row + 1);
    chips.extend(pack(sessions, cols, false, session_rows, session_base));
    chips
}

/// Split `max_rows` between the two sides. Each side that has chips
/// keeps a row when there are two or more rows to give.
fn allocate_rows(tab_need: usize, session_need: usize, max_rows: usize) -> (usize, usize) {
    if max_rows == 0 {
        return (0, 0);
    }
    if tab_need + session_need <= max_rows {
        return (tab_need, session_need);
    }
    if tab_need == 0 {
        return (0, session_need.min(max_rows));
    }
    if session_need == 0 {
        return (tab_need.min(max_rows), 0);
    }
    if max_rows == 1 {
        return (1, 0);
    }
    let mut tabs = 1;
    let mut sessions = 1;
    let mut extra = max_rows - 2;
    let tab_extra = tab_need.saturating_sub(1).min(extra);
    tabs += tab_extra;
    extra -= tab_extra;
    sessions += session_need.saturating_sub(1).min(extra);
    (tabs, sessions)
}

fn row_count(chips: &[Chip], cols: usize) -> usize {
    pack(chips, cols, false, usize::MAX, 0)
        .iter()
        .map(|chip| chip.row)
        .max()
        .map_or(0, |row| row as usize + 1)
}

/// `right` packs each wrapped row against the right edge. Sessions pass
/// `false` and stay on the left.
fn pack(chips: &[Chip], cols: usize, right: bool, max_rows: usize, row_base: u16) -> Vec<Placed> {
    if max_rows == 0 || cols == 0 {
        return Vec::new();
    }
    let mut placed = Vec::new();
    let mut current: Vec<Chip> = Vec::new();
    let mut used = 0usize;
    let mut rows = 0usize;
    for chip in chips {
        let w = chip.label.chars().count();
        if w == 0 || w > cols {
            continue;
        }
        if !current.is_empty() && used + w > cols {
            placed.extend(place_row(&current, cols, right, row_base + rows as u16));
            current.clear();
            used = 0;
            rows += 1;
            if rows >= max_rows {
                return placed;
            }
        }
        used += w;
        current.push(Chip {
            kind: chip.kind,
            label: chip.label.clone(),
        });
    }
    if !current.is_empty() && rows < max_rows {
        placed.extend(place_row(&current, cols, right, row_base + rows as u16));
    }
    placed
}

fn place_row(chips: &[Chip], cols: usize, right: bool, row: u16) -> Vec<Placed> {
    let start = if right {
        cols.saturating_sub(width_of(chips)) as u16
    } else {
        0
    };
    place_run(chips, start, row)
}

fn place_run(chips: &[Chip], start: u16, row: u16) -> Vec<Placed> {
    let mut x = start;
    let mut out = Vec::with_capacity(chips.len());
    for chip in chips {
        let w = chip.label.chars().count() as u16;
        out.push(Placed {
            kind: chip.kind,
            label: chip.label.clone(),
            col: x,
            row,
        });
        x += w;
    }
    out
}

fn take_fitting(chips: &[Chip], cols: usize) -> Vec<Chip> {
    let mut kept = Vec::new();
    let mut used = 0usize;
    for chip in chips {
        let w = chip.label.chars().count();
        if used + w > cols {
            break;
        }
        used += w;
        kept.push(Chip {
            kind: chip.kind,
            label: chip.label.clone(),
        });
    }
    kept
}

/// Blank a bar row from column 0. The cursor stays on that column.
fn clear_bar_row(out: &mut impl Write, row: u16) -> Result<()> {
    queue!(
        out,
        MoveTo(0, row),
        SetAttribute(Attribute::Reset),
        Clear(ClearType::UntilNewLine),
    )?;
    Ok(())
}

// Line-component bits for box-drawing junction resolution.
pub(crate) const B_UP: u8 = 1;
pub(crate) const B_DOWN: u8 = 2;
pub(crate) const B_LEFT: u8 = 4;
pub(crate) const B_RIGHT: u8 = 8;

/// Draw the divider lines of every split, resolving crossings and tees
/// (`┬ ┴ ├ ┤ ┼`) where dividers meet instead of overdrawing. Divider
/// cells that border the focused pane are drawn in the accent color so
/// the active terminal reads as framed.
fn draw_dividers(
    out: &mut impl Write,
    layout: &Layout,
    rect: Rect,
    focused: Option<Rect>,
    accent: Color,
) -> Result<()> {
    // (bits, real): `real` cells are on a divider line; hint-only cells
    // exist so a neighboring divider knows a line abuts it.
    let mut cells: HashMap<(u16, u16), (u8, bool)> = HashMap::new();
    collect_dividers(layout, rect, &mut cells);
    if cells.is_empty() {
        return Ok(());
    }

    // Dim pass for dividers away from the focused pane...
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Dim)
    )?;
    for (&(x, y), &(bits, real)) in &cells {
        if !real || focused.is_some_and(|f| touches(f, x, y)) {
            continue;
        }
        queue!(out, MoveTo(x, y), Print(box_char(bits)))?;
    }
    // ...then an accent pass for the ones framing it.
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(accent),
    )?;
    for (&(x, y), &(bits, real)) in &cells {
        if !real || !focused.is_some_and(|f| touches(f, x, y)) {
            continue;
        }
        queue!(out, MoveTo(x, y), Print(box_char(bits)))?;
    }

    // Focus arrows: one on every divider bordering the focused pane,
    // centered on the shared edge and pointing into the pane.
    if let Some(f) = focused {
        let sides: [(bool, Option<u16>, u16, u16, char); 4] = [
            (true, f.x.checked_sub(1), f.y, f.h, '▸'),  // left divider
            (true, Some(f.x + f.w), f.y, f.h, '◂'),     // right divider
            (false, f.y.checked_sub(1), f.x, f.w, '▾'), // top divider
            (false, Some(f.y + f.h), f.x, f.w, '▴'),    // bottom divider
        ];
        for (vertical, fixed, lo, len, glyph) in sides {
            let Some(fixed) = fixed else { continue };
            if let Some((x, y)) = arrow_cell(&cells, vertical, fixed, lo, len) {
                queue!(out, MoveTo(x, y), Print(glyph))?;
            }
        }
    }
    queue!(
        out,
        SetAttribute(Attribute::Reset),
        SetForegroundColor(Color::Reset)
    )?;
    Ok(())
}

/// The cell an arrow lands on for one side of the focused pane: the
/// center of the shared edge along the divider at `fixed`, nudged
/// sideways when the center is a junction (`┼ ├ ┬ …`), so junctions
/// stay readable. Returns `None` when the side has no plain divider
/// cell at all (e.g. it is a screen edge).
fn arrow_cell(
    cells: &HashMap<(u16, u16), (u8, bool)>,
    vertical: bool,
    fixed: u16,
    lo: u16,
    len: u16,
) -> Option<(u16, u16)> {
    if len == 0 {
        return None;
    }
    let plain = if vertical {
        B_UP | B_DOWN
    } else {
        B_LEFT | B_RIGHT
    };
    let pos = |v: u16| if vertical { (fixed, v) } else { (v, fixed) };
    let center = lo + (len - 1) / 2;
    // Center first, then nudge outward in both directions.
    let candidates = std::iter::once(center).chain((1..len).flat_map(|d| {
        let up = (center + d < lo + len).then_some(center + d);
        let down = center.checked_sub(d).filter(|v| *v >= lo);
        [up, down].into_iter().flatten()
    }));
    for v in candidates {
        if let Some(&(bits, real)) = cells.get(&pos(v)) {
            if real && bits == plain {
                return Some(pos(v));
            }
        }
    }
    None
}

/// Whether a divider cell lies on the one-cell ring around `f` — the
/// dividers that visually frame that pane (corners included).
fn touches(f: Rect, x: u16, y: u16) -> bool {
    let x_in = x + 1 >= f.x && x <= f.x + f.w;
    let y_in = y + 1 >= f.y && y <= f.y + f.h;
    let on_vertical = (x + 1 == f.x || x == f.x + f.w) && y_in;
    let on_horizontal = (y + 1 == f.y || y == f.y + f.h) && x_in;
    on_vertical || on_horizontal
}

pub(crate) fn collect_dividers(
    layout: &Layout,
    rect: Rect,
    cells: &mut HashMap<(u16, u16), (u8, bool)>,
) {
    let Layout::Split { dir, a, b } = layout else {
        return;
    };
    let (ra, rb) = split_rect(*dir, rect);
    match dir {
        SplitDir::Horizontal => {
            let y = rect.y + ra.h;
            for x in rect.x..rect.x + rect.w {
                let cell = cells.entry((x, y)).or_insert((0, false));
                cell.0 |= B_LEFT | B_RIGHT;
                cell.1 = true;
            }
            // Tell abutting vertical dividers a line arrives from the side.
            if rect.x > 0 {
                cells.entry((rect.x - 1, y)).or_insert((0, false)).0 |= B_RIGHT;
            }
            cells.entry((rect.x + rect.w, y)).or_insert((0, false)).0 |= B_LEFT;
        }
        SplitDir::Vertical => {
            let x = rect.x + ra.w;
            for y in rect.y..rect.y + rect.h {
                let cell = cells.entry((x, y)).or_insert((0, false));
                cell.0 |= B_UP | B_DOWN;
                cell.1 = true;
            }
            if rect.y > 0 {
                cells.entry((x, rect.y - 1)).or_insert((0, false)).0 |= B_DOWN;
            }
            cells.entry((x, rect.y + rect.h)).or_insert((0, false)).0 |= B_UP;
        }
    }
    collect_dividers(a, ra, cells);
    collect_dividers(b, rb, cells);
}

pub(crate) fn box_char(bits: u8) -> char {
    let (u, d, l, r) = (
        bits & B_UP != 0,
        bits & B_DOWN != 0,
        bits & B_LEFT != 0,
        bits & B_RIGHT != 0,
    );
    match (u, d, l, r) {
        (true, true, true, true) => '┼',
        (true, true, true, false) => '┤',
        (true, true, false, true) => '├',
        (true, false, true, true) => '┴',
        (false, true, true, true) => '┬',
        (true, true, _, _) => '│',
        _ => '─',
    }
}

/// One row of a manager panel.
pub struct ListItem {
    pub label: String,
    /// The currently open session/tab — marked with an accent dot.
    pub active: bool,
    /// Rendered dim (e.g. a pinned session that isn't running).
    pub dim: bool,
    /// Length of the session name at the start of `label` while its
    /// agent is working. The highlight walks those characters and wraps.
    /// `None` when this row is not a working session.
    pub working_chars: Option<usize>,
}

/// A centered, rounded panel geometry for the overlays.
struct Panel {
    x: u16,
    y: u16,
    /// Interior width (inside the borders, minus 1-cell side padding).
    iw: usize,
}

/// Draw the panel frame (rounded corners, dim border, bold inline title)
/// and `body_rows` blank interior rows, returning the geometry.
fn draw_panel(
    out: &mut impl Write,
    title: &str,
    body_rows: u16,
    min_interior: usize,
    size: (u16, u16),
) -> Result<Panel> {
    let need = (min_interior.max(title.chars().count() + 2) + 4) as u16;
    let w = need.clamp(24, size.0.saturating_sub(2).max(10));
    let h = body_rows + 2;
    let x = size.0.saturating_sub(w) / 2;
    let y = size.1.saturating_sub(h) / 2;
    let iw = w.saturating_sub(4) as usize;

    // Top border with the title inline: ╭─ title ────╮
    let title = fit(title, iw);
    let dash_count = (w as usize).saturating_sub(title.chars().count() + 5);
    queue!(
        out,
        MoveTo(x, y),
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Dim),
        Print("╭─ "),
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Bold),
        Print(&title),
        SetAttribute(Attribute::Reset),
        SetAttribute(Attribute::Dim),
        Print(format!(" {}╮", "─".repeat(dash_count))),
    )?;
    for row in 1..=body_rows {
        queue!(
            out,
            MoveTo(x, y + row),
            Print(format!("│{}│", " ".repeat(w as usize - 2))),
        )?;
    }
    queue!(
        out,
        MoveTo(x, y + body_rows + 1),
        Print(format!("╰{}╯", "─".repeat(w as usize - 2))),
        SetAttribute(Attribute::Reset),
    )?;
    Ok(Panel { x, y, iw })
}

/// Draw a manager overlay: a centered panel listing sessions or tabs,
/// with a `❯` selector, an accent dot on the open entry, and stopped
/// entries dimmed.
pub struct ManagerView<'a> {
    pub title: &'a str,
    /// Entries to show (already filtered when a search is active).
    pub items: &'a [ListItem],
    pub selected: usize,
    pub footer: &'a str,
    /// Active `/` query, drawn as a search bar in the panel's top row.
    pub search: Option<&'a str>,
    /// Caret position within that query, in characters.
    pub search_cursor: usize,
    /// Reserve space for at least this many rows / this interior width,
    /// so the panel doesn't jump around while a search filters it.
    pub min_rows: usize,
    pub min_interior: usize,
}

pub fn draw_manager(
    out: &mut impl Write,
    view: &ManagerView,
    size: (u16, u16),
    accent: Color,
    highlight: Color,
    tick: usize,
    clear: bool,
) -> Result<()> {
    let ManagerView {
        title,
        items,
        selected,
        footer,
        search,
        search_cursor,
        min_rows,
        min_interior,
    } = *view;
    queue!(
        out,
        BeginSynchronizedUpdate,
        Hide,
        SetAttribute(Attribute::Reset),
    )?;
    // A name-letter tick reprints the panel in place. The panel blanks
    // its own rows, and the text does not change width, so skipping the
    // screen clear keeps the menu from flashing on every frame.
    if clear {
        queue!(out, Clear(ClearType::All))?;
    }
    // Window the list if the screen is short.
    let max_shown = (size.1.saturating_sub(6) as usize).max(1);
    let offset = (selected + 1).saturating_sub(max_shown);
    let shown = &items[offset.min(items.len())..(offset + max_shown).min(items.len())];

    let min_interior = items
        .iter()
        .map(|i| i.label.chars().count() + 2)
        .chain([footer.chars().count(), min_interior])
        .max()
        .unwrap_or(0);
    // Filtering leaves blank rows instead of shrinking the panel.
    let list_rows = shown.len().max(min_rows.min(max_shown));
    let body_rows = list_rows as u16 + 3;
    let panel = draw_panel(out, title, body_rows, min_interior, size)?;

    // The search bar sits in the blank row under the title, with the
    // terminal's own cursor parked at the caret.
    let mut caret = None;
    if let Some(query) = search {
        let shown = fit(query, panel.iw.saturating_sub(3));
        queue!(
            out,
            MoveTo(panel.x + 2, panel.y + 1),
            SetForegroundColor(accent),
            Print("/"),
            SetForegroundColor(Color::Reset),
            Print(&shown),
            SetForegroundColor(Color::Reset),
        )?;
        caret = Some((
            panel.x + 3 + search_cursor.min(shown.chars().count()) as u16,
            panel.y + 1,
        ));
    }

    for (row, item) in shown.iter().enumerate() {
        let is_selected = offset + row == selected;
        queue!(out, MoveTo(panel.x + 2, panel.y + 2 + row as u16))?;
        if is_selected {
            queue!(
                out,
                SetForegroundColor(accent),
                Print("❯ "),
                SetAttribute(Attribute::Bold),
            )?;
        } else {
            queue!(out, Print("  "))?;
        }
        // The open session/tab is named in the accent color; the ❯
        // above marks where the cursor sits, so the two signals stay
        // independent. A working session lights one letter of its name.
        let paint = TextPaint::Fg {
            color: if item.active { accent } else { Color::Reset },
            bold: is_selected,
            dim: item.dim && !is_selected,
        };
        apply_text_paint(out, paint)?;
        let shown = fit(&item.label, panel.iw.saturating_sub(2));
        let lit = item
            .working_chars
            .filter(|n| *n > 0)
            .map(|n| letter_step(tick) % n);
        print_with_lit(out, &shown, lit, highlight, paint)?;
        queue!(
            out,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(Color::Reset),
        )?;
    }

    queue!(
        out,
        MoveTo(panel.x + 2, panel.y + body_rows),
        SetAttribute(Attribute::Dim),
        Print(fit(footer, panel.iw)),
        SetAttribute(Attribute::Reset),
    )?;
    if let Some((x, y)) = caret {
        queue!(out, MoveTo(x, y), Show)?;
    }
    queue!(out, EndSynchronizedUpdate)?;
    out.flush()?;
    Ok(())
}

/// Draw the name prompt for a new session/tab as a centered panel.
pub fn draw_naming(
    out: &mut impl Write,
    title: &str,
    name: &str,
    cursor: usize,
    size: (u16, u16),
    accent: Color,
    footer: &str,
) -> Result<()> {
    queue!(
        out,
        BeginSynchronizedUpdate,
        Hide,
        SetAttribute(Attribute::Reset),
        Clear(ClearType::All),
    )?;
    let min_interior = (name.chars().count() + 6).max(footer.chars().count());
    let panel = draw_panel(out, title, 4, min_interior, size)?;

    // Text longer than the field scrolls horizontally to keep the
    // cursor in view instead of being ellipsized.
    let chars: Vec<char> = name.chars().collect();
    let width = panel.iw.saturating_sub(2).max(1);
    let start = if chars.len() <= width {
        0
    } else {
        cursor
            .saturating_sub(width - 1)
            .min(chars.len().saturating_sub(width))
    };
    let visible: String = chars[start..(start + width).min(chars.len())]
        .iter()
        .collect();

    queue!(
        out,
        MoveTo(panel.x + 2, panel.y + 2),
        SetForegroundColor(accent),
        Print("❯ "),
        SetForegroundColor(Color::Reset),
        Print(&visible),
        MoveTo(panel.x + 2, panel.y + 4),
        SetAttribute(Attribute::Dim),
        Print(fit(footer, panel.iw)),
        SetAttribute(Attribute::Reset),
        // Put the terminal's own cursor where the caret is.
        MoveTo(
            panel.x + 4 + cursor.saturating_sub(start).min(width) as u16,
            panel.y + 2,
        ),
        Show,
        EndSynchronizedUpdate,
    )?;
    out.flush()?;
    Ok(())
}

pub struct Renderer<'alloc> {
    render_state: RenderState<'alloc>,
    row_it: RowIterator<'alloc>,
    cell_it: CellIterator<'alloc>,
}

/// The SGR state we last emitted, so we only send color/attribute
/// sequences when a cell actually differs from the previous one.
#[derive(PartialEq, Clone, Copy)]
struct Pen {
    fg: Color,
    bg: Color,
    bold: bool,
    italic: bool,
    underline: bool,
    reverse: bool,
}

/// Map a cell's color to what we emit: palette indices and unset
/// (default) colors pass through untouched, so the *host* terminal's
/// theme decides what they look like — only genuine truecolor cells
/// are sent as RGB. This is why xmux panes match the colors of the
/// terminal they run in.
fn style_color(c: StyleColor, default: Color) -> Color {
    match c {
        StyleColor::None => default,
        StyleColor::Palette(idx) => Color::AnsiValue(idx.0),
        StyleColor::Rgb(rgb) => color(rgb),
    }
}

impl<'alloc> Renderer<'alloc> {
    pub fn new() -> Result<Self> {
        Ok(Self {
            render_state: RenderState::new()?,
            row_it: RowIterator::new()?,
            cell_it: CellIterator::new()?,
        })
    }

    /// Draw one terminal's grid with its top-left at `rect`'s origin
    /// (the terminal is kept sized to the rect by `Tab::apply_layout`).
    /// Returns the coordinates of the terminal's cursor if visible.
    /// The caller wraps the frame in a synchronized update and flushes.
    /// `full` paints every row regardless of the terminal's dirty state.
    fn draw_at(
        &mut self,
        term: &Terminal<'alloc, '_>,
        out: &mut impl Write,
        rect: Rect,
        full: bool,
    ) -> Result<Option<(u16, u16)>> {
        // Snapshot the terminal state; everything below reads the snapshot.
        let snapshot = self.render_state.update(term)?;

        // Pane defaults: the host terminal's own defaults (SGR 39/49),
        // unless a program inside the pane overrode them via OSC 10/11.
        let default = Pen {
            fg: term.fg_color()?.map_or(Color::Reset, color),
            bg: term.bg_color()?.map_or(Color::Reset, color),
            bold: false,
            italic: false,
            underline: false,
            reverse: false,
        };
        let mut pen = default;

        queue!(
            out,
            SetAttribute(Attribute::Reset),
            SetForegroundColor(pen.fg),
            SetBackgroundColor(pen.bg),
        )?;

        let frame_dirty = if full { Dirty::Full } else { snapshot.dirty()? };
        if frame_dirty != Dirty::Clean {
            let mut row_it = self.row_it.update(&snapshot)?;
            let mut y: u16 = 0;
            let mut text = String::with_capacity(16);

            while let Some(row) = row_it.next() {
                let paint = frame_dirty == Dirty::Full || row.dirty()?;
                if !paint {
                    y += 1;
                    continue;
                }
                queue!(out, MoveTo(rect.x, rect.y + y))?;
                let sel = row.selection()?;
                let mut cell_it = self.cell_it.update(row)?;
                let mut col: u16 = 0;

                while let Some(cell) = cell_it.next() {
                    // A wide character already advanced the cursor two
                    // columns; printing anything for its spacer cell would
                    // clobber the glyph's right half.
                    let wide = cell.raw_cell()?.wide()?;
                    match wide {
                        CellWide::SpacerTail | CellWide::SpacerHead => {
                            col = col.saturating_add(1);
                            continue;
                        }
                        CellWide::Narrow | CellWide::Wide => {}
                    }

                    let mut next = default;
                    if cell.has_styling()? {
                        let style = cell.style()?;
                        next.fg = style_color(style.fg_color, default.fg);
                        next.bg = style_color(style.bg_color, default.bg);
                        next.bold = style.bold;
                        next.italic = style.italic;
                        next.underline = style.underline != Underline::None;
                        // Pass inverse through as an attribute instead of
                        // swapping colors ourselves: default fg/bg can't be
                        // swapped in SGR, and the host does it correctly.
                        next.reverse = style.inverse;
                    }
                    // Mouse selection highlight: one range query per row
                    // instead of a C call per cell.
                    if let Some(sel) = sel {
                        let last = if wide == CellWide::Wide {
                            col.saturating_add(1)
                        } else {
                            col
                        };
                        if col <= sel.end_x && last >= sel.start_x {
                            next.reverse = !next.reverse;
                        }
                    }

                    Self::apply_pen(out, &mut pen, next)?;

                    if cell.graphemes_len()? == 0 {
                        queue!(out, Print(' '))?;
                    } else {
                        cell.graphemes_utf8(&mut text)?;
                        queue!(out, Print(&text))?;
                    }
                    col = col.saturating_add(1);
                }

                row.set_dirty(false)?;
                y += 1;
            }
        }

        // Report where the cursor should sit for this terminal.
        let cursor = if snapshot.cursor_visible()? {
            snapshot
                .cursor_viewport()?
                .map(|vp| (rect.x + vp.x, rect.y + vp.y as u16))
        } else {
            None
        };

        snapshot.set_dirty(Dirty::Clean)?;
        Ok(cursor)
    }

    /// Emit the escape sequences needed to go from `pen` to `next`.
    fn apply_pen(out: &mut impl Write, pen: &mut Pen, next: Pen) -> Result<()> {
        if *pen == next {
            return Ok(());
        }

        // Attributes can only be cleared by a full reset, which also
        // clears colors, so re-emit everything in that case.
        let attrs_changed = (pen.bold, pen.italic, pen.underline, pen.reverse)
            != (next.bold, next.italic, next.underline, next.reverse);

        if attrs_changed {
            queue!(out, SetAttribute(Attribute::Reset))?;
            if next.bold {
                queue!(out, SetAttribute(Attribute::Bold))?;
            }
            if next.italic {
                queue!(out, SetAttribute(Attribute::Italic))?;
            }
            if next.underline {
                queue!(out, SetAttribute(Attribute::Underlined))?;
            }
            if next.reverse {
                queue!(out, SetAttribute(Attribute::Reverse))?;
            }
        }
        if attrs_changed || pen.fg != next.fg {
            queue!(out, SetForegroundColor(next.fg))?;
        }
        if attrs_changed || pen.bg != next.bg {
            queue!(out, SetBackgroundColor(next.bg))?;
        }

        *pen = next;
        Ok(())
    }
}

fn color(rgb: RgbColor) -> Color {
    Color::Rgb {
        r: rgb.r,
        g: rgb.g,
        b: rgb.b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libghostty_vt::TerminalOptions;

    /// Crossterm's color switch is process-global, and these tests run in
    /// parallel. The lock is held until the previous setting is restored,
    /// so one test cannot turn colors off while another is still drawing.
    fn colors_on() -> ColorLock {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let lock = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
        let prev = crossterm::style::Colored::ansi_color_disabled_memoized();
        crossterm::style::Colored::set_ansi_color_disabled(false);
        ColorLock { _lock: lock, prev }
    }

    struct ColorLock {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: bool,
    }

    impl Drop for ColorLock {
        fn drop(&mut self) {
            crossterm::style::Colored::set_ansi_color_disabled(self.prev);
        }
    }

    /// A pane that produced no output since the last frame reports
    /// nothing dirty, so an ordinary frame paints nothing at all. That is
    /// what left the manager's panel on screen after Esc: the overlay
    /// cleared the client's screen, and the frame that was supposed to
    /// restore the panes was a no-op. A full frame must paint anyway.
    #[test]
    fn a_full_frame_repaints_an_idle_terminal() {
        let mut term = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 3,
            max_scrollback: 0,
        })
        .unwrap();
        term.vt_write(b"hello");
        let mut renderer = Renderer::new().unwrap();
        let rect = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 3,
        };
        let mut paint = |full: bool| {
            let mut buf = Vec::new();
            renderer.draw_at(&term, &mut buf, rect, full).unwrap();
            String::from_utf8_lossy(&buf).contains("hello")
        };

        assert!(paint(false), "first frame paints the pane");
        assert!(!paint(false), "idle pane stays unpainted");
        assert!(paint(true), "a full frame repaints it");
    }

    fn chip(
        index: Option<usize>,
        pin: Option<usize>,
        name: &str,
        activity: Option<AgentActivity>,
        finished: bool,
    ) -> BarSession {
        BarSession {
            index,
            pin,
            name: name.to_string(),
            activity,
            finished,
        }
    }

    fn running(
        index: usize,
        name: &str,
        activity: Option<AgentActivity>,
        finished: bool,
    ) -> BarSession {
        chip(Some(index), None, name, activity, finished)
    }

    fn laid_out(items: &[BarSession], session: &Session, cols: u16, rows: u16) -> BarLayout {
        bar_layout(items, session, cols, rows)
    }

    fn hit(
        items: &[BarSession],
        session: &Session,
        cols: u16,
        rows: u16,
        row: u16,
        x: u16,
    ) -> Option<BarHit> {
        bar_hit(items, session, cols, rows, row, x)
    }

    fn view(name: &str, tabs: &[&str]) -> Session {
        Session {
            id: 1,
            name: name.to_string(),
            tabs: tabs
                .iter()
                .map(|name| crate::model::Tab {
                    name: name.to_string(),
                    layout: Layout::Empty,
                    focused: 0,
                    zoomed: false,
                })
                .collect(),
            active_tab: 0,
            agent: false,
            last_activity: std::time::Instant::now(),
            last_size: (80, 24),
            last_agent_activity: None,
            finished_unseen: false,
        }
    }

    fn labels_on(layout: &BarLayout, row: u16) -> Vec<(String, u16)> {
        layout
            .chips
            .iter()
            .filter(|chip| chip.row == row)
            .map(|chip| (chip.label.clone(), chip.col))
            .collect()
    }

    #[test]
    fn content_shrinks_by_the_bar_rows_and_keeps_one_pane_row() {
        assert_eq!(content_size((80, 24), 1), (80, 23));
        assert_eq!(content_size((80, 24), 3), (80, 21));
        // Two rows: the bar keeps one and the panes keep the other.
        assert_eq!(content_size((80, 2), 4), (80, 1));
        assert_eq!(content_size((80, 1), 1), (80, 1));
    }

    #[test]
    fn chrome_puts_the_bar_on_the_configured_edge() {
        let bottom = chrome((80, 24), false, 1);
        assert_eq!(bottom.bar_row, Some(23));
        assert_eq!(bottom.bar_rows, 1);
        assert_eq!(bottom.content, (80, 23));
        assert_eq!(bottom.content_y, 0);

        let top = chrome((80, 24), true, 1);
        assert_eq!(top.bar_row, Some(0));
        assert_eq!(top.bar_rows, 1);
        assert_eq!(top.content_y, 1);
        assert_eq!(top.content, (80, 23));

        // Wrapped bar: tabs occupy the upper row of the block, sessions
        // the lower one. On the bottom edge that puts sessions on the
        // last screen row; on the top edge, tabs are row 0.
        let stacked_bottom = chrome((80, 24), false, 2);
        assert_eq!(stacked_bottom.bar_row, Some(22));
        assert_eq!(stacked_bottom.content, (80, 22));
        assert_eq!(stacked_bottom.content_y, 0);

        let stacked_top = chrome((80, 24), true, 2);
        assert_eq!(stacked_top.bar_row, Some(0));
        assert_eq!(stacked_top.bar_rows, 2);
        assert_eq!(stacked_top.content_y, 2);
        assert_eq!(stacked_top.content, (80, 22));
    }

    #[test]
    fn shared_row_puts_sessions_left_and_tabs_right() {
        // " work " is 6 columns, " shell " is 7. Together they fit in 20.
        let items = vec![running(3, "work", None, false)];
        let session = view("work", &["shell"]);
        let layout = laid_out(&items, &session, 20, 24);
        assert_eq!(layout.rows, 1);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" work ".into(), 0), (" shell ".into(), 13)]
        );
        assert_eq!(
            hit(&items, &session, 20, 24, 0, 0),
            Some(BarHit::Session(3))
        );
        assert_eq!(
            hit(&items, &session, 20, 24, 0, 5),
            Some(BarHit::Session(3))
        );
        assert_eq!(hit(&items, &session, 20, 24, 0, 6), None);
        assert_eq!(hit(&items, &session, 20, 24, 0, 13), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 20, 24, 0, 19), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 20, 24, 0, 20), None);
    }

    #[test]
    fn overlap_stacks_tabs_above_sessions() {
        // " work " is 6 and " shell " is 7, which does not fit in 11. The
        // tab moves up and stays on the right (11 - 7 = 4); the session
        // stays on the left.
        let items = vec![running(3, "work", None, false)];
        let session = view("work", &["shell"]);
        let layout = laid_out(&items, &session, 11, 24);
        assert_eq!(layout.rows, 2);
        assert_eq!(labels_on(&layout, 0), vec![(" shell ".into(), 4)]);
        assert_eq!(labels_on(&layout, 1), vec![(" work ".into(), 0)]);
        assert_eq!(hit(&items, &session, 11, 24, 0, 3), None);
        assert_eq!(hit(&items, &session, 11, 24, 0, 4), Some(BarHit::Tab(0)));
        assert_eq!(
            hit(&items, &session, 11, 24, 1, 0),
            Some(BarHit::Session(3))
        );
    }

    #[test]
    fn sessions_and_tabs_wrap_onto_extra_rows() {
        // Session chips are 6 columns; tab chips are 7. Width 14 holds
        // two of either. Three sessions and three tabs cannot share a
        // row, so tabs wrap on top (2 rows) and sessions below (2 rows).
        let items = vec![
            running(0, "work", None, false),
            running(1, "meow", None, false),
            running(2, "abcd", None, false),
        ];
        let session = view("work", &["shell", "build", "tests"]);
        let layout = laid_out(&items, &session, 14, 24);
        assert_eq!(layout.rows, 4);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" shell ".into(), 0), (" build ".into(), 7)]
        );
        // The leftover tab row is right-aligned: 14 - 7 = 7.
        assert_eq!(labels_on(&layout, 1), vec![(" tests ".into(), 7)]);
        assert_eq!(
            labels_on(&layout, 2),
            vec![(" work ".into(), 0), (" meow ".into(), 6)]
        );
        assert_eq!(labels_on(&layout, 3), vec![(" abcd ".into(), 0)]);
        assert_eq!(hit(&items, &session, 14, 24, 1, 7), Some(BarHit::Tab(2)));
        assert_eq!(
            hit(&items, &session, 14, 24, 3, 0),
            Some(BarHit::Session(2))
        );
    }

    #[test]
    fn a_short_screen_keeps_one_row_of_each_side() {
        // Three sessions need two rows and three tabs need two, four in
        // all. A 3-row screen can spare two bar rows, one per side, so
        // the third chip of each side is dropped.
        let items = vec![
            running(0, "work", None, false),
            running(1, "meow", None, false),
            running(2, "abcd", None, false),
        ];
        let session = view("work", &["shell", "build", "tests"]);
        let layout = laid_out(&items, &session, 14, 3);
        assert_eq!(layout.rows, 2);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" shell ".into(), 0), (" build ".into(), 7)]
        );
        assert_eq!(
            labels_on(&layout, 1),
            vec![(" work ".into(), 0), (" meow ".into(), 6)]
        );
        assert_eq!(hit(&items, &session, 14, 3, 0, 0), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 14, 3, 1, 0), Some(BarHit::Session(0)));
    }

    #[test]
    fn one_row_budget_clips_instead_of_overlapping() {
        // " work " is 6, " shell " and " build " are 7. Width 16 holds the
        // session and the first tab (6 + 7) and drops the second tab.
        let items = vec![running(1, "work", None, false)];
        let session = view("work", &["shell", "build"]);
        let layout = laid_out(&items, &session, 16, 2);
        assert_eq!(layout.rows, 1);
        assert_eq!(
            labels_on(&layout, 0),
            vec![(" work ".into(), 0), (" shell ".into(), 9)]
        );
        assert_eq!(hit(&items, &session, 16, 2, 0, 9), Some(BarHit::Tab(0)));
        assert_eq!(hit(&items, &session, 16, 2, 0, 8), None);
    }

    #[test]
    fn a_chip_wider_than_the_screen_is_ellipsized() {
        let items = vec![running(0, "workspace", None, false)];
        let session = view("workspace", &["sh"]);
        // " workspace " is 11 columns. Width 5 keeps an ellipsized chip
        // on its own row, and the tab still gets the row above it,
        // right-aligned: " sh " is 4 columns, so it starts at column 1.
        let layout = laid_out(&items, &session, 5, 24);
        assert_eq!(layout.rows, 2);
        assert_eq!(labels_on(&layout, 0), vec![(" sh ".into(), 1)]);
        let sessions = labels_on(&layout, 1);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].1, 0);
        assert_eq!(sessions[0].0.chars().count(), 5);
        assert!(sessions[0].0.contains('…'), "{sessions:?}");
    }

    #[test]
    fn working_and_idle_sessions_keep_their_names() {
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(1, "notes", Some(AgentActivity::Idle), true),
        ];
        let session = view("work", &["shell"]);
        // " work " is 6 columns, " notes " is 7. The activity does not
        // change the label; the letter color is applied at paint time.
        let laid = laid_out(&items, &session, 40, 24);
        assert_eq!(laid.rows, 1);
        assert_eq!(
            labels_on(&laid, 0),
            vec![
                (" work ".into(), 0),
                (" notes ".into(), 6),
                (" shell ".into(), 33),
            ]
        );
    }

    #[test]
    fn a_stopped_pin_is_clickable_and_a_running_pin_switches() {
        let items = vec![
            chip(None, Some(2), "meow", None, false),
            chip(Some(4), Some(0), "work", None, false),
        ];
        let session = view("work", &["shell"]);
        assert_eq!(hit(&items, &session, 40, 24, 0, 0), Some(BarHit::Pin(2)));
        // " meow " is 6 columns, so the trailing space is still the pin
        // and the running session starts at 6.
        assert_eq!(hit(&items, &session, 40, 24, 0, 5), Some(BarHit::Pin(2)));
        assert_eq!(
            hit(&items, &session, 40, 24, 0, 6),
            Some(BarHit::Session(4))
        );
    }

    #[test]
    fn the_bar_draws_the_tabs_on_the_right_after_clearing_the_row() {
        let items = vec![running(0, "work", None, false)];
        let session = view("work", &["shell"]);
        // " shell " is 7 columns. Width 20 puts it at column 13:
        // MoveTo(13, 5) is ESC[6;14H.
        let layout = laid_out(&items, &session, 20, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &items,
            0,
            0,
            Color::Cyan,
            Color::Magenta,
            5,
            0,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        let clear = text.find("\u{1b}[K").expect(&text);
        let jump = text.find("\u{1b}[6;14H").expect(&text);
        let label = text.find(" shell ").expect(&text);
        assert!(clear < jump && jump < label, "{text:?}");
    }

    #[test]
    fn the_open_session_and_tab_use_the_accent_chip() {
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(2, "notes", Some(AgentActivity::Idle), false),
        ];
        let session = view("work", &["shell", "build"]);
        let layout = laid_out(&items, &session, 80, 24);
        let mut buf = Vec::new();
        draw_bar(
            &mut buf,
            &layout,
            &items,
            0,
            0,
            Color::Cyan,
            Color::Magenta,
            5,
            0,
        )
        .unwrap();
        let text = String::from_utf8(buf).unwrap();
        // Tick 0 lights 'w', so the rest of the name is still one run.
        assert!(text.contains("ork"), "{text:?}");
        assert!(text.contains(" notes"), "{text:?}");
        assert!(text.contains(" shell "), "{text:?}");
        assert!(text.contains(" build "), "{text:?}");
        let work = text.find("ork").unwrap();
        let notes = text.find(" notes").unwrap();
        let shell = text.find(" shell ").unwrap();
        let build = text.find(" build ").unwrap();
        assert!(text[..work].contains("\u{1b}[7m"), "{text:?}");
        assert!(text[work..notes].contains("\u{1b}[2m"), "{text:?}");
        assert!(text[notes..shell].contains("\u{1b}[7m"), "{text:?}");
        assert!(text[shell..build].contains("\u{1b}[2m"), "{text:?}");
    }

    #[test]
    fn a_finished_agent_uses_the_configured_background() {
        // NO_COLOR makes crossterm drop color sequences.
        let _colors = colors_on();
        let purple = Color::Rgb {
            r: 0xa8,
            g: 0x55,
            b: 0xf7,
        };
        let items = vec![
            running(0, "work", Some(AgentActivity::Working), false),
            running(2, "notes", Some(AgentActivity::Idle), true),
            running(4, "shell", Some(AgentActivity::Idle), false),
        ];
        let session = view("work", &["sh"]);
        let layout = laid_out(&items, &session, 80, 24);
        let mut buf = Vec::new();
        draw_bar(&mut buf, &layout, &items, 0, 0, Color::Cyan, purple, 5, 0).unwrap();
        let text = String::from_utf8(buf).unwrap();
        // Tick 0 lights 'w' of the open session. The finished and idle
        // names stay one run each.
        let work = text.find("ork").unwrap();
        let notes = text.find(" notes").unwrap();
        let shell = text.find(" shell").unwrap();
        let rgb = "38;2;168;85;247";
        // The open session stays an accent chip. Its lit letter is the
        // highlight color, so purple shows up before the rest of the name.
        assert!(text[..work].contains("\u{1b}[7m"), "{text:?}");
        assert!(text[..work].contains(rgb), "{text:?}");
        // A working → finished session is a purple reverse chip, not dim.
        // The style is applied immediately before the label.
        assert!(text[..notes].ends_with("\u{1b}[7m"), "{text:?}");
        assert!(text[work..notes].contains(rgb), "{text:?}");
        assert!(!text[work..notes].contains("\u{1b}[2m"), "{text:?}");
        // Idle that never finished a turn stays dim.
        assert!(text[notes..shell].contains("\u{1b}[2m"), "{text:?}");
        assert!(!text[notes..shell].contains(rgb), "{text:?}");

        // The session you are in keeps the accent chip even if its flag
        // is still set, so the bar still shows where you are.
        let open = vec![running(2, "notes", Some(AgentActivity::Idle), true)];
        let session = view("notes", &["sh"]);
        let layout = laid_out(&open, &session, 40, 24);
        let mut buf = Vec::new();
        draw_bar(&mut buf, &layout, &open, 2, 0, Color::Cyan, purple, 5, 0).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let notes = text.find(" notes").unwrap();
        assert!(text[..notes].contains("\u{1b}[7m"), "{text:?}");
        assert!(!text[..notes].contains(rgb), "{text:?}");
    }

    #[test]
    fn a_name_tick_reprints_the_manager_without_clearing_the_screen() {
        let _colors = colors_on();
        let purple = Color::Rgb {
            r: 0xa8,
            g: 0x55,
            b: 0xf7,
        };
        let items = vec![ListItem {
            label: "build".into(),
            active: false,
            dim: false,
            working_chars: Some("build".chars().count()),
        }];
        let view = ManagerView {
            title: "sessions",
            items: &items,
            selected: 0,
            footer: "esc close",
            search: None,
            search_cursor: 0,
            min_rows: 1,
            min_interior: 0,
        };
        let mut cleared = Vec::new();
        draw_manager(&mut cleared, &view, (80, 24), Color::Cyan, purple, 0, true).unwrap();
        let mut held = Vec::new();
        draw_manager(&mut held, &view, (80, 24), Color::Cyan, purple, 1, false).unwrap();
        let mut tick = Vec::new();
        draw_manager(&mut tick, &view, (80, 24), Color::Cyan, purple, 2, false).unwrap();
        let cleared = String::from_utf8(cleared).unwrap();
        let held = String::from_utf8(held).unwrap();
        let tick = String::from_utf8(tick).unwrap();
        // Clear(All) is CSI 2 J. A letter tick must not wipe the screen.
        assert!(cleared.contains("\u{1b}[2J"), "{cleared:?}");
        assert!(!held.contains("\u{1b}[2J"), "{held:?}");
        assert!(!tick.contains("\u{1b}[2J"), "{tick:?}");
        let rgb = "38;2;168;85;247m";
        let lit = |text: &str| text[text.find(rgb).unwrap() + rgb.len()..].chars().next();
        // The letter holds for one frame, then advances.
        assert_eq!(lit(&cleared), Some('b'), "{cleared:?}");
        assert_eq!(lit(&held), Some('b'), "{held:?}");
        assert_eq!(lit(&tick), Some('u'), "{tick:?}");
    }

    #[test]
    fn a_working_session_lights_one_letter_then_wraps() {
        let _colors = colors_on();
        let purple = Color::Rgb {
            r: 0xa8,
            g: 0x55,
            b: 0xf7,
        };
        let items = vec![running(1, "work", Some(AgentActivity::Working), false)];
        let session = view("other", &["sh"]);
        let layout = laid_out(&items, &session, 40, 24);
        let rgb = "38;2;168;85;247m";
        let lit_at = |tick: usize| {
            let mut buf = Vec::new();
            draw_bar(
                &mut buf,
                &layout,
                &items,
                0,
                0,
                Color::Cyan,
                purple,
                5,
                tick,
            )
            .unwrap();
            let text = String::from_utf8(buf).unwrap();
            let i = text.find(rgb).expect(&text);
            text[i + rgb.len()..].chars().next().unwrap()
        };
        // Two repaint frames per letter: half the previous walk speed.
        assert_eq!(lit_at(0), 'w');
        assert_eq!(lit_at(1), 'w');
        assert_eq!(lit_at(2), 'o');
        assert_eq!(lit_at(6), 'k');
        assert_eq!(lit_at(8), 'w');
    }

    #[test]
    fn a_lit_letter_keeps_the_chip_background() {
        let _colors = colors_on();
        let purple = Color::Rgb {
            r: 0xa8,
            g: 0x55,
            b: 0xf7,
        };
        // Open session: reverse accent chip, so the background is cyan.
        let items = vec![running(0, "work", Some(AgentActivity::Working), false)];
        let session = view("work", &["sh"]);
        let layout = laid_out(&items, &session, 40, 24);
        let mut buf = Vec::new();
        draw_bar(&mut buf, &layout, &items, 0, 0, Color::Cyan, purple, 5, 0).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let rgb = "\u{1b}[38;2;168;85;247m";
        let at = text.find(rgb).expect(&text);
        // The same cyan the chip uses as its reverse foreground
        // (38;5;14) is set as this cell's background (48;5;14). Reverse
        // is off for that cell so the purple stays the foreground.
        let cell = &text[..at];
        let reset = cell.rfind("\u{1b}[0m").expect(&text);
        let lit_sgr = &cell[reset..];
        assert!(lit_sgr.contains("\u{1b}[48;5;14m"), "{text:?}");
        assert!(!lit_sgr.contains("\u{1b}[7m"), "{text:?}");
        assert_eq!(text[at + rgb.len()..].chars().next(), Some('w'));

        // A dim chip has no background of its own. The lit letter must
        // not invent one. Index 1 is not the open session (0).
        let items = vec![running(1, "work", Some(AgentActivity::Working), false)];
        let session = view("other", &["sh"]);
        let layout = laid_out(&items, &session, 40, 24);
        let mut buf = Vec::new();
        draw_bar(&mut buf, &layout, &items, 0, 0, Color::Cyan, purple, 5, 0).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let at = text.find(rgb).expect(&text);
        let cell = &text[..at];
        let reset = cell.rfind("\u{1b}[0m").expect(&text);
        let lit_sgr = &cell[reset..];
        assert!(!lit_sgr.contains("\u{1b}[4"), "{text:?}");
        assert!(!lit_sgr.contains("\u{1b}[7m"), "{text:?}");
    }
}
