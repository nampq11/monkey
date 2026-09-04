use regex::Regex;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

use crate::gh_writeback::RepoRef;
use monkey_core::db::{Store, StoreError};
use monkey_core::hmac_auth::hmac_sign_with_timestamp;

#[derive(Debug, Error)]
pub enum GhProxyError {
    #[error("failed to build gh-proxy HTTP client: {0}")]
    ClientBuild(#[source] reqwest::Error),
    #[error("failed to serialize request body: {0}")]
    SerializeBody(#[source] serde_json::Error),
    #[error("invalid header value: {0}")]
    InvalidHeader(#[from] reqwest::header::InvalidHeaderValue),
    #[error("gh-proxy request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("failed to audit gh-proxy call: {0}")]
    Audit(#[from] StoreError),
    #[error("gh-proxy {method} {path} failed with status {status}: {body}")]
    Status {
        method: reqwest::Method,
        path: String,
        status: reqwest::StatusCode,
        body: String,
    },
    #[error("malformed gh-proxy JSON response: {0}")]
    Decode(#[from] serde_json::Error),
}

#[derive(Debug, Clone)]
pub struct GHProxy {
    pub base: String,
    pub key: String,
    pub store: Store,
    pub owner: String,
    pub repo: String,
    pub number: i64,
    client: reqwest::Client,
}

impl GHProxy {
    pub fn new(
        base_url: &str,
        hmac_key: &str,
        store: Store,
        repo_ref: &RepoRef,
    ) -> Result<Self, GhProxyError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(GhProxyError::ClientBuild)?;

        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            key: hmac_key.to_string(),
            store,
            owner: repo_ref.owner.clone(),
            repo: repo_ref.repo.clone(),
            number: repo_ref.number,
            client,
        })
    }

    pub async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Value, GhProxyError> {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let body_value = payload.unwrap_or_else(|| json!({}));
        let serialized = serde_json::to_vec(&body_value).map_err(GhProxyError::SerializeBody)?;
        let signature = hmac_sign_with_timestamp(&self.key, &serialized, timestamp as i64);

        let mut headers = HeaderMap::new();
        headers.insert("x-monkey-sig", HeaderValue::from_str(&signature)?);
        headers.insert(
            "x-monkey-ts",
            HeaderValue::from_str(&timestamp.to_string())?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let url = format!("{}{}", self.base, path);
        let response = self
            .client
            .request(method.clone(), &url)
            .headers(headers)
            .body(serialized.clone())
            .send()
            .await?;

        let status = response.status();
        let text = response.text().await?;
        let result = redact(&text);
        let serialized_str = String::from_utf8_lossy(&serialized);

        self.store
            .audit_tool_call(
                &self.owner,
                &self.repo,
                self.number,
                path,
                &redact(&serialized_str),
                &result,
            )
            .await?;

        if status.is_client_error() || status.is_server_error() {
            return Err(GhProxyError::Status {
                method,
                path: path.to_string(),
                status,
                body: result,
            });
        }

        let trimmed = text.trim();
        if trimmed.is_empty() {
            Ok(json!({ "ok": true }))
        } else {
            Ok(serde_json::from_str(trimmed)?)
        }
    }

    pub async fn add_issue_comment(&self, body: &str) -> Result<Value, GhProxyError> {
        let path = format!(
            "/issues/{}/{}/{}/comment",
            self.owner, self.repo, self.number
        );
        self.call(reqwest::Method::POST, &path, Some(json!({ "body": body })))
            .await
    }

    pub async fn add_labels(&self, labels: &[String]) -> Result<Value, GhProxyError> {
        let path = format!(
            "/issues/{}/{}/{}/labels",
            self.owner, self.repo, self.number
        );
        self.call(
            reqwest::Method::POST,
            &path,
            Some(json!({ "labels": labels })),
        )
        .await
    }

    pub async fn update_issue(&self, body: Value) -> Result<Value, GhProxyError> {
        let path = format!("/issues/{}/{}/{}", self.owner, self.repo, self.number);
        self.call(reqwest::Method::PATCH, &path, Some(body)).await
    }

    pub async fn open_pull_request(&self, body: Value) -> Result<Value, GhProxyError> {
        let path = format!("/pulls/{}/{}", self.owner, self.repo);
        self.call(reqwest::Method::POST, &path, Some(body)).await
    }

    pub async fn list_pull_requests(
        &self,
        head: Option<&str>,
        state: Option<&str>,
    ) -> Result<Value, GhProxyError> {
        let mut query = Vec::new();
        if let Some(head) = head {
            query.push(format!("head={}", head));
        }
        if let Some(state) = state {
            query.push(format!("state={}", state));
        }
        let query_str = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        let path = format!("/pulls/{}/{}{}", self.owner, self.repo, query_str);
        self.call(reqwest::Method::GET, &path, None).await
    }

    pub async fn update_pull_request(
        &self,
        pull_number: i64,
        body: Value,
    ) -> Result<Value, GhProxyError> {
        let path = format!("/pulls/{}/{}/{}", self.owner, self.repo, pull_number);
        self.call(reqwest::Method::PATCH, &path, Some(body)).await
    }

    pub async fn push(&self, worktree: &Path, branch: &str) -> Result<Value, GhProxyError> {
        self.call(
            reqwest::Method::POST,
            "/git/push",
            Some(json!({
                "worktree": worktree.to_string_lossy(),
                "branch": branch,
                "repo": format!("{}/{}", self.owner, self.repo)
            })),
        )
        .await
    }
}

pub fn redact(text: &str) -> String {
    static TOKEN_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"gh[pousr]_[A-Za-z0-9]+|github_pat_[A-Za-z0-9_]+")
            .expect("token redaction regex must compile")
    });
    TOKEN_PATTERN.replace_all(text, "[redacted]").to_string()
}
