//! Modern single-line column rendering.
//!
//! Visual language (inspired by tools like eza / lsd / modern TUIs):
//! - columns separated by a dim hairline `│`
//! - type glyph before names (`▸` dir, `›` exec, `↗` link, …)
//! - permissions as type + triads: `d rwx·r-x·r-x`
//! - empty optional fields as an em dash `—`
//! - sparse as a filled/empty diamond
//! - size with a slightly brighter number, quieter unit

use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::columns::Column;
use crate::entry::{Entry, Kind};

/// Whether ANSI colors are emitted (auto / always / never).
static COLOR_ENABLED: AtomicBool = AtomicBool::new(true);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorMode {
    /// Color only when stdout is a TTY (default). Respects `NO_COLOR`.
    Auto,
    Always,
    Never,
}

/// Configure color output. Call once at startup before printing.
pub fn init_color(mode: ColorMode) {
    let on = match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => {
            // https://no-color.org/ — any value disables.
            if std::env::var_os("NO_COLOR").is_some() {
                false
            } else if force_color_env() {
                true
            } else if std::env::var("CLICOLOR").ok().as_deref() == Some("0") {
                false
            } else {
                io::stdout().is_terminal()
            }
        }
    };
    COLOR_ENABLED.store(on, Ordering::Relaxed);
}

fn force_color_env() -> bool {
    // Common force flags used by CI / tooling.
    matches!(
        std::env::var("CLICOLOR_FORCE").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    ) || matches!(
        std::env::var("FORCE_COLOR").ok().as_deref(),
        Some(v) if v != "0"
    )
}

pub fn color_enabled() -> bool {
    COLOR_ENABLED.load(Ordering::Relaxed)
}

/// ANSI SGR code that becomes a no-op when colors are disabled.
#[derive(Clone, Copy)]
pub struct Ansi(pub &'static str);

impl fmt::Display for Ansi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if color_enabled() {
            f.write_str(self.0)
        } else {
            Ok(())
        }
    }
}

impl Ansi {
    /// Raw escape (or empty) for string building.
    pub fn as_str(self) -> &'static str {
        if color_enabled() {
            self.0
        } else {
            ""
        }
    }
}

pub const WHITE: Ansi = Ansi("\x1b[97m");
pub const BOLD_WHITE: Ansi = Ansi("\x1b[1;97m");
pub const LIGHT_BLUE: Ansi = Ansi("\x1b[38;5;117m");
pub const GREEN: Ansi = Ansi("\x1b[92m");
pub const RED: Ansi = Ansi("\x1b[91m");
pub const ORANGE: Ansi = Ansi("\x1b[38;5;214m");
pub const DIM: Ansi = Ansi("\x1b[90m");
pub const SOFT: Ansi = Ansi("\x1b[38;5;247m");
pub const SOFT_BLUE: Ansi = Ansi("\x1b[38;5;111m");
pub const RESET: Ansi = Ansi("\x1b[0m");

fn sep(table: bool) -> &'static str {
    if table {
        if color_enabled() {
            "\x1b[90m │ \x1b[0m"
        } else {
            " │ "
        }
    } else {
        "  "
    }
}

#[derive(Default)]
pub struct Widths {
    perms: usize,
    nlink: usize,
    user: usize,  // "rwx name"
    group: usize, // "r-x name"
    other: usize, // "r-x" (+ markers)
    size: usize,
    blocks: usize,
    ino: usize,
    dev: usize,
    time: usize,
    flags: usize,
    xattrs: usize,
    xfs: usize,
    name: usize,
}

