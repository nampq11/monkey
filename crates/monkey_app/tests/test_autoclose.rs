use std::sync::{Arc, Mutex};

use monkey_app::autoclose::{author_downvoted, process_due_closings};
use monkey_app::config::Settings;
use monkey_app::db::Store;
use serde_json::{Value, json};
use std::sync::OnceLock;
use tempfile::tempdir;

fn settings(proxy_url: &str) -> Settings {
    Settings {
        github_webhook_secret: "secret".to_string(),
        bot_login: "monkey".to_string(),
        git_author_name: "monkey".to_string(),
        git_author_email: "monkey@example.com".to_string(),
        repo_allowlist: "acme/widget".to_string(),
        allowlist_cache: OnceLock::new(),
        model: String::new(),
        models_cache: OnceLock::new(),
        thinking: "medium".to_string(),
        provider: String::new(),
        session_dir: String::new(),
        max_concurrency: 1,
        question_autoclose_hours: 4,
        release_sentinel_enabled: false,
        release_max_rounds: 5,
        gh_proxy_url: proxy_url.to_string(),
        gh_proxy_hmac_key: "hmac-key".to_string(),
        github_token: String::new(),
        workspaces_root: String::new(),
    }
}

fn reaction(content: &str, login: &str) -> Value {
    json!({ "content": content, "user": { "login": login } })
}

#[test]
fn test_only_the_author_downvote_vetoes_the_close() {
    let reactions = json!([reaction("-1", "someone-else"), reaction("+1", "reporter"),]);
    assert!(
        !author_downvoted(&reactions, "reporter"),
        "another user's downvote must not veto the close"
    );

    let reactions = json!([reaction("-1", "REPORTER")]);
    assert!(
        author_downvoted(&reactions, "reporter"),
        "GitHub logins are case-insensitive"
    );
}

#[test]
fn test_non_array_reaction_payload_is_treated_as_no_reactions() {
    // The proxy answers `{"ok": true}` when GitHub returns no body at all.
    assert!(!author_downvoted(&json!({ "ok": true }), "reporter"));
    assert!(!author_downvoted(&json!([]), "reporter"));
}

struct RecordingProxy {
    closings: Arc<Mutex<Vec<Value>>>,
    reactions: Arc<Mutex<Vec<Value>>>,
}

/// Stands in for gh-proxy: the orchestrator client hits these very paths, so
/// the test sees exactly what would be sent to GitHub.
async fn recording_proxy(
    reactions_response: Value,
    reactions_status: axum::http::StatusCode,
) -> (axum::http::Uri, RecordingProxy) {
    let closings = Arc::new(Mutex::new(Vec::new()));
    let reactions = Arc::new(Mutex::new(Vec::new()));

    let closing_clone = closings.clone();
    let reactions_clone = reactions.clone();

    let app = axum::Router::new()
        .route(
            "/issues/acme/widget/7/reactions",
            axum::routing::get(move || {
                let reactions = reactions_clone.clone();
                let response = reactions_response.clone();
                async move {
                    reactions.lock().unwrap().push(json!({}));
                    (reactions_status, axum::Json(response))
                }
            }),
        )
        .route(
            "/issues/acme/widget/7",
            axum::routing::patch({
                move |axum::Json(body): axum::Json<Value>| {
                    let closings = closing_clone.clone();
                    async move {
                        closings.lock().unwrap().push(body);
                        axum::Json(json!({ "number": 7, "state": "closed" }))
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    (
        axum::http::Uri::builder()
            .scheme("http")
            .authority(address.to_string())
            .path_and_query("/")
            .build()
            .unwrap(),
        RecordingProxy {
            closings,
            reactions,
        },
    )
}

async fn store_with_due_closing(author_login: &str) -> (tempfile::TempDir, Store) {
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("test.db")).unwrap();
    // close_at 0 makes the window overdue without depending on the clock.
    store
        .schedule_autoclose("acme", "widget", 7, author_login, 0.0)
        .await
        .unwrap();
    (dir, store)
}

#[tokio::test]
async fn test_due_question_issue_is_closed() {
    let (uri, proxy) = recording_proxy(json!([]), axum::http::StatusCode::OK).await;
    let (_dir, store) = store_with_due_closing("reporter").await;
    let settings = settings(&uri.to_string());

    let processed = process_due_closings(&store, &settings).await.unwrap();

    assert_eq!(processed, 1);
    assert_eq!(proxy.reactions.lock().unwrap().len(), 1);
    // The guard must not live across the awaits below, so the assertions that
    // need it copy what they check out of the locked section.
    let (closings, state) = {
        let closings = proxy.closings.lock().unwrap();
        (
            closings.len(),
            closings.first().map(|body| body["state"].clone()),
        )
    };
    assert_eq!(closings, 1, "the issue must be closed exactly once");
    assert_eq!(state, Some(json!("closed")));
    assert!(store.due_autocloses(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_author_downvote_skips_the_close() {
    let (uri, proxy) = recording_proxy(
        json!([reaction("-1", "reporter")]),
        axum::http::StatusCode::OK,
    )
    .await;
    let (_dir, store) = store_with_due_closing("reporter").await;
    let settings = settings(&uri.to_string());

    let processed = process_due_closings(&store, &settings).await.unwrap();

    assert_eq!(processed, 1);
    assert!(
        proxy.closings.lock().unwrap().is_empty(),
        "a vetoed issue must not be closed"
    );
    // Marked as handled either way, otherwise the vetoed issue is retried
    // forever every poll interval.
    assert!(store.due_autocloses(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_failed_reaction_lookup_leaves_the_closing_pending() {
    let (uri, proxy) =
        recording_proxy(json!({}), axum::http::StatusCode::INTERNAL_SERVER_ERROR).await;
    let (_dir, store) = store_with_due_closing("reporter").await;
    let settings = settings(&uri.to_string());

    let processed = process_due_closings(&store, &settings).await.unwrap();

    assert_eq!(processed, 0);
    assert!(
        proxy.closings.lock().unwrap().is_empty(),
        "an unreadable reaction list must not close the issue"
    );
    assert_eq!(
        store.due_autocloses(10).await.unwrap().len(),
        1,
        "a failed pass must be retried on the next tick"
    );
}

#[tokio::test]
async fn test_no_due_closing_touches_the_proxy() {
    let (uri, proxy) = recording_proxy(json!([]), axum::http::StatusCode::OK).await;
    let dir = tempdir().unwrap();
    let store = Store::new(dir.path().join("test.db")).unwrap();
    // A window that is still open must not be closed early.
    store
        .schedule_autoclose("acme", "widget", 7, "reporter", f64::MAX / 4.0)
        .await
        .unwrap();
    let settings = settings(&uri.to_string());

    assert_eq!(process_due_closings(&store, &settings).await.unwrap(), 0);
    assert!(proxy.reactions.lock().unwrap().is_empty());
    assert!(proxy.closings.lock().unwrap().is_empty());
}
