use futures_util::StreamExt;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec};

use super::{EngineAdapter, Outcome, RunParams};

pub struct PiAdapter {
    pub binary: String,
}

impl Default for PiAdapter {
    fn default() -> Self {
        Self {
            binary: "pi".to_string(),
        }
    }
}

impl PiAdapter {
    pub fn new(binary: Option<&str>) -> Self {
        Self {
            binary: binary.unwrap_or("pi").to_string(),
        }
    }
}

impl EngineAdapter for PiAdapter {
    async fn run(&self, params: RunParams<'_>) -> Result<Outcome, String> {
        tokio::fs::create_dir_all(params.session_dir)
            .await
            .map_err(|e| format!("failed to create session dir: {}", e))?;

        drive_rpc_session(&self.binary, &params, None).await
    }

    async fn resume(&self, params: RunParams<'_>) -> Result<Outcome, String> {
        drive_rpc_session(&self.binary, &params, find_session_file(params.session_dir)).await
    }

    fn session_artifacts(&self, session_dir: &Path) -> Value {
        match find_session_file(session_dir) {
            Some(path) => json!({
                "session_file": path.to_string_lossy(),
                "messages": parse_transcript(&path)
            }),
            None => json!({}),
        }
    }
}

// run() and resume() share the whole session drive: prompt, drain to
// agent_settled, then collect stats and last text. The only difference is
// resume() switching into an existing transcript first.
async fn drive_rpc_session(
    binary: &str,
    params: &RunParams<'_>,
    switch_to: Option<PathBuf>,
) -> Result<Outcome, String> {
    let mut child = spawn_pi(binary, params)?;

    let mut stdin = child.stdin.take().ok_or("failed to open stdin")?;
    let stdout = child.stdout.take().ok_or("failed to open stdout")?;
    let mut lines = FramedRead::new(stdout, LinesCodec::new());

    if let Some(path) = switch_to {
        send_line(
            &mut stdin,
            &json!({
                "type": "switch_session",
                "sessionPath": path.to_string_lossy()
            }),
        )
        .await?;
    }

    send_line(
        &mut stdin,
        &json!({
            "type": "prompt",
            "message": params.prompt,
            "id": "monkey-1"
        }),
    )
    .await?;

    let mut raw_events = Vec::new();
    drain_until_settled(&mut lines, &mut raw_events, params.timeout).await?;
    let stats_resp = rpc_request(&mut stdin, &mut lines, "get_session_stats", "stats-1").await?;
    let text_resp =
        rpc_request(&mut stdin, &mut lines, "get_last_assistant_text", "text-1").await?;

    let _ = child.kill().await;

    build_outcome(
        params.worktree,
        params.session_dir,
        raw_events,
        &stats_resp,
        &text_resp,
    )
    .await
}

async fn rpc_request(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    request_type: &str,
    req_id: &str,
) -> Result<Value, String> {
    send_line(stdin, &json!({ "type": request_type, "id": req_id })).await?;
    wait_for_response(lines, req_id, Duration::from_secs(10)).await
}

// build_outcome is the "collect stats, read branch, classify output" tail
// shared by run() and resume() so the two paths cannot drift apart.
async fn build_outcome(
    worktree: &Path,
    session_dir: &Path,
    raw_events: Vec<Value>,
    stats_resp: &Value,
    text_resp: &Value,
) -> Result<Outcome, String> {
    let stats_data = stats_resp
        .get("data")
        .and_then(Value::as_object)
        .ok_or("pi stats response missing data object")?;
    let session_file = stats_data
        .get("sessionFile")
        .and_then(|session_file| session_file.as_str())
        .map(PathBuf::from);

    let text_data = text_resp
        .get("data")
        .and_then(Value::as_object)
        .ok_or("pi text response missing data object")?;
    let last_text = text_data
        .get("text")
        .and_then(|text| text.as_str())
        .ok_or("pi text response missing data.text")?
        .to_string();

    let branch = read_branch(worktree).await;

    let mut outcome = Outcome {
        session_dir: session_dir.to_path_buf(),
        status: "ok".to_string(),
        summary: last_text.clone(),
        pr_body: String::new(),
        comment: String::new(),
        branch,
        artifact_paths: Vec::new(),
        raw_events,
    };

    if let Some(file) = session_file {
        outcome.artifact_paths.push(file);
    }

    if looks_like_pr_body(&last_text) {
        outcome.pr_body = last_text;
    } else if !last_text.is_empty() {
        outcome.comment = last_text;
    }

    Ok(outcome)
}

