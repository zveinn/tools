//! The on-screen switcher: a centered layer-shell overlay driven by the
//! keyboard.
//!
//! Presented on the `overlay` layer with exclusive keyboard interactivity, so
//! it draws above everything and receives every key including the modifier the
//! user is holding. It is deliberately *not* anchored: layer-shell centers a
//! surface that sets a size without anchors, which is exactly the placement we
//! want and saves us computing it from output geometry.

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::foreign_toplevel_list::{
    ForeignToplevelList, ForeignToplevelListHandler,
};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
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
use wayland_client::protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;

use std::time::{Duration, Instant};

use crate::config::{Config, Display};
use crate::icons::IconCache;
use crate::ipc::Step;
use crate::mru::Mru;
use crate::render::{Renderer, Row};
use crate::toplevels::{CosmicToplevels, Window, snapshot};

/// How long to wait for the compositor to report modifier state before
/// painting regardless.
///
/// Nothing is drawn until that state is known, because it decides whether this
/// invocation shows a list at all or was a flick of the binding that should
/// just switch windows. cosmic-comp reports it before the first configure, so
/// this only matters for a compositor that never does; painting is the safe
/// failure, since an invisible keyboard grab is worse than an unwanted list.
const MODIFIER_REPORT_GRACE: Duration = Duration::from_millis(100);

/// Which modifier the user is holding, if we could tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hold {
    Alt,
    Logo,
}

impl Hold {
    /// Reads this modifier out of a modifiers event.
    fn is_held(self, modifiers: &Modifiers) -> bool {
        match self {
            Self::Alt => modifiers.alt,
            Self::Logo => modifiers.logo,
        }
    }

    /// Maps a keysym to the modifier it represents.
    fn from_keysym(keysym: Keysym) -> Option<Self> {
        match keysym {
            Keysym::Alt_L | Keysym::Alt_R | Keysym::Meta_L | Keysym::Meta_R => Some(Self::Alt),
            Keysym::Super_L | Keysym::Super_R => Some(Self::Logo),
            _ => None,
        }
    }
}

pub struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    /// Created by [`App::present`], once the window count is known and we know
    /// there is actually something to show.
    layer: Option<LayerSurface>,

    foreign: ForeignToplevelList,
    cosmic: CosmicToplevels,
    config: Config,
    renderer: Renderer,
    icons: IconCache,

    seat: Option<wl_seat::WlSeat>,
    keyboard: Option<wl_keyboard::WlKeyboard>,

    /// Snapshot taken at startup. Held rather than recomputed per frame so the
    /// list the user is looking at does not reorder while they tab through it.
    windows: Vec<Window>,
    selected: usize,
    /// First visible row, so the selection stays on screen when the list is
    /// longer than `max_rows`.
    scroll: usize,

    /// Which modifier commits on release, once we have identified one.
    hold: Option<Hold>,
    /// Whether the compositor has yet said which modifiers were held when the
    /// switcher took keyboard focus. Nothing is painted before that is known,
    /// which is what lets a flicked binding switch windows without the list
    /// ever appearing.
    modifier_state_known: bool,
    /// When the list may first be drawn. Set once a held modifier is seen, to
    /// `debounce` in the future; a flick commits and exits before it arrives,
    /// so nothing is ever painted.
    paint_at: Option<Instant>,
    /// Backstop deadline for [`MODIFIER_REPORT_GRACE`].
    grace_until: Option<Instant>,
    /// Whether anything has been drawn yet. Once it has, redraws are immediate.
    painted: bool,
    /// Focus history, so the list can be ordered by recency.
    mru: Mru,

    scale: u32,
    /// Device-pixel size last configured.
    size: (u32, u32),
    configured: bool,
    /// Set when the user should be sent to the selected window on exit.
    activate: bool,
    pub exit: bool,
}

