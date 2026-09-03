use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
};
use serde_json::{Value, json};
use std::process::Command;

use monkey_core::hmac_auth::verify_internal_signature;

const API: &str = "https://api.github.com";
const CREDENTIAL_HELPER: &str =
    "!f() { echo username=x-access-token; echo password=$GIT_TOKEN; }; f";

#[derive(Clone)]
pub struct GhProxyState {
    pub github_token: String,
    pub hmac_key: String,
    pub client: reqwest::Client,
}

impl GhProxyState {
    pub fn new(github_token: String, hmac_key: String) -> Self {
        Self {
            github_token,
            hmac_key,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }
}

pub fn app(state: GhProxyState) -> Router {
    let auth_state = state.clone();
    Router::new()
        .route("/healthz", get(healthz))
        .route(
            "/issues/{owner}/{repo}/{number}/comment",
            post(add_issue_comment),
        )
        .route("/issues/{owner}/{repo}/{number}/labels", post(add_labels))
        .route("/issues/{owner}/{repo}/{number}", patch(update_issue))
        .route("/pulls/{owner}/{repo}", post(open_pull_request))
        .route(
            "/pulls/{owner}/{repo}/{number}/comments",
            post(add_pr_review_comment),
        )
        .route("/repos/{owner}/{repo}/git/refs", post(create_ref))
        .route("/git/push", post(git_push))
        .route_layer(middleware::from_fn_with_state(auth_state, hmac_gate))
        .with_state(state)
}

async fn healthz() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn hmac_gate(State(state): State<GhProxyState>, req: Request, next: Next) -> Response {
    if req.uri().path() == "/healthz" {
        return next.run(req).await;
    }

    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "failed to read body" })),
            )
                .into_response();
        }
    };

    let sig = parts
        .headers
        .get("x-monkey-sig")
        .and_then(|v| v.to_str().ok());
    let ts = parts
        .headers
        .get("x-monkey-ts")
        .and_then(|v| v.to_str().ok());

    if verify_internal_signature(&state.hmac_key, &bytes, sig, ts, 30).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "bad signature" })),
        )
            .into_response();
    }

    let req = Request::from_parts(parts, Body::from(bytes));
    next.run(req).await
}

async fn add_issue_comment(
    State(state): State<GhProxyState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> Response {
    call_gh(
        &state,
        reqwest::Method::POST,
        &format!("/repos/{}/{}/issues/{}/comments", owner, repo, number),
        Some(body),
    )
    .await
}

async fn add_labels(
    State(state): State<GhProxyState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> Response {
    call_gh(
        &state,
        reqwest::Method::POST,
        &format!("/repos/{}/{}/issues/{}/labels", owner, repo, number),
        Some(body),
    )
    .await
}

async fn update_issue(
    State(state): State<GhProxyState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> Response {
    call_gh(
        &state,
        reqwest::Method::PATCH,
        &format!("/repos/{}/{}/issues/{}", owner, repo, number),
        Some(body),
    )
    .await
}

async fn open_pull_request(
    State(state): State<GhProxyState>,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    call_gh(
        &state,
        reqwest::Method::POST,
        &format!("/repos/{}/{}/pulls", owner, repo),
        Some(body),
    )
    .await
}

async fn add_pr_review_comment(
    State(state): State<GhProxyState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> Response {
    call_gh(
        &state,
        reqwest::Method::POST,
        &format!("/repos/{}/{}/pulls/{}/comments", owner, repo, number),
        Some(body),
    )
    .await
}

async fn create_ref(
    State(state): State<GhProxyState>,
    Path((owner, repo)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> Response {
    call_gh(
        &state,
        reqwest::Method::POST,
        &format!("/repos/{}/{}/git/refs", owner, repo),
        Some(body),
    )
    .await
}

async fn git_push(State(state): State<GhProxyState>, Json(body): Json<Value>) -> Response {
    let (Some(worktree), Some(branch), Some(repo)) = (
        body.get("worktree").and_then(|value| value.as_str()),
        body.get("branch").and_then(|value| value.as_str()),
        body.get("repo").and_then(|value| value.as_str()),
    ) else {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "detail": "worktree, branch, and repo required" })),
        )
            .into_response();
    };

    let remote = format!("https://github.com/{}.git", repo);

    let output = Command::new("git")
        .args([
            "-C",
            worktree,
            "-c",
            "credential.helper=",
            "-c",
            CREDENTIAL_HELPER,
            "push",
            "-f",
            &remote,
            &format!("HEAD:{}", branch),
        ])
        .env("GIT_TOKEN", &state.github_token)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output();

    match output {
        Ok(out) if out.status.success() => {
            (StatusCode::OK, Json(json!({ "ok": true }))).into_response()
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let redacted = redact_token(&stderr, &state.github_token);
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "detail": "git push failed",
                    "stderr": redacted
                })),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "detail": "failed to spawn git",
                "error": e.to_string()
            })),
        )
            .into_response(),
    }
}

async fn call_gh(
    state: &GhProxyState,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Response {
    let url = format!("{}{}", API, path);
    let mut req = state
        .client
        .request(method, &url)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", state.github_token),
        )
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header(header::USER_AGENT, "monkey-gh-proxy");

    if let Some(json_body) = body {
        req = req.json(&json_body);
    }

    match req.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let upstream_failed = status.is_client_error() || status.is_server_error();
            let response_text = match resp.text().await {
                Ok(text) => text,
                Err(error) => {
                    let response_status = if upstream_failed {
                        status
                    } else {
                        StatusCode::BAD_GATEWAY
                    };
                    return (
                        response_status,
                        Json(json!({
                            "error": true,
                            "status": status.as_u16(),
                            "detail": format!("failed to read GitHub response: {}", error)
                        })),
                    )
                        .into_response();
                }
            };
            let parsed_json = match serde_json::from_str::<Value>(&response_text) {
                Ok(value) => value,
                Err(error) => {
                    let response_status = if upstream_failed {
                        status
                    } else {
                        StatusCode::BAD_GATEWAY
                    };
                    return (
                        response_status,
                        Json(json!({
                            "error": true,
                            "status": status.as_u16(),
                            "detail": format!("malformed GitHub JSON response: {}", error)
                        })),
                    )
                        .into_response();
                }
            };

            if upstream_failed {
                (
                    status,
                    Json(json!({
                        "error": true,
                        "status": status.as_u16(),
                        "body": parsed_json
                    })),
                )
                    .into_response()
            } else {
                (status, Json(parsed_json)).into_response()
            }
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "detail": format!("upstream error: {}", e)
            })),
        )
            .into_response(),
    }
}

fn redact_token(text: &str, token: &str) -> String {
    if !token.is_empty() {
        text.replace(token, "[redacted]")
    } else {
        text.to_string()
    }
}
