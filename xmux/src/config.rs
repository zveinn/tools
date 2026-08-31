//! Config, loaded from `$HOME/.config/rmux/config.yaml`:
//!
//! ```yaml
//! start_dir: /home/you/code
//! commands:
//!   alt+h: htop
//!   alt+g: lazygit
//! keybindings:
//!   session-manager: ctrl+o
//!   split-vertical: alt+v
//! sessions:
//!   1: { name: meow, key: F1 }
//!   5: { name: scratch, key: F5 }
//! ```
//!
//! `start_dir` is where new shells start (unset = your home directory);
//! `commands` type a program into the shell; `keybindings` rebind the
//! server's own controls (any action not listed keeps its default);
//! `sessions` pin sessions to slots in the session list — the slot
//! number orders them (gaps collapse in the display) and the key opens
//! the session from anywhere, starting it if it isn't running.
//! Keys are `[ctrl+][alt+]<char>` or `F1`-`F12`; ctrl+letter becomes the
//! matching C0 control byte (0x01-0x1a), an alt prefix a leading ESC,
//! and F-keys their escape sequences.

use std::collections::HashMap;

use crossterm::style::Color;

use crate::input::{InputAction, Overlay};
use crate::model::NavDir;

/// What a bound key does.
pub enum BindingAction {
    /// Type this program into the shell and press Enter.
    Run(String),
    /// One of the server's own controls.
    Control(InputAction),
}

pub struct Binding {
    /// Byte sequence the client's terminal sends for this chord.
    pub seq: Vec<u8>,
    pub action: BindingAction,
}

/// A session pinned to a slot in the session list. Pins are kept in
/// slot order; the slot numbers themselves only order the list (gaps
/// collapse in the display).
pub struct Pin {
    pub name: String,
}

pub struct Config {
    /// All key bindings (controls, command bindings, session keys), longest
    /// sequence first.
    pub bindings: Vec<Binding>,
    /// Pinned sessions in slot order.
    pub pins: Vec<Pin>,
    /// UI accent color (`accent: "#7aa2f7"`); defaults to the terminal
    /// palette's cyan so it follows the theme.
    pub accent: Color,
    /// Environment variables set in every spawned shell
    /// (`terminal_envs:`). Defaults to just `TERM=xterm-256color` when
    /// the section is absent; when present, it is used exactly as given.
    pub envs: Vec<(String, String)>,
    /// Shell to spawn (`shell: /usr/bin/fish`). When unset, falls back
    /// to $SHELL, then the passwd entry, then /bin/sh.
    pub shell: Option<String>,
    /// Directory new shells start in (`start_dir: /home/you/code`).
    /// When unset, the user's home directory.
    pub start_dir: Option<String>,
    /// Lines of scrollback kept per pane (`scrollback_lines: 5000`).
    /// Applies to shells spawned after a change.
    pub scrollback_lines: usize,
    /// Mouse select-to-copy (`select_copy: true`): drag selects text in
    /// a pane, releasing copies it to the client's clipboard via OSC 52.
    pub select_copy: bool,
    /// Tab bar at the top of the screen (`bar_position: top`) instead
    /// of the default bottom.
    pub bar_top: bool,
}

/// The server's controls: config name, default key, action.
const ACTIONS: [(&str, &str, InputAction); 12] = [
    ("session-manager", "ctrl+o", InputAction::Manager(Overlay::Sessions { agents: false })),
    ("tab-manager", "ctrl+n", InputAction::Manager(Overlay::Tabs)),
    ("split-horizontal", "ctrl+k", InputAction::SplitH),
    ("split-vertical", "ctrl+l", InputAction::SplitV),
    ("focus-next", "ctrl+t", InputAction::FocusNext),
    ("focus-left", "ctrl+q", InputAction::FocusDir(NavDir::Left)),
    ("focus-right", "ctrl+w", InputAction::FocusDir(NavDir::Right)),
    ("focus-up", "ctrl+e", InputAction::FocusDir(NavDir::Up)),
    ("focus-down", "ctrl+r", InputAction::FocusDir(NavDir::Down)),
    ("detach", "ctrl+g", InputAction::Detach),
    ("fullscreen", "ctrl+f", InputAction::Fullscreen),
    ("terminal-settings", "ctrl+s", InputAction::PaneSettings),
];

