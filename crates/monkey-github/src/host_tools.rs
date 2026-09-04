use regex::Regex;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::gh_writeback::RepoRef;
use monkey_core::db::Store;
use monkey_core::hmac_auth::hmac_sign_with_timestamp;

#[derive(Clone)]
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
    pub fn new(base_url: &str, hmac_key: &str, store: Store, repo_ref: &RepoRef) -> Self {
        Self {
            base: base_url.trim_end_matches('/').to_string(),
            key: hmac_key.to_string(),
            store,
            owner: repo_ref.owner.clone(),
            repo: repo_ref.repo.clone(),
            number: repo_ref.number,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }

    pub async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        payload: Option<Value>,
    ) -> Result<Value, String> {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let body_val = payload.unwrap_or_else(|| json!({}));
        let serialized = serde_json::to_vec(&body_val)
            .map_err(|e| format!("failed to serialize body: {}", e))?;
        let sig = hmac_sign_with_timestamp(&self.key, &serialized, ts as i64);

        let mut headers = HeaderMap::new();
        headers.insert(
            "x-monkey-sig",
            HeaderValue::from_str(&sig)
                .map_err(|e| format!("invalid x-monkey-sig header: {}", e))?,
        );
        headers.insert(
            "x-monkey-ts",
            HeaderValue::from_str(&ts.to_string())
                .map_err(|e| format!("invalid x-monkey-ts header: {}", e))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

        let url = format!("{}{}", self.base, path);
        let resp = self
            .client
            .request(method.clone(), &url)
            .headers(headers)
            .body(serialized.clone())
            .send()
            .await
            .map_err(|e| format!("gh-proxy request failed: {}", e))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("failed to read gh-proxy response: {}", e))?;
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
            .map_err(|e| format!("failed to audit gh-proxy call: {}", e))?;

        if status.is_client_error() || status.is_server_error() {
            return Err(format!(
                "gh-proxy {} {} -> {}: {}",
                method, path, status, result
            ));
        }

        serde_json::from_str(&text).map_err(|e| format!("malformed gh-proxy JSON response: {}", e))
    }

    pub async fn add_issue_comment(&self, body: &str) -> Result<Value, String> {
        let path = format!(
            "/issues/{}/{}/{}/comment",
            self.owner, self.repo, self.number
        );
        self.call(reqwest::Method::POST, &path, Some(json!({ "body": body })))
            .await
    }

    pub async fn add_labels(&self, labels: &[String]) -> Result<Value, String> {
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

    pub async fn update_issue(&self, body: Value) -> Result<Value, String> {
        let path = format!("/issues/{}/{}/{}", self.owner, self.repo, self.number);
        self.call(reqwest::Method::PATCH, &path, Some(body)).await
    }

    pub async fn open_pull_request(&self, body: Value) -> Result<Value, String> {
        let path = format!("/pulls/{}/{}", self.owner, self.repo);
        self.call(reqwest::Method::POST, &path, Some(body)).await
    }

    pub async fn push(&self, worktree: &Path, branch: &str) -> Result<Value, String> {
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
