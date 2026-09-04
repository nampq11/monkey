use monkey_app::adapters::pi::{PiAdapter, TranscriptHealth, parse_transcript};
use monkey_app::adapters::{EngineAdapter, EngineError, Outcome, OutcomeStatus, RunParams};
use std::fs;
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;

#[cfg(unix)]
fn fake_pi(contents: &str, directory: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join("fake-pi.sh");
    fs::write(&path, contents).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).unwrap();
    path
}

#[cfg(unix)]
fn params<'a>(session_dir: &'a Path, worktree: &'a Path) -> RunParams<'a> {
    RunParams {
        prompt: "test prompt",
        worktree,
        session_dir,
        model: "",
        thinking: "medium",
        provider: "",
        timeout: Duration::from_millis(100),
    }
}

#[test]
fn default_adapter_uses_pi_binary() {
    assert_eq!(PiAdapter::default().binary, "pi");
}

#[cfg(unix)]
#[tokio::test]
async fn malformed_agent_event_fails_the_run() {
    let directory = tempdir().unwrap();
    let script = fake_pi(
        "#!/bin/sh\nread prompt\nprintf '%s\\n' 'not-json'\n",
        directory.path(),
    );
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    let result = adapter.run(params(&session_dir, directory.path())).await;

    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("malformed pi event")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn missing_agent_settled_event_fails_the_run() {
    let directory = tempdir().unwrap();
    let script = fake_pi("#!/bin/sh\nread prompt\nexit 0\n", directory.path());
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    let result = adapter.run(params(&session_dir, directory.path())).await;

    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("exited before agent_settled")
    );
}

#[cfg(not(unix))]
#[test]
fn pi_failure_tests_require_unix() {}

// Invariant test: adapter runs must never leave defunct children behind.
// A pi child still alive when the session drive gives up is killed AND
// waited on in shutdown_child; if someone regresses to an un-reaped spawn
// (e.g. std::process-style kill without wait), the defunct child shows up
// in /proc and this test fails.
#[cfg(all(unix, target_os = "linux"))]
#[tokio::test]
async fn timed_out_pi_child_is_reaped_not_left_zombied() {
    let directory = tempdir().unwrap();
    let script = fake_pi(
        "#!/bin/sh\nread prompt\nprintf '%s\\n' '{\"type\":\"prompting\"}'\nsleep 30\n",
        directory.path(),
    );
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    let result = adapter.run(params(&session_dir, directory.path())).await;
    assert!(result.is_err());

    let zombies = zombie_children_of(std::process::id());
    assert!(
        zombies.is_empty(),
        "zombie children remained: {:?}",
        zombies
    );
}

#[cfg(target_os = "linux")]
fn zombie_children_of(parent_pid: u32) -> Vec<u32> {
    let mut zombies = Vec::new();
    let Ok(proc_entries) = std::fs::read_dir("/proc") else {
        return zombies;
    };
    for entry in proc_entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // /proc/<pid>/stat is "pid (comm) state ppid ..."; comm may contain
        // spaces, so parse after the closing parenthesis.
        let Some(fields) = stat.rsplit(')').next() else {
            continue;
        };
        let mut fields = fields.split_whitespace();
        let state = fields.next().unwrap_or_default();
        let parent = fields.next().unwrap_or_default();
        if state == "Z" && parent == parent_pid.to_string() {
            zombies.push(pid);
        }
    }
    zombies
}

// Sanity check for the zombie detector itself: a std::process child that is
// SIGKILLed but never waited on must be reported as defunct, and disappear
// from the report once it is reaped.
#[cfg(target_os = "linux")]
#[test]
fn zombie_detector_reports_and_clears_defunct_children() {
    use std::process::{Command, Stdio};

    let mut child = Command::new("sleep")
        .arg("30")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let child_pid = child.id();

    let killed = Command::new("kill")
        .arg("-9")
        .arg(child_pid.to_string())
        .status()
        .unwrap();
    assert!(killed.success());

    // Give the kernel a moment to transition the child to defunct.
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        zombie_children_of(std::process::id()).contains(&child_pid),
        "expected pid {child_pid} to be defunct before wait()"
    );

    child.wait().unwrap();
    assert!(
        !zombie_children_of(std::process::id()).contains(&child_pid),
        "pid {child_pid} should be reaped after wait()"
    );
}

// Every scripted session has to answer the two RPC calls the adapter always
// makes after settling, so tests only script the interesting part.
#[cfg(unix)]
const RPC_TAIL: &str = concat!(
    "read stats\n",
    "printf '%s\\n' '{\"type\":\"response\",\"id\":\"stats-1\",\"success\":true,\"data\":{}}'\n",
    "read text\n",
    "printf '%s\\n' ",
    "'{\"type\":\"response\",\"id\":\"text-1\",\"success\":true,\"data\":{\"text\":\"partial answer\"}}'\n",
);

