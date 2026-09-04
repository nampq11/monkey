use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, Command};
use tokio_util::codec::{FramedRead, LinesCodec};

use super::pi_protocol::{PiEvent, PiResponse};
use super::{EngineAdapter, EngineError, Outcome, OutcomeStatus, RunParams};

#[derive(Debug, Clone)]
pub struct PiAdapter {
    pub binary: String,
}

impl Default for PiAdapter {
    fn default() -> Self {
        Self::new(None)
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
    async fn run(&self, params: RunParams<'_>) -> Result<Outcome, EngineError> {
        tokio::fs::create_dir_all(params.session_dir).await?;

        drive_rpc_session(&self.binary, &params, None).await
    }

    async fn resume(&self, params: RunParams<'_>) -> Result<Outcome, EngineError> {
        drive_rpc_session(&self.binary, &params, find_session_file(params.session_dir)).await
    }

    fn session_artifacts(&self, session_dir: &Path) -> Value {
        match find_session_file(session_dir) {
            Some(path) => {
                let (messages, health) = parse_transcript(&path);
                json!({
                    "session_file": path.to_string_lossy(),
                    "messages": messages,
                    "health": health,
                })
            }
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
) -> Result<Outcome, EngineError> {
    let mut child = spawn_pi(binary, params)?;

    let outcome = drive_protocol(&mut child, params, switch_to).await;
    // Reap on every path (success or failure): kill() alone leaves the child
    // in the process table as a zombie until it is waited on, which exhausts
    // PIDs in a long-lived container.
    shutdown_child(&mut child).await;

    outcome
}

async fn drive_protocol(
    child: &mut Child,
    params: &RunParams<'_>,
    switch_to: Option<PathBuf>,
) -> Result<Outcome, EngineError> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| EngineError::Framing("failed to open stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| EngineError::Framing("failed to open stdout".into()))?;
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
    let terminal_error = drain_until_settled(&mut lines, &mut raw_events, params.timeout).await?;
    let stats_resp = rpc_request(&mut stdin, &mut lines, "get_session_stats", "stats-1").await?;
    let text_resp =
        rpc_request(&mut stdin, &mut lines, "get_last_assistant_text", "text-1").await?;

    build_outcome(
        params.worktree,
        params.session_dir,
        raw_events,
        &stats_resp,
        &text_resp,
        terminal_error.as_deref(),
    )
    .await
}

// kill() may report an error when the child already exited (the normal case
// after a successful session); wait() is what actually reaps it.
async fn shutdown_child(child: &mut Child) {
    if let Err(error) = child.kill().await {
        tracing::debug!(
            "pi child kill returned an error (likely already exited): {}",
            error
        );
    }
    if let Err(error) = child.wait().await {
        tracing::warn!("failed to reap pi child process: {}", error);
    }
}

async fn rpc_request(
    stdin: &mut tokio::process::ChildStdin,
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    request_type: &str,
    request_id: &str,
) -> Result<PiResponse, EngineError> {
    send_line(stdin, &json!({ "type": request_type, "id": request_id })).await?;
    wait_for_response(lines, request_id, request_type, Duration::from_secs(10)).await
}

// build_outcome is the "collect stats, read branch, classify output" tail
// shared by run() and resume() so the two paths cannot drift apart.
/// `terminal_error` is pi's reason for exhausting its automatic retries. When
/// it is set the run is reported as failed and the partial model output is
/// deliberately dropped: `pr_body` and `comment` stay empty so that
/// `gh_writeback` falls through to `summary`, which posts the failure reason
/// instead of half an answer as if triage had succeeded.
async fn build_outcome(
    worktree: &Path,
    session_dir: &Path,
    raw_events: Vec<Value>,
    stats_resp: &PiResponse,
    text_resp: &PiResponse,
    terminal_error: Option<&str>,
) -> Result<Outcome, EngineError> {
    let session_file = stats_resp
        .data
        .get("sessionFile")
        .and_then(Value::as_str)
        .map(PathBuf::from);

    let last_text = text_resp
        .data
        .get("text")
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::Framing("pi text response missing data.text".into()))?
        .to_string();

    let branch = read_branch(worktree).await;

    let mut outcome = Outcome {
        session_dir: session_dir.to_path_buf(),
        status: match terminal_error {
            Some(_) => OutcomeStatus::Error,
            None => OutcomeStatus::Ok,
        },
        summary: terminal_error.unwrap_or(&last_text).to_string(),
        pr_body: String::new(),
        comment: String::new(),
        branch,
        artifact_paths: Vec::new(),
        raw_events,
    };

    if let Some(file) = session_file {
        outcome.artifact_paths.push(file);
    }

    if terminal_error.is_none() {
        if looks_like_pr_body(&last_text) {
            outcome.pr_body = last_text;
        } else if !last_text.is_empty() {
            outcome.comment = last_text;
        }
    }

    Ok(outcome)
}

fn spawn_pi(binary: &str, params: &RunParams<'_>) -> Result<Child, EngineError> {
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
    for (env_key, _) in std::env::vars() {
        if env_key.starts_with("GITHUB_") || env_key.starts_with("MONKEY_") {
            cmd.env_remove(&env_key);
        }
    }

    cmd.spawn().map_err(|source| EngineError::Spawn {
        binary: binary.to_string(),
        source,
    })
}

async fn send_line(stdin: &mut tokio::process::ChildStdin, obj: &Value) -> Result<(), EngineError> {
    let line = serde_json::to_string(obj).map_err(|error| {
        EngineError::Framing(format!("failed to serialize pi request: {error}"))
    })? + "\n";
    stdin.write_all(line.as_bytes()).await?;
    stdin.flush().await?;
    Ok(())
}

/// Consumes events until pi reports the session has settled.
///
/// The `Ok` payload is pi's stated reason for exhausting its automatic
/// retries. That is deliberately not an `Err`: the process settles normally and
/// the worker still owes the issue a comment, but the outcome has to be
/// recorded as a failure rather than a clean run.
async fn drain_until_settled(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    raw_events: &mut Vec<Value>,
    timeout: Duration,
) -> Result<Option<String>, EngineError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut terminal_error = None;
    loop {
        let (val, event) = next_json_event(lines, deadline, "agent_settled", "event").await?;
        raw_events.push(val);
        match event {
            PiEvent::AgentSettled => {
                drain_trailing_events(lines, raw_events).await?;
                return Ok(terminal_error);
            }
            PiEvent::AutoRetryEnd {
                success: false,
                final_error,
            } => {
                terminal_error = Some(
                    final_error.unwrap_or_else(|| "pi exhausted its automatic retries".into()),
                );
            }
            _ => {}
        }
    }
}

/// pi can still be flushing events when `agent_settled` arrives, so keep
/// draining for a short grace period instead of cutting the child off.
async fn drain_trailing_events(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    raw_events: &mut Vec<Value>,
) -> Result<(), EngineError> {
    loop {
        match tokio::time::timeout(Duration::from_millis(50), lines.next()).await {
            Ok(Some(Ok(extra))) => {
                let trimmed = extra.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let extra_value = serde_json::from_str::<Value>(trimmed).map_err(|error| {
                    EngineError::Framing(format!("malformed pi event: {error}"))
                })?;
                raw_events.push(extra_value);
            }
            Ok(Some(Err(error))) => {
                return Err(EngineError::Framing(format!(
                    "failed reading pi output: {error}"
                )));
            }
            Ok(None) | Err(_) => return Ok(()),
        }
    }
}

/// Reads the next non-empty JSON line, enforcing `deadline`.
///
/// Returns the raw `Value` alongside the typed event because
/// `Outcome::raw_events` stores pi's output verbatim and tests assert on it.
///
/// The two name parameters only shape error messages ("pi exited before
/// {waiting_for}", "malformed pi {parsing}").
async fn next_json_event(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    deadline: tokio::time::Instant,
    waiting_for: &str,
    parsing: &str,
) -> Result<(Value, PiEvent), EngineError> {
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(EngineError::Timeout(waiting_for.to_string()));
        }

