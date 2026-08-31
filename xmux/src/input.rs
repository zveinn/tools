//! Input handling: scanning stdin bytes for bound chords and command bindings,
//! and the manager-overlay / name-prompt key state machines.

use crate::Result;
use crate::config::{Binding, BindingAction, Config, Pin};
use crate::model::{NavDir, Rect, Session};
use crate::pty::Pty;

/// Which manager overlay is on screen (or being named for).
#[derive(Clone, Copy)]
pub enum Overlay {
    /// Sessions: named groups of tabs. `agents` selects which list is
    /// shown — normal sessions or agent sessions (toggled with `a`).
    Sessions { agents: bool },
    /// Tabs of the currently viewed session.
    Tabs,
}

/// What a client is currently showing.
#[derive(Default)]
pub enum Mode {
    /// The viewed tab's panes.
    #[default]
    Running,
    /// A manager overlay, with the highlighted entry. `search` holds
    /// the `/` filter query while the user is typing one.
    Manager {
        overlay: Overlay,
        selected: usize,
        search: Option<TextInput>,
    },
    /// The name prompt for a session/tab being created or renamed.
    Naming {
        overlay: Overlay,
        name: TextInput,
        /// `None` = creating something new; `Some` = renaming this.
        rename: Option<RenameTarget>,
    },
    /// The per-pane settings prompt: the auto-run command typed into
    /// the focused pane's shell when the layout is restored.
    PaneSettings { text: TextInput },
}

/// What a rename prompt applies to.
#[derive(Clone, Copy)]
pub enum RenameTarget {
    /// A session, by id (stable across list changes).
    Session(u64),
    /// A tab of the viewed session, by index.
    Tab(usize),
}

/// A control chord found in the input stream.
#[derive(Clone, Copy)]
pub enum InputAction {
    /// Open a manager overlay.
    Manager(Overlay),
    /// Split the focused pane horizontally (stacked).
    SplitH,
    /// Split the focused pane vertically (side by side).
    SplitV,
    /// Move focus to the next pane.
    FocusNext,
    /// Move focus directionally.
    FocusDir(NavDir),
    /// Detach the client from the server.
    Detach,
    /// Toggle fullscreen for the focused pane.
    Fullscreen,
    /// Switch to the pinned session at this index in `Config::pins`,
    /// starting it if it isn't running.
    OpenSession(usize),
    /// Open the per-pane settings prompt (auto-run command).
    PaneSettings,
}

/// One row of the session-manager list: every pinned session (running or
/// not, in slot order) followed by running unpinned sessions.
pub struct SessionEntry {
    pub name: String,
    /// Index into the sessions vec when the session is running.
    pub running: Option<usize>,
}

pub fn session_entries(pins: &[Pin], sessions: &[Session]) -> Vec<SessionEntry> {
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
    // Agent sessions last, most recently active first.
    entries.extend(crate::agent::manager_entries(&[], sessions, true));
    entries
}

/// Create a session, inserting it at its place in the display order:
/// pinned sessions sit before unpinned ones, in slot order. Returns the
/// new session's index.
pub fn create_session(
    sessions: &mut Vec<Session>,
    config: &Config,
    size: (u16, u16),
    name: String,
) -> Result<usize> {
    let pins: &[Pin] = &config.pins;
    let rank = |n: &str| pins.iter().position(|p| p.name == n);
    let session = Session::new(size, name, config)?;
    let pos = match rank(&session.name) {
        None => sessions.len(),
        Some(new_rank) => sessions
            .iter()
            .position(|s| match rank(&s.name) {
                None => true,
                Some(r) => r > new_rank,
            })
            .unwrap_or(sessions.len()),
    };
    sessions.insert(pos, session);
    Ok(pos)
}

/// Returns `Some((action, i))` when a control chord was pressed, where
/// `i` is the index just past it; bytes before it have been forwarded,
/// bytes from `i` on have not.
///
/// When the byte sequence of a `Run` command binding appears, we swallow it and
/// instead type the configured program name plus Enter into the shell.
/// Bindings are matched longest-first (see `config::load`), and a chord
/// split across two reads (e.g. a lone ESC press followed later by a
/// letter) does not match.
pub fn forward_input(pty: &Pty, bindings: &[Binding], buf: &[u8]) -> Option<(InputAction, usize)> {
    let mut start = 0;
    let mut i = 0;
    'scan: while i < buf.len() {
        for binding in bindings {
            if !buf[i..].starts_with(&binding.seq) {
                continue;
            }
            match &binding.action {
                BindingAction::Control(action) => {
                    pty.write(&buf[start..i]);
                    return Some((*action, i + binding.seq.len()));
                }
                BindingAction::Run(cmd) => {
                    pty.write(&buf[start..i]);
                    pty.write(cmd.as_bytes());
                    pty.write(b"\r");
                    i += binding.seq.len();
                    start = i;
                    continue 'scan;
                }
            }
        }
        i += 1;
    }
    pty.write(&buf[start..]);
    None
}

/// A mouse/scroll intent extracted from raw client input. `raw` keeps
/// the original SGR bytes so panes that track the mouse themselves get
/// the event untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseEvent {
    /// Wheel notch; `cb` is the SGR button byte (modifiers included),
    /// `x`/`y` are 1-based screen coordinates.
    Wheel { up: bool, cb: u32, x: u16, y: u16 },
    /// PageUp / PageDown.
    Page { up: bool },
    /// Button press.
    Press { left: bool, cb: u32, x: u16, y: u16 },
    /// Motion with a button held.
    Drag { cb: u32, x: u16, y: u16 },
    /// Button release.
    Release { cb: u32, x: u16, y: u16 },
}

impl MouseEvent {
    /// (button byte, screen x, screen y) for events that have a
    /// position; `None` for PageUp/PageDown.
    fn at(&self) -> Option<(u32, u16, u16)> {
        match self {
            MouseEvent::Wheel { cb, x, y, .. }
            | MouseEvent::Press { cb, x, y, .. }
            | MouseEvent::Drag { cb, x, y }
            | MouseEvent::Release { cb, x, y } => Some((*cb, *x, *y)),
            MouseEvent::Page { .. } => None,
        }
    }
}

/// Split raw input into (bytes to process normally, mouse events,
/// incomplete prefix to prepend to the next chunk).
///
/// SGR mouse sequences are always consumed here so they never leak
/// into a shell as garbage bytes. PageUp/PageDown become events too.
/// A sequence split across reads is returned as the remainder rather
/// than dropped — dropping it used to feed the continuation (`0;24M`
/// and so on) into the shell on the next chunk.
///
/// A trailing lone ESC is the one prefix we do *not* hold blindly: it
/// is far more often the user pressing Esc than half a mouse report,
/// and holding it stitches it onto whatever they type next — turning
/// Esc then `h` into the `alt+h` chord, so Esc silently acted like a
/// command prefix. It is only held directly after a sequence we just
/// parsed, which is what a report split mid-burst actually looks like.
pub fn extract_mouse(buf: &[u8]) -> (Vec<u8>, Vec<MouseEvent>, Vec<u8>) {
    let mut clean = Vec::with_capacity(buf.len());
    let mut events = Vec::new();
    let mut i = 0;
    // Whether the bytes just before `i` were a sequence we consumed.
    let mut after_seq = false;
    while i < buf.len() {
        // Incomplete prefix of a sequence we parse: hold it for the
        // next chunk instead of emitting ESC into the shell. The longer
        // prefixes are unambiguous — `\x1b[` and friends can only ever
        // complete into a key or a report, never into an alt+ chord.
        let rest = &buf[i..];
        if (rest == b"\x1b" && after_seq)
            || rest == b"\x1b["
            || rest == b"\x1b[5"
            || rest == b"\x1b[6"
        {
            break;
        }
        if buf[i..].starts_with(b"\x1b[<") {
            // SGR mouse: ESC [ < Cb ; Cx ; Cy (M|m)
            let mut j = i + 3;
            while j < buf.len() && (buf[j].is_ascii_digit() || buf[j] == b';') {
                j += 1;
            }
            if j < buf.len() && (buf[j] == b'M' || buf[j] == b'm') {
                let mut params = std::str::from_utf8(&buf[i + 3..j])
                    .unwrap_or("")
                    .split(';')
                    .map(|p| p.parse::<u32>().unwrap_or(0));
                let cb = params.next().unwrap_or(0);
                let x = params.next().unwrap_or(1).clamp(1, u32::from(u16::MAX)) as u16;
                let y = params.next().unwrap_or(1).clamp(1, u32::from(u16::MAX)) as u16;
                if cb & 64 != 0 {
                    // Wheel: low two bits 0 (up) or 1 (down); modifier
                    // bits may also be set. Wheel left/right dropped.
                    if buf[j] == b'M' && cb & 2 == 0 {
                        events.push(MouseEvent::Wheel {
                            up: cb & 1 == 0,
                            cb,
                            x,
                            y,
                        });
                    }
                } else if buf[j] == b'm' {
                    events.push(MouseEvent::Release { cb, x, y });
                } else if cb & 32 != 0 {
                    events.push(MouseEvent::Drag { cb, x, y });
                } else {
                    events.push(MouseEvent::Press {
                        left: cb & 3 == 0,
                        cb,
                        x,
                        y,
                    });
                }
                i = j + 1;
                after_seq = true;
                continue;
            }
            if j >= buf.len() {
                // Sequence split across reads. A real SGR mouse report
                // is a few bytes; anything this long is not one, so
                // dump it into the shell rather than buffering forever.
                if buf.len() - i > 64 {
                    clean.extend_from_slice(&buf[i..]);
                    i = buf.len();
                }
                break;
            }
        }
        if buf[i..].starts_with(b"\x1b[5~") || buf[i..].starts_with(b"\x1b[6~") {
            events.push(MouseEvent::Page {
                up: buf[i + 2] == b'5',
            });
            i += 4;
            after_seq = true;
            continue;
        }
        clean.push(buf[i]);
        i += 1;
        after_seq = false;
    }
    let rest = if i < buf.len() {
        buf[i..].to_vec()
    } else {
        Vec::new()
    };
    (clean, events, rest)
}

