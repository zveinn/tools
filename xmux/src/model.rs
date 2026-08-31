//! The data model: sessions → tabs → panes, with each tab holding its
//! panes in a binary split tree.

use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use libghostty_vt::{
    Terminal, TerminalOptions,
    terminal::{
        ClipboardLocation, ConformanceLevel, DeviceAttributeFeature, DeviceAttributes,
        DeviceType, PrimaryDeviceAttributes, SecondaryDeviceAttributes, SizeReportSize,
    },
};

use crate::Result;
use crate::config::Config;
use crate::pty::Pty;

/// Nominal cell pixel size reported to applications that ask (XTWINOPS).
/// We don't render pixels ourselves, so any sane value works.
pub const CELL_PX: (u32, u32) = (8, 16);

static NEXT_PANE_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

pub fn winsize(cols: u16, rows: u16) -> nix::pty::Winsize {
    nix::pty::Winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

/// A direction to move pane focus in.
#[derive(Clone, Copy)]
pub enum NavDir {
    Left,
    Right,
    Up,
    Down,
}

/// One shell running on its own pty with its own virtual terminal.
///
/// The pty lives in an `Rc` because the terminal's `on_pty_write` effect
/// needs its own handle to write query responses back to the shell.
pub struct Pane {
    pub id: u64,
    pub pty: Rc<Pty>,
    pub term: Terminal<'static, 'static>,
    /// OSC 52 clipboard writes from programs in this pane, queued as
    /// (register, text) until the server forwards them to the attached
    /// client (helix/vim yank-to-clipboard).
    pub clipboard: Rc<std::cell::RefCell<Vec<(char, String)>>>,
    /// Command typed into the shell when this pane is restored after a
    /// server restart (set via the terminal-settings prompt).
    pub auto_run: Option<String>,
}

/// How a split divides a pane's rectangle.
#[derive(Clone, Copy)]
pub enum SplitDir {
    /// Stacked top/bottom — the divider line is horizontal.
    Horizontal,
    /// Side by side left/right — the divider line is vertical.
    Vertical,
}

/// The split tree of a tab: panes at the leaves, 50/50 splits inside.
pub enum Layout {
    /// Transient placeholder while a tab is being emptied; cleaned up in
    /// the same server-loop iteration.
    Empty,
    Leaf(Pane),
    Split {
        dir: SplitDir,
        a: Box<Layout>,
        b: Box<Layout>,
    },
}

/// A rectangle of host terminal cells.
#[derive(Clone, Copy)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// Divide a rect for a split: `a` keeps the top/left half, `b` gets the
/// bottom/right half, one row/column in between is the divider.
pub fn split_rect(dir: SplitDir, r: Rect) -> (Rect, Rect) {
    match dir {
        SplitDir::Horizontal => {
            let ha = r.h.saturating_sub(1) / 2;
            (
                Rect { h: ha, ..r },
                Rect {
                    y: r.y + ha + 1,
                    h: r.h.saturating_sub(ha + 1),
                    ..r
                },
            )
        }
        SplitDir::Vertical => {
            let wa = r.w.saturating_sub(1) / 2;
            (
                Rect { w: wa, ..r },
                Rect {
                    x: r.x + wa + 1,
                    w: r.w.saturating_sub(wa + 1),
                    ..r
                },
            )
        }
    }
}

impl Layout {
    /// All panes, in tree order (top/left before bottom/right).
    pub fn panes(&self) -> Vec<&Pane> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'s>(&'s self, out: &mut Vec<&'s Pane>) {
        match self {
            Layout::Empty => {}
            Layout::Leaf(pane) => out.push(pane),
            Layout::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn pane(&self, id: u64) -> Option<&Pane> {
        match self {
            Layout::Empty => None,
            Layout::Leaf(pane) => (pane.id == id).then_some(pane),
            Layout::Split { a, b, .. } => a.pane(id).or_else(|| b.pane(id)),
        }
    }

    pub fn pane_mut(&mut self, id: u64) -> Option<&mut Pane> {
        match self {
            Layout::Empty => None,
            Layout::Leaf(pane) => (pane.id == id).then_some(pane),
            Layout::Split { a, b, .. } => {
                if a.pane(id).is_some() {
                    a.pane_mut(id)
                } else {
                    b.pane_mut(id)
                }
            }
        }
    }

    /// Visit every pane with its rectangle within `rect`.
    pub fn for_each(
        &self,
        rect: Rect,
        f: &mut dyn FnMut(&Pane, Rect) -> Result<()>,
    ) -> Result<()> {
        match self {
            Layout::Empty => Ok(()),
            Layout::Leaf(pane) => f(pane, rect),
            Layout::Split { dir, a, b } => {
                let (ra, rb) = split_rect(*dir, rect);
                a.for_each(ra, f)?;
                b.for_each(rb, f)
            }
        }
    }

    pub fn for_each_mut(
        &mut self,
        rect: Rect,
        f: &mut dyn FnMut(&mut Pane, Rect) -> Result<()>,
    ) -> Result<()> {
        match self {
            Layout::Empty => Ok(()),
            Layout::Leaf(pane) => f(pane, rect),
            Layout::Split { dir, a, b } => {
                let (ra, rb) = split_rect(*dir, rect);
                a.for_each_mut(ra, f)?;
                b.for_each_mut(rb, f)
            }
        }
    }

    /// Replace the leaf `at` with a split of it and `new` (which is
    /// consumed on success).
    fn split_leaf(&mut self, at: u64, dir: SplitDir, new: &mut Option<Pane>) -> bool {
        match self {
            Layout::Empty => false,
            Layout::Leaf(pane) if pane.id == at => {
                let Some(new_pane) = new.take() else {
                    return false;
                };
                let old = std::mem::replace(self, Layout::Empty);
                *self = Layout::Split {
                    dir,
                    a: Box::new(old),
                    b: Box::new(Layout::Leaf(new_pane)),
                };
                true
            }
            Layout::Leaf(_) => false,
            Layout::Split { a, b, .. } => {
                a.split_leaf(at, dir, new) || b.split_leaf(at, dir, new)
            }
        }
    }

    /// Remove the pane `id`; a split with one side gone collapses into
    /// the surviving side. Returns `None` when the last pane is removed.
    fn remove(layout: Layout, id: u64) -> Option<Layout> {
        match layout {
            Layout::Empty => Some(Layout::Empty),
            Layout::Leaf(pane) if pane.id == id => None,
            leaf @ Layout::Leaf(_) => Some(leaf),
            Layout::Split { dir, a, b } => match (Self::remove(*a, id), Self::remove(*b, id)) {
                (Some(a), Some(b)) => Some(Layout::Split {
                    dir,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (None, Some(survivor)) | (Some(survivor), None) => Some(survivor),
                (None, None) => None,
            },
        }
    }
}

/// A named group of panes arranged in a split tree.
pub struct Tab {
    pub name: String,
    pub layout: Layout,
    /// Id of the pane receiving input.
    pub focused: u64,
    /// Fullscreen: the focused pane takes the whole content area.
    pub zoomed: bool,
}

/// A named group of tabs.
pub struct Session {
    pub id: u64,
    pub name: String,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// Created via `xmux agent new`: listed separately in the session
    /// manager and addressable by the agent commands, but otherwise a
    /// normal session.
    pub agent: bool,
    /// Last agent activity (`agent new/send/read`); agent sessions are
    /// listed most-recently-active first.
    pub last_activity: std::time::Instant,
    /// The content size the session was last laid out for (its client's
    /// size, or the agent default when never attached).
    pub last_size: (u16, u16),
}

impl Pane {
    pub fn new(size: (u16, u16), config: &Config) -> Result<Self> {
        Self::new_in(size, config, None)
    }

    /// Like `new`, with the shell started in `cwd` (state restore).
    pub fn new_in(size: (u16, u16), config: &Config, cwd: Option<&str>) -> Result<Self> {
        let (cols, rows) = size;
        let pty = Rc::new(Pty::spawn(winsize(cols, rows), config, cwd)?);

        let mut term = Terminal::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: config.scrollback_lines,
        })?;
        term.resize(cols, rows, CELL_PX.0, CELL_PX.1)?;

        // Effects let the terminal answer VT queries (device attributes,
        // size reports, ...) that programs like vim and htop send on
        // startup. Without at least `on_pty_write`, those programs can hang.
        let write_pty = Rc::clone(&pty);
        term.on_pty_write(move |_t, data| write_pty.write(data))?
            .on_size(|t| {
                Some(SizeReportSize {
                    rows: t.rows().unwrap_or(0),
                    columns: t.cols().unwrap_or(0),
                    cell_width: CELL_PX.0,
                    cell_height: CELL_PX.1,
                })
            })?
            .on_device_attributes(|_t| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[
                            DeviceAttributeFeature::COLUMNS_132,
                            DeviceAttributeFeature::SELECTIVE_ERASE,
                            DeviceAttributeFeature::ANSI_COLOR,
                        ],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: Default::default(),
                })
            })?;

        // Queue OSC 52 clipboard writes (helix/vim yank) for the server
        // to forward to the attached client's terminal.
        let clipboard: Rc<std::cell::RefCell<Vec<(char, String)>>> = Rc::default();
        let clip = Rc::clone(&clipboard);
        term.on_clipboard_write(move |_t, write| {
            let register = match write.location() {
                ClipboardLocation::Standard => 'c',
                ClipboardLocation::Selection | ClipboardLocation::Primary => 'p',
            };
            let contents: Vec<_> = write.contents().collect();
            let content = contents
                .iter()
                .find(|c| c.mime.starts_with("text/plain"))
                .or_else(|| contents.first());
            if let Some(content) = content {
                clip.borrow_mut().push((register, content.data.to_string()));
            }
            Ok(())
        })?;

        Ok(Self {
            id: NEXT_PANE_ID.fetch_add(1, Ordering::Relaxed),
            pty,
            term,
            clipboard,
            auto_run: None,
        })
    }

    pub fn resize(&mut self, size: (u16, u16)) -> Result<()> {
        let (cols, rows) = (size.0.max(1), size.1.max(1));
        self.term.resize(cols, rows, CELL_PX.0, CELL_PX.1)?;
        self.pty.resize(winsize(cols, rows));
        Ok(())
    }
}

