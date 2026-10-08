//! xla - an application launcher for the COSMIC desktop.
//!
//! Bind it to a key combination: a centered panel appears with a query row
//! above a list of installed applications, typing filters the list, and Enter
//! launches the highlighted one.
//!
//! It is xsw's panel with a query row on top, and shares that tool's structure
//! and its configuration style.

mod apps;
mod config;
mod history;
mod icons;
mod ipc;
mod outputs;
mod render;
mod search;
mod ui;

use std::io::ErrorKind;
use std::time::{Duration, Instant};

use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, Mode as IoMode, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::Shm;
use wayland_client::globals::registry_queue_init;
use wayland_client::{Connection, EventQueue};

use config::{Config, Display, Mode};
use ipc::Role;
use outputs::PrimaryFinder;
use ui::Launcher;

/// How long to wait for the compositor to finish listing its displays.
///
/// Only reached with `display: primary`, which is the one setting that needs an
/// answer before the surface can be mapped. The wait ends as soon as the list
/// is done, normally within a frame or two; the cap only applies if it never
/// is, and appearing on the active display beats not appearing.
const OUTPUT_TIMEOUT: Duration = Duration::from_millis(200);

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xla: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (config, mode, warnings) = Config::load(std::env::args().skip(1))?;
    // A bad config file is reported but not fatal; see config.rs for why.
    for warning in &warnings {
        eprintln!("xla: {warning}");
    }

    match mode {
        Mode::Help => {
            print!("{}", config::USAGE);
            return Ok(());
        }
        Mode::Version => {
            println!("xla {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Mode::DumpConfig => {
            println!("# effective configuration");
            println!("# file: {}", config.config_path.display());
            print!("{}", config.to_yaml());
            return Ok(());
        }
        // Listing what was found needs no compositor at all, which makes it
        // the one thing here that works over ssh.
        Mode::List => {
            for app in apps::load(&config) {
                println!("{}\t{}\t{}", app.id, app.name, app.description);
            }
            return Ok(());
        }
        Mode::Show => {}
    }

    // Claim the launcher before touching Wayland: if one is already up, this
    // process only has to tell it to close and exit.
    let primary = match ipc::claim(config.toggle)? {
        Role::Secondary => return Ok(()),
        Role::Primary(primary) => primary,
    };

    let conn = Connection::connect_to_env()
        .map_err(|err| format!("cannot connect to a Wayland compositor: {err}"))?;
    let (globals, mut queue) = registry_queue_init::<Launcher>(&conn)?;
    let qh = queue.handle();

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|err| format!("wl_compositor unavailable: {err}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|err| format!("wlr-layer-shell unavailable: {err}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|err| format!("wl_shm unavailable: {err}"))?;

    // Bound only when the primary display is actually wanted, so the other
    // settings cost no globals, no events and no extra roundtrip.
    let primary_finder = if config.display == Display::Primary {
        let finder = PrimaryFinder::bind(&globals, &qh);
        if !finder.is_available() {
            eprintln!(
                "xla: this compositor does not report a primary display; using the active one"
            );
        }
        Some(finder)
    } else {
        None
    };

    // Push the requests made so far out to the compositor before reading the
    // desktop files, so it is preparing its replies while we do that rather
    // than after. The scan is a few milliseconds of small reads, which is
    // roughly what the roundtrip below costs anyway.
    let _ = conn.flush();
    let apps = apps::load(&config);
    if apps.is_empty() {
        return Err("no applications found under XDG_DATA_DIRS".into());
    }

    // Created before the launcher, which needs a handle to it: key repeat is a
    // calloop timer, so the keyboard cannot be set up without one.
    let mut event_loop: EventLoop<Launcher> = EventLoop::try_new()?;
    let handle = event_loop.handle();

    let max_lifetime = config.max_lifetime;
    let mut launcher = Launcher::new(&globals, &qh, handle.clone(), shm, config, apps)?;

    queue.roundtrip(&mut launcher)?;
    let primary_name = wait_for_outputs(&mut queue, &mut launcher, primary_finder.as_ref())?;
    launcher.present(&qh, &compositor, &layer_shell, primary_name.as_deref());

    // Both the Wayland connection and the IPC socket have to be watched at
    // once, which is what calloop is for.
    WaylandSource::new(conn.clone(), queue).insert(handle.clone())?;

    // Keep the guard alive for the process lifetime so the socket file is
    // unlinked on the way out; the listener itself moves into the event loop.
    let _guard = {
        let (listener, guard) = primary.into_parts();
        let source = Generic::new(listener, Interest::READ, IoMode::Level);
        let inserted =
            handle.insert_source(source, move |_readiness, listener, launcher: &mut Launcher| {
                loop {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            if ipc::asks_to_dismiss(&mut stream) {
                                launcher.dismiss();
                            }
                        }
                        // Level-triggered, so drain until the queue is empty.
                        Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                        Err(_) => break,
                    }
                }
                Ok(PostAction::Continue)
            });
        if let Err(err) = inserted {
            eprintln!("xla: cannot watch the launcher socket: {err}");
        }
        guard
    };

    // Safety net rather than a feature: an exclusive keyboard grab that never
    // ends would leave the session unable to type. Reached only if the
    // launcher is left open and never dismissed.
    let deadline = Instant::now() + max_lifetime;
    while !launcher.exit {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        event_loop.dispatch(Some(remaining), &mut launcher)?;
    }

    launcher.finish(&conn);
    Ok(())
}

/// Waits for display enumeration, returning the primary output's name.
///
/// `None` either because no primary display was asked for, or because the
/// compositor does not report one; both mean "let the compositor choose".
fn wait_for_outputs(
    queue: &mut EventQueue<Launcher>,
    launcher: &mut Launcher,
    finder: Option<&PrimaryFinder>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Some(finder) = finder else { return Ok(None) };
    let deadline = Instant::now() + OUTPUT_TIMEOUT;

    while !finder.is_done() {
        if Instant::now() >= deadline {
            eprintln!("xla: the compositor did not finish listing displays; using the active one");
            break;
        }
        queue.roundtrip(launcher)?;
        std::thread::sleep(Duration::from_millis(2));
    }

    Ok(finder.primary_name())
}
