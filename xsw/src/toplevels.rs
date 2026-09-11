//! Window enumeration and focusing, via COSMIC's toplevel protocols.
//!
//! Enumeration uses the standard `ext-foreign-toplevel-list-v1`, which reports
//! app_id/title but by design offers no way to focus a window. Focusing is
//! COSMIC-specific: `zcosmic_toplevel_info_v1.get_cosmic_toplevel` upgrades a
//! foreign handle into a `zcosmic_toplevel_handle_v1`, which is what
//! `zcosmic_toplevel_manager_v1.activate` accepts. cosmic-comp implements no
//! wlr-foreign-toplevel-management, so this pairing is the only route to
//! raising a window, and it is why xsw is COSMIC-specific rather than generic
//! wlroots.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1::{self, ZcosmicToplevelHandleV1},
    zcosmic_toplevel_info_v1::{self, ZcosmicToplevelInfoV1},
};
use cosmic_protocols::toplevel_management::v1::client::zcosmic_toplevel_manager_v1::{
    self, ZcosmicToplevelManagerV1,
};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::foreign_toplevel_list::ForeignToplevelList;
use wayland_client::globals::{BindError, GlobalList};
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;

/// `get_cosmic_toplevel` arrived in version 2 and the `ext_workspace_*` events
/// in version 3; we need the former and tolerate the absence of the latter, so
/// ask for 3 but accept 2.
const INFO_VERSION: std::ops::RangeInclusive<u32> = 2..=3;

/// `activate` has existed since version 1, so any version the compositor
/// offers will do.
const MANAGER_VERSION: std::ops::RangeInclusive<u32> = 1..=4;

/// Marker user-data for the two COSMIC globals.
///
/// sctk's own `GlobalData` cannot be reused here: `Dispatch2` is a foreign
/// trait and `GlobalData` a foreign type, so implementing one for the other
/// violates the orphan rule.
#[derive(Debug, Clone, Copy)]
pub struct CosmicGlobal;

/// State that arrives on a `zcosmic_toplevel_handle_v1` after creation.
///
/// Held behind an `Arc<Mutex<..>>` because it doubles as the proxy's user data,
/// which Wayland hands back to us by shared reference during dispatch.
#[derive(Debug, Clone, Default)]
pub struct CosmicToplevelData {
    state: Arc<Mutex<CosmicToplevelState>>,
    /// Shared with every other handle and with [`CosmicToplevels`], so the UI
    /// can notice that some window's state changed. Dispatch only hands us
    /// `&self`, so a flag we can set through a shared reference is the way to
    /// signal out of an event.
    dirty: Arc<AtomicBool>,
}

#[derive(Debug, Default)]
struct CosmicToplevelState {
    activated: bool,
    minimized: bool,
    /// Outputs this window belongs to. Usually exactly one, but the protocol
    /// allows a window to span several.
    outputs: Vec<WlOutput>,
}

impl CosmicToplevelData {
    /// Whether the compositor currently considers this window focused.
    pub fn activated(&self) -> bool {
        self.state.lock().unwrap().activated
    }

    /// Whether the window is minimized, which we label in the list.
    pub fn minimized(&self) -> bool {
        self.state.lock().unwrap().minimized
    }

    /// The outputs this window belongs to.
    pub fn outputs(&self) -> Vec<WlOutput> {
        self.state.lock().unwrap().outputs.clone()
    }
}

/// The COSMIC half of window management: the two globals plus the foreign ->
/// cosmic handle pairing.
#[derive(Debug)]
pub struct CosmicToplevels {
    info: Option<ZcosmicToplevelInfoV1>,
    manager: Option<ZcosmicToplevelManagerV1>,
    /// Parallel to the foreign handles, keyed by the foreign handle's id so it
    /// survives the handle being cloned around.
    paired: Vec<(ExtForeignToplevelHandleV1, ZcosmicToplevelHandleV1)>,
    /// Set whenever any tracked window's state changes.
    dirty: Arc<AtomicBool>,
}

