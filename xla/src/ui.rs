//! The on-screen launcher: a centered layer-shell overlay driven by the
//! keyboard.
//!
//! Presented on the `overlay` layer with exclusive keyboard interactivity, so
//! it draws above everything and receives every keystroke, which is what a
//! query row needs. It is deliberately *not* anchored: layer-shell centers a
//! surface that sets a size without anchors, which is exactly the placement we
//! want and saves us computing it from output geometry.
//!
//! Same overlay as xsw, with two differences that follow from being a launcher
//! rather than a switcher. There is no held modifier to wait for or commit on,
//! so the panel is drawn as soon as it is configured; and the list changes as
//! the query does, so the surface is resized between keystrokes.

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, Keysym, KeyboardHandler, Modifiers, RawModifiers,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{delegate_registry, registry_handlers};
use tiny_skia::Pixmap;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

use crate::apps::{self, App};
use crate::config::{Config, Display};
use crate::history::History;
use crate::icons::IconCache;
use crate::render::{Renderer, Row};
use crate::search;

pub struct Launcher {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    /// Created by [`Launcher::present`], once the row count is known.
    layer: Option<LayerSurface>,
    /// Needed to create the keyboard, whose key repeat is a calloop timer.
    loop_handle: LoopHandle<'static, Self>,

    config: Config,
    renderer: Renderer,
    icons: IconCache,

    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// Last reported modifier state. `KeyEvent` does not carry it, and the
    /// difference between `w` and `Ctrl+W` is the difference between typing a
    /// letter and deleting a word.
    modifiers: Modifiers,

    apps: Vec<App>,
    history: History,
    /// Indices into `apps`, best match first.
    matches: Vec<usize>,
    query: String,
    /// Index into `matches`.
    selected: usize,
    /// First visible row, so the selection stays on screen when the list is
    /// longer than `max_rows`.
    scroll: usize,

    scale: u32,
    /// Height last asked for, in logical pixels. The width never changes.
    logical_height: u32,
    configured: bool,
    /// Index into `apps` to launch on the way out.
    launch: Option<usize>,
    pub exit: bool,
}

impl Launcher {
    /// Sets up the Wayland state without showing anything yet.
    ///
    /// The surface cannot be created here because its height depends on how
    /// many applications match, and mapping it needs the output the launcher
    /// belongs on, which is still being resolved at this point.
    pub fn new(
        globals: &wayland_client::globals::GlobalList,
        qh: &QueueHandle<Self>,
        loop_handle: LoopHandle<'static, Self>,
        shm: Shm,
        config: Config,
        apps: Vec<App>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Enough for a full-height panel; grown on configure if needed.
        let initial = (config.width * config.layout.height_for(config.max_rows) * 4) as usize;
        let pool = SlotPool::new(initial, &shm)?;
        let renderer = Renderer::new(&config);
        let icons = IconCache::new(&config.icon_theme, 1, config.layout.icon_size);
        let history = if config.history { History::load() } else { History::default() };
        let query = config.query.clone();

        let mut launcher = Self {
            registry_state: RegistryState::new(globals),
            seat_state: SeatState::new(globals, qh),
            output_state: OutputState::new(globals, qh),
            shm,
            pool,
            layer: None,
            loop_handle,
            config,
            renderer,
            icons,
            seat: None,
            keyboard: None,
            modifiers: Modifiers::default(),
            apps,
            history,
            matches: Vec::new(),
            query,
            selected: 0,
            scroll: 0,
            scale: 1,
            logical_height: 0,
            configured: false,
            launch: None,
            exit: false,
        };
        launcher.refresh_matches();
        Ok(launcher)
    }

