//! App state — what's loaded, what's selected, the resolved JQL for
//! each configured tab. The UI layer reads from this.

use crate::config::{Config, ResolveMode, Tab};
use crate::jira::{Client, Issue, IssueDetail, Transition};
use anyhow::{Context, Result};
use std::collections::HashMap;

pub struct App {
    pub cfg: Config,
    pub client: Client,
    /// One entry per `cfg.tabs`. Same order.
    pub tabs: Vec<TabState>,
    pub active_tab: usize,
    /// Toast/status line at the bottom of the screen.
    pub status: String,
    /// When true, render a right-half detail panel for the focused
    /// ticket. Toggled by `d`. Off by default — the table is wider.
    pub details_visible: bool,
    /// First-line offset into the detail body (vertical scroll within
    /// the right panel). Reset when the focused ticket changes.
    pub details_scroll: u16,
    /// Cache of `(issue.key, IssueDetail)` — populated on demand when
    /// the user focuses a ticket with the detail pane open. Survives
    /// tab switches; cleared per-key by an explicit refresh.
    pub detail_cache: HashMap<String, IssueDetail>,
    /// `Some(key)` while a detail-fetch is in flight, so we don't fire
    /// duplicate requests on rapid arrow-key navigation.
    pub detail_in_flight: Option<String>,
    /// Active client-side filter (substring match against key + summary,
    /// case-insensitive). Mode lifecycle:
    ///   `None`            → no filter; show all issues.
    ///   `Some(s)` + `editing == true` → user is typing; row count
    ///     updates live as `s` changes.
    ///   `Some(s)` + `editing == false` → filter committed; selection
    ///     navigates within the filtered subset; `n`/`N` jump matches.
    pub filter: Option<FilterState>,
    /// Status-transition overlay for the focused ticket. Opened by
    /// `t`. `Some` ⇒ greedy modal — keys go to the picker (digits to
    /// pick, ↑↓/jk to move, Enter / Esc to commit / cancel) instead
    /// of the list. Loaded lazily — `transitions` is `None` while
    /// the fetch is in flight.
    pub transition_picker: Option<TransitionPicker>,
    /// AccountId of the authenticated user, fetched once on first
    /// use (the unwatch DELETE endpoint requires it as a query
    /// param). `None` ⇒ not fetched yet; `Some(Err)` ⇒ permanent
    /// error (e.g. token revoked) — `w` no-ops with a status toast
    /// rather than retrying every keypress.
    pub my_account_id: Option<Result<String, String>>,
    /// Inline comment editor at the bottom of the detail panel. Opened
    /// by `c` when the detail panel is visible. Greedy modal — printable
    /// keys insert, Esc cancels, Ctrl+P posts. Multi-line via Enter.
    pub comment_editor: Option<CommentEditor>,
}

#[derive(Debug, Clone, Default)]
pub struct CommentEditor {
    /// Issue key the comment will be posted against (captured at open).
    pub key: String,
    pub buffer: String,
    pub cursor: usize,
    /// `Some(msg)` while posting; suppresses further key input and is
    /// displayed in the editor's status row.
    pub posting: bool,
    pub error: Option<String>,
}

/// Modal state for the `t` transition picker.
#[derive(Debug, Clone)]
pub struct TransitionPicker {
    /// Issue key the picker is bound to (captured at open time so a
    /// background list refresh / cursor move doesn't change targets
    /// mid-pick).
    pub key: String,
    /// `None` while the GET is in flight; `Some(Vec)` once loaded.
    /// Empty vec is a legitimate response — no transitions available.
    pub transitions: Option<Vec<Transition>>,
    /// Highlighted row in the picker (0-based).
    pub selected: usize,
    /// Most recent error message — surfaced inside the overlay rather
    /// than blowing away the status line.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct FilterState {
    pub buffer: String,
    pub cursor: usize,
    /// True while `/` is open and the user hasn't hit Enter / Esc yet.
    pub editing: bool,
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
            details_visible: false,
            details_scroll: 0,
            detail_cache: HashMap::new(),
            detail_in_flight: None,
            filter: None,
            transition_picker: None,
            my_account_id: None,
            comment_editor: None,
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
        // Step through `visible_indices()` rather than the raw issue
        // list so a filter doesn't strand the selection on a hidden
        // row. With no filter, visible_indices is `0..len`, so this
        // behaves identically to a raw clamp.
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let cur = self.active().selected;
        let pos = visible.iter().position(|&i| i == cur).unwrap_or(0) as isize;
        let new_pos = (pos + delta).clamp(0, visible.len() as isize - 1) as usize;
        self.active_mut().selected = visible[new_pos];
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

