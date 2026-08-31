//! The attach client: a thin pump between the user's terminal and the
//! server. Raw stdin bytes go up the socket; rendered output frames come
//! back down and go straight to stdout. All key handling, state, and
//! rendering live in the server.

use std::io::{self, Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, RawFd};

use crossterm::{
    cursor::{Hide, Show},
    execute, terminal,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use nix::errno::Errno;
use nix::fcntl::{self, OFlag};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd;

use crate::Result;
use crate::protocol::{
    self, C2S_ATTACH, C2S_INPUT, C2S_LIST, C2S_RESIZE, FrameReader, S2C_BYE, S2C_LIST, S2C_OUTPUT,
    frame,
};

/// Cap on unwritten host-terminal output. Past this we stop reading the
/// socket so the server's own buffer applies back-pressure; stdin is
/// still drained so mouse tracking cannot deadlock the pty. Latest-frame
/// wins, so this is a safety net rather than a typical size.
const MAX_STDOUT_BUF: usize = 4 * 1024 * 1024;

/// Print the server's session listing and exit.
pub fn list() -> Result<()> {
    let mut stream = protocol::connect()?;
    stream.write_all(&frame(C2S_LIST, &[]))?;

    let mut reader = FrameReader::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Err("server closed the connection without a listing".into());
        }
        reader.extend(&buf[..n]);
        while let Some((kind, payload)) = reader.next_frame().map_err(io::Error::other)? {
            if kind == S2C_LIST {
                let text = String::from_utf8_lossy(&payload);
                // Colors for humans; plain text when piped.
                if std::io::IsTerminal::is_terminal(&io::stdout()) {
                    print!("{text}");
                } else {
                    print!("{}", strip_ansi(&text));
                }
                return Ok(());
            }
        }
    }
}

