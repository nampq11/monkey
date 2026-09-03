use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use monkey::gh_proxy::{GhProxyState, app};
use monkey::hmac_auth::hmac_sign;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};
use tower::ServiceExt;

const HMAC_KEY: &str = "test-gh-proxy-hmac";
const TOKEN: &str = "test-gh-proxy-token";

fn test_app() -> axum::Router {
    let state = GhProxyState::new(TOKEN.to_string(), HMAC_KEY.to_string());
    app(state)
}

fn signed_request(
    method: &str,
    uri: &str,
    body: serde_json::Value,
    bad_sig: Option<&str>,
    bad_ts: Option<&str>,
) -> Request<Body> {
    let serialized = serde_json::to_vec(&body).unwrap();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
    let sig = bad_sig
        .map(|s| s.to_string())
        .unwrap_or_else(|| hmac_sign(HMAC_KEY, &serialized));
    let ts_hdr = bad_ts.unwrap_or(&ts);

    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-monkey-sig", sig)
        .header("x-monkey-ts", ts_hdr)
        .body(Body::from(serialized))
        .unwrap()
}

#[tokio::test]
async fn test_healthz_is_unauthenticated() {
    let app = test_app();
    let req = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ok"], true);
}

#[tokio::test]
async fn test_missing_signature_is_rejected() {
    let app = test_app();
    let req = Request::builder()
        .method("POST")
        .uri("/issues/acme/widget/1/comment")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"body":"hi"}"#))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_bad_signature_is_rejected() {
    let app = test_app();
    let bad_sig = format!("sha256={}", "0".repeat(64));
    let req = signed_request(
        "POST",
        "/issues/acme/widget/1/comment",
        json!({"body": "hi"}),
        Some(&bad_sig),
        None,
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_invalid_timestamp_is_rejected() {
    let app = test_app();
    let old_ts = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        - 300)
        .to_string();
    let req = signed_request(
        "POST",
        "/issues/acme/widget/1/comment",
        json!({"body": "hi"}),
        None,
        Some(&old_ts),
    );

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_git_push_requires_repo() {
    let app = test_app();

    for missing in ["worktree", "branch", "repo"] {
        let mut map = serde_json::Map::new();
        map.insert("worktree".to_string(), json!("/tmp/wt"));
        map.insert("branch".to_string(), json!("farm/x"));
        map.insert("repo".to_string(), json!("acme/widget"));
        map.remove(missing);

        let req = signed_request(
            "POST",
            "/git/push",
            serde_json::Value::Object(map),
            None,
            None,
        );

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "missing {} should be 422",
            missing
        );
    }
}
