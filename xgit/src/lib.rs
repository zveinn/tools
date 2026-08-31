pub mod clipboard;
pub mod config;
pub mod db;
pub mod github;
pub mod model;
pub mod refs;
pub mod sync;
pub mod timeutil;
pub mod tui;

use std::fs::OpenOptions;
use std::sync::Mutex;

use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::db::Db;

pub fn init_logging(cfg: &Config, verbose: bool) -> Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&cfg.log_path)
        .with_context(|| format!("open log {}", cfg.log_path.display()))?;
    let filter = if verbose {
        EnvFilter::new("gitsync=debug,info")
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("gitsync=info"))
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(Mutex::new(file))
        .with_ansi(false)
        .init();
    Ok(())
}

pub fn open_db(cfg: &Config) -> Result<Db> {
    Db::open(&cfg.db_path)
}

pub async fn sync_once(cfg: &Config, full: bool) -> Result<sync::SyncReport> {
    let db = open_db(cfg)?;
    sync::run_once(&db, cfg, full).await
}

pub fn print_stats(cfg: &Config) -> Result<()> {
    let db = open_db(cfg)?;
    let stats = db.stats()?;
    println!("database  {}", cfg.db_path.display());
    println!("pulls     {}", stats.prs);
    println!("issues    {}", stats.issues);
    println!("open      {}", stats.open);
    println!("unread    {}", stats.unread);
    println!("comments  {}", stats.comments);
    if let Some(v) = db.meta_get("viewer_login")? {
        println!("user      {v}");
    }
    if let Some(v) = db.meta_get("last_poll_at")? {
        println!("last poll {v}");
    }
    if let Some(v) = db.meta_get("backfill_done")? {
        println!("backfill  {v}");
    }
    Ok(())
}

pub fn run_tui(cfg: Config) -> Result<()> {
    let db = open_db(&cfg)?;
    let (ev_tx, ev_rx) = std::sync::mpsc::channel();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel();

    if cfg.has_token() && !cfg.offline {
        let db_w = db.clone();
        let cfg_w = cfg.clone();
        tokio::spawn(async move {
            crate::sync::run_worker(db_w, cfg_w, cmd_rx, ev_tx).await;
        });
    } else {
        drop(cmd_rx);
        drop(ev_tx);
    }

    crate::tui::run(db, cfg, ev_rx, cmd_tx)
}