/// Consecutive drags only differ by pointer position; applying each one
/// forces libghostty to treat the screen as fully dirty. Keep the latest.
fn coalesce_mouse(events: Vec<MouseEvent>) -> Vec<MouseEvent> {
    let mut out = Vec::with_capacity(events.len());
    for event in events {
        if matches!(event, MouseEvent::Drag { .. })
            && matches!(out.last(), Some(MouseEvent::Drag { .. }))
        {
            *out.last_mut().unwrap() = event;
        } else {
            out.push(event);
        }
    }
    out
}

fn encode_mouse_event(event: &MouseEvent) -> Vec<u8> {
    match *event {
        MouseEvent::Wheel { cb, x, y, .. }
        | MouseEvent::Press { cb, x, y, .. }
        | MouseEvent::Drag { cb, x, y } => format!("\x1b[<{cb};{x};{y}M").into_bytes(),
        MouseEvent::Release { cb, x, y } => format!("\x1b[<{cb};{x};{y}m").into_bytes(),
        MouseEvent::Page { up } => {
            if up {
                b"\x1b[5~".to_vec()
            } else {
                b"\x1b[6~".to_vec()
            }
        }
    }
}

/// Coalesce a stdin chunk into one payload: carry an incomplete SGR
/// prefix in `tail`, keep only the latest drag, and re-encode. Used by
/// the attach client when the host terminal is already behind, so a
/// burst of motion becomes one C2S_INPUT instead of many.
pub(crate) fn compact_mouse_input(buf: &[u8], tail: &mut Vec<u8>) -> Vec<u8> {
    let stitched = if tail.is_empty() {
        None
    } else {
        let mut v = std::mem::take(tail);
        v.extend_from_slice(buf);
        Some(v)
    };
    let (clean, events, rest) = extract_mouse(stitched.as_deref().unwrap_or(buf));
    *tail = rest;
    let events = coalesce_mouse(events);
    let mut out = Vec::with_capacity(clean.len() + events.len() * 16);
    for event in &events {
        out.extend_from_slice(&encode_mouse_event(event));
    }
    out.extend_from_slice(&clean);
    out
}

/// Pane rectangles of a session's visible tab, in content coordinates
/// (the tab bar is already excluded by `size`).
fn tab_rects(session: &Session, size: (u16, u16)) -> Vec<(u64, Rect)> {
    let tab = &session.tabs[session.active_tab];
    let full = Rect {
        x: 0,
        y: 0,
        w: size.0,
        h: size.1,
    };
    let mut rects: Vec<(u64, Rect)> = Vec::new();
    if tab.zoomed {
        rects.push((tab.focused, full));
    } else {
        let _ = tab.layout.for_each(full, &mut |pane, rect| {
            rects.push((pane.id, rect));
            Ok(())
        });
    }
    rects
}

/// Which pane a pointer event belongs to. Drags and releases stick to
/// the pane the gesture started in (so a drag that leaves the pane
/// still extends its selection); everything else hit-tests the point.
fn pointer_target(
    rects: &[(u64, Rect)],
    sticky: Option<u64>,
    px: u16,
    py: u16,
) -> Option<(u64, Rect)> {
    let hit = rects
        .iter()
        .find(|(_, r)| px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h)
        .map(|(id, _)| *id);
    let id = sticky.or(hit)?;
    rects
        .iter()
        .find(|(pid, _)| *pid == id)
        .map(|(pid, r)| (*pid, *r))
}

/// Send a mouse event to an application that asked to track the mouse.
///
/// Coordinates are translated into the pane's own cell space — the app
/// believes it owns the whole screen, so forwarding raw screen
/// coordinates would land clicks off by the pane's origin. Encoding is
/// delegated to libghostty, which emits whatever protocol the app
/// actually requested (X10, SGR, urxvt, ...) and drops events its
/// tracking mode doesn't want (e.g. motion for a click-only app).
fn forward_mouse(
    encoder: &mut Option<libghostty_vt::mouse::Encoder<'static>>,
    pane: &crate::model::Pane,
    rect: Rect,
    cb: u32,
    press: bool,
    lx: u16,
    ly: u16,
) {
    use libghostty_vt::key::Mods;
    use libghostty_vt::mouse::{Action, Button, Encoder, EncoderSize, Event, Position};

    let enc = match encoder {
        Some(enc) => enc,
        None => match Encoder::new() {
            Ok(new) => encoder.insert(new),
            Err(_) => return,
        },
    };

    let motion = cb & 32 != 0;
    let wheel = cb & 64 != 0;
    let action = match (press, motion) {
        (false, _) => Action::Release,
        (_, true) => Action::Motion,
        _ => Action::Press,
    };
    let button = if wheel {
        match cb & 3 {
            0 => Button::Four,  // wheel up
            1 => Button::Five,  // wheel down
            2 => Button::Six,   // wheel left
            _ => Button::Seven, // wheel right
        }
    } else if cb & 128 != 0 {
        match cb & 3 {
            0 => Button::Eight,
            1 => Button::Nine,
            2 => Button::Ten,
            _ => Button::Eleven,
        }
    } else {
        match cb & 3 {
            0 => Button::Left,
            1 => Button::Middle,
            2 => Button::Right,
            _ => Button::Unknown,
        }
    };
    let mut mods = Mods::empty();
    if cb & 4 != 0 {
        mods |= Mods::SHIFT;
    }
    if cb & 8 != 0 {
        mods |= Mods::ALT;
    }
    if cb & 16 != 0 {
        mods |= Mods::CTRL;
    }

    let (cw, ch) = crate::model::CELL_PX;
    let build = (|| -> crate::Result<Vec<u8>> {
        let mut event = Event::new()?;
        event
            .set_action(action)
            .set_button(Some(button))
            .set_mods(mods)
            // Aim at the middle of the cell so rounding can't spill
            // into a neighbour.
            .set_position(Position {
                x: (f32::from(lx) + 0.5) * cw as f32,
                y: (f32::from(ly) + 0.5) * ch as f32,
            });
        enc.set_options_from_terminal(&pane.term)
            .set_any_button_pressed(motion && !wheel)
            .set_size(EncoderSize {
                screen_width: u32::from(rect.w.max(1)) * cw,
                screen_height: u32::from(rect.h.max(1)) * ch,
                cell_width: cw,
                cell_height: ch,
                padding_top: 0,
                padding_bottom: 0,
                padding_right: 0,
                padding_left: 0,
            });
        let mut out = Vec::with_capacity(16);
        enc.encode_to_vec(&event, &mut out)?;
        Ok(out)
    })();
    // An empty encoding means the app's tracking mode doesn't want it.
    if let Ok(bytes) = build
        && !bytes.is_empty()
    {
        pane.pty.write(&bytes);
    }
}