impl Widths {
    pub fn measure(entries: &[Entry], cols: &[Column]) -> Self {
        let mut w = Self {
            time: "DD-MM-YYYY HH:MM:SS".len(),
            ..Default::default()
        };

        for c in cols {
            match c {
                Column::Perms => w.perms = w.perms.max("PERMS".len()),
                Column::User => w.user = w.user.max("USER".len()),
                Column::Group => w.group = w.group.max("GROUP".len()),
                Column::Other => w.other = w.other.max("OTHER".len()),
                Column::Size => w.size = w.size.max("SIZE".len()),
                Column::Nlink => w.nlink = w.nlink.max("N".len()),
                Column::Blocks => w.blocks = w.blocks.max("BLOCKS".len()),
                Column::Ino => w.ino = w.ino.max("INO:IGEN".len()),
                Column::Dev => w.dev = w.dev.max("DEV".len()),
                Column::Flags => w.flags = w.flags.max("FLAGS".len()),
                Column::Xattrs => w.xattrs = w.xattrs.max("XATTRS".len()),
                Column::Xfs => w.xfs = w.xfs.max("XFS".len()),
                Column::Name => w.name = w.name.max("NAME".len()),
                Column::Mtime
                | Column::Atime
                | Column::Ctime
                | Column::Birth
                | Column::Sparse => {}
            }
        }

        for e in entries {
            for c in cols {
                match c {
                    Column::Perms => w.perms = w.perms.max(perms_plain(e).chars().count()),
                    Column::User => w.user = w.user.max(user_plain(e).chars().count()),
                    Column::Group => w.group = w.group.max(group_plain(e).chars().count()),
                    Column::Other => w.other = w.other.max(other_plain(e).chars().count()),
                    Column::Size => w.size = w.size.max(human_size(e.size).len()),
                    Column::Nlink => w.nlink = w.nlink.max(e.nlink.to_string().len()),
                    Column::Blocks => w.blocks = w.blocks.max(format_blocks(e).len()),
                    Column::Ino => w.ino = w.ino.max(format_ino(e).len()),
                    Column::Dev => w.dev = w.dev.max(format_dev(e).len()),
                    Column::Flags => w.flags = w.flags.max(format_list_field(&e.extras.flags).len()),
                    Column::Xattrs => {
                        w.xattrs = w.xattrs.max(format_list_field_owned(&e.extras.xattrs).len())
                    }
                    Column::Xfs => w.xfs = w.xfs.max(format_xfs(e).len()),
                    Column::Name => {
                        w.name = w.name.max(e.name.chars().count());
                    }
                    _ => {}
                }
            }
        }
        w
    }

    fn width_for(&self, c: Column) -> usize {
        match c {
            Column::Perms => self.perms,
            Column::User => self.user,
            Column::Group => self.group,
            Column::Other => self.other,
            Column::Size => self.size,
            Column::Nlink => self.nlink,
            Column::Blocks => self.blocks,
            Column::Ino => self.ino,
            Column::Dev => self.dev,
            Column::Flags => self.flags,
            Column::Xattrs => self.xattrs,
            Column::Xfs => self.xfs,
            Column::Mtime | Column::Atime | Column::Ctime | Column::Birth => self.time,
            Column::Sparse => 1,
            Column::Name => self.name,
        }
    }
}

/// Terminal width in columns, if available.
pub fn terminal_width() -> Option<usize> {
    if let Ok(c) = std::env::var("COLUMNS") {
        if let Ok(n) = c.parse::<usize>() {
            if n > 0 {
                return Some(n);
            }
        }
    }
    // Safety: TIOCGWINSZ on stdout; fails when not a tty.
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return Some(ws.ws_col as usize);
        }
    }
    None
}

pub fn color_for(e: &Entry) -> Ansi {
    if e.broken_symlink {
        return RED;
    }
    match e.kind {
        Kind::Dir => SOFT_BLUE, // same as USER column
        Kind::Symlink => ORANGE,
        Kind::File if e.executable => GREEN,
        Kind::Fifo | Kind::Socket | Kind::Block | Kind::Char => ORANGE,
        Kind::File | Kind::Unknown => WHITE,
    }
}

/// Type word used at the end of PERMS (dir, file, exec, …).
fn type_word(e: &Entry) -> &'static str {
    if e.broken_symlink {
        return "broken";
    }
    match e.kind {
        Kind::Dir => "dir",
        Kind::Symlink => "link",
        Kind::Fifo => "fifo",
        Kind::Socket => "sock",
        Kind::Block => "block",
        Kind::Char => "char",
        Kind::File if e.executable => "exec",
        Kind::File => "file",
        Kind::Unknown => "unknown",
    }
}

pub fn write_header(
    out: &mut impl Write,
    cols: &[Column],
    w: &Widths,
    table: bool,
) -> io::Result<()> {
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            write!(out, "{}", sep(table))?;
        }
        let label = c.header();
        let width = w.width_for(*c);
        if matches!(c, Column::Size | Column::Nlink) {
            write!(out, "{BOLD_WHITE}{label:>width$}{RESET}")?;
        } else if width == 0 {
            write!(out, "{BOLD_WHITE}{label}{RESET}")?;
        } else {
            write!(out, "{BOLD_WHITE}{label:<width$}{RESET}")?;
        }
    }
    writeln!(out)?;
    if table {
        write_header_rule(out, cols, w)?;
    }
    Ok(())
}