/// Drop CSI escape sequences (colors, attributes) from server output.
fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for c2 in chars.by_ref() {
                if c2.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

pub fn run(name: &str) -> Result<()> {
    let mut stream = protocol::connect()?;

    let mut size = match terminal::size() {
        Ok((0, _)) | Ok((_, 0)) | Err(_) => (80, 24),
        Ok(s) => s,
    };

    // Attach first so the server's initial frame is already in flight
    // when we enter the alternate screen.
    let mut payload = Vec::with_capacity(4 + name.len());
    payload.extend_from_slice(&size.0.to_le_bytes());
    payload.extend_from_slice(&size.1.to_le_bytes());
    payload.extend_from_slice(name.as_bytes());
    stream.write_all(&frame(C2S_ATTACH, &payload))?;

    let guard = ScreenGuard::enter()?;

    // Non-blocking IO so a full host-pty output buffer cannot stop us
    // reading stdin. Mouse tracking (1002) writes a drag event per cell;
    // the server answers each burst with a full-screen frame. Blocking
    // on stdout.write while the terminal blocks on stdin.write is the
    // classic pty deadlock — the screen freezes mid-drag, especially
    // with a synchronized-update (2026) frame stuck half-written.
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    stream.set_nonblocking(true)?;
    let stdin_nb = FdFlagsGuard::set_nonblock(stdin.as_fd())?;
    let stdout_nb = FdFlagsGuard::set_nonblock(stdout.as_fd())?;

    let mut stdin_buf = [0u8; 65536];
    let mut sock_buf = [0u8; 65536];
    let mut sock_out = Vec::new();
    let mut host_out = HostOut::default();
    let mut mouse_tail = Vec::new();
    let mut held_stdin = Vec::new();
    let mut reader = FrameReader::new();
    let mut reason = String::from("connection closed by server");

    'outer: loop {
        let (stdin_ready, sock_readable) = {
            // Don't watch POLLIN when the host terminal is behind: the
            // socket would stay readable and we'd spin. Empty events
            // still report HUP/ERR.
            let mut sock_interest = PollFlags::empty();
            if host_out.pending() < MAX_STDOUT_BUF {
                sock_interest |= PollFlags::POLLIN;
            }
            if !sock_out.is_empty() {
                sock_interest |= PollFlags::POLLOUT;
            }
            let mut fds = vec![
                PollFd::new(stdin.as_fd(), PollFlags::POLLIN),
                PollFd::new(stream.as_fd(), sock_interest),
            ];
            if !host_out.is_empty() {
                fds.push(PollFd::new(stdout.as_fd(), PollFlags::POLLOUT));
            }
            poll(&mut fds, PollTimeout::from(100u16))?;
            let sock_rev = fds[1].revents().unwrap_or(PollFlags::empty());
            (
                fds[0].any().unwrap_or(false),
                sock_rev.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR),
            )
        };

        if stdin_ready {
            loop {
                match unistd::read(stdin.as_fd(), &mut stdin_buf) {
                    Ok(0) => {
                        reason = "detached (stdin closed)".to_string();
                        break 'outer;
                    }
                    Ok(len) => {
                        if host_out.is_empty() {
                            sock_out.extend_from_slice(&frame(C2S_INPUT, &stdin_buf[..len]));
                        } else {
                            // Host is behind: hold and coalesce into one
                            // input frame after the read loop.
                            held_stdin.extend_from_slice(&stdin_buf[..len]);
                        }
                    }
                    Err(Errno::EINTR) => continue,
                    Err(Errno::EAGAIN) => break,
                    Err(e) => return Err(e.into()),
                }
            }
        }
        if !held_stdin.is_empty() {
            let compacted = crate::input::compact_mouse_input(&held_stdin, &mut mouse_tail);
            held_stdin.clear();
            if !compacted.is_empty() {
                sock_out.extend_from_slice(&frame(C2S_INPUT, &compacted));
            }
        }

        if sock_readable && host_out.pending() < MAX_STDOUT_BUF {
            loop {
                match stream.read(&mut sock_buf) {
                    Ok(0) => break 'outer,
                    Ok(n) => reader.extend(&sock_buf[..n]),
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(_) => break 'outer,
                }
            }
            while let Some((kind, payload)) = reader.next_frame().map_err(io::Error::other)? {
                match kind {
                    S2C_OUTPUT => host_out.push(&payload),
                    S2C_BYE => {
                        reason = String::from_utf8_lossy(&payload).into_owned();
                        break 'outer;
                    }
                    _ => {}
                }
            }
        }

        // Propagate terminal resizes (checked per tick; SIGWINCH-free).
        if let Ok(now) = terminal::size()
            && now != size
            && now.0 > 0
            && now.1 > 0
        {
            size = now;
            let mut payload = Vec::with_capacity(4);
            payload.extend_from_slice(&size.0.to_le_bytes());
            payload.extend_from_slice(&size.1.to_le_bytes());
            sock_out.extend_from_slice(&frame(C2S_RESIZE, &payload));
        }

        // Always try; EAGAIN is fine. Poll's POLLOUT is only so we wake
        // once a previously-full buffer drains — a burst this tick still
        // has to go out before we wait again.
        if !flush_stream(&mut stream, &mut sock_out)? {
            break;
        }
        match flush_host(stdout.as_fd(), &mut host_out) {
            Ok(true) => {}
            Ok(false) => {
                reason = "detached (stdout closed)".to_string();
                break;
            }
            Err(e) => return Err(e.into()),
        }
    }

    drop(stdin_nb);
    drop(stdout_nb);
    let leftover = host_out.into_bytes();
    let _ = stdout.write_all(&leftover);
    let _ = stdout.flush();
    drop(guard);
    println!("[rmux] {reason}");
    Ok(())
}

/// Restore fd flags on drop so a panic doesn't leave stdin/stdout
/// non-blocking for the user's shell.
struct FdFlagsGuard {
    fd: RawFd,
    old: OFlag,
}

impl FdFlagsGuard {
    fn set_nonblock(fd: BorrowedFd<'_>) -> io::Result<Self> {
        let raw = fd.as_raw_fd();
        let bits = fcntl::fcntl(fd, fcntl::F_GETFL)?;
        let old = OFlag::from_bits_retain(bits);
        fcntl::fcntl(fd, fcntl::F_SETFL(old | OFlag::O_NONBLOCK))?;
        Ok(Self { fd: raw, old })
    }
}

