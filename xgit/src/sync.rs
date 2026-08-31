use std::collections::BTreeSet;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use tracing::info;

use crate::config::Config;
use crate::db::Db;
use crate::github::{GhClient, ItemRef, search_queries};
use crate::model::{HydratedItem, Role, TimeRange};
use crate::timeutil::now_rfc3339;

const OPEN_REFRESH_BATCH: usize = 25;
const HYDRATE_BATCH: usize = 20;
const OPEN_REFRESH_AGE_HOURS: i64 = 6;
const SAFETY_SEARCH_HOURS: i64 = 36;

#[derive(Debug, Clone)]
pub enum SyncKind {
    Poll,
    Search,
    RefreshItem,
    Comments,
}

#[derive(Debug, Clone)]
pub enum SyncEvent {
    Started {
        kind: SyncKind,
        message: String,
    },
    Progress {
        message: String,
    },
    Finished {
        kind: SyncKind,
        fetched: u32,
        upserted: u32,
        unread: u32,
        message: String,
    },
    Failed {
        kind: SyncKind,
        error: String,
    },
    Rate {
        remaining: Option<u32>,
        limit: Option<u32>,
    },
}

#[derive(Debug, Clone)]
pub enum SyncCmd {
    Poll,
    /// Re-search involvement. `created_days: None` means no created-date bound.
    Search {
        created_days: Option<u32>,
    },
    Refresh {
        owner: String,
        repo: String,
        number: i64,
        item_id: i64,
    },
    Comments {
        owner: String,
        repo: String,
        number: i64,
        item_id: i64,
    },
    Shutdown,
}

pub struct SyncReport {
    pub fetched: u32,
    pub upserted: u32,
    pub unread: u32,
    pub message: String,
}

pub async fn run_worker(
    db: Db,
    cfg: Config,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<SyncCmd>,
    ev_tx: std::sync::mpsc::Sender<SyncEvent>,
) {
    let client = match GhClient::new(&cfg) {
        Ok(c) => c,
        Err(e) => {
            let _ = ev_tx.send(SyncEvent::Failed {
                kind: SyncKind::Poll,
                error: e.to_string(),
            });
            return;
        }
    };

    if let Err(e) = ensure_username(&db, &cfg, &client).await {
        let _ = ev_tx.send(SyncEvent::Failed {
            kind: SyncKind::Poll,
            error: format!("cannot resolve GitHub login: {e}"),
        });
    }

    let _ = poll_once(&db, &cfg, &client, &ev_tx).await;

    let mut interval = tokio::time::interval(Duration::from_secs(cfg.poll_seconds.max(30)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    interval.tick().await;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    None | Some(SyncCmd::Shutdown) => break,
                    Some(SyncCmd::Poll) => {
                        let _ = poll_once(&db, &cfg, &client, &ev_tx).await;
                    }
                    Some(SyncCmd::Search { created_days }) => {
                        let label = match created_days {
                            Some(d) => format!("sync created last {d}d"),
                            None => "sync all".into(),
                        };
                        emit(
                            &ev_tx,
                            SyncEvent::Started {
                                kind: SyncKind::Search,
                                message: label,
                            },
                        );
                        match search_created(&db, &cfg, &client, &ev_tx, created_days).await {
                            Ok(r) => emit(
                                &ev_tx,
                                SyncEvent::Finished {
                                    kind: SyncKind::Search,
                                    fetched: r.fetched,
                                    upserted: r.upserted,
                                    unread: r.unread,
                                    message: r.message,
                                },
                            ),
                            Err(e) => emit(
                                &ev_tx,
                                SyncEvent::Failed {
                                    kind: SyncKind::Search,
                                    error: e.to_string(),
                                },
                            ),
                        }
                        emit_rate(&ev_tx, &client);
                    }
                    Some(SyncCmd::Refresh { owner, repo, number, item_id }) => {
                        emit(
                            &ev_tx,
                            SyncEvent::Started {
                                kind: SyncKind::RefreshItem,
                                message: format!("refresh {owner}/{repo}#{number}"),
                            },
                        );
                        match refresh_one(&db, &cfg, &client, &owner, &repo, number).await {
                            Ok(_) => emit(
                                &ev_tx,
                                SyncEvent::Finished {
                                    kind: SyncKind::RefreshItem,
                                    fetched: 1,
                                    upserted: 1,
                                    unread: 0,
                                    message: format!("refreshed {owner}/{repo}#{number}"),
                                },
                            ),
                            Err(e) => emit(
                                &ev_tx,
                                SyncEvent::Failed {
                                    kind: SyncKind::RefreshItem,
                                    error: e.to_string(),
                                },
                            ),
                        }
                        let _ = item_id;
                    }
                    Some(SyncCmd::Comments { owner, repo, number, item_id }) => {
                        emit(
                            &ev_tx,
                            SyncEvent::Started {
                                kind: SyncKind::Comments,
                                message: format!("comments {owner}/{repo}#{number}"),
                            },
                        );
                        match load_comments(&db, &client, &owner, &repo, number, item_id).await {
                            Ok(n) => emit(
                                &ev_tx,
                                SyncEvent::Finished {
                                    kind: SyncKind::Comments,
                                    fetched: 1,
                                    upserted: n,
                                    unread: 0,
                                    message: format!("{n} comments"),
                                },
                            ),
                            Err(e) => emit(
                                &ev_tx,
                                SyncEvent::Failed {
                                    kind: SyncKind::Comments,
                                    error: e.to_string(),
                                },
                            ),
                        }
                    }
                }
            }
            _ = interval.tick() => {
                let _ = poll_once(&db, &cfg, &client, &ev_tx).await;
            }
        }
    }
}

