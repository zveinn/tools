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
use crate::model::{
    InboxRow, ItemDetail, ItemQuery, ItemRow, RepoRow, RepoTab, RunRow, StateFilter, TimeRange,
    View,
};
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
    Repo(usize),
    Run(usize),
}

/// A single repository taken over the whole window: the main tab bar is
/// replaced by a repo bar with its own PRs / Issues / Actions tabs.
#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub(crate) owner: String,
    pub(crate) repo: String,
    pub(crate) tab: RepoTab,
}

impl Scope {
    pub(crate) fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

pub(crate) struct App {
    db: Db,
    cfg: Config,
    ev_rx: Receiver<SyncEvent>,
    cmd_tx: UnboundedSender<SyncCmd>,
    query: ItemQuery,
    items: Vec<ItemRow>,
    inbox: Vec<InboxRow>,
    repos: Vec<RepoRow>,
    runs: Vec<RunRow>,
    flat: Vec<FlatRow>,
    counts: Vec<(View, usize)>,
    scope: Option<Scope>,
    scope_counts: Vec<(RepoTab, usize)>,
    /// View to restore when esc leaves a repo scope.
    return_view: View,
    /// Repos whose Actions runs were already requested this session.
    runs_requested: HashSet<String>,
    /// Whether the owned-repo list was already requested this session.
    owned_requested: bool,
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
            scope_repo: None,
            scope_kind: None,
        };
        Self {
            db,
            cfg,
            ev_rx,
            cmd_tx,
            query,
            items: Vec::new(),
            inbox: Vec::new(),
            repos: Vec::new(),
            runs: Vec::new(),
            flat: Vec::new(),
            counts: Vec::new(),
            scope: None,
            scope_counts: Vec::new(),
            return_view: View::SeenRepos,
            runs_requested: HashSet::new(),
            owned_requested: false,
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
                } else if self.scope.is_some() {
                    self.leave_scope();
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
            KeyCode::Char('h') | KeyCode::Left => self.shift_tab(-1),
            KeyCode::Char('l') | KeyCode::Right => self.shift_tab(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_sel(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('g') => self.select_abs(0),
            KeyCode::Char('G') => {
                let last = self.flat.len().saturating_sub(1);
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
            KeyCode::Enter if self.selected_repo().is_some() => self.enter_scope(),
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
                FlatRow::Repo(idx) => {
                    if self.repos.get(idx).is_some_and(|r| r.unread > 0) {
                        self.select_abs(i);
                        return;
                    }
                }
                FlatRow::Child { .. } | FlatRow::Run(_) => {}
            }
        }
        self.set_status(StatusKind::Info, "no unread in this view");
    }

    fn current_item(&self) -> Option<&ItemRow> {
        match self.flat.get(self.selected)? {
            FlatRow::Item(i) => self.items.get(*i),
            FlatRow::Child { parent, .. } => self.items.get(*parent),
            FlatRow::Notif(_) | FlatRow::Repo(_) | FlatRow::Run(_) => None,
        }
    }

    fn selected_inbox(&self) -> Option<&InboxRow> {
        match self.flat.get(self.selected)? {
            FlatRow::Notif(i) => self.inbox.get(*i),
            _ => None,
        }
    }

    pub(crate) fn selected_repo(&self) -> Option<&RepoRow> {
        match self.flat.get(self.selected)? {
            FlatRow::Repo(i) => self.repos.get(*i),
            _ => None,
        }
    }

    pub(crate) fn selected_run(&self) -> Option<&RunRow> {
        match self.flat.get(self.selected)? {
            FlatRow::Run(i) => self.runs.get(*i),
            _ => None,
        }
    }

    fn selected_link(&self) -> Option<&crate::model::IssueLink> {
        match self.flat.get(self.selected)? {
            FlatRow::Child { parent, link } => self.items.get(*parent)?.links.get(*link),
            FlatRow::Item(_) | FlatRow::Notif(_) | FlatRow::Repo(_) | FlatRow::Run(_) => None,
        }
    }

    /// Repo name plus PRs / Issues / Actions replace the main tab bar.
    fn enter_scope(&mut self) {
        let Some(repo) = self.selected_repo() else {
            return;
        };
        let scope = Scope {
            owner: repo.owner.clone(),
            repo: repo.name.clone(),
            tab: RepoTab::Prs,
        };
        let name = scope.full_name();
        self.return_view = self.query.view;
        self.scope = Some(scope);
        self.focus = Focus::List;
        self.detail_scroll = 0;
        self.selected = 0;
        self.reload();
        self.set_status(StatusKind::Info, format!("{name}  ·  h/l tabs"));
    }

    fn leave_scope(&mut self) {
        let Some(scope) = self.scope.take() else {
            return;
        };
        self.query.view = self.return_view;
        self.query.scope_repo = None;
        self.query.scope_kind = None;
        self.runs.clear();
        self.scope_counts.clear();
        self.focus = Focus::List;
        self.detail_scroll = 0;
        if self.query.view.is_repo_list() {
            self.reload_repo_list(Some(&scope.full_name()));
        } else {
            self.reload();
        }
        self.set_status(StatusKind::Info, self.query.view.name().to_string());
    }

    fn shift_tab(&mut self, delta: i32) {
        match self.scope.as_mut() {
            Some(scope) => {
                scope.tab = scope.tab.shift(delta);
                self.selected = 0;
                self.detail_scroll = 0;
                self.reload();
                self.request_runs(false);
            }
            None => {
                self.shift_view(delta);
                self.request_owned_repos(false);
            }
        }
    }

    /// The owned-repo list is not part of the background poll either.
    fn request_owned_repos(&mut self, force: bool) {
        if self.query.view != View::MyRepos || self.scope.is_some() {
            return;
        }
        if !force && self.owned_requested {
            return;
        }
        if self.cfg.offline || !self.cfg.has_token() {
            self.set_status(StatusKind::Warn, "offline / no token");
            return;
        }
        self.owned_requested = true;
        let _ = self.cmd_tx.send(SyncCmd::OwnedRepos);
        self.set_status(StatusKind::Info, "loading your repositories");
    }

    /// Actions runs are never part of the background poll — fetch on demand.
    fn request_runs(&mut self, force: bool) {
        let Some(scope) = self.scope.as_ref() else {
            return;
        };
        if scope.tab != RepoTab::Actions {
            return;
        }
        let key = scope.full_name();
        if !force && self.runs_requested.contains(&key) {
            return;
        }
        if self.cfg.offline || !self.cfg.has_token() {
            self.set_status(StatusKind::Warn, "offline / no token");
            return;
        }
        let (owner, repo) = (scope.owner.clone(), scope.repo.clone());
        self.runs_requested.insert(key);
        let _ = self.cmd_tx.send(SyncCmd::Actions { owner, repo });
        self.set_status(StatusKind::Info, "loading workflow runs");
    }

    /// The base query plus the repo scope, when one is open.
    fn effective_query(&self) -> ItemQuery {
        let mut q = self.query.clone();
        match self.scope.as_ref() {
            Some(scope) => {
                q.scope_repo = Some(scope.full_name());
                q.scope_kind = scope.tab.kind();
            }
            None => {
                q.scope_repo = None;
                q.scope_kind = None;
            }
        }
        q
    }

    /// Name of the list on screen: a repo tab when scoped, else the view.
    pub(crate) fn list_name(&self) -> &'static str {
        match self.scope.as_ref() {
            Some(scope) => scope.tab.name(),
            None => self.query.view.name(),
        }
    }