    /// Toggle the right-half ticket detail panel. On first show, kicks
    /// off a detail fetch for the focused ticket (if not already cached).
    pub async fn toggle_details(&mut self) {
        self.details_visible = !self.details_visible;
        self.details_scroll = 0;
        if self.details_visible {
            self.ensure_focused_detail().await;
        }
    }

    /// Issue key of the currently-focused ticket, or `None` if the
    /// active tab is empty.
    pub fn focused_key(&self) -> Option<String> {
        self.active()
            .issues
            .get(self.active().selected)
            .map(|i| i.key.clone())
    }

    /// Borrow the detail for the focused ticket, if cached.
    pub fn focused_detail(&self) -> Option<&IssueDetail> {
        let key = self
            .active()
            .issues
            .get(self.active().selected)?
            .key
            .clone();
        self.detail_cache.get(&key)
    }

    /// Fetch the focused ticket's description + comments if we don't
    /// already have them cached. No-op when the focused row is empty
    /// or another fetch is in flight.
    pub async fn ensure_focused_detail(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        if self.detail_cache.contains_key(&key) {
            return;
        }
        if self.detail_in_flight.as_deref() == Some(&key) {
            return;
        }
        self.detail_in_flight = Some(key.clone());
        match self.client.fetch_issue_detail(&key).await {
            Ok(detail) => {
                self.detail_cache.insert(key, detail);
            }
            Err(e) => {
                // Park an error placeholder so we don't refetch on
                // every key event. User-facing message in the status
                // line.
                self.status = format!("detail fetch failed for {key}: {e}");
                self.detail_cache.insert(key, IssueDetail::default());
            }
        }
        self.detail_in_flight = None;
    }

    /// Drop the cached detail for the focused ticket so the next
    /// `ensure_focused_detail` call re-fetches. Used by `r` when the
    /// detail panel is visible — the list refresh would otherwise
    /// leave stale narrative content.
    pub fn invalidate_focused_detail(&mut self) {
        if let Some(key) = self.focused_key() {
            self.detail_cache.remove(&key);
        }
    }

    /// Open the `/` filter editor. Pre-loads with whatever's already
    /// committed (so re-pressing `/` lets you refine an existing
    /// filter without retyping). Cursor at end.
    pub fn open_filter(&mut self) {
        let initial = self
            .filter
            .as_ref()
            .map(|f| f.buffer.clone())
            .unwrap_or_default();
        let cursor = initial.chars().count();
        self.filter = Some(FilterState {
            buffer: initial,
            cursor,
            editing: true,
        });
    }

    /// Close the `/` filter editor. Mode picks whether to keep what's
    /// typed (`Commit` — Enter) or drop it entirely (`Cancel` — Esc on
    /// an empty filter, or two Esc's). An empty committed buffer is
    /// treated as "no filter".
    pub fn close_filter(&mut self, mode: FilterClose) {
        let Some(state) = self.filter.as_mut() else {
            return;
        };
        match mode {
            FilterClose::Commit => {
                if state.buffer.trim().is_empty() {
                    self.filter = None;
                } else {
                    state.editing = false;
                }
            }
            FilterClose::Cancel => {
                self.filter = None;
            }
        }
        // The committed filter may have shrunk the row list; clamp
        // selection so it doesn't end up past the last visible row.
        self.clamp_selection_to_filter();
    }

    /// Push a character into the filter buffer at the cursor.
    pub fn filter_insert(&mut self, c: char) {
        if let Some(f) = self.filter.as_mut() {
            let byte = f
                .buffer
                .char_indices()
                .nth(f.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| f.buffer.len());
            f.buffer.insert(byte, c);
            f.cursor += 1;
        }
        self.clamp_selection_to_filter();
    }

    /// Delete the character before the cursor (Backspace).
    pub fn filter_backspace(&mut self) {
        if let Some(f) = self.filter.as_mut()
            && f.cursor > 0
        {
            let start = f
                .buffer
                .char_indices()
                .nth(f.cursor - 1)
                .map(|(b, _)| b)
                .unwrap_or(0);
            let end = f
                .buffer
                .char_indices()
                .nth(f.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| f.buffer.len());
            f.buffer.replace_range(start..end, "");
            f.cursor -= 1;
        }
        self.clamp_selection_to_filter();
    }

