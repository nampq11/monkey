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

    assert!(result.unwrap_err().contains("malformed pi event"));
}

#[cfg(unix)]
#[tokio::test]
async fn missing_agent_settled_event_fails_the_run() {
    let directory = tempdir().unwrap();
    let script = fake_pi("#!/bin/sh\nread prompt\nexit 0\n", directory.path());
    let session_dir = directory.path().join("session");
    let adapter = PiAdapter::new(script.to_str());

    let result = adapter.run(params(&session_dir, directory.path())).await;

    assert!(result.unwrap_err().contains("exited before agent_settled"));
}

#[cfg(not(unix))]
#[test]
fn pi_failure_tests_require_unix() {}
