use anyhow::Result;
use clap::{Parser, Subcommand};

use gitsync::config::Config;

#[derive(Parser, Debug)]
#[command(
    name = "gitsync",
    about = "Local-first GitHub issues and pull requests TUI",
    version
)]
struct Cli {
    /// Open the TUI without talking to GitHub
    #[arg(long)]
    offline: bool,

    /// Verbose file logging (~/.gitsync/gitsync.log)
    #[arg(short, long)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Sync once and print a summary (no TUI)
    Sync {
        /// Force a full search backfill instead of the notification poll
        #[arg(long)]
        full: bool,
    },
    /// Print local database counts
    Stats,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load(cli.offline)?;
    gitsync::init_logging(&cfg, cli.verbose)?;

    match cli.command {
        None => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let _guard = rt.enter();
            gitsync::run_tui(cfg)
        }
        Some(Command::Sync { full }) => {
            if !cfg.has_token() {
                anyhow::bail!(
                    "a GitHub token is required for sync.\nSet GITHUB_TOKEN, or add token = \"...\" to ~/.gitsync/config.toml"
                );
            }
            let rt = tokio::runtime::Runtime::new()?;
            let report = rt.block_on(gitsync::sync_once(&cfg, full))?;
            println!("{}", report.message);
            println!(
                "fetched {}  upserted {}  unread {}",
                report.fetched, report.upserted, report.unread
            );
            Ok(())
        }
        Some(Command::Stats) => gitsync::print_stats(&cfg),
    }
}
