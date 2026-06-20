//! mnml-tracker-jira — terminal TUI for browsing Jira tickets, with
//! configurable per-tab JQL queries and (optionally) auto-resolved
//! release fixVersions.
//!
//! Runs standalone (ratatui + crossterm) by default. With
//! `--blit <socket>` it connects to a tmnl-protocol server (mnml's
//! `pane_host` or tmnl itself) and ships diff'd cell frames over the
//! UDS instead of writing to stdout. The data layer + drawing code
//! are identical between the two modes.

mod app;
mod auth;
mod blit;
mod config;
mod jira;
mod keys;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mnml-tracker-jira", version, about)]
struct Cli {
    /// Path to the config file. Defaults to
    /// `~/.config/mnml-tracker-jira.toml`.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Print the resolved config + auth setup hints and exit.
    #[arg(long)]
    check: bool,

    /// Run in blit-host mode: connect to the given Unix socket
    /// (a tmnl-protocol server — usually mnml's `pane_host` slot)
    /// and render cell frames over the wire instead of stdout.
    #[arg(long, value_name = "SOCKET")]
    blit: Option<std::path::PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let cfg_path = cli
        .config
        .clone()
        .unwrap_or_else(config::default_config_path);

    let cfg = config::load_or_init(&cfg_path)?;

    if cli.check {
        config::print_check_report(&cfg, &cfg_path)?;
        return Ok(());
    }

    let token = auth::load_token()?;
    let client = jira::Client::new(&cfg.jira_url, &cfg.email, &token)?;

    let mut app = app::App::new(cfg, client).await?;
    if let Some(socket) = cli.blit.as_deref() {
        blit::run(&mut app, socket).await?;
    } else {
        ui::run(&mut app).await?;
    }
    Ok(())
}
