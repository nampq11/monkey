use regex::Regex;
use serde_json::{Value, json};
use std::path::Path;

use crate::host_tools::GHProxy;
use monkey_core::config::Settings;
use monkey_core::db::Store;
use monkey_core::dispatch::TaskKind;
use monkey_engine::adapters::Outcome;
use monkey_engine::adapters::pi::REPORT_SECTIONS;

/// Identifies the GitHub issue a write-back targets. Grouping the three
/// fields keeps them from drifting apart across function signatures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
    pub number: i64,
}

impl RepoRef {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

pub async fn write_back(
    outcome: &Outcome,
    kind: TaskKind,
    repo_ref: &RepoRef,
    store: &Store,
    worktree: &Path,
    settings: &Settings,
) -> Result<Value, String> {
    let proxy = GHProxy::new(
        &settings.gh_proxy_url,
        &settings.gh_proxy_hmac_key,
        store.clone(),
        repo_ref,
    );

    match kind {
        TaskKind::Fix => open_pr_if_gated(&proxy, outcome, repo_ref, worktree).await,
        TaskKind::Answer | TaskKind::Comment | TaskKind::Invalid => {
            let body = if !outcome.comment.is_empty() {
                &outcome.comment
            } else {
                &outcome.summary
            };
            proxy.add_issue_comment(&ensure_comment(body)).await?;
            Ok(json!({ "action": "comment", "kind": kind }))
        }
        TaskKind::Skip => Ok(json!({ "action": "none", "kind": kind })),
    }
}

pub async fn open_pr_if_gated(
    proxy: &GHProxy,
    outcome: &Outcome,
    repo_ref: &RepoRef,
    worktree: &Path,
) -> Result<Value, String> {
    let body = if !outcome.pr_body.is_empty() {
        &outcome.pr_body
    } else {
        &outcome.summary
    };

    if !has_required_headers(body, repo_ref.number) {
        proxy.add_issue_comment(&ensure_comment(body)).await?;
        return Ok(json!({
            "action": "comment_fallback",
            "reason": "missing_required_headers"
        }));
    }

    let branch = &outcome.branch;
    if branch.is_empty() {
        proxy.add_issue_comment(&ensure_comment(body)).await?;
        return Ok(json!({
            "action": "comment_fallback",
            "reason": "missing_branch"
        }));
    }

    // Push the worktree branch so the head ref exists on remote
    proxy.push(worktree, branch).await?;

    let first_line = outcome.summary.lines().next().unwrap_or("");
    let title = truncate_utf8(first_line, 120);

    let pr = proxy
        .open_pull_request(json!({
            "title": title,
            "head": branch,
            "base": "main",
            "body": body
        }))
        .await?;

    Ok(json!({ "action": "open_pr", "pr": pr }))
}

pub fn has_required_headers(body: &str, number: i64) -> bool {
    if !REPORT_SECTIONS.iter().all(|&s| body.contains(s)) {
        return false;
    }

    let re_str = format!(r"(?i)(?:Fixes|Closes|Resolves)\s+#{}\b", number);
    if let Ok(re) = Regex::new(&re_str) {
        re.is_match(body)
    } else {
        false
    }
}

pub fn ensure_comment(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        "_no response produced_".to_string()
    } else {
        trimmed.to_string()
    }
}

fn truncate_utf8(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    text.char_indices()
        .take_while(|(index, character)| index + character.len_utf8() <= max_bytes)
        .map(|(_, character)| character)
        .collect()
}
