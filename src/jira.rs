//! Minimal Jira REST API client — only the endpoints we need.
//!
//! Uses HTTP Basic auth with `email:api_token`. The `Client` is
//! `Clone` and cheap to copy across tasks (it holds an `Arc`-backed
//! `reqwest::Client` internally).

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    email: String,
    token: String,
}

impl Client {
    pub fn new(base_url: &str, email: &str, token: &str) -> Result<Self> {
        let base = base_url.trim_end_matches('/').to_string();
        let http = reqwest::Client::builder()
            .user_agent("mnml-tracker-jira/0.1.0")
            .build()?;
        Ok(Self {
            http,
            base,
            email: email.to_string(),
            token: token.to_string(),
        })
    }

    /// Run a JQL query. Returns up to `max_results` issues (the
    /// Jira API caps this at 100).
    pub async fn search(&self, jql: &str, max_results: u32) -> Result<Vec<Issue>> {
        // Use the older /rest/api/3/search endpoint (works on
        // Atlassian Cloud and Server). The newer /search/jql is
        // similar but cloud-only.
        let url = format!("{}/rest/api/3/search", self.base);
        let body = serde_json::json!({
            "jql": jql,
            "maxResults": max_results,
            "fields": [
                "summary", "status", "assignee", "reporter",
                "priority", "issuetype", "updated", "created",
                "fixVersions",
            ],
        });
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .context("Jira search request failed")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira search failed: {status}: {text}"));
        }
        let sr: SearchResult = resp.json().await.context("parsing Jira search response")?;
        Ok(sr.issues)
    }

    /// Fetch the unreleased versions of `project`, ordered by start
    /// date ascending (so `[0]` is the next-up release).
    pub async fn unreleased_versions(&self, project_key: &str) -> Result<Vec<ProjectVersion>> {
        let url = format!("{}/rest/api/3/project/{project_key}/versions", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching versions for project {project_key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira versions fetch failed: {status}: {text}"));
        }
        let mut versions: Vec<ProjectVersion> = resp
            .json()
            .await
            .context("parsing Jira versions response")?;
        versions.retain(|v| !v.released);
        // Sort by startDate (None last), then by name as fallback.
        versions.sort_by(
            |a, b| match (a.start_date.as_deref(), b.start_date.as_deref()) {
                (Some(x), Some(y)) => x.cmp(y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            },
        );
        Ok(versions)
    }

    /// Browser URL for a given issue key (e.g. `TE-1234`).
    pub fn issue_url(&self, key: &str) -> String {
        format!("{}/browse/{key}", self.base)
    }

    /// Fetch the workflow transitions available for `key`. Different
    /// per-issue depending on the project's workflow + the current
    /// status (Jira's workflow engine is graph-based; you can only
    /// see outgoing edges from the current node). Empty list is
    /// valid — it just means the user has no transitions available
    /// (lacks permission, or a terminal state with no outgoing edges).
    pub async fn fetch_transitions(&self, key: &str) -> Result<Vec<Transition>> {
        let url = format!("{}/rest/api/3/issue/{key}/transitions", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching transitions for {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira transitions fetch failed for {key}: {status}: {text}"
            ));
        }
        let raw: TransitionsRaw = resp
            .json()
            .await
            .with_context(|| format!("parsing transitions for {key}"))?;
        Ok(raw
            .transitions
            .into_iter()
            .map(|t| Transition {
                id: t.id,
                name: t.name,
                to_name: t.to.as_ref().map(|s| s.name.clone()),
            })
            .collect())
    }

    /// Add the authenticated user as a watcher of `key`. Jira's POST
    /// endpoint with an empty body watches as the basic-auth user, so
    /// we don't need the accountId for this direction.
    pub async fn watch_issue(&self, key: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/watchers", self.base);
        // The endpoint accepts an empty string for "current user".
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .body("\"\"")
            .send()
            .await
            .with_context(|| format!("watching {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira watch failed for {key}: {status}: {text}"));
        }
        Ok(())
    }

    /// Drop `account_id` from the watcher list of `key`. The accountId
    /// is required by the DELETE endpoint; for the authenticated-user
    /// case fetch it once via [`Self::myself`] and pass it in.
    pub async fn unwatch_issue(&self, key: &str, account_id: &str) -> Result<()> {
        let url = format!(
            "{}/rest/api/3/issue/{key}/watchers?accountId={account_id}",
            self.base
        );
        let resp = self
            .http
            .delete(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("unwatching {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira unwatch failed for {key}: {status}: {text}"));
        }
        Ok(())
    }

    /// Return the authenticated user's accountId. Required for the
    /// unwatch DELETE call; cache it once per session at the call
    /// site (App owns the cache, not the Client — keeps the Client
    /// stateless / re-runnable).
    pub async fn myself(&self) -> Result<String> {
        let url = format!("{}/rest/api/3/myself", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .context("fetching authenticated user")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Jira myself failed: {status}: {text}"));
        }
        let raw: MyselfRaw = resp.json().await.context("parsing myself response")?;
        Ok(raw.account_id)
    }

    /// Fetch users assignable to `project_key`, narrowed by `query`
    /// (Jira does the substring match server-side). Used by the `a`
    /// assignee picker — pre-fetched on first open per-project, then
    /// re-queried as the user types if more than a small page is
    /// available.
    pub async fn fetch_assignable_users(
        &self,
        project_key: &str,
        query: &str,
    ) -> Result<Vec<User>> {
        // The `query` param is the case-insensitive substring filter;
        // empty `query` returns the first page of all assignable users.
        let url = format!(
            "{}/rest/api/3/user/assignable/search?project={project_key}&query={query}&maxResults=50",
            self.base
        );
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching assignable users for {project_key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira assignable users fetch failed for {project_key}: {status}: {text}"
            ));
        }
        let users: Vec<UserWithId> = resp
            .json()
            .await
            .with_context(|| format!("parsing assignable users for {project_key}"))?;
        Ok(users
            .into_iter()
            .map(|u| User {
                display_name: u.display_name,
                account_id: u.account_id,
            })
            .collect())
    }

    /// Fetch every version of `project_key` (released + unreleased,
    /// archived skipped). Sorted by startDate desc then name — the
    /// most-recent / next-up versions show up first.
    pub async fn fetch_versions(&self, project_key: &str) -> Result<Vec<ProjectVersion>> {
        let url = format!("{}/rest/api/3/project/{project_key}/versions", self.base);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching versions for {project_key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira versions fetch failed for {project_key}: {status}: {text}"
            ));
        }
        let mut versions: Vec<ProjectVersion> = resp
            .json()
            .await
            .with_context(|| format!("parsing versions for {project_key}"))?;
        versions.retain(|v| !v.archived);
        // Sort by startDate descending (most recent first), then name.
        versions.sort_by(
            |a, b| match (a.start_date.as_deref(), b.start_date.as_deref()) {
                (Some(x), Some(y)) => y.cmp(x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.name.cmp(&b.name),
            },
        );
        Ok(versions)
    }

    /// PUT a new assignee on `key`. Empty `account_id` ⇒ unassign.
    pub async fn set_assignee(&self, key: &str, account_id: Option<&str>) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base);
        let assignee = match account_id {
            Some(id) => serde_json::json!({ "accountId": id }),
            None => serde_json::Value::Null,
        };
        let body = serde_json::json!({ "fields": { "assignee": assignee } });
        let resp = self
            .http
            .put(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("setting assignee on {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira assignee set failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// PUT a fixVersion list on `key`. Empty Vec ⇒ clear fixVersions.
    pub async fn set_fix_versions(&self, key: &str, version_names: &[String]) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}", self.base);
        let versions: Vec<serde_json::Value> = version_names
            .iter()
            .map(|n| serde_json::json!({ "name": n }))
            .collect();
        let body = serde_json::json!({ "fields": { "fixVersions": versions } });
        let resp = self
            .http
            .put(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("setting fixVersions on {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira fixVersions set failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// POST a plain-text comment to `key`. The body gets wrapped in
    /// the minimal ADF JSON the v3 API requires — one paragraph per
    /// line in `text`, blank lines become empty paragraphs.
    pub async fn post_comment(&self, key: &str, text: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/comment", self.base);
        let body = serde_json::json!({
            "body": plain_to_adf(text),
        });
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("posting comment on {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira comment post failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// Fire a workflow transition by id. Returns `Ok(())` on success
    /// (Jira returns 204 No Content on a successful transition).
    pub async fn run_transition(&self, key: &str, transition_id: &str) -> Result<()> {
        let url = format!("{}/rest/api/3/issue/{key}/transitions", self.base);
        let body = serde_json::json!({
            "transition": { "id": transition_id }
        });
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("posting transition for {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira transition failed for {key}: {status}: {text}"
            ));
        }
        Ok(())
    }

    /// Fetch a single issue's description + comments + watch state.
    /// The fields already on `Issue` (status, assignee, …) are
    /// included too so the detail view can re-read updated state
    /// without a stale pre-detail fetch.
    pub async fn fetch_issue_detail(&self, key: &str) -> Result<IssueDetail> {
        let url = format!(
            "{}/rest/api/3/issue/{key}?fields=description,comment,watches,summary,status,assignee,issuetype,priority,fixVersions,updated,reporter",
            self.base
        );
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.email, Some(&self.token))
            .header("Accept", "application/json")
            .send()
            .await
            .with_context(|| format!("fetching issue {key}"))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "Jira issue fetch failed for {key}: {status}: {text}"
            ));
        }
        let raw: IssueDetailRaw = resp
            .json()
            .await
            .with_context(|| format!("parsing detail for {key}"))?;
        let description = raw
            .fields
            .description
            .as_ref()
            .map(adf_to_text)
            .filter(|s| !s.trim().is_empty());
        let comments = raw
            .fields
            .comment
            .map(|c| {
                c.comments
                    .into_iter()
                    .map(|raw| Comment {
                        author: raw.author.as_ref().map(|u| u.display_name.clone()),
                        created: raw.created,
                        body: raw.body.as_ref().map(adf_to_text).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let watches = raw.fields.watches.unwrap_or_default();
        Ok(IssueDetail {
            description,
            comments,
            watching: watches.is_watching,
            watch_count: watches.watch_count,
        })
    }
}

/// One ticket's narrative content — description + the comment thread,
/// plus watch state. Lazy-loaded per-issue when the detail pane opens
/// or `w` is pressed.
#[derive(Debug, Clone, Default)]
pub struct IssueDetail {
    pub description: Option<String>,
    pub comments: Vec<Comment>,
    /// True when the authenticated user is currently a watcher of the
    /// issue. Drives the watcher chip + the `w` toggle direction.
    pub watching: bool,
    /// Total watcher count (including non-self). Surfaces in the
    /// detail header so the user can see whether anyone else cares.
    pub watch_count: u32,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct MyselfRaw {
    #[serde(rename = "accountId")]
    account_id: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub author: Option<String>,
    pub created: Option<String>,
    pub body: String,
}

/// One outgoing workflow edge from the issue's current status. The
/// `to_name` is the resulting status (e.g. "In Review"); `name` is
/// the *button label* the user clicks in Jira's UI (e.g. "Start review")
/// which can differ from the destination state.
#[derive(Debug, Clone)]
pub struct Transition {
    pub id: String,
    pub name: String,
    pub to_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TransitionsRaw {
    transitions: Vec<TransitionRaw>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct TransitionRaw {
    id: String,
    name: String,
    #[serde(default)]
    to: Option<NamedField>,
}

#[derive(Debug, Deserialize)]
struct IssueDetailRaw {
    fields: IssueDetailFieldsRaw,
}

#[derive(Debug, Deserialize)]
struct IssueDetailFieldsRaw {
    #[serde(default)]
    description: Option<serde_json::Value>,
    #[serde(default)]
    comment: Option<CommentListRaw>,
    #[serde(default)]
    watches: Option<WatchesRaw>,
}

#[derive(Debug, Default, Deserialize)]
struct WatchesRaw {
    #[serde(default, rename = "watchCount")]
    watch_count: u32,
    #[serde(default, rename = "isWatching")]
    is_watching: bool,
}

#[derive(Debug, Deserialize)]
struct CommentListRaw {
    #[serde(default)]
    comments: Vec<CommentRaw>,
}

#[derive(Debug, Deserialize)]
struct CommentRaw {
    #[serde(default)]
    author: Option<User>,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    body: Option<serde_json::Value>,
}

/// Plain text → minimal ADF JSON. Inverse of [`adf_to_text`]; used
/// when posting comments back to Jira. One paragraph per non-empty
/// input line; blank lines pass through as empty paragraphs so the
/// reader sees the same visual break.
pub(crate) fn plain_to_adf(text: &str) -> serde_json::Value {
    let paragraphs: Vec<serde_json::Value> = text
        .lines()
        .map(|line| {
            if line.is_empty() {
                serde_json::json!({ "type": "paragraph" })
            } else {
                serde_json::json!({
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": line }]
                })
            }
        })
        .collect();
    serde_json::json!({
        "type": "doc",
        "version": 1,
        "content": paragraphs,
    })
}

/// Atlassian Document Format → plain text. ADF is a recursive JSON
/// tree with `type` + `content` arrays + leaf `text` nodes. We walk
/// the tree, concatenate `text` values, and emit newlines for the
/// block-level types we care about (`paragraph`, `heading`, `bullet`
/// items, `code_block`). Inline formatting marks are stripped — the
/// detail pane is plain-text only in v1.
pub(crate) fn adf_to_text(v: &serde_json::Value) -> String {
    let mut out = String::new();
    walk_adf(v, &mut out);
    out
}

fn walk_adf(node: &serde_json::Value, out: &mut String) {
    if let Some(s) = node.get("text").and_then(|v| v.as_str()) {
        out.push_str(s);
    }
    if let Some(children) = node.get("content").and_then(|v| v.as_array()) {
        for child in children {
            walk_adf(child, out);
        }
    }
    let kind = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if matches!(
        kind,
        "paragraph"
            | "heading"
            | "codeBlock"
            | "blockquote"
            | "rule"
            | "listItem"
            | "bulletList"
            | "orderedList"
            | "hardBreak"
    ) && !out.ends_with('\n')
    {
        out.push('\n');
    }
}

#[derive(Debug, Deserialize)]
struct SearchResult {
    issues: Vec<Issue>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Issue {
    pub key: String,
    pub fields: Fields,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Fields {
    pub summary: String,
    #[serde(default)]
    pub status: Option<NamedField>,
    #[serde(default)]
    pub assignee: Option<User>,
    /// Parsed but not yet rendered. Will surface in the planned
    /// per-tab column override + ticket detail panel.
    #[serde(default)]
    pub reporter: Option<User>,
    #[serde(default)]
    pub priority: Option<NamedField>,
    #[serde(default)]
    pub issuetype: Option<NamedField>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    #[serde(rename = "fixVersions")]
    pub fix_versions: Vec<NamedField>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NamedField {
    pub name: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct User {
    #[serde(rename = "displayName", default)]
    pub display_name: String,
    /// Atlassian accountId — present on `/user/assignable/search` and
    /// `/myself` responses but not the abbreviated user objects that
    /// appear inside `Issue.fields.assignee` etc. Empty string ⇒ not
    /// known (e.g. a legacy email-only assignee that didn't migrate
    /// to GDPR-mode accountIds).
    #[serde(default, rename = "accountId")]
    pub account_id: String,
}

/// Same shape as User but with `accountId` deserialized as the
/// required field — the assignable-search endpoint always returns it.
#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
struct UserWithId {
    #[serde(rename = "displayName", default)]
    display_name: String,
    #[serde(rename = "accountId")]
    account_id: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct ProjectVersion {
    pub name: String,
    #[serde(default)]
    pub released: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, rename = "startDate")]
    pub start_date: Option<String>,
    /// Kept for future "release date" column / filter; not yet used.
    #[serde(default, rename = "releaseDate")]
    pub release_date: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adf_to_text_extracts_paragraph_text() {
        let doc = json!({
            "type": "doc",
            "content": [
                {
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": "hello world" }]
                }
            ]
        });
        let out = adf_to_text(&doc);
        assert_eq!(out.trim(), "hello world");
    }

    #[test]
    fn adf_to_text_joins_multiple_paragraphs_with_newlines() {
        let doc = json!({
            "type": "doc",
            "content": [
                { "type": "paragraph", "content": [{ "type": "text", "text": "first" }] },
                { "type": "paragraph", "content": [{ "type": "text", "text": "second" }] }
            ]
        });
        let out = adf_to_text(&doc);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, vec!["first", "second"]);
    }

    #[test]
    fn adf_to_text_walks_nested_marks_and_inline() {
        let doc = json!({
            "type": "paragraph",
            "content": [
                { "type": "text", "text": "bold ", "marks": [{ "type": "strong" }] },
                { "type": "text", "text": "and " },
                { "type": "text", "text": "italic", "marks": [{ "type": "em" }] }
            ]
        });
        let out = adf_to_text(&doc);
        assert_eq!(out.trim(), "bold and italic");
    }

    #[test]
    fn adf_to_text_handles_bullet_list() {
        let doc = json!({
            "type": "bulletList",
            "content": [
                {
                    "type": "listItem",
                    "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "one" }] }
                    ]
                },
                {
                    "type": "listItem",
                    "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "two" }] }
                    ]
                }
            ]
        });
        let out = adf_to_text(&doc);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines, vec!["one", "two"]);
    }

    #[test]
    fn adf_to_text_on_empty_doc_returns_empty() {
        let doc = json!({});
        assert_eq!(adf_to_text(&doc), "");
    }

    #[test]
    fn plain_to_adf_wraps_single_line() {
        let doc = plain_to_adf("hello");
        assert_eq!(doc["type"], "doc");
        assert_eq!(doc["version"], 1);
        assert_eq!(doc["content"][0]["type"], "paragraph");
        assert_eq!(doc["content"][0]["content"][0]["text"], "hello");
    }

    #[test]
    fn plain_to_adf_one_paragraph_per_line() {
        let doc = plain_to_adf("first\nsecond\nthird");
        let paragraphs = doc["content"].as_array().unwrap();
        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[0]["content"][0]["text"], "first");
        assert_eq!(paragraphs[1]["content"][0]["text"], "second");
        assert_eq!(paragraphs[2]["content"][0]["text"], "third");
    }

    #[test]
    fn plain_to_adf_blank_lines_become_empty_paragraphs() {
        let doc = plain_to_adf("a\n\nb");
        let paragraphs = doc["content"].as_array().unwrap();
        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[1]["type"], "paragraph");
        assert!(paragraphs[1].get("content").is_none());
    }

    #[test]
    fn plain_to_adf_then_adf_to_text_round_trips() {
        let original = "hello world\nsecond line\nthird";
        let doc = plain_to_adf(original);
        let back = adf_to_text(&doc);
        // adf_to_text emits a trailing newline after each block;
        // trim for comparison.
        assert_eq!(back.trim_end(), original);
    }
}
