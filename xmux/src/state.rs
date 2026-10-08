//! Session-layout persistence: every normal (non-agent) session — its
//! tabs, their split trees, and each shell's working directory — is
//! saved to `layout.json` next to the config every ten seconds, and
//! restored when the server starts. Shell *contents* (scrollback,
//! running programs) are not saved; restored panes are fresh shells
//! started in the saved directories.
//!
//! Writes are durable (fsync the temp file and the directory) and keep
//! the previous good file as `layout.json.bak`. A power cut used to
//! leave `layout.json` zero-length because the rename could hit disk
//! while the new inode's data was still in the page cache; restore then
//! treated that as a fresh start. On a missing, empty, or invalid
//! primary we fall back to the temp file (a completed write that had
//! not been renamed) and then the backup.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

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
fn path() -> Option<PathBuf> {
    Some(config::path()?.parent()?.join("layout.json"))
}

fn tmp_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn bak_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
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

/// Write `data` over `path` so a crash cannot leave a zero-length file.
///
/// 1. Write + fsync a sibling `.tmp`.
/// 2. If `path` holds a parseable layout, rename it to `.bak` (an empty
///    or corrupt primary is not promoted, so it cannot clobber a good
///    backup).
/// 3. Rename `.tmp` into place and fsync the directory.
fn durable_replace(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = tmp_path(path);
    let bak = bak_path(path);
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    if path.exists() {
        if is_plausible(path) {
            fs::rename(path, &bak)?;
        } else {
            let _ = fs::remove_file(path);
        }
    }
    fs::rename(&tmp, path)?;
    fsync_parent(path)?;
    Ok(())
}

fn is_plausible(path: &Path) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    !bytes.is_empty() && serde_json::from_slice::<SavedState>(&bytes).is_ok()
}

fn fsync_parent(path: &Path) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    File::open(dir)?.sync_all()
}

/// Write the current layout of every normal session to `layout.json`
/// (durable: fsynced temp file + rename, previous good file kept as
/// `.bak`). Errors are logged, not fatal.
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
        durable_replace(&path, json.as_bytes())
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

/// Read the first parseable layout among `path`, its `.tmp`, and its
/// `.bak`. Empty and invalid files are skipped (and logged) so a
/// zero-length primary from a crash still restores from the backup.
fn load_saved(path: &Path) -> Option<(SavedState, PathBuf)> {
    let tmp = tmp_path(path);
    let bak = bak_path(path);
    for candidate in [path, tmp.as_path(), bak.as_path()] {
        if let Some(state) = load_one(candidate) {
            return Some((state, candidate.to_path_buf()));
        }
    }
    None
}

fn load_one(path: &Path) -> Option<SavedState> {
    let text = match fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => text,
        Ok(_) => {
            eprintln!("ignoring empty {}", path.display());
            return None;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!("failed to read {}: {e}", path.display());
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(state) => Some(state),
        Err(e) => {
            eprintln!("ignoring invalid {}: {e}", path.display());
            None
        }
    }
}