async fn poll_once(
    db: &Db,
    cfg: &Config,
    client: &GhClient,
    ev: &std::sync::mpsc::Sender<SyncEvent>,
) -> Result<SyncReport> {
    emit(
        ev,
        SyncEvent::Started {
            kind: SyncKind::Poll,
            message: "checking notifications".into(),
        },
    );
    match incremental(db, cfg, client, ev).await {
        Ok(r) => {
            emit(
                ev,
                SyncEvent::Finished {
                    kind: SyncKind::Poll,
                    fetched: r.fetched,
                    upserted: r.upserted,
                    unread: r.unread,
                    message: r.message.clone(),
                },
            );
            emit_rate(ev, client);
            Ok(r)
        }
        Err(e) => {
            emit(
                ev,
                SyncEvent::Failed {
                    kind: SyncKind::Poll,
                    error: e.to_string(),
                },
            );
            Err(e)
        }
    }
}

pub async fn run_once(db: &Db, cfg: &Config, full: bool) -> Result<SyncReport> {
    let client = GhClient::new(cfg)?;
    ensure_username(db, cfg, &client).await?;
    let (ev_tx, _ev_rx) = std::sync::mpsc::channel();
    if full || db.meta_get("backfill_done")?.as_deref() != Some("1") {
        let days = if full { None } else { Some(cfg.backfill_days) };
        search_created(db, cfg, &client, &ev_tx, days).await
    } else {
        incremental(db, cfg, &client, &ev_tx).await
    }
}

async fn ensure_username(db: &Db, cfg: &Config, client: &GhClient) -> Result<String> {
    if let Some(u) = cfg.username.clone() {
        db.meta_set("viewer_login", &u)?;
        return Ok(u);
    }
    if let Some(u) = db.meta_get("viewer_login")? {
        return Ok(u);
    }
    let login = client.viewer_login().await?;
    db.meta_set("viewer_login", &login)?;
    info!("resolved GitHub login {login}");
    Ok(login)
}

