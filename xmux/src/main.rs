//! A minimal terminal multiplexer built on `libghostty-vt` that runs
//! inside your existing terminal (no display server needed).
//!
//! It follows the tmux client/server model: a long-lived server (run it
//! under systemd) owns all sessions → tabs → panes, parsing pty output
//! through libghostty's VT engine; thin clients attach to a session by
//! name over a Unix socket, so sessions survive SSH disconnects. At most
//! one client per session — a new attach kicks the old client.
//!
//!   xmux server        run the server (foreground)
//!   xmux a <name>      attach to session <name>, creating it if new
//!
//! Inside a client (defaults; rebindable in the config): Ctrl+O opens
//! the session manager, Ctrl+N the tab manager, Ctrl+K/L split the
//! focused pane, Ctrl+Q/W/E/R move pane focus, Ctrl+T cycles it, and
//! Ctrl+G detaches.

mod agent;
mod client;
mod config;
mod input;
mod model;
mod protocol;
mod pty;
mod render;
mod server;
mod state;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // --config <dir> / --config=<dir>: custom directory for config.yaml
    // and layout.json. Accepted anywhere in the argument list.
    let mut i = 0;
    while i < args.len() {
        if let Some(dir) = args[i].strip_prefix("--config=").map(str::to_string) {
            config::set_dir(dir.into());
            args.remove(i);
        } else if args[i] == "--config" {
            if i + 1 >= args.len() {
                eprintln!("xmux: --config needs a directory");
                std::process::exit(2);
            }
            config::set_dir(args.remove(i + 1).into());
            args.remove(i);
        } else {
            i += 1;
        }
    }

    let result = match args.first().map(String::as_str) {
        Some("server") => server::run(),
        Some("a" | "attach") => match args.get(1) {
            Some(name) => client::run(name),
            None => Err("usage: xmux a[ttach] <session-name>".into()),
        },
        Some("list" | "ls") => client::list(),
        Some("agent") => agent::run(&args[1..]),
        _ => {
            eprintln!(
                "usage: xmux [--config <dir>] server | xmux a[ttach] <session-name> | xmux list\n\
                        xmux agent new|kill|send|read ...  (xmux agent for details)"
            );
            std::process::exit(2);
        }
    };
    // One clean line on stderr (journalctl shows it) instead of Rust's
    // escaped Debug formatting.
    if let Err(e) = result {
        eprintln!("xmux: {e}");
        std::process::exit(1);
    }
}