/// Scroll one pane: alt-screen apps get arrow keys (like most terminal
/// emulators), everything else scrolls our own scrollback.
fn scroll_pane(pane: &mut crate::model::Pane, up: bool, page: bool) {
    use libghostty_vt::screen::Screen;
    use libghostty_vt::terminal::{Mode as TermMode, ScrollViewport};

    let alt = matches!(pane.term.active_screen(), Ok(Screen::Alternate));
    if page {
        if alt {
            pane.pty.write(if up { b"\x1b[5~" } else { b"\x1b[6~" });
        } else {
            let rows = pane.term.rows().unwrap_or(24).saturating_sub(2) as isize;
            let delta = if up { -rows } else { rows };
            pane.term.scroll_viewport(ScrollViewport::Delta(delta));
        }
        return;
    }
    if alt {
        let key: &[u8] = match (pane.term.mode(TermMode::DECCKM), up) {
            (Ok(true), true) => b"\x1bOA",
            (Ok(true), false) => b"\x1bOB",
            (_, true) => b"\x1b[A",
            (_, false) => b"\x1b[B",
        };
        for _ in 0..3 {
            pane.pty.write(key);
        }
    } else {
        let delta = if up { -3 } else { 3 };
        pane.term.scroll_viewport(ScrollViewport::Delta(delta));
    }
}

/// Per-client mouse-selection state (`select_copy`).
#[derive(Default)]
pub struct SelectState {
    /// The gesture and the pane it is anchored in.
    gesture: Option<(u64, libghostty_vt::selection::gesture::Gesture<'static>)>,
    /// Pane holding an installed (visible) selection.
    selected_pane: Option<u64>,
    /// Pane-local cell the gesture was anchored at.
    anchor: Option<(u16, u16)>,
    /// Whether the current drag runs backward (before the anchor).
    backward: bool,
    /// Encoder for events forwarded to apps that track the mouse; it
    /// keeps motion-dedup state, so it is per-client and long-lived.
    encoder: Option<libghostty_vt::mouse::Encoder<'static>>,
    /// Incomplete SGR/page prefix from the previous input chunk.
    mouse_tail: Vec<u8>,
}

/// Look up a pane by id across every session.
fn pane_by_id(sessions: &mut [Session], pid: u64) -> Option<&mut crate::model::Pane> {
    for session in sessions.iter_mut() {
        for tab in &mut session.tabs {
            if tab.layout.pane(pid).is_some() {
                return tab.layout.pane_mut(pid);
            }
        }
    }
    None
}

/// Drop the gesture after resetting it against its pane, so libghostty
/// can untrack the click pin. Dropping without reset leaks that pin
/// until the terminal itself is dropped.
fn drop_gesture(select: &mut SelectState, sessions: &mut [Session]) {
    let Some((pid, mut gesture)) = select.gesture.take() else {
        return;
    };
    if let Some(pane) = pane_by_id(sessions, pid) {
        gesture.reset(&pane.term);
    }
}

/// Clear the visible selection (if any) and forget the gesture.
fn clear_selection(select: &mut SelectState, sessions: &mut [Session]) {
    drop_gesture(select, sessions);
    let Some(pid) = select.selected_pane.take() else {
        return;
    };
    if let Some(pane) = pane_by_id(sessions, pid) {
        let _ = pane.term.set_selection(None);
    }
}

/// Handle press/drag/release for mouse select-to-copy. Returns text to
/// copy to the client's clipboard when a selection completes.
fn apply_select(
    kind: u8,
    pane_id: u64,
    rect: Rect,
    lx: u16,
    ly: u16,
    select: &mut SelectState,
    sessions: &mut [Session],
    active: usize,
    enabled: bool,
) -> Option<String> {
    use libghostty_vt::selection::gesture::{DragEvent, Gesture, PressEvent, ReleaseEvent};
    use libghostty_vt::selection::FormatOptions;
    use libghostty_vt::terminal::{Point, PointCoordinate};

    match kind {
        // Press: click-to-focus, then anchor a selection gesture.
        0 | 3 => {
            let session = &mut sessions[active];
            let tab = &mut session.tabs[session.active_tab];
            if kind == 3 || !enabled {
                return None; // middle/right click: focus only
            }
            // A press starts a fresh drag; the gesture lives only until
            // the button comes back up.
            select.gesture = Gesture::new().ok().map(|g| (pane_id, g));
            let Some((_, gesture)) = &mut select.gesture else {
                return None;
            };
            let Some(pane) = tab.layout.pane(pane_id) else {
                return None;
            };
            let result = (|| -> crate::Result<Option<()>> {
                let grid_ref = pane.term.grid_ref(Point::Viewport(PointCoordinate {
                    x: lx,
                    y: u32::from(ly),
                }))?;
                let mut press = PressEvent::new()?;
                // Left untimed on purpose: libghostty then offers only
                // single-click behaviour, which is all we want.
                press
                    .set_position(
                        // Left-of-cell boundary: the anchor cell is
                        // included when dragging forward (re-anchored
                        // right-of-cell when a drag turns backward).
                        (f64::from(lx) + 0.25) * f64::from(crate::model::CELL_PX.0),
                        (f64::from(ly) + 0.5) * f64::from(crate::model::CELL_PX.1),
                    )?;
                if let Some(selection) = press.apply(gesture, &pane.term, grid_ref)? {
                    pane.term.set_selection(Some(&selection))?;
                    return Ok(Some(()));
                }
                pane.term.set_selection(None)?;
                Ok(None)
            })();
            if matches!(result, Ok(Some(()))) {
                select.selected_pane = Some(pane_id);
            }
            select.anchor = Some((lx, ly));
            select.backward = false;
            None
        }
        // Drag: extend the selection.
        1 => {
            if !enabled {
                return None;
            }
            let Some((gpid, _)) = &select.gesture else {
                return None;
            };
            if *gpid != pane_id {
                return None;
            }
            let (ax, ay) = select.anchor?;
            // Endpoint boundaries sit on the biased side of the cell:
            // the leftmost end needs a left bias, the rightmost a right
            // bias. The anchor's side is fixed at press time, so when a
            // drag crosses to the other side of the anchor, re-anchor
            // with a fresh (untimed) press biased the other way.
            let backward = (ly, lx) < (ay, ax);
            if backward != select.backward {
                select.backward = backward;
                drop_gesture(select, sessions);
                let rebuilt = (|| -> crate::Result<()> {
                    let session = &sessions[active];
                    let tab = &session.tabs[session.active_tab];
                    let Some(pane) = tab.layout.pane(pane_id) else {
                        return Err("pane gone".into());
                    };
                    let mut gesture = Gesture::new()?;
                    let grid_ref = pane.term.grid_ref(Point::Viewport(PointCoordinate {
                        x: ax,
                        y: u32::from(ay),
                    }))?;
                    let bias = if backward { 0.75 } else { 0.25 };
                    let mut press = PressEvent::new()?;
                    press.set_position(
                        (f64::from(ax) + bias) * f64::from(crate::model::CELL_PX.0),
                        (f64::from(ay) + 0.5) * f64::from(crate::model::CELL_PX.1),
                    )?;
                    let _ = press.apply(&mut gesture, &pane.term, grid_ref)?;
                    select.gesture = Some((pane_id, gesture));
                    Ok(())
                })();
                if rebuilt.is_err() {
                    return None;
                }
            }
            let Some((_, gesture)) = &mut select.gesture else {
                return None;
            };
            let session = &sessions[active];
            let tab = &session.tabs[session.active_tab];
            let Some(pane) = tab.layout.pane(pane_id) else {
                return None;
            };
            let pointer_bias = if backward { 0.25 } else { 0.75 };
            let result = (|| -> crate::Result<Option<()>> {
                let grid_ref = pane.term.grid_ref(Point::Viewport(PointCoordinate {
                    x: lx,
                    y: u32::from(ly),
                }))?;
                let mut drag = DragEvent::new()?;
                drag.set_position(
                    (f64::from(lx) + pointer_bias) * f64::from(crate::model::CELL_PX.0),
                    (f64::from(ly) + 0.5) * f64::from(crate::model::CELL_PX.1),
                )?;
                let geometry = libghostty_vt::selection::gesture::Geometry {
                    columns: u32::from(rect.w.max(1)),
                    cell_width: crate::model::CELL_PX.0,
                    padding_left: 0,
                    screen_height: u32::from(rect.h.max(1)) * crate::model::CELL_PX.1,
                };
                if let Some(selection) = drag.apply(gesture, &pane.term, grid_ref, geometry)? {
                    pane.term.set_selection(Some(&selection))?;
                    return Ok(Some(()));
                }
                Ok(None)
            })();
            if matches!(result, Ok(Some(()))) {
                select.selected_pane = Some(pane_id);
            }
            None
        }
        // Release: finish the gesture; copy when something is selected.
        _ => {
            if !enabled {
                return None;
            }
            let Some((gpid, gesture)) = &mut select.gesture else {
                return None;
            };
            if *gpid != pane_id {
                return None;
            }
            let session = &sessions[active];
            let tab = &session.tabs[session.active_tab];
            let Some(pane) = tab.layout.pane(pane_id) else {
                return None;
            };
            let copied = (|| -> crate::Result<Option<String>> {
                let grid_ref = pane.term.grid_ref(Point::Viewport(PointCoordinate {
                    x: lx,
                    y: u32::from(ly),
                }))?;
                let mut release = ReleaseEvent::new()?;
                release.apply(gesture, &pane.term, Some(grid_ref))?;
                // A bare click just moves focus; only a drag copies.
                let dragged = gesture.dragged(&pane.term).unwrap_or(false);
                if !dragged {
                    return Ok(None);
                }
                // Format from the installed selection *before* it is
                // dropped below.
                let options = FormatOptions::new().with_unwrap(true).with_trim(true);
                let text = pane
                    .term
                    .format_selection_alloc(None, options)?
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .filter(|t| !t.is_empty());
                Ok(text)
            })()
            .ok()
            .flatten();
            // The drag is over: the text is on the clipboard, so the
            // highlight has done its job and the gesture must not
            // outlive it (a stale one would capture later drags).
            clear_selection(select, sessions);
            copied
        }
    }
}

/// A key press inside a manager overlay.
pub enum MgrAction {
    Up,
    Down,
    Select,
    New,
    Rename,
    Kill,
    /// Flip the session manager between normal and agent sessions.
    ToggleAgents,
    /// Start typing a `/` filter.
    Search,
    Close,
}

/// What happened after applying manager key presses.
enum MgrOutcome {
    /// Keep showing the manager.
    Stay,
    /// Close it and show the viewed tab.
    Close,
    /// Switch to the selected entry, then close.
    Switch(usize),
    /// Open the name prompt for a new session/tab.
    StartNaming,
    /// Open the rename prompt for the selected entry.
    Rename(usize),
    /// Kill the selected entry (all its shells).
    Kill(usize),
    /// Flip between the normal and agent session lists.
    ToggleAgents,
    /// Open the `/` filter prompt.
    StartSearch,
}

/// Parse manager-mode key presses from raw stdin bytes. The chords bound
/// to opening a manager also close it (toggle behavior).
pub fn manager_actions(buf: &[u8], bindings: &[Binding]) -> Vec<MgrAction> {
    let mut actions = Vec::new();
    let mut i = 0;
    'scan: while i < buf.len() {
        for binding in bindings {
            if matches!(
                binding.action,
                BindingAction::Control(InputAction::Manager(_))
            ) && buf[i..].starts_with(&binding.seq)
            {
                actions.push(MgrAction::Close);
                i += binding.seq.len();
                continue 'scan;
            }
        }
        match buf[i] {
            b'\r' | b'\n' => actions.push(MgrAction::Select),
            b'n' => actions.push(MgrAction::New),
            b'r' => actions.push(MgrAction::Rename),
            b'x' => actions.push(MgrAction::Kill),
            b'a' => actions.push(MgrAction::ToggleAgents),
            b'/' => actions.push(MgrAction::Search),
            b'j' => actions.push(MgrAction::Down),
            b'k' => actions.push(MgrAction::Up),
            b'q' => actions.push(MgrAction::Close),
            0x1b => {
                // Arrow keys arrive as ESC [ A/B; a bare ESC closes.
                if buf[i + 1..].starts_with(b"[A") {
                    actions.push(MgrAction::Up);
                    i += 3;
                    continue;
                }
                if buf[i + 1..].starts_with(b"[B") {
                    actions.push(MgrAction::Down);
                    i += 3;
                    continue;
                }
                actions.push(MgrAction::Close);
            }
            _ => {}
        }
        i += 1;
    }
    actions
}

