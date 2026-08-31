//! Agent mode: a non-interactive control surface for LLM agents (and
//! scripts). `xmux agent new/kill/send/read` are one-shot commands that
//! speak the normal socket protocol — no pty or attach needed.
//!
//! Sessions created here are marked as *agent sessions*: they behave
//! like normal sessions (attachable, shells on ptys, tabs, splits) but
//! are listed separately in the session manager (toggle with `a`), and
//! the agent commands refuse to touch anything else — an agent can
//! never kill or type into a user's session.
//!
//! Wire format: each command is one frame; payload fields are utf-8
//! separated by NUL bytes.
//!   NEW    name [\0 tab]         create session (or add a tab to one)
//!   KILL   name [\0 tab]         kill session (or one tab of it)
//!   SEND   name \0 tab \0 text   type text + Enter (empty tab = active)
//!   READ   name \0 tab           plain-text screen (empty tab = active)
//!   RENAME name \0 new-name      rename an agent session
//!
//! `new`/`send`/`read` bump the session's activity timestamp; agent
//! sessions are listed most-recently-active first.

use std::collections::HashMap;
use std::io::{Read, Write};

use libghostty_vt::{
    render::{CellIterator, RenderState, RowIterator},
    screen::CellWide,
};

use crate::Result;
use crate::config::{Config, Pin};
use crate::input::{SessionEntry, create_session};
use crate::model::{Rect, Session, Tab};
use crate::protocol::{
    self, C2S_AGENT_KILL, C2S_AGENT_NEW, C2S_AGENT_READ, C2S_AGENT_RENAME, C2S_AGENT_SEND,
    FrameReader, S2C_AGENT_ERR, S2C_AGENT_OK, frame,
};

/// Content size for sessions created without a client to size them
/// (matches a common terminal, minus the tab bar row).
const DEFAULT_SIZE: (u16, u16) = (120, 31);

const USAGE: &str = "usage: xmux agent new <session> [tab]\n\
       xmux agent kill <session> [tab]\n\
       xmux agent send <session> [-t tab] <text...>\n\
       xmux agent read <session> [-t tab]\n\
       xmux agent rename <session> <new-name>";

// ---------------------------------------------------------------------
// Client side: parse argv, send one frame, print the reply.
// ---------------------------------------------------------------------

/// Run `xmux agent <cmd> ...`: send the command, print the server's
/// reply, error out (nonzero exit) when the server reports a failure.
pub fn run(args: &[String]) -> Result<()> {
    let (kind, payload) = parse_args(args)?;

    let mut stream = protocol::connect()?;
    stream.write_all(&frame(kind, &payload))?;

    let mut reader = FrameReader::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Err("server closed the connection without a reply \
                        (server too old? restart it after upgrading)"
                .into());
        }
        reader.extend(&buf[..n]);
        while let Some((kind, payload)) = reader.next_frame().map_err(std::io::Error::other)? {
            let text = String::from_utf8_lossy(&payload).into_owned();
            match kind {
                S2C_AGENT_OK => {
                    if !text.is_empty() {
                        println!("{}", text.trim_end_matches('\n'));
                    }
                    return Ok(());
                }
                S2C_AGENT_ERR => {
                    eprintln!("xmux agent: {text}");
                    std::process::exit(1);
                }
                _ => {}
            }
        }
    }
}

