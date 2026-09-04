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

/// Adapter that records which entry point the worker selected.
struct RecordingAdapter {
    runs: AtomicUsize,
    resumes: AtomicUsize,
}

impl EngineAdapter for RecordingAdapter {
    async fn run(&self, _params: RunParams<'_>) -> Result<Outcome, EngineError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(Outcome::default())
    }

    async fn resume(&self, _params: RunParams<'_>) -> Result<Outcome, EngineError> {
        self.resumes.fetch_add(1, Ordering::SeqCst);
        Ok(Outcome::default())
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

async fn wait_for_counter(counter: &AtomicUsize, expected: usize) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if counter.load(Ordering::SeqCst) >= expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!(
        "counter reached only {}, expected {}",
        counter.load(Ordering::SeqCst),
        expected
    );
}

fn pre_create_worktree(workspaces: &Path, owner: &str, repo: &str, number: i64) {
    let worktree = workspaces
        .join(format!("{}__{}__{}", owner, repo, number))
        .join("repo");
    std::fs::create_dir_all(worktree.join(".git")).unwrap();
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_event_for_issue_uses_run() {
    let workspaces = tempdir().unwrap();
    let sessions = tempdir().unwrap();
    pre_create_worktree(workspaces.path(), "acme", "widget", 1);

    let store = Store::new(workspaces.path().join("test.db")).unwrap();
    let payload = json!({
        "action": "opened",
        "issue": {"title": "Fix the crash on save", "body": "it dies"}
    })
    .to_string();
    store
        .enqueue("d1", "issues", "acme", "widget", 1, &payload)
        .await
        .unwrap();

    let adapter = Arc::new(RecordingAdapter {
        runs: AtomicUsize::new(0),
        resumes: AtomicUsize::new(0),
    });
    let worker = tokio::spawn(worker_loop(
        store.clone(),
        adapter.clone(),
        base_settings(workspaces.path(), sessions.path()),
    ));

    wait_for_counter(&adapter.runs, 1).await;
    assert_eq!(adapter.resumes.load(Ordering::SeqCst), 0);

    worker.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn follow_up_comment_resumes_existing_session() {
    let workspaces = tempdir().unwrap();
    let sessions = tempdir().unwrap();
    pre_create_worktree(workspaces.path(), "acme", "widget", 1);

    let store = Store::new(workspaces.path().join("test.db")).unwrap();

    // Seed a completed prior run for this issue, as a first triage would
    // record it via `done`.
    let issue_payload = json!({
        "action": "opened",
        "issue": {"title": "Fix the crash on save", "body": "it dies"}
    })
    .to_string();
    store
        .enqueue("seed", "issues", "acme", "widget", 1, &issue_payload)
        .await
        .unwrap();
    store.claim("seed").await.unwrap();
    let prior_session = sessions.path().join("acme__widget__1");
    store
        .done("seed", Some(&prior_session.to_string_lossy()))
        .await
        .unwrap();

    // The follow-up comment carries the new request; the worker must resume
    // the recorded session instead of starting a fresh one.
    let comment_payload = json!({
        "action": "created",
        "issue": {"title": "Fix the crash on save", "body": "it dies", "state": "open"},
        "comment": {"body": "please also handle read-only files"}
    })
    .to_string();
    store
        .enqueue("d1", "issue_comment", "acme", "widget", 1, &comment_payload)
        .await
        .unwrap();

    let adapter = Arc::new(RecordingAdapter {
        runs: AtomicUsize::new(0),
        resumes: AtomicUsize::new(0),
    });
    let worker = tokio::spawn(worker_loop(
        store.clone(),
        adapter.clone(),
        base_settings(workspaces.path(), sessions.path()),
    ));

    wait_for_counter(&adapter.resumes, 1).await;
    assert_eq!(
        adapter.runs.load(Ordering::SeqCst),
        0,
        "an issue with an existing session must not start a fresh run"
    );

    worker.abort();
}