#[derive(serde::Deserialize)]
struct RawPin {
    name: String,
    key: String,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    commands: HashMap<String, String>,
    #[serde(default)]
    keybindings: HashMap<String, String>,
    #[serde(default)]
    sessions: HashMap<u32, RawPin>,
    #[serde(default)]
    accent: Option<String>,
    #[serde(default)]
    terminal_envs: Option<HashMap<String, String>>,
    #[serde(default)]
    shell: Option<String>,
    #[serde(default)]
    start_dir: Option<String>,
    #[serde(default)]
    scrollback_lines: Option<usize>,
    #[serde(default)]
    select_copy: Option<bool>,
    #[serde(default)]
    bar_position: Option<String>,
}

pub fn load() -> Result<Config, String> {
    let raw = match read_config()? {
        Some((raw, path)) => {
            // Reject unknown action names early with the file context.
            for name in raw.keybindings.keys() {
                if !ACTIONS.iter().any(|(n, ..)| n == name) {
                    let known: Vec<&str> = ACTIONS.iter().map(|(n, ..)| *n).collect();
                    return Err(format!(
                        "unknown keybinding \"{name}\" in {path} (known: {})",
                        known.join(", ")
                    ));
                }
            }
            raw
        }
        None => RawConfig {
            commands: HashMap::new(),
            keybindings: HashMap::new(),
            sessions: HashMap::new(),
            accent: None,
            terminal_envs: None,
            shell: None,
            start_dir: None,
            scrollback_lines: None,
            select_copy: None,
            bar_position: None,
        },
    };

    let accent = match &raw.accent {
        Some(spec) => parse_accent(spec)?,
        None => Color::Cyan,
    };

    let envs: Vec<(String, String)> = match raw.terminal_envs {
        Some(map) => {
            for key in map.keys() {
                if key.is_empty() || key.contains('=') || key.contains('\0') {
                    return Err(format!("invalid terminal_envs name \"{key}\""));
                }
            }
            let mut envs: Vec<(String, String)> = map.into_iter().collect();
            envs.sort();
            envs
        }
        None => vec![("TERM".to_string(), "xterm-256color".to_string())],
    };

    let mut bindings = Vec::new();

    // Controls: user's key if rebound, else the default.
    for (name, default_key, action) in ACTIONS {
        let key = raw.keybindings.get(name).map_or(default_key, |k| k.as_str());
        let seqs = parse_key_multi(key)
            .map_err(|e| format!("invalid key \"{key}\" for keybinding \"{name}\": {e}"))?;
        for seq in seqs {
            bindings.push(Binding {
                seq,
                action: BindingAction::Control(action),
            });
        }
    }

    // Command bindings: the chord types a program into the shell.
    for (key, run) in raw.commands {
        let seqs =
            parse_key_multi(&key).map_err(|e| format!("invalid command binding \"{key}\": {e}"))?;
        for seq in seqs {
            bindings.push(Binding {
                seq,
                action: BindingAction::Run(run.clone()),
            });
        }
    }

    // Pinned sessions, sorted by slot; each gets an OpenSession binding.
    let mut slots: Vec<(&u32, &RawPin)> = raw.sessions.iter().collect();
    slots.sort_by_key(|(slot, _)| **slot);
    let mut pins = Vec::new();
    for (slot, pin) in slots {
        if pin.name.trim().is_empty() {
            return Err(format!("session slot {slot}: name must not be empty"));
        }
        if pins.iter().any(|p: &Pin| p.name == pin.name.trim()) {
            return Err(format!(
                "session \"{}\" is pinned to more than one slot",
                pin.name.trim()
            ));
        }
        let seqs = parse_key_multi(&pin.key).map_err(|e| {
            format!("invalid key \"{}\" for session slot {slot}: {e}", pin.key)
        })?;
        for seq in seqs {
            bindings.push(Binding {
                seq,
                action: BindingAction::Control(InputAction::OpenSession(pins.len())),
            });
        }
        pins.push(Pin {
            name: pin.name.trim().to_string(),
        });
    }

    // The same chord bound twice would be ambiguous.
    for (i, a) in bindings.iter().enumerate() {
        if bindings[i + 1..].iter().any(|b| b.seq == a.seq) {
            return Err(format!(
                "duplicate key binding: {} is bound more than once",
                describe_seq(&a.seq)
            ));
        }
    }

    // Longest sequence first, so e.g. ctrl+alt+h wins over ctrl+h when
    // both could match at the same input position.
    bindings.sort_by_key(|b| std::cmp::Reverse(b.seq.len()));
    let shell = match raw.shell {
        Some(s) => {
            let s = s.trim().to_string();
            if s.is_empty() {
                return Err("shell must not be empty".to_string());
            }
            // Absolute paths can be checked up front; bare names are
            // resolved via PATH at spawn time.
            if s.contains('/') && !std::path::Path::new(&s).exists() {
                return Err(format!("shell \"{s}\" does not exist"));
            }
            Some(s)
        }
        None => None,
    };

    let start_dir = match raw.start_dir {
        Some(s) => {
            let s = s.trim();
            if s.is_empty() {
                None // same as unset: the home directory
            } else {
                // Allow `~/...` for convenience.
                let expanded = match (s.strip_prefix("~"), std::env::var_os("HOME")) {
                    (Some(rest), Some(home)) => {
                        format!("{}{rest}", home.to_string_lossy())
                    }
                    _ => s.to_string(),
                };
                if !std::path::Path::new(&expanded).is_dir() {
                    return Err(format!("start_dir \"{s}\" is not a directory"));
                }
                Some(expanded)
            }
        }
        None => None,
    };

    let scrollback_lines = match raw.scrollback_lines {
        Some(n) if n > 1_000_000 => {
            return Err(format!("scrollback_lines {n} is too large (max 1000000)"));
        }
        Some(n) => n,
        None => 5000,
    };

    let bar_top = match raw.bar_position.as_deref().map(str::trim) {
        None | Some("") | Some("bottom") => false,
        Some("top") => true,
        Some(other) => {
            return Err(format!(
                "invalid bar_position \"{other}\" (expected top or bottom)"
            ));
        }
    };

    Ok(Config {
        bindings,
        pins,
        accent,
        envs,
        shell,
        start_dir,
        scrollback_lines,
        select_copy: raw.select_copy.unwrap_or(true),
        bar_top,
    })
}

