use std::io::{self, Write};
use std::process::{Command, Stdio};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

/// Copy text to the *local* clipboard.
///
/// OSC 52 is the path that works over SSH: the sequence is interpreted by the
/// terminal on your laptop, not by the remote box. tmux/screen get a DCS wrap
/// so they forward it. Local `wl-copy` / `xclip` / `pbcopy` are only used when
/// we are not in an SSH session (those tools would otherwise copy on the server).
pub fn copy_text(text: &str) -> io::Result<()> {
    osc52(text)?;
    if !is_ssh() {
        let _ = copy_local_tool(text);
    }
    Ok(())
}

fn osc52(text: &str) -> io::Result<()> {
    let encoded = STANDARD.encode(text.as_bytes());
    let mut out = io::stdout();
    if std::env::var_os("TMUX").is_some() {
        write!(out, "\x1bPtmux;\x1b\x1b]52;c;{encoded}\x07\x1b\\")?;
    } else if std::env::var_os("STY").is_some() {
        write!(out, "\x1bP\x1b]52;c;{encoded}\x07\x1b\\")?;
    } else {
        write!(out, "\x1b]52;c;{encoded}\x07")?;
    }
    out.flush()
}

fn is_ssh() -> bool {
    std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some()
}

fn copy_local_tool(text: &str) -> io::Result<()> {
    let candidates: &[&[&str]] = &[
        &["wl-copy"],
        &["xclip", "-selection", "clipboard"],
        &["pbcopy"],
    ];
    for cmd in candidates {
        if pipe(cmd, text).is_ok() {
            return Ok(());
        }
    }
    Err(io::Error::other("no local clipboard tool"))
}

fn pipe(cmd: &[&str], text: &str) -> io::Result<()> {
    let mut child = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    }
    let status = child.wait()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("clipboard command failed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_encodes() {
        let encoded = STANDARD.encode("https://github.com/acme/box/pull/1");
        assert!(!encoded.contains("https"));
        assert!(!encoded.is_empty());
    }
}
