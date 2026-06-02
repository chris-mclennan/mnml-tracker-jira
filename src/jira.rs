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
            .user_agent("mnml-tickets-jira/0.1.0")
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

#[derive(Debug, Deserialize, Clone)]
pub struct User {
    #[serde(rename = "displayName", default)]
    pub display_name: String,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct ProjectVersion {
    pub name: String,
    #[serde(default)]
    pub released: bool,
    #[serde(default, rename = "startDate")]
    pub start_date: Option<String>,
    /// Kept for future "release date" column / filter; not yet used.
    #[serde(default, rename = "releaseDate")]
    pub release_date: Option<String>,
}