fn write_header_rule(out: &mut impl Write, cols: &[Column], w: &Widths) -> io::Result<()> {
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            write!(out, "{DIM}─┼─{RESET}")?;
        }
        let width = w.width_for(*c).max(c.header().len());
        let width = if *c == Column::Sparse { 1 } else { width };
        for _ in 0..width {
            write!(out, "{DIM}─{RESET}")?;
        }
    }
    writeln!(out)
}

/// Wide-terminal: single table row.
pub fn write_entry(
    out: &mut impl Write,
    e: &Entry,
    cols: &[Column],
    w: &Widths,
    table: bool,
) -> io::Result<()> {
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            write!(out, "{}", sep(table))?;
        }
        write_column(out, e, *c, w, false)?;
    }
    writeln!(out)
}

/// Gap (spaces) between side-by-side cards.
const CARD_GAP: usize = 1;

/// Render all entries as bordered cards, packing as many as fit per row.
pub fn write_entry_cards(
    out: &mut impl Write,
    entries: &[Entry],
    cols: &[Column],
    show_labels: bool,
) -> io::Result<()> {
    if entries.is_empty() {
        return Ok(());
    }

    let term = terminal_width().unwrap_or(80);
    // Skip Name (title), Group (already in USER as user/group), Other (in PERMS triads).
    let meta: Vec<Column> = cols
        .iter()
        .copied()
        .filter(|c| !matches!(c, Column::Name | Column::Group | Column::Other))
        .collect();
    let label_w = meta
        .iter()
        .map(|c| c.header().len())
        .max()
        .unwrap_or(0);

    // Natural content width across all cards (so a grid looks even).
    let mut content_w = 20usize;
    for e in entries {
        content_w = content_w.max(measure_card_content(e, cols, &meta, label_w, show_labels));
    }

    // Aim for a multi-column grid when the terminal is wide enough.
    // Long fields (XFS, etc.) wrap onto extra lines inside the card.
    let target_cols = if term >= 120 {
        3
    } else if term >= 72 {
        2
    } else {
        1
    };
    let max_outer_for_grid = if target_cols > 1 {
        term.saturating_sub(CARD_GAP * (target_cols - 1)) / target_cols
    } else {
        term
    }
    .max(22);

    // Prefer content size, but cap so the target grid still fits.
    let mut outer = (content_w + 2).min(max_outer_for_grid).clamp(22, term.max(22));
    let per_row = cards_per_row(term, outer);
    // Tile remaining space evenly across the row.
    if per_row > 1 {
        let usable = term.saturating_sub(CARD_GAP * (per_row - 1));
        outer = (usable / per_row).clamp(22, term);
    }
    let inner = outer.saturating_sub(2);

    for chunk in entries.chunks(per_row) {
        let mut cards: Vec<Vec<String>> = chunk
            .iter()
            .map(|e| build_card(e, cols, &meta, label_w, show_labels, outer, inner))
            .collect();

        let height = cards.iter().map(|c| c.len()).max().unwrap_or(0);
        // Equalize height with blank interior lines.
        for card in &mut cards {
            while card.len() < height {
                // Insert empty body lines before the bottom border.
                if let Some(bottom) = card.pop() {
                    card.push(card_empty_line(outer, inner));
                    card.push(bottom);
                } else {
                    break;
                }
            }
        }

        for row in 0..height {
            for (i, card) in cards.iter().enumerate() {
                if i > 0 {
                    for _ in 0..CARD_GAP {
                        write!(out, " ")?;
                    }
                }
                write!(out, "{}", card[row])?;
            }
            writeln!(out)?;
        }
        writeln!(out)?; // vertical gap between card rows
    }
    Ok(())
}

fn cards_per_row(term: usize, outer: usize) -> usize {
    if outer == 0 {
        return 1;
    }
    let n = (term + CARD_GAP) / (outer + CARD_GAP);
    n.max(1)
}

/// Visible width needed for card body (title / labeled fields).
fn measure_card_content(
    e: &Entry,
    cols: &[Column],
    meta: &[Column],
    label_w: usize,
    show_labels: bool,
) -> usize {
    let mut w = 0usize;
    if cols.contains(&Column::Name) {
        let mut title = 1 + e.name.chars().count(); // leading space
        if let Some(t) = &e.symlink {
            title += 3 + t.chars().count(); // " → target"
        }
        w = w.max(title);
    }
    let dummy = Widths::default();
    for c in meta {
        let mut body = Vec::new();
        write!(body, " ").ok();
        if show_labels {
            let _ = write!(body, "{:<label_w$}  ", c.header());
        }
        let _ = write_column(&mut body, e, *c, &dummy, true);
        w = w.max(visible_width(&String::from_utf8_lossy(&body)));
    }
    w.max(12)
}

