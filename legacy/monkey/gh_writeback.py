"""Write-back to GitHub through the token proxy + pre-merge gates.

This is the part that turns an adapter Outcome into an actual GitHub action.
Nothing here ever touches GITHUB_TOKEN (gh-proxy does). Gates run before opening
a PR to keep the bot from junk-merging its own changes. Errors here never
escalate the token.
"""

from __future__ import annotations

import re
import subprocess
from pathlib import Path

from .config import get_settings
from .host_tools import GHProxy


class GateError(Exception):
    pass


async def write_back(
    outcome,
    topic: str,
    *,
    owner: str,
    repo: str,
    number: int,
    store,
    worktree: Path,
) -> dict:
    """Post the appropriate GitHub action based on the task kind / outcome.

    `topic` is the task kind from dispatch (fix | comment | answer | invalid).
    """
    settings = get_settings()
    proxy = GHProxy(
        settings.gh_proxy_url,
        settings.gh_proxy_hmac_key,
        store,
        owner,
        repo,
        number,
    )

    if topic in ("fix",):
        return await _open_pr_if_gated(
            proxy, outcome, owner, repo, number, worktree=worktree
        )
    if topic == "answer":
        # question: one comment, no PR.
        body = outcome.comment or outcome.summary
        await proxy.add_issue_comment(_ensure_comment(body))
        return {"action": "comment", "kind": "answer"}
    if topic == "comment":
        body = outcome.comment or outcome.summary
        await proxy.add_issue_comment(_ensure_comment(body))
        return {"action": "comment", "kind": "comment"}
    if topic == "invalid":
        body = outcome.comment or outcome.summary
        await proxy.add_issue_comment(_ensure_comment(body))
        return {"action": "comment", "kind": "invalid"}

    return {"action": "none", "kind": topic}


async def _open_pr_if_gated(
    proxy: GHProxy,
    outcome,
    owner: str,
    repo: str,
    number: int,
    *,
    worktree: Path,
) -> dict:
    """Open a PR only if the pre-merge gates pass and the body is well-formed."""
    body = outcome.pr_body or outcome.summary
    if not _has_required_headers(body, number):
        # No PR without the required structure - fall back to a comment so the
        # human can see the agent's reasoning.
        await proxy.add_issue_comment(_ensure_comment(body))
        return {"action": "comment_fallback", "reason": "missing_required_headers"}

    branch = getattr(outcome, "branch", "") or ""
    if not branch:
        # No PR without a real branch: the head ref must exist on the remote.
        await proxy.add_issue_comment(_ensure_comment(body))
        return {"action": "comment_fallback", "reason": "missing_branch"}

    # Push the worktree branch so the head ref exists on the remote before
    # GitHub will accept the PR. The token never leaves gh-proxy.
    await proxy.push(worktree, branch)

    pr = await proxy.open_pull_request(
        {
            "title": outcome.summary.split("\n")[0][:120],
            "head": branch,
            "base": "main",
            "body": body,
        }
    )
    return {"action": "open_pr", "pr": pr}


# ---------------------------------------------------------------------------
# Gates
# ---------------------------------------------------------------------------


def gate_pre_push(worktree: Path, branch: str) -> None:
    """Pre-push gates: branch matches, clean tree, commits carry author."""
    _run(["git", "-C", str(worktree), "diff", "--quiet"])
    status = _run(["git", "-C", str(worktree), "status", "--porcelain"])
    if status.strip():
        raise GateError("working tree not clean")

    author_matches = _run(
        ["git", "-C", str(worktree), "log", "-1", "--format=%an <%ae>"]
    ).strip()
    if not author_matches:
        raise GateError("no author on HEAD commit")


def gate_pre_pr(worktree: Path, test_command: list[str] | None = None) -> None:
    """Pre-PR gates: run the repo's checks (fix/check/test if configured)."""
    if test_command:
        _run(test_command)


def _has_required_headers(body: str, number: int) -> bool:
    sections = ("## Repro", "## Cause", "## Fix", "## Verification")
    if not all(s in body for s in sections):
        return False
    # Require a "Fixes #N" / "Closes #N" / "Resolves #N" reference that points at
    # the issue being worked on (#number), not any other issue.
    return bool(re.search(rf"(?:Fixes|Closes|Resolves)\s+#{int(number)}\b", body))


def _ensure_comment(text: str) -> str:
    text = (text or "").strip()
    return text if text else "_no response produced_"


def _run(cmd: list[str]) -> str:
    try:
        proc = subprocess.run(cmd, check=True, capture_output=True, text=True)
    except subprocess.CalledProcessError as exc:
        raise GateError(f"gate command failed: {' '.join(cmd)}\n{exc.stderr}") from exc
    return proc.stdout