async fn search_created(
    db: &Db,
    cfg: &Config,
    client: &GhClient,
    ev: &std::sync::mpsc::Sender<SyncEvent>,
    created_days: Option<u32>,
) -> Result<SyncReport> {
    let me = ensure_username(db, cfg, client).await?;
    let since = created_days.and_then(|d| TimeRange::Days(d).github_since_date());
    let qualifier = since.as_deref().map(|d| ("created", d));
    let queries = search_queries(&me, qualifier);
    let mut fetched = 0u32;
    let mut upserted = 0u32;

    for (qi, (query, hint)) in queries.iter().enumerate() {
        emit(
            ev,
            SyncEvent::Progress {
                message: format!("search {}/{}: {query}", qi + 1, queries.len()),
            },
        );
        let mut after: Option<String> = None;
        loop {
            let page = client.search(query, after.as_deref()).await?;
            fetched += 1;
            for item in &page.items {
                if !repo_allowed(cfg, &item.owner, &item.repo) {
                    continue;
                }
                persist(db, item, &me, *hint, None, false)?;
                upserted += 1;
            }
            emit(
                ev,
                SyncEvent::Progress {
                    message: format!(
                        "search {}/{} · {} items this page · {} total",
                        qi + 1,
                        queries.len(),
                        page.items.len(),
                        upserted
                    ),
                },
            );
            if !page.has_next {
                break;
            }
            after = page.cursor;
            // Search is the scarce quota (30/min). Stay polite between pages.
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
    }

    db.meta_set("backfill_done", "1")?;
    db.meta_set("last_search_at", &now_rfc3339())?;
    db.meta_set("last_poll_at", &now_rfc3339())?;

    Ok(SyncReport {
        fetched,
        upserted,
        unread: 0,
        message: match created_days {
            Some(d) => format!("synced {upserted} items created in last {d}d ({fetched} pages)"),
            None => format!("synced {upserted} items ({fetched} search pages)"),
        },
    })
}

async fn incremental(
    db: &Db,
    cfg: &Config,
    client: &GhClient,
    ev: &std::sync::mpsc::Sender<SyncEvent>,
) -> Result<SyncReport> {
    let me = ensure_username(db, cfg, client).await?;
    let snapshot_done = db.meta_get("notif_snapshot_done")?.as_deref() == Some("1");
    // First poll must list the whole inbox. `since` from a search backfill is
    // only ~10 minutes wide, so historical threads never land in Inbox.
    let since = if snapshot_done {
        db.meta_get("notif_since")?
    } else {
        None
    };
    let etag = if snapshot_done {
        db.meta_get("notif_etag")?
    } else {
        None
    };

    emit(
        ev,
        SyncEvent::Progress {
            message: if snapshot_done {
                "notifications".into()
            } else {
                "notification inbox".into()
            },
        },
    );

    let notifs = match client
        .notifications(since.as_deref(), etag.as_deref(), cfg.participating_only)
        .await
    {
        Ok(n) => n,
        Err(e) => {
            emit(
                ev,
                SyncEvent::Progress {
                    message: format!("notifications failed ({e}); falling back to search"),
                },
            );
            maybe_safety_search(db, cfg, client, ev, &me, true).await?;
            db.meta_set("last_poll_at", &now_rfc3339())?;
            return Ok(SyncReport {
                fetched: 1,
                upserted: 0,
                unread: 0,
                message: format!("notifications unavailable: {e}"),
            });
        }
    };

    let mut notifs = notifs;
    if !snapshot_done && cfg.participating_only && !notifs.not_modified && notifs.items.is_empty() {
        emit(
            ev,
            SyncEvent::Progress {
                message: "no participating notifications; fetching watched".into(),
            },
        );
        match client.notifications(None, None, false).await {
            Ok(all) => notifs = all,
            Err(e) => {
                emit(
                    ev,
                    SyncEvent::Progress {
                        message: format!("watched notifications failed ({e})"),
                    },
                );
            }
        }
    }

    if let Some(et) = &notifs.etag {
        db.meta_set("notif_etag", et)?;
    }
    if let Some(secs) = notifs.poll_interval {
        db.meta_set("poll_interval", &secs.to_string())?;
    }
    if !snapshot_done && !notifs.not_modified {
        db.meta_set("notif_snapshot_done", "1")?;
    }

    let mut fetched = 1u32;
    let mut upserted = 0u32;
    let mut unread = 0u32;

    for n in &notifs.items {
        if !repo_allowed(cfg, &n.owner, &n.repo) {
            continue;
        }
        db.upsert_notification(
            &n.id,
            n.unread,
            &n.reason,
            &n.updated_at,
            &n.subject_type,
            &n.title,
            &n.owner,
            &n.repo,
            n.number,
        )?;
    }

    if notifs.not_modified {
        let extra = refresh_stale_open(db, cfg, client, ev).await?;
        fetched += extra.0;
        upserted += extra.1;
        maybe_safety_search(db, cfg, client, ev, &me, false).await?;
        db.meta_set("last_poll_at", &now_rfc3339())?;
        return Ok(SyncReport {
            fetched,
            upserted,
            unread: 0,
            message: "no new notifications".into(),
        });
    }

    let mut refs: Vec<ItemRef> = Vec::new();
    let mut roles_by_key: std::collections::HashMap<String, (BTreeSet<Role>, bool)> =
        std::collections::HashMap::new();
    for n in &notifs.items {
        if !n.is_item() || !repo_allowed(cfg, &n.owner, &n.repo) {
            continue;
        }
        let number = n.number.unwrap();
        let key = format!("{}/{}#{}", n.owner, n.repo, number);
        let entry = roles_by_key
            .entry(key)
            .or_insert_with(|| (BTreeSet::new(), false));
        if let Some(role) = n.extra_role() {
            entry.0.insert(role);
        }
        if n.unread {
            entry.1 = true;
        }
        if !refs
            .iter()
            .any(|r| r.owner == n.owner && r.repo == n.repo && r.number == number)
        {
            if let Some(r) = n.item_ref() {
                refs.push(r);
            }
        }
    }

    emit(
        ev,
        SyncEvent::Progress {
            message: format!("{} notification subjects to hydrate", refs.len()),
        },
    );

    for chunk in refs.chunks(HYDRATE_BATCH) {
        let items = client.hydrate(chunk).await?;
        fetched += 1;
        for item in &items {
            let key = item.key();
            let (extra, mark_unread) = roles_by_key.remove(&key).unwrap_or_default();
            let id = persist(db, item, &me, None, Some(&extra), mark_unread)?;
            db.bind_notification_item(&item.owner, &item.repo, item.number, id)?;
            upserted += 1;
            if mark_unread {
                unread += 1;
            }
        }
    }

    if let Some(newest) = notifs
        .items
        .iter()
        .map(|n| n.updated_at.as_str())
        .max()
        .map(str::to_string)
    {
        db.meta_set("notif_since", &newest)?;
    }

    let extra = refresh_stale_open(db, cfg, client, ev).await?;
    fetched += extra.0;
    upserted += extra.1;
    maybe_safety_search(db, cfg, client, ev, &me, false).await?;
    db.meta_set("last_poll_at", &now_rfc3339())?;

    Ok(SyncReport {
        fetched,
        upserted,
        unread,
        message: if upserted == 0 {
            "caught up".into()
        } else {
            format!("updated {upserted} items ({unread} unread)")
        },
    })
}

async fn refresh_stale_open(
    db: &Db,
    cfg: &Config,
    client: &GhClient,
    ev: &std::sync::mpsc::Sender<SyncEvent>,
) -> Result<(u32, u32)> {
    let cutoff = (Utc::now() - ChronoDuration::hours(OPEN_REFRESH_AGE_HOURS))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let stale = db.open_stale(&cutoff, OPEN_REFRESH_BATCH)?;
    if stale.is_empty() {
        return Ok((0, 0));
    }
    emit(
        ev,
        SyncEvent::Progress {
            message: format!("refresh {} open items", stale.len()),
        },
    );
    let me = db.meta_get("viewer_login")?.unwrap_or_default();
    let refs: Vec<ItemRef> = stale
        .into_iter()
        .filter(|(o, r, _, _)| repo_allowed(cfg, o, r))
        .map(|(owner, repo, number, node_id)| ItemRef {
            owner,
            repo,
            number,
            node_id,
        })
        .collect();
    let mut fetched = 0u32;
    let mut upserted = 0u32;
    for chunk in refs.chunks(HYDRATE_BATCH) {
        let items = client.hydrate(chunk).await?;
        fetched += 1;
        for item in &items {
            persist(db, item, &me, None, None, false)?;
            upserted += 1;
        }
    }
    Ok((fetched, upserted))
}

async fn maybe_safety_search(
    db: &Db,
    cfg: &Config,
    client: &GhClient,
    ev: &std::sync::mpsc::Sender<SyncEvent>,
    me: &str,
    force: bool,
) -> Result<()> {
    if !force {
        let last = db.meta_get("last_search_at")?;
        let due = match last.as_deref().and_then(crate::timeutil::parse_rfc3339) {
            Some(t) => Utc::now() - t > ChronoDuration::hours(6),
            None => true,
        };
        if !due {
            return Ok(());
        }
    }
    let since = (Utc::now() - ChronoDuration::hours(SAFETY_SEARCH_HOURS))
        .format("%Y-%m-%d")
        .to_string();
    emit(
        ev,
        SyncEvent::Progress {
            message: format!("safety search since {since}"),
        },
    );
    // One involves query is enough as a net — reviewed-by is covered on the 6h cadence too.
    for (query, hint) in search_queries(me, Some(("updated", since.as_str()))) {
        let page = client.search(&query, None).await?;
        for item in &page.items {
            if repo_allowed(cfg, &item.owner, &item.repo) {
                persist(db, item, me, hint, None, false)?;
            }
        }
        // Don't walk every page of the safety net; first page of each query is the recent slice.
        let _ = page.has_next;
    }
    db.meta_set("last_search_at", &now_rfc3339())?;
    Ok(())
}

async fn refresh_one(
    db: &Db,
    cfg: &Config,
    client: &GhClient,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<()> {
    let me = ensure_username(db, cfg, client).await?;
    let items = client
        .hydrate(&[ItemRef {
            owner: owner.into(),
            repo: repo.into(),
            number,
            node_id: None,
        }])
        .await?;
    let item = items
        .into_iter()
        .next()
        .with_context(|| format!("{owner}/{repo}#{number} not found"))?;
    persist(db, &item, &me, None, None, false)?;
    Ok(())
}

async fn load_comments(
    db: &Db,
    client: &GhClient,
    owner: &str,
    repo: &str,
    number: i64,
    item_id: i64,
) -> Result<u32> {
    let comments = client.comments(owner, repo, number).await?;
    let n = comments.len() as u32;
    db.replace_comments(item_id, &comments)?;
    Ok(n)
}

fn persist(
    db: &Db,
    item: &HydratedItem,
    me: &str,
    search_hint: Option<Role>,
    extra: Option<&BTreeSet<Role>>,
    mark_unread: bool,
) -> Result<i64> {
    let mut roles = item.field_roles(me);
    if let Some(hint) = search_hint {
        roles.insert(hint);
    }
    if let Some(extra) = extra {
        roles.extend(extra.iter().copied());
    }
    db.upsert_item(item, &roles, mark_unread, true)
}

fn repo_allowed(cfg: &Config, owner: &str, repo: &str) -> bool {
    if cfg.allowed_repos.is_empty() {
        return true;
    }
    let key = format!("{owner}/{repo}");
    cfg.allowed_repos
        .iter()
        .any(|r| r.eq_ignore_ascii_case(&key))
}

fn emit(tx: &std::sync::mpsc::Sender<SyncEvent>, ev: SyncEvent) {
    let _ = tx.send(ev);
}

fn emit_rate(tx: &std::sync::mpsc::Sender<SyncEvent>, client: &GhClient) {
    let snap = client.rate_snapshot();
    emit(
        tx,
        SyncEvent::Rate {
            remaining: snap.graphql_remaining,
            limit: snap.graphql_limit,
        },
    );
}
