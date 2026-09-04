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
use tokio::process::Command;

use monkey_core::hmac_auth::verify_internal_signature;

const API: &str = "https://api.github.com";
const CREDENTIAL_HELPER_CONFIG: &str =
    "credential.helper=!f() { echo username=x-access-token; echo password=$GIT_TOKEN; }; f";

#[derive(Debug, Clone)]
pub struct GhProxyState {
    pub github_token: String,
    pub hmac_key: String,
    pub client: reqwest::Client,
}

impl GhProxyState {
    pub fn new(github_token: String, hmac_key: String) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self {
            github_token,
            hmac_key,
            client,
        })
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
        .route(
            "/pulls/{owner}/{repo}",
            get(list_pull_requests).post(open_pull_request),
        )
        .route("/pulls/{owner}/{repo}/{number}", patch(update_pull_request))
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

async fn hmac_gate(State(state): State<GhProxyState>, request: Request, next: Next) -> Response {
    if request.uri().path() == "/healthz" {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let body_bytes = match axum::body::to_bytes(body, 10 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": "failed to read body" })),
            )
                .into_response();
        }
    };

    let signature = parts
        .headers
        .get("x-monkey-sig")
        .and_then(|v| v.to_str().ok());
    let timestamp = parts
        .headers
        .get("x-monkey-ts")
        .and_then(|v| v.to_str().ok());

    if verify_internal_signature(&state.hmac_key, &body_bytes, signature, timestamp, 30).is_err() {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "detail": "bad signature" })),
        )
            .into_response();
    }

    let request = Request::from_parts(parts, Body::from(body_bytes));
    next.run(request).await
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

async fn list_pull_requests(
    State(state): State<GhProxyState>,
    Path((owner, repo)): Path<(String, String)>,
    uri: axum::http::Uri,
) -> Response {
    let query_suffix = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    call_gh(
        &state,
        reqwest::Method::GET,
        &format!("/repos/{}/{}/pulls{}", owner, repo, query_suffix),
        None,
    )
    .await
}

async fn update_pull_request(
    State(state): State<GhProxyState>,
    Path((owner, repo, number)): Path<(String, String, i64)>,
    Json(body): Json<Value>,
) -> Response {
    call_gh(
        &state,
        reqwest::Method::PATCH,
        &format!("/repos/{}/{}/pulls/{}", owner, repo, number),
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
            CREDENTIAL_HELPER_CONFIG,
            "push",
            "-f",
            &remote,
            &format!("HEAD:{}", branch),
        ])
        .env("GIT_TOKEN", &state.github_token)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .await;

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
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "detail": "failed to spawn git",
                "error": error.to_string()
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
    let mut request = state
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
        request = request.json(&json_body);
    }

    match request.send().await {
        Ok(response) => {
            let status = StatusCode::from_u16(response.status().as_u16())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let upstream_failed = status.is_client_error() || status.is_server_error();
            let response_text = match response.text().await {
                Ok(text) => text,
                Err(read_error) => {
                    return github_error_response(
                        upstream_failed,
                        status,
                        format!("failed to read GitHub response: {}", read_error),
                    );
                }
            };
            proxy_response(status, response_text)
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({
                "detail": format!("upstream error: {}", error)
            })),
        )
            .into_response(),
    }
}

pub(crate) fn empty_success_value() -> Value {
    json!({ "ok": true })
}

// Maps an upstream GitHub response onto the internal proxy protocol: empty 2xx
// bodies become the shared success value, other 2xx bodies are forwarded as-is,
// and upstream failures keep their status and details.
fn proxy_response(status: StatusCode, response_text: String) -> Response {
    let upstream_failed = status.is_client_error() || status.is_server_error();
    let parsed_json = if response_text.trim().is_empty() {
        empty_success_value()
    } else {
        match serde_json::from_str::<Value>(&response_text) {
            Ok(value) => value,
            Err(decode_error) => {
                return github_error_response(
                    upstream_failed,
                    status,
                    format!("malformed GitHub JSON response: {}", decode_error),
                );
            }
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
    } else if status == StatusCode::NO_CONTENT {
        (StatusCode::OK, Json(parsed_json)).into_response()
    } else {
        (status, Json(parsed_json)).into_response()
    }
}

fn redact_token(text: &str, token: &str) -> String {
    if !token.is_empty() {
        text.replace(token, "[redacted]")
    } else {
        text.to_string()
    }
}

// Errors reading/decoding the upstream response are passed through as-is for
// upstream failures; anything else is our fault (bad gateway).
fn github_error_response(upstream_failed: bool, status: StatusCode, detail: String) -> Response {
    let response_status = if upstream_failed {
        status
    } else {
        StatusCode::BAD_GATEWAY
    };
    (
        response_status,
        Json(json!({
            "error": true,
            "status": status.as_u16(),
            "detail": detail
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::{CREDENTIAL_HELPER_CONFIG, proxy_response};
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use axum::response::Response;
    use serde_json::{Value, json};
    use std::process::Command;

    async fn status_and_body(response: Response) -> (StatusCode, Value) {
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body should be readable");
        let value =
            serde_json::from_slice(&body).expect("response body should be valid JSON");
        (status, value)
    }

    #[tokio::test]
    async fn empty_no_content_response_is_reported_as_success() {
        let response = proxy_response(StatusCode::NO_CONTENT, String::new());
        let (status, body) = status_and_body(response).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn empty_ok_response_is_reported_as_success() {
        let response = proxy_response(StatusCode::OK, "  \n".to_string());
        let (status, body) = status_and_body(response).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!({ "ok": true }));
    }

    #[tokio::test]
    async fn non_empty_success_response_is_forwarded_with_its_status() {
        let payload = json!({ "id": 1, "number": 7 });
        let response = proxy_response(StatusCode::CREATED, payload.to_string());
        let (status, body) = status_and_body(response).await;

        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body, payload);
    }

    #[tokio::test]
    async fn upstream_error_keeps_status_and_details() {
        let payload = json!({ "message": "Not Found" });
        let response = proxy_response(StatusCode::NOT_FOUND, payload.to_string());
        let (status, body) = status_and_body(response).await;

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body,
            json!({
                "error": true,
                "status": 404,
                "body": payload
            })
        );
    }

    #[tokio::test]
    async fn malformed_success_response_is_a_bad_gateway() {
        let response = proxy_response(StatusCode::OK, "not json".to_string());
        let (status, body) = status_and_body(response).await;

        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert_eq!(body["error"], json!(true));
        assert_eq!(body["status"], json!(200));
    }

    #[test]
    fn credential_helper_command_line_config_is_valid() {
        let output = Command::new("git")
            .args([
                "-c",
                "credential.helper=",
                "-c",
                CREDENTIAL_HELPER_CONFIG,
                "config",
                "--get-all",
                "credential.helper",
            ])
            .output()
            .expect("git must be available for this test");

        assert!(
            output.status.success(),
            "git rejected the credential helper config: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            &CREDENTIAL_HELPER_CONFIG["credential.helper=".len()..]
        );
    }
}
