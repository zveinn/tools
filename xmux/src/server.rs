//! The server: owns all sessions/tabs/panes and keeps them alive across
//! client connections. Clients attach over a Unix socket; at most one
//! client per session — a new attach kicks the old client.
//!
//! Single-threaded by design: libghostty's types are !Send/!Sync, so
//! everything — pty parsing, input handling, rendering — happens on one
//! poll(2) loop over the listener, client sockets, and pty fds.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{
    queue,
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

use crate::Result;
use crate::agent;
use crate::config::{self, Config};
use crate::input::{Mode, Overlay, SelectState, handle_input, manager_count, session_entries};
use crate::model::Session;
use crate::protocol::{
    C2S_ATTACH, C2S_INPUT, C2S_LIST, C2S_RESIZE, FrameReader, S2C_AGENT_ERR, S2C_AGENT_OK,
    S2C_BYE, S2C_LIST, S2C_OUTPUT, frame, socket_path,
};
use crate::render::{ListItem, Renderer, content_size, draw_manager, draw_naming, draw_session};

/// A slow client gets this much buffered output before being dropped.
const MAX_OUTBUF: usize = 8 * 1024 * 1024;

/// Raised by the stop-signal handler; the poll loop sees it and runs the
/// shutdown path.
static SHUTDOWN: AtomicBool = AtomicBool::new(false);

/// Stop-signal handler. Flipping a flag is the only async-signal-safe
/// thing worth doing here — the loop does the actual shutdown.
extern "C" fn on_stop_signal(_sig: i32) {
    SHUTDOWN.store(true, Ordering::Relaxed);
}

/// One connected client.
struct ClientConn {
    stream: UnixStream,
    reader: FrameReader,
    /// Output queued while the socket is full (written when writable).
    outbuf: Vec<u8>,
    /// Id of the viewed session; None until the client attaches.
    attached: Option<u64>,
    size: (u16, u16),
    mode: Mode,
    /// Mouse select-to-copy state.
    select: SelectState,
    needs_redraw: bool,
    /// Latest OSC 52 text to send after the next redraw (last write wins).
    pending_copy: Option<String>,
    /// Set when the last input was pointer-only, so the running-mode
    /// paint can skip a synchronized update (faster to appear, some tearing).
    skip_sync: bool,
    /// A BYE was queued; drop the client once outbuf drains.
    closing: bool,
    /// The socket died; drop the client this iteration.
    dead: bool,
}

impl ClientConn {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            reader: FrameReader::new(),
            outbuf: Vec::new(),
            attached: None,
            size: (80, 24),
            mode: Mode::Running,
            select: SelectState::default(),
            needs_redraw: false,
            pending_copy: None,
            skip_sync: false,
            closing: false,
            dead: false,
        }
    }

    /// Queue a frame; kills the client instead if it is hopelessly slow.
    fn send(&mut self, kind: u8, payload: &[u8]) {
        if self.outbuf.len() + payload.len() > MAX_OUTBUF {
            eprintln!("dropping unresponsive client (output buffer over {MAX_OUTBUF} bytes)");
            self.dead = true;
            return;
        }
        self.outbuf.extend_from_slice(&frame(kind, payload));
        self.try_flush();
    }

    /// Say goodbye (reason shown by the client) and stop serving.
    fn bye(&mut self, reason: &str) {
        if !self.closing {
            self.send(S2C_BYE, reason.as_bytes());
            self.closing = true;
        }
    }

    fn try_flush(&mut self) {
        while !self.outbuf.is_empty() {
            match self.stream.write(&self.outbuf) {
                Ok(0) => {
                    self.dead = true;
                    return;
                }
                Ok(n) => {
                    self.outbuf.drain(..n);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.dead = true;
                    return;
                }
            }
        }
    }
}