fn spawn_pi(binary: &str, params: &RunParams<'_>) -> Result<tokio::process::Child, String> {
    let mut cmd = Command::new(binary);
    cmd.arg("--mode").arg("rpc");
    cmd.arg("--session-dir").arg(params.session_dir);
    cmd.arg("--name").arg("monkey");

    if !params.model.is_empty() {
        cmd.arg("--model").arg(params.model);
    }
    if !params.provider.is_empty() {
        cmd.arg("--provider").arg(params.provider);
    }
    cmd.arg("--thinking").arg(params.thinking);

    cmd.current_dir(params.worktree);
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::null());
    cmd.kill_on_drop(true);

    // Keep internal configuration and GitHub credentials out of prompt-injected code.
    for (k, _) in std::env::vars() {
        if k.starts_with("GITHUB_") || k.starts_with("MONKEY_") {
            cmd.env_remove(&k);
        }
    }

    cmd.spawn()
        .map_err(|e| format!("failed to spawn pi ({}): {}", binary, e))
}

async fn send_line(stdin: &mut tokio::process::ChildStdin, obj: &Value) -> Result<(), String> {
    let line = serde_json::to_string(obj).map_err(|e| e.to_string())? + "\n";
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("failed to write to child stdin: {}", e))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("failed to flush child stdin: {}", e))?;
    Ok(())
}

async fn drain_until_settled(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    raw_events: &mut Vec<Value>,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let val = next_json_event(lines, deadline, "agent_settled", "event").await?;
        let is_settled = val.get("type").and_then(|value| value.as_str()) == Some("agent_settled");
        raw_events.push(val);
        if is_settled {
            loop {
                match tokio::time::timeout(Duration::from_millis(50), lines.next()).await {
                    Ok(Some(Ok(extra))) => {
                        let trimmed = extra.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        let extra_value = serde_json::from_str::<Value>(trimmed)
                            .map_err(|error| format!("malformed pi event: {}", error))?;
                        raw_events.push(extra_value);
                    }
                    Ok(Some(Err(error))) => {
                        return Err(format!("failed reading pi output: {}", error));
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            return Ok(());
        }
    }
}

// Reads the next non-empty JSON line, enforcing `deadline`. The two name
// parameters only shape error messages ("pi exited before {waiting_for}",
// "malformed pi {parsing}").
async fn next_json_event(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    deadline: tokio::time::Instant,
    waiting_for: &str,
    parsing: &str,
) -> Result<Value, String> {
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("pi timed out waiting for {waiting_for}"));
        }

        let line = match tokio::time::timeout(remaining, lines.next()).await {
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(error))) => return Err(format!("failed reading pi output: {}", error)),
            Ok(None) => return Err(format!("pi exited before {waiting_for}")),
            Err(_) => return Err(format!("pi timed out waiting for {waiting_for}")),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        return serde_json::from_str(trimmed)
            .map_err(|error| format!("malformed pi {parsing}: {}", error));
    }
}

async fn wait_for_response(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    req_id: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let val =
            next_json_event(lines, deadline, &format!("response {req_id}"), "response").await?;
        if val.get("type").and_then(|value| value.as_str()) == Some("response")
            && val.get("id").and_then(|value| value.as_str()) == Some(req_id)
        {
            return Ok(val);
        }
    }
}

/// The structured report sections the fix prompt requires the engine to
/// emit. Single source for both the lenient check here and the strict
/// write-back gate in monkey-github.
pub const REPORT_SECTIONS: [&str; 4] = ["## Repro", "## Cause", "## Fix", "## Verification"];

pub fn looks_like_pr_body(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let count = REPORT_SECTIONS
        .iter()
        .filter(|&&s| text.contains(s))
        .count();
    count >= 2
}

pub fn find_session_file(session_dir: &Path) -> Option<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(session_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                files.push(path);
            }
        }
    }
    files.sort();
    files.pop()
}

pub async fn read_branch(worktree: &Path) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => String::new(),
    }
}

pub fn parse_transcript(session_path: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    if let Ok(content) = std::fs::read_to_string(session_path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(val) = serde_json::from_str(trimmed) {
                out.push(val);
            }
        }
    }
    out
}