impl Tab {
    /// A new tab starts with a single full-size pane.
    pub fn new(size: (u16, u16), name: String, config: &Config) -> Result<Self> {
        let pane = Pane::new(size, config)?;
        Ok(Self {
            name,
            focused: pane.id,
            layout: Layout::Leaf(pane),
            zoomed: false,
        })
    }

    pub fn is_empty(&self) -> bool {
        matches!(self.layout, Layout::Empty)
    }

    /// Resize every pane to its rectangle in the current layout — or,
    /// when zoomed, just the focused pane to the full area.
    pub fn apply_layout(&mut self, size: (u16, u16)) -> Result<()> {
        if self.zoomed {
            let focused = self.focused;
            if let Some(pane) = self.layout.pane_mut(focused) {
                return pane.resize(size);
            }
        }
        let full = Rect {
            x: 0,
            y: 0,
            w: size.0,
            h: size.1,
        };
        self.layout
            .for_each_mut(full, &mut |pane, rect| pane.resize((rect.w, rect.h)))
    }

    /// Toggle or clear fullscreen, resizing panes accordingly.
    pub fn set_zoom(&mut self, on: bool, size: (u16, u16)) -> Result<()> {
        if self.zoomed != on {
            self.zoomed = on;
            self.apply_layout(size)?;
        }
        Ok(())
    }

