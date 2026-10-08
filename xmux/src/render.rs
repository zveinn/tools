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
use crate::agent_status::AgentActivity;
use crate::model::{Layout, Rect, Session, SplitDir, split_rect};

/// One session drawn on the agent bar.
pub struct AgentBarItem {
    /// Index into the server's session vec. Clicking the chip switches
    /// to this session.
    pub index: usize,
    pub name: String,
    pub activity: AgentActivity,
}

/// Where the tab bar, the agent bar, and the pane area sit on one
/// client's screen.
///
/// `content` is the pane area's size (origin is always the pane area's
/// own top-left; `content_y` is where that lands on the screen). Bars
/// are screen rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    pub content: (u16, u16),
    pub content_y: u16,
    pub tab_row: Option<u16>,
    pub agent_row: Option<u16>,
}

/// Screen geometry for one client.
///
/// The tab bar takes one row once the terminal is at least two rows
/// tall. The agent bar takes a second row when `show_agents` is set
/// and a content row would still remain; on a two-row screen the tab
/// bar wins and the agent bar stays hidden. A bar on top shifts the
/// pane area down. When both bars share an edge, the agent bar sits
/// on the screen edge and the tab bar stays next to the panes.
pub fn chrome(size: (u16, u16), tab_top: bool, agent_top: bool, show_agents: bool) -> Chrome {
    let (cols, rows) = size;
    let tab = rows >= 2;
    let agent = show_agents && rows >= 3;

    let mut tab_row = None;
    let mut agent_row = None;

    // Top edge, outer row first so a shared edge puts the agent bar
    // on row 0 and the tab bar beside the panes.
    let mut next_top = 0u16;
    if agent && agent_top {
        agent_row = Some(next_top);
        next_top += 1;
    }
    if tab && tab_top {
        tab_row = Some(next_top);
        next_top += 1;
    }

    // Bottom edge, outer row first: the agent bar is the last row,
    // the tab bar the one above it.
    let agent_bottom = agent && !agent_top;
    let tab_bottom = tab && !tab_top;
    if agent_bottom {
        agent_row = Some(rows - 1);
    }
    if tab_bottom {
        tab_row = Some(rows - 1 - u16::from(agent_bottom));
    }

    let chrome_rows = u16::from(tab) + u16::from(agent);
    Chrome {
        content: (cols, rows.saturating_sub(chrome_rows)),
        content_y: next_top,
        tab_row,
        agent_row,
    }
}

/// The pane area of a client screen: everything except the tab bar and,
/// when `show_agents` is set, the agent bar. Sessions are laid out and
/// shells sized to this, so it must be used for split/navigation
/// geometry too. Which edge a bar sits on changes the pane origin, not
/// this size.
pub fn content_size(size: (u16, u16), show_agents: bool) -> (u16, u16) {
    chrome(size, false, false, show_agents).content
}

