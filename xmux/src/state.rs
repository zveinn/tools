//! Session-layout persistence: every normal (non-agent) session — its
//! tabs, their split trees, and each shell's working directory — is
//! saved to `layout.json` next to the config every ten seconds, and
//! restored when the server starts. Shell *contents* (scrollback,
//! running programs) are not saved; restored panes are fresh shells
//! started in the saved directories.

use serde::{Deserialize, Serialize};

use crate::Result;
use crate::config::{self, Config};
use crate::model::{Layout, Pane, Rect, Session, SplitDir, Tab, split_rect};
use crate::render::content_size;

/// Content size restored sessions are laid out for until a client
/// attaches and resizes them (matches the agent-session default).
const RESTORE_SIZE: (u16, u16) = (120, 32);

#[derive(Serialize, Deserialize)]
struct SavedState {
    sessions: Vec<SavedSession>,
}

#[derive(Serialize, Deserialize)]
struct SavedSession {
    name: String,
    active_tab: usize,
    tabs: Vec<SavedTab>,
}

#[derive(Serialize, Deserialize)]
struct SavedTab {
    name: String,
    layout: SavedLayout,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum SavedLayout {
    /// A shell, with the directory it was in (absent when unknown).
    Pane {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// Typed + run in the restored shell (terminal-settings prompt).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_run: Option<String>,
    },
    Split {
        dir: SavedDir,
        a: Box<SavedLayout>,
        b: Box<SavedLayout>,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
enum SavedDir {
    Horizontal,
    Vertical,
}

/// Where the state lives (`~/.config/xmux/layout.json`).
fn path() -> Option<std::path::PathBuf> {
    Some(config::path()?.parent()?.join("layout.json"))
}

// ---------------------------------------------------------------------
// Saving
// ---------------------------------------------------------------------

fn capture_layout(layout: &Layout) -> SavedLayout {
    match layout {
        // `Empty` is a transient placeholder; treat it as a plain pane.
        Layout::Empty => SavedLayout::Pane {
            cwd: None,
            auto_run: None,
        },
        Layout::Leaf(pane) => SavedLayout::Pane {
            cwd: pane.pty.cwd(),
            auto_run: pane.auto_run.clone(),
        },
        Layout::Split { dir, a, b } => SavedLayout::Split {
            dir: match dir {
                SplitDir::Horizontal => SavedDir::Horizontal,
                SplitDir::Vertical => SavedDir::Vertical,
            },
            a: Box::new(capture_layout(a)),
            b: Box::new(capture_layout(b)),
        },
    }
}

/// Write the current layout of every normal session to `layout.json`
/// (atomically: temp file + rename). Errors are logged, not fatal.
pub fn save(sessions: &[Session]) {
    let Some(path) = path() else { return };
    let state = SavedState {
        sessions: sessions
            .iter()
            .filter(|s| !s.agent)
            .map(|s| SavedSession {
                name: s.name.clone(),
                active_tab: s.active_tab,
                tabs: s
                    .tabs
                    .iter()
                    .map(|t| SavedTab {
                        name: t.name.clone(),
                        layout: capture_layout(&t.layout),
                    })
                    .collect(),
            })
            .collect(),
    };
    let write = (|| -> Result<()> {
        let json = serde_json::to_string_pretty(&state)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if let Err(e) = write {
        eprintln!("failed to save {}: {e}", path.display());
    }
}

// ---------------------------------------------------------------------
// Restoring
// ---------------------------------------------------------------------

fn build_layout(saved: &SavedLayout, rect: Rect, config: &Config) -> Result<Layout> {
    Ok(match saved {
        SavedLayout::Pane { cwd, auto_run } => {
            let mut pane = Pane::new_in((rect.w.max(1), rect.h.max(1)), config, cwd.as_deref())?;
            if let Some(cmd) = auto_run {
                // Typed into the shell: it sits in the pty buffer until
                // the shell reads it, joins history, answers Ctrl+C.
                pane.pty.write(cmd.as_bytes());
                pane.pty.write(b"\r");
                pane.auto_run = Some(cmd.clone());
            }
            Layout::Leaf(pane)
        }
        SavedLayout::Split { dir, a, b } => {
            let dir = match dir {
                SavedDir::Horizontal => SplitDir::Horizontal,
                SavedDir::Vertical => SplitDir::Vertical,
            };
            let (ra, rb) = split_rect(dir, rect);
            Layout::Split {
                dir,
                a: Box::new(build_layout(a, ra, config)?),
                b: Box::new(build_layout(b, rb, config)?),
            }
        }
    })
}

/// Recreate the sessions saved in `layout.json`, spawning a fresh shell
/// per pane in its saved directory. A missing file means a fresh start;
/// a broken one is logged and skipped.
pub fn restore(config: &Config) -> Vec<Session> {
    let Some(path) = path() else {
        return Vec::new();
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!("failed to read {}: {e}", path.display());
            return Vec::new();
        }
    };
    let state: SavedState = match serde_json::from_str(&text) {
        Ok(state) => state,
        Err(e) => {
            eprintln!("ignoring invalid {}: {e}", path.display());
            return Vec::new();
        }
    };

    let size = content_size(RESTORE_SIZE);
    let full = Rect {
        x: 0,
        y: 0,
        w: size.0,
        h: size.1,
    };
    let mut sessions = Vec::new();
    for saved in &state.sessions {
        let build = (|| -> Result<Session> {
            let mut tabs = Vec::new();
            for tab in &saved.tabs {
                let layout = build_layout(&tab.layout, full, config)?;
                let focused = layout.panes().first().map_or(0, |p| p.id);
                tabs.push(Tab {
                    name: tab.name.clone(),
                    layout,
                    focused,
                    zoomed: false,
                });
            }
            if tabs.is_empty() {
                return Err("session has no tabs".into());
            }
            Ok(Session::restore(
                saved.name.clone(),
                tabs,
                saved.active_tab,
                size,
            ))
        })();
        match build {
            Ok(session) => sessions.push(session),
            Err(e) => eprintln!("could not restore session \"{}\": {e}", saved.name),
        }
    }
    if !sessions.is_empty() {
        eprintln!(
            "restored {} session{} from {}",
            sessions.len(),
            if sessions.len() == 1 { "" } else { "s" },
            path.display()
        );
    }
    sessions
}
