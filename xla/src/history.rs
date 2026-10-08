//! Which applications were launched most recently, remembered across runs.
//!
//! Without this the launcher offers the same alphabetical list every time and
//! the four programs actually used every day sit wherever the alphabet put
//! them. With it, an empty query is a most-recently-used list, and a query that
//! two applications match equally well resolves towards the one that was
//! wanted last time.
//!
//! The influence is deliberately small: [`History::bonus`] is capped below
//! what a prefix match is worth, so recency breaks ties between comparable
//! matches but cannot promote an application the query fits worse. A launcher
//! that reorders results out from under what you typed is worse than one that
//! never learns anything.
//!
//! Unlike xsw's window history this belongs in `$XDG_STATE_HOME` rather than
//! `$XDG_RUNTIME_DIR`: it is keyed by desktop entry id, which means the same
//! thing next week as it does today.

use std::path::PathBuf;

/// How many applications to remember. Past this, "recently used" has stopped
/// being true, and the file has to stop growing somewhere.
const MAX_ENTRIES: usize = 64;

/// How many of them the ranking bonus reaches, and the bonus the most recent
/// one gets.
const RANKED: usize = 20;

/// Launch history, most recent first.
#[derive(Debug, Default)]
pub struct History {
    order: Vec<String>,
}

impl History {
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

    /// Moves `id` to the front, which is what "just launched" means.
    pub fn promote(&mut self, id: &str) {
        if id.is_empty() {
            return;
        }
        self.order.retain(|entry| entry != id);
        self.order.insert(0, id.to_string());
        self.order.truncate(MAX_ENTRIES);
    }

    /// Writes the history back, creating its directory if needed.
    ///
    /// Best effort: losing it costs some ordering, which is not worth failing
    /// a launch over.
    pub fn save(&self) {
        let path = path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, self.order.join("\n"));
    }

    /// Ranking bonus for an application, larger the more recently it was
    /// launched, and zero for anything not in the history.
    pub fn bonus(&self, id: &str) -> i32 {
        match self.order.iter().position(|entry| entry == id) {
            Some(rank) if rank < RANKED => (RANKED - rank) as i32,
            _ => 0,
        }
    }
}

/// `~/.local/state/xla/history`, honouring `XDG_STATE_HOME`.
fn path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
        .join("xla/history")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(entries: &[&str]) -> History {
        History { order: entries.iter().map(|s| s.to_string()).collect() }
    }

    #[test]
    fn promote_moves_to_front_without_duplicating() {
        let mut h = history(&["a", "b", "c"]);
        h.promote("c");
        assert_eq!(h.order, ["c", "a", "b"]);
        h.promote("c");
        assert_eq!(h.order, ["c", "a", "b"], "already first");
    }

    #[test]
    fn promote_adds_unknown_entries() {
        let mut h = history(&["a"]);
        h.promote("new");
        assert_eq!(h.order, ["new", "a"]);
    }

    #[test]
    fn promote_ignores_empty_ids() {
        let mut h = history(&["a"]);
        h.promote("");
        assert_eq!(h.order, ["a"]);
    }

    #[test]
    fn promote_is_bounded() {
        let mut h = History::default();
        for i in 0..MAX_ENTRIES * 2 {
            h.promote(&format!("a{i}"));
        }
        assert_eq!(h.order.len(), MAX_ENTRIES);
        assert_eq!(h.order[0], format!("a{}", MAX_ENTRIES * 2 - 1), "newest kept");
    }

    #[test]
    fn bonus_decreases_with_age_and_runs_out() {
        let h = history(&["first", "second"]);
        assert_eq!(h.bonus("first"), RANKED as i32);
        assert_eq!(h.bonus("second"), RANKED as i32 - 1);
        assert_eq!(h.bonus("never-launched"), 0);

        // Deep history stops counting rather than going negative.
        let deep: Vec<String> = (0..MAX_ENTRIES).map(|i| format!("a{i}")).collect();
        let h = History { order: deep };
        assert_eq!(h.bonus(&format!("a{}", MAX_ENTRIES - 1)), 0);
    }

    #[test]
    fn the_bonus_cannot_beat_a_better_match() {
        // The property that keeps typed queries in charge: an application the
        // query names outright still wins over the most recently launched one
        // that merely contains it somewhere.
        let named = crate::search::score("firefox", "fire").unwrap();
        let mentioned = crate::search::score("some firefox thing", "fire").unwrap();
        assert!(named > mentioned + RANKED as i32, "{named} vs {mentioned} + {RANKED}");
    }
}