/// Whether any session has an agent at work or waiting at its prompt.
/// The agent bar is shown exactly when this is true (and the screen
/// has room for it).
pub fn any_agent(sessions: &[Session]) -> bool {
    sessions.iter().any(|s| s.agent_activity().is_some())
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
/// rectangles, dim divider lines between them, the tab bar, the agent
/// bar when `agents` is non-empty, and the focused pane's cursor.
///
/// `full` repaints every cell instead of only the rows the panes marked
/// dirty. The caller sets it when something outside the panes owns what
/// is on the client's screen — an overlay that cleared it, a resize —
/// because the panes' dirty state says nothing about that.
pub fn draw_session(
    renderer: &mut Renderer<'static>,
    session: &Session,
    agents: &[AgentBarItem],
    active: usize,
    out: &mut impl Write,
    size: (u16, u16),
    accent: Color,
    tab_top: bool,
    agent_top: bool,
    synchronized: bool,
    full: bool,
) -> Result<()> {
    let screen = chrome(size, tab_top, agent_top, !agents.is_empty());
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

    if let Some(row) = screen.tab_row {
        draw_tab_bar(out, session, size.0, accent, row)?;
    }
    if let Some(row) = screen.agent_row {
        draw_agent_bar(out, agents, active, size.0, accent, row)?;
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
    cols: u16,
    accent: Color,
    row: u16,
) -> Result<()> {
    queue!(out, MoveTo(0, row), SetAttribute(Attribute::Reset))?;
    let (chip, segments) = tab_bar_layout(session, cols);

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
    // Session-wide mark: a working pane anywhere in the session, else an
    // agent sitting at its prompt. Tabs below carry their own mark.
    let chip = match crate::agent_status::mark(session.agent_activity()) {
        "" => format!(" {} ", session.name),
        mark => format!(" {mark} {} ", session.name),
    };
    let mut used = 0usize;
    let chip = if chip.chars().count() <= cols {
        used += chip.chars().count();
        Some(chip)
    } else {
        None
    };

    let mut segments = Vec::new();
    for (i, tab) in session.tabs.iter().enumerate() {
        // A fullscreened tab advertises it in its label. An agent mark
        // sits in front of the name so a background tab shows whether
        // its agent is mid-turn or back at the prompt.
        let name = crate::agent_status::prefix_name(&tab.name, tab.agent_activity());
        let label = if tab.zoomed {
            format!(" {name} [F] ")
        } else {
            format!(" {name} ")
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
        .find(|(_, label, start)| x >= *start && x < start + label.chars().count() as u16)
        .map(|(i, _, _)| i)
}

/// The agent bar: one chip per session that has an agent, in session-list
/// order. The session you're in is an accent chip, the same as the open
/// tab; the rest are dim. Chips past the right edge are dropped.
fn draw_agent_bar(
    out: &mut impl Write,
    items: &[AgentBarItem],
    active: usize,
    cols: u16,
    accent: Color,
    row: u16,
) -> Result<()> {
    queue!(out, MoveTo(0, row), SetAttribute(Attribute::Reset))?;
    for (index, label, _) in agent_bar_layout(items, cols) {
        if index == active {
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

/// `(session index, label, start column)` for each agent chip that fits.
/// Drawing and click hit-testing share this, so they cannot disagree
/// about where a session is.
fn agent_bar_layout(items: &[AgentBarItem], cols: u16) -> Vec<(usize, String, u16)> {
    let cols = cols as usize;
    let mut used = 0usize;
    let mut segments = Vec::new();
    for item in items {
        // Same label shape as a tab: the list's mark, then the name,
        // padded like a tab chip.
        let name = crate::agent_status::prefix_name(&item.name, Some(item.activity));
        let label = format!(" {name} ");
        let width = label.chars().count();
        if used + width > cols {
            break;
        }
        segments.push((item.index, label, used as u16));
        used += width;
    }
    segments
}

/// The session whose agent-bar chip covers column `x`, if any.
pub fn agent_at(items: &[AgentBarItem], cols: u16, x: u16) -> Option<usize> {
    agent_bar_layout(items, cols)
        .into_iter()
        .find(|(_, label, start)| x >= *start && x < start + label.chars().count() as u16)
        .map(|(index, _, _)| index)
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
    /// Agent activity drawn as a mark ahead of the label.
    pub agent: Option<crate::agent_status::AgentActivity>,
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
        .map(|i| i.label.chars().count() + 2 + crate::agent_status::mark_columns(i.agent))
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
        // independent. A working agent takes the accent mark; a
        // finished one stays dim so the busy rows are the ones that
        // read as live.
        let mark_cols = crate::agent_status::mark_columns(item.agent);
        if let Some(state) = item.agent {
            if state == crate::agent_status::AgentActivity::Working {
                queue!(
                    out,
                    SetForegroundColor(accent),
                    SetAttribute(Attribute::Bold)
                )?;
            } else if !is_selected {
                queue!(out, SetAttribute(Attribute::Dim))?;
            }
            queue!(
                out,
                Print(crate::agent_status::mark(Some(state))),
                Print(" "),
                SetAttribute(Attribute::Reset),
            )?;
        }
        queue!(
            out,
            SetForegroundColor(if item.active { accent } else { Color::Reset }),
        )?;
        if item.dim && !is_selected {
            queue!(out, SetAttribute(Attribute::Dim))?;
        }
        if is_selected {
            queue!(out, SetAttribute(Attribute::Bold))?;
        }
        queue!(
            out,
            Print(fit(
                &item.label,
                (panel.iw as usize).saturating_sub(2 + mark_cols),
            )),
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

    fn chip(index: usize, name: &str, activity: AgentActivity) -> AgentBarItem {
        AgentBarItem {
            index,
            name: name.to_string(),
            activity,
        }
    }

    #[test]
    fn content_shrinks_only_while_an_agent_bar_is_shown() {
        assert_eq!(content_size((80, 24), false), (80, 23));
        assert_eq!(content_size((80, 24), true), (80, 22));
        // Two rows: the tab bar keeps its row and the agent bar stays off.
        assert_eq!(content_size((80, 2), true), (80, 1));
        assert_eq!(content_size((80, 1), true), (80, 1));
    }

    #[test]
    fn chrome_puts_a_lone_tab_bar_on_the_configured_edge() {
        let bottom = chrome((80, 24), false, false, false);
        assert_eq!(bottom.tab_row, Some(23));
        assert_eq!(bottom.agent_row, None);
        assert_eq!(bottom.content, (80, 23));
        assert_eq!(bottom.content_y, 0);

        let top = chrome((80, 24), true, false, false);
        assert_eq!(top.tab_row, Some(0));
        assert_eq!(top.content_y, 1);
        assert_eq!(top.content, (80, 23));
    }

    #[test]
    fn chrome_stacks_the_agent_bar_on_the_outer_edge() {
        let both_bottom = chrome((80, 24), false, false, true);
        assert_eq!(both_bottom.tab_row, Some(22));
        assert_eq!(both_bottom.agent_row, Some(23));
        assert_eq!(both_bottom.content, (80, 22));
        assert_eq!(both_bottom.content_y, 0);

        let both_top = chrome((80, 24), true, true, true);
        assert_eq!(both_top.agent_row, Some(0));
        assert_eq!(both_top.tab_row, Some(1));
        assert_eq!(both_top.content_y, 2);
        assert_eq!(both_top.content, (80, 22));

        let agent_top = chrome((80, 24), false, true, true);
        assert_eq!(agent_top.agent_row, Some(0));
        assert_eq!(agent_top.tab_row, Some(23));
        assert_eq!(agent_top.content_y, 1);
        assert_eq!(agent_top.content, (80, 22));

        let agent_bottom = chrome((80, 24), true, false, true);
        assert_eq!(agent_bottom.tab_row, Some(0));
        assert_eq!(agent_bottom.agent_row, Some(23));
        assert_eq!(agent_bottom.content_y, 1);
        assert_eq!(agent_bottom.content, (80, 22));
    }

    #[test]
    fn agent_bar_labels_match_tabs_and_drop_what_does_not_fit() {
        let items = vec![
            chip(3, "work", AgentActivity::Working),
            chip(1, "notes", AgentActivity::Idle),
        ];
        // " ▶ work " is 8 columns, " ✓ notes " is 9.
        let fit = agent_bar_layout(&items, 17);
        assert_eq!(fit.len(), 2);
        assert_eq!(fit[0].0, 3);
        assert_eq!(fit[0].1, " ▶ work ");
        assert_eq!(fit[0].2, 0);
        assert_eq!(fit[1].1, " ✓ notes ");
        assert_eq!(fit[1].2, 8);

        assert_eq!(agent_bar_layout(&items, 8).len(), 1);
        assert!(agent_bar_layout(&items, 7).is_empty());
        assert_eq!(agent_at(&items, 17, 0), Some(3));
        assert_eq!(agent_at(&items, 17, 7), Some(3));
        assert_eq!(agent_at(&items, 17, 8), Some(1));
        assert_eq!(agent_at(&items, 17, 16), Some(1));
        assert_eq!(agent_at(&items, 17, 17), None);
    }

    #[test]
    fn agent_bar_uses_the_tab_bar_colors() {
        let items = vec![
            chip(0, "work", AgentActivity::Working),
            chip(2, "notes", AgentActivity::Idle),
        ];
        let mut buf = Vec::new();
        draw_agent_bar(&mut buf, &items, 0, 40, Color::Cyan, 5).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.contains(" ▶ work "), "{text:?}");
        assert!(text.contains(" ✓ notes "), "{text:?}");
        // MoveTo(0, 5) is the 1-based sequence for that row.
        assert!(text.contains("\u{1b}[6;1H"), "{text:?}");
        let work = text.find(" ▶ work ").unwrap();
        let notes = text.find(" ✓ notes ").unwrap();
        // Accent reverse on the open session, dim on the other — the
        // same attributes the tab bar uses for the open and resting tabs.
        assert!(text[..work].contains("\u{1b}[7m"), "{text:?}");
        assert!(text[work..notes].contains("\u{1b}[2m"), "{text:?}");
    }
}