        let line = match tokio::time::timeout(remaining, lines.next()).await {
            Ok(Some(Ok(line))) => line,
            Ok(Some(Err(error))) => {
                return Err(EngineError::Framing(format!(
                    "failed reading pi output: {error}"
                )));
            }
            Ok(None) => return Err(EngineError::PrematureExit(waiting_for.to_string())),
            Err(_) => return Err(EngineError::Timeout(waiting_for.to_string())),
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let raw: Value = serde_json::from_str(trimmed)
            .map_err(|error| EngineError::Framing(format!("malformed pi {parsing}: {error}")))?;
        let event = match serde_json::from_value::<PiEvent>(raw.clone()) {
            Ok(event) => event,
            // A `response` we cannot model is a contract break on the one
            // message we must act on, so it fails loudly. Anything else that
            // does not fit is merely an engine version we do not model, and
            // degrades to `Unknown` rather than killing a live run.
            Err(error) if raw.get("type").and_then(Value::as_str) == Some("response") => {
                return Err(EngineError::Framing(format!(
                    "malformed pi {parsing}: {error}"
                )));
            }
            Err(error) => {
                tracing::warn!("unmodelled pi {parsing}, ignoring: {error}");
                PiEvent::Unknown
            }
        };
        if event.is_unknown() {
            tracing::debug!("unmodelled pi {parsing}: {trimmed}");
        }
        return Ok((raw, event));
    }
}

