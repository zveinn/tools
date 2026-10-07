//! Single-instance coordination over a Unix socket.
//!
//! Pressing the launcher's own binding again while it is up has to close it,
//! and that key never reaches us: a compositor keybinding outranks an exclusive
//! layer-shell keyboard grab, so cosmic-comp consumes the combination and runs
//! the bound command a second time instead of delivering the key. Whatever we
//! do about it therefore has to be driven by process rather than by keystroke.
//!
//! So the first invocation binds this socket and draws the launcher, and every
//! later one connects, says "dismiss", and exits. That is also what keeps two
//! launchers from stacking on top of each other, which is the failure a plain
//! "spawn it again" gets you.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// What a later invocation asks the running launcher to do.
const DISMISS: u8 = b'q';

/// Whether this process should draw the launcher or has already handed its
/// message to the one that is.
pub enum Role {
    /// This process owns the launcher; keep `Primary` alive for its lifetime.
    Primary(Primary),
    /// A launcher was already running, and has been told about this press.
    Secondary,
}

/// The listening socket, which unlinks itself on drop.
///
/// Holds the guard by value rather than implementing `Drop` itself so that
/// [`Primary::into_parts`] can still move the socket out, while dropping a
/// `Primary` on an early return path — no applications found, or a failure
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

    dir.join(format!("xla-{display}.sock"))
}

/// Becomes the launcher, or tells the one already running about this press.
///
/// With `toggle` off the second invocation still stops here rather than opening
/// a second launcher; it just leaves the first one alone instead of dismissing
/// it.
pub fn claim(toggle: bool) -> std::io::Result<Role> {
    let path = socket_path();

    if let Some(role) = try_send(&path, toggle) {
        return Ok(role);
    }

    // Nothing answered, so any socket file present is stale: a previous
    // launcher was killed before its guard could unlink it. Bind fails with
    // AddrInUse until it is gone.
    let _ = std::fs::remove_file(&path);

    match UnixListener::bind(&path) {
        Ok(listener) => {
            listener.set_nonblocking(true)?;
            Ok(Role::Primary(Primary { listener, guard: PathGuard(path) }))
        }
        // Lost a race with another instance that bound between our failed
        // connect and this bind; it is the launcher, so defer to it.
        Err(err) if err.kind() == ErrorKind::AddrInUse => {
            Ok(try_send(&path, toggle).unwrap_or(Role::Secondary))
        }
        Err(err) => Err(err),
    }
}

/// Connects to a listening launcher, if one answers.
fn try_send(path: &Path, toggle: bool) -> Option<Role> {
    let mut stream = UnixStream::connect(path).ok()?;
    if toggle {
        // A failed write means the peer died between connect and write; treat
        // it as delivered anyway rather than opening a second launcher on top
        // of a possibly still-mapped one.
        let _ = stream.write_all(&[DISMISS]);
        let _ = stream.flush();
    }
    Some(Role::Secondary)
}

/// Whether a connecting instance asked the launcher to close.
pub fn asks_to_dismiss(stream: &mut UnixStream) -> bool {
    // `accept` returns a blocking socket even from a non-blocking listener, so
    // without a timeout a peer that connects and never writes would stall the
    // event loop while the launcher is on screen holding the keyboard.
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(50)));

    let mut buffer = [0u8; 16];
    match stream.read(&mut buffer) {
        Ok(count) => buffer[..count].contains(&DISMISS),
        Err(_) => false,
    }
}