impl CosmicToplevels {
    /// Binds both COSMIC globals. Absence is reported rather than fatal so the
    /// caller can print a diagnostic naming the missing protocol.
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> (Self, Vec<BindError>)
    where
        D: Dispatch<ZcosmicToplevelInfoV1, CosmicGlobal>
            + Dispatch<ZcosmicToplevelManagerV1, CosmicGlobal>
            + 'static,
    {
        let mut errors = Vec::new();

        let info = match globals.bind(qh, INFO_VERSION, CosmicGlobal) {
            Ok(info) => Some(info),
            Err(err) => {
                errors.push(err);
                None
            }
        };
        let manager = match globals.bind(qh, MANAGER_VERSION, CosmicGlobal) {
            Ok(manager) => Some(manager),
            Err(err) => {
                errors.push(err);
                None
            }
        };

        (Self { info, manager, paired: Vec::new(), dirty: Arc::default() }, errors)
    }

    /// True once both globals are present, i.e. we can list *and* focus.
    pub fn is_usable(&self) -> bool {
        self.info.is_some() && self.manager.is_some()
    }

    /// Consumes the "some window's state changed" flag.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::Relaxed)
    }

    /// Upgrades a freshly announced foreign handle to a COSMIC handle so that
    /// its state events start flowing and it becomes focusable.
    pub fn track<D>(&mut self, foreign: &ExtForeignToplevelHandleV1, qh: &QueueHandle<D>)
    where
        D: Dispatch<ZcosmicToplevelHandleV1, CosmicToplevelData> + 'static,
    {
        let Some(info) = self.info.as_ref() else { return };
        if self.paired.iter().any(|(f, _)| f == foreign) {
            return;
        }
        let data = CosmicToplevelData {
            state: Arc::default(),
            dirty: Arc::clone(&self.dirty),
        };
        let cosmic = info.get_cosmic_toplevel(foreign, qh, data);
        self.paired.push((foreign.clone(), cosmic));
    }

    /// Drops the pairing for a window that has gone away.
    pub fn forget(&mut self, foreign: &ExtForeignToplevelHandleV1) {
        if let Some(idx) = self.paired.iter().position(|(f, _)| f == foreign) {
            let (_, cosmic) = self.paired.remove(idx);
            cosmic.destroy();
        }
    }

    /// The COSMIC handle paired with a foreign handle, if we have one.
    pub fn handle(&self, foreign: &ExtForeignToplevelHandleV1) -> Option<&ZcosmicToplevelHandleV1> {
        self.paired.iter().find(|(f, _)| f == foreign).map(|(_, c)| c)
    }

    /// The per-window state (activated/minimized) for a foreign handle.
    pub fn state(&self, foreign: &ExtForeignToplevelHandleV1) -> Option<CosmicToplevelData> {
        self.handle(foreign)?.data::<CosmicToplevelData>().cloned()
    }

    /// Focuses a window. The seat is required by the protocol as the focus is
    /// per-seat.
    pub fn activate(&self, foreign: &ExtForeignToplevelHandleV1, seat: &WlSeat) -> bool {
        let (Some(manager), Some(cosmic)) = (self.manager.as_ref(), self.handle(foreign)) else {
            return false;
        };
        manager.activate(cosmic, seat);
        true
    }
}

/// Snapshot of one window, flattened for the UI to render.
#[derive(Debug, Clone)]
pub struct Window {
    pub foreign: ExtForeignToplevelHandleV1,
    /// Outputs this window belongs to, for the `windows: primary` filter.
    pub outputs: Vec<WlOutput>,
    /// Opaque per-toplevel id from the protocol, stable for the window's
    /// lifetime and across connections. This is what the MRU history keys on.
    pub identifier: String,
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    pub minimized: bool,
}

