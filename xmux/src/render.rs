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
use crate::model::{Layout, Rect, Session, SplitDir, split_rect};

/// The pane area of a client screen: everything except the bottom tab
/// bar row. Sessions are laid out and shells sized to this, so it must
/// be used for split/navigation geometry too.
pub fn content_size(size: (u16, u16)) -> (u16, u16) {
    if size.1 >= 2 {
        (size.0, size.1 - 1)
    } else {
        size
    }
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
/// rectangles, dim divider lines between them, the bottom tab bar, and
/// the focused pane's cursor.
pub fn draw_session(
    renderer: &mut Renderer<'static>,
    session: &Session,
    out: &mut impl Write,
    size: (u16, u16),
    accent: Color,
    bar_top: bool,
    synchronized: bool,
) -> Result<()> {
    let content = content_size(size);
    let full = Rect {
        x: 0,
        // With the bar on top, the pane area shifts down one row.
        y: u16::from(bar_top && size.1 >= 2),
        w: content.0,
        h: content.1,
    };
    let tab = &session.tabs[session.active_tab];
    if synchronized {
        queue!(out, BeginSynchronizedUpdate, Hide)?;
    } else {
        queue!(out, Hide)?;
    }

    let mut cursor = None;
    let mut focus_rect = None;
    let focused = tab.focused;
    if tab.zoomed && let Some(pane) = tab.layout.pane(focused) {
        // Fullscreen: only the focused pane, no dividers.
        cursor = renderer.draw_at(&pane.term, out, full)?;
    } else {
        tab.layout.for_each(full, &mut |pane, rect| {
            let pane_cursor = renderer.draw_at(&pane.term, out, rect)?;
            if pane.id == focused {
                cursor = pane_cursor;
                focus_rect = Some(rect);
            }
            Ok(())
        })?;
        draw_dividers(out, &tab.layout, full, focus_rect, accent)?;
    }

    if size.1 >= 2 {
        let row = if bar_top { 0 } else { size.1 - 1 };
        draw_tab_bar(out, session, size, accent, row)?;
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

/// The tab bar (top or bottom row): the session name as an accent chip,
/// then the tabs — the open tab in accent, the rest dim. Segments past
/// the right edge are dropped.
fn draw_tab_bar(
    out: &mut impl Write,
    session: &Session,
    size: (u16, u16),
    accent: Color,
    row: u16,
) -> Result<()> {
    queue!(out, MoveTo(0, row), SetAttribute(Attribute::Reset))?;
    let (chip, segments) = tab_bar_layout(session, size.0);

    // Session name as a chip: accent background, terminal-background
    // text (accent foreground + reverse adapts to any theme).
    if let Some(chip) = chip {
        queue!(
            out,
            SetForegroundColor(accent),
            SetAttribute(Attribute::Reverse),
            Print(&chip),
            SetAttribute(Attribute::Reset),
        )?;
    }

    for (i, label, _) in &segments {
        if *i == session.active_tab {
            // Accent background chip: accent foreground + reverse gives
            // accent-colored background with terminal-background text.
            queue!(
                out,
                SetForegroundColor(accent),
                SetAttribute(Attribute::Reverse),
            )?;
        } else {
            queue!(out, SetAttribute(Attribute::Dim))?;
        }
        queue!(out, Print(label), SetAttribute(Attribute::Reset))?;
    }
    queue!(out, Clear(ClearType::UntilNewLine))?;
    Ok(())
}

/// The tab bar's contents: the session chip (when it fits) and one
/// `(tab index, label, start column)` per tab that fits on the row.
/// Drawing and click hit-testing share this, so they cannot disagree
/// about where a tab is.
fn tab_bar_layout(session: &Session, cols: u16) -> (Option<String>, Vec<(usize, String, u16)>) {
    let cols = cols as usize;
    let chip = format!(" {} ", session.name);
    let mut used = 0usize;
    let chip = if chip.chars().count() <= cols {
        used += chip.chars().count();
        Some(chip)
    } else {
        None
    };

    let mut segments = Vec::new();
    for (i, tab) in session.tabs.iter().enumerate() {
        // A fullscreened tab advertises it in its label.
        let label = if tab.zoomed {
            format!(" {} [F] ", tab.name)
        } else {
            format!(" {} ", tab.name)
        };
        let width = label.chars().count();
        if used + width > cols {
            break;
        }
        segments.push((i, label, used as u16));
        used += width;
    }
    (chip, segments)
}

/// The tab whose label covers column `x` of the tab bar, if any.
pub fn tab_at(session: &Session, cols: u16, x: u16) -> Option<usize> {
    tab_bar_layout(session, cols)
        .1
        .into_iter()
        .find(|(_, label, start)| {
            x >= *start && x < start + label.chars().count() as u16
        })
        .map(|(i, _, _)| i)
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
    queue!(out, SetAttribute(Attribute::Reset), SetAttribute(Attribute::Dim))?;
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
            (true, f.x.checked_sub(1), f.y, f.h, '▸'), // left divider
            (true, Some(f.x + f.w), f.y, f.h, '◂'),    // right divider
            (false, f.y.checked_sub(1), f.x, f.w, '▾'), // top divider
            (false, Some(f.y + f.h), f.x, f.w, '▴'),   // bottom divider
        ];
        for (vertical, fixed, lo, len, glyph) in sides {
            let Some(fixed) = fixed else { continue };
            if let Some((x, y)) = arrow_cell(&cells, vertical, fixed, lo, len) {
                queue!(out, MoveTo(x, y), Print(glyph))?;
            }
        }
    }
    queue!(out, SetAttribute(Attribute::Reset), SetForegroundColor(Color::Reset))?;
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
    let plain = if vertical { B_UP | B_DOWN } else { B_LEFT | B_RIGHT };
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

pub(crate) fn collect_dividers(layout: &Layout, rect: Rect, cells: &mut HashMap<(u16, u16), (u8, bool)>) {
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
        Clear(ClearType::All),
    )?;
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
        // independent.
        queue!(
            out,
            SetForegroundColor(if item.active { accent } else { Color::Reset }),
        )?;
        if item.dim && !is_selected {
            queue!(out, SetAttribute(Attribute::Dim))?;
        }
        queue!(
            out,
            Print(fit(&item.label, panel.iw.saturating_sub(2))),
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
    let visible: String = chars[start..(start + width).min(chars.len())].iter().collect();

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
    fn draw_at(
        &mut self,
        term: &Terminal<'alloc, '_>,
        out: &mut impl Write,
        rect: Rect,
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

        let frame_dirty = snapshot.dirty()?;
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
