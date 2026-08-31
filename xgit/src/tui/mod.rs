mod ui;

use std::collections::HashSet;
use std::io::IsTerminal;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::db::Db;
use crate::model::{InboxRow, ItemDetail, ItemQuery, ItemRow, StateFilter, TimeRange, View};
use crate::sync::{SyncCmd, SyncEvent, SyncKind};

const SYNC_OPTIONS: [&str; 6] = [
    "this item",
    "last 7 days",
    "last 30 days",
    "last 60 days",
    "last 90 days",
    "all",
];

pub fn run(
    db: Db,
    cfg: Config,
    ev_rx: Receiver<SyncEvent>,
    cmd_tx: UnboundedSender<SyncCmd>,
) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        bail!("xgit needs a real terminal (stdout is not a TTY)");
    }
    let mut terminal = ratatui::try_init()?;
    let mut app = App::new(db, cfg, ev_rx, cmd_tx);
    app.reload();
    let result = app.loop_until_quit(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Filter,
    Help,
    SyncMenu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    List,
    Detail,
}

struct Status {
    message: String,
    kind: StatusKind,
    set_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Ok,
    Warn,
    Err,
}

#[derive(Clone, Copy)]
pub(crate) enum FlatRow {
    Item(usize),
    Child { parent: usize, link: usize },
    Notif(usize),
}

pub(crate) struct App {
    db: Db,
    cfg: Config,
    ev_rx: Receiver<SyncEvent>,
    cmd_tx: UnboundedSender<SyncCmd>,
    query: ItemQuery,
    items: Vec<ItemRow>,
    inbox: Vec<InboxRow>,
    flat: Vec<FlatRow>,
    counts: Vec<(View, usize)>,
    selected: usize,
    table_state: ratatui::widgets::TableState,
    detail: Option<ItemDetail>,
    detail_scroll: u16,
    preview_open: bool,
    focus: Focus,
    mode: Mode,
    filter_buf: String,
    sync_choice: usize,
    show_links: bool,
    link_override: HashSet<i64>,
    status: Status,
    syncing: bool,
    sync_label: String,
    last_sync: Option<Instant>,
    last_sync_msg: String,
    gql_remaining: Option<u32>,
    gql_limit: Option<u32>,
    spinner: usize,
    tick: Instant,
    should_quit: bool,
}

impl App {
    fn new(
        db: Db,
        cfg: Config,
        ev_rx: Receiver<SyncEvent>,
        cmd_tx: UnboundedSender<SyncCmd>,
    ) -> Self {
        let query = ItemQuery {
            allowed_repos: cfg.allowed_repos.clone(),
            time: TimeRange::Days(30),
            state: StateFilter::Open,
            view: View::Inbox,
            search: String::new(),
        };
        Self {
            db,
            cfg,
            ev_rx,
            cmd_tx,
            query,
            items: Vec::new(),
            inbox: Vec::new(),
            flat: Vec::new(),
            counts: Vec::new(),
            selected: 0,
            table_state: ratatui::widgets::TableState::default(),
            detail: None,
            detail_scroll: 0,
            preview_open: false,
            focus: Focus::List,
            mode: Mode::Normal,
            filter_buf: String::new(),
            sync_choice: 0,
            show_links: false,
            link_override: HashSet::new(),
            status: Status {
                message: "local cache · syncing in background".into(),
                kind: StatusKind::Info,
                set_at: Instant::now(),
            },
            syncing: false,
            sync_label: String::new(),
            last_sync: None,
            last_sync_msg: String::new(),
            gql_remaining: None,
            gql_limit: None,
            spinner: 0,
            tick: Instant::now(),
            should_quit: false,
        }
    }