    /// Maps the overlay.
    pub fn present(
        &mut self,
        qh: &QueueHandle<Self>,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        primary_name: Option<&str>,
    ) {
        self.logical_height = self.config.layout.height_for(self.visible_rows());

        let surface = compositor.create_surface(qh);
        // A `None` output lets the compositor choose, which gives the output
        // holding the focused window; naming one pins the launcher there.
        let output = self.target_output(primary_name);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("xla"),
            output.as_ref(),
        );
        // No anchor: the compositor centers a sized, unanchored layer surface.
        layer.set_size(self.config.width, self.logical_height);
        // Exclusive, not OnDemand: every keystroke has to reach the query row,
        // including the ones that are shortcuts elsewhere.
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.layer = Some(layer);
    }

    /// The output to map onto, or `None` to let the compositor decide.
    ///
    /// Falls back to `None` whenever the wanted output cannot be found — an
    /// unplugged monitor, a name that does not match, or a compositor that
    /// does not report the primary. Appearing on the active display is a much
    /// better outcome than not appearing at all.
    fn target_output(&self, primary_name: Option<&str>) -> Option<wl_output::WlOutput> {
        let wanted = match &self.config.display {
            Display::Active => return None,
            Display::Primary => primary_name?,
            Display::Named(name) => name.as_str(),
        };

        let found = self.output_state.outputs().find(|output| {
            self.output_state
                .info(output)
                .and_then(|info| info.name)
                .is_some_and(|name| name == wanted)
        });

        if found.is_none() {
            eprintln!("xla: no output named {wanted:?}; using the active display");
        }
        found
    }

    /// Closes the launcher, having been told to by a second invocation of xla.
    pub fn dismiss(&mut self) {
        self.launch = None;
        self.exit = true;
    }

    /// Runs the chosen application, if the user picked one.
    ///
    /// The overlay is torn down first and the compositor given a chance to see
    /// it: our exclusive keyboard grab would otherwise still be in the way when
    /// the new window asks for focus.
    pub fn finish(&mut self, conn: &Connection) {
        let Some(index) = self.launch else { return };
        let Some(app) = self.apps.get(index) else { return };

        self.layer = None;
        let _ = conn.roundtrip();

        // Resolved only when it is actually needed, since finding a terminal
        // means walking PATH.
        let terminal = if app.terminal {
            self.config.terminal.clone().or_else(apps::default_terminal)
        } else {
            None
        };

        match apps::launch(app, terminal.as_deref()) {
            Ok(()) => {
                if self.config.history {
                    self.history.promote(&app.id);
                    self.history.save();
                }
            }
            Err(err) => eprintln!("xla: {err}"),
        }
    }

    /// How many result rows the panel shows.
    ///
    /// Always at least one, because an empty result set still draws a line
    /// saying so; a panel that collapsed to just the query row would look like
    /// the launcher had lost its list.
    fn visible_rows(&self) -> usize {
        if self.matches.is_empty() { 1 } else { self.matches.len().min(self.config.max_rows) }
    }

    /// Re-runs the query and puts the selection back on the first result.
    fn refresh_matches(&mut self) {
        let history = self.config.history.then_some(&self.history);
        self.matches = search::filter(&self.apps, &self.query, history);
        self.selected = 0;
        self.scroll = 0;
    }

    /// Handles a query that has changed: new results, new panel height.
    fn on_query_changed(&mut self) {
        self.refresh_matches();
        self.relayout();
    }

    /// Resizes the surface to fit the current results, then redraws.
    ///
    /// The new size and the buffer for it go out in one commit, which is why
    /// `set_size` is not committed here: [`Launcher::draw`] does that.
    fn relayout(&mut self) {
        let height = self.config.layout.height_for(self.visible_rows());
        if height != self.logical_height {
            self.logical_height = height;
            let _ = self.pool.resize(self.buffer_bytes());
            if let Some(layer) = self.layer.as_ref() {
                layer.set_size(self.config.width, height);
                // Before the first configure there is no buffer to attach, so
                // the size change has to be committed on its own to get one.
                if !self.configured {
                    layer.commit();
                    return;
                }
            }
        }
        self.draw();
    }

    fn buffer_bytes(&self) -> usize {
        (self.config.width * self.scale * self.logical_height * self.scale * 4) as usize
    }

    fn draw(&mut self) {
        // Attaching a buffer before the first configure has been acknowledged
        // is a layer-shell protocol violation.
        if !self.configured || self.layer.is_none() {
            return;
        }
        let width = self.config.width * self.scale;
        let height = self.logical_height * self.scale;
        if width == 0 || height == 0 {
            return;
        }

        let visible = self.visible_rows();
        // Resolved in two passes: picking the rows only borrows `self.apps`,
        // while looking their icons up needs `self.icons` mutably, so the
        // first pass hands owned values to the second.
        let wanted: Vec<_> = self
            .matches
            .iter()
            .skip(self.scroll)
            .take(visible)
            .filter_map(|&index| self.apps.get(index))
            .map(|app| (app.name.clone(), app.description.clone(), app.icon.clone()))
            .collect();

        let resolved: Vec<(String, String, Option<Pixmap>)> = wanted
            .into_iter()
            .map(|(name, description, icon)| {
                let icon = icon.and_then(|icon| self.icons.get(&icon));
                (name, description, icon)
            })
            .collect();

        let rows: Vec<Row<'_>> = resolved
            .iter()
            .map(|(name, description, icon)| Row {
                name,
                description,
                icon: icon.as_ref(),
            })
            .collect();

        let Some(pixmap) = self.renderer.draw(
            &self.query,
            &rows,
            self.selected.saturating_sub(self.scroll),
            width,
            height,
            self.scale,
        ) else {
            return;
        };

        let stride = width as i32 * 4;
        let Ok((buffer, canvas)) =
            self.pool.create_buffer(width as i32, height as i32, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };

        // tiny-skia stores premultiplied RGBA bytes; Argb8888 is a 32-bit
        // little-endian ARGB word, i.e. BGRA in memory. Swap R and B.
        let (dst_pixels, _) = canvas.as_chunks_mut::<4>();
        let (src_pixels, _) = pixmap.data().as_chunks::<4>();
        for (dst, src) in dst_pixels.iter_mut().zip(src_pixels) {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = src[3];
        }

        let Some(layer) = self.layer.as_ref() else { return };
        let surface = layer.wl_surface();
        surface.set_buffer_scale(self.scale as i32);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        // No frame callback is requested: nothing animates, and every redraw is
        // triggered by a key the user just pressed.
        if buffer.attach_to(surface).is_ok() {
            layer.commit();
        }
    }

    /// Re-renders after the scale changed, which requires a new buffer size.
    fn rescale(&mut self, scale: u32) {
        let scale = scale.max(1);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.icons = IconCache::new(&self.config.icon_theme, scale, self.config.layout.icon_size);
        // The pool only grows, so a scale increase needs the extra room.
        let _ = self.pool.resize(self.buffer_bytes());
        self.draw();
    }

    /// Moves the selection by `delta`, wrapping at both ends.
    fn move_selection(&mut self, delta: isize) {
        let count = self.matches.len() as isize;
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;

        // Keep the selection inside the visible window.
        let visible = self.visible_rows();
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }

    /// Chooses the highlighted application and ends the loop.
    fn commit_selection(&mut self) {
        // With nothing matching there is nothing to launch, and Enter is then
        // just a way to close the launcher.
        self.launch = self.matches.get(self.selected).copied();
        self.exit = true;
    }

    /// Handles one key press, from either a real press or a repeat.
    ///
    /// Query editing is append-only: `Backspace`, `Ctrl+W` and `Ctrl+U` are the
    /// whole of it, and there is no caret to move. That leaves `Home`, `End` and
    /// the arrows free for the list, which is what they are for in a launcher
    /// where a query is rarely more than a few characters.
    fn on_key(&mut self, event: KeyEvent) {
        let ctrl = self.modifiers.ctrl;
        let page = self.config.max_rows as isize;

        match event.keysym {
            Keysym::Escape => {
                self.exit = true;
                return;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.commit_selection();
                return;
            }
            // Shift+Tab arrives as ISO_Left_Tab on most layouts.
            Keysym::ISO_Left_Tab => self.move_selection(-1),
            Keysym::Tab => self.move_selection(1),
            Keysym::Down => self.move_selection(1),
            Keysym::Up => self.move_selection(-1),
            Keysym::n | Keysym::j if ctrl => self.move_selection(1),
            Keysym::p | Keysym::k if ctrl => self.move_selection(-1),
            Keysym::Page_Down | Keysym::KP_Page_Down => self.move_selection(page),
            Keysym::Page_Up | Keysym::KP_Page_Up => self.move_selection(-page),
            Keysym::Home => {
                self.selected = 0;
                self.scroll = 0;
            }
            Keysym::End => {
                self.selected = self.matches.len().saturating_sub(1);
                self.scroll = self.matches.len().saturating_sub(self.visible_rows());
            }
            Keysym::BackSpace if ctrl => {
                self.delete_word();
                return;
            }
            Keysym::BackSpace => {
                if self.query.pop().is_some() {
                    self.on_query_changed();
                }
                return;
            }
            Keysym::w if ctrl => {
                self.delete_word();
                return;
            }
            Keysym::u if ctrl => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.on_query_changed();
                }
                return;
            }
            _ => {
                // Ctrl and Alt combinations are commands elsewhere, and the
                // ones that are not are still not text. Logo is deliberately
                // allowed through: the launcher is usually bound to a Super
                // combination, and the first letter typed while Super is still
                // held should not be swallowed.
                if ctrl || self.modifiers.alt {
                    return;
                }
                let Some(text) = event.utf8 else { return };
                if text.is_empty() || text.chars().any(char::is_control) {
                    return;
                }
                self.query.push_str(&text);
                self.on_query_changed();
                return;
            }
        }

        // Only the selection moved, so the panel keeps its size.
        self.draw();
    }

    /// Deletes the last word of the query, plus the whitespace before it.
    fn delete_word(&mut self) {
        let trimmed = self.query.trim_end();
        let cut = trimmed.rfind(char::is_whitespace).map_or(0, |index| index + 1);
        if cut == self.query.len() {
            return;
        }
        self.query.truncate(cut);
        self.on_query_changed();
    }
}

