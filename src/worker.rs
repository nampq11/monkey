use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Semaphore};

use monkey_core::config::Settings;
use monkey_core::db::{Event, Store};
use monkey_core::dispatch::{TaskKind, classify_and_build_task};
use monkey_core::sandbox::{cleanup_workspace, ensure_workspace};
use monkey_engine::adapters::{EngineAdapter, Outcome, RunParams};
use monkey_github::gh_writeback::{RepoRef, write_back};

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
        let rows = match store.get_pending(50) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("worker failed to get pending events: {}", e);
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
            match store.claim(&row.delivery_id) {
                Ok(claimed) if claimed => {}
                _ => {
                    let mut lock = inflight.lock().await;
                    lock.remove(&key);
                    continue;
                }
            }

            let store_clone = store.clone();
            let adapter_clone = adapter.clone();
            let settings_clone = settings.clone();
            let inflight_clone = inflight.clone();
            let key_clone = key.clone();

            tokio::spawn(async move {
                let _permit = permit;
                let delivery_id = row.delivery_id.clone();
                if let Err(e) =
                    handle_event(&store_clone, &*adapter_clone, &row, &settings_clone).await
                {
                    tracing::error!("failed handling event {}: {}", delivery_id, e);
                    if let Err(fail_error) = store_clone.fail(&delivery_id) {
                        tracing::error!(
                            "failed marking event {} as failed: {}",
                            delivery_id,
                            fail_error
                        );
                    }
                }
                let mut lock = inflight_clone.lock().await;
                lock.remove(&key_clone);
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
) -> Result<(), String> {
    let payload: serde_json::Value = serde_json::from_str(&row.payload)
        .map_err(|e| format!("failed to parse payload: {}", e))?;
    let repo_ref = RepoRef {
        owner: row.owner.clone(),
        repo: row.repo.clone(),
        number: row.number,
    };

    // Classify before cloning so ignored webhook actions never start sandbox work.
    let task = match classify_and_build_task(&row.event_type, &payload) {
        Some(task) => task,
        None => {
            store
                .done(&row.delivery_id, None)
                .map_err(|e| format!("failed to mark skipped event done: {}", e))?;
            return Ok(());
        }
    };

    let workspaces_root = Path::new(&settings.workspaces_root);
    let repo_url = format!(
        "https://github.com/{}/{}.git",
        repo_ref.owner, repo_ref.repo
    );

    // 1. Sandbox checkout
    let worktree = ensure_workspace(
        workspaces_root,
        &repo_url,
        &repo_ref.owner,
        &repo_ref.repo,
        repo_ref.number,
        "main",
    )
    .map_err(|e| format!("sandbox error: {}", e))?;

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
    write_outcome(&session_dir, &outcome, task.kind)?;

    // 4. Write back to GitHub
    write_back(&outcome, task.kind, &repo_ref, store, &worktree, settings).await?;

    store
        .done(&row.delivery_id, Some(&session_dir.to_string_lossy()))
        .map_err(|e| format!("failed to mark event done: {}", e))?;

    // 5. Cleanup workspace
    cleanup_workspace(
        workspaces_root,
        &repo_ref.owner,
        &repo_ref.repo,
        repo_ref.number,
    );

    Ok(())
}

fn write_outcome(session_dir: &Path, outcome: &Outcome, kind: TaskKind) -> Result<(), String> {
    std::fs::create_dir_all(session_dir)
        .map_err(|e| format!("failed to create session artifact directory: {}", e))?;
    let artifact = json!({
        "kind": kind,
        "status": outcome.status,
        "summary": outcome.summary,
        "pr_body": outcome.pr_body,
        "comment": outcome.comment,
        "branch": outcome.branch,
    });
    let serialized = serde_json::to_string_pretty(&artifact)
        .map_err(|e| format!("failed to serialize outcome artifact: {}", e))?;
    std::fs::write(session_dir.join("outcome.json"), serialized)
        .map_err(|e| format!("failed to write outcome artifact: {}", e))
}
