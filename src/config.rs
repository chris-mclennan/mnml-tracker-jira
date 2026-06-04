//! TOML config — read from `~/.config/mnml-tickets-jira.toml`.
//!
//! See `Config::EXAMPLE` for the default template that gets written
//! when no file exists. Each tab is either:
//!   - a literal `jql = "..."` query, or
//!   - a `mode = "current_release" | "next_release"` (optionally
//!     scoped by `project` / `component`) that gets resolved at
//!     startup against Jira's release list.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub jira_url: String,
    pub email: String,
    /// Polling interval. `0` disables auto-refresh; user can still
    /// press `r` to refresh the active tab. Default 60s.
    #[serde(default = "default_refresh")]
    pub refresh_interval_secs: u64,
    /// Tab list — at least one required.
    pub tabs: Vec<Tab>,
}

fn default_refresh() -> u64 {
    60
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tab {
    /// Human label shown in the tab strip.
    pub name: String,
    /// Mutually exclusive with `jql`: auto-resolve a fixVersion.
    #[serde(default)]
    pub mode: Option<ResolveMode>,
    /// Mutually exclusive with `mode`: literal JQL query.
    #[serde(default)]
    pub jql: Option<String>,
    /// Project key (e.g. "TE"). Required when `mode` is set.
    #[serde(default)]
    pub project: Option<String>,
    /// Component filter (e.g. "Mobile"). Optional when `mode` is set.
    #[serde(default)]
    pub component: Option<String>,
    /// Override the default column set for this tab. Useful when one
    /// tab is "Mine" and you want to see priority + reporter, while
    /// another tab is a release-tracking view where assignee + updated
    /// matter more. `None` ⇒ use the family default (key, status,
    /// assignee, updated, summary).
    #[serde(default)]
    pub columns: Option<Vec<Column>>,
}

/// One column in the issue table. Used in per-tab overrides via
/// `[[tabs]] columns = [...]`. Case is preserved on input; serde
/// expects snake-case strings (`"fix_version"`, etc.).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Key,
    Status,
    Assignee,
    Reporter,
    Priority,
    Type,
    Updated,
    FixVersion,
    Summary,
}

impl Column {
    /// The family default — what every tab gets when `columns` is unset.
    pub fn default_set() -> Vec<Column> {
        vec![
            Column::Key,
            Column::Status,
            Column::Assignee,
            Column::Updated,
            Column::Summary,
        ]
    }