    /// Split the focused pane, focusing the new bottom/right shell.
    /// Ignored when the pane is too small to split.
    pub fn split(&mut self, dir: SplitDir, size: (u16, u16), config: &Config) -> Result<()> {
        let full = Rect {
            x: 0,
            y: 0,
            w: size.0,
            h: size.1,
        };
        let mut focused_rect = None;
        let focused = self.focused;
        self.layout.for_each(full, &mut |pane, rect| {
            if pane.id == focused {
                focused_rect = Some(rect);
            }
            Ok(())
        })?;
        let Some(rect) = focused_rect else {
            return Ok(());
        };
        let big_enough = match dir {
            SplitDir::Horizontal => rect.h >= 5,
            SplitDir::Vertical => rect.w >= 5,
        };
        if !big_enough {
            return Ok(());
        }

        // Spawned at the pre-split size; apply_layout corrects it below.
        // The new shell starts where the shell it was split from is.
        let cwd = self
            .layout
            .pane(self.focused)
            .and_then(|pane| pane.pty.cwd());
        let pane = Pane::new_in((rect.w, rect.h), config, cwd.as_deref())?;
        let new_id = pane.id;
        let mut new = Some(pane);
        if self.layout.split_leaf(self.focused, dir, &mut new) {
            self.focused = new_id;
            self.apply_layout(size)?;
        }
        Ok(())
    }

    /// Remove a pane (its shell exited); the tab may end up empty.
    /// Any structural change drops fullscreen.
    pub fn remove_pane(&mut self, id: u64) {
        self.zoomed = false;
        let layout = std::mem::replace(&mut self.layout, Layout::Empty);
        if let Some(layout) = Layout::remove(layout, id) {
            self.layout = layout;
        }
        if self.focused == id
            && let Some(first) = self.layout.panes().first()
        {
            self.focused = first.id;
        }
    }

