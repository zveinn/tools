//! Most-recently-used window ordering, remembered across invocations.
//!
//! `ext-foreign-toplevel-list` announces windows in creation order and exposes
//! no notion of recency, so Alt-Tab ordering has to be remembered by us. Every
//! run records which window the compositor reported as focused, and which one
//! it switched to, keyed by the protocol's opaque `identifier` — a field that
//! exists precisely so a toplevel can be recognised across separate
//! connections.
//!
//! The file lives in `$XDG_RUNTIME_DIR`, which is the right lifetime: those
//! identifiers are only meaningful while the compositor that issued them is
//! running, and the directory is emptied when the session ends. Identifiers
//! for windows that have since closed are harmless, since ranking only ever
//! looks up windows that currently exist.

use std::path::PathBuf;

/// How many windows to remember. Anything past this is older than any
/// plausible Alt-Tab reach, and keeps the file from growing without bound
/// across a long session.
const MAX_ENTRIES: usize = 64;

/// Focus history, most recent first.
#[derive(Debug, Default)]
pub struct Mru {
    order: Vec<String>,
}

impl Mru {
    /// Reads the history, treating any problem as "no history".
    pub fn load() -> Self {
        let order = std::fs::read_to_string(path())
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Self { order }
    }

    /// Moves `identifier` to the front, which is what "just used" means.
    pub fn promote(&mut self, identifier: &str) {
        if identifier.is_empty() {
            return;
        }
        self.order.retain(|entry| entry != identifier);
        self.order.insert(0, identifier.to_string());
        self.order.truncate(MAX_ENTRIES);
    }

    /// Writes the history back. Best effort: losing it costs one invocation's
    /// worth of ordering, which is not worth failing the switcher over.
    pub fn save(&self) {
        let _ = std::fs::write(path(), self.order.join("\n"));
    }

    /// Position in the history, or `None` for a window never seen focused.
    fn rank(&self, identifier: &str) -> Option<usize> {
        self.order.iter().position(|entry| entry == identifier)
    }

    /// Reorders `windows` most-recently-used first.
    ///
    /// Windows with no history sort last and keep their announcement order
    /// relative to each other, because `sort_by_key` is stable. In practice
    /// there are few of them: opening a window focuses it, and the focused
    /// window is recorded on every run.
    pub fn sort<T>(&self, windows: &mut [T], identifier: impl Fn(&T) -> &str) {
        windows.sort_by_key(|window| self.rank(identifier(window)).unwrap_or(usize::MAX));
    }
}

/// One history file per Wayland display, matching the socket in [`crate::ipc`].
fn path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);

    // WAYLAND_DISPLAY may be an absolute path, which would turn the file name
    // into directories that do not exist.
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
    let display = std::path::Path::new(&display)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("wayland-0")
        .to_string();

    dir.join(format!("xsw-mru-{display}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mru(entries: &[&str]) -> Mru {
        Mru { order: entries.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn promote_moves_to_front_without_duplicating() {
        let mut m = mru(&["a", "b", "c"]);
        m.promote("c");
        assert_eq!(m.order, ["c", "a", "b"]);
        m.promote("c");
        assert_eq!(m.order, ["c", "a", "b"], "already first");
    }

    #[test]
    fn promote_adds_unknown_entries() {
        let mut m = mru(&["a"]);
        m.promote("new");
        assert_eq!(m.order, ["new", "a"]);
    }

    #[test]
    fn promote_ignores_empty_identifiers() {
        let mut m = mru(&["a"]);
        m.promote("");
        assert_eq!(m.order, ["a"]);
    }

    #[test]
    fn promote_is_bounded() {
        let mut m = Mru::default();
        for i in 0..MAX_ENTRIES * 2 {
            m.promote(&format!("w{i}"));
        }
        assert_eq!(m.order.len(), MAX_ENTRIES);
        assert_eq!(m.order[0], format!("w{}", MAX_ENTRIES * 2 - 1), "newest kept");
    }

    #[test]
    fn sort_puts_recent_first_and_unknown_last_in_order() {
        // "b" was used most recently, "a" before that; "x" and "y" are unseen.
        let m = mru(&["b", "a"]);
        let mut windows = vec!["x", "a", "y", "b"];
        m.sort(&mut windows, |w| w);
        assert_eq!(windows, ["b", "a", "x", "y"]);
    }

    #[test]
    fn sort_leaves_a_fully_unknown_list_alone() {
        let m = Mru::default();
        let mut windows = vec!["x", "y", "z"];
        m.sort(&mut windows, |w| w);
        assert_eq!(windows, ["x", "y", "z"]);
    }
}
