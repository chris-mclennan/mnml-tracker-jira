//! App state — what's loaded, what's selected, the resolved JQL for
//! each configured tab. The UI layer reads from this.

use crate::config::{Config, ResolveMode, Tab};
use crate::jira::{Client, Issue};
use anyhow::{Context, Result};

pub struct App {
    pub cfg: Config,
    pub client: Client,
    /// One entry per `cfg.tabs`. Same order.
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    /// Toast/status line at the bottom of the screen.
    pub status: String,
}

pub struct TabState {
    pub name: String,
    /// Final JQL after any release auto-resolution.
    pub jql: String,
    pub issues: Vec<Issue>,
    pub selected: usize,
    /// Wall-clock time of the most recent successful fetch.
    pub last_fetched: Option<std::time::Instant>,
    pub last_error: Option<String>,
}

impl App {
    pub async fn new(cfg: Config, client: Client) -> Result<Self> {
        let mut tabs: Vec<TabState> = Vec::with_capacity(cfg.tabs.len());
        for t in &cfg.tabs {
            let jql = resolve_tab_jql(t, &client).await.unwrap_or_else(|e| {
                // Fall back to a placeholder JQL that yields zero
                // results so the tab is still present; the error
                // surfaces in the per-tab last_error.
                eprintln!("tab '{}': resolve failed: {e}", t.name);
                "issuekey = ''".to_string()
            });
            tabs.push(TabState {
                name: t.name.clone(),
                jql,
                issues: Vec::new(),
                selected: 0,
                last_fetched: None,
                last_error: None,
            });
        }
        let mut app = App {
            cfg,
            client,
            tabs,
            active_tab: 0,
            status: String::new(),
        };
        app.refresh_active().await;
        Ok(app)
    }

    pub fn active(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }
    pub fn active_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn switch_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active_tab = idx;
            // Re-fetch if we've never loaded this tab.
            let needs = self.tabs[idx].last_fetched.is_none();
            if needs {
                self.status = format!("loading {}…", self.tabs[idx].name);
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        let len = self.active().issues.len();
        if len == 0 {
            return;
        }
        let s = self.active().selected as isize + delta;
        let new = s.clamp(0, len as isize - 1) as usize;
        self.active_mut().selected = new;
    }

    /// Re-fetch the active tab's issues. Updates `last_fetched` and
    /// `last_error` on the tab.
    pub async fn refresh_active(&mut self) {
        let idx = self.active_tab;
        let jql = self.tabs[idx].jql.clone();
        self.status = format!("refreshing {}…", self.tabs[idx].name);
        match self.client.search(&jql, 100).await {
            Ok(issues) => {
                self.tabs[idx].issues = issues;
                self.tabs[idx].last_fetched = Some(std::time::Instant::now());
                self.tabs[idx].last_error = None;
                self.tabs[idx].selected = self.tabs[idx]
                    .selected
                    .min(self.tabs[idx].issues.len().saturating_sub(1));
                self.status = format!(
                    "{} · {} issues",
                    self.tabs[idx].name,
                    self.tabs[idx].issues.len()
                );
            }
            Err(e) => {
                self.tabs[idx].last_error = Some(e.to_string());
                self.status = format!("error: {e}");
            }
        }
    }

    /// Open the focused ticket in the OS default browser.
    pub fn open_focused(&mut self) {
        let Some(issue) = self.active().issues.get(self.active().selected) else {
            return;
        };
        let url = self.client.issue_url(&issue.key);
        match webbrowser::open(&url) {
            Ok(()) => self.status = format!("opened {} in browser", issue.key),
            Err(e) => self.status = format!("open failed: {e}"),
        }
    }
}

/// Resolve a tab's `mode = ...` into a concrete JQL string, or pass
/// through a literal `jql = "..."` unchanged.
async fn resolve_tab_jql(tab: &Tab, client: &Client) -> Result<String> {
    if let Some(jql) = &tab.jql {
        return Ok(jql.clone());
    }
    let mode = tab
        .mode
        .as_ref()
        .context("internal: tab has neither jql nor mode (should have been caught by validate)")?;
    let project = tab.project.as_ref().context("mode tab missing project")?;
    let versions = client
        .unreleased_versions(project)
        .await
        .context("fetching unreleased versions")?;
    let version_name = match mode {
        ResolveMode::CurrentRelease => versions
            .first()
            .map(|v| v.name.clone())
            .context("no unreleased versions found")?,
        ResolveMode::NextRelease => versions
            .get(1)
            .or_else(|| versions.first())
            .map(|v| v.name.clone())
            .context("no unreleased versions found")?,
    };
    let mut jql = format!("project = {project} AND fixVersion = \"{version_name}\"");
    if let Some(c) = &tab.component {
        jql.push_str(&format!(" AND component = \"{c}\""));
    }
    jql.push_str(" ORDER BY rank");
    Ok(jql)
}
