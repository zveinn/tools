//! Minimal pty handling: forkpty a shell, non-blocking reads into the
//! terminal, best-effort writes, and TIOCSWINSZ on resize.

use std::{
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::PathBuf,
    process::Command,
};

use libghostty_vt::Terminal;

use crate::config::Config;
use nix::{
    errno::Errno,
    fcntl::{self, OFlag},
    pty::ForkptyResult,
    unistd,
};

pub struct Pty {
    fd: OwnedFd,
    /// The shell's pid, for reading /proc/<pid>/cwd when saving state.
    child: unistd::Pid,
}

#[derive(Debug)]
pub enum PtyError {
    EndOfStream,
    ReadFailed,
}

impl Pty {
    /// Fork a shell on a new pty, returning the master fd handle. The
    /// shell and its environment come from the config; `cwd` (used when
    /// restoring saved state) overrides the configured start_dir.
    pub fn spawn(
        winsize: nix::pty::Winsize,
        config: &Config,
        cwd: Option<&str>,
    ) -> std::io::Result<Self> {
        match unsafe { nix::pty::forkpty(&winsize, None)? } {
            ForkptyResult::Child => {
                // The server ignores SIGCHLD (auto-reaping) and the Rust
                // runtime ignores SIGPIPE; SIG_IGN survives fork *and*
                // exec, so without a reset every program in the pane
                // inherits broken wait()/pipe semantics (exit statuses
                // get auto-reaped away, pipelines error instead of
                // terminating). Hand children pristine dispositions.
                unsafe {
                    use nix::sys::signal::{SigHandler, Signal, signal};
                    let _ = signal(Signal::SIGCHLD, SigHandler::SigDfl);
                    let _ = signal(Signal::SIGPIPE, SigHandler::SigDfl);
                }
                // Prefer the configured shell, then $SHELL, then the
                // passwd entry, then /bin/sh.
                let shell = match &config.shell {
                    Some(s) => PathBuf::from(s),
                    None => match std::env::var_os("SHELL") {
                        Some(s) if !s.is_empty() => PathBuf::from(s),
                        _ => match unistd::User::from_uid(unistd::getuid()) {
                            Ok(Some(user)) => user.shell,
                            _ => PathBuf::from("/bin/sh"),
                        },
                    },
                };
                // Start in the restored cwd, then the configured
                // start_dir, then the home directory (never the
                // server's own cwd).
                let home = || -> Option<PathBuf> {
                    match std::env::var_os("HOME") {
                        Some(h) if !h.is_empty() => Some(PathBuf::from(h)),
                        _ => unistd::User::from_uid(unistd::getuid())
                            .ok()
                            .flatten()
                            .map(|user| user.dir),
                    }
                };
                let candidates = [
                    cwd.map(PathBuf::from),
                    config.start_dir.as_ref().map(PathBuf::from),
                    home(),
                ];
                for dir in candidates.into_iter().flatten() {
                    if std::env::set_current_dir(dir).is_ok() {
                        break;
                    }
                }
                let arg0 = shell.file_name().unwrap_or(shell.as_os_str());
                let mut cmd = Command::new(&shell);
                for (key, value) in &config.envs {
                    cmd.env(key, value);
                }
                let _ = cmd.arg0(arg0).exec();
                std::process::exit(127); // exec only returns on error
            }
            ForkptyResult::Parent { master: fd, child } => {
                // Non-blocking so a drain loop can't stall the server.
                let raw = fcntl::fcntl(&fd, fcntl::F_GETFL)?;
                let flags = OFlag::from_bits_retain(raw) | OFlag::O_NONBLOCK;
                fcntl::fcntl(&fd, fcntl::F_SETFL(flags))?;
                Ok(Self { fd, child })
            }
        }
    }

    /// The shell's current working directory, best-effort (None once
    /// the shell has exited).
    pub fn cwd(&self) -> Option<String> {
        std::fs::read_link(format!("/proc/{}/cwd", self.child.as_raw()))
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Drain available pty output into the terminal's VT parser.
    pub fn read(&self, term: &mut Terminal) -> Result<(), PtyError> {
        let mut buf = [0u8; 4096];
        loop {
            match unistd::read(&self.fd, &mut buf) {
                Ok(0) => return Err(PtyError::EndOfStream),
                Ok(len) => term.vt_write(&buf[..len]),
                Err(Errno::EAGAIN) => return Ok(()),
                Err(Errno::EINTR) => continue,
                // Linux reports the slave side closing as EIO.
                Err(Errno::EIO) => return Err(PtyError::EndOfStream),
                Err(_) => return Err(PtyError::ReadFailed),
            }
        }
    }

    /// Best-effort write; drops data under back-pressure like most
    /// terminal emulators do.
    pub fn write(&self, data: &[u8]) {
        let mut remaining = data;
        while !remaining.is_empty() {
            match unistd::write(&self.fd, remaining) {
                Ok(len) => remaining = &remaining[len..],
                Err(Errno::EINTR) => continue,
                Err(_) => break,
            }
        }
    }

    pub fn resize(&self, winsize: nix::pty::Winsize) {
        nix::ioctl_write_ptr_bad!(tiocswinsz, nix::libc::TIOCSWINSZ, nix::pty::Winsize);
        let _ = unsafe { tiocswinsz(self.fd.as_raw_fd(), &winsize) };
    }
}

impl AsFd for Pty {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}