/// Parse `#rrggbb` (or shorthand `#rgb`) into an RGB color.
fn parse_accent(spec: &str) -> Result<Color, String> {
    let hex = spec.trim().trim_start_matches('#');
    let expand = |h: &str| -> Option<(u8, u8, u8)> {
        let full: String = match h.len() {
            3 => h.chars().flat_map(|c| [c, c]).collect(),
            6 => h.to_string(),
            _ => return None,
        };
        let byte = |i| u8::from_str_radix(&full[i..i + 2], 16).ok();
        Some((byte(0)?, byte(2)?, byte(4)?))
    };
    match expand(hex) {
        Some((r, g, b)) => Ok(Color::Rgb { r, g, b }),
        None => Err(format!(
            "invalid accent \"{spec}\" (expected hex like #7aa2f7)"
        )),
    }
}

/// Directory override from `--config <dir>` (config.yaml and
/// layout.json both live in it).
static CONFIG_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Set the config directory (from the `--config` flag); first call wins.
pub fn set_dir(dir: std::path::PathBuf) {
    let _ = CONFIG_DIR.set(dir);
}

/// Where the config lives: `<--config dir>/config.yaml` when the flag
/// was given, else `~/.config/rmux/config.yaml`.
pub fn path() -> Option<std::path::PathBuf> {
    if let Some(dir) = CONFIG_DIR.get() {
        return Some(dir.join("config.yaml"));
    }
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join(".config/rmux/config.yaml"))
}

/// Written to `~/.config/rmux/config.yaml` when no config exists, so
/// there is always a file to edit.
const DEFAULT_CONFIG: &str = include_str!("../default-config.yaml");