async fn wait_for_response(
    lines: &mut FramedRead<tokio::process::ChildStdout, LinesCodec>,
    request_id: &str,
    command: &str,
    timeout: Duration,
) -> Result<PiResponse, EngineError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (_, event) = next_json_event(
            lines,
            deadline,
            &format!("response {request_id}"),
            "response",
        )
        .await?;
        let PiEvent::Response(response) = event else {
            continue;
        };
        // pi omits `id` when it could not parse the request that carried ours,
        // so an unattributed rejection still has to count as ours. Correlating
        // strictly on id is what turned a rejected command into a bare timeout.
        let ours = response.id.as_deref() == Some(request_id)
            || (response.id.is_none() && !response.success);
        if !ours {
            continue;
        }
        if !response.success {
            return Err(EngineError::EngineRejected {
                command: command.to_string(),
                error: response
                    .error
                    .unwrap_or_else(|| "pi reported no reason".to_string()),
            });
        }
        return Ok(response);
    }
}

/// The structured report sections the fix prompt requires the engine to
/// emit. Single source for both the lenient check here and the strict
/// write-back gate in monkey_github.
pub const REPORT_SECTIONS: [&str; 4] = ["## Repro", "## Cause", "## Fix", "## Verification"];

pub fn looks_like_pr_body(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let count = REPORT_SECTIONS
        .iter()
        .filter(|&&section| text.contains(section))
        .count();
    count >= 2
}

pub fn find_session_file(session_dir: &Path) -> Option<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(session_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
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

/// How much of a pi session file we could actually trust.
///
/// Recorded rather than swallowed because pi writes sessions as JSONL and the
/// child is killed via `kill_on_drop`, so a cut-off final line is an expected
/// outcome, not an exotic one. Before this existed every unparsable line just
/// vanished and the transcript was silently shorter than reality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TranscriptHealth {
    Complete,
    /// The last record was cut off mid-write. Everything before it is sound.
    TruncatedTail,
    /// An interior record failed to parse, so the file is not what we think
    /// it is. Reported for the first such line only.
    Malformed {
        line_number: usize,
        reason: String,
    },
}

impl TranscriptHealth {
    fn is_malformed(&self) -> bool {
        matches!(self, Self::Malformed { .. })
    }
}

/// Reads a pi session transcript, returning the entries it could parse plus a
/// health signal describing what it could not.
///
/// An unreadable or missing file is not an error: `session_artifacts` runs
/// against directories pi may never have written to.
pub fn parse_transcript(session_path: &Path) -> (Vec<Value>, TranscriptHealth) {
    let Ok(content) = std::fs::read_to_string(session_path) else {
        return (Vec::new(), TranscriptHealth::Complete);
    };

    let lines: Vec<&str> = content.lines().collect();
    let last_index = lines.len().saturating_sub(1);
    let mut entries = Vec::new();
    let mut health = TranscriptHealth::Complete;

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(value) => entries.push(value),
            // Keep going either way: partial data plus an honest health flag
            // beats discarding the rest of the transcript.
            Err(error) => {
                let line_number = index + 1;
                if health.is_malformed() {
                    continue;
                }
                if index == last_index {
                    tracing::warn!(
                        "pi session file {} ends in an incomplete record: {error}",
                        session_path.display()
                    );
                    health = TranscriptHealth::TruncatedTail;
                } else {
                    tracing::error!(
                        "pi session file {} has an unparsable interior record at line \
                         {line_number}: {error}",
                        session_path.display()
                    );
                    health = TranscriptHealth::Malformed {
                        line_number,
                        reason: error.to_string(),
                    };
                }
            }
        }
    }

    (entries, health)
}
