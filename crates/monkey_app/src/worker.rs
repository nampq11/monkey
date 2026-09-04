use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{Mutex, Semaphore};

use monkey_core::config::Settings;
use monkey_core::db::{Event, Store, StoreError};
use monkey_core::dispatch::{TaskKind, classify_and_build_task};
use monkey_core::sandbox::{
    SandboxError, cleanup_workspace, ensure_workspace, resolve_default_branch,
};
use monkey_engine::adapters::{EngineAdapter, EngineError, Outcome, RunParams};
use monkey_github::gh_writeback::{RepoRef, write_back};
use monkey_github::host_tools::GhProxyError;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("failed to parse webhook payload: {0}")]
    Payload(#[from] serde_json::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Sandbox(#[from] SandboxError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Proxy(#[from] GhProxyError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("failed to create session artifact directory: {0}")]
    CreateDir(#[source] std::io::Error),
    #[error("failed to serialize outcome artifact: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("failed to write outcome artifact: {0}")]
    Write(#[source] std::io::Error),
}

pub async fn worker_loop<A: EngineAdapter + 'static>(
    store: Store,
    adapter: Arc<A>,
    settings: Settings,
) {
    if settings.max_concurrency == 0 {
        tracing::error!("worker cannot start with max_concurrency=0");
        return;
    }

    let inflight: Arc<Mutex<HashSet<(String, String, i64)>>> = Arc::new(Mutex::new(HashSet::new()));
    let concurrency = Arc::new(Semaphore::new(settings.max_concurrency));

    loop {
        let rows = match store.pending_events(50).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!("worker failed to get pending events: {}", error);
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };

        for row in rows {
            let permit = match concurrency.clone().try_acquire_owned() {
                Ok(permit) => permit,
                Err(_) => break,
            };
            let key = (row.owner.clone(), row.repo.clone(), row.number);
            {
                let mut lock = inflight.lock().await;
                if lock.contains(&key) {
                    continue;
                }
                lock.insert(key.clone());
            }

            // Claim the event in the DB
            match store.claim(&row.delivery_id).await {
                Ok(claimed) if claimed => {}
                Err(error) => {
                    tracing::error!(
                        "worker failed to claim event {}: {}",
                        row.delivery_id,
                        error
                    );
                    let mut lock = inflight.lock().await;
                    lock.remove(&key);
                    continue;
                }
                Ok(_) => {
                    let mut lock = inflight.lock().await;
                    lock.remove(&key);
                    continue;
                }
            }

            tokio::spawn({
                // Shadow clones for state shared across loop iterations;
                // `row` and `key` are per-event values and move directly.
                let store = store.clone();
                let adapter = adapter.clone();
                let settings = settings.clone();
                let inflight = inflight.clone();

                async move {
                    let _permit = permit;
                    let delivery_id = row.delivery_id.clone();
                    if let Err(error) = handle_event(&store, &*adapter, &row, &settings).await {
                        tracing::error!("failed handling event {}: {}", delivery_id, error);
                        if let Err(fail_error) = store.fail(&delivery_id).await {
                            tracing::error!(
                                "failed marking event {} as failed: {}",
                                delivery_id,
                                fail_error
                            );
                        }
                    }
                    inflight.lock().await.remove(&key);
                }
            });
        }

        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn handle_event<A: EngineAdapter + ?Sized>(
    store: &Store,
    adapter: &A,
    row: &Event,
    settings: &Settings,
) -> Result<(), WorkerError> {
    let payload: serde_json::Value = serde_json::from_str(&row.payload)?;
    let repo_ref = RepoRef {
        owner: row.owner.clone(),
        repo: row.repo.clone(),
        number: row.number,
    };

    // Classify before cloning so ignored webhook actions never start sandbox work.
    let task = match classify_and_build_task(&row.event_type, &payload) {
        Some(task) => task,
        None => {
            store.done(&row.delivery_id, None).await?;
            return Ok(());
        }
    };

    let workspaces_root = Path::new(&settings.workspaces_root);
    let repo_url = format!(
        "https://github.com/{}/{}.git",
        repo_ref.owner, repo_ref.repo
    );

    // 1. Sandbox checkout, based on the repository's configured default branch
    let default_branch = resolve_default_branch(
        workspaces_root,
        &repo_url,
        &repo_ref.owner,
        &repo_ref.repo,
    )
    .await?;
    let worktree = ensure_workspace(
        workspaces_root,
        &repo_url,
        &repo_ref.owner,
        &repo_ref.repo,
        repo_ref.number,
        &default_branch,
    )
    .await?;

    // 2. Drive engine adapter
    let session_dir = PathBuf::from(&settings.session_dir).join(format!(
        "{}__{}__{}",
        repo_ref.owner, repo_ref.repo, repo_ref.number
    ));
    let model = settings.models().first().cloned().unwrap_or_default();

    let params = RunParams {
        prompt: &task.prompt,
        worktree: &worktree,
        session_dir: &session_dir,
        model: &model,
        thinking: &settings.thinking,
        provider: &settings.provider,
        timeout: Duration::from_secs(3600),
    };
    let outcome = adapter.run(params).await?;

    // 3. Persist outcome
    write_outcome(&session_dir, &outcome, task.kind).await?;

    // 4. Write back to GitHub
    write_back(
        &outcome,
        task.kind,
        &repo_ref,
        store,
        &worktree,
        &default_branch,
        settings,
    )
    .await?;

    store
        .done(&row.delivery_id, Some(&session_dir.to_string_lossy()))
        .await?;

    // 5. Cleanup workspace; a cleanup failure must not fail the already
    // succeeded event, but the error stays visible in the logs.
    if let Err(error) = cleanup_workspace(
        workspaces_root,
        &repo_ref.owner,
        &repo_ref.repo,
        repo_ref.number,
    )
    .await
    {
        tracing::warn!(
            "failed to clean up workspace {}/{}#{}: {}",
            repo_ref.owner,
            repo_ref.repo,
            repo_ref.number,
            error
        );
    }

    Ok(())
}

async fn write_outcome(
    session_dir: &Path,
    outcome: &Outcome,
    kind: TaskKind,
) -> Result<(), ArtifactError> {
    tokio::fs::create_dir_all(session_dir)
        .await
        .map_err(ArtifactError::CreateDir)?;
    let artifact = json!({
        "kind": kind,
        "status": outcome.status,
        "summary": outcome.summary,
        "pr_body": outcome.pr_body,
        "comment": outcome.comment,
        "branch": outcome.branch,
    });
    let serialized = serde_json::to_string_pretty(&artifact)?;
    tokio::fs::write(session_dir.join("outcome.json"), serialized)
        .await
        .map_err(ArtifactError::Write)
}
