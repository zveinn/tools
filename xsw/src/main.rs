//! xsw - a window switcher for the COSMIC desktop.
//!
//! Bind it to a key combination and hold that combination: each press cycles a
//! centered list of open windows, and releasing the modifier focuses the
//! highlighted one.

mod config;
mod icons;
mod ipc;
mod mru;
mod outputs;
mod render;
mod toplevels;
mod ui;

use std::io::ErrorKind;
use std::time::{Duration, Instant};

use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::foreign_toplevel_list::ForeignToplevelList;
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{EventLoop, Interest, Mode as IoMode, PostAction};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::shell::wlr_layer::LayerShell;
use smithay_client_toolkit::shm::Shm;
use wayland_client::globals::registry_queue_init;
use wayland_client::{Connection, EventQueue};

use config::{Config, Display, Mode};
use ipc::{Role, Step};
use outputs::PrimaryFinder;
use toplevels::CosmicToplevels;
use ui::App;

/// How long to wait for the compositor to report which window is focused.
///
/// cosmic-comp emits toplevel state on its own refresh cycle rather than in
/// reply to our request, so there is nothing to synchronise on but time. This
/// wait has to happen before the overlay is mapped, because taking an
/// exclusive keyboard grab makes cosmic-comp deactivate the focused toplevel:
/// once we are on screen, nothing is activated and there is no longer any way
/// to ask what the user was just looking at. The wait ends as soon as the state
/// lands, normally within a frame; the cap only applies if it never does.
const STATE_TIMEOUT: Duration = Duration::from_millis(250);

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("xsw: {err}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let (config, mode, warnings) = Config::load(std::env::args().skip(1))?;
    // A bad config file is reported but not fatal; see config.rs for why.
    for warning in &warnings {
        eprintln!("xsw: {warning}");
    }

    match mode {
        Mode::Help => {
            print!("{}", config::USAGE);
            return Ok(());
        }
        Mode::Version => {
            println!("xsw {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Mode::DumpConfig => {
            println!("# effective configuration");
            println!("# file: {}", config.config_path.display());
            print!("{}", config.to_yaml());
            return Ok(());
        }
        Mode::Show | Mode::List => {}
    }

    let step = if config.reverse { Step::Prev } else { Step::Next };

    // Claim the switcher before touching Wayland: if one is already up, this
    // process only has to forward a keystroke and exit, and doing that in a
    // couple of syscalls keeps repeated presses of the binding responsive.
    let primary = if mode == Mode::Show {
        match ipc::claim(step)? {
            Role::Secondary => return Ok(()),
            Role::Primary(primary) => Some(primary),
        }
    } else {
        None
    };

    let conn = Connection::connect_to_env()
        .map_err(|err| format!("cannot connect to a Wayland compositor: {err}"))?;
    let (globals, mut queue) = registry_queue_init::<App>(&conn)?;
    let qh = queue.handle();

    let foreign = ForeignToplevelList::new(&globals, &qh);
    let (cosmic, bind_errors) = CosmicToplevels::bind(&globals, &qh);
    if !cosmic.is_usable() {
        for err in &bind_errors {
            eprintln!("xsw: {err}");
        }
        return Err("this compositor does not implement COSMIC's toplevel protocols; \
                    xsw requires cosmic-comp"
            .into());
    }

    let compositor = CompositorState::bind(&globals, &qh)
        .map_err(|err| format!("wl_compositor unavailable: {err}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|err| format!("wlr-layer-shell unavailable: {err}"))?;
    let shm = Shm::bind(&globals, &qh).map_err(|err| format!("wl_shm unavailable: {err}"))?;

    // Bound only when the primary display is actually wanted, so the default
    // costs no globals, no events and no extra roundtrip.
    let primary_finder = match config.display {
        Display::Primary => {
            let finder = PrimaryFinder::bind(&globals, &qh);
            if !finder.is_available() {
                eprintln!(
                    "xsw: this compositor does not report a primary display; \
                     using the active one"
                );
            }
            Some(finder)
        }
        Display::Active | Display::Named(_) => None,
    };

    let max_lifetime = config.max_lifetime;
    let mut app = App::new(&globals, &qh, shm, foreign, cosmic, config)?;

    // Drain the toplevel announcements. The window list is delivered in reply
    // to these roundtrips; per-window state follows later, on the
    // compositor's own schedule.
    conn.roundtrip()?;
    queue.roundtrip(&mut app)?;
    let windows = wait_for_state(&mut queue, &mut app, primary_finder.as_ref())?;
    let primary_name = primary_finder.as_ref().and_then(PrimaryFinder::primary_name);

    if mode == Mode::List {
        for window in &windows {
            println!(
                "{}\t{}\t{}",
                window.app_id,
                window.title,
                match (window.activated, window.minimized) {
                    (true, _) => "active",
                    (_, true) => "minimized",
                    _ => "-",
                }
            );
        }
        return Ok(());
    }

    if windows.is_empty() {
        // Nothing to switch between; say nothing and exit quietly, since this
        // runs from a keybinding with no terminal attached.
        return Ok(());
    }

    app.present(&qh, &compositor, &layer_shell, windows, primary_name.as_deref());

    // Both the Wayland connection and the IPC socket have to be watched at
    // once, which is what calloop is for.
    let mut event_loop: EventLoop<App> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    WaylandSource::new(conn.clone(), queue).insert(handle.clone())?;

    // Keep the guard alive for the process lifetime so the socket file is
    // unlinked on the way out; the listener itself moves into the event loop.
    let _guard = primary.map(|primary| {
        let (listener, guard) = primary.into_parts();
        let qh = qh.clone();
        let source = Generic::new(listener, Interest::READ, IoMode::Level);
        let inserted = handle.insert_source(source, move |_readiness, listener, app: &mut App| {
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let steps = ipc::read_steps(&mut stream);
                        app.apply_steps(&steps, &qh);
                    }
                    // Level-triggered, so drain until the queue is empty.
                    Err(err) if err.kind() == ErrorKind::WouldBlock => break,
                    Err(_) => break,
                }
            }
            Ok(PostAction::Continue)
        });
        if let Err(err) = inserted {
            eprintln!("xsw: cannot watch the switcher socket: {err}");
        }
        guard
    });

    // Safety net rather than a feature: if the compositor never reports the
    // modifier being released and the user never presses Enter or Escape, an
    // exclusive grab would leave the session unable to type.
    let deadline = Instant::now() + max_lifetime;
    while !app.exit {
        let now = Instant::now();
        let remaining = deadline.saturating_duration_since(now);
        if remaining.is_zero() {
            break;
        }
        // Wake for whichever comes first: the debounce that decides when the
        // list is drawn, a periodic tick, or the overall lifetime cap.
        let mut timeout = remaining.min(Duration::from_millis(250));
        if let Some(next) = app.next_deadline() {
            timeout = timeout.min(
                next.saturating_duration_since(now).max(Duration::from_millis(1)),
            );
        }
        event_loop.dispatch(Some(timeout), &mut app)?;

        app.on_timer(&qh);
        app.tick(&qh);
    }

    app.finish(&conn);
    Ok(())
}

/// Waits for per-window state to arrive, returning the list either way.
///
/// Also waits for output enumeration when a primary display was asked for,
/// since both answers are needed before the surface can be mapped and they
/// arrive over the same roundtrips.
fn wait_for_state(
    queue: &mut EventQueue<App>,
    app: &mut App,
    primary: Option<&PrimaryFinder>,
) -> Result<Vec<toplevels::Window>, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + STATE_TIMEOUT;

    loop {
        let windows = app.snapshot();
        let outputs_ready = primary.is_none_or(PrimaryFinder::is_done);
        let windows_ready = windows.is_empty() || windows.iter().any(|w| w.activated);
        if (windows_ready && outputs_ready) || Instant::now() >= deadline {
            return Ok(windows);
        }
        queue.roundtrip(app)?;
        std::thread::sleep(Duration::from_millis(4));
    }
}
