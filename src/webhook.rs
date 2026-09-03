use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};

use monkey_core::config::Settings;
use monkey_core::db::Store;
use monkey_core::hmac_auth::verify_github_signature;

#[derive(Clone)]
pub struct WebhookState {
    pub settings: Settings,
    pub store: Store,
}

pub fn app(state: WebhookState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/webhook/github", post(github_webhook))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn github_webhook(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    req: Request,
) -> Response {
    let body = match axum::body::to_bytes(req.into_body(), 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "failed to read body" })),
            )
                .into_response();
        }
    };

    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok());

    if verify_github_signature(&state.settings.github_webhook_secret, &body, signature).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "bad signature" })),
        )
            .into_response();
    }

    let delivery_id = match headers
        .get("x-github-delivery")
        .and_then(|v| v.to_str().ok())
    {
        Some(d) if !d.is_empty() => d,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "missing x-github-delivery" })),
            )
                .into_response();
        }
    };

    let event_type = headers
        .get("x-github-event")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "invalid json" })),
            )
                .into_response();
        }
    };

    let (owner, repo, number) = match parse_target(event_type, &payload) {
        (Some(o), Some(r), Some(n)) => (o, r, n),
        _ => {
            return (
                StatusCode::OK,
                Json(json!({ "ok": true, "skipped": "not an actionable event" })),
            )
                .into_response();
        }
    };

    let target_repo = format!("{}/{}", owner, repo);
    if !state
        .settings
        .allowlist()
        .iter()
        .any(|allowed| allowed == &target_repo)
    {
        return (
            StatusCode::OK,
            Json(json!({ "ok": true, "skipped": "repo not in allowlist" })),
        )
            .into_response();
    }

    let sender = payload
        .get("sender")
        .and_then(|sender| sender.get("login"))
        .and_then(|login| login.as_str())
        .unwrap_or("");
    let sender_type = payload
        .get("sender")
        .and_then(|sender| sender.get("type"))
        .and_then(|sender_type| sender_type.as_str())
        .unwrap_or("");
    if (!state.settings.bot_login.is_empty()
        && sender.eq_ignore_ascii_case(&state.settings.bot_login))
        || sender.ends_with("[bot]")
        || sender_type.eq_ignore_ascii_case("bot")
    {
        return (
            StatusCode::OK,
            Json(json!({ "ok": true, "skipped": "bot-authored event" })),
        )
            .into_response();
    }

    let body_str = String::from_utf8_lossy(&body);
    let is_new =
        match state
            .store
            .enqueue(delivery_id, event_type, &owner, &repo, number, &body_str)
        {
            Ok(inserted) => inserted,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "detail": format!("database error: {}", e) })),
                )
                    .into_response();
            }
        };

    (StatusCode::OK, Json(json!({ "ok": true, "new": is_new }))).into_response()
}

pub fn parse_target(
    event_type: &str,
    payload: &Value,
) -> (Option<String>, Option<String>, Option<i64>) {
    if !matches!(
        event_type,
        "issues" | "pull_request" | "issue_comment" | "pull_request_review"
    ) {
        return (None, None, None);
    }

    let repo_obj = match payload.get("repository") {
        Some(r) => r,
        None => return (None, None, None),
    };
    let owner = repo_obj
        .get("owner")
        .and_then(|o| o.get("login").or_else(|| o.get("name")))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let repo = repo_obj
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let number = payload
        .get("issue")
        .or_else(|| payload.get("pull_request"))
        .or_else(|| payload.get("review"))
        .and_then(|item| item.get("number"))
        .and_then(|n| n.as_i64());

    (owner, repo, number)
}
