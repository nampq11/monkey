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

#[derive(Debug, Clone)]
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
    request: Request,
) -> Response {
    let body = match axum::body::to_bytes(request.into_body(), 10 * 1024 * 1024).await {
        Ok(body) => body,
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
        .and_then(|value| value.to_str().ok());

    if verify_github_signature(&state.settings.github_webhook_secret, &body, signature).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "bad signature" })),
        )
            .into_response();
    }

    let delivery_id = match headers
        .get("x-github-delivery")
        .and_then(|value| value.to_str().ok())
    {
        Some(delivery) if !delivery.is_empty() => delivery,
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
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    let payload: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "invalid json" })),
            )
                .into_response();
        }
    };

    let (owner, repo, number) = match parse_target(event_type, &payload) {
        (Some(owner), Some(repo), Some(number)) => (owner, repo, number),
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

    let sender = payload.get("sender");
    let sender_login = sender
        .and_then(|sender| sender.get("login"))
        .and_then(|login| login.as_str())
        .unwrap_or("");
    let sender_type = sender
        .and_then(|sender| sender.get("type"))
        .and_then(|sender_type| sender_type.as_str())
        .unwrap_or("");
    if (!state.settings.bot_login.is_empty()
        && sender_login.eq_ignore_ascii_case(&state.settings.bot_login))
        || sender_login.ends_with("[bot]")
        || sender_type.eq_ignore_ascii_case("bot")
    {
        return (
            StatusCode::OK,
            Json(json!({ "ok": true, "skipped": "bot-authored event" })),
        )
            .into_response();
    }

    let body_str = String::from_utf8_lossy(&body);
    let is_new = match state
        .store
        .enqueue(delivery_id, event_type, &owner, &repo, number, &body_str)
        .await
    {
        Ok(inserted) => inserted,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "detail": format!("database error: {}", error) })),
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
        "issues" | "issue_comment" | "pull_request_review"
    ) {
        return (None, None, None);
    }

    let repo_object = match payload.get("repository") {
        Some(repo_object) => repo_object,
        None => return (None, None, None),
    };
    let owner = repo_object
        .get("owner")
        .and_then(|owner_object| {
            owner_object
                .get("login")
                .or_else(|| owner_object.get("name"))
        })
        .and_then(|login| login.as_str())
        .map(str::to_string);

    let repo = repo_object
        .get("name")
        .and_then(|name| name.as_str())
        .map(str::to_string);

    let number = payload
        .get("issue")
        // review payloads carry the reviewed PR (and its number) under "pull_request"
        .or_else(|| payload.get("pull_request"))
        .or_else(|| payload.get("review"))
        .and_then(|item| item.get("number"))
        .and_then(|number_value| number_value.as_i64());

    (owner, repo, number)
}