/// Translate agent argv into a protocol frame.
fn parse_args(args: &[String]) -> Result<(u8, Vec<u8>)> {
    let arg = |i: usize| args.get(i).map(String::as_str);
    let name = |i: usize| -> Result<&str> {
        match arg(i) {
            Some(n) if !n.trim().is_empty() && !n.contains('\0') => Ok(n),
            _ => Err(USAGE.into()),
        }
    };
    match arg(0) {
        Some("new") => {
            let mut payload = name(1)?.as_bytes().to_vec();
            if let Some(tab) = arg(2) {
                payload.push(0);
                payload.extend_from_slice(tab.as_bytes());
            }
            Ok((C2S_AGENT_NEW, payload))
        }
        Some("kill") => {
            let mut payload = name(1)?.as_bytes().to_vec();
            if let Some(tab) = arg(2) {
                payload.push(0);
                payload.extend_from_slice(tab.as_bytes());
            }
            Ok((C2S_AGENT_KILL, payload))
        }
        Some("send") => {
            let session = name(1)?;
            let (tab, text_from) = if arg(2) == Some("-t") {
                (name(3)?, 4)
            } else {
                ("", 2)
            };
            if args.len() <= text_from {
                return Err(USAGE.into());
            }
            let text = args[text_from..].join(" ");
            let mut payload = session.as_bytes().to_vec();
            payload.push(0);
            payload.extend_from_slice(tab.as_bytes());
            payload.push(0);
            payload.extend_from_slice(text.as_bytes());
            Ok((C2S_AGENT_SEND, payload))
        }
        Some("read") => {
            let session = name(1)?;
            let tab = if arg(2) == Some("-t") { name(3)? } else { "" };
            let mut payload = session.as_bytes().to_vec();
            payload.push(0);
            payload.extend_from_slice(tab.as_bytes());
            Ok((C2S_AGENT_READ, payload))
        }
        Some("rename") => {
            let mut payload = name(1)?.as_bytes().to_vec();
            payload.push(0);
            payload.extend_from_slice(name(2)?.as_bytes());
            Ok((C2S_AGENT_RENAME, payload))
        }
        _ => Err(USAGE.into()),
    }
}

// ---------------------------------------------------------------------
// Server side: execute one agent command against the session list.
// ---------------------------------------------------------------------

/// Whether a frame kind is an agent command (server dispatch helper).
pub fn is_agent_frame(kind: u8) -> bool {
    matches!(
        kind,
        C2S_AGENT_NEW | C2S_AGENT_KILL | C2S_AGENT_SEND | C2S_AGENT_READ | C2S_AGENT_RENAME
    )
}

/// Execute an agent command. `Ok` text goes back as `S2C_AGENT_OK`,
/// `Err` text as `S2C_AGENT_ERR`. Returns whether sessions were
/// created/removed (the caller then rehomes clients and redraws).
pub fn execute(
    kind: u8,
    payload: &[u8],
    sessions: &mut Vec<Session>,
    config: &Config,
) -> (std::result::Result<String, String>, bool) {
    let fields: Vec<String> = payload
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).trim().to_string())
        .collect();
    let name = fields[0].clone();
    if name.is_empty() {
        return (Err("empty session name".into()), false);
    }

    match kind {
        C2S_AGENT_NEW => match fields.get(1) {
            None => match new_session(sessions, config, &name) {
                Ok(msg) => (Ok(msg), true),
                Err(e) => (Err(e), false),
            },
            Some(tab) => (new_tab(sessions, config, &name, tab), false),
        },
        C2S_AGENT_KILL => {
            let result = kill(sessions, &name, fields.get(1).map(String::as_str));
            let changed = result.is_ok();
            (result, changed)
        }
        C2S_AGENT_SEND => {
            let tab = fields.get(1).map(String::as_str).unwrap_or("");
            let text = fields.get(2).map(String::as_str).unwrap_or("");
            (send(sessions, &name, tab, text), false)
        }
        C2S_AGENT_READ => {
            let tab = fields.get(1).map(String::as_str).unwrap_or("");
            (read(sessions, &name, tab), false)
        }
        C2S_AGENT_RENAME => {
            let to = fields.get(1).map(String::as_str).unwrap_or("");
            let result = rename(sessions, config, &name, to);
            // A rename shows up in tab bars and manager lists.
            let changed = result.is_ok();
            (result, changed)
        }
        _ => (Err("unknown agent command".into()), false),
    }
}

/// Find a session by name, requiring the agent mark: the agent commands
/// must never touch a user's session.
fn agent_session<'s>(
    sessions: &'s mut [Session],
    name: &str,
) -> std::result::Result<&'s mut Session, String> {
    match sessions.iter_mut().find(|s| s.name == name) {
        Some(s) if s.agent => Ok(s),
        Some(_) => Err(format!(
            "\"{name}\" is not an agent session (agent commands only touch \
             sessions created with: xmux agent new)"
        )),
        None => Err(format!("no session named \"{name}\"")),
    }
}

fn new_session(
    sessions: &mut Vec<Session>,
    config: &Config,
    name: &str,
) -> std::result::Result<String, String> {
    if sessions.iter().any(|s| s.name == name) {
        return Err(format!("session \"{name}\" already exists"));
    }
    if config.pins.iter().any(|p| p.name == name) {
        return Err(format!("\"{name}\" is pinned in the config; pick another name"));
    }
    let si = create_session(sessions, config, DEFAULT_SIZE, name.to_string())
        .map_err(|e| e.to_string())?;
    sessions[si].agent = true;
    Ok(format!("created agent session \"{name}\""))
}