/// Recreate the sessions saved in `layout.json`, spawning a fresh shell
/// per pane in its saved directory. A missing file means a fresh start;
/// an empty or broken primary falls back to `.tmp` then `.bak`.
pub fn restore(config: &Config) -> Vec<Session> {
    let Some(path) = path() else {
        return Vec::new();
    };
    let Some((state, used)) = load_saved(&path) else {
        return Vec::new();
    };

    // Restored shells have no agent titles yet, so the agent bar is
    // not on screen. Attaching resizes to the client's real chrome.
    let size = content_size(RESTORE_SIZE, false);
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
        if used == path {
            eprintln!(
                "restored {} session{} from {}",
                sessions.len(),
                if sessions.len() == 1 { "" } else { "s" },
                used.display()
            );
        } else {
            eprintln!(
                "restored {} session{} from {} ({} unreadable)",
                sessions.len(),
                if sessions.len() == 1 { "" } else { "s" },
                used.display(),
                path.display()
            );
        }
    }
    sessions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_DIR_SEQ: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "xmux-state-{}-{}",
            std::process::id(),
            TEST_DIR_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const WORK: &str = r#"{
  "sessions": [
    {
      "name": "work",
      "active_tab": 0,
      "tabs": [{ "name": "tab 1", "layout": { "type": "pane" } }]
    }
  ]
}"#;

    const NOTES: &str = r#"{
  "sessions": [
    {
      "name": "notes",
      "active_tab": 0,
      "tabs": [{ "name": "tab 1", "layout": { "type": "pane" } }]
    }
  ]
}"#;

    fn names(state: &SavedState) -> Vec<&str> {
        state.sessions.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn durable_replace_writes_primary() {
        let dir = scratch();
        let path = dir.join("layout.json");
        durable_replace(&path, WORK.as_bytes()).unwrap();
        assert_eq!(names(&load_saved(&path).unwrap().0), ["work"]);
        assert!(!tmp_path(&path).exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn durable_replace_keeps_previous_as_bak() {
        let dir = scratch();
        let path = dir.join("layout.json");
        durable_replace(&path, WORK.as_bytes()).unwrap();
        durable_replace(&path, NOTES.as_bytes()).unwrap();
        assert_eq!(names(&load_one(&path).unwrap()), ["notes"]);
        assert_eq!(names(&load_one(&bak_path(&path)).unwrap()), ["work"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_primary_does_not_clobber_bak() {
        let dir = scratch();
        let path = dir.join("layout.json");
        durable_replace(&path, WORK.as_bytes()).unwrap();
        durable_replace(&path, NOTES.as_bytes()).unwrap();
        fs::write(&path, b"").unwrap();
        durable_replace(&path, NOTES.as_bytes()).unwrap();
        assert_eq!(names(&load_one(&path).unwrap()), ["notes"]);
        assert_eq!(names(&load_one(&bak_path(&path)).unwrap()), ["work"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_saved_falls_back_to_bak_when_primary_empty() {
        let dir = scratch();
        let path = dir.join("layout.json");
        fs::write(&path, b"").unwrap();
        fs::write(bak_path(&path), WORK).unwrap();
        let (state, used) = load_saved(&path).unwrap();
        assert_eq!(names(&state), ["work"]);
        assert_eq!(used, bak_path(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_saved_falls_back_to_bak_when_primary_invalid() {
        let dir = scratch();
        let path = dir.join("layout.json");
        fs::write(&path, b"{").unwrap();
        fs::write(bak_path(&path), WORK).unwrap();
        let (state, used) = load_saved(&path).unwrap();
        assert_eq!(names(&state), ["work"]);
        assert_eq!(used, bak_path(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_saved_prefers_tmp_over_bak_when_primary_missing() {
        let dir = scratch();
        let path = dir.join("layout.json");
        fs::write(tmp_path(&path), NOTES).unwrap();
        fs::write(bak_path(&path), WORK).unwrap();
        let (state, used) = load_saved(&path).unwrap();
        assert_eq!(names(&state), ["notes"]);
        assert_eq!(used, tmp_path(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_saved_prefers_valid_primary() {
        let dir = scratch();
        let path = dir.join("layout.json");
        fs::write(&path, NOTES).unwrap();
        fs::write(bak_path(&path), WORK).unwrap();
        let (state, used) = load_saved(&path).unwrap();
        assert_eq!(names(&state), ["notes"]);
        assert_eq!(used, path);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_file_is_not_plausible() {
        let dir = scratch();
        let path = dir.join("layout.json");
        fs::write(&path, b"").unwrap();
        assert!(!is_plausible(&path));
        fs::write(&path, WORK).unwrap();
        assert!(is_plausible(&path));
        let _ = fs::remove_dir_all(&dir);
    }
}