fn bind_listener() -> Result<UnixListener> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if path.exists() {
        // A live server means we must not steal the socket; a stale file
        // from a crash is safe to replace.
        if UnixStream::connect(&path).is_ok() {
            return Err(format!("server already running on {}", path.display()).into());
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

/// Device+inode of the socket file, so the server can tell its own
/// listening socket from a replacement — or from nothing at all.
fn socket_id(path: &std::path::Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Modification time of the config file, if any.
fn config_mtime() -> Option<std::time::SystemTime> {
    let path = config::path()?;
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Pinned sessions first (slot order), unpinned after in their existing
/// order — applied after a reload changes the pins.
fn resort_sessions(sessions: &mut [Session], config: &Config) {
    sessions.sort_by_key(|s| {
        config
            .pins
            .iter()
            .position(|p| p.name == s.name)
            .unwrap_or(usize::MAX)
    });
}

pub fn run() -> Result<()> {
    let mut config: Config = config::load().map_err(|e| {
        format!(
            "failed to load {} — fix it and restart: {e}",
            config::path().map_or("config".into(), |p| p.display().to_string())
        )
    })?;
    // Auto-reap exited shells (kernel discards their status), so killed
    // sessions/tabs don't leave zombies behind.
    unsafe {
        let _ = nix::sys::signal::signal(
            nix::sys::signal::Signal::SIGCHLD,
            nix::sys::signal::SigHandler::SigIgn,
        );
    }
    // Catch the stop signals rather than dying on the default action,
    // which leaves the socket file behind — clients then get
    // ECONNREFUSED (a socket nobody serves) instead of a clean "not
    // running", and the panes outlive us until systemd's SIGKILL.
    unsafe {
        use nix::sys::signal::{SigHandler, Signal, signal};
        for sig in [Signal::SIGTERM, Signal::SIGINT, Signal::SIGHUP] {
            let _ = signal(sig, SigHandler::Handler(on_stop_signal));
        }
    }
    let mut listener = bind_listener()?;
    let sock_path = socket_path();
    let mut bound_id = socket_id(&sock_path);
    let mut rebind_failed = false;
    let mut renderer = Renderer::new()?;
    // Bring back the sessions (tabs, splits, shell cwds) saved on the
    // last run; agent sessions are not part of saved state.
    let mut sessions: Vec<Session> = crate::state::restore(&config);
    resort_sessions(&mut sessions, &config);
    let mut clients: Vec<ClientConn> = Vec::new();

    eprintln!("xmux server listening on {}", socket_path().display());

    let mut last_mtime = config_mtime();
    let mut last_save = std::time::Instant::now();
    let mut tick: u32 = 0;

    loop {
        // ---- Hot-reload the config when the file changes (checked about
        // once a second). Bindings, accent, and pins apply immediately;
        // shell/envs affect newly spawned shells. A broken config is
        // rejected and the old one stays active.
        tick = tick.wrapping_add(1);
        if tick % 10 == 0 {
            let mtime = config_mtime();
            if mtime != last_mtime {
                last_mtime = mtime;
                match config::load() {
                    Ok(new_config) => {
                        config = new_config;
                        resort_sessions(&mut sessions, &config);
                        for client in clients.iter_mut() {
                            if client.attached.is_some() && !client.closing && !client.dead {
                                client.needs_redraw = true;
                            }
                        }
                        eprintln!("config reloaded");
                    }
                    Err(e) => eprintln!("config reload failed (keeping old config): {e}"),
                }
            }
        }

        // ---- Re-bind if the socket file vanished under us (a /tmp
        // cleaner, a stray rm). Unlinking the path does not disturb the
        // listening socket, so without this the server keeps running,
        // perfectly healthy as far as systemd can tell, while every
        // client gets "no such file or directory" and is told to start
        // a server that is already up.
        if tick % 10 == 0 && socket_id(&sock_path) != bound_id {
            match bind_listener() {
                Ok(new_listener) => {
                    eprintln!("socket {} went missing; re-bound it", sock_path.display());
                    listener = new_listener;
                    bound_id = socket_id(&sock_path);
                    rebind_failed = false;
                }
                Err(e) => {
                    // Someone else owns the path now: say so once and
                    // keep serving the clients already connected.
                    if !rebind_failed {
                        eprintln!("socket {} is gone: {e}", sock_path.display());
                        rebind_failed = true;
                    }
                }
            }
        }

        // ---- Persist the session layout every ten seconds. ----
        if last_save.elapsed().as_secs() >= 10 {
            last_save = std::time::Instant::now();
            crate::state::save(&sessions);
        }

        // ---- Poll: listener + client sockets + every pane's pty. ----
        let mut fd_map = Vec::new();
        let client_count = clients.len();
        let ready: Vec<(bool, bool)> = {
            let mut fds = vec![PollFd::new(listener.as_fd(), PollFlags::POLLIN)];
            for client in &clients {
                let mut events = PollFlags::POLLIN;
                if !client.outbuf.is_empty() {
                    events |= PollFlags::POLLOUT;
                }
                fds.push(PollFd::new(client.stream.as_fd(), events));
            }
            for (si, session) in sessions.iter().enumerate() {
                for (ti, tab) in session.tabs.iter().enumerate() {
                    for pane in tab.layout.panes() {
                        fds.push(PollFd::new(pane.pty.as_fd(), PollFlags::POLLIN));
                        fd_map.push((si, ti, pane.id));
                    }
                }
            }
            match poll(&mut fds, PollTimeout::from(100u16)) {
                Ok(_) => fds
                    .iter()
                    .map(|f| {
                        let r = f.revents().unwrap_or(PollFlags::empty());
                        (
                            r.intersects(
                                PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR,
                            ),
                            r.contains(PollFlags::POLLOUT),
                        )
                    })
                    .collect(),
                // A signal (a stop signal, or SIGWINCH from a pane) cut
                // the wait short; revents are meaningless, so treat the
                // round as idle and come back next iteration.
                Err(nix::errno::Errno::EINTR) => vec![(false, false); fds.len()],
                Err(e) => return Err(e.into()),
            }
        };

        if SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }

        // ---- Accept new clients. ----
        if ready[0].0 {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        stream.set_nonblocking(true)?;
                        clients.push(ClientConn::new(stream));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }

        // ---- Client IO. ----
        for ci in 0..client_count {
            let (readable, writable) = ready[ci + 1];
            if writable {
                clients[ci].try_flush();
            }
            if !readable || clients[ci].closing || clients[ci].dead {
                continue;
            }
            let mut tmp = [0u8; 4096];
            loop {
                match clients[ci].stream.read(&mut tmp) {
                    Ok(0) => {
                        clients[ci].dead = true;
                        break;
                    }
                    Ok(n) => clients[ci].reader.extend(&tmp[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        clients[ci].dead = true;
                        break;
                    }
                }
            }
            loop {
                match clients[ci].reader.next_frame() {
                    Ok(Some((kind, payload))) => {
                        handle_frame(ci, kind, payload, &mut clients, &mut sessions, &config)?
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("dropping client after protocol error: {e}");
                        clients[ci].dead = true;
                        break;
                    }
                }
            }
        }

        // ---- Drain ptys into their pane terminals. ----
        let mut any_removed = false;
        let mut changed_sessions: Vec<u64> = Vec::new();
        let mut clipboard_out: Vec<(u64, char, String)> = Vec::new();
        for (k, &(si, ti, pane_id)) in fd_map.iter().enumerate() {
            if !ready[k + 1 + client_count].0 {
                continue;
            }
            let session_id = sessions[si].id;
            let tab = &mut sessions[si].tabs[ti];
            let Some(pane) = tab.layout.pane_mut(pane_id) else {
                continue; // already removed earlier this round
            };
            match pane.pty.read(&mut pane.term) {
                Ok(()) => {
                    for (register, text) in pane.clipboard.borrow_mut().drain(..) {
                        clipboard_out.push((session_id, register, text));
                    }
                    if ti == sessions[si].active_tab && !changed_sessions.contains(&session_id) {
                        changed_sessions.push(session_id);
                    }
                }
                Err(_) => {
                    tab.remove_pane(pane_id);
                    any_removed = true;
                }
            }
        }

        // OSC 52 from programs inside panes: pass each write through to
        // the client attached to that pane's session (in-band, so the
        // clipboard lands on the user's local machine even over SSH).
        for (session_id, register, text) in clipboard_out {
            for client in clients.iter_mut() {
                if client.attached == Some(session_id) && !client.closing && !client.dead {
                    client.send(S2C_OUTPUT, &osc52_to(register, &text));
                }
            }
        }

        // Redraw clients viewing sessions whose active tab produced output.
        for client in &mut clients {
            if let Some(id) = client.attached
                && changed_sessions.contains(&id)
                && matches!(client.mode, Mode::Running)
            {
                client.needs_redraw = true;
                client.skip_sync = false;
            }
        }

        // ---- Cleanup: empty tabs, empty sessions, homeless clients. ----
        if any_removed {
            for si in (0..sessions.len()).rev() {
                let session = &mut sessions[si];
                for ti in (0..session.tabs.len()).rev() {
                    if session.tabs[ti].is_empty() {
                        session.tabs.remove(ti);
                        if ti < session.active_tab {
                            session.active_tab -= 1;
                        }
                    }
                }
                if session.tabs.is_empty() {
                    sessions.remove(si);
                } else {
                    session.active_tab = session.active_tab.min(session.tabs.len() - 1);
                }
            }
            for client in &mut clients {
                let Some(id) = client.attached else { continue };
                if sessions.iter().any(|s| s.id == id) {
                    client.needs_redraw = true;
                    continue;
                }
                // Viewed session is gone: fall back to any surviving
                // session, else say goodbye.
                match sessions.first() {
                    Some(first) => {
                        client.attached = Some(first.id);
                        client.mode = Mode::Running;
                        client.needs_redraw = true;
                    }
                    None => client.bye("session closed"),
                }
            }
            // A sibling session may have vanished; drop the cursor onto
            // the overlay's own list, not the full sessions vec.
            for client in &mut clients {
                if let Mode::Manager {
                    overlay,
                    ref mut selected,
                    ..
                } = client.mode
                {
                    let active = client
                        .attached
                        .and_then(|id| sessions.iter().position(|s| s.id == id))
                        .unwrap_or(0);
                    let count = manager_count(overlay, &sessions, active, &config.pins);
                    *selected = (*selected).min(count.saturating_sub(1));
                }
            }
            // Fallback-attached clients need their session resized.
            for ci in 0..clients.len() {
                if let Some(id) = clients[ci].attached
                    && let Some(si) = sessions.iter().position(|s| s.id == id)
                {
                    sessions[si].resize(content_size(clients[ci].size))?;
                }
            }
        }

        // ---- Render for clients that need it. ----
        for ci in 0..clients.len() {
            if !clients[ci].needs_redraw || clients[ci].closing || clients[ci].dead {
                continue;
            }
            // Previous frame still in flight: keep needs_redraw and skip
            // so we paint the latest state once the socket drains, instead
            // of queuing a backlog of full-screen frames.
            if !clients[ci].outbuf.is_empty() {
                continue;
            }
            clients[ci].needs_redraw = false;
            let Some(id) = clients[ci].attached else {
                continue;
            };
            let Some(si) = sessions.iter().position(|s| s.id == id) else {
                continue;
            };
            let size = clients[ci].size;
            let skip_sync = clients[ci].skip_sync;
            let mut buf: Vec<u8> = Vec::with_capacity(4096);
            match &clients[ci].mode {
                Mode::Running => {
                    draw_session(
                        &mut renderer,
                        &sessions[si],
                        &mut buf,
                        size,
                        config.accent,
                        config.bar_top,
                        !skip_sync,
                    )?;
                }
                Mode::Manager {
                    overlay,
                    selected,
                    search,
                } => match overlay {
                    Overlay::Sessions { agents } => {
                        let entries = agent::manager_entries(&config.pins, &sessions, *agents);
                        // Agent view: append each session's last-activity
                        // age as an aligned column.
                        let name_width = entries
                            .iter()
                            .map(|e| e.name.chars().count())
                            .max()
                            .unwrap_or(0);
                        let query = search.as_ref().map(|q| q.text.as_str());
                        let mut min_interior = 0;
                        let mut items: Vec<ListItem> = Vec::new();
                        for e in &entries {
                            let label = match (*agents, e.running) {
                                (true, Some(esi)) => format!(
                                    "{:<name_width$}  · {}",
                                    e.name,
                                    agent::age(sessions[esi].last_activity),
                                ),
                                _ => e.name.clone(),
                            };
                            min_interior = min_interior.max(label.chars().count() + 2);
                            if query.is_none_or(|q| crate::input::name_matches(&e.name, q)) {
                                items.push(ListItem {
                                    label,
                                    active: e.running == Some(si),
                                    dim: e.running.is_none(),
                                });
                            }
                        }
                        let (title, toggle) = if *agents {
                            ("agent sessions", "a normal")
                        } else {
                            ("sessions", "a agents")
                        };
                        let full_footer = format!(
                            "enter switch · n new · r rename · x kill · {toggle} · / search · esc close"
                        );
                        // Width stays pinned to the normal footer so the
                        // panel doesn't jump when a search starts.
                        min_interior = min_interior.max(full_footer.chars().count());
                        let footer = match query {
                            Some(_) => "enter switch · esc cancel".to_string(),
                            None => full_footer,
                        };
                        let view = crate::render::ManagerView {
                            title,
                            items: &items,
                            selected: (*selected).min(items.len().saturating_sub(1)),
                            footer: &footer,
                            search: query,
                            search_cursor: search.as_ref().map_or(0, |q| q.cursor),
                            min_rows: entries.len(),
                            min_interior,
                        };
                        draw_manager(&mut buf, &view, size, config.accent)?;
                    }
                    Overlay::Tabs => {
                        let session = &sessions[si];
                        let query = search.as_ref().map(|q| q.text.as_str());
                        let mut min_interior = 0;
                        let mut items: Vec<ListItem> = Vec::new();
                        for (ti, t) in session.tabs.iter().enumerate() {
                            min_interior = min_interior.max(t.name.chars().count() + 2);
                            if query.is_none_or(|q| crate::input::name_matches(&t.name, q)) {
                                items.push(ListItem {
                                    label: t.name.clone(),
                                    active: ti == session.active_tab,
                                    dim: false,
                                });
                            }
                        }
                        let title = format!("{} · tabs", session.name);
                        let full_footer =
                            "enter switch · n new · r rename · x kill · / search · esc close";
                        min_interior = min_interior.max(full_footer.chars().count());
                        let footer = match query {
                            Some(_) => "enter switch · esc cancel".to_string(),
                            None => full_footer.to_string(),
                        };
                        let view = crate::render::ManagerView {
                            title: &title,
                            items: &items,
                            selected: (*selected).min(items.len().saturating_sub(1)),
                            footer: &footer,
                            search: query,
                            search_cursor: search.as_ref().map_or(0, |q| q.cursor),
                            min_rows: session.tabs.len(),
                            min_interior,
                        };
                        draw_manager(&mut buf, &view, size, config.accent)?;
                    }
                },
                Mode::PaneSettings { text } => {
                    draw_naming(
                        &mut buf,
                        "terminal settings · auto-run on restore",
                        &text.text,
                        text.cursor,
                        size,
                        config.accent,
                        "enter save · empty clears · esc cancel",
                    )?;
                }
                Mode::Naming {
                    overlay,
                    name,
                    rename,
                } => {
                    let title = match (overlay, rename.is_some()) {
                        (Overlay::Sessions { agents: false }, false) => "new session",
                        (Overlay::Sessions { agents: true }, false) => "new agent session",
                        (Overlay::Sessions { .. }, true) => "rename session",
                        (Overlay::Tabs, false) => "new tab",
                        (Overlay::Tabs, true) => "rename tab",
                    };
                    let footer = if rename.is_some() {
                        "enter rename · esc cancel"
                    } else {
                        "enter create · esc cancel"
                    };
                    draw_naming(&mut buf, title, &name.text, name.cursor, size, config.accent, footer)?;
                }
            }
            clients[ci].send(S2C_OUTPUT, &buf);
            if let Some(text) = clients[ci].pending_copy.take() {
                clients[ci].send(S2C_OUTPUT, &osc52(&text));
            }
            clients[ci].skip_sync = false;
        }

        // ---- Drop finished clients (sessions keep running). ----
        clients.retain(|c| !c.dead && !(c.closing && c.outbuf.is_empty()));
    }

    // ---- Orderly shutdown on SIGTERM/SIGINT/SIGHUP. ----
    // Unlink before anything slow: a client racing us then gets "no
    // server" rather than a connection to a socket we will never serve.
    let path = socket_path();
    let _ = std::fs::remove_file(&path);
    drop(listener);
    // Save while the shells still live — layout.json records each pane's
    // cwd, read from /proc/<pid>/cwd.
    crate::state::save(&sessions);
    // Agent sessions are deliberately left out of saved state, so count
    // what actually landed in layout.json.
    let saved = sessions.iter().filter(|s| !s.agent).count();
    for client in &mut clients {
        client.bye("server shutting down");
    }
    drop(clients);
    // Dropping the sessions closes every pty master, so the shells get
    // SIGHUP and exit on their own. Without this they would sit there
    // until systemd gave up waiting and sent SIGKILL — interactive bash
    // ignores SIGTERM, so the service stop blocked for the full
    // TimeoutStopSec every restart.
    drop(sessions);
    eprintln!("xmux server stopped ({saved} sessions saved)");
    Ok(())
}

/// Handle one protocol frame from client `ci`.
fn handle_frame(
    ci: usize,
    kind: u8,
    payload: Vec<u8>,
    clients: &mut Vec<ClientConn>,
    sessions: &mut Vec<Session>,
    config: &Config,
) -> Result<()> {
    match kind {
        C2S_ATTACH => {
            if payload.len() < 4 {
                clients[ci].dead = true;
                return Ok(());
            }
            let cols = u16::from_le_bytes([payload[0], payload[1]]);
            let rows = u16::from_le_bytes([payload[2], payload[3]]);
            let name = String::from_utf8_lossy(&payload[4..]).into_owned();
            let name = if name.trim().is_empty() {
                "main".to_string()
            } else {
                name.trim().to_string()
            };
            if cols > 0 && rows > 0 {
                clients[ci].size = (cols, rows);
            }
            let size = clients[ci].size;

            let si = match sessions.iter().position(|s| s.name == name) {
                Some(si) => si,
                None => crate::input::create_session(
                    sessions,
                    config,
                    crate::render::content_size(size),
                    name,
                )?,
            };
            attach_to(ci, si, clients, sessions)?;
        }
        C2S_INPUT => {
            let Some(id) = clients[ci].attached else {
                return Ok(());
            };
            let Some(mut active) = sessions.iter().position(|s| s.id == id) else {
                return Ok(());
            };
            let size = clients[ci].size;
            // Take the mode out so `clients` stays free for kick handling.
            let mut mode = std::mem::take(&mut clients[ci].mode);
            let mut select = std::mem::take(&mut clients[ci].select);
            let was_settings = matches!(mode, Mode::PaneSettings { .. });
            let overlay_involved = !matches!(mode, Mode::Running);
            let (detach, copied, mouse_only) = handle_input(
                &payload,
                &mut mode,
                sessions,
                &mut active,
                content_size(size),
                config,
                &mut select,
            )?;
            let overlay_involved = overlay_involved || !matches!(mode, Mode::Running);
            clients[ci].mode = mode;
            clients[ci].select = select;
            clients[ci].needs_redraw = true;
            clients[ci].skip_sync = mouse_only && matches!(clients[ci].mode, Mode::Running);

            // Defer OSC 52 until after the redraw so the host terminal
            // sees EndSynchronizedUpdate before a clipboard write that
            // some terminals handle synchronously (and can stall on).
            // Last write wins: spam-select only needs the latest copy.
            if let Some(text) = copied {
                clients[ci].pending_copy = Some(text);
            }

            // Persist a freshly confirmed auto-run right away instead of
            // waiting for the periodic save.
            if was_settings && !matches!(clients[ci].mode, Mode::PaneSettings { .. }) {
                crate::state::save(sessions);
            }

            if detach {
                clients[ci].bye("detached");
            } else if sessions.is_empty() {
                // The last session was killed from the manager.
                clients[ci].bye("all sessions closed");
            } else {
                let active = active.min(sessions.len() - 1);
                if sessions.get(active).map(|s| s.id) != Some(id) {
                    // The manager switched sessions (created, killed, ...).
                    attach_to(ci, active, clients, sessions)?;
                }
            }

            // Kills may have orphaned other clients; renames/kills change
            // what everyone's overlays and bars show.
            rehome_homeless_clients(clients, sessions)?;
            if overlay_involved {
                for client in clients.iter_mut() {
                    if client.attached.is_some() && !client.closing && !client.dead {
                        client.needs_redraw = true;
                    }
                }
            }
        }
        C2S_LIST => {
            let listing = format_listing(sessions, clients, config);
            clients[ci].send(S2C_LIST, &listing);
            // One-shot request: close once the reply drains.
            clients[ci].closing = true;
        }
        kind if agent::is_agent_frame(kind) => {
            let (result, sessions_changed) = agent::execute(kind, &payload, sessions, config);
            match result {
                Ok(text) => clients[ci].send(S2C_AGENT_OK, text.as_bytes()),
                Err(e) => {
                    eprintln!("agent command failed: {e}");
                    clients[ci].send(S2C_AGENT_ERR, e.as_bytes());
                }
            }
            // One-shot request: close once the reply drains.
            clients[ci].closing = true;
            if sessions_changed {
                rehome_homeless_clients(clients, sessions)?;
                for client in clients.iter_mut() {
                    if client.attached.is_some() && !client.closing && !client.dead {
                        client.needs_redraw = true;
                    }
                }
            }
        }
        C2S_RESIZE => {
            if payload.len() < 4 {
                return Ok(());
            }
            let cols = u16::from_le_bytes([payload[0], payload[1]]);
            let rows = u16::from_le_bytes([payload[2], payload[3]]);
            if cols == 0 || rows == 0 {
                return Ok(());
            }
            clients[ci].size = (cols, rows);
            if let Some(id) = clients[ci].attached
                && let Some(si) = sessions.iter().position(|s| s.id == id)
            {
                sessions[si].resize(content_size((cols, rows)))?;
            }
            clients[ci].needs_redraw = true;
            clients[ci].skip_sync = false;
        }
        _ => {
            eprintln!("dropping client after unknown frame kind {kind} (client newer than server?)");
            clients[ci].dead = true;
        }
    }
    Ok(())
}

/// One line per session, colored for tty clients (the client strips the
/// colors when its stdout is piped): an accent dot and "attached" tag
/// for attached sessions, dim counts, and pinned-but-stopped sessions
/// listed dim — consistent with the session manager.
fn format_listing(sessions: &[Session], clients: &[ClientConn], config: &Config) -> Vec<u8> {
    let entries = session_entries(&config.pins, sessions);
    let mut out: Vec<u8> = Vec::new();
    if entries.is_empty() {
        out.extend_from_slice(b"no sessions\n");
        return out;
    }
    let name_width = entries
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(0);

    for entry in &entries {
        let write = (|| -> crate::Result<()> {
            let padded = format!("{:<name_width$}", entry.name);
            match entry.running {
                Some(si) => {
                    let session = &sessions[si];
                    let panes: usize = session
                        .tabs
                        .iter()
                        .map(|tab| tab.layout.panes().len())
                        .sum();
                    let attached = clients
                        .iter()
                        .any(|c| c.attached == Some(session.id) && !c.closing && !c.dead);
                    if attached {
                        queue!(
                            out,
                            SetForegroundColor(config.accent),
                            Print("● "),
                            SetForegroundColor(Color::Reset),
                            SetAttribute(Attribute::Bold),
                            Print(&padded),
                            SetAttribute(Attribute::Reset),
                        )?;
                    } else {
                        queue!(
                            out,
                            SetAttribute(Attribute::Dim),
                            Print("○ "),
                            SetAttribute(Attribute::Reset),
                            Print(&padded),
                        )?;
                    }
                    queue!(
                        out,
                        SetAttribute(Attribute::Dim),
                        Print(format!(
                            "  {} tab{} · {} pane{}{}",
                            session.tabs.len(),
                            if session.tabs.len() == 1 { "" } else { "s" },
                            panes,
                            if panes == 1 { "" } else { "s" },
                            if session.agent {
                                format!(" · agent · {}", crate::agent::age(session.last_activity))
                            } else {
                                String::new()
                            },
                        )),
                        SetAttribute(Attribute::Reset),
                    )?;
                    if attached {
                        queue!(
                            out,
                            Print("  "),
                            SetForegroundColor(config.accent),
                            Print("attached"),
                            SetForegroundColor(Color::Reset),
                        )?;
                    }
                }
                None => {
                    queue!(
                        out,
                        SetAttribute(Attribute::Dim),
                        Print(format!("○ {padded}  not running")),
                        SetAttribute(Attribute::Reset),
                    )?;
                }
            }
            queue!(out, Print("\n"))?;
            Ok(())
        })();
        // Writing into a Vec cannot fail.
        let _ = write;
    }
    out
}

/// Wrap text in an OSC 52 clipboard-set sequence (base64 payload). The
/// client's terminal — however many SSH hops away — sets its clipboard.
fn osc52(text: &str) -> Vec<u8> {
    osc52_to('c', text)
}

/// Like `osc52`, targeting a specific register ('c' clipboard, 'p'
/// primary).
fn osc52_to(register: char, text: &str) -> Vec<u8> {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    // Terminals cap OSC payload sizes; clamp to something generous.
    let data = text.as_bytes();
    let data = &data[..data.len().min(512 * 1024)];
    let mut out = format!("\x1b]52;{register};").into_bytes();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63]);
        out.push(TABLE[(n >> 12) as usize & 63]);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] } else { b'=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] } else { b'=' });
    }
    out.push(0x07);
    out
}

