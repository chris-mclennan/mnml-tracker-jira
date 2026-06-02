//! Auth — the Jira API token lives in
//! `~/.config/mnml-tickets-jira/token`. Generated at:
//!   https://id.atlassian.com/manage-profile/security/api-tokens
//!
//! The HTTP layer uses HTTP Basic auth with `email:token`.

use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;

/// Location of the API token file.
pub fn token_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("mnml-tickets-jira")
        .join("token")
}

/// Read the API token. Errors include a clear hint to create one.
pub fn load_token() -> Result<String> {
    let p = token_path();
    if !p.exists() {
        return Err(anyhow!(
            "missing Jira API token.\n\
             Generate one at https://id.atlassian.com/manage-profile/security/api-tokens\n\
             and save it (chmod 600) to:\n\
               {}",
            p.display(),
        ));
    }
    let raw = std::fs::read_to_string(&p)
        .with_context(|| format!("reading token from {}", p.display()))?;
    let token = raw.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("token file {} is empty", p.display()));
    }
    Ok(token)
}