/// Apply manager key presses; any actions after one that leaves the
/// manager are dropped.
fn manager_apply(actions: &[MgrAction], selected: &mut usize, count: usize) -> MgrOutcome {
    // Drop a stale cursor (a sessions-vec index, a killed row) onto the
    // last real row before moving, so up/down never walk phantom slots.
    if count == 0 {
        *selected = 0;
    } else {
        *selected = (*selected).min(count - 1);
    }
    for action in actions {
        match action {
            MgrAction::Up => *selected = selected.saturating_sub(1),
            MgrAction::Down => *selected = (*selected + 1).min(count - 1),
            MgrAction::Select => return MgrOutcome::Switch(*selected),
            MgrAction::New => return MgrOutcome::StartNaming,
            MgrAction::Rename => return MgrOutcome::Rename(*selected),
            MgrAction::Kill => return MgrOutcome::Kill(*selected),
            MgrAction::ToggleAgents => return MgrOutcome::ToggleAgents,
            MgrAction::Search => return MgrOutcome::StartSearch,
            MgrAction::Close => return MgrOutcome::Close,
        }
    }
    MgrOutcome::Stay
}

/// Case-insensitive substring match for `/` filters.
pub fn name_matches(name: &str, query: &str) -> bool {
    query.is_empty() || name.to_lowercase().contains(&query.to_lowercase())
}

/// The names the manager currently lists (used for `/` filtering).
pub fn manager_names(overlay: Overlay, sessions: &[Session], active: usize, pins: &[Pin]) -> Vec<String> {
    match overlay {
        Overlay::Sessions { agents } => crate::agent::manager_entries(pins, sessions, agents)
            .into_iter()
            .map(|e| e.name)
            .collect(),
        Overlay::Tabs => sessions[active].tabs.iter().map(|t| t.name.clone()).collect(),
    }
}

/// How many rows the overlay currently lists.
pub fn manager_count(overlay: Overlay, sessions: &[Session], active: usize, pins: &[Pin]) -> usize {
    match overlay {
        Overlay::Sessions { agents } => crate::agent::manager_entries(pins, sessions, agents).len(),
        Overlay::Tabs => sessions.get(active).map(|s| s.tabs.len()).unwrap_or(0),
    }
}

/// Highlight index of the viewed session/tab in the overlay's list.
/// A session that belongs to the other list (agent vs normal) is not a
/// row here, so the cursor falls back to 0 rather than the raw vec index.
pub fn manager_cursor(overlay: Overlay, sessions: &[Session], active: usize, pins: &[Pin]) -> usize {
    match overlay {
        Overlay::Sessions { agents } => crate::agent::manager_entries(pins, sessions, agents)
            .iter()
            .position(|e| e.running == Some(active))
            .unwrap_or(0),
        Overlay::Tabs => sessions.get(active).map(|s| s.active_tab).unwrap_or(0),
    }
}

/// Handle keys while a `/` filter is being typed: text edits the query,
/// arrows move within the matches, Enter switches to the selection, a
/// bare Esc cancels the search.
fn run_search(
    buf: &[u8],
    overlay: Overlay,
    selected: &mut usize,
    query: &mut TextInput,
    sessions: &mut Vec<Session>,
    active: &mut usize,
    size: (u16, u16),
    config: &Config,
) -> Result<Option<Mode>> {
    let before = query.text.clone();
    // The field owns editing and cursor movement; up/down come back as
    // list movement for the manager to apply.
    let (outcome, moved) = query.apply(buf, 40);
    let matches = |sessions: &[Session], active: usize| -> Vec<usize> {
        manager_names(overlay, sessions, active, &config.pins)
            .iter()
            .enumerate()
            .filter(|(_, name)| name_matches(name, &query.text))
            .map(|(idx, _)| idx)
            .collect()
    };
    // Editing the query re-filters the list, so the old selection index
    // no longer means anything.
    if query.text != before {
        *selected = 0;
    }
    if moved != 0 {
        let count = matches(sessions, *active).len();
        let last = count.saturating_sub(1);
        *selected = selected.saturating_add_signed(moved).min(last);
    }

    match outcome {
        NamingOutcome::Pending => Ok(None),
        // Esc drops the filter and returns to the plain manager.
        NamingOutcome::Cancel => Ok(Some(Mode::Manager {
            overlay,
            selected: 0,
            search: None,
        })),
        NamingOutcome::Create => {
            let matched = matches(sessions, *active);
            let Some(&orig) = matched.get((*selected).min(matched.len().saturating_sub(1)))
            else {
                return Ok(None); // no matches: stay in the search
            };
            match overlay {
                Overlay::Sessions { agents } => {
                    let entries = crate::agent::manager_entries(&config.pins, sessions, agents);
                    if let Some(entry) = entries.get(orig) {
                        *active = match entry.running {
                            Some(si) => si,
                            None => create_session(sessions, config, size, entry.name.clone())?,
                        };
                    }
                }
                Overlay::Tabs => sessions[*active].active_tab = orig,
            }
            Ok(Some(Mode::Running))
        }
    }
}