    /// Return the indices of `tab.issues` that pass the current
    /// filter, or `0..len` when there's none. Used by both the UI
    /// (to know what to render) and the keys layer (to translate
    /// selection navigation into raw `issues[]` indices).
    pub fn visible_indices(&self) -> Vec<usize> {
        let tab = self.active();
        let Some(filter) = self.filter.as_ref() else {
            return (0..tab.issues.len()).collect();
        };
        let needle = filter.buffer.to_ascii_lowercase();
        if needle.is_empty() {
            return (0..tab.issues.len()).collect();
        }
        tab.issues
            .iter()
            .enumerate()
            .filter_map(|(i, issue)| {
                let key_match = issue.key.to_ascii_lowercase().contains(&needle);
                let summary_match = issue.fields.summary.to_ascii_lowercase().contains(&needle);
                (key_match || summary_match).then_some(i)
            })
            .collect()
    }

    /// Clamp the active tab's `selected` index into the current
    /// filtered set. If the previously-selected row is filtered out,
    /// jumps to the first visible row.
    fn clamp_selection_to_filter(&mut self) {
        let visible = self.visible_indices();
        if visible.is_empty() {
            return;
        }
        let cur = self.active().selected;
        if !visible.contains(&cur) {
            self.active_mut().selected = visible[0];
        }
    }
}

/// How `close_filter` should treat the in-progress buffer.
#[derive(Debug, Clone, Copy)]
pub enum FilterClose {
    /// Enter — keep what's typed (or drop to None if empty).
    Commit,
    /// Esc — discard whatever's typed and drop the filter entirely.
    Cancel,
}

impl App {
    /// Open the `t` transition picker for the focused ticket. Fires
    /// a transitions fetch; the picker renders a spinner state until
    /// it arrives. No-op when there's no focused ticket.
    pub async fn open_transition_picker(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        self.transition_picker = Some(TransitionPicker {
            key: key.clone(),
            transitions: None,
            selected: 0,
            error: None,
        });
        match self.client.fetch_transitions(&key).await {
            Ok(list) => {
                if let Some(p) = self.transition_picker.as_mut() {
                    p.transitions = Some(list);
                }
            }
            Err(e) => {
                if let Some(p) = self.transition_picker.as_mut() {
                    p.error = Some(e.to_string());
                    p.transitions = Some(Vec::new());
                }
            }
        }
    }

    /// Close the picker without firing a transition.
    pub fn close_transition_picker(&mut self) {
        self.transition_picker = None;
    }

    /// Move the picker highlight by `delta` rows, clamped to the
    /// loaded transitions list.
    pub fn transition_picker_move(&mut self, delta: isize) {
        if let Some(p) = self.transition_picker.as_mut()
            && let Some(list) = p.transitions.as_ref()
            && !list.is_empty()
        {
            let s = p.selected as isize + delta;
            p.selected = s.clamp(0, list.len() as isize - 1) as usize;
        }
    }

    /// Jump the picker highlight to row `idx` (used for digit keys
    /// 1-9). No-op if idx is out of range.
    pub fn transition_picker_select(&mut self, idx: usize) {
        if let Some(p) = self.transition_picker.as_mut()
            && let Some(list) = p.transitions.as_ref()
            && idx < list.len()
        {
            p.selected = idx;
        }
    }

    /// Open the inline comment editor for the focused ticket. No-op
    /// unless the detail panel is visible — without it there'd be
    /// nowhere to render the editor.
    pub fn open_comment_editor(&mut self) {
        if !self.details_visible {
            return;
        }
        let Some(key) = self.focused_key() else {
            return;
        };
        self.comment_editor = Some(CommentEditor {
            key,
            buffer: String::new(),
            cursor: 0,
            posting: false,
            error: None,
        });
    }

    pub fn close_comment_editor(&mut self) {
        self.comment_editor = None;
    }

    pub fn comment_editor_insert(&mut self, c: char) {
        if let Some(e) = self.comment_editor.as_mut()
            && !e.posting
        {
            let byte = e
                .buffer
                .char_indices()
                .nth(e.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| e.buffer.len());
            e.buffer.insert(byte, c);
            e.cursor += 1;
        }
    }