impl App {
    /// Sets up the Wayland state without showing anything yet.
    ///
    /// The surface cannot be created here because its height depends on how
    /// many windows exist, and those only arrive once the queue has been
    /// dispatched, which in turn needs this type to exist.
    pub fn new(
        globals: &wayland_client::globals::GlobalList,
        qh: &QueueHandle<Self>,
        shm: Shm,
        foreign: ForeignToplevelList,
        cosmic: CosmicToplevels,
        config: Config,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        // Enough for the initial layout; grown on configure if needed.
        let initial = (config.width * config.layout.height_for(config.max_rows) * 4) as usize;
        let pool = SlotPool::new(initial, &shm)?;
        let renderer = Renderer::new(&config);
        let icons = IconCache::new(&config.icon_theme, 1, config.layout.icon_size);

        Ok(Self {
            registry_state: RegistryState::new(globals),
            seat_state: SeatState::new(globals, qh),
            output_state: OutputState::new(globals, qh),
            shm,
            pool,
            layer: None,
            foreign,
            cosmic,
            config,
            renderer,
            icons,
            seat: None,
            keyboard: None,
            windows: Vec::new(),
            selected: 0,
            scroll: 0,
            hold: None,
            modifier_state_known: false,
            paint_at: None,
            grace_until: None,
            painted: false,
            mru: Mru::load(),
            scale: 1,
            size: (0, 0),
            configured: false,
            activate: false,
            exit: false,
        })
    }

    /// The current window list, in the compositor's announcement order.
    pub fn snapshot(&self) -> Vec<Window> {
        snapshot(&self.foreign, &self.cosmic)
    }