/// Apply manager key presses to whichever list the overlay shows.
/// Returns the mode to switch to, or `None` to stay in the manager with
/// the (possibly moved) selection. Selecting a pinned session that isn't
/// running starts it.
pub fn run_manager(
    actions: &[MgrAction],
    overlay: Overlay,
    selected: &mut usize,
    sessions: &mut Vec<Session>,
    active: &mut usize,
    size: (u16, u16),
    config: &Config,
) -> Result<Option<Mode>> {
    let pins: &[Pin] = &config.pins;
    let agents = matches!(overlay, Overlay::Sessions { agents: true });
    let count = manager_count(overlay, sessions, *active, pins);
    Ok(match manager_apply(actions, selected, count.max(1)) {
        MgrOutcome::Stay => None,
        MgrOutcome::Close => Some(Mode::Running),
        MgrOutcome::StartSearch => Some(Mode::Manager {
            overlay,
            selected: 0,
            search: Some(TextInput::default()),
        }),
        MgrOutcome::ToggleAgents => match overlay {
            Overlay::Sessions { agents } => {
                let overlay = Overlay::Sessions { agents: !agents };
                *selected = manager_cursor(overlay, sessions, *active, pins);
                Some(Mode::Manager {
                    overlay,
                    selected: *selected,
                    search: None,
                })
            }
            Overlay::Tabs => None,
        },
        MgrOutcome::Switch(i) => {
            match overlay {
                Overlay::Sessions { .. } => {
                    let entries = crate::agent::manager_entries(pins, sessions, agents);
                    if let Some(entry) = entries.get(i) {
                        *active = match entry.running {
                            Some(si) => si,
                            None => {
                                create_session(sessions, config, size, entry.name.clone())?
                            }
                        };
                    }
                }
                Overlay::Tabs => sessions[*active].active_tab = i,
            }
            Some(Mode::Running)
        }
        MgrOutcome::StartNaming => Some(Mode::Naming {
            overlay,
            name: TextInput::default(),
            rename: None,
        }),
        MgrOutcome::Rename(i) => match overlay {
            Overlay::Sessions { .. } => {
                let entries = crate::agent::manager_entries(pins, sessions, agents);
                match entries.get(i).and_then(|e| e.running) {
                    Some(si) => Some(Mode::Naming {
                        overlay,
                        name: TextInput::new(sessions[si].name.clone()),
                        rename: Some(RenameTarget::Session(sessions[si].id)),
                    }),
                    // A stopped pin's name comes from the config.
                    None => None,
                }
            }
            Overlay::Tabs => sessions[*active].tabs.get(i).map(|tab| Mode::Naming {
                overlay,
                name: TextInput::new(tab.name.clone()),
                rename: Some(RenameTarget::Tab(i)),
            }),
        },
        MgrOutcome::Kill(i) => match overlay {
            Overlay::Sessions { .. } => {
                let entries = crate::agent::manager_entries(pins, sessions, agents);
                if let Some(si) = entries.get(i).and_then(|e| e.running) {
                    let viewed = sessions.get(*active).map(|s| s.id);
                    // Dropping the session closes its ptys; the shells
                    // get SIGHUP and the kernel reaps them.
                    let killed = sessions.remove(si);
                    if viewed == Some(killed.id) {
                        *active = 0;
                    } else if let Some(vid) = viewed {
                        *active = sessions.iter().position(|s| s.id == vid).unwrap_or(0);
                    }
                }
                *selected = (*selected)
                    .min(manager_count(overlay, sessions, *active, pins).saturating_sub(1));
                None // stay in the manager
            }
            Overlay::Tabs => {
                let session = &mut sessions[*active];
                if session.tabs.len() > 1 && i < session.tabs.len() {
                    session.tabs.remove(i);
                    if i < session.active_tab {
                        session.active_tab -= 1;
                    }
                    session.active_tab = session.active_tab.min(session.tabs.len() - 1);
                    *selected = (*selected).min(session.tabs.len() - 1);
                    None // stay in the manager
                } else if session.tabs.len() == 1 && i == 0 {
                    // Killing the last tab kills the session.
                    sessions.remove(*active);
                    *active = 0;
                    Some(Mode::Running)
                } else {
                    None
                }
            }
        },
    })
}

/// What happened after applying name-prompt key presses.
pub enum NamingOutcome {
    /// Still typing.
    Pending,
    /// Enter pressed — create with the typed name.
    Create,
    /// Esc pressed — back to the manager.
    Cancel,
}

/// A single-line text field with a cursor. Every prompt in xmux — the
/// name prompts, the pane settings, the manager search — edits through
/// this, so they all support the same keys.
#[derive(Default)]
pub struct TextInput {
    pub text: String,
    /// Cursor position, counted in characters (`0..=len`).
    pub cursor: usize,
}

impl TextInput {
    /// A field pre-filled with `text`, cursor at the end.
    pub fn new(text: String) -> Self {
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// Byte offset of character `idx` (end of string when past the end).
    fn byte_at(&self, idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(idx)
            .map_or(self.text.len(), |(b, _)| b)
    }

    fn insert(&mut self, c: char, max: usize) {
        if self.len() >= max {
            return;
        }
        let at = self.byte_at(self.cursor);
        self.text.insert(at, c);
        self.cursor += 1;
    }

    /// Delete the character before the cursor.
    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let range = self.byte_at(self.cursor - 1)..self.byte_at(self.cursor);
        self.text.replace_range(range, "");
        self.cursor -= 1;
    }

    /// Delete the character under the cursor.
    fn delete(&mut self) {
        if self.cursor >= self.len() {
            return;
        }
        let range = self.byte_at(self.cursor)..self.byte_at(self.cursor + 1);
        self.text.replace_range(range, "");
    }

    /// Delete the word before the cursor (ctrl+w).
    fn delete_word(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut start = self.cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let range = self.byte_at(start)..self.byte_at(self.cursor);
        self.text.replace_range(range, "");
        self.cursor = start;
    }