/// Builds the render list in the compositor's announcement order.
///
/// Recency is not available here — the protocol exposes no such ordering — so
/// callers that want most-recently-used order hand the result to
/// [`crate::mru::Mru::sort`].
pub fn snapshot(foreign: &ForeignToplevelList, cosmic: &CosmicToplevels) -> Vec<Window> {
    foreign
        .toplevels()
        .iter()
        .filter_map(|handle| {
            let info = foreign.info(handle)?;
            let state = cosmic.state(handle);
            Some(Window {
                foreign: handle.clone(),
                outputs: state.as_ref().map(CosmicToplevelData::outputs).unwrap_or_default(),
                identifier: info.identifier,
                app_id: info.app_id,
                title: info.title,
                activated: state.as_ref().is_some_and(CosmicToplevelData::activated),
                minimized: state.as_ref().is_some_and(CosmicToplevelData::minimized),
            })
        })
        .collect()
}

impl<D> Dispatch2<ZcosmicToplevelInfoV1, D> for CosmicGlobal
where
    D: Dispatch<ZcosmicToplevelHandleV1, CosmicToplevelData> + 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _proxy: &ZcosmicToplevelInfoV1,
        _event: zcosmic_toplevel_info_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        // Every event on this interface (`toplevel`, `finished`) is deprecated
        // from version 2 on, and we bind 2 at minimum, so nothing should
        // arrive. `done` carries no payload we need: the foreign handle's own
        // `done` is what sctk already uses as the commit point.
    }

    // Required even though the compositor will not send the deprecated
    // `toplevel` event at the version we bind: wayland-client needs a way to
    // construct the child object for that opcode or it panics at dispatch.
    wayland_client::event_created_child!(D, ZcosmicToplevelInfoV1, [
        zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (ZcosmicToplevelHandleV1, CosmicToplevelData::default())
    ]);
}

impl<D> Dispatch2<ZcosmicToplevelHandleV1, D> for CosmicToplevelData
where
    D: 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _handle: &ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        // The protocol describes `output_enter` as the toplevel becoming
        // "visible" on an output, but cosmic-comp keeps the association while a
        // window is minimized or parked on another of that output's
        // workspaces — both verified against it. So this really means
        // "belongs to", which is what makes filtering by display useful:
        // windows elsewhere on the same display stay reachable.
        match &event {
            zcosmic_toplevel_handle_v1::Event::OutputEnter { output } => {
                let mut inner = self.state.lock().unwrap();
                if !inner.outputs.contains(output) {
                    inner.outputs.push(output.clone());
                }
                drop(inner);
                self.dirty.store(true, Ordering::Relaxed);
                return;
            }
            zcosmic_toplevel_handle_v1::Event::OutputLeave { output } => {
                self.state.lock().unwrap().outputs.retain(|o| o != output);
                self.dirty.store(true, Ordering::Relaxed);
                return;
            }
            _ => {}
        }

        if let zcosmic_toplevel_handle_v1::Event::State { state } = event {
            // The array is a packed list of u32 enum values in host byte order;
            // a state not present in the array is off.
            let mut inner = self.state.lock().unwrap();
            inner.activated = false;
            inner.minimized = false;
            let (values, _) = state.as_chunks::<4>();
            for value in values {
                let value = u32::from_ne_bytes(*value);
                match zcosmic_toplevel_handle_v1::State::try_from(value) {
                    Ok(zcosmic_toplevel_handle_v1::State::Activated) => inner.activated = true,
                    Ok(zcosmic_toplevel_handle_v1::State::Minimized) => inner.minimized = true,
                    _ => {}
                }
            }
            drop(inner);
            self.dirty.store(true, Ordering::Relaxed);
        }
    }
}

impl<D> Dispatch2<ZcosmicToplevelManagerV1, D> for CosmicGlobal
where
    D: 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _proxy: &ZcosmicToplevelManagerV1,
        _event: zcosmic_toplevel_manager_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        // Only `capabilities` is sent, advertising which requests are allowed.
        // We use `activate`, which every version supports unconditionally.
    }
}