#[cfg(unix)]
async fn run_with_events(events: &str) -> Result<Outcome, EngineError> {
    let directory = tempdir().unwrap();
    let script = fake_pi(
        &format!("#!/bin/sh\nread prompt\n{events}{RPC_TAIL}"),
        directory.path(),
    );
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    adapter.run(params(&session_dir, directory.path())).await
}

#[cfg(unix)]
#[tokio::test]
async fn unmodelled_event_is_collected_and_does_not_fail_the_run() {
    // Forward compatibility: a newer engine emitting an event this version does
    // not model must not break triage.
    let outcome = run_with_events(
        "printf '%s\\n' '{\"type\":\"some_future_pi_event\",\"whatever\":1}'\nprintf '%s\\n' '{\"type\":\"agent_settled\"}'\n",
    )
    .await
    .expect("run should succeed despite the unknown event");

    assert_eq!(outcome.status, OutcomeStatus::Ok);
    assert!(
        outcome
            .raw_events
            .iter()
            .any(|event| event["type"] == "some_future_pi_event"),
        "unknown event was dropped from raw_events: {:?}",
        outcome.raw_events
    );
}

#[cfg(unix)]
#[tokio::test]
async fn exhausted_retries_report_the_run_as_failed_not_ok() {
    // pi settles the process normally even after giving up on retries, so the
    // only failure signal is auto_retry_end. Before the typed protocol this
    // produced status "ok" and the partial text was posted as if it were an
    // answer.
    let outcome = run_with_events(
        "printf '%s\\n' '{\"type\":\"auto_retry_end\",\"success\":false,\"attempt\":3,\"finalError\":\"529 overloaded\"}'\nprintf '%s\\n' '{\"type\":\"agent_settled\"}'\n",
    )
    .await
    .expect("a settled run is not an Err even when it failed");

    assert_eq!(outcome.status, OutcomeStatus::Error);
    assert!(
        outcome.summary.contains("529 overloaded"),
        "summary lost the engine reason: {}",
        outcome.summary
    );
    // Empty pr_body/comment make gh_writeback fall through to `summary`, which
    // is what posts the failure reason instead of the partial answer.
    assert!(outcome.pr_body.is_empty());
    assert!(outcome.comment.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn rejected_rpc_surfaces_the_engine_reason() {
    let directory = tempdir().unwrap();
    let script = fake_pi(
        "#!/bin/sh\nread prompt\nprintf '%s\\n' '{\"type\":\"agent_settled\"}'\nread stats\nprintf '%s\\n' '{\"type\":\"response\",\"id\":\"stats-1\",\"command\":\"get_session_stats\",\"success\":false,\"error\":\"Model not found: x\"}'\n",
        directory.path(),
    );
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    let error = adapter
        .run(params(&session_dir, directory.path()))
        .await
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("Model not found: x"),
        "engine reason was discarded: {error}"
    );
    assert!(
        !error.contains("missing data object"),
        "still reporting the downstream symptom: {error}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unattributed_rejection_is_still_matched_to_the_pending_request() {
    // pi cannot echo an id it could not parse out of the request, so a
    // rejection may arrive with no id. Correlating strictly on id turns that
    // into a timeout that hides the real error.
    let directory = tempdir().unwrap();
    let script = fake_pi(
        "#!/bin/sh\nread prompt\nprintf '%s\\n' '{\"type\":\"agent_settled\"}'\nread stats\nprintf '%s\\n' '{\"type\":\"response\",\"command\":\"parse\",\"success\":false,\"error\":\"Failed to parse command\"}'\n",
        directory.path(),
    );
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    let error = adapter
        .run(params(&session_dir, directory.path()))
        .await
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("Failed to parse command"),
        "unattributed rejection was skipped: {error}"
    );
    assert!(!error.contains("timed out"), "still a timeout: {error}");
}

fn write_session(directory: &Path, contents: &str) -> std::path::PathBuf {
    let path = directory.join("session.jsonl");
    fs::write(&path, contents).unwrap();
    path
}

// Echoes the first request the adapter writes into first-request.txt, consumes
// `reads` requests, then settles. The count matters: resume writes
// switch_session before the prompt, so a fake that stops reading after the
// first line drifts out of sync with the adapter and exits while the adapter is
// still writing, which surfaces as an intermittent broken pipe.
#[cfg(unix)]
fn echo_script(reads: usize) -> String {
    let mut script = String::from("#!/bin/sh\n");
    for index in 0..reads {
        script.push_str(&format!("read request{index}\n"));
        if index == 0 {
            script.push_str("printf '%s' \"$request0\" > first-request.txt\n");
        }
    }
    script.push_str(&format!(
        "printf '%s\\n' '{{\"type\":\"agent_settled\"}}'\n{RPC_TAIL}"
    ));
    script
}

// The resume path must hand pi the transcript to continue before the prompt,
// otherwise pi starts a brand-new session and the follow-up loses the whole
// earlier conversation.
#[cfg(unix)]
#[tokio::test]
async fn resume_switches_into_the_latest_session_transcript() {
    let directory = tempdir().unwrap();
    let script = fake_pi(&echo_script(2), directory.path());
    let session_dir = directory.path().join("session");
    fs::create_dir_all(&session_dir).unwrap();
    fs::write(session_dir.join("aaa.jsonl"), "{\"role\":\"user\"}").unwrap();
    fs::write(session_dir.join("zzz.jsonl"), "{\"role\":\"user\"}").unwrap();
    let adapter = PiAdapter::new(script.to_str());

    let outcome = adapter
        .resume(params(&session_dir, directory.path()))
        .await
        .expect("resume should drive a settled session");

    let first_request = fs::read_to_string(directory.path().join("first-request.txt")).unwrap();
    let request: serde_json::Value =
        serde_json::from_str(&first_request).expect("first request is a JSON line");
    assert_eq!(request["type"], "switch_session");
    assert_eq!(
        request["sessionPath"],
        session_dir.join("zzz.jsonl").to_string_lossy().as_ref(),
        "the newest transcript must be the one resumed"
    );
    assert_eq!(outcome.status, OutcomeStatus::Ok);
}

#[cfg(unix)]
#[tokio::test]
async fn resume_without_a_transcript_still_runs() {
    // A recorded session whose directory was wiped must degrade to a normal
    // run rather than failing the follow-up event.
    let directory = tempdir().unwrap();
    let script = fake_pi(&echo_script(1), directory.path());
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    let outcome = adapter
        .resume(params(&session_dir, directory.path()))
        .await
        .expect("a missing transcript is not a fatal resume error");

    let first_request = fs::read_to_string(directory.path().join("first-request.txt")).unwrap();
    let request: serde_json::Value = serde_json::from_str(&first_request).unwrap();
    assert_eq!(request["type"], "prompt");
    assert_eq!(outcome.status, OutcomeStatus::Ok);
}

#[test]
fn transcript_with_a_complete_final_record_is_healthy() {
    let directory = tempdir().unwrap();
    let path = write_session(
        directory.path(),
        "{\"role\":\"user\"}\n{\"role\":\"assistant\"}\n",
    );

    let (entries, health) = parse_transcript(&path);

    assert_eq!(entries.len(), 2);
    assert_eq!(health, TranscriptHealth::Complete);
}

#[test]
fn truncated_tail_is_reported_without_losing_the_valid_entries() {
    // The expected failure mode: kill_on_drop stops pi mid-write, so the last
    // record is cut off. Everything before it is still trustworthy.
    let directory = tempdir().unwrap();
    let path = write_session(directory.path(), "{\"role\":\"user\"}\n{\"role\":\"assis");

    let (entries, health) = parse_transcript(&path);

    assert_eq!(entries.len(), 1, "valid leading entry was dropped");
    assert_eq!(health, TranscriptHealth::TruncatedTail);
}

#[test]
fn interior_corruption_is_reported_with_its_line_number() {
    // An interior failure means the file is not what we think it is, which is
    // a different problem from a cut-off tail and must not be conflated.
    let directory = tempdir().unwrap();
    let path = write_session(
        directory.path(),
        "{\"role\":\"user\"}\nNOT JSON\n{\"role\":\"assistant\"}\n",
    );

    let (entries, health) = parse_transcript(&path);

    match &health {
        TranscriptHealth::Malformed {
            line_number,
            reason,
        } => {
            assert_eq!(*line_number, 2, "wrong line blamed: {health:?}");
            assert!(!reason.is_empty(), "reason was dropped: {health:?}");
        }
        other => panic!("expected Malformed, got {other:?}"),
    }
    assert_eq!(
        entries.len(),
        2,
        "entries after the bad line were discarded"
    );
}

#[test]
fn interior_corruption_outranks_a_truncated_tail() {
    let directory = tempdir().unwrap();
    let path = write_session(directory.path(), "NOT JSON\n{\"role\":\"user\"}\n{\"cut");

    let (_, health) = parse_transcript(&path);

    assert!(
        matches!(health, TranscriptHealth::Malformed { line_number: 1, .. }),
        "truncation masked the more serious interior corruption: {health:?}"
    );
}

#[test]
fn missing_session_file_is_not_reported_as_corruption() {
    // session_artifacts runs against directories pi may never have written to.
    let directory = tempdir().unwrap();
    let path = directory.path().join("does-not-exist.jsonl");

    let (entries, health) = parse_transcript(&path);

    assert!(entries.is_empty());
    assert_eq!(health, TranscriptHealth::Complete);
}

#[test]
fn session_artifacts_expose_the_health_signal() {
    let directory = tempdir().unwrap();
    let session_dir = directory.path().join("session");
    fs::create_dir_all(&session_dir).unwrap();
    write_session(&session_dir, "{\"role\":\"user\"}\n{\"cut");

    let artifacts = PiAdapter::new(None).session_artifacts(&session_dir);

    assert_eq!(artifacts["health"]["state"], "truncated_tail");
    assert_eq!(artifacts["messages"].as_array().map(Vec::len), Some(1));
}
