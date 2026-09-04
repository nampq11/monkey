use monkey::adapters::pi::PiAdapter;
use monkey::adapters::{EngineAdapter, RunParams};
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
