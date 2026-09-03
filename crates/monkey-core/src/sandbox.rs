use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("sandbox command failed: {0}")]
    CommandFailed(String),
    #[error("sandbox io error: {0}")]
    Io(String),
}

pub fn slug(text: &str) -> String {
    let re = Regex::new(r"[^a-z0-9]+").unwrap();
    let lower = text.to_lowercase();
    let replaced = re.replace_all(&lower, "-");
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

pub fn ensure_workspace(
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
    if !mirror.exists() {
        run_git_cmd(&["clone", "--mirror", repo_url, mirror.to_str().unwrap()])?;
    }

    // Base branch off refs/heads/<default_branch>
    let mirror_str = mirror.to_str().unwrap();
    let default_ref = format!("refs/heads/{}", default_branch);
    run_git_cmd(&["-C", mirror_str, "branch", "-f", &branch, &default_ref])?;

    std::fs::create_dir_all(&base).map_err(|e| SandboxError::Io(e.to_string()))?;

    let worktree_str = worktree.to_str().unwrap();
    run_git_cmd(&[
        "-C",
        mirror_str,
        "worktree",
        "add",
        "-f",
        worktree_str,
        &branch,
    ])?;

    Ok(worktree)
}

pub fn cleanup_workspace(workspaces_root: &Path, owner: &str, repo: &str, number: i64) {
    let base = workspaces_root.join(format!("{}__{}__{}", owner, repo, number));
    let worktree = base.join("repo");
    let mirror = workspaces_root.join(format!("{}__{}.git", owner, repo));

    if worktree.exists()
        && let Some(mirror_str) = mirror.to_str()
        && let Some(worktree_str) = worktree.to_str()
    {
        let _ = run_git_cmd(&[
            "-C",
            mirror_str,
            "worktree",
            "remove",
            "--force",
            worktree_str,
        ]);
    }
    let _ = std::fs::remove_dir_all(&base);
}

fn run_git_cmd(args: &[&str]) -> Result<String, SandboxError> {
    let output = Command::new("git")
        .args(args)
        .output()
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