/// Build one card as a list of complete lines (no trailing newline).
fn build_card(
    e: &Entry,
    cols: &[Column],
    meta: &[Column],
    label_w: usize,
    show_labels: bool,
    outer: usize,
    inner: usize,
) -> Vec<String> {
    let dummy = Widths::default();
    let mut lines = Vec::new();

    lines.push(card_top(outer));

    if cols.contains(&Column::Name) {
        let mut title = Vec::new();
        let color = color_for(e);
        let _ = write!(title, " {color}{name}{RESET}", name = e.name);
        if let Some(target) = &e.symlink {
            let tc = if e.broken_symlink { RED } else { ORANGE };
            let _ = write!(title, " {DIM}→{RESET} {tc}{target}{RESET}");
        }
        lines.push(card_content_line(&String::from_utf8_lossy(&title), inner));
        if !meta.is_empty() {
            lines.push(card_divider(inner));
        }
    }

    for c in meta {
        let mut prefix = String::from(" ");
        if show_labels {
            prefix.push_str(&format!(
                "{DIM}{label:<label_w$}{RESET}  ",
                label = c.header(),
            ));
        }
        let mut value = Vec::new();
        let _ = write_column(&mut value, e, *c, &dummy, true);
        let value = String::from_utf8_lossy(&value);
        for line in wrap_labeled_field(&prefix, &value, inner) {
            lines.push(card_content_line(&line, inner));
        }
    }

    if !cols.contains(&Column::Name) && meta.is_empty() {
        lines.push(card_empty_line(outer, inner));
    }

    lines.push(card_bottom(outer));
    lines
}

/// Split a labeled field across lines when the value is wider than the card.
/// Continuations are indented to align under the value.
fn wrap_labeled_field(prefix: &str, value: &str, inner: usize) -> Vec<String> {
    let prefix_w = visible_width(prefix);
    let avail = inner.saturating_sub(prefix_w).max(8);
    let chunks = wrap_ansi(value, avail);
    if chunks.is_empty() {
        return vec![prefix.to_string()];
    }
    let indent = " ".repeat(prefix_w);
    let last = chunks.len().saturating_sub(1);
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, chunk)| {
            let mut chunk = chunk;
            if i > 0 {
                // Drop leading separators so wrapped lines don't start with " · "
                chunk = trim_ansi_start_separators(&chunk);
            }
            if i < last {
                // Avoid dangling " ·" at the end of a continued line.
                chunk = trim_ansi_end_separators(&chunk);
            }
            if i == 0 {
                format!("{prefix}{chunk}")
            } else {
                format!("{indent}{chunk}")
            }
        })
        .collect()
}