impl CompositorHandler for Launcher {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        self.rescale(new_factor.max(1) as u32);
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // Redraws are driven by input, not by the clock; nothing animates.
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Launcher {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // A zero dimension means "pick your own", which is the normal reply for
        // an unanchored surface that asked for a specific size.
        let (_, height) = configure.new_size;
        let height = if height == 0 { self.logical_height } else { height };

        let first = !self.configured;
        let resized = height != self.logical_height;
        self.logical_height = height;
        self.configured = true;

        // Every resize we ask for comes back as a configure for the size we
        // already drew, so redrawing unconditionally would double the work of
        // every keystroke.
        if first || resized {
            let _ = self.pool.resize(self.buffer_bytes());
            self.draw();
        }
    }
}

impl SeatHandler for Launcher {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            // With repeat, because holding Backspace or Down is how a launcher
            // is actually used; the compositor's own rate and delay are
            // followed.
            let keyboard = self.seat_state.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.loop_handle.clone(),
                Box::new(|state: &mut Self, _keyboard, event| state.on_key(event)),
            );
            match keyboard {
                Ok(keyboard) => {
                    self.keyboard = Some(keyboard);
                    self.seat = Some(seat);
                }
                Err(err) => eprintln!("xla: no keyboard on seat: {err}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
    }

    fn remove_seat(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
    ) {
    }
}

impl KeyboardHandler for Launcher {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        // The keys reported as already down are of no interest: whatever
        // combination summoned the launcher is still held at this point, and
        // treating those as typed would put the binding's own letters into the
        // query.
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        // Losing an exclusive grab means something took over the screen; treat
        // it as a cancel rather than leaving an invisible grab behind.
        self.exit = true;
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.on_key(event);
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
        // Nothing to do. In particular a modifier release must *not* commit the
        // selection the way it does in xsw: the launcher is summoned with a
        // combination the user then releases in order to start typing.
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        // Only reached if the compositor sends repeats itself; ours come from
        // the timer set up in `new_capability`.
        self.on_key(event);
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl ShmHandler for Launcher {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl OutputHandler for Launcher {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ProvidesRegistryState for Launcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_registry!(Launcher);
smithay_client_toolkit::delegate_dispatch2!(Launcher);