fn new_tab(
    sessions: &mut [Session],
    config: &Config,
    name: &str,
    tab: &str,
) -> std::result::Result<String, String> {
    if tab.is_empty() {
        return Err("empty tab name".into());
    }
    let session = agent_session(sessions, name)?;
    if session.tabs.iter().any(|t| t.name == tab) {
        return Err(format!("session \"{name}\" already has a tab \"{tab}\""));
    }
    let size = session.last_size;
    session
        .tabs
        .push(Tab::new(size, tab.to_string(), config).map_err(|e| e.to_string())?);
    session.active_tab = session.tabs.len() - 1;
    session.last_activity = std::time::Instant::now();
    Ok(format!("created tab \"{tab}\" in \"{name}\""))
}

fn rename(
    sessions: &mut [Session],
    config: &Config,
    name: &str,
    to: &str,
) -> std::result::Result<String, String> {
    if to.is_empty() {
        return Err("empty new name".into());
    }
    if sessions.iter().any(|s| s.name == to) {
        return Err(format!("session \"{to}\" already exists"));
    }
    if config.pins.iter().any(|p| p.name == to) {
        return Err(format!("\"{to}\" is pinned in the config; pick another name"));
    }
    let session = agent_session(sessions, name)?;
    session.name = to.to_string();
    Ok(format!("renamed agent session \"{name}\" to \"{to}\""))
}

fn kill(
    sessions: &mut Vec<Session>,
    name: &str,
    tab: Option<&str>,
) -> std::result::Result<String, String> {
    let Some(tab) = tab else {
        // Whole session: dropping it closes the ptys; the shells get
        // SIGHUP and the (ignored) SIGCHLD handler reaps them.
        agent_session(sessions, name)?;
        let si = sessions.iter().position(|s| s.name == name).unwrap();
        sessions.remove(si);
        return Ok(format!("killed agent session \"{name}\""));
    };
    let session = agent_session(sessions, name)?;
    let Some(ti) = session.tabs.iter().position(|t| t.name == tab) else {
        return Err(format!("session \"{name}\" has no tab \"{tab}\""));
    };
    if session.tabs.len() == 1 {
        let si = sessions.iter().position(|s| s.name == name).unwrap();
        sessions.remove(si);
        return Ok(format!("killed tab \"{tab}\" — it was the last, session \"{name}\" is gone"));
    }
    session.tabs.remove(ti);
    if ti < session.active_tab {
        session.active_tab -= 1;
    }
    session.active_tab = session.active_tab.min(session.tabs.len() - 1);
    Ok(format!("killed tab \"{tab}\" in \"{name}\""))
}

/// Pick a tab by name, or the active one for an empty name.
fn tab_index(session: &Session, tab: &str) -> std::result::Result<usize, String> {
    if tab.is_empty() {
        return Ok(session.active_tab);
    }
    session
        .tabs
        .iter()
        .position(|t| t.name == tab)
        .ok_or_else(|| format!("session \"{}\" has no tab \"{tab}\"", session.name))
}

fn send(
    sessions: &mut [Session],
    name: &str,
    tab: &str,
    text: &str,
) -> std::result::Result<String, String> {
    let session = agent_session(sessions, name)?;
    let ti = tab_index(session, tab)?;
    session.last_activity = std::time::Instant::now();
    let tab = &session.tabs[ti];
    let Some(pane) = tab.layout.pane(tab.focused) else {
        return Err("tab has no focused pane".into());
    };
    pane.pty.write(text.as_bytes());
    pane.pty.write(b"\r");
    Ok(String::new())
}

