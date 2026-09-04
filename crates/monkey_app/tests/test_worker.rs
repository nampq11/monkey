use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use monkey_app::adapters::{EngineAdapter, EngineError, Outcome, RunParams};
use monkey_app::config::Settings;
use monkey_app::db::Store;
use monkey_app::worker::worker_loop;
use serde_json::json;
use tempfile::tempdir;

/// Adapter whose `run` always panics, simulating an engine crash that
/// unwinds through the worker's spawned task.
struct PanickingAdapter {
    runs: AtomicUsize,
}

impl EngineAdapter for PanickingAdapter {
    async fn run(&self, _params: RunParams<'_>) -> Result<Outcome, EngineError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        panic!("engine crashed");
    }

    async fn resume(&self, _params: RunParams<'_>) -> Result<Outcome, EngineError> {
        Err(EngineError::Framing(
            "resume not used in this test".to_string(),
        ))
    }

    fn session_artifacts(&self, _session_dir: &Path) -> serde_json::Value {
        serde_json::Value::Null
    }
}

fn base_settings(workspaces_root: &Path, session_root: &Path) -> Settings {
    Settings {
        github_webhook_secret: "secret".to_string(),
        bot_login: "monkey".to_string(),
        git_author_name: "monkey".to_string(),
        git_author_email: "monkey@example.com".to_string(),
        repo_allowlist: "acme/widget".to_string(),
        allowlist_cache: OnceLock::new(),
        model: "m1".to_string(),
        models_cache: OnceLock::new(),
        thinking: "medium".to_string(),
        provider: "".to_string(),
        session_dir: session_root.to_string_lossy().to_string(),
        max_concurrency: 2,
        question_autoclose_hours: 4,
        release_sentinel_enabled: false,
        release_max_rounds: 5,
        gh_proxy_url: String::new(),
        gh_proxy_hmac_key: String::new(),
        github_token: String::new(),
        workspaces_root: workspaces_root.to_string_lossy().to_string(),
    }
}

async fn wait_for_runs(adapter: &PanickingAdapter, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if adapter.runs.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "adapter was only scheduled {} time(s), expected {}",
        adapter.runs.load(Ordering::SeqCst),
        expected
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn inflight_key_released_when_task_panics() {
    let workspaces = tempdir().unwrap();
    let sessions = tempdir().unwrap();

    // Pre-create the workspace so ensure_workspace short-circuits and the
    // test never shells out to git or touches the network.
    let worktree = workspaces.path().join("acme__widget__1").join("repo");
    std::fs::create_dir_all(worktree.join(".git")).unwrap();

    let store = Store::new(workspaces.path().join("test.db")).unwrap();
    let settings = base_settings(workspaces.path(), sessions.path());
    let adapter = Arc::new(PanickingAdapter {
        runs: AtomicUsize::new(0),
    });

    let payload = json!({
        "action": "opened",
        "issue": {"title": "Fix the crash on save", "body": "it dies"}
    })
    .to_string();
    store
        .enqueue("d1", "issues", "acme", "widget", 1, &payload)
        .await
        .unwrap();

    let worker_store = store.clone();
    let worker = tokio::spawn(worker_loop(worker_store, adapter.clone(), settings));

    // The first event reaches the engine, whose run panics and kills the
    // spawned task before the inflight key is removed.
    wait_for_runs(&adapter, 1).await;

    // A later event for the same issue must still be schedulable.
    store
        .enqueue("d2", "issues", "acme", "widget", 1, &payload)
        .await
        .unwrap();
    wait_for_runs(&adapter, 2).await;

    worker.abort();
}
