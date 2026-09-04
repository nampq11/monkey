use serde_json::Value;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use monkey_core::config::Settings;
use monkey_core::db::{Store, StoreError};
use monkey_github::gh_writeback::RepoRef;
use monkey_github::host_tools::{GHProxy, GhProxyError};

// A four-hour window does not need sub-minute precision, but polling too
// rarely would let a whole backlog wait behind a single slow tick.
const POLL_INTERVAL: Duration = Duration::from_secs(60);
const BATCH_LIMIT: usize = 50;

/// What the auto-close pass decided to do with a due question issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoCloseOutcome {
    Closed,
    /// The issue author reacted with a downvote, which vetoes the close.
    AuthorVetoed,
}

pub async fn autoclose_loop(store: Store, settings: Settings) {
    loop {
        match process_due_closings(&store, &settings).await {
            Ok(0) => {}
            Ok(processed) => tracing::info!("processed {processed} question auto-close(s)"),
            Err(error) => tracing::error!("question auto-close pass failed: {}", error),
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Closes every question issue whose auto-close window has elapsed.
///
/// Returns how many scheduled closings were handled. A GitHub failure on one
/// issue is logged and left pending so the next pass retries it; it does not
/// abort the remaining issues in the batch.
pub async fn process_due_closings(
    store: &Store,
    settings: &Settings,
) -> Result<usize, StorePassError> {
    let due = store.due_autocloses(BATCH_LIMIT).await?;

    let mut processed = 0;
    for entry in due {
        let repo_ref = RepoRef {
            owner: entry.owner.clone(),
            repo: entry.repo.clone(),
            number: entry.number,
        };

        let outcome =
            match close_question_issue(store, settings, &repo_ref, &entry.author_login).await {
                Ok(outcome) => outcome,
                Err(error) => {
                    tracing::error!(
                        "failed to auto-close {}/{}#{}: {}",
                        entry.owner,
                        entry.repo,
                        entry.number,
                        error
                    );
                    continue;
                }
            };

        store
            .complete_autoclose(&entry.owner, &entry.repo, entry.number)
            .await?;

        match outcome {
            AutoCloseOutcome::Closed => {
                tracing::info!(
                    "auto-closed {}/{}#{}",
                    entry.owner,
                    entry.repo,
                    entry.number
                )
            }
            AutoCloseOutcome::AuthorVetoed => tracing::info!(
                "author {} vetoed the auto-close of {}/{}#{}",
                entry.author_login,
                entry.owner,
                entry.repo,
                entry.number
            ),
        }

        processed += 1;
    }

    Ok(processed)
}

/// Schedules the auto-close of a `question` issue at now + configured hours.
///
/// The author login comes from the webhook payload rather than a GitHub lookup,
/// which is what keeps the downvote check at close time down to one API call.
/// Without it the downvote veto could not be evaluated at all, so an issue
/// whose author is unknown is deliberately left open.
pub async fn schedule_question_autoclose(
    store: &Store,
    repo_ref: &RepoRef,
    payload: &Value,
    settings: &Settings,
) -> Result<(), StoreError> {
    let Some(author_login) = issue_author_login(payload) else {
        tracing::warn!(
            "not scheduling an auto-close for {}: the payload carries no issue author",
            repo_ref.slug()
        );
        return Ok(());
    };

    let close_at = now_secs() + (settings.question_autoclose_hours as f64) * 3600.0;
    store
        .schedule_autoclose(
            &repo_ref.owner,
            &repo_ref.repo,
            repo_ref.number,
            author_login,
            close_at,
        )
        .await?;
    Ok(())
}

/// GitHub's downvote reaction is `content: "-1"` from the issue author; any
/// other reaction, or one from anybody else, leaves the close in place.
pub fn author_downvoted(reactions: &Value, author_login: &str) -> bool {
    let Some(reactions) = reactions.as_array() else {
        // An empty reaction list serialises to nothing, so the proxy answers
        // with its `{"ok": true}` stub rather than an array.
        return false;
    };

    reactions.iter().any(|reaction| {
        reaction.get("content").and_then(Value::as_str) == Some("-1")
            && reaction
                .get("user")
                .and_then(|user| user.get("login"))
                .and_then(Value::as_str)
                .is_some_and(|login| login.eq_ignore_ascii_case(author_login))
    })
}

async fn close_question_issue(
    store: &Store,
    settings: &Settings,
    repo_ref: &RepoRef,
    author_login: &str,
) -> Result<AutoCloseOutcome, StorePassError> {
    // Same construction as `write_back`: all orchestrator GitHub traffic goes
    // through the HMAC-signed gh-proxy path, whatever auth mode is configured.
    let proxy = GHProxy::new(
        &settings.gh_proxy_url,
        &settings.gh_proxy_hmac_key,
        store.clone(),
        repo_ref,
    )?;

    let reactions = proxy.list_issue_reactions().await?;
    if author_downvoted(&reactions, author_login) {
        return Ok(AutoCloseOutcome::AuthorVetoed);
    }

    proxy.close_issue().await?;
    Ok(AutoCloseOutcome::Closed)
}

fn issue_author_login(payload: &Value) -> Option<&str> {
    payload
        .get("issue")
        .and_then(|issue| issue.get("user"))
        .and_then(|user| user.get("login"))
        .and_then(Value::as_str)
        .filter(|login| !login.is_empty())
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

#[derive(Debug, thiserror::Error)]
pub enum StorePassError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Proxy(#[from] GhProxyError),
}
