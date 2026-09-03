use serde_json::json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

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
    let inflight: Arc<Mutex<HashSet<(String, String, i64)>>> = Arc::new(Mutex::new(HashSet::new()));

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
                let delivery_id = row.delivery_id.clone();
                if let Err(e) =
                    handle_event(&store_clone, &*adapter_clone, &row, &settings_clone).await
                {
                    tracing::error!("failed handling event {}: {}", delivery_id, e);
                    let _ = store_clone.fail(&delivery_id);
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

    // 2. Classify and build task
    let task = match classify_and_build_task(&row.event_type, &payload) {
        Some(t) => t,
        None => {
            let _ = store.done(&row.delivery_id, None);
            return Ok(());
        }
    };

    // 3. Drive engine adapter
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

    // 4. Persist outcome
    write_outcome(&session_dir, &outcome, task.kind);

    // 5. Write back to GitHub
    let _ = write_back(&outcome, task.kind, &repo_ref, store, &worktree, settings).await;

    let _ = store.done(&row.delivery_id, Some(&session_dir.to_string_lossy()));

    // 6. Cleanup workspace
    cleanup_workspace(
        workspaces_root,
        &repo_ref.owner,
        &repo_ref.repo,
        repo_ref.number,
    );

    Ok(())
}

fn write_outcome(session_dir: &Path, outcome: &Outcome, kind: TaskKind) {
    let _ = std::fs::create_dir_all(session_dir);
    let artifact = json!({
        "kind": kind,
        "status": outcome.status,
        "summary": outcome.summary,
        "pr_body": outcome.pr_body,
        "comment": outcome.comment,
        "branch": outcome.branch,
    });
    let _ = std::fs::write(
        session_dir.join("outcome.json"),
        serde_json::to_string_pretty(&artifact).unwrap_or_default(),
    );
}