    fn loop_until_quit(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.should_quit {
            terminal.draw(|f| ui::draw(f, self))?;
            self.drain_sync();
            if event::poll(Duration::from_millis(120))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key),
                    Event::Resize(_, _) => {}
                    _ => {}
                }
            }
            if self.tick.elapsed() >= Duration::from_millis(250) {
                self.spinner = self.spinner.wrapping_add(1);
                self.tick = Instant::now();
            }
        }
        let _ = self.cmd_tx.send(SyncCmd::Shutdown);
        Ok(())
    }

    fn drain_sync(&mut self) {
        loop {
            match self.ev_rx.try_recv() {
                Ok(ev) => self.on_sync(ev),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.cfg.offline {
                        self.set_status(StatusKind::Warn, "sync worker stopped");
                    }
                    break;
                }
            }
        }
    }

    fn on_sync(&mut self, ev: SyncEvent) {
        match ev {
            SyncEvent::Started { kind, message } => {
                self.syncing = true;
                self.sync_label = message.clone();
                self.set_status(StatusKind::Info, format!("{}…", kind_label(kind)));
            }
            SyncEvent::Progress { message } => {
                self.sync_label = message.clone();
            }
            SyncEvent::Finished {
                kind,
                message,
                upserted,
                unread,
                ..
            } => {
                self.syncing = false;
                self.last_sync = Some(Instant::now());
                self.last_sync_msg = message.clone();
                self.reload();
                let extra = if unread > 0 {
                    format!(" · {unread} unread")
                } else if upserted > 0 {
                    format!(" · {upserted} updated")
                } else {
                    String::new()
                };
                self.set_status(StatusKind::Ok, format!("{}{extra}", message));
                let _ = kind;
            }
            SyncEvent::Failed { kind, error } => {
                self.syncing = false;
                self.set_status(StatusKind::Err, format!("{}: {error}", kind_label(kind)));
            }
            SyncEvent::Rate { remaining, limit } => {
                self.gql_remaining = remaining;
                self.gql_limit = limit;
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        match self.mode {
            Mode::Help => {
                if matches!(
                    key.code,
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                ) {
                    self.mode = Mode::Normal;
                }
            }
            Mode::SyncMenu => match key.code {
                KeyCode::Esc => self.mode = Mode::Normal,
                KeyCode::Char('j') | KeyCode::Down => {
                    self.sync_choice = (self.sync_choice + 1) % SYNC_OPTIONS.len();
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.sync_choice =
                        (self.sync_choice + SYNC_OPTIONS.len() - 1) % SYNC_OPTIONS.len();
                }
                KeyCode::Char(c @ '1'..='6') => {
                    self.sync_choice = (c as u8 - b'1') as usize;
                    self.run_sync_choice();
                }
                KeyCode::Enter => self.run_sync_choice(),
                _ => {}
            },
            Mode::Filter => match key.code {
                KeyCode::Esc => {
                    self.filter_buf.clear();
                    self.query.search.clear();
                    self.mode = Mode::Normal;
                    self.reload();
                }
                KeyCode::Enter => {
                    self.query.search = self.filter_buf.clone();
                    self.mode = Mode::Normal;
                    self.reload();
                }
                KeyCode::Backspace => {
                    self.filter_buf.pop();
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.filter_buf.push(c);
                }
                _ => {}
            },
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn on_normal_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('/') => {
                self.mode = Mode::Filter;
                self.filter_buf = self.query.search.clone();
            }
            KeyCode::Esc => {
                if self.preview_open {
                    self.close_preview();
                } else if !self.query.search.is_empty() {
                    self.query.search.clear();
                    self.filter_buf.clear();
                    self.reload();
                }
            }
            KeyCode::Tab => {
                if self.preview_open {
                    self.focus = match self.focus {
                        Focus::List => Focus::Detail,
                        Focus::Detail => Focus::List,
                    };
                }
            }
            KeyCode::Char('i') => self.toggle_preview(),
            KeyCode::Char('h') | KeyCode::Left => self.shift_view(-1),
            KeyCode::Char('l') | KeyCode::Right => self.shift_view(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('g') => self.select_abs(0),
            KeyCode::Char('G') => {
                let last = self.items.len().saturating_sub(1);
                self.select_abs(last);
            }
            KeyCode::PageDown | KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || matches!(key.code, KeyCode::PageDown) =>
            {
                if self.preview_open && self.focus == Focus::Detail {
                    self.detail_scroll = self.detail_scroll.saturating_add(8);
                } else {
                    self.move_sel(10);
                }
            }
            KeyCode::PageUp | KeyCode::Char('u')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || matches!(key.code, KeyCode::PageUp) =>
            {
                if self.preview_open && self.focus == Focus::Detail {
                    self.detail_scroll = self.detail_scroll.saturating_sub(8);
                } else {
                    self.move_sel(-10);
                }
            }
            KeyCode::Char('J') => self.detail_scroll = self.detail_scroll.saturating_add(1),
            KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(1),
            KeyCode::Char('s') => {
                if self.query.view.uses_state_filter() {
                    self.query.state = self.query.state.cycle();
                    self.reload();
                    self.set_status(
                        StatusKind::Info,
                        format!("state {}", self.query.state.label()),
                    );
                }
            }
            KeyCode::Char('t') => self.toggle_selected_links(),
            KeyCode::Char('T') => self.toggle_all_links(),
            KeyCode::Char('m') => self.toggle_read(),
            KeyCode::Char('n') => self.next_unread(1),
            KeyCode::Char('N') => self.next_unread(-1),
            KeyCode::Char('o') | KeyCode::Enter => self.open_browser(),
            KeyCode::Char('y') => self.copy_url(),
            KeyCode::Char('c') => self.request_comments(),
            KeyCode::Char('r') => {
                self.sync_choice = 0;
                self.mode = Mode::SyncMenu;
            }
            _ => {}
        }
    }

    fn move_sel(&mut self, delta: i32) {
        if self.preview_open && self.focus == Focus::Detail {
            if delta > 0 {
                self.detail_scroll = self.detail_scroll.saturating_add(delta as u16);
            } else {
                self.detail_scroll = self.detail_scroll.saturating_sub((-delta) as u16);
            }
            return;
        }
        if self.flat.is_empty() {
            return;
        }
        let next = (self.selected as i32 + delta).clamp(0, self.flat.len() as i32 - 1) as usize;
        self.select_abs(next);
    }

    fn select_abs(&mut self, idx: usize) {
        if self.flat.is_empty() {
            self.selected = 0;
            self.table_state.select(None);
            self.detail = None;
            return;
        }
        self.selected = idx.min(self.flat.len() - 1);
        self.table_state.select(Some(self.selected));
        self.detail_scroll = 0;
        self.load_detail();
    }

    fn next_unread(&mut self, dir: i32) {
        if self.flat.is_empty() {
            return;
        }
        let n = self.flat.len();
        let start = self.selected;
        let mut i = start;
        for _ in 0..n {
            i = if dir > 0 {
                (i + 1) % n
            } else {
                (i + n - 1) % n
            };
            match self.flat[i] {
                FlatRow::Item(idx) => {
                    if self.items.get(idx).is_some_and(|it| it.unread) {
                        self.select_abs(i);
                        return;
                    }
                }
                FlatRow::Notif(idx) => {
                    if self.inbox.get(idx).is_some_and(|n| n.unread) {
                        self.select_abs(i);
                        return;
                    }
                }
                FlatRow::Child { .. } => {}
            }
        }
        self.set_status(StatusKind::Info, "no unread in this view");
    }

    fn current_item(&self) -> Option<&ItemRow> {
        match self.flat.get(self.selected)? {
            FlatRow::Item(i) => self.items.get(*i),
            FlatRow::Child { parent, .. } => self.items.get(*parent),
            FlatRow::Notif(_) => None,
        }
    }

    fn selected_inbox(&self) -> Option<&InboxRow> {
        match self.flat.get(self.selected)? {
            FlatRow::Notif(i) => self.inbox.get(*i),
            _ => None,
        }
    }

    fn selected_link(&self) -> Option<&crate::model::IssueLink> {
        match self.flat.get(self.selected)? {
            FlatRow::Child { parent, link } => self.items.get(*parent)?.links.get(*link),
            FlatRow::Item(_) | FlatRow::Notif(_) => None,
        }
    }

    fn toggle_read(&mut self) {
        if let Some(n) = self.selected_inbox().cloned() {
            let next = !n.unread;
            if let Err(e) = self.db.set_notif_unread(&n.github_id, next) {
                self.set_status(StatusKind::Err, e.to_string());
                return;
            }
            self.reload();
            self.set_status(
                StatusKind::Ok,
                if next { "marked unread" } else { "marked read" },
            );
            return;
        }
        let Some(item) = self.current_item().cloned() else {
            return;
        };
        let next = !item.unread;
        if let Err(e) = self.db.set_unread(item.id, next) {
            self.set_status(StatusKind::Err, e.to_string());
            return;
        }
        self.reload_keep(item.id);
        self.set_status(
            StatusKind::Ok,
            if next { "marked unread" } else { "marked read" },
        );
    }

    fn selected_url(&self) -> Option<String> {
        if let Some(n) = self.selected_inbox() {
            return n.html_url();
        }
        if let Some(link) = self.selected_link() {
            return Some(format!(
                "https://github.com/{}/issues/{}",
                link.repo, link.number
            ));
        }
        if let Some(url) = self.current_item().and_then(|i| i.html_url.clone()) {
            return Some(url);
        }
        if let Some(url) = self.detail.as_ref().and_then(|d| d.row.html_url.clone()) {
            return Some(url);
        }
        let item = self.current_item()?;
        let kind = if item.kind == crate::model::Kind::Pr {
            "pull"
        } else {
            "issues"
        };
        Some(format!(
            "https://github.com/{}/{}/{kind}/{}",
            item.owner, item.repo, item.number
        ))
    }

    fn open_browser(&mut self) {
        match self.selected_url() {
            Some(url) => match open::that(&url) {
                Ok(()) => self.set_status(StatusKind::Ok, "opened in browser"),
                Err(e) => self.set_status(StatusKind::Err, e.to_string()),
            },
            None => self.set_status(StatusKind::Warn, "no url"),
        }
    }

    fn copy_url(&mut self) {
        let Some(url) = self.selected_url() else {
            self.set_status(StatusKind::Warn, "no url");
            return;
        };
        match crate::clipboard::copy_text(&url) {
            Ok(()) => self.set_status(StatusKind::Ok, format!("copied {url}")),
            Err(e) => self.set_status(StatusKind::Err, format!("copy failed: {e}")),
        }
    }

    fn request_comments(&mut self) {
        if self.cfg.offline || !self.cfg.has_token() {
            self.set_status(StatusKind::Warn, "offline / no token");
            return;
        }
        if let Some(n) = self.selected_inbox() {
            let (Some(number), Some(item_id)) = (n.number, n.item_id) else {
                self.set_status(StatusKind::Warn, "notification is not a cached issue/PR");
                return;
            };
            let _ = self.cmd_tx.send(SyncCmd::Comments {
                owner: n.owner.clone(),
                repo: n.repo.clone(),
                number,
                item_id,
            });
            self.set_status(StatusKind::Info, "loading comments");
            return;
        }
        if let Some(link) = self.selected_link() {
            let Some(item_id) = link.to_id else {
                self.set_status(StatusKind::Warn, "linked issue is not in the local cache");
                return;
            };
            let (owner, repo) = split_repo(&link.repo);
            let _ = self.cmd_tx.send(SyncCmd::Comments {
                owner,
                repo,
                number: link.number,
                item_id,
            });
            self.set_status(StatusKind::Info, "loading comments");
            return;
        }
        let Some(item) = self.current_item() else {
            return;
        };
        let _ = self.cmd_tx.send(SyncCmd::Comments {
            owner: item.owner.clone(),
            repo: item.repo.clone(),
            number: item.number,
            item_id: item.id,
        });
        self.set_status(StatusKind::Info, "loading comments");
    }

    fn run_sync_choice(&mut self) {
        self.mode = Mode::Normal;
        match self.sync_choice {
            0 => self.refresh_selected(),
            1 => self.start_created_sync(Some(7)),
            2 => self.start_created_sync(Some(30)),
            3 => self.start_created_sync(Some(60)),
            4 => self.start_created_sync(Some(90)),
            5 => self.start_created_sync(None),
            _ => {}
        }
    }

    fn start_created_sync(&mut self, created_days: Option<u32>) {
        if self.cfg.offline || !self.cfg.has_token() {
            self.set_status(StatusKind::Warn, "offline / no token");
            return;
        }
        let _ = self.cmd_tx.send(SyncCmd::Search { created_days });
        self.set_status(
            StatusKind::Info,
            match created_days {
                Some(d) => format!("syncing items created in the last {d}d"),
                None => "syncing all involvement".into(),
            },
        );
    }

    fn refresh_selected(&mut self) {
        if self.cfg.offline || !self.cfg.has_token() {
            self.set_status(StatusKind::Warn, "offline / no token");
            return;
        }
        if let Some(n) = self.selected_inbox() {
            let Some(number) = n.number else {
                self.set_status(StatusKind::Warn, "notification has no issue/PR");
                return;
            };
            let _ = self.cmd_tx.send(SyncCmd::Refresh {
                owner: n.owner.clone(),
                repo: n.repo.clone(),
                number,
                item_id: n.item_id.unwrap_or(0),
            });
            self.set_status(StatusKind::Info, "refreshing item");
            return;
        }
        if let Some(link) = self.selected_link() {
            let (owner, repo) = split_repo(&link.repo);
            let _ = self.cmd_tx.send(SyncCmd::Refresh {
                owner,
                repo,
                number: link.number,
                item_id: link.to_id.unwrap_or(0),
            });
            self.set_status(StatusKind::Info, "refreshing linked issue");
            return;
        }
        let Some(item) = self.current_item() else {
            return;
        };
        let _ = self.cmd_tx.send(SyncCmd::Refresh {
            owner: item.owner.clone(),
            repo: item.repo.clone(),
            number: item.number,
            item_id: item.id,
        });
        self.set_status(StatusKind::Info, "refreshing item");
    }

    fn toggle_preview(&mut self) {
        if self.preview_open {
            self.close_preview();
            return;
        }
        if self.flat.is_empty() {
            self.set_status(StatusKind::Info, "nothing to preview");
            return;
        }
        self.preview_open = true;
        self.focus = Focus::Detail;
        self.detail_scroll = 0;
        self.load_detail();
        if let Some(d) = &self.detail {
            if d.comments_fetched_at.is_none() {
                self.request_comments();
            }
        }
    }

    fn close_preview(&mut self) {
        self.preview_open = false;
        self.focus = Focus::List;
        self.detail_scroll = 0;
    }

    fn shift_view(&mut self, delta: i32) {
        self.query.view = self.query.view.shift(delta);
        self.reload();
    }

    fn item_shows_links(&self, id: i64) -> bool {
        self.show_links ^ self.link_override.contains(&id)
    }

    fn relayout_links(&mut self) {
        let keep = self.current_item().map(|i| i.id);
        self.rebuild_flat();
        let idx = keep
            .and_then(|id| {
                self.flat.iter().position(|row| {
                    matches!(row, FlatRow::Item(i) if self.items.get(*i).is_some_and(|it| it.id == id))
                })
            })
            .unwrap_or(0);
        self.select_abs(idx);
    }

    fn toggle_selected_links(&mut self) {
        if self.query.view == View::Inbox {
            return;
        }
        let Some(id) = self.current_item().map(|i| i.id) else {
            return;
        };
        if !self.link_override.remove(&id) {
            self.link_override.insert(id);
        }
        let on = self.item_shows_links(id);
        self.relayout_links();
        self.set_status(
            StatusKind::Info,
            if on {
                "linked items on for this item"
            } else {
                "linked items off for this item"
            },
        );
    }

    fn toggle_all_links(&mut self) {
        if self.query.view == View::Inbox {
            return;
        }
        self.show_links = !self.show_links;
        self.link_override.clear();
        self.relayout_links();
        self.set_status(
            StatusKind::Info,
            if self.show_links {
                "linked items on"
            } else {
                "linked items off"
            },
        );
    }

    fn rebuild_flat(&mut self) {
        self.flat.clear();
        for (i, item) in self.items.iter().enumerate() {
            self.flat.push(FlatRow::Item(i));
            if !self.item_shows_links(item.id) {
                continue;
            }
            for (j, link) in item.links.iter().enumerate() {
                if item.shows_nested(link) {
                    self.flat.push(FlatRow::Child { parent: i, link: j });
                }
            }
        }
    }

    fn reload(&mut self) {
        if self.query.view == View::Inbox {
            let keep = self.selected_inbox().map(|n| n.github_id.clone());
            self.reload_inbox(keep.as_deref());
        } else {
            let keep = self.current_item().map(|i| i.id);
            self.reload_keep(keep.unwrap_or(-1));
        }
    }

    fn reload_inbox(&mut self, keep_id: Option<&str>) {
        match self
            .db
            .list_notifications(&self.query.search, &self.query.allowed_repos)
        {
            Ok(rows) => self.inbox = rows,
            Err(e) => {
                self.set_status(StatusKind::Err, format!("db: {e}"));
                return;
            }
        }
        self.items.clear();
        self.flat = (0..self.inbox.len()).map(FlatRow::Notif).collect();
        match self.db.counts_by_view(&self.query) {
            Ok(c) => self.counts = c,
            Err(e) => self.set_status(StatusKind::Warn, format!("counts: {e}")),
        }
        let idx = keep_id
            .and_then(|id| self.inbox.iter().position(|n| n.github_id == id))
            .unwrap_or(0);
        self.select_abs(idx);
    }

    fn reload_keep(&mut self, keep_id: i64) {
        self.inbox.clear();
        match self.db.list(&self.query) {
            Ok(items) => self.items = items,
            Err(e) => {
                self.set_status(StatusKind::Err, format!("db: {e}"));
                return;
            }
        }
        self.rebuild_flat();
        match self.db.counts_by_view(&self.query) {
            Ok(c) => self.counts = c,
            Err(e) => self.set_status(StatusKind::Warn, format!("counts: {e}")),
        }
        let idx = self
            .flat
            .iter()
            .position(|row| matches!(row, FlatRow::Item(i) if self.items.get(*i).is_some_and(|it| it.id == keep_id)))
            .unwrap_or(0);
        self.select_abs(idx);
    }

    fn load_detail(&mut self) {
        let id = match self.flat.get(self.selected) {
            Some(FlatRow::Item(i)) => self.items.get(*i).map(|it| it.id),
            Some(FlatRow::Child { parent, link }) => self
                .items
                .get(*parent)
                .and_then(|p| p.links.get(*link))
                .and_then(|l| l.to_id),
            Some(FlatRow::Notif(i)) => {
                let n = self.inbox.get(*i);
                n.and_then(|n| n.item_id).or_else(|| {
                    let n = n?;
                    let num = n.number?;
                    self.db.find_item_id(&n.owner, &n.repo, num).ok().flatten()
                })
            }
            None => None,
        };
        let Some(id) = id else {
            self.detail = None;
            return;
        };
        match self.db.get_detail(id) {
            Ok(d) => self.detail = d,
            Err(e) => {
                self.detail = None;
                self.set_status(StatusKind::Err, format!("detail: {e}"));
            }
        }
    }

    fn set_status(&mut self, kind: StatusKind, msg: impl Into<String>) {
        self.status = Status {
            message: msg.into(),
            kind,
            set_at: Instant::now(),
        };
    }
}

fn split_repo(repo: &str) -> (String, String) {
    match repo.split_once('/') {
        Some((o, n)) => (o.to_string(), n.to_string()),
        None => (repo.to_string(), String::new()),
    }
}

fn kind_label(k: SyncKind) -> &'static str {
    match k {
        SyncKind::Poll => "sync",
        SyncKind::Search => "sync",
        SyncKind::RefreshItem => "refresh",
        SyncKind::Comments => "comments",
    }
}