    pub fn comment_editor_backspace(&mut self) {
        if let Some(e) = self.comment_editor.as_mut()
            && !e.posting
            && e.cursor > 0
        {
            let start = e
                .buffer
                .char_indices()
                .nth(e.cursor - 1)
                .map(|(b, _)| b)
                .unwrap_or(0);
            let end = e
                .buffer
                .char_indices()
                .nth(e.cursor)
                .map(|(b, _)| b)
                .unwrap_or_else(|| e.buffer.len());
            e.buffer.replace_range(start..end, "");
            e.cursor -= 1;
        }
    }

    /// POST the comment to Jira. On success drops the editor, refreshes
    /// the cached detail (so the new comment appears in the thread), and
    /// toasts. On failure surfaces the error inside the editor + leaves
    /// it open so the user can retry or copy the text out.
    pub async fn submit_comment(&mut self) {
        let Some(editor) = self.comment_editor.as_ref() else {
            return;
        };
        if editor.buffer.trim().is_empty() || editor.posting {
            return;
        }
        let key = editor.key.clone();
        let body = editor.buffer.clone();
        if let Some(e) = self.comment_editor.as_mut() {
            e.posting = true;
            e.error = None;
        }
        match self.client.post_comment(&key, &body).await {
            Ok(()) => {
                self.comment_editor = None;
                self.status = format!("commented on {key}");
                self.detail_cache.remove(&key);
                if self.details_visible {
                    self.ensure_focused_detail().await;
                }
            }
            Err(e) => {
                if let Some(ed) = self.comment_editor.as_mut() {
                    ed.posting = false;
                    ed.error = Some(e.to_string());
                }
            }
        }
    }

    /// Toggle watch state on the focused ticket. Direction is
    /// derived from `detail.watching` — needs the detail cached
    /// (force-fetches if not), so the toggle reflects the current
    /// server state. After the API call succeeds we drop the cached
    /// detail for this key so the next render shows the updated
    /// watcher count.
    pub async fn toggle_watch(&mut self) {
        let Some(key) = self.focused_key() else {
            return;
        };
        // Make sure the detail is loaded — we need `watching` to know
        // which direction to toggle.
        self.ensure_focused_detail().await;
        let was_watching = self
            .detail_cache
            .get(&key)
            .map(|d| d.watching)
            .unwrap_or(false);
        let result = if was_watching {
            // Unwatch needs the authenticated user's accountId.
            let account_id = match self.fetch_or_cached_account_id().await {
                Some(id) => id,
                None => {
                    return; // Status line already explains.
                }
            };
            self.client.unwatch_issue(&key, &account_id).await
        } else {
            self.client.watch_issue(&key).await
        };
        match result {
            Ok(()) => {
                let verb = if was_watching { "unwatched" } else { "watched" };
                self.status = format!("{verb} {key}");
                // The watch_count + isWatching on the server changed;
                // drop the cache so re-render shows fresh state.
                self.detail_cache.remove(&key);
                if self.details_visible {
                    self.ensure_focused_detail().await;
                }
            }
            Err(e) => {
                self.status = format!("watch toggle failed for {key}: {e}");
            }
        }
    }

    /// Lazy-fetch the authenticated user's accountId, caching the
    /// success / permanent-failure result on `self.my_account_id`.
    /// Returns `None` and toasts the error on failure.
    async fn fetch_or_cached_account_id(&mut self) -> Option<String> {
        if let Some(slot) = self.my_account_id.as_ref() {
            return match slot {
                Ok(id) => Some(id.clone()),
                Err(e) => {
                    self.status = format!("can't unwatch — myself fetch failed earlier: {e}");
                    None
                }
            };
        }
        match self.client.myself().await {
            Ok(id) => {
                self.my_account_id = Some(Ok(id.clone()));
                Some(id)
            }
            Err(e) => {
                let msg = e.to_string();
                self.my_account_id = Some(Err(msg.clone()));
                self.status = format!("myself fetch failed: {msg}");
                None
            }
        }
    }