fn read_config() -> Result<Option<(RawConfig, String)>, String> {
    let Some(path) = path() else {
        return Ok(None);
    };

    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // No config file: create the default one, then run with it. The
        // write is best-effort — a read-only $HOME still gets defaults.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            match std::fs::write(&path, DEFAULT_CONFIG) {
                Ok(()) => eprintln!("created default config at {}", path.display()),
                Err(e) => eprintln!(
                    "could not create default config at {}: {e}",
                    path.display()
                ),
            }
            DEFAULT_CONFIG.to_string()
        }
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };

    let raw: RawConfig = serde_yaml::from_str(&text)
        .map_err(|e| format!("invalid config {}: {e}", path.display()))?;
    Ok(Some((raw, path.display().to_string())))
}

/// Human-readable form of a chord's byte sequence for error messages.
fn describe_seq(seq: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut rest = seq;
    if rest.first() == Some(&0x1b) && rest.len() > 1 {
        parts.push("alt".to_string());
        rest = &rest[1..];
    }
    match rest {
        [b] if *b < 0x20 => parts.push(format!("ctrl+{}", (b'a' + (b - 1)) as char)),
        _ => parts.push(String::from_utf8_lossy(rest).into_owned()),
    }
    parts.join("+")
}

/// Like `parse_key`, but also accepts `F1`-`F12`. F-keys have two
/// encodings in the wild (SS3 for F1-F4 in most terminals, plus the
/// legacy CSI form), so a spec may map to several byte sequences.
fn parse_key_multi(spec: &str) -> Result<Vec<Vec<u8>>, String> {
    let lower = spec.trim().to_ascii_lowercase();
    if let Some(n) = lower.strip_prefix('f').and_then(|r| r.parse::<u8>().ok()) {
        let seqs: &[&[u8]] = match n {
            1 => &[b"\x1bOP", b"\x1b[11~"],
            2 => &[b"\x1bOQ", b"\x1b[12~"],
            3 => &[b"\x1bOR", b"\x1b[13~"],
            4 => &[b"\x1bOS", b"\x1b[14~"],
            5 => &[b"\x1b[15~"],
            6 => &[b"\x1b[17~"],
            7 => &[b"\x1b[18~"],
            8 => &[b"\x1b[19~"],
            9 => &[b"\x1b[20~"],
            10 => &[b"\x1b[21~"],
            11 => &[b"\x1b[23~"],
            12 => &[b"\x1b[24~"],
            _ => return Err(format!("F{n} is not supported (F1-F12)")),
        };
        return Ok(seqs.iter().map(|s| s.to_vec()).collect());
    }
    Ok(vec![parse_key(spec)?])
}

/// Translate a `[ctrl+][alt+]<char>` spec into the bytes the client's
/// terminal sends for that chord.
fn parse_key(spec: &str) -> Result<Vec<u8>, String> {
    let parts: Vec<&str> = spec.split('+').collect();
    let (mods, key) = parts.split_at(parts.len() - 1);

    let (mut ctrl, mut alt) = (false, false);
    for m in mods {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" => ctrl = true,
            "alt" => alt = true,
            other => return Err(format!("unknown modifier \"{other}\" (use ctrl/alt)")),
        }
    }

    let mut chars = key[0].chars();
    let (Some(ch), None) = (chars.next(), chars.next()) else {
        return Err(format!("key must be a single character, got \"{}\"", key[0]));
    };

    // A binding without modifiers would hijack ordinary typing.
    if !ctrl && !alt {
        return Err("key needs at least one modifier (ctrl/alt)".into());
    }

    let mut seq = Vec::with_capacity(5);
    if alt {
        seq.push(0x1b); // ESC prefix
    }
    if ctrl {
        if !ch.is_ascii_alphabetic() {
            return Err("ctrl+ bindings only support letters a-z".into());
        }
        seq.push(ch.to_ascii_lowercase() as u8 & 0x1f);
    } else {
        let mut buf = [0u8; 4];
        seq.extend_from_slice(ch.to_ascii_lowercase().encode_utf8(&mut buf).as_bytes());
    }
    Ok(seq)
}
