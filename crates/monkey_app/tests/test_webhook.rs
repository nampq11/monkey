use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use monkey_app::config::Settings;
use monkey_app::db::Store;
use monkey_app::hmac_auth::hmac_sign;
use monkey_app::webhook::{WebhookState, app};
use serde_json::json;
use std::sync::OnceLock;
use tempfile::tempdir;
use tower::ServiceExt;

const SECRET: &str = "test-webhook-secret";

fn setup_webhook_app() -> (axum::Router, Store, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::new(&db_path).unwrap();

    let settings = Settings {
        github_webhook_secret: SECRET.to_string(),
        bot_login: "monkey".to_string(),
        git_author_name: "monkey".to_string(),
        git_author_email: "monkey@example.com".to_string(),
        repo_allowlist: "acme/widget,foo/bar".to_string(),
        allowlist_cache: OnceLock::new(),
        model: "".to_string(),
        models_cache: OnceLock::new(),
        thinking: "medium".to_string(),
        provider: "".to_string(),
        session_dir: "/data/sessions".to_string(),
        max_concurrency: 8,
        question_autoclose_hours: 4,
        release_sentinel_enabled: false,
        release_max_rounds: 5,
        gh_proxy_url: "http://gh-proxy:8080".to_string(),
        gh_proxy_hmac_key: "key".to_string(),
        github_token: "".to_string(),
        workspaces_root: "/data/workspaces".to_string(),
    };

    let state = WebhookState {
        settings,
        store: store.clone(),
    };

    (app(state), store, dir)
}

#[tokio::test]
async fn test_webhook_healthz() {
    let (app, _, _dir) = setup_webhook_app();
    let req = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_webhook_missing_signature_returns_401() {
    let (app, _, _dir) = setup_webhook_app();
    let req = Request::builder()
        .method("POST")
        .uri("/webhook/github")
        .header("x-github-delivery", "d1")
        .header("x-github-event", "issues")
        .body(Body::from(r#"{"action":"opened"}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_webhook_valid_signature_enqueues_event() {
    let (app, store, _dir) = setup_webhook_app();

    let payload = json!({
        "action": "opened",
        "repository": {
            "name": "widget",
            "owner": { "login": "acme" }
        },
        "issue": {
            "number": 42,
            "title": "Bug in login",
            "body": "It crashes"
        }
    });

    let body_bytes = serde_json::to_vec(&payload).unwrap();
    let sig = hmac_sign(SECRET, &body_bytes);

    let req = Request::builder()
        .method("POST")
        .uri("/webhook/github")
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-hub-signature-256", sig)
        .header("x-github-delivery", "d-12345")
        .header("x-github-event", "issues")
        .body(Body::from(body_bytes))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let resp_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&resp_bytes).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["new"], true);

    let pending = store.pending_events(10).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].delivery_id, "d-12345");
    assert_eq!(pending[0].owner, "acme");
    assert_eq!(pending[0].repo, "widget");
    assert_eq!(pending[0].number, 42);
}

#[tokio::test]
async fn test_webhook_skips_configured_bot_sender() {
    let (app, store, _dir) = setup_webhook_app();
    let payload = json!({
        "action": "opened",
        "sender": {"login": "MONKEY"},
        "repository": {"name": "widget", "owner": {"login": "acme"}},
        "issue": {"number": 42, "title": "Bug", "body": "crash"}
    });
    let body = serde_json::to_vec(&payload).unwrap();
    let signature = hmac_sign(SECRET, &body);
    let request = Request::builder()
        .method("POST")
        .uri("/webhook/github")
        .header("x-hub-signature-256", signature)
        .header("x-github-delivery", "bot-delivery")
        .header("x-github-event", "issues")
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(store.pending_events(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_webhook_skips_pull_request_event() {
    let (app, store, _dir) = setup_webhook_app();
    let payload = json!({
        "action": "synchronize",
        "repository": {"name": "widget", "owner": {"login": "acme"}},
        "pull_request": {"number": 44, "title": "Fix crash"}
    });
    let body = serde_json::to_vec(&payload).unwrap();
    let signature = hmac_sign(SECRET, &body);
    let request = Request::builder()
        .method("POST")
        .uri("/webhook/github")
        .header("x-hub-signature-256", signature)
        .header("x-github-delivery", "pr-delivery")
        .header("x-github-event", "pull_request")
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(store.pending_events(10).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_webhook_skips_github_bot_sender() {
    let (app, store, _dir) = setup_webhook_app();
    let payload = json!({
        "action": "opened",
        "sender": {"login": "dependabot[bot]", "type": "Bot"},
        "repository": {"name": "widget", "owner": {"login": "acme"}},
        "issue": {"number": 43, "title": "Bug", "body": "crash"}
    });
    let body = serde_json::to_vec(&payload).unwrap();
    let signature = hmac_sign(SECRET, &body);
    let request = Request::builder()
        .method("POST")
        .uri("/webhook/github")
        .header("x-hub-signature-256", signature)
        .header("x-github-delivery", "bot-delivery-2")
        .header("x-github-event", "issues")
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(store.pending_events(10).await.unwrap().is_empty());
}