fn read(
    sessions: &mut [Session],
    name: &str,
    tab: &str,
) -> std::result::Result<String, String> {
    let session = agent_session(sessions, name)?;
    let ti = tab_index(session, tab)?;
    session.last_activity = std::time::Instant::now();
    render_text(session, ti).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------
// Plain-text rendering for `read`.
// ---------------------------------------------------------------------

/// Render one tab of a session as a plain-text grid — the same layout a
/// client would see (panes, dividers), followed by a tab-bar line, but
/// without colors. Deliberately does not clear dirty flags, so an
/// attached client's rendering is unaffected.
fn render_text(session: &Session, ti: usize) -> Result<String> {
    let (w, h) = session.last_size;
    let (w, h) = (w as usize, h as usize);
    let mut grid: Vec<Vec<String>> = vec![vec![" ".to_string(); w]; h];

    let tab = &session.tabs[ti];
    let full = Rect {
        x: 0,
        y: 0,
        w: w as u16,
        h: h as u16,
    };

    let mut render_state = RenderState::new()?;
    let mut row_alloc = RowIterator::new()?;
    let mut cell_alloc = CellIterator::new()?;

    let mut draw_pane = |grid: &mut Vec<Vec<String>>,
                         pane: &crate::model::Pane,
                         rect: Rect|
     -> Result<()> {
        let snapshot = render_state.update(&pane.term)?;
        let mut row_it = row_alloc.update(&snapshot)?;
        let mut y = rect.y as usize;
        let mut text = String::with_capacity(16);
        while let Some(row) = row_it.next() {
            if y >= h {
                break;
            }
            let mut x = rect.x as usize;
            let mut cell_it = cell_alloc.update(row)?;
            while let Some(cell) = cell_it.next() {
                if x >= w {
                    break;
                }
                let wide = match cell.raw_cell()?.wide()? {
                    CellWide::SpacerTail | CellWide::SpacerHead => continue,
                    CellWide::Wide => true,
                    CellWide::Narrow => false,
                };
                if cell.graphemes_len()? == 0 {
                    grid[y][x] = " ".to_string();
                } else {
                    cell.graphemes_utf8(&mut text)?;
                    grid[y][x] = text.clone();
                }
                if wide && x + 1 < w {
                    // The glyph spans two columns; blank its spacer so
                    // the line stays the right visual width.
                    grid[y][x + 1] = String::new();
                    x += 1;
                }
                x += 1;
            }
            y += 1;
        }
        Ok(())
    };

    if tab.zoomed && let Some(pane) = tab.layout.pane(tab.focused) {
        draw_pane(&mut grid, pane, full)?;
    } else {
        tab.layout
            .for_each(full, &mut |pane, rect| draw_pane(&mut grid, pane, rect))?;
        let mut cells: HashMap<(u16, u16), (u8, bool)> = HashMap::new();
        crate::render::collect_dividers(&tab.layout, full, &mut cells);
        for (&(x, y), &(bits, real)) in &cells {
            if real && (x as usize) < w && (y as usize) < h {
                grid[y as usize][x as usize] = crate::render::box_char(bits).to_string();
            }
        }
    }

    let mut out = String::with_capacity(w * h);
    for row in &grid {
        out.push_str(row.concat().trim_end());
        out.push('\n');
    }
    // Tab-bar line, with the rendered tab bracketed.
    out.push_str(&format!("== session \"{}\" · tabs:", session.name));
    for (i, t) in session.tabs.iter().enumerate() {
        if i == ti {
            out.push_str(&format!(" [{}]", t.name));
        } else {
            out.push_str(&format!(" {}", t.name));
        }
    }
    out.push_str(" ==\n");
    Ok(out)
}

// ---------------------------------------------------------------------
// Session-manager list filtering (the `a` toggle).
// ---------------------------------------------------------------------

/// Compact "time since last agent activity" for list displays.
pub fn age(last_activity: std::time::Instant) -> String {
    let s = last_activity.elapsed().as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// The session-manager entries for one view: the normal view is pins +
/// unpinned non-agent sessions (as before); the agent view is agent
/// sessions only, most recently active first.
pub fn manager_entries(pins: &[Pin], sessions: &[Session], agents: bool) -> Vec<SessionEntry> {
    if agents {
        let mut agents: Vec<(usize, &Session)> = sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| s.agent)
            .collect();
        agents.sort_by_key(|(_, s)| std::cmp::Reverse(s.last_activity));
        return agents
            .into_iter()
            .map(|(si, s)| SessionEntry {
                name: s.name.clone(),
                running: Some(si),
            })
            .collect();
    }
    let mut entries: Vec<SessionEntry> = pins
        .iter()
        .map(|pin| SessionEntry {
            name: pin.name.clone(),
            running: sessions.iter().position(|s| s.name == pin.name),
        })
        .collect();
    for (si, session) in sessions.iter().enumerate() {
        if !session.agent && !pins.iter().any(|p| p.name == session.name) {
            entries.push(SessionEntry {
                name: session.name.clone(),
                running: Some(si),
            });
        }
    }
    entries
}