    /// Rows in the list are issues/PRs (so linking and read state apply).
    fn shows_items(&self) -> bool {
        match self.scope.as_ref() {
            Some(scope) => scope.tab != RepoTab::Actions,
            None => !self.query.view.is_repo_list() && self.query.view != View::Inbox,
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
        if let Some(repo) = self.selected_repo() {
            return Some(repo.html_url());
        }
        if let Some(run) = self.selected_run() {
            return run.html_url.clone().or_else(|| {
                Some(format!(
                    "https://github.com/{}/{}/actions/runs/{}",
                    run.owner, run.repo, run.github_id
                ))
            });
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
        if self
            .scope
            .as_ref()
            .is_some_and(|s| s.tab == RepoTab::Actions)
        {
            self.request_runs(true);
            return;
        }
        if self.query.view == View::MyRepos && self.scope.is_none() {
            self.request_owned_repos(true);
            return;
        }
        if let Some(repo) = self.selected_repo() {
            let name = repo.full_name();
            self.set_status(
                StatusKind::Info,
                format!("{name}: nothing to refresh — enter opens it, r → a time window resyncs"),
            );
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
        self.selected = 0;
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
        if !self.shows_items() {
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
        if !self.shows_items() {
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
        if self
            .scope
            .as_ref()
            .is_some_and(|s| s.tab == RepoTab::Actions)
        {
            let keep = self.selected_run().map(|r| r.github_id);
            self.reload_runs(keep);
            return;
        }
        if self.scope.is_some() {
            let keep = self.current_item().map(|i| i.id);
            self.reload_keep(keep.unwrap_or(-1));
            return;
        }
        match self.query.view {
            View::SeenRepos | View::MyRepos => {
                let keep = self.selected_repo().map(|r| r.full_name());
                self.reload_repo_list(keep.as_deref());
            }
            View::Inbox => {
                let keep = self.selected_inbox().map(|n| n.github_id.clone());
                self.reload_inbox(keep.as_deref());
            }
            _ => {
                let keep = self.current_item().map(|i| i.id);
                self.reload_keep(keep.unwrap_or(-1));
            }
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
        self.repos.clear();
        self.runs.clear();
        self.flat = (0..self.inbox.len()).map(FlatRow::Notif).collect();
        self.refresh_counts();
        let idx = keep_id
            .and_then(|id| self.inbox.iter().position(|n| n.github_id == id))
            .unwrap_or(0);
        self.select_abs(idx);
    }

    /// Seen Repos aggregates the local cache; My Repos reads back the last
    /// fetched owned list. Same columns, same order.
    fn reload_repo_list(&mut self, keep: Option<&str>) {
        let rows = if self.query.view == View::MyRepos {
            self.db.list_owned_repos(&self.query)
        } else {
            self.db.list_repos(&self.query)
        };
        match rows {
            Ok(rows) => self.repos = rows,
            Err(e) => {
                self.set_status(StatusKind::Err, format!("db: {e}"));
                return;
            }
        }
        self.items.clear();
        self.inbox.clear();
        self.runs.clear();
        self.flat = (0..self.repos.len()).map(FlatRow::Repo).collect();
        self.refresh_counts();
        let idx = keep
            .and_then(|name| {
                self.repos
                    .iter()
                    .position(|r| r.full_name().eq_ignore_ascii_case(name))
            })
            .unwrap_or(0);
        self.select_abs(idx);
    }

    fn reload_runs(&mut self, keep: Option<i64>) {
        let Some((owner, repo)) = self
            .scope
            .as_ref()
            .map(|s| (s.owner.clone(), s.repo.clone()))
        else {
            return;
        };
        match self.db.list_workflow_runs(&owner, &repo) {
            Ok(rows) => self.runs = rows,
            Err(e) => {
                self.set_status(StatusKind::Err, format!("db: {e}"));
                return;
            }
        }
        self.items.clear();
        self.inbox.clear();
        self.repos.clear();
        self.flat = (0..self.runs.len()).map(FlatRow::Run).collect();
        self.refresh_counts();
        let idx = keep
            .and_then(|id| self.runs.iter().position(|r| r.github_id == id))
            .unwrap_or(0);
        self.select_abs(idx);
    }

    fn reload_keep(&mut self, keep_id: i64) {
        self.inbox.clear();
        self.repos.clear();
        self.runs.clear();
        match self.db.list(&self.effective_query()) {
            Ok(items) => self.items = items,
            Err(e) => {
                self.set_status(StatusKind::Err, format!("db: {e}"));
                return;
            }
        }
        self.rebuild_flat();
        self.refresh_counts();
        let idx = self
            .flat
            .iter()
            .position(|row| matches!(row, FlatRow::Item(i) if self.items.get(*i).is_some_and(|it| it.id == keep_id)))
            .unwrap_or(0);
        self.select_abs(idx);
    }

    /// Tab-bar badges: the main views, or the open repo's three tabs.
    fn refresh_counts(&mut self) {
        let Some(scope) = self.scope.clone() else {
            match self.db.counts_by_view(&self.query) {
                Ok(c) => self.counts = c,
                Err(e) => self.set_status(StatusKind::Warn, format!("counts: {e}")),
            }
            return;
        };
        let mut out = Vec::with_capacity(RepoTab::ALL.len());
        for tab in RepoTab::ALL {
            let count = match tab.kind() {
                Some(kind) => {
                    let mut q = self.query.clone();
                    q.scope_repo = Some(scope.full_name());
                    q.scope_kind = Some(kind);
                    self.db.count(&q)
                }
                None => self.db.count_workflow_runs(&scope.owner, &scope.repo),
            };
            match count {
                Ok(n) => out.push((tab, n)),
                Err(e) => {
                    self.set_status(StatusKind::Warn, format!("counts: {e}"));
                    return;
                }
            }
        }
        self.scope_counts = out;
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
            Some(FlatRow::Repo(_)) | Some(FlatRow::Run(_)) | None => None,
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
        SyncKind::Actions => "actions",
        SyncKind::OwnedRepos => "repos",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HydratedItem, ItemState, Kind, Role};
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// Online with a token, so the on-demand fetches actually get queued on
    /// `cmd_tx` (no worker runs in tests, so nothing hits the network).
    fn test_cfg() -> Config {
        Config {
            username: Some("me".into()),
            token: Some("ghp_test".into()),
            api_url: "https://api.github.com".into(),
            graphql_url: "https://api.github.com/graphql".into(),
            allowed_repos: Vec::new(),
            poll_seconds: 90,
            backfill_days: 90,
            participating_only: false,
            dir: PathBuf::from("/tmp"),
            db_path: PathBuf::from("/tmp/xgit-test.db"),
            log_path: PathBuf::from("/tmp/xgit-test.log"),
            offline: false,
        }
    }

    fn item(repo: &str, number: i64, kind: Kind, state: ItemState) -> HydratedItem {
        HydratedItem {
            node_id: Some(format!("N{repo}{number}")),
            owner: "acme".into(),
            repo: repo.into(),
            number,
            kind,
            title: format!("item {number}"),
            body: String::new(),
            state,
            author: Some("me".into()),
            draft: false,
            html_url: format!("https://github.com/acme/{repo}/pull/{number}"),
            created_at: Some("2026-01-01T00:00:00Z".into()),
            updated_at: Some("2026-08-01T00:00:00Z".into()),
            closed_at: None,
            merged_at: None,
            comments_count: 0,
            review_decision: None,
            additions: None,
            deletions: None,
            changed_files: None,
            assignees: Vec::new(),
            labels: Vec::new(),
            review_requests: Vec::new(),
            reviews: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Two repos: `busy` has more open PRs, so it sorts first.
    fn seeded_app() -> (App, tokio::sync::mpsc::UnboundedReceiver<SyncCmd>) {
        let db = Db::open(":memory:").unwrap();
        let mut authored = BTreeSet::new();
        authored.insert(Role::Authored);
        for n in [1, 2] {
            db.upsert_item(
                &item("busy", n, Kind::Pr, ItemState::Open),
                &authored,
                false,
                true,
            )
            .unwrap();
        }
        db.upsert_item(
            &item("busy", 3, Kind::Issue, ItemState::Open),
            &authored,
            false,
            true,
        )
        .unwrap();
        // Unread, so the owned-repo join has something to report.
        db.upsert_item(
            &item("quiet", 4, Kind::Pr, ItemState::Open),
            &authored,
            true,
            true,
        )
        .unwrap();

        let (_ev_tx, ev_rx) = std::sync::mpsc::channel();
        let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(db, test_cfg(), ev_rx, cmd_tx);
        app.query.view = View::SeenRepos;
        app.reload();
        (app, cmd_rx)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn tab_count(app: &App, view: View) -> Option<usize> {
        app.counts.iter().find(|(v, _)| *v == view).map(|(_, n)| *n)
    }

    #[test]
    fn seen_repos_lists_repos_by_open_prs() {
        let (app, _rx) = seeded_app();
        assert_eq!(app.flat.len(), 2);
        assert_eq!(app.selected_repo().unwrap().full_name(), "acme/busy");
        assert_eq!(app.selected_repo().unwrap().open_prs, 2);
        assert_eq!(tab_count(&app, View::SeenRepos), Some(2));
    }

    /// My Repos is the fetched owned list, not the cache aggregate: repos the
    /// user owns but has no cached items for still show up, with GitHub's
    /// counts, and repos they only contribute to drop out.
    #[test]
    fn my_repos_lists_the_fetched_owned_repos() {
        let (mut app, mut rx) = seeded_app();
        app.db
            .replace_owned_repos(&[
                RepoRow {
                    owner: "acme".into(),
                    name: "quiet".into(),
                    open_prs: 9,
                    bot_prs: 2,
                    open_issues: 4,
                    unread: 0,
                    updated_at: Some("2026-09-01T00:00:00Z".into()),
                },
                RepoRow {
                    owner: "acme".into(),
                    name: "untouched".into(),
                    open_prs: 1,
                    bot_prs: 40,
                    open_issues: 0,
                    unread: 0,
                    updated_at: Some("2026-08-01T00:00:00Z".into()),
                },
            ])
            .unwrap();

        press(&mut app, KeyCode::Char('l')); // Seen Repos -> My Repos
        assert_eq!(app.query.view, View::MyRepos);
        assert!(
            matches!(rx.try_recv(), Ok(SyncCmd::OwnedRepos)),
            "opening the tab asks for a fresh list"
        );
        assert_eq!(app.list_name(), "My Repos");
        assert_eq!(app.flat.len(), 2);
        assert_eq!(app.selected_repo().unwrap().full_name(), "acme/quiet");
        assert_eq!(app.selected_repo().unwrap().open_prs, 9, "GitHub's total");
        assert_eq!(
            app.selected_repo().unwrap().unread,
            1,
            "unread still comes from the cache"
        );
        assert_eq!(
            app.repos[1].full_name(),
            "acme/untouched",
            "40 dependabot PRs do not outrank 9 human ones"
        );
        assert_eq!(tab_count(&app, View::MyRepos), Some(2));
        assert_eq!(tab_count(&app, View::SeenRepos), Some(2), "unchanged");
    }

    #[test]
    fn escape_from_a_my_repos_scope_returns_to_my_repos() {
        let (mut app, _rx) = seeded_app();
        app.db
            .replace_owned_repos(&[RepoRow {
                owner: "acme".into(),
                name: "busy".into(),
                open_prs: 5,
                bot_prs: 0,
                open_issues: 1,
                unread: 0,
                updated_at: None,
            }])
            .unwrap();
        press(&mut app, KeyCode::Char('l'));

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.scope.as_ref().unwrap().full_name(), "acme/busy");
        assert_eq!(app.items.len(), 2, "the scope still reads the cache");

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.query.view, View::MyRepos, "not back to Seen Repos");
        assert_eq!(app.selected_repo().unwrap().full_name(), "acme/busy");
    }

    #[test]
    fn enter_scopes_a_repo_and_escape_restores_the_bar() {
        let (mut app, _rx) = seeded_app();

        press(&mut app, KeyCode::Enter);
        let scope = app.scope.clone().expect("enter opens the repo");
        assert_eq!(scope.full_name(), "acme/busy");
        assert_eq!(scope.tab, RepoTab::Prs);
        assert_eq!(app.items.len(), 2, "only that repo's PRs");
        assert!(
            app.items
                .iter()
                .all(|i| i.repo == "busy" && i.kind == Kind::Pr)
        );
        assert_eq!(app.list_name(), "PRs");
        assert_eq!(
            app.scope_counts,
            vec![
                (RepoTab::Prs, 2),
                (RepoTab::Issues, 1),
                (RepoTab::Actions, 0)
            ]
        );

        press(&mut app, KeyCode::Char('l'));
        assert_eq!(app.scope.as_ref().unwrap().tab, RepoTab::Issues);
        assert_eq!(app.items.len(), 1);
        assert_eq!(app.items[0].number, 3);

        press(&mut app, KeyCode::Esc);
        assert!(app.scope.is_none(), "esc leaves the repo");
        assert_eq!(app.query.view, View::SeenRepos);
        assert_eq!(app.flat.len(), 2);
        assert_eq!(
            app.selected_repo().unwrap().full_name(),
            "acme/busy",
            "the repo we came from stays selected"
        );
    }

    #[test]
    fn escape_closes_the_preview_before_leaving_the_repo() {
        let (mut app, _rx) = seeded_app();
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        assert!(app.preview_open);

        press(&mut app, KeyCode::Esc);
        assert!(!app.preview_open);
        assert!(app.scope.is_some(), "first esc only closed the preview");

        press(&mut app, KeyCode::Esc);
        assert!(app.scope.is_none());
    }

    #[test]
    fn actions_tab_reads_cached_runs() {
        let (mut app, _rx) = seeded_app();
        app.db
            .replace_workflow_runs(
                "acme",
                "busy",
                &[RunRow {
                    github_id: 7,
                    owner: "acme".into(),
                    repo: "busy".into(),
                    name: "ci".into(),
                    title: "build".into(),
                    status: "completed".into(),
                    conclusion: Some("success".into()),
                    event: "push".into(),
                    branch: "main".into(),
                    run_number: 12,
                    actor: Some("me".into()),
                    html_url: Some("https://example.invalid/run".into()),
                    created_at: Some("2026-09-01T00:00:00Z".into()),
                    updated_at: None,
                }],
            )
            .unwrap();

        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('h')); // PRs -> Actions
        assert_eq!(app.scope.as_ref().unwrap().tab, RepoTab::Actions);
        assert_eq!(app.flat.len(), 1);
        assert_eq!(app.selected_run().unwrap().run_number, 12);
        assert_eq!(
            app.selected_url().as_deref(),
            Some("https://example.invalid/run")
        );
        assert!(app.items.is_empty(), "the item table is not drawn here");
    }

    #[test]
    fn filter_applies_to_the_repo_list() {
        let (mut app, _rx) = seeded_app();
        press(&mut app, KeyCode::Char('/'));
        for c in "quiet".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.flat.len(), 1);
        assert_eq!(app.selected_repo().unwrap().full_name(), "acme/quiet");
    }
}