    /// Commit the highlighted transition. Closes the picker on
    /// success; leaves it open with an error message on failure.
    /// Invalidates the cached detail for the issue (status changed)
    /// and re-fetches the list so the new status shows up.
    pub async fn commit_transition(&mut self) {
        let Some(p) = self.transition_picker.as_ref() else {
            return;
        };
        let Some(list) = p.transitions.as_ref() else {
            return;
        };
        let Some(transition) = list.get(p.selected) else {
            return;
        };
        let key = p.key.clone();
        let transition_id = transition.id.clone();
        let label = transition
            .to_name
            .clone()
            .unwrap_or_else(|| transition.name.clone());
        match self.client.run_transition(&key, &transition_id).await {
            Ok(()) => {
                self.transition_picker = None;
                self.status = format!("{key} → {label}");
                // Status changed server-side — drop cached detail
                // for this key, refresh the list (so the new status
                // chip in the table updates), and re-warm the
                // detail if the panel is open.
                self.detail_cache.remove(&key);
                self.refresh_active().await;
                if self.details_visible {
                    self.ensure_focused_detail().await;
                }
            }
            Err(e) => {
                if let Some(p) = self.transition_picker.as_mut() {
                    p.error = Some(e.to_string());
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jira::{Fields, Issue};

    /// Build an App skipping the async init — we don't need a real
    /// Jira client to exercise the filter logic, just `issues` +
    /// `selected` on a single tab.
    fn app_with_issues(keys_and_summaries: &[(&str, &str)]) -> App {
        let client = Client::new("https://example.atlassian.net", "x@y.z", "tok").unwrap();
        let tab = TabState {
            name: "Test".to_string(),
            jql: String::new(),
            issues: keys_and_summaries
                .iter()
                .map(|(k, s)| Issue {
                    key: k.to_string(),
                    fields: Fields {
                        summary: s.to_string(),
                        status: None,
                        assignee: None,
                        reporter: None,
                        priority: None,
                        issuetype: None,
                        updated: None,
                        created: None,
                        fix_versions: Vec::new(),
                    },
                })
                .collect(),
            selected: 0,
            last_fetched: None,
            last_error: None,
        };
        App {
            cfg: Config {
                jira_url: "https://example.atlassian.net".to_string(),
                email: "x@y.z".to_string(),
                refresh_interval_secs: 60,
                tabs: Vec::new(),
            },
            client,
            tabs: vec![tab],
            active_tab: 0,
            status: String::new(),
            details_visible: false,
            details_scroll: 0,
            detail_cache: HashMap::new(),
            detail_in_flight: None,
            filter: None,
            transition_picker: None,
            my_account_id: None,
            comment_editor: None,
        }
    }

    fn picker_with_transitions(keys: &[(&str, &str)]) -> TransitionPicker {
        let transitions = keys
            .iter()
            .map(|(id, name)| Transition {
                id: id.to_string(),
                name: name.to_string(),
                to_name: Some(name.to_string()),
            })
            .collect();
        TransitionPicker {
            key: "TE-1".to_string(),
            transitions: Some(transitions),
            selected: 0,
            error: None,
        }
    }

    #[test]
    fn visible_indices_with_no_filter_returns_all() {
        let app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        assert_eq!(app.visible_indices(), vec![0, 1]);
    }

    #[test]
    fn visible_indices_matches_summary_substring_case_insensitive() {
        let mut app = app_with_issues(&[
            ("TE-1", "Fix the bufferline"),
            ("TE-2", "AI panel margin"),
            ("TE-3", "Update README"),
        ]);
        app.filter = Some(FilterState {
            buffer: "PANEL".to_string(),
            cursor: 0,
            editing: false,
        });
        assert_eq!(app.visible_indices(), vec![1]);
    }

    #[test]
    fn visible_indices_matches_key_substring() {
        let mut app = app_with_issues(&[("TE-1234", "a"), ("TE-1235", "b"), ("XX-9", "te-trap")]);
        app.filter = Some(FilterState {
            buffer: "te-1234".to_string(),
            cursor: 0,
            editing: false,
        });
        assert_eq!(app.visible_indices(), vec![0]);
    }

    #[test]
    fn empty_filter_buffer_shows_all_issues() {
        let mut app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        app.filter = Some(FilterState {
            buffer: String::new(),
            cursor: 0,
            editing: true,
        });
        assert_eq!(app.visible_indices(), vec![0, 1]);
    }

    #[test]
    fn move_selection_skips_filtered_rows() {
        let mut app = app_with_issues(&[
            ("TE-1", "alpha"),
            ("TE-2", "hidden"),
            ("TE-3", "gamma"),
            ("TE-4", "omega"),
        ]);
        app.filter = Some(FilterState {
            buffer: "a".to_string(), // matches alpha (0), gamma (2), omega (3)
            cursor: 0,
            editing: false,
        });
        assert_eq!(app.visible_indices(), vec![0, 2, 3]);
        // Start at 0 (alpha); j → 2 (gamma) — skips 1 (hidden).
        app.move_selection(1);
        assert_eq!(app.tabs[0].selected, 2);
        // j → 3 (omega).
        app.move_selection(1);
        assert_eq!(app.tabs[0].selected, 3);
        // j at the end clamps.
        app.move_selection(1);
        assert_eq!(app.tabs[0].selected, 3);
    }

    #[test]
    fn close_filter_commit_with_empty_buffer_drops_to_none() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.close_filter(FilterClose::Commit);
        assert!(app.filter.is_none());
    }

    #[test]
    fn close_filter_commit_keeps_non_empty_buffer_committed() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.filter_insert('a');
        app.close_filter(FilterClose::Commit);
        let f = app.filter.expect("commit should keep a non-empty filter");
        assert_eq!(f.buffer, "a");
        assert!(!f.editing);
    }

    #[test]
    fn close_filter_cancel_always_drops() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.filter_insert('x');
        app.close_filter(FilterClose::Cancel);
        assert!(app.filter.is_none());
    }

    #[test]
    fn filter_insert_then_backspace_round_trips() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.open_filter();
        app.filter_insert('a');
        app.filter_insert('b');
        app.filter_insert('c');
        let f = app.filter.as_ref().unwrap();
        assert_eq!(f.buffer, "abc");
        assert_eq!(f.cursor, 3);
        app.filter_backspace();
        let f = app.filter.as_ref().unwrap();
        assert_eq!(f.buffer, "ab");
        assert_eq!(f.cursor, 2);
    }

