//! Single-instance coordination over a Unix socket.
//!
//! A compositor keybinding outranks an exclusive layer-shell keyboard grab: on
//! cosmic-comp, once the switcher is on screen a second press of the binding
//! that launched it never reaches our surface, because the compositor consumes
//! the combination and runs the bound command again. That was measured, not
//! assumed — with `Alt+Tab` bound, no Tab key event is delivered to the
//! surface at all, while an unbound combination like `Alt+j` arrives normally.
//!
//! So cycling with the same key the switcher is bound to has to be driven by
//! process rather than by keystroke. The first invocation binds this socket and
//! draws the list; every later invocation connects, says which way to move, and
//! exits immediately. Releasing the modifier is still what commits, and that is
//! handled by the first instance, which holds the keyboard focus.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// Which way a later invocation asks the running switcher to move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Next,
    Prev,
}

impl Step {
    fn as_byte(self) -> u8 {
        match self {
            Self::Next => b'n',
            Self::Prev => b'p',
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            b'n' => Some(Self::Next),
            b'p' => Some(Self::Prev),
            _ => None,
        }
    }
}

/// Whether this process should draw the switcher or has already handed its
/// keystroke to the one that is.
pub enum Role {
    /// This process owns the switcher; keep `Primary` alive for its lifetime.
    Primary(Primary),
    /// A switcher was already running and has been told to move.
    Secondary,
}

/// The listening socket, which unlinks itself on drop.
///
/// Holds the guard by value rather than implementing `Drop` itself so that
/// [`Primary::into_parts`] can still move the socket out, while dropping a
/// `Primary` on any early return path — no windows to show, or a failure
/// between claiming and mapping — still removes the file.
pub struct Primary {
    listener: UnixListener,
    guard: PathGuard,
}

impl Primary {
    /// Splits into the socket and the unlink-on-drop guard, so the socket can
    /// be handed to an event loop that takes ownership while the file is still
    /// cleaned up when the guard goes out of scope.
    pub fn into_parts(self) -> (UnixListener, PathGuard) {
        (self.listener, self.guard)
    }
}

/// Removes the socket file when dropped.
pub struct PathGuard(PathBuf);

impl Drop for PathGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// One socket per Wayland display, so two sessions do not talk to each other.
fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    // WAYLAND_DISPLAY may be an absolute path rather than a bare name, and a
    // path would turn the file name into nested directories that do not exist.
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".to_string());
    let display = Path::new(&display)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("wayland-0")
        .to_string();

    dir.join(format!("xsw-{display}.sock"))
}

/// Becomes the switcher, or forwards `step` to the one already running.
pub fn claim(step: Step) -> std::io::Result<Role> {
    let path = socket_path();

    if let Some(role) = try_send(&path, step) {
        return Ok(role);
    }

    // Nothing answered, so any socket file present is stale: a previous
    // switcher was killed before its guard could unlink it. Bind fails with
    // AddrInUse until it is gone.
    let _ = std::fs::remove_file(&path);

    match UnixListener::bind(&path) {
        Ok(listener) => {
            listener.set_nonblocking(true)?;
            Ok(Role::Primary(Primary { listener, guard: PathGuard(path) }))
        }
        // Lost a race with another instance that bound between our failed
        // connect and this bind; it is the switcher, so hand it the keystroke.
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            Ok(try_send(&path, step).unwrap_or(Role::Secondary))
        }
        Err(err) => Err(err),
    }
}

/// Delivers `step` to a listening switcher, if one answers.
fn try_send(path: &Path, step: Step) -> Option<Role> {
    let mut stream = UnixStream::connect(path).ok()?;
    // A failed write means the peer died between connect and write; treat it
    // as delivered anyway rather than starting a second switcher on top of a
    // possibly still-mapped one.
    let _ = stream.write_all(&[step.as_byte()]);
    let _ = stream.flush();
    Some(Role::Secondary)
}

/// Reads the steps a connecting instance sent.
///
/// Reads to end of stream: the peer writes one byte and exits, so the read
/// terminates promptly, and batching means a burst of fast keypresses is not
/// dropped.
pub fn read_steps(stream: &mut UnixStream) -> Vec<Step> {
    // `accept` returns a blocking socket even from a non-blocking listener, so
    // without a timeout a peer that connects and never writes would stall the
    // event loop while the switcher is on screen holding the keyboard.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));

    let mut buffer = [0u8; 16];
    let mut steps = Vec::new();

    loop {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => steps.extend(buffer[..count].iter().copied().filter_map(Step::from_byte)),
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            // WouldBlock and TimedOut both mean the read timeout expired.
            Err(_) => break,
        }
    }

    steps
}
