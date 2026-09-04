use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox command failed: {0}")]
    CommandFailed(String),
    #[error("sandbox io error: {0}")]
    Io(String),
}

pub fn slug(text: &str) -> String {
    static SEPARATOR: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"[^a-z0-9]+").expect("slug regex must compile"));

    let lower = text.to_lowercase();
    let replaced = SEPARATOR.replace_all(&lower, "-");
    let trimmed = replaced.trim_matches('-');
    if trimmed.is_empty() {
        "issue".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn farm_dir(owner: &str, repo: &str, number: i64) -> String {
    let key = format!("{owner}/{repo}#{number}");
    let mut hasher = Sha256::new();
    hasher.update(key.as_bytes());
    let hex_digest = hex::encode(hasher.finalize());
    hex_digest[..8].to_string()
}

pub async fn ensure_workspace(
    workspaces_root: &Path,
    repo_url: &str,
    owner: &str,
    repo: &str,
    number: i64,
    default_branch: &str,
) -> Result<PathBuf, SandboxError> {
    let base = workspaces_root.join(format!("{}__{}__{}", owner, repo, number));
    let worktree = base.join("repo");
    let branch = format!("farm/{}/{}", farm_dir(owner, repo, number), slug(repo));

    if worktree.exists() && worktree.join(".git").exists() {
        return Ok(worktree);
    }

    std::fs::create_dir_all(workspaces_root).map_err(|e| SandboxError::Io(e.to_string()))?;

    let mirror = workspaces_root.join(format!("{}__{}.git", owner, repo));
    let (Some(mirror_str), Some(worktree_str)) = (mirror.to_str(), worktree.to_str()) else {
        return Err(SandboxError::Io(
            "workspace path is not valid UTF-8".to_string(),
        ));
    };
    if mirror.exists() {
        // Refresh an existing mirror so new worktrees are based on the
        // remote's current default branch instead of the state at clone time.
        run_git_cmd(&["-C", mirror_str, "remote", "update", "--prune"])
            .await
            .map_err(|error| {
                SandboxError::CommandFailed(format!(
                    "failed to refresh existing mirror at {} for repository {}: {}",
                    mirror.display(),
                    repo_url,
                    error
                ))
            })?;
    } else {
        // Cloning a large mirror can take minutes; the async git command keeps
        // the Tokio worker free while git runs.
        run_git_cmd(&["clone", "--mirror", repo_url, mirror_str]).await?;
    }

    // Base branch off refs/heads/<default_branch>
    let default_ref = format!("refs/heads/{}", default_branch);
    run_git_cmd(&["-C", mirror_str, "branch", "-f", &branch, &default_ref]).await?;

    std::fs::create_dir_all(&base).map_err(|e| SandboxError::Io(e.to_string()))?;
    run_git_cmd(&[
        "-C",
        mirror_str,
        "worktree",
        "add",
        "-f",
        worktree_str,
        &branch,
    ])
    .await?;

    Ok(worktree)
}

pub async fn cleanup_workspace(
    workspaces_root: &Path,
    owner: &str,
    repo: &str,
    number: i64,
) -> Result<(), SandboxError> {
    let base = workspaces_root.join(format!("{}__{}__{}", owner, repo, number));
    let worktree = base.join("repo");
    let mirror = workspaces_root.join(format!("{}__{}.git", owner, repo));

    if worktree.exists() {
        let (Some(mirror_str), Some(worktree_str)) = (mirror.to_str(), worktree.to_str()) else {
            return Err(SandboxError::Io(
                "workspace path is not valid UTF-8".to_string(),
            ));
        };
        run_git_cmd(&[
            "-C",
            mirror_str,
            "worktree",
            "remove",
            "--force",
            worktree_str,
        ])
        .await?;
    }

    if base.exists() {
        std::fs::remove_dir_all(&base).map_err(|error| {
            SandboxError::Io(format!(
                "failed to remove workspace {}: {}",
                base.display(),
                error
            ))
        })?;
    }

    Ok(())
}

async fn run_git_cmd(args: &[&str]) -> Result<String, SandboxError> {
    let output = Command::new("git")
        .args(args)
        .output()
        .await
        .map_err(|e| SandboxError::CommandFailed(format!("failed to spawn git: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SandboxError::CommandFailed(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    async fn run_test_git(repo: &Path, args: &[&str]) {
        let mut all_args = vec!["-C", repo.to_str().expect("test repo path is UTF-8")];
        all_args.extend_from_slice(args);
        run_git_cmd(&all_args)
            .await
            .expect("test git command must succeed");
    }

    async fn commit_file(repo: &Path, file: &str, contents: &str, message: &str) {
        fs::write(repo.join(file), contents).expect("failed to write test file");
        run_test_git(repo, &["add", file]).await;
        run_test_git(
            repo,
            &[
                "-c",
                "user.name=monkey",
                "-c",
                "user.email=monkey@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                message,
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn refreshes_existing_mirror_when_remote_default_branch_advances() {
        let upstream_root = tempfile::tempdir().expect("failed to create upstream tempdir");
        let upstream = upstream_root.path().join("upstream");
        run_test_git(upstream_root.path(), &["init", "-b", "main", "upstream"]).await;
        commit_file(&upstream, "state.txt", "revision-1\n", "revision 1").await;

        let workspaces = tempfile::tempdir().expect("failed to create workspaces tempdir");
        let repo_url = upstream.to_str().expect("upstream path is UTF-8");

        let first = ensure_workspace(workspaces.path(), repo_url, "acme", "widgets", 1, "main")
            .await
            .expect("first workspace preparation must succeed");
        assert_eq!(
            fs::read_to_string(first.join("state.txt")).expect("failed to read state file"),
            "revision-1\n"
        );

        // The remote default branch advances after the mirror was cloned.
        commit_file(&upstream, "state.txt", "revision-2\n", "revision 2").await;

        // A new issue for the same repository reuses the shared mirror.
        let second = ensure_workspace(workspaces.path(), repo_url, "acme", "widgets", 2, "main")
            .await
            .expect("second workspace preparation must succeed");
        assert_eq!(
            fs::read_to_string(second.join("state.txt")).expect("failed to read state file"),
            "revision-2\n"
        );
    }

    #[tokio::test]
    async fn prepares_workspace_for_non_main_default_branch() {
        let upstream_root = tempfile::tempdir().expect("failed to create upstream tempdir");
        let upstream = upstream_root.path().join("upstream");
        run_test_git(upstream_root.path(), &["init", "-b", "master", "upstream"]).await;
        commit_file(&upstream, "state.txt", "on-master\n", "revision 1").await;

        let workspaces = tempfile::tempdir().expect("failed to create workspaces tempdir");
        let repo_url = upstream.to_str().expect("upstream path is UTF-8");

        let worktree = ensure_workspace(workspaces.path(), repo_url, "acme", "widgets", 7, "master")
            .await
            .expect("workspace preparation for a master-default repository must succeed");
        assert_eq!(
            fs::read_to_string(worktree.join("state.txt")).expect("failed to read state file"),
            "on-master\n"
        );

        let branch = run_test_git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]).await;
        assert_eq!(
            branch.trim(),
            format!(
                "farm/{}/{}",
                farm_dir("acme", "widgets", 7),
                slug("widgets")
            )
        );
    }
}