    /// Takes the window list and maps the overlay. `windows` must be non-empty.
    ///
    /// `windows` has to carry the activation state captured *before* this
    /// point: mapping an exclusive-keyboard layer surface makes cosmic-comp
    /// deactivate the focused toplevel, so from here on no window reports
    /// itself as active and the initial selection could no longer be derived.
    pub fn present(
        &mut self,
        qh: &QueueHandle<Self>,
        compositor: &CompositorState,
        layer_shell: &LayerShell,
        windows: Vec<Window>,
        primary_name: Option<&str>,
    ) {
        debug_assert!(!windows.is_empty());
        self.windows = windows;
        self.order_windows();

        let visible = self.windows.len().min(self.config.max_rows);
        self.size = (self.config.width, self.config.layout.height_for(visible));

        let surface = compositor.create_surface(qh);
        // A `None` output lets the compositor choose, which gives the output
        // holding the focused window; naming one pins the switcher there.
        let output = self.target_output(primary_name);
        let layer = layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("xsw"),
            output.as_ref(),
        );
        // No anchor: the compositor centers a sized, unanchored layer surface.
        layer.set_size(self.size.0, self.size.1);
        // Exclusive, not OnDemand: we must receive the held modifier's release
        // even though the user never clicked on us.
        layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
        layer.commit();
        self.layer = Some(layer);
        self.grace_until = Some(Instant::now() + MODIFIER_REPORT_GRACE);
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
            eprintln!("xsw: no output named {wanted:?}; using the active display");
        }
        found
    }

    /// Records the focused window in the history.
    ///
    /// Takes the *unfiltered* list and runs before any display filtering, so
    /// that focusing a window on another display is still remembered.
    /// Filtering first would quietly stop the history tracking anything
    /// outside the filter, degrading the ordering over time.
    ///
    /// Whatever the compositor says is focused is the authority here rather
    /// than our own history, because focus also changes by clicking a window,
    /// which never goes through xsw and so leaves no trace.
    pub fn note_focus(&mut self, windows: &[Window]) {
        if let Some(active) = windows.iter().find(|w| w.activated) {
            let identifier = active.identifier.clone();
            self.mru.promote(&identifier);
        }
        // Saved now rather than only on commit, so a cancelled run still
        // leaves the history reflecting reality.
        self.mru.save();
    }

    /// The output with this name, if it is currently connected.
    pub fn output_by_name(&self, wanted: &str) -> Option<wl_output::WlOutput> {
        self.output_state.outputs().find(|output| {
            self.output_state
                .info(output)
                .and_then(|info| info.name)
                .is_some_and(|name| name == wanted)
        })
    }

    /// Puts the windows in most-recently-used order and picks the first
    /// selection.
    fn order_windows(&mut self) {
        if self.config.mru {
            self.mru.sort(&mut self.windows, |window| window.identifier.as_str());
        }

        // Row 0 is the current window, so one step from it is where a single
        // flick of the binding should land. Stepping backwards for `--prev`
        // wraps to the bottom, which is what the shift variant should reach.
        let count = self.windows.len() as isize;
        let step: isize = if self.config.reverse { -1 } else { 1 };
        self.selected = step.rem_euclid(count) as usize;

        // Scroll it into view; reversing lands on the last row, which is past
        // the end of the first page whenever the list is longer than it.
        let visible = self.windows.len().min(self.config.max_rows);
        self.scroll = if self.selected >= visible { self.selected + 1 - visible } else { 0 };
    }

    /// The next moment the event loop needs to wake up for, if any.
    ///
    /// Nothing is reported before the surface is configured: the configure
    /// event wakes the loop by itself and draws, so a deadline that has
    /// already passed would otherwise spin the loop at its minimum timeout
    /// until the configure arrived.
    pub fn next_deadline(&self) -> Option<Instant> {
        if self.painted || !self.configured {
            return None;
        }
        match (self.paint_at, self.grace_until) {
            (Some(paint), _) => Some(paint),
            // Only relevant until modifier state arrives, which is what sets
            // `paint_at`.
            (None, Some(grace)) if !self.modifier_state_known => Some(grace),
            _ => None,
        }
    }

    /// Handles whichever deadline has come due.
    pub fn on_timer(&mut self, qh: &QueueHandle<Self>) {
        if self.painted {
            return;
        }
        let now = Instant::now();

        // The compositor never told us what was held: treat it as a hold and
        // show the list, since an exclusive grab with nothing on screen to
        // explain it is the worse outcome.
        if !self.modifier_state_known && self.grace_until.is_some_and(|at| now >= at) {
            self.modifier_state_known = true;
            self.paint_at = Some(now);
        }

        if self.paint_at.is_some_and(|at| now >= at) {
            self.draw(qh);
        }
    }

    /// Whether drawing is allowed yet.
    ///
    /// `configured` is part of it because attaching a buffer before the first
    /// configure has been acknowledged is a layer-shell protocol violation,
    /// and with `debounce_ms: 0` the paint deadline is already due by the time
    /// the modifiers event arrives, which can precede the configure.
    fn may_paint(&self) -> bool {
        self.configured
            && (self.painted || self.paint_at.is_some_and(|at| Instant::now() >= at))
    }

    /// Applies steps forwarded by a later invocation of xsw.
    ///
    /// This is how the binding's own key cycles the list: the compositor
    /// consumes that combination and spawns xsw again rather than delivering
    /// the key to us, so the new process hands its keystroke over the socket.
    pub fn apply_steps(&mut self, steps: &[Step], qh: &QueueHandle<Self>) {
        let before = self.selected;
        for step in steps {
            self.move_selection(match step {
                Step::Next => 1,
                Step::Prev => -1,
            });
        }
        if self.selected != before && self.configured {
            self.draw(qh);
        }
    }

    /// Applies window state that changed while the overlay is up.
    ///
    /// Called from the event loop rather than from the dispatch callback
    /// because the state lives behind a shared flag: Wayland hands the handle's
    /// user data to us by reference, so an event cannot reach `App` directly.
    ///
    /// Only `minimized` is worth reacting to. Activation is deliberately not
    /// copied back: while we hold the keyboard grab every window reports itself
    /// as inactive, so adopting that would throw away the focus we captured
    /// before mapping and which [`App::finish`] still needs.
    pub fn tick(&mut self, qh: &QueueHandle<Self>) {
        if !self.cosmic.take_dirty() {
            return;
        }

        let fresh = self.snapshot();
        let mut changed = false;
        for window in &mut self.windows {
            if let Some(update) = fresh.iter().find(|w| w.foreign == window.foreign)
                && window.minimized != update.minimized
            {
                window.minimized = update.minimized;
                changed = true;
            }
        }

        if changed && self.configured {
            self.draw(qh);
        }
    }

    /// Focuses the selected window, if the user committed to one.
    ///
    /// Called after the event loop ends so the overlay is already being torn
    /// down when the focus change lands, which avoids the compositor briefly
    /// handing focus back to our own surface.
    pub fn finish(&mut self, conn: &Connection) {
        if !self.activate {
            return;
        }
        let (Some(window), Some(seat)) = (self.windows.get(self.selected), self.seat.as_ref())
        else {
            return;
        };
        let identifier = window.identifier.clone();
        if self.cosmic.activate(&window.foreign, seat) {
            // The window we just switched to is now the most recent, so the
            // next invocation offers the one we came from.
            self.mru.promote(&identifier);
            self.mru.save();
            let _ = conn.roundtrip();
        }
    }

    /// Moves the selection by `delta`, wrapping at both ends.
    fn move_selection(&mut self, delta: isize) {
        let count = self.windows.len() as isize;
        if count == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(count) as usize;

        // Keep the selection inside the visible window.
        let visible = self.windows.len().min(self.config.max_rows);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }

    /// Commits to the selection and ends the loop.
    fn commit_selection(&mut self) {
        self.activate = true;
        self.exit = true;
    }

    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let (width, height) = self.size;
        if width == 0 || height == 0 || self.layer.is_none() {
            return;
        }
        // Held below the debounce so a flick of the binding never puts
        // anything on screen. Selection changes still apply; they are simply
        // not drawn until the first paint is due.
        if !self.may_paint() {
            return;
        }

        let visible = self.windows.len().min(self.config.max_rows);
        // Resolved in two passes: matching the rules only borrows `self.config`,
        // while looking icons up needs `self.icons` mutably, so the first pass
        // hands owned values to the second.
        let matched: Vec<_> = self
            .windows
            .iter()
            .skip(self.scroll)
            .take(visible)
            .map(|w| {
                let rule = self.config.title_rule(&w.app_id, &w.title);
                let title = rule.map_or_else(|| w.title.clone(), |r| r.title.clone());
                let name = rule.and_then(|r| r.name.clone());
                let icon = rule.and_then(|r| r.icon.clone());
                (w.app_id.clone(), title, w.minimized, name, icon)
            })
            .collect();

        let rows: Vec<_> = matched
            .into_iter()
            .map(|(app_id, title, minimized, name, icon)| {
                let mut app = self.icons.get(&app_id);
                if let Some(name) = name {
                    app.name = name;
                }
                // Only replace the icon if the override actually resolves, so
                // a typo in the rule leaves the real icon rather than a blank.
                if let Some(icon) = icon
                    && let Some(pixmap) = self.icons.by_name(&icon)
                {
                    app.icon = Some(pixmap);
                }
                (app, title, minimized)
            })
            .collect();

        let rows: Vec<Row<'_>> = rows
            .iter()
            .map(|(app, title, minimized)| Row { app, title, minimized: *minimized })
            .collect();

        let Some(pixmap) = self.renderer.draw(
            &rows,
            self.selected.saturating_sub(self.scroll),
            width,
            height,
            self.scale,
        ) else {
            return;
        };

        let stride = width as i32 * 4;
        let Ok((buffer, canvas)) = self.pool.create_buffer(
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Argb8888,
        ) else {
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
            self.painted = true;
        }
    }

    /// Re-renders after the scale changed, which requires a new buffer size.
    fn rescale(&mut self, scale: u32, qh: &QueueHandle<Self>) {
        let scale = scale.max(1);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.icons =
            IconCache::new(&self.config.icon_theme, scale, self.config.layout.icon_size);

        let visible = self.windows.len().min(self.config.max_rows);
        self.size = (self.config.width * scale, self.config.layout.height_for(visible) * scale);
        // The pool only grows, so a scale increase needs the extra room.
        let _ = self.pool.resize((self.size.0 * self.size.1 * 4) as usize);
        if self.configured {
            self.draw(qh);
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        self.rescale(new_factor.max(1) as u32, qh);
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

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        // A zero dimension means "pick your own", which is the normal reply for
        // an unanchored surface that asked for a specific size.
        let (w, h) = configure.new_size;
        let visible = self.windows.len().min(self.config.max_rows);
        let logical_w = if w == 0 { self.config.width } else { w };
        let logical_h = if h == 0 { self.config.layout.height_for(visible) } else { h };
        self.size = (logical_w * self.scale, logical_h * self.scale);
        let _ = self.pool.resize((self.size.0 * self.size.1 * 4) as usize);

        self.configured = true;
        // May be a no-op: `draw` holds off until the debounce has elapsed, and
        // cosmic-comp sends the modifiers event before this configure, so the
        // decision of whether to show anything has usually been made already.
        self.draw(qh);
    }
}