impl Drop for FdFlagsGuard {
    fn drop(&mut self) {
        let fd = unsafe { BorrowedFd::borrow_raw(self.fd) };
        let _ = fcntl::fcntl(fd, fcntl::F_SETFL(self.old));
    }
}

/// Write as much of `buf` as the fd will take. `Ok(false)` means the
/// peer went away.
fn flush_stream(stream: &mut impl Write, buf: &mut Vec<u8>) -> io::Result<bool> {
    while !buf.is_empty() {
        match stream.write(buf) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                buf.drain(..n);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(true),
            Err(_) => return Ok(false),
        }
    }
    Ok(true)
}

/// Host-terminal output: at most one in-flight payload (must be finished
/// so a 2026h is never stranded), one replaceable render frame, and one
/// replaceable OSC 52. Spam-select then only paints/copies the latest.
#[derive(Default)]
struct HostOut {
    writing: Vec<u8>,
    off: usize,
    frame: Option<Vec<u8>>,
    clipboard: Option<Vec<u8>>,
}

impl HostOut {
    fn is_empty(&self) -> bool {
        self.off >= self.writing.len() && self.frame.is_none() && self.clipboard.is_none()
    }

    fn pending(&self) -> usize {
        self.writing.len().saturating_sub(self.off)
            + self.frame.as_ref().map_or(0, Vec::len)
            + self.clipboard.as_ref().map_or(0, Vec::len)
    }

    fn push(&mut self, payload: &[u8]) {
        if payload.starts_with(b"\x1b]52;") {
            self.clipboard = Some(payload.to_vec());
        } else {
            self.frame = Some(payload.to_vec());
        }
    }

    fn writing_slice(&self) -> Option<&[u8]> {
        let rest = &self.writing[self.off..];
        if rest.is_empty() {
            None
        } else {
            Some(rest)
        }
    }

    fn advance(&mut self, n: usize) {
        self.off += n;
        if self.off >= self.writing.len() {
            self.writing.clear();
            self.off = 0;
        }
    }

    fn promote(&mut self) -> bool {
        if self.off < self.writing.len() {
            return true;
        }
        self.writing.clear();
        self.off = 0;
        if let Some(frame) = self.frame.take() {
            self.writing = frame;
            return true;
        }
        if let Some(clip) = self.clipboard.take() {
            self.writing = clip;
            return true;
        }
        false
    }

    fn into_bytes(mut self) -> Vec<u8> {
        let mut out = self.writing.split_off(self.off.min(self.writing.len()));
        if let Some(frame) = self.frame.take() {
            out.extend_from_slice(&frame);
        }
        if let Some(clip) = self.clipboard.take() {
            out.extend_from_slice(&clip);
        }
        out
    }
}

fn flush_host(fd: BorrowedFd<'_>, out: &mut HostOut) -> io::Result<bool> {
    loop {
        if out.writing_slice().is_none() {
            if !out.promote() {
                return Ok(true);
            }
        }
        let n = {
            let Some(chunk) = out.writing_slice() else {
                return Ok(true);
            };
            match unistd::write(fd, chunk) {
                Ok(0) => return Ok(false),
                Ok(n) => n,
                Err(Errno::EINTR) => continue,
                Err(Errno::EAGAIN) => return Ok(true),
                Err(e) => return Err(e.into()),
            }
        };
        out.advance(n);
    }
}

/// Puts the terminal into raw mode + alternate screen, and restores it
/// on drop so a panic doesn't leave the user's terminal broken.
struct ScreenGuard;

impl ScreenGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen, Hide)?;
        // Button-event mouse tracking + SGR encoding: wheel (and, for
        // selection, press/drag) events arrive on stdin and are handled
        // server-side.
        let mut out = io::stdout();
        out.write_all(b"\x1b[?1002h\x1b[?1006h")?;
        out.flush()?;
        Ok(Self)
    }
}

impl Drop for ScreenGuard {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = out.write_all(b"\x1b[?1006l\x1b[?1002l");
        let _ = out.flush();
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}