/// Trim trailing spaces / middle-dots from a wrapped chunk (ANSI-safe enough for our values).
fn trim_ansi_end_separators(s: &str) -> String {
    // Strip ANSI to find plain end, then cut original at last non-separator visible char.
    let plain = strip_ansi(s);
    let trimmed = plain.trim_end_matches([' ', '·']);
    if trimmed.len() == plain.len() {
        return s.to_string();
    }
    // Keep only prefix of `s` that produces `trimmed` visible chars.
    truncate_ansi(s, trimmed.chars().count())
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

/// Trim leading spaces / middle-dots (and their ANSI wrappers) from a wrapped chunk.
fn trim_ansi_start_separators(s: &str) -> String {
    let mut chars = s.chars().peekable();
    let mut pending_ansi = String::new();
    while let Some(&ch) = chars.peek() {
        if ch == '\u{1b}' {
            pending_ansi.push(chars.next().unwrap());
            if chars.peek() == Some(&'[') {
                pending_ansi.push(chars.next().unwrap());
                for c in chars.by_ref() {
                    pending_ansi.push(c);
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if ch == ' ' || ch == '·' {
            chars.next();
            pending_ansi.clear(); // separators don't need prior SGR
            continue;
        }
        break;
    }
    let rest: String = chars.collect();
    if rest.is_empty() {
        s.to_string()
    } else {
        format!("{pending_ansi}{rest}")
    }
}

/// Wrap an ANSI string to a maximum visible width.
/// Prefers breaks after spaces / middle-dots; hard-wraps otherwise.
/// Re-applies the active SGR on continuation lines.
fn wrap_ansi(s: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![s.to_string()];
    }
    if visible_width(s) <= width {
        return vec![s.to_string()];
    }

    let chars: Vec<char> = s.chars().collect();
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_vis = 0usize;
    let mut active_sgr = String::new();
    // Snapshot after a breakable char: (line text, visible len, sgr, next index)
    let mut good_break: Option<(String, usize, String, usize)> = None;
    let mut i = 0usize;

    while i < chars.len() {
        // Copy CSI sequences through unchanged.
        if chars[i] == '\u{1b}' && i + 1 < chars.len() && chars[i + 1] == '[' {
            let start = i;
            i += 2;
            while i < chars.len() && !chars[i].is_ascii_alphabetic() {
                i += 1;
            }
            if i < chars.len() {
                let final_ch = chars[i];
                i += 1;
                let seq: String = chars[start..i].iter().collect();
                cur.push_str(&seq);
                if final_ch == 'm' {
                    if seq == "\x1b[0m" || seq == "\x1b[m" {
                        active_sgr.clear();
                    } else {
                        active_sgr = seq;
                    }
                }
            }
            continue;
        }

        // Would one more visible char overflow?
        if cur_vis + 1 > width {
            if let Some((snap, _, sgr_at_break, next_i)) = good_break.take() {
                let mut line = snap;
                line.push_str(RESET.as_str());
                lines.push(line);
                cur.clear();
                cur_vis = 0;
                active_sgr = sgr_at_break;
                if !active_sgr.is_empty() {
                    cur.push_str(&active_sgr);
                }
                i = next_i;
                continue;
            }
            // Hard wrap: flush what we have, then place current char on next line.
            if cur_vis > 0 {
                let mut line = std::mem::take(&mut cur);
                line.push_str(RESET.as_str());
                lines.push(line);
                cur_vis = 0;
                if !active_sgr.is_empty() {
                    cur.push_str(&active_sgr);
                }
                // fall through to add current char to the new line
            }
        }

        let ch = chars[i];
        cur.push(ch);
        cur_vis += 1;
        i += 1;

        if ch == ' ' || ch == '·' {
            good_break = Some((cur.clone(), cur_vis, active_sgr.clone(), i));
        }
    }

    if cur_vis > 0 || lines.is_empty() {
        if cur_vis > 0 {
            cur.push_str(RESET.as_str());
        }
        lines.push(cur);
    }
    lines
}

fn card_top(outer: usize) -> String {
    let mut s = format!("{DIM}╭{RESET}");
    for _ in 0..outer.saturating_sub(2) {
        s.push_str(&format!("{DIM}─{RESET}"));
    }
    s.push_str(&format!("{DIM}╮{RESET}"));
    s
}

fn card_bottom(outer: usize) -> String {
    let mut s = format!("{DIM}╰{RESET}");
    for _ in 0..outer.saturating_sub(2) {
        s.push_str(&format!("{DIM}─{RESET}"));
    }
    s.push_str(&format!("{DIM}╯{RESET}"));
    s
}

fn card_divider(inner: usize) -> String {
    let mut s = format!("{DIM}│{RESET} ");
    let rule = inner.saturating_sub(2);
    for _ in 0..rule {
        s.push_str(&format!("{DIM}─{RESET}"));
    }
    if inner >= 1 {
        s.push(' ');
    }
    let written = 1 + rule + usize::from(inner >= 1);
    for _ in written..inner {
        s.push(' ');
    }
    s.push_str(&format!("{DIM}│{RESET}"));
    s
}

fn card_empty_line(outer: usize, inner: usize) -> String {
    let _ = outer;
    card_content_line(" ", inner)
}

fn card_content_line(content: &str, inner: usize) -> String {
    let mut s = format!("{DIM}│{RESET}");
    let vis = visible_width(content);
    if vis <= inner {
        s.push_str(content);
        for _ in 0..(inner - vis) {
            s.push(' ');
        }
    } else {
        let keep = inner.saturating_sub(1);
        s.push_str(&truncate_ansi(content, keep));
        s.push_str(&format!("{DIM}…{RESET}"));
    }
    s.push_str(&format!("{DIM}│{RESET}"));
    s
}

/// Count printable width, ignoring ANSI CSI sequences.
fn visible_width(s: &str) -> usize {
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        n += 1;
    }
    n
}

/// Best-effort visible truncate; always ends with RESET so colors don't leak.
fn truncate_ansi(s: &str, max_vis: usize) -> String {
    if max_vis == 0 {
        return RESET.as_str().to_string();
    }
    let mut out = String::new();
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            out.push(ch);
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap());
                for c in chars.by_ref() {
                    out.push(c);
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if n >= max_vis {
            break;
        }
        out.push(ch);
        n += 1;
    }
    out.push_str(RESET.as_str());
    out
}

fn write_column(
    out: &mut impl Write,
    e: &Entry,
    c: Column,
    widths: &Widths,
    compact: bool,
) -> io::Result<()> {
    // compact = natural width (card mode); else pad to measured column width.
    let col_w = |measured: usize| -> usize {
        if compact {
            0
        } else {
            measured
        }
    };

    match c {
        Column::Mtime => write_time(out, e.mtime, col_w(widths.time)),
        Column::Atime => write_time(out, e.atime, col_w(widths.time)),
        Column::Ctime => write_time(out, epoch_to_system(e.ctime_secs), col_w(widths.time)),
        Column::Birth => write_time(out, e.birth, col_w(widths.time)),
        Column::Perms => write_perms(out, e, col_w(widths.perms)),
        Column::User => write_user_col(out, e, col_w(widths.user)),
        Column::Group => write_group_col(out, e, col_w(widths.group)),
        Column::Other => write_other_col(out, e, col_w(widths.other)),
        Column::Size => write_size(out, e.size, col_w(widths.size)),
        Column::Name => write_name(out, e, col_w(widths.name)),
        Column::Nlink => {
            if compact {
                write!(out, "{SOFT}{}{RESET}", e.nlink)
            } else {
                write!(
                    out,
                    "{SOFT}{v:>width$}{RESET}",
                    v = e.nlink,
                    width = widths.nlink
                )
            }
        }
        Column::Blocks => {
            let v = format_blocks(e);
            if compact {
                write!(out, "{DIM}{v}{RESET}")
            } else {
                write!(out, "{DIM}{v:<width$}{RESET}", width = widths.blocks)
            }
        }
        Column::Sparse => {
            if compact {
                if e.sparse {
                    write!(out, "{ORANGE}yes{RESET}")
                } else {
                    write!(out, "{DIM}no{RESET}")
                }
            } else if e.sparse {
                write!(out, "{ORANGE}◆{RESET}")
            } else {
                write!(out, "{DIM}◇{RESET}")
            }
        }
        Column::Ino => write_ino(out, e, col_w(widths.ino)),
        Column::Dev => {
            let v = format_dev(e);
            if compact {
                write!(out, "{DIM}{v}{RESET}")
            } else {
                write!(out, "{DIM}{v:<width$}{RESET}", width = widths.dev)
            }
        }
        Column::Flags => {
            write_badge(out, &format_list_field(&e.extras.flags), col_w(widths.flags))
        }
        Column::Xattrs => write_badge(
            out,
            &format_list_field_owned(&e.extras.xattrs),
            col_w(widths.xattrs),
        ),
        Column::Xfs => write_badge(out, &format_xfs(e), col_w(widths.xfs)),
    }
}

fn write_time(out: &mut impl Write, t: Option<SystemTime>, width: usize) -> io::Result<()> {
    let plain = fmt_time_short(t);
    // Split date / time for a two-tone look when well-formed.
    if let Some((date, time)) = plain.split_once(' ') {
        write!(out, "{DIM}{date}{RESET} {SOFT}{time}{RESET}")?;
        let used = plain.chars().count();
        pad(out, width.saturating_sub(used))?;
    } else {
        write!(out, "{DIM}{plain:<width$}{RESET}")?;
    }
    Ok(())
}

fn write_size(out: &mut impl Write, n: u64, width: usize) -> io::Result<()> {
    let plain = human_size(n);
    // Right-align the whole token, but paint unit quieter.
    let pad_n = width.saturating_sub(plain.len());
    for _ in 0..pad_n {
        write!(out, " ")?;
    }
    if let Some(i) = plain.find(|c: char| c.is_ascii_alphabetic()) {
        write!(out, "{SOFT}{}{RESET}{DIM}{}{RESET}", &plain[..i], &plain[i..])?;
    } else {
        write!(out, "{SOFT}{plain}{RESET}")?;
    }
    Ok(())
}

fn write_ino(out: &mut impl Write, e: &Entry, width: usize) -> io::Result<()> {
    let plain = format_ino(e);
    if let Some((ino, igen)) = plain.split_once(':') {
        write!(out, "{SOFT}{ino}{RESET}{DIM}:{igen}{RESET}")?;
        pad(out, width.saturating_sub(plain.len()))?;
    } else {
        write!(out, "{DIM}{plain:<width$}{RESET}")?;
    }
    Ok(())
}

fn write_badge(out: &mut impl Write, text: &str, width: usize) -> io::Result<()> {
    if text == "—" {
        write!(out, "{DIM}{text:<width$}{RESET}")
    } else {
        // Soft “chip” feel without true background colors (portable).
        write!(out, "{ORANGE}{text:<width$}{RESET}")
    }
}

/// USER column: `sveinn`, or `sveinn/staff` when group differs.
fn write_user_col(out: &mut impl Write, e: &Entry, width: usize) -> io::Result<()> {
    write!(out, "{SOFT_BLUE}{}{RESET}", e.user)?;
    if !same_owner(e) {
        write!(out, "{DIM}/{RESET}{SOFT_BLUE}{}{RESET}", e.group)?;
    }
    pad(out, width.saturating_sub(user_plain(e).chars().count()))
}

/// Optional GROUP column: group name only.
fn write_group_col(out: &mut impl Write, e: &Entry, width: usize) -> io::Result<()> {
    write!(out, "{SOFT_BLUE}{}{RESET}", e.group)?;
    pad(out, width.saturating_sub(group_plain(e).chars().count()))
}

fn same_owner(e: &Entry) -> bool {
    e.user == e.group
}

/// Other class: `[r-x]` + optional ACL/xattr markers.
fn write_other_col(out: &mut impl Write, e: &Entry, width: usize) -> io::Result<()> {
    let plain = other_plain(e);
    write!(out, "{DIM}[{RESET}")?;
    write_triad(out, &triad_other(e))?;
    write!(out, "{DIM}]{RESET}")?;
    if e.extras.has_acl {
        write!(out, "{LIGHT_BLUE}+{RESET}")?;
    } else if !e.extras.xattrs.is_empty() {
        write!(out, "{ORANGE}@{RESET}")?;
    }
    pad(out, width.saturating_sub(plain.chars().count()))
}

fn write_triad(out: &mut impl Write, triad: &str) -> io::Result<()> {
    for ch in triad.chars() {
        match ch {
            'r' => write!(out, "{WHITE}r{RESET}")?,
            'w' => write!(out, "{RED}w{RESET}")?,
            'x' => write!(out, "{GREEN}x{RESET}")?,
            's' | 'S' | 't' | 'T' => write!(out, "{ORANGE}{ch}{RESET}")?,
            '-' => write!(out, "{DIM}-{RESET}")?,
            other => write!(out, "{DIM}{other}{RESET}")?,
        }
    }
    Ok(())
}

/// PERMS: `[rwx][r-x][r-x] dir` — user/group/other triads, type word at end.
fn write_perms(out: &mut impl Write, e: &Entry, width: usize) -> io::Result<()> {
    let plain = perms_plain(e);

    for triad in [triad_user(e), triad_group(e), triad_other(e)] {
        write!(out, "{DIM}[{RESET}")?;
        write_triad(out, &triad)?;
        write!(out, "{DIM}]{RESET}")?;
    }
    if e.extras.has_acl {
        write!(out, "{LIGHT_BLUE}+{RESET}")?;
    } else if !e.extras.xattrs.is_empty() {
        write!(out, "{ORANGE}@{RESET}")?;
    }
    // Dim type word at the end of PERMS.
    write!(out, " {DIM}{}{RESET}", type_word(e))?;
    pad(out, width.saturating_sub(plain.chars().count()))
}

fn user_plain(e: &Entry) -> String {
    if same_owner(e) {
        e.user.clone()
    } else {
        format!("{}/{}", e.user, e.group)
    }
}

fn group_plain(e: &Entry) -> String {
    e.group.clone()
}

/// Plain PERMS for width: `[rwx][r-x][r-x] dir`.
fn perms_plain(e: &Entry) -> String {
    let mut s = format!(
        "[{}][{}][{}]",
        triad_user(e),
        triad_group(e),
        triad_other(e)
    );
    if e.extras.has_acl {
        s.push('+');
    } else if !e.extras.xattrs.is_empty() {
        s.push('@');
    }
    s.push(' ');
    s.push_str(type_word(e));
    s
}

fn triad_user(e: &Entry) -> String {
    bits_to_triad(e.mode, 0o400, 0o200, 0o100, Some(0o4000), false)
}

fn triad_group(e: &Entry) -> String {
    bits_to_triad(e.mode, 0o040, 0o020, 0o010, Some(0o2000), false)
}

fn triad_other(e: &Entry) -> String {
    bits_to_triad(e.mode, 0o004, 0o002, 0o001, None, true)
}

fn other_plain(e: &Entry) -> String {
    let mut s = format!("[{}]", triad_other(e));
    if e.extras.has_acl {
        s.push('+');
    } else if !e.extras.xattrs.is_empty() {
        s.push('@');
    }
    s
}

fn bits_to_triad(
    mode: u32,
    r: u32,
    w: u32,
    x: u32,
    special: Option<u32>,
    sticky: bool,
) -> String {
    let mut s = String::with_capacity(3);
    s.push(if mode & r != 0 { 'r' } else { '-' });
    s.push(if mode & w != 0 { 'w' } else { '-' });
    let exec = mode & x != 0;
    let ch = if sticky {
        let st = mode & 0o1000 != 0;
        match (exec, st) {
            (true, true) => 't',
            (false, true) => 'T',
            (true, false) => 'x',
            (false, false) => '-',
        }
    } else {
        let sp = special.is_some_and(|b| mode & b != 0);
        match (exec, sp) {
            (true, true) => 's',
            (false, true) => 'S',
            (true, false) => 'x',
            (false, false) => '-',
        }
    };
    s.push(ch);
    s
}

fn write_name(out: &mut impl Write, e: &Entry, width: usize) -> io::Result<()> {
    let color = color_for(e);
    write!(out, "{color}{name}{RESET}", name = e.name)?;

    let mut used = e.name.chars().count();
    if let Some(target) = &e.symlink {
        let tc = if e.broken_symlink { RED } else { ORANGE };
        write!(out, " {DIM}→{RESET} {tc}{target}{RESET}")?;
        used = width; // skip trailing pad for long targets
    }
    pad(out, width.saturating_sub(used))
}

fn format_blocks(e: &Entry) -> String {
    format!("{}b/{}", e.blocks, e.blksize)
}

fn format_ino(e: &Entry) -> String {
    let igen = e
        .extras
        .inode_gen
        .map(|g| g.to_string())
        .unwrap_or_else(|| "—".into());
    format!("{}:{}", e.ino, igen)
}

fn format_dev(e: &Entry) -> String {
    let mut s = format!("{}:{}", e.dev_major, e.dev_minor);
    if matches!(e.kind, Kind::Block | Kind::Char) {
        s.push_str(&format!(" ▸ {}:{}", e.rdev_major, e.rdev_minor));
    }
    s
}

fn format_xfs(e: &Entry) -> String {
    match e.xfs() {
        None => "—".into(),
        Some(x) => {
            let flags = if x.xflags.is_empty() {
                "—".into()
            } else {
                x.xflags.join(",")
            };
            let mut s = format!(
                "{flags} · exts={} · proj={} · esz={} · cow={}",
                x.nextents, x.projid, x.extsize, x.cowextsize
            );
            if let (Some(mem), Some(min), Some(max)) = (x.dio_mem, x.dio_min, x.dio_max) {
                s.push_str(&format!(" · dio={mem}/{min}/{max}"));
            }
            s
        }
    }
}

fn format_list_field(items: &[&str]) -> String {
    if items.is_empty() {
        "—".into()
    } else {
        items.join(" · ")
    }
}

fn format_list_field_owned(items: &[String]) -> String {
    if items.is_empty() {
        "—".into()
    } else {
        items.join(" · ")
    }
}

fn pad(out: &mut impl Write, n: usize) -> io::Result<()> {
    for _ in 0..n {
        write!(out, " ")?;
    }
    Ok(())
}

fn epoch_to_system(secs: i64) -> Option<SystemTime> {
    if secs <= 0 {
        None
    } else {
        Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
    }
}

pub fn human_size(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} {}", UNITS[0])
    } else if v >= 10.0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

fn fmt_time_short(t: Option<SystemTime>) -> String {
    match t.and_then(system_parts) {
        Some((y, mo, d, h, mi, s)) => format!("{d:02}-{mo:02}-{y:04} {h:02}:{mi:02}:{s:02}"),
        None => "—".into(),
    }
}

fn system_parts(t: SystemTime) -> Option<(u64, u64, u64, u64, u64, u64)> {
    let secs = t.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let z = secs / 86400;
    let tod = secs % 86400;
    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;

    let z = z as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    Some((y as u64, m as u64, d, h, mi, s))
}