impl SeatHandler for App {
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
            match self.seat_state.get_keyboard(qh, &seat, None) {
                Ok(keyboard) => {
                    self.keyboard = Some(keyboard);
                    self.seat = Some(seat);
                }
                Err(err) => eprintln!("xsw: no keyboard on seat: {err}"),
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

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for App {
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
        // The pressed-key list that comes with focus is deliberately ignored.
        // cosmic-comp has been observed reporting a modifier keycode here while
        // the `modifiers` event sent with the same serial says nothing is held;
        // trusting this list put the switcher into hold mode waiting for a
        // release that never came, which hung it with the keyboard grabbed.
        // `modifiers` is the authoritative view of modifier state, so
        // `update_modifiers` alone decides whether this is a hold invocation.
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
        qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        let previous = self.selected;

        match event.keysym {
            Keysym::Escape | Keysym::q => {
                self.exit = true;
                return;
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                self.commit_selection();
                return;
            }
            // Shift+Tab arrives as ISO_Left_Tab on most layouts, but check the
            // modifier too in case it does not.
            Keysym::ISO_Left_Tab => self.move_selection(-1),
            Keysym::Tab => self.move_selection(1),
            Keysym::Down | Keysym::j | Keysym::n => self.move_selection(1),
            Keysym::Up | Keysym::k | Keysym::p => self.move_selection(-1),
            Keysym::Home => {
                self.selected = 0;
                self.scroll = 0;
            }
            Keysym::End => {
                self.selected = self.windows.len().saturating_sub(1);
                let visible = self.windows.len().min(self.config.max_rows);
                self.scroll = self.windows.len().saturating_sub(visible);
            }
            _ => return,
        }

        if self.selected != previous {
            self.draw(qh);
        }
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        // Releasing the held modifier commits. `update_modifiers` also covers
        // this, but a compositor is not obliged to send it for every release,
        // so accept whichever arrives first.
        if let Some(hold) = self.hold
            && Hold::from_keysym(event.keysym) == Some(hold)
        {
            self.commit_selection();
        }
    }

    fn repeat_key(
        &mut self,
        conn: &Connection,
        qh: &QueueHandle<Self>,
        keyboard: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.press_key(conn, qh, keyboard, serial, event);
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        // Arm on *any* event that reports a modifier depressed, not just the
        // first. cosmic-comp does not reliably include the held modifier in the
        // state it sends with keyboard focus: the same binding was observed
        // arriving both as `enter` with Alt already listed and as a focus with
        // no modifiers followed by a separate Alt press. Waiting for the next
        // report instead of only trusting the first covers both orderings.
        let depressed = if modifiers.alt {
            Some(Hold::Alt)
        } else if modifiers.logo {
            Some(Hold::Logo)
        } else {
            None
        };

        if !self.modifier_state_known {
            self.modifier_state_known = true;
            match depressed {
                // Held down, so the user is holding the binding. Schedule the
                // list rather than drawing it now: a quick flick releases
                // inside the debounce window and commits before this deadline
                // arrives, so nothing flickers on screen.
                Some(hold) => {
                    self.hold = Some(hold);
                    self.paint_at = Some(Instant::now() + self.config.debounce);
                    self.draw(qh);
                }
                // Nothing held by the time we got focus, so the binding was
                // flicked and released before the switcher finished starting.
                // Commit straight away without ever attaching a buffer, so a
                // quick Alt-Tab just switches windows and no list appears.
                None => self.commit_selection(),
            }
            return;
        }

        match self.hold {
            // Not a hold invocation yet; if a modifier is down it is now.
            None => self.hold = depressed,
            // Armed and the modifier is gone: this is the commit.
            Some(hold) if !hold.is_held(&modifiers) => self.commit_selection(),
            Some(_) => {}
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl OutputHandler for App {
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

impl ForeignToplevelListHandler for App {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelList {
        &mut self.foreign
    }

    fn new_toplevel(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        handle: ExtForeignToplevelHandleV1,
    ) {
        // A window opening while the switcher is up is tracked but not added to
        // the list on screen, so the row under the user's finger stays put.
        self.cosmic.track(&handle, qh);
    }

    fn update_toplevel(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _handle: ExtForeignToplevelHandleV1,
    ) {
        // Titles change as pages load; refresh the text without touching the
        // selection or the ordering.
        let fresh = snapshot(&self.foreign, &self.cosmic);
        let mut changed = false;
        for window in &mut self.windows {
            if let Some(update) = fresh.iter().find(|w| w.foreign == window.foreign)
                && window.title != update.title
            {
                window.title = update.title.clone();
                changed = true;
            }
        }
        if changed && self.configured {
            self.draw(qh);
        }
    }

    fn toplevel_closed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        handle: ExtForeignToplevelHandleV1,
    ) {
        self.cosmic.forget(&handle);

        let Some(index) = self.windows.iter().position(|w| w.foreign == handle) else { return };
        self.windows.remove(index);

        if self.windows.is_empty() {
            self.exit = true;
            return;
        }
        // Keep the highlight on the same window where possible.
        if index < self.selected || self.selected >= self.windows.len() {
            self.selected = self.selected.saturating_sub(1);
        }

        let visible = self.windows.len().min(self.config.max_rows);
        self.scroll = self.scroll.min(self.windows.len() - visible);
        // The list is shorter now, so ask for a smaller surface; the resulting
        // configure redraws.
        if let Some(layer) = self.layer.as_ref() {
            layer.set_size(self.config.width, self.config.layout.height_for(visible));
            layer.commit();
        }
    }
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
smithay_client_toolkit::delegate_dispatch2!(App);