    /// Cycle focus to the next pane in tree order.
    pub fn focus_next(&mut self) {
        let panes = self.layout.panes();
        if panes.len() < 2 {
            return;
        }
        let pos = panes.iter().position(|p| p.id == self.focused).unwrap_or(0);
        self.focused = panes[(pos + 1) % panes.len()].id;
    }

    /// Move focus to the nearest pane in the given direction: panes whose
    /// rectangle lies past the focused pane's edge, preferring ones that
    /// overlap it on the perpendicular axis, then the closest. Returns
    /// false when there is no pane in that direction (the caller may then
    /// jump to a neighboring tab).
    pub fn focus_dir(&mut self, dir: NavDir, size: (u16, u16)) -> Result<bool> {
        let full = Rect {
            x: 0,
            y: 0,
            w: size.0,
            h: size.1,
        };
        let mut rects = Vec::new();
        self.layout.for_each(full, &mut |pane, rect| {
            rects.push((pane.id, rect));
            Ok(())
        })?;
        let Some(&(_, f)) = rects.iter().find(|(id, _)| *id == self.focused) else {
            return Ok(false);
        };

        // Rank candidates by (has no perpendicular overlap, edge distance,
        // less overlap) so the natural neighbor wins.
        let mut best: Option<(u64, (bool, u16, u16))> = None;
        for &(id, r) in &rects {
            if id == self.focused {
                continue;
            }
            let (in_dir, dist) = match dir {
                NavDir::Left => (r.x + r.w <= f.x, f.x.saturating_sub(r.x + r.w)),
                NavDir::Right => (r.x >= f.x + f.w, r.x.saturating_sub(f.x + f.w)),
                NavDir::Up => (r.y + r.h <= f.y, f.y.saturating_sub(r.y + r.h)),
                NavDir::Down => (r.y >= f.y + f.h, r.y.saturating_sub(f.y + f.h)),
            };
            if !in_dir {
                continue;
            }
            let overlap = match dir {
                NavDir::Left | NavDir::Right => {
                    (f.y + f.h).min(r.y + r.h).saturating_sub(f.y.max(r.y))
                }
                NavDir::Up | NavDir::Down => {
                    (f.x + f.w).min(r.x + r.w).saturating_sub(f.x.max(r.x))
                }
            };
            let key = (overlap == 0, dist, u16::MAX - overlap);
            if best.is_none_or(|(_, bk)| key < bk) {
                best = Some((id, key));
            }
        }
        if let Some((id, _)) = best {
            self.focused = id;
            return Ok(true);
        }
        Ok(false)
    }

    /// Focus the pane hugging the given edge of the tab (e.g. `Left` =
    /// the leftmost pane, topmost on ties). Used when jumping in from a
    /// neighboring tab.
    pub fn focus_edge(&mut self, edge: NavDir, size: (u16, u16)) -> Result<()> {
        let full = Rect {
            x: 0,
            y: 0,
            w: size.0,
            h: size.1,
        };
        let mut best: Option<(u64, (u16, u16))> = None;
        self.layout.for_each(full, &mut |pane, r| {
            let key = match edge {
                NavDir::Left => (r.x, r.y),
                NavDir::Right => (u16::MAX - (r.x + r.w), r.y),
                NavDir::Up => (r.y, r.x),
                NavDir::Down => (u16::MAX - (r.y + r.h), r.x),
            };
            if best.is_none_or(|(_, bk)| key < bk) {
                best = Some((pane.id, key));
            }
            Ok(())
        })?;
        if let Some((id, _)) = best {
            self.focused = id;
        }
        Ok(())
    }
}

impl Session {
    /// A new session starts with a single tab running a fresh shell.
    pub fn new(size: (u16, u16), name: String, config: &Config) -> Result<Self> {
        Ok(Self {
            id: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            name,
            tabs: vec![Tab::new(size, "tab 1".to_string(), config)?],
            active_tab: 0,
            agent: false,
            last_activity: std::time::Instant::now(),
            last_size: size,
        })
    }

    /// Rebuild a session from saved state (tabs already constructed).
    pub fn restore(name: String, tabs: Vec<Tab>, active_tab: usize, size: (u16, u16)) -> Self {
        Self {
            id: NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed),
            name,
            active_tab: active_tab.min(tabs.len().saturating_sub(1)),
            tabs,
            agent: false,
            last_activity: std::time::Instant::now(),
            last_size: size,
        }
    }

    pub fn resize(&mut self, size: (u16, u16)) -> Result<()> {
        self.last_size = size;
        for tab in &mut self.tabs {
            tab.apply_layout(size)?;
        }
        Ok(())
    }
}