    /// Header label for the column.
    pub fn header(self) -> &'static str {
        match self {
            Column::Key => "KEY",
            Column::Status => "STATUS",
            Column::Assignee => "ASSIGNEE",
            Column::Reporter => "REPORTER",
            Column::Priority => "PRIORITY",
            Column::Type => "TYPE",
            Column::Updated => "UPDATED",
            Column::FixVersion => "FIXVERSION",
            Column::Summary => "SUMMARY",
        }
    }

    /// Render width (in cells) — `None` ⇒ "fill remaining space"
    /// (used by Summary; only one such column makes sense per row).
    pub fn width(self) -> Option<u16> {
        match self {
            Column::Key => Some(10),
            Column::Status => Some(14),
            Column::Assignee => Some(20),
            Column::Reporter => Some(20),
            Column::Priority => Some(10),
            Column::Type => Some(10),
            Column::Updated => Some(12),
            Column::FixVersion => Some(14),
            Column::Summary => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResolveMode {
    /// Earliest unreleased fixVersion of `project`.
    CurrentRelease,
    /// Second-earliest unreleased fixVersion of `project` (falls
    /// back to `CurrentRelease` if there's only one).
    NextRelease,
}

impl Config {
    pub const EXAMPLE: &'static str = r##"# mnml-tickets-jira config. Edit and re-run.

# Your Atlassian-hosted Jira instance.
jira_url = "https://yourorg.atlassian.net"

# Email associated with your API token. Used as the HTTP basic-auth
# username. Generate the token at:
#   https://id.atlassian.com/manage-profile/security/api-tokens
# and save it (chmod 600) to:
#   ~/.config/mnml-tickets-jira/token
email = "you@example.com"

# Auto-refresh in seconds. 0 disables; user can still press `r`.
refresh_interval_secs = 60

# ── Tabs ─────────────────────────────────────────────────────────
# Each `[[tabs]]` entry is one tab. Either set `jql = "..."` directly
# or use `mode = "current_release" | "next_release"` with a
# `project` (and optional `component`) to auto-resolve.
#
# Tabs are switched via 1-9 keys (or click) and ordered left→right
# as listed. Edit/reorder/remove freely.

[[tabs]]
name = "Testing"
jql  = "status = Testing AND assignee = currentUser() ORDER BY updated DESC"

[[tabs]]
name = "Current"
mode = "current_release"
project = "TE"

[[tabs]]
name = "Next"
mode = "next_release"
project = "TE"

[[tabs]]
name = "Mobile"
mode = "next_release"
project = "TE"
component = "Mobile"

[[tabs]]
name = "Mine"
jql  = "reporter = currentUser() ORDER BY updated DESC"
# Per-tab column override — drop one or both list entries to use the
# default (key/status/assignee/updated/summary). Valid values:
# "key", "status", "assignee", "reporter", "priority", "type",
# "updated", "fix_version", "summary". `summary` should be last —
# it's the only column that fills remaining row width.
columns = ["key", "priority", "status", "updated", "summary"]
"##;

    pub fn validate(&self) -> Result<()> {
        if self.tabs.is_empty() {
            return Err(anyhow!("config: at least one [[tabs]] entry required"));
        }
        for (i, t) in self.tabs.iter().enumerate() {
            let label = format!("tab #{i} ({})", t.name);
            match (&t.jql, &t.mode) {
                (Some(_), None) => {}
                (None, Some(_)) => {
                    if t.project.is_none() {
                        return Err(anyhow!("{label}: mode = '...' requires project = '<KEY>'"));
                    }
                }
                (Some(_), Some(_)) => {
                    return Err(anyhow!(
                        "{label}: set exactly one of `jql` or `mode`, not both"
                    ));
                }
                (None, None) => {
                    return Err(anyhow!(
                        "{label}: set either `jql = \"...\"` or `mode = \"current_release|next_release\"`"
                    ));
                }
            }
        }
        Ok(())
    }
}

pub fn default_config_path() -> PathBuf {
    // Use `~/.config/` everywhere (including macOS) — matches what
    // the README documents and what the rest of the family TUIs do,
    // rather than the OS-default `~/Library/Application Support/`.
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("mnml-tickets-jira.toml")
}

/// Load the config from `path`. If the file doesn't exist, write the
/// example template there and return an error pointing the user at it.
pub fn load_or_init(path: &std::path::Path) -> Result<Config> {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, Config::EXAMPLE)
            .with_context(|| format!("writing example config to {}", path.display()))?;
        return Err(anyhow!(
            "no config found — wrote an example to {}.\n\
             Edit it (jira_url + email at minimum), generate an API token at\n\
               https://id.atlassian.com/manage-profile/security/api-tokens\n\
             and save the token (chmod 600) to:\n\
               {}",
            path.display(),
            crate::auth::token_path().display(),
        ));
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading config from {}", path.display()))?;
    let cfg: Config =
        toml::from_str(&raw).with_context(|| format!("parsing config from {}", path.display()))?;
    cfg.validate()?;
    Ok(cfg)
}

/// Pretty-print the resolved config + auth hints. Used by `--check`.
pub fn print_check_report(cfg: &Config, path: &std::path::Path) -> Result<()> {
    println!("config: {}", path.display());
    println!("  jira_url: {}", cfg.jira_url);
    println!("  email:    {}", cfg.email);
    println!("  refresh:  {}s", cfg.refresh_interval_secs);
    println!("  tabs:     {}", cfg.tabs.len());
    for (i, t) in cfg.tabs.iter().enumerate() {
        let kind = if let Some(m) = &t.mode {
            format!(
                "{:?}{}{}",
                m,
                t.project
                    .as_deref()
                    .map(|p| format!(" project={p}"))
                    .unwrap_or_default(),
                t.component
                    .as_deref()
                    .map(|c| format!(" component={c}"))
                    .unwrap_or_default(),
            )
        } else {
            format!("jql = {}", t.jql.as_deref().unwrap_or(""))
        };
        println!("    {}: {} → {}", i + 1, t.name, kind);
    }
    let token_path = crate::auth::token_path();
    if token_path.exists() {
        println!("token:    {} (present)", token_path.display());
    } else {
        println!("token:    {} (MISSING)", token_path.display());
        println!("  Generate at https://id.atlassian.com/manage-profile/security/api-tokens");
        println!("  Save the token to that path (chmod 600) and re-run.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses_and_validates() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).expect("example parses");
        cfg.validate().expect("example validates");
        assert_eq!(cfg.tabs.len(), 5, "example should have 5 tabs");
        assert!(cfg.tabs.iter().any(|t| t.name == "Testing"));
        assert!(
            cfg.tabs
                .iter()
                .any(|t| t.mode == Some(ResolveMode::CurrentRelease))
        );
        assert!(
            cfg.tabs
                .iter()
                .any(|t| t.mode == Some(ResolveMode::NextRelease))
        );
    }

    #[test]
    fn example_config_parses_columns_override_on_mine_tab() {
        let cfg: Config = toml::from_str(Config::EXAMPLE).unwrap();
        let mine = cfg.tabs.iter().find(|t| t.name == "Mine").unwrap();
        assert_eq!(
            mine.columns,
            Some(vec![
                Column::Key,
                Column::Priority,
                Column::Status,
                Column::Updated,
                Column::Summary,
            ])
        );
    }

    #[test]
    fn columns_default_set_is_the_five_classic_columns() {
        assert_eq!(
            Column::default_set(),
            vec![
                Column::Key,
                Column::Status,
                Column::Assignee,
                Column::Updated,
                Column::Summary,
            ]
        );
    }

    #[test]
    fn columns_summary_has_no_explicit_width() {
        assert!(Column::Summary.width().is_none());
        // Every non-summary column has a fixed width.
        for c in [
            Column::Key,
            Column::Status,
            Column::Assignee,
            Column::Reporter,
            Column::Priority,
            Column::Type,
            Column::Updated,
            Column::FixVersion,
        ] {
            assert!(c.width().is_some(), "{c:?} should have a fixed width");
        }
    }

    #[test]
    fn columns_snake_case_serde_round_trip() {
        let toml_in = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Demo"
jql = "x"
columns = ["fix_version", "summary"]
"##;
        let cfg: Config = toml::from_str(toml_in).unwrap();
        assert_eq!(
            cfg.tabs[0].columns,
            Some(vec![Column::FixVersion, Column::Summary])
        );
    }

    #[test]
    fn validate_rejects_tab_with_both_jql_and_mode() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
jql = "status = Open"
mode = "current_release"
project = "X"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_tab_with_neither_jql_nor_mode() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validate_rejects_mode_tab_missing_project() {
        let raw = r##"
jira_url = "https://x.atlassian.net"
email = "a@b.c"

[[tabs]]
name = "Bad"
mode = "current_release"
"##;
        let cfg: Config = toml::from_str(raw).unwrap();
        assert!(cfg.validate().is_err());
    }
}
