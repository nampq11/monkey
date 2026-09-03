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

        let mut child = spawn_pi(&self.binary, &params)?;

        let mut stdin = child.stdin.take().ok_or("failed to open stdin")?;
        let stdout = child.stdout.take().ok_or("failed to open stdout")?;
        let mut lines = FramedRead::new(stdout, LinesCodec::new());

        // 1. Send initial prompt
        let prompt_msg = json!({
            "type": "prompt",
            "message": params.prompt,
            "id": "monkey-1"
        });
        send_line(&mut stdin, &prompt_msg).await?;

        // 2. Drain until agent_settled
        let mut raw_events = Vec::new();
        drain_until_settled(&mut lines, &mut raw_events, params.timeout).await?;

        // 3. Get session stats
        let stats_req = json!({
            "type": "get_session_stats",
            "id": "stats-1"
        });
        send_line(&mut stdin, &stats_req).await?;
        let stats_resp = wait_for_response(&mut lines, "stats-1", Duration::from_secs(10)).await?;

        // 4. Get last assistant text
        let text_req = json!({
            "type": "get_last_assistant_text",
            "id": "text-1"
        });
        send_line(&mut stdin, &text_req).await?;
        let text_resp = wait_for_response(&mut lines, "text-1", Duration::from_secs(10)).await?;

        // Clean up process
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

    async fn resume(&self, params: RunParams<'_>) -> Result<Outcome, String> {
        let session_path = find_session_file(params.session_dir);

        let mut child = spawn_pi(&self.binary, &params)?;

        let mut stdin = child.stdin.take().ok_or("failed to open stdin")?;
        let stdout = child.stdout.take().ok_or("failed to open stdout")?;
        let mut lines = FramedRead::new(stdout, LinesCodec::new());

        // 1. Switch session if existing transcript exists
        if let Some(ref path) = session_path {
            let switch_msg = json!({
                "type": "switch_session",
                "sessionPath": path.to_string_lossy()
            });
            send_line(&mut stdin, &switch_msg).await?;
        }

        // 2. Send follow-up prompt
        let prompt_msg = json!({
            "type": "prompt",
            "message": params.prompt,
            "id": "monkey-1"
        });
        send_line(&mut stdin, &prompt_msg).await?;

        // 3. Drain until settled
        let mut raw_events = Vec::new();
        drain_until_settled(&mut lines, &mut raw_events, params.timeout).await?;

        // 4. Get stats and last text
        let stats_req = json!({
            "type": "get_session_stats",
            "id": "stats-1"
        });
        send_line(&mut stdin, &stats_req).await?;
        let stats_resp = wait_for_response(&mut lines, "stats-1", Duration::from_secs(10)).await?;

        let text_req = json!({
            "type": "get_last_assistant_text",
            "id": "text-1"
        });
        send_line(&mut stdin, &text_req).await?;
        let text_resp = wait_for_response(&mut lines, "text-1", Duration::from_secs(10)).await?;

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

    fn session_artifacts(&self, session_dir: &Path) -> Value {
        let session_path = find_session_file(session_dir);
        match session_path {
            Some(path) => {
                let msgs = parse_transcript(&path);
                json!({
                    "session_file": path.to_string_lossy(),
                    "messages": msgs
                })
            }
            None => json!({}),
        }
    }
}

// run() and resume() shared the entire "collect stats, read branch, classify
// output" tail - extracted so the two paths cannot drift apart.
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
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err("pi timed out waiting for agent_settled".to_string());
        }

        let line = match tokio::time::timeout(remaining, lines.next()).await {
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(error))) => return Err(format!("failed reading pi output: {}", error)),
            Ok(None) => return Err("pi exited before agent_settled".to_string()),
            Err(_) => return Err("pi timed out waiting for agent_settled".to_string()),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val = serde_json::from_str::<Value>(trimmed)
            .map_err(|error| format!("malformed pi event: {}", error))?;
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

async fn wait_for_response(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    req_id: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!("timed out waiting for pi response {}", req_id));
        }

        let line = match tokio::time::timeout(remaining, lines.next()).await {
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(error))) => return Err(format!("failed reading pi output: {}", error)),
            Ok(None) => return Err(format!("pi exited before response {}", req_id)),
            Err(_) => return Err(format!("timed out waiting for pi response {}", req_id)),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let val = serde_json::from_str::<Value>(trimmed)
            .map_err(|error| format!("malformed pi response: {}", error))?;
        if val.get("type").and_then(|value| value.as_str()) == Some("response")
            && val.get("id").and_then(|value| value.as_str()) == Some(req_id)
        {
            return Ok(val);
        }
    }
}

pub fn looks_like_pr_body(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let sections = ["## Repro", "## Cause", "## Fix", "## Verification"];
    let count = sections.iter().filter(|&&s| text.contains(s)).count();
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
