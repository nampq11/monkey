use regex::Regex;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::LazyLock;

use crate::host_tools::{GHProxy, GhProxyError};
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
    base_branch: &str,
) -> Result<Value, GhProxyError> {
    let proxy = GHProxy::new(
        &settings.gh_proxy_url,
        &settings.gh_proxy_hmac_key,
        store.clone(),
        repo_ref,
    )?;

    match kind {
        TaskKind::Fix => open_pr_if_gated(&proxy, outcome, repo_ref, worktree, base_branch).await,
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
    base_branch: &str,
) -> Result<Value, GhProxyError> {
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

    let title = determine_pr_title(worktree, outcome, repo_ref).await;

    let pr_result = proxy
        .open_pull_request(json!({
            "title": title,
            "head": branch,
            "base": base_branch,
            "body": body
        }))
        .await;

    let pr = match pr_result {
        Ok(pr) => pr,
        Err(GhProxyError::Status {
            ref body, status, ..
        }) if status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
            && body.contains("already exists") =>
        {
            let head_filter = format!("{}:{}", repo_ref.owner, branch);
            let mut pulls = proxy
                .list_pull_requests(Some(&head_filter), Some("open"))
                .await?;
            if pulls.as_array().is_none_or(|list| list.is_empty()) {
                pulls = proxy.list_pull_requests(Some(branch), Some("open")).await?;
            }

            if let Some(existing_pr) = pulls.as_array().and_then(|list| list.first()) {
                if let Some(pull_number) = existing_pr.get("number").and_then(Value::as_i64) {
                    tracing::info!(
                        "pull request #{} already exists for branch {}, updating title and body",
                        pull_number,
                        branch
                    );
                    proxy
                        .update_pull_request(
                            pull_number,
                            json!({
                                "title": title,
                                "body": body
                            }),
                        )
                        .await?
                } else {
                    existing_pr.clone()
                }
            } else {
                return Err(pr_result.unwrap_err());
            }
        }
        Err(error) => return Err(error),
    };

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

pub fn clean_pr_title(raw: &str) -> String {
    static CONVENTIONAL_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(?i:[a-z]+)(?:\([^\)]+\))?!?:\s*")
            .expect("conventional prefix regex compiles")
    });

    let trimmed = raw.trim();
    let without_header = trimmed.trim_start_matches('#').trim();
    let without_prefix = CONVENTIONAL_PREFIX.replace(without_header, "");
    let mut cleaned = without_prefix.trim().to_string();

    while cleaned.ends_with('.')
        || cleaned.ends_with('!')
        || cleaned.ends_with('?')
        || cleaned.ends_with(';')
        || cleaned.ends_with(',')
    {
        cleaned.pop();
    }

    if let Some(first_char) = cleaned.chars().next()
        && first_char.is_lowercase()
    {
        let mut capitalized = String::new();
        let mut chars = cleaned.chars();
        if let Some(first) = chars.next() {
            for upper in first.to_uppercase() {
                capitalized.push(upper);
            }
            capitalized.extend(chars);
        }
        cleaned = capitalized;
    }

    truncate_utf8(&cleaned, 120)
}

pub async fn determine_pr_title(worktree: &Path, outcome: &Outcome, repo_ref: &RepoRef) -> String {
    if let Ok(subject) = read_commit_subject(worktree).await
        && !subject.is_empty()
    {
        let cleaned = clean_pr_title(&subject);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    for line in outcome.summary.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            let cleaned = clean_pr_title(trimmed);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }

    format!("Fix {}#{}", repo_ref.slug(), repo_ref.number)
}

async fn read_commit_subject(worktree: &Path) -> Result<String, std::io::Error> {
    let output = tokio::process::Command::new("git")
        .args([
            "-C",
            worktree.to_str().unwrap_or("."),
            "log",
            "-1",
            "--format=%s",
        ])
        .output()
        .await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Ok(String::new())
    }
}
