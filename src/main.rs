//! mnml-tickets-jira — terminal TUI for browsing Jira tickets, with
//! configurable per-tab JQL queries and (optionally) auto-resolved
//! release fixVersions.
//!
//! Runs standalone (ratatui + crossterm) by default. Blit mode
//! (`--blit <socket>` to be hosted by mnml/tmnl) lands in a follow-up.

mod app;
mod auth;
mod config;
mod jira;
mod keys;
mod ui;

use anyhow::Result;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "mnml-tickets-jira", version, about)]
struct Cli {
    /// Path to the config file. Defaults to
    /// `~/.config/mnml-tickets-jira.toml`.
    #[arg(long)]
    config: Option<std::path::PathBuf>,

    /// Print the resolved config + auth setup hints and exit.
    #[arg(long)]
    check: bool,
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
    ui::run(&mut app).await?;
    Ok(())
}
