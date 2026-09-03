"""Git worktree sandbox.

Each issue gets its own git worktree so the coding agent can edit code with a
clean checkout on a dedicated branch, never touching the default branch. This
mirrors roboomp's per-issue `farm/<8hex>/<slug>` scheme.

Layout: <workspaces_root>/<owner>__<repo>__<n>/repo   (the worktree checkout)
"""

from __future__ import annotations

import hashlib
import re
import subprocess
from pathlib import Path


class SandboxError(Exception):
    pass


def _slug(text: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-") or "issue"


def _farm_dir(owner: str, repo: str, number: int) -> str:
    key = f"{owner}/{repo}#{number}"
    return hashlib.sha256(key.encode()).hexdigest()[:8]


def ensure_workspace(
    workspaces_root: Path,
    repo_url: str,
    owner: str,
    repo: str,
    number: int,
    *,
    default_branch: str = "main",
) -> Path:
    """Create a git worktree for one issue and return the checkout path.

    Clones lazily if the repo isn't on disk yet, then adds a worktree on a fresh
    branch `farm/<hex>/<slug>`. Idempotent: returns the existing worktree if the
    branch already exists.
    """
    base = workspaces_root / f"{owner}__{repo}__{number}"
    worktree = base / "repo"
    branch = f"farm/{_farm_dir(owner, repo, number)}/{_slug(repo)}"

    if worktree.exists() and (worktree / ".git").exists():
        return worktree

    workspaces_root.mkdir(parents=True, exist_ok=True)

    # 1. Clone into a bare-ish cache path if needed (reuse across issues).
    mirror = workspaces_root / f"{owner}__{repo}.git"
    if not mirror.exists():
        _run(["git", "clone", "--mirror", repo_url, str(mirror)])

    # 2. Create worktree from the mirror on a dedicated branch.
    # A `git clone --mirror` stores branches as refs/heads/* (no origin/remote
    # tracking refs), so base the new branch on refs/heads/<default_branch>
    # rather than origin/<default_branch>.
    _run(["git", "-C", str(mirror), "branch", "-f", branch, f"refs/heads/{default_branch}"])
    base.mkdir(parents=True, exist_ok=True)
    _run(["git", "-C", str(mirror), "worktree", "add", "-f", str(worktree), branch])

    return worktree


def cleanup_workspace(workspaces_root: Path, owner: str, repo: str, number: int) -> None:
    """Remove the worktree checkout for an issue (release disk space)."""
    base = workspaces_root / f"{owner}__{repo}__{number}"
    worktree = base / "repo"
    mirror = workspaces_root / f"{owner}__{repo}.git"
    branch = f"farm/{_farm_dir(owner, repo, number)}/{_slug(repo)}"

    try:
        if worktree.exists():
            _run(["git", "-C", str(mirror), "worktree", "remove", "--force", str(worktree)])
    except SandboxError:
        pass
    _rmtree(base)


def _run(cmd: list[str]) -> str:
    try:
        proc = subprocess.run(cmd, check=True, capture_output=True, text=True)
    except subprocess.CalledProcessError as exc:
        raise SandboxError(f"command failed: {' '.join(cmd)}\n{exc.stderr}") from exc
    return proc.stdout


def _rmtree(path: Path) -> None:
    import shutil

    shutil.rmtree(path, ignore_errors=True)