    #[test]
    fn transition_picker_move_clamps_to_bounds() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(picker_with_transitions(&[
            ("11", "Start review"),
            ("21", "Mark blocked"),
            ("31", "Resolve"),
        ]));
        app.transition_picker_move(1);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 1);
        app.transition_picker_move(10);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 2);
        app.transition_picker_move(-100);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn transition_picker_select_jumps_to_index() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(picker_with_transitions(&[
            ("11", "Start review"),
            ("21", "Mark blocked"),
            ("31", "Resolve"),
        ]));
        app.transition_picker_select(2);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 2);
        // Out-of-range no-op.
        app.transition_picker_select(99);
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 2);
    }

    #[test]
    fn close_transition_picker_drops_the_modal() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(picker_with_transitions(&[("11", "Resolve")]));
        app.close_transition_picker();
        assert!(app.transition_picker.is_none());
    }

    #[test]
    fn transition_picker_move_with_empty_list_is_a_no_op() {
        let mut app = app_with_issues(&[("TE-1", "alpha")]);
        app.transition_picker = Some(TransitionPicker {
            key: "TE-1".to_string(),
            transitions: Some(Vec::new()),
            selected: 0,
            error: None,
        });
        app.transition_picker_move(1);
        // Stays at 0; no panic.
        assert_eq!(app.transition_picker.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn typing_into_filter_clamps_selection_to_filtered_set() {
        let mut app = app_with_issues(&[("TE-1", "alpha"), ("TE-2", "beta")]);
        app.tabs[0].selected = 1; // on "beta"
        app.open_filter();
        // Type `a` — matches both "alpha" (key TE-1) and "beta" (key
        // is TE-2, but `a` ALSO matches "alpha" not "beta", so
        // visible should be just [0]). Selection should jump to 0.
        app.filter_insert('a');
        // Wait — `beta` contains `a`. Filter is summary substring
        // match — both alpha and beta match `a`. Selection should
        // stay where it is (1) since it's still in the filtered set.
        assert_eq!(app.visible_indices(), vec![0, 1]);
        assert_eq!(app.tabs[0].selected, 1);

        // Now type `lph` (so buffer = "alph"). Only alpha matches.
        app.filter_insert('l');
        app.filter_insert('p');
        app.filter_insert('h');
        assert_eq!(app.visible_indices(), vec![0]);
        // Selection clamps from 1 (beta, no longer visible) to 0.
        assert_eq!(app.tabs[0].selected, 0);
    }
}