    /// Apply a chunk of client input. Returns the prompt outcome and the
    /// net up/down arrow movement (used by the manager search to move
    /// its selection; the name prompts ignore it).
    pub fn apply(&mut self, buf: &[u8], max: usize) -> (NamingOutcome, isize) {
        let mut moved = 0isize;
        let mut i = 0;
        while i < buf.len() {
            match buf[i] {
                0x1b => {
                    // CSI (ESC [ ...) or SS3 (ESC O x): a key, not a
                    // cancel. Anything else beginning with ESC — a bare
                    // Escape, or an alt+key chord — closes the prompt.
                    let Some(&intro) = buf.get(i + 1) else {
                        return (NamingOutcome::Cancel, moved);
                    };
                    if intro != b'[' && intro != b'O' {
                        return (NamingOutcome::Cancel, moved);
                    }
                    let mut j = i + 2;
                    if intro == b'[' {
                        while j < buf.len() && (0x30..=0x3f).contains(&buf[j]) {
                            j += 1;
                        }
                        while j < buf.len() && (0x20..=0x2f).contains(&buf[j]) {
                            j += 1;
                        }
                    }
                    let Some(&final_byte) = buf.get(j) else {
                        break; // sequence split across reads: swallow it
                    };
                    let param: u32 = std::str::from_utf8(&buf[i + 2..j])
                        .ok()
                        .and_then(|s| s.split(';').next()?.parse().ok())
                        .unwrap_or(0);
                    match final_byte {
                        b'D' => self.cursor = self.cursor.saturating_sub(1),
                        b'C' => self.cursor = (self.cursor + 1).min(self.len()),
                        b'A' => moved -= 1,
                        b'B' => moved += 1,
                        b'H' => self.cursor = 0,
                        b'F' => self.cursor = self.len(),
                        b'~' => match param {
                            1 | 7 => self.cursor = 0,
                            3 => self.delete(),
                            4 | 8 => self.cursor = self.len(),
                            _ => {}
                        },
                        // Unknown sequence: swallow rather than cancel.
                        _ => {}
                    }
                    i = j + 1;
                    continue;
                }
                b'\r' | b'\n' => return (NamingOutcome::Create, moved),
                0x7f | 0x08 => self.backspace(),
                0x01 => self.cursor = 0,            // ctrl+a
                0x05 => self.cursor = self.len(),   // ctrl+e
                0x15 => {
                    // ctrl+u: clear the line
                    self.text.clear();
                    self.cursor = 0;
                }
                0x17 => self.delete_word(),         // ctrl+w
                b if b < 0x20 => {}                 // other control bytes
                b => {
                    // One UTF-8 character, however many bytes it takes.
                    let width = match b {
                        0x00..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    let end = (i + width).min(buf.len());
                    if let Some(c) = String::from_utf8_lossy(&buf[i..end]).chars().next() {
                        self.insert(c, max);
                    }
                    i = end;
                    continue;
                }
            }
            i += 1;
        }
        (NamingOutcome::Pending, moved)
    }
}

/// Handle one chunk of client input against the client's viewed session.
///
/// `active` is the index of the viewed session and may change (manager
/// switches); `mode` is the client's overlay state. Returns
/// `(detach, copied, mouse_only)`: `mouse_only` is set when the chunk
/// contained pointer events and no keyboard bytes, so the renderer can
/// skip a synchronized update.
pub fn handle_input(
    buf: &[u8],
    mode: &mut Mode,
    sessions: &mut Vec<Session>,
    active: &mut usize,
    size: (u16, u16),
    config: &Config,
    select: &mut SelectState,
) -> Result<(bool, Option<String>, bool)> {
    use crate::model::SplitDir;
    let bindings: &[Binding] = &config.bindings;

    // Pull mouse and PageUp/PageDown events out of the stream first.
    // An incomplete SGR sequence at the end of the last chunk is
    // prepended so a split report is not leaked into the shell.
    let stitched = if select.mouse_tail.is_empty() {
        None
    } else {
        let mut v = std::mem::take(&mut select.mouse_tail);
        v.extend_from_slice(buf);
        Some(v)
    };
    let (clean, mouse, rest) = extract_mouse(stitched.as_deref().unwrap_or(buf));
    select.mouse_tail = rest;
    let mouse = coalesce_mouse(mouse);
    let mouse_only = clean.is_empty() && !mouse.is_empty();
    let mut synthetic: Vec<u8> = Vec::new();
    let mut copied: Option<String> = None;
    match mode {
        Mode::Running => {
            for event in &mouse {
                // PageUp/PageDown have no position: they act on the
                // focused pane.
                let Some((cb, x, y)) = event.at() else {
                    let session = &mut sessions[*active];
                    let tab = &mut session.tabs[session.active_tab];
                    let focused = tab.focused;
                    let up = matches!(event, MouseEvent::Page { up: true });
                    if let Some(pane) = tab.layout.pane_mut(focused) {
                        if pane.term.is_mouse_tracking().unwrap_or(false) {
                            pane.pty.write(if up { b"\x1b[5~" } else { b"\x1b[6~" });
                        } else {
                            scroll_pane(pane, up, true);
                        }
                    }
                    continue;
                };

                // 1-based screen coords -> 0-based content coords (a
                // top bar shifts content down a row; the bar itself is
                // not part of any pane).
                let px = x.saturating_sub(1);

                let kind = match event {
                    MouseEvent::Press { left: true, .. } => 0u8,
                    MouseEvent::Press { .. } => 3,
                    MouseEvent::Drag { .. } => 1,
                    MouseEvent::Release { .. } => 2,
                    _ => 4, // wheel
                };

                // The tab bar is xmux's own row: clicking a tab label
                // opens that tab, and nothing there belongs to a pane.
                let bar_row = if config.bar_top { 1 } else { size.1 + 1 };
                if y == bar_row {
                    if matches!(kind, 0 | 3) {
                        clear_selection(select, sessions);
                        let session = &mut sessions[*active];
                        if let Some(ti) = crate::render::tab_at(session, size.0, px) {
                            session.active_tab = ti;
                        }
                    }
                    continue;
                }

                let Some(py) = y.checked_sub(1 + u16::from(config.bar_top)) else {
                    continue;
                };
                // Drags and releases belong to the pane the gesture
                // started in; presses and wheel hit-test the point.
                let sticky = match kind {
                    1 | 2 => select.gesture.as_ref().map(|(pid, _)| *pid),
                    _ => None,
                };
                let rects = tab_rects(&sessions[*active], size);
                let Some((pane_id, rect)) = pointer_target(&rects, sticky, px, py) else {
                    continue; // divider or outside every pane
                };
                // Pane-local cell coordinates, clamped into the pane.
                let lx = px.clamp(rect.x, rect.x + rect.w.saturating_sub(1)) - rect.x;
                let ly = py.clamp(rect.y, rect.y + rect.h.saturating_sub(1)) - rect.y;

                // A click (any button) or a scroll focuses the pane it
                // landed in, so the mouse and the keyboard can never
                // point at different panes. This has to happen before
                // routing, since apps that track the mouse take the
                // event and never reach the selection path. Drags and
                // releases belong to a gesture already in progress.
                if matches!(kind, 0 | 3 | 4) {
                    let session = &mut sessions[*active];
                    let tab = &mut session.tabs[session.active_tab];
                    tab.focused = pane_id;
                }

                // Any press ends the previous selection, wherever it
                // was — including presses that go on to an app which
                // takes the mouse, which never reach the selection code
                // below. Dropping the gesture with it is what keeps a
                // finished selection from capturing later drags.
                if matches!(kind, 0 | 3) {
                    clear_selection(select, sessions);
                }

                let session = &mut sessions[*active];
                let tab = &mut session.tabs[session.active_tab];
                let tracking = tab
                    .layout
                    .pane(pane_id)
                    .is_some_and(|p| p.term.is_mouse_tracking().unwrap_or(false));

                if tracking {
                    // The app wants the mouse: hand it the event in its
                    // own coordinate space and protocol.
                    if let Some(pane) = tab.layout.pane(pane_id) {
                        let press = !matches!(event, MouseEvent::Release { .. });
                        forward_mouse(&mut select.encoder, pane, rect, cb, press, lx, ly);
                    }
                    continue;
                }

                if kind == 4 {
                    // Wheel scrolls the pane under the pointer.
                    let up = matches!(event, MouseEvent::Wheel { up: true, .. });
                    if let Some(pane) = tab.layout.pane_mut(pane_id) {
                        scroll_pane(pane, up, false);
                    }
                    continue;
                }

                if let Some(text) = apply_select(
                    kind,
                    pane_id,
                    rect,
                    lx,
                    ly,
                    select,
                    sessions,
                    *active,
                    config.select_copy,
                ) {
                    copied = Some(text);
                }
            }
            if !clean.is_empty() {
                // Typing snaps the view back to the live bottom and
                // drops any visible selection.
                use libghostty_vt::terminal::ScrollViewport;
                clear_selection(select, sessions);
                let session = &mut sessions[*active];
                let tab = &mut session.tabs[session.active_tab];
                let focused = tab.focused;
                if let Some(pane) = tab.layout.pane_mut(focused) {
                    pane.term.scroll_viewport(ScrollViewport::Bottom);
                }
            }
        }
        // In a manager, wheel/page move the selection.
        Mode::Manager { .. } => {
            for event in &mouse {
                match event {
                    MouseEvent::Wheel { up: true, .. } | MouseEvent::Page { up: true } => {
                        synthetic.extend_from_slice(b"\x1b[A");
                    }
                    MouseEvent::Wheel { up: false, .. } | MouseEvent::Page { up: false } => {
                        synthetic.extend_from_slice(b"\x1b[B");
                    }
                    _ => {}
                }
            }
        }
        Mode::Naming { .. } | Mode::PaneSettings { .. } => {}
    }
    synthetic.extend_from_slice(&clean);
    let mut buf: &[u8] = &synthetic;

    loop {
        let mut next_mode = None;
        match mode {
            Mode::Running => {
                // Forward keyboard input untouched to the focused pane;
                // `forward_input` stops at bound control chords.
                let session = &sessions[*active];
                let tab = &session.tabs[session.active_tab];
                let pty = std::rc::Rc::clone(
                    &tab.layout
                        .pane(tab.focused)
                        .expect("focused pane exists")
                        .pty,
                );
                let Some((action, rest)) = forward_input(&pty, bindings, buf) else {
                    break;
                };
                buf = &buf[rest..];
                match action {
                    InputAction::Detach => return Ok((true, copied, mouse_only)),
                    InputAction::OpenSession(pi) => {
                        if let Some(pin) = config.pins.get(pi) {
                            *active = match sessions.iter().position(|s| s.name == pin.name) {
                                Some(si) => si,
                                None => {
                                    create_session(sessions, config, size, pin.name.clone())?
                                }
                            };
                        }
                    }
                    InputAction::Fullscreen => {
                        let session = &mut sessions[*active];
                        let tab = &mut session.tabs[session.active_tab];
                        tab.set_zoom(!tab.zoomed, size)?;
                    }
                    InputAction::PaneSettings => {
                        let session = &sessions[*active];
                        let tab = &session.tabs[session.active_tab];
                        let text = tab
                            .layout
                            .pane(tab.focused)
                            .and_then(|p| p.auto_run.clone())
                            .unwrap_or_default();
                        next_mode = Some(Mode::PaneSettings {
                            text: TextInput::new(text),
                        });
                    }
                    InputAction::SplitH | InputAction::SplitV => {
                        let dir = match action {
                            InputAction::SplitH => SplitDir::Horizontal,
                            _ => SplitDir::Vertical,
                        };
                        let session = &mut sessions[*active];
                        let tab = &mut session.tabs[session.active_tab];
                        tab.set_zoom(false, size)?;
                        tab.split(dir, size, config)?;
                    }
                    InputAction::FocusNext => {
                        let session = &mut sessions[*active];
                        let tab = &mut session.tabs[session.active_tab];
                        let _ = tab.set_zoom(false, size);
                        tab.focus_next();
                    }
                    InputAction::FocusDir(dir) => {
                        let session = &mut sessions[*active];
                        session.tabs[session.active_tab].set_zoom(false, size)?;
                        let moved =
                            session.tabs[session.active_tab].focus_dir(dir, size)?;
                        // At the tab's edge, left/right jump to the
                        // neighboring tab (wrapping) and land on the pane
                        // nearest the edge we came in over.
                        if !moved {
                            let count = session.tabs.len();
                            match dir {
                                NavDir::Right => {
                                    session.active_tab = (session.active_tab + 1) % count;
                                    session.tabs[session.active_tab]
                                        .focus_edge(NavDir::Left, size)?;
                                }
                                NavDir::Left => {
                                    session.active_tab =
                                        (session.active_tab + count - 1) % count;
                                    session.tabs[session.active_tab]
                                        .focus_edge(NavDir::Right, size)?;
                                }
                                NavDir::Up | NavDir::Down => {}
                            }
                        }
                    }
                    InputAction::Manager(overlay) => {
                        // Bytes typed right after the chord are already
                        // manager input.
                        let mut selected =
                            manager_cursor(overlay, sessions, *active, &config.pins);
                        next_mode = Some(
                            run_manager(
                                &manager_actions(buf, bindings),
                                overlay,
                                &mut selected,
                                sessions,
                                active,
                                size,
                                config,
                            )?
                            .unwrap_or(Mode::Manager { overlay, selected, search: None }),
                        );
                        buf = &[];
                    }
                }
            }
            Mode::Manager {
                overlay,
                selected,
                search,
            } => {
                next_mode = match search {
                    Some(query) => run_search(
                        buf, *overlay, selected, query, sessions, active, size, config,
                    )?,
                    None => run_manager(
                        &manager_actions(buf, bindings),
                        *overlay,
                        selected,
                        sessions,
                        active,
                        size,
                        config,
                    )?,
                };
                buf = &[];
            }
            Mode::Naming {
                overlay,
                name,
                rename,
            } => {
                match name.apply(buf, 40).0 {
                    NamingOutcome::Pending => {}
                    NamingOutcome::Cancel => {
                        next_mode = Some(Mode::Manager {
                            overlay: *overlay,
                            selected: manager_cursor(*overlay, sessions, *active, &config.pins),
                            search: None,
                        });
                    }
                    NamingOutcome::Create => {
                        let typed = std::mem::take(&mut name.text);
                        let typed = typed.trim().to_string();
                        match rename {
                            // Rename: apply and return to the manager;
                            // an empty name changes nothing.
                            Some(RenameTarget::Session(id)) => {
                                if !typed.is_empty()
                                    && let Some(s) = sessions.iter_mut().find(|s| s.id == *id)
                                {
                                    s.name = typed;
                                }
                                next_mode = Some(Mode::Manager {
                                    overlay: *overlay,
                                    selected: manager_cursor(
                                        *overlay,
                                        sessions,
                                        *active,
                                        &config.pins,
                                    ),
                                    search: None,
                                });
                            }
                            Some(RenameTarget::Tab(ti)) => {
                                if !typed.is_empty()
                                    && let Some(t) = sessions[*active].tabs.get_mut(*ti)
                                {
                                    t.name = typed;
                                }
                                next_mode = Some(Mode::Manager {
                                    overlay: *overlay,
                                    selected: *ti,
                                    search: None,
                                });
                            }
                            None => {
                                match overlay {
                                    Overlay::Sessions { agents } => {
                                        let name = if typed.is_empty() {
                                            format!("session {}", sessions.len() + 1)
                                        } else {
                                            typed
                                        };
                                        *active =
                                            create_session(sessions, config, size, name)?;
                                        // Created from the agent view =
                                        // an agent session.
                                        sessions[*active].agent = *agents;
                                    }
                                    Overlay::Tabs => {
                                        let session = &mut sessions[*active];
                                        let name = if typed.is_empty() {
                                            format!("tab {}", session.tabs.len() + 1)
                                        } else {
                                            typed
                                        };
                                        session.tabs.push(crate::model::Tab::new(size, name, config)?);
                                        session.active_tab = session.tabs.len() - 1;
                                    }
                                }
                                next_mode = Some(Mode::Running);
                            }
                        }
                    }
                }
                buf = &[];
            }
            Mode::PaneSettings { text } => {
                match text.apply(buf, 200).0 {
                    NamingOutcome::Pending => {}
                    NamingOutcome::Cancel => next_mode = Some(Mode::Running),
                    NamingOutcome::Create => {
                        let cmd = std::mem::take(&mut text.text).trim().to_string();
                        let session = &mut sessions[*active];
                        let tab = &mut session.tabs[session.active_tab];
                        let focused = tab.focused;
                        if let Some(pane) = tab.layout.pane_mut(focused) {
                            // Empty clears the auto-run.
                            pane.auto_run = (!cmd.is_empty()).then_some(cmd);
                        }
                        next_mode = Some(Mode::Running);
                    }
                }
                buf = &[];
            }
        }
        if let Some(m) = next_mode {
            *mode = m;
        }
        if buf.is_empty() {
            break;
        }
    }
    Ok((false, copied, mouse_only))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn session(name: &str, agent: bool, age_secs: u64) -> Session {
        Session {
            id: 0,
            name: name.to_string(),
            tabs: vec![],
            active_tab: 0,
            agent,
            last_activity: Instant::now() - Duration::from_secs(age_secs),
            last_size: (80, 24),
        }
    }

    /// meow (pin) + work + agent build + notes: vec indices 0..=3, but
    /// the normal list is meow/work/notes.
    fn mixed() -> (Vec<Pin>, Vec<Session>) {
        let pins = vec![Pin {
            name: "meow".to_string(),
        }];
        let sessions = vec![
            session("meow", false, 0),
            session("work", false, 0),
            session("build", true, 5),
            session("notes", false, 0),
        ];
        (pins, sessions)
    }

    #[test]
    fn normal_list_skips_agent_sessions() {
        let (pins, sessions) = mixed();
        let overlay = Overlay::Sessions { agents: false };
        assert_eq!(manager_count(overlay, &sessions, 0, &pins), 3);
        assert_eq!(manager_cursor(overlay, &sessions, 0, &pins), 0); // meow
        assert_eq!(manager_cursor(overlay, &sessions, 1, &pins), 1); // work
        assert_eq!(manager_cursor(overlay, &sessions, 2, &pins), 0); // agent: not in list
        assert_eq!(manager_cursor(overlay, &sessions, 3, &pins), 2); // notes, not vec 3
    }

    #[test]
    fn agent_list_skips_normal_sessions() {
        let pins = vec![Pin {
            name: "meow".to_string(),
        }];
        let sessions = vec![
            session("meow", false, 0),
            session("work", false, 0),
            session("old", true, 30),
            session("new", true, 1),
        ];
        let overlay = Overlay::Sessions { agents: true };
        assert_eq!(manager_count(overlay, &sessions, 0, &pins), 2);
        // Most recently active first: new (vec 3), then old (vec 2).
        assert_eq!(manager_cursor(overlay, &sessions, 3, &pins), 0);
        assert_eq!(manager_cursor(overlay, &sessions, 2, &pins), 1);
        assert_eq!(manager_cursor(overlay, &sessions, 0, &pins), 0); // normal: not in list
        assert_eq!(manager_cursor(overlay, &sessions, 1, &pins), 0);
    }

    #[test]
    fn stopped_pin_shifts_the_normal_cursor() {
        let pins = vec![
            Pin {
                name: "meow".to_string(),
            },
            Pin {
                name: "work".to_string(),
            },
        ];
        // meow is pinned but not running; work is vec 0, notes vec 1.
        let sessions = vec![session("work", false, 0), session("notes", false, 0)];
        let overlay = Overlay::Sessions { agents: false };
        assert_eq!(manager_count(overlay, &sessions, 0, &pins), 3);
        assert_eq!(manager_cursor(overlay, &sessions, 0, &pins), 1); // work, after stopped meow
        assert_eq!(manager_cursor(overlay, &sessions, 1, &pins), 2); // notes
    }

    #[test]
    fn up_from_a_vec_index_does_not_walk_phantom_rows() {
        // Viewing notes (vec 3) used to seed selected=3 in a 3-row list.
        let mut selected = 3;
        let outcome = manager_apply(&[MgrAction::Up], &mut selected, 3);
        assert!(matches!(outcome, MgrOutcome::Stay));
        assert_eq!(selected, 1); // clamp to 2, then up — not 2 (one phantom press)
    }

    #[test]
    fn down_from_a_vec_index_stays_on_the_last_row() {
        let mut selected = 5;
        let outcome = manager_apply(&[MgrAction::Down], &mut selected, 3);
        assert!(matches!(outcome, MgrOutcome::Stay));
        assert_eq!(selected, 2);
    }

    fn sgr(cb: u32, x: u16, y: u16, press: bool) -> Vec<u8> {
        let end = if press { 'M' } else { 'm' };
        format!("\x1b[<{cb};{x};{y}{end}").into_bytes()
    }

    #[test]
    fn extract_mouse_press_drag_release() {
        let mut buf = sgr(0, 10, 20, true);
        buf.extend(sgr(32, 12, 20, true));
        buf.extend(sgr(0, 12, 20, false));
        let (clean, events, rest) = extract_mouse(&buf);
        assert!(clean.is_empty());
        assert!(rest.is_empty());
        assert_eq!(
            events,
            vec![
                MouseEvent::Press {
                    left: true,
                    cb: 0,
                    x: 10,
                    y: 20
                },
                MouseEvent::Drag {
                    cb: 32,
                    x: 12,
                    y: 20
                },
                MouseEvent::Release {
                    cb: 0,
                    x: 12,
                    y: 20
                },
            ]
        );
    }

    #[test]
    fn extract_mouse_carries_a_split_sequence() {
        let full = sgr(32, 40, 10, true);
        let split = 6; // in the middle of the digits
        assert!(split < full.len());
        let (clean1, events1, rest1) = extract_mouse(&full[..split]);
        assert!(clean1.is_empty());
        assert!(events1.is_empty());
        assert_eq!(rest1, full[..split]);

        let mut next = rest1;
        next.extend_from_slice(&full[split..]);
        next.extend(sgr(32, 41, 10, true));
        let (clean2, events2, rest2) = extract_mouse(&next);
        assert!(clean2.is_empty());
        assert!(rest2.is_empty());
        assert_eq!(
            events2,
            vec![
                MouseEvent::Drag {
                    cb: 32,
                    x: 40,
                    y: 10
                },
                MouseEvent::Drag {
                    cb: 32,
                    x: 41,
                    y: 10
                },
            ]
        );
    }

    #[test]
    fn extract_mouse_does_not_leak_a_split_into_clean() {
        // Previously the first chunk dropped `\x1b[<32;1` and the
        // second chunk's `0;5M` went to the shell.
        let (c, e, rest) = extract_mouse(b"\x1b[<32;1");
        assert!(c.is_empty() && e.is_empty());
        let mut next = rest;
        next.extend_from_slice(b"0;5Mhello");
        let (clean, events, rem) = extract_mouse(&next);
        assert_eq!(clean, b"hello");
        assert!(rem.is_empty());
        assert_eq!(
            events,
            vec![MouseEvent::Drag {
                cb: 32,
                x: 10,
                y: 5
            }]
        );
    }

    #[test]
    fn a_lone_esc_is_delivered_instead_of_held() {
        // Holding it made the next keystroke complete an alt+ chord:
        // Esc then `h` arrived as `\x1bh` and moved the pane focus.
        let (clean, events, rest) = extract_mouse(b"\x1b");
        assert_eq!(clean, b"\x1b");
        assert!(events.is_empty() && rest.is_empty());

        // So the following keystroke stays a plain `h`.
        let (clean, _, rest) = extract_mouse(b"h");
        assert_eq!(clean, b"h");
        assert!(rest.is_empty());
    }

    #[test]
    fn an_esc_typed_after_other_keys_is_not_held_either() {
        // `x` then Esc landing in one read is still a human pressing
        // Esc, not a split report.
        let (clean, events, rest) = extract_mouse(b"x\x1b");
        assert_eq!(clean, b"x\x1b");
        assert!(events.is_empty() && rest.is_empty());
    }

    #[test]
    fn an_esc_right_after_a_report_is_still_held() {
        // Mid-burst split: the ESC begins the next report, so holding
        // it keeps `0;5M` out of the shell.
        let mut buf = sgr(0, 1, 1, true);
        buf.extend_from_slice(b"\x1b");
        let (clean, events, rest) = extract_mouse(&buf);
        assert!(clean.is_empty());
        assert_eq!(events.len(), 1);
        assert_eq!(rest, b"\x1b");

        let mut next = rest;
        next.extend_from_slice(b"[<32;10;5M");
        let (clean, events, rest) = extract_mouse(&next);
        assert!(clean.is_empty() && rest.is_empty());
        assert_eq!(
            events,
            vec![MouseEvent::Drag {
                cb: 32,
                x: 10,
                y: 5
            }]
        );
    }

    #[test]
    fn compact_mouse_input_coalesces_and_carries_a_tail() {
        let mut tail = Vec::new();
        let mut buf = sgr(0, 1, 1, true);
        buf.extend(sgr(32, 2, 1, true));
        buf.extend(sgr(32, 8, 4, true));
        buf.extend(sgr(0, 8, 4, false));
        let out = compact_mouse_input(&buf, &mut tail);
        assert!(tail.is_empty());
        let (clean, events, rest) = extract_mouse(&out);
        assert!(clean.is_empty() && rest.is_empty());
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], MouseEvent::Press { x: 1, y: 1, .. }));
        assert!(matches!(events[1], MouseEvent::Drag { x: 8, y: 4, .. }));
        assert!(matches!(events[2], MouseEvent::Release { x: 8, y: 4, .. }));

        let split = compact_mouse_input(b"\x1b[<32;1", &mut tail);
        assert!(split.is_empty());
        assert_eq!(tail, b"\x1b[<32;1");
        let rest = compact_mouse_input(b"0;5M", &mut tail);
        assert!(tail.is_empty());
        let (_, events, _) = extract_mouse(&rest);
        assert_eq!(
            events,
            vec![MouseEvent::Drag {
                cb: 32,
                x: 10,
                y: 5
            }]
        );
    }

    #[test]
    fn coalesce_mouse_keeps_the_latest_drag() {
        let events = vec![
            MouseEvent::Press {
                left: true,
                cb: 0,
                x: 1,
                y: 1,
            },
            MouseEvent::Drag {
                cb: 32,
                x: 2,
                y: 1,
            },
            MouseEvent::Drag {
                cb: 32,
                x: 3,
                y: 1,
            },
            MouseEvent::Drag {
                cb: 32,
                x: 8,
                y: 4,
            },
            MouseEvent::Release {
                cb: 0,
                x: 8,
                y: 4,
            },
        ];
        assert_eq!(
            coalesce_mouse(events),
            vec![
                MouseEvent::Press {
                    left: true,
                    cb: 0,
                    x: 1,
                    y: 1,
                },
                MouseEvent::Drag {
                    cb: 32,
                    x: 8,
                    y: 4,
                },
                MouseEvent::Release {
                    cb: 0,
                    x: 8,
                    y: 4,
                },
            ]
        );
    }
}