/// Clients whose session vanished (killed from a manager) fall back to
/// the first surviving session, or get a goodbye when none remain.
fn rehome_homeless_clients(
    clients: &mut [ClientConn],
    sessions: &mut [Session],
) -> Result<()> {
    for ci in 0..clients.len() {
        if clients[ci].closing || clients[ci].dead {
            continue;
        }
        let Some(id) = clients[ci].attached else {
            continue;
        };
        if sessions.iter().any(|s| s.id == id) {
            continue;
        }
        if sessions.is_empty() {
            clients[ci].bye("session closed");
            continue;
        }
        clients[ci].attached = Some(sessions[0].id);
        clients[ci].mode = Mode::Running;
        clients[ci].needs_redraw = true;
        sessions[0].resize(content_size(clients[ci].size))?;
    }
    Ok(())
}

/// Point client `ci` at session index `si`: kick any other client on that
/// session, size it for this client, and schedule a full redraw.
fn attach_to(
    ci: usize,
    si: usize,
    clients: &mut [ClientConn],
    sessions: &mut [Session],
) -> Result<()> {
    let id = sessions[si].id;
    for cj in 0..clients.len() {
        if cj != ci && clients[cj].attached == Some(id) && !clients[cj].closing {
            clients[cj].bye("kicked: another client attached to this session");
        }
    }
    clients[ci].attached = Some(id);
    sessions[si].resize(content_size(clients[ci].size))?;
    clients[ci].needs_redraw = true;
    Ok(())
}
