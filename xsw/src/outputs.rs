//! Finding which output COSMIC treats as the primary one.
//!
//! Wayland core has no notion of a primary display, and `wl_output` says
//! nothing about it. COSMIC does have one — Settings calls it the Xwayland
//! primary, and `cosmic-randr list` reports it — exposed as the
//! `xwayland_primary` event on `zcosmic_output_head_v1`. Reaching that means
//! going through `wlr-output-management`, because a cosmic head is obtained by
//! upgrading a `zwlr_output_head_v1`, the same pattern the toplevel protocols
//! use.
//!
//! Guessing instead was tempting — the leftmost output is usually the primary,
//! and is on the machine this was written on — but it stops being true the
//! moment someone puts their primary display in the middle of a row, which is
//! a common arrangement. Reading the flag is a little more code and is simply
//! correct.
//!
//! None of this is bound unless `display: primary` is configured, so the
//! default path pays no globals, no events and no extra roundtrip.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use cosmic_protocols::output_management::v1::client::{
    zcosmic_output_head_v1::{self, ZcosmicOutputHeadV1},
    zcosmic_output_manager_v1::{self, ZcosmicOutputManagerV1},
};
use smithay_client_toolkit::dispatch2::Dispatch2;
use wayland_client::globals::GlobalList;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
// Reached through sctk's reexport rather than as a direct dependency, so the
// crate version cannot diverge from the one sctk itself links against.
use smithay_client_toolkit::reexports::protocols_wlr::output_management::v1::client::{
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

/// `xwayland_primary` arrived in version 3, which is the only event we want.
const COSMIC_OUTPUT_VERSION: std::ops::RangeInclusive<u32> = 3..=3;
/// Any version announces heads and their names, which is all we need from it.
const WLR_OUTPUT_VERSION: std::ops::RangeInclusive<u32> = 1..=4;

/// User data for the mode objects we are obliged to accept but never read.
///
/// A local type rather than `()`: `Dispatch2` is a foreign trait, so
/// implementing it for a foreign type would break the orphan rule.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModeData;

/// What one output head told us about itself.
///
/// Doubles as the user data of both the wlr head and the cosmic head, so the
/// name from one and the primary flag from the other land in the same place.
#[derive(Debug, Clone, Default)]
pub struct HeadData(Arc<Mutex<HeadState>>);

#[derive(Debug, Default)]
struct HeadState {
    name: Option<String>,
    primary: bool,
}

/// Shared by the manager's dispatch, which needs somewhere to record heads and
/// the cosmic manager to upgrade them with.
#[derive(Debug, Clone, Default)]
pub struct ManagerData {
    heads: Arc<Mutex<Vec<HeadData>>>,
    done: Arc<AtomicBool>,
    cosmic: Arc<Mutex<Option<ZcosmicOutputManagerV1>>>,
}

/// Resolves the primary output's name, once the compositor has finished
/// enumerating heads.
#[derive(Debug)]
pub struct PrimaryFinder {
    data: ManagerData,
    /// `None` when the compositor does not implement the protocols, in which
    /// case there is nothing to wait for and no answer to give.
    available: bool,
}

impl PrimaryFinder {
    /// Binds both output managers. Absence is not an error: the caller falls
    /// back to letting the compositor choose the output.
    pub fn bind<D>(globals: &GlobalList, qh: &QueueHandle<D>) -> Self
    where
        D: Dispatch<ZwlrOutputManagerV1, ManagerData>
            + Dispatch<ZcosmicOutputManagerV1, ManagerData>
            + 'static,
    {
        let data = ManagerData::default();

        // The cosmic manager has to exist before the first head arrives, since
        // upgrading a head needs it.
        let cosmic = globals.bind::<ZcosmicOutputManagerV1, _, _>(
            qh,
            COSMIC_OUTPUT_VERSION,
            data.clone(),
        );
        let wlr =
            globals.bind::<ZwlrOutputManagerV1, _, _>(qh, WLR_OUTPUT_VERSION, data.clone());

        let available = match (cosmic, wlr) {
            (Ok(cosmic), Ok(_)) => {
                *data.cosmic.lock().unwrap() = Some(cosmic);
                true
            }
            _ => false,
        };

        Self { data, available }
    }

    /// Whether the compositor implements what we need.
    pub fn is_available(&self) -> bool {
        self.available
    }

    /// Whether head enumeration has finished, so the answer is trustworthy.
    pub fn is_done(&self) -> bool {
        !self.available || self.data.done.load(Ordering::Relaxed)
    }

    /// The name of the primary output, e.g. "HDMI-A-1".
    pub fn primary_name(&self) -> Option<String> {
        self.data
            .heads
            .lock()
            .unwrap()
            .iter()
            .find_map(|head| {
                let state = head.0.lock().unwrap();
                state.primary.then(|| state.name.clone()).flatten()
            })
    }
}

impl<D> Dispatch2<ZwlrOutputManagerV1, D> for ManagerData
where
    D: Dispatch<ZwlrOutputHeadV1, HeadData>
        + Dispatch<ZcosmicOutputHeadV1, HeadData>
        + 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _proxy: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _conn: &Connection,
        qh: &QueueHandle<D>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                // The proxy already carries its own `HeadData`, created by
                // `event_created_child` below; sharing that same instance with
                // the cosmic head is what lets one object's name and the
                // other's primary flag describe one display.
                let Some(data) = head.data::<HeadData>().cloned() else { return };
                self.heads.lock().unwrap().push(data.clone());
                if let Some(cosmic) = self.cosmic.lock().unwrap().as_ref() {
                    cosmic.get_head(&head, qh, data);
                }
            }
            zwlr_output_manager_v1::Event::Done { .. } => {
                self.done.store(true, Ordering::Relaxed);
            }
            // `finished` means the manager is gone; whatever we collected
            // before that still stands.
            _ => {}
        }
    }

    wayland_client::event_created_child!(D, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, HeadData::default())
    ]);
}

impl<D> Dispatch2<ZwlrOutputHeadV1, D> for HeadData
where
    D: Dispatch<ZwlrOutputModeV1, ModeData> + 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _head: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        // Only the name matters: it is what ties this head to a `wl_output`,
        // whose geometry and scale we already track through sctk.
        if let zwlr_output_head_v1::Event::Name { name } = event {
            self.0.lock().unwrap().name = Some(name);
        }
    }

    // Modes are of no interest, but the compositor sends them regardless and
    // wayland-client needs a way to construct the objects or it panics.
    wayland_client::event_created_child!(D, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ModeData)
    ]);
}

impl<D> Dispatch2<ZwlrOutputModeV1, D> for ModeData
where
    D: 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _mode: &ZwlrOutputModeV1,
        _event: zwlr_output_mode_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
    }
}

impl<D> Dispatch2<ZcosmicOutputHeadV1, D> for HeadData
where
    D: 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _head: &ZcosmicOutputHeadV1,
        event: zcosmic_output_head_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        if let zcosmic_output_head_v1::Event::XwaylandPrimary { state } = event {
            self.0.lock().unwrap().primary = state != 0;
        }
    }
}

impl<D> Dispatch2<ZcosmicOutputManagerV1, D> for ManagerData
where
    D: Dispatch<ZcosmicOutputHeadV1, HeadData> + 'static,
{
    fn event(
        &self,
        _state: &mut D,
        _proxy: &ZcosmicOutputManagerV1,
        _event: zcosmic_output_manager_v1::Event,
        _conn: &Connection,
        _qh: &QueueHandle<D>,
    ) {
        // This interface has no events; heads are created by request.
    }
}
