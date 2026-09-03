"""Disptach: classify a GitHub event and build the prompt for the engine.

Mirrors roboomp's branching:
  - bug / documentation -> reproduce, fix on a fresh branch, open a PR whose body
    has ## Repro / ## Cause / ## Fix / ## Verification + "Fixes #N"
  - question           -> one comment (no PR); auto-close after autoclose hours
    unless the author reacts with a downvote
  - enhancement / proposal -> one comment, no PR
  - invalid / duplicate    -> one brief comment

The PR body/comment/classification state is returned so the caller (or a gh
writeback step) can push to GitHub through the proxy.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field


@dataclass
class Task:
    kind: str                 # fix | comment | answer | invalid | skip
    prompt: str
    pr_body: str = ""
    comment: str = ""
    labels: list[str] = field(default_factory=list)
    autoclose: bool = False


def classify_and_build_task(event_type: str, payload: dict, settings) -> Task | None:
    """Return a Task for the engine, or None to skip (no work)."""
    issue = payload.get("issue") or payload.get("pull_request") or {}
    title = issue.get("title", "")
    body = issue.get("body") or ""
    labels = [l.get("name") for l in issue.get("labels") or []]

    combined = f"{title}\n{body}".lower()

    if "question" in labels or "?" in title:
        return Task(
            kind="answer",
            prompt=_question_prompt(title, body),
            labels=["question"],
        )
    if "invalid" in labels:
        return Task(kind="invalid", prompt=_invalid_prompt(title, body), labels=["invalid"])
    if "duplicate" in labels:
        return Task(kind="invalid", prompt=_duplicate_prompt(title, body), labels=["duplicate"])

    is_bug = _has_any(combined, ["bug", "error", "crash", "fail", "broken", "exception", "regression"])
    is_doc = _has_any(combined, ["documentation", "doc", "typo", "readme", "docs"])

    if is_bug or is_doc:
        return Task(
            kind="fix",
            prompt=fix_prompt(title, body),
            labels=["bug" if is_bug else "documentation"],
        )

    is_enh = _has_any(combined, ["feature", "enhancement", "proposal", "suggestion", "request"])
    if is_enh:
        return Task(
            kind="comment",
            prompt=_enhancement_prompt(title, body),
            labels=["enhancement"],
        )

    # Fallback: enhancement-ish default (comment only).
    return Task(kind="comment", prompt=_enhancement_prompt(title, body))


# ---------------------------------------------------------------------------
# Prompts. Each instructs the agent to emit a structured result so a following
# step can turn it into a PR body / comment + run the pre-merge gates.
# ---------------------------------------------------------------------------


def fix_prompt(title: str, body: str) -> str:
    return (
        f"You are helping triage a GitHub issue in this repository.\n"
        f"Title: {title}\n\nBody:\n{body}\n\n"
        "If this is a bug or documentation issue, REPRODUCE it, find the cause, "
        "fix it on the current branch, and verify. Then finalize with a clear "
        "structured report using exactly these sections:\n"
        "## Repro\n## Cause\n## Fix\n## Verification\n"
        "End with a line: Fixes #<issue-number>. Do not open the PR yourself; "
        "just produce the body and leave the branch/commits ready."
    )


def _question_prompt(title: str, body: str) -> str:
    return (
        f"A user asked a question in this repository. Answer helpfully and "
        f"concisely, citing the relevant code where possible.\n"
        f"Question: {title}\n\n{body}\n\n"
        "Produce your answer as a single comment below."
    )


def _invalid_prompt(title: str, body: str) -> str:
    return (
        f"This issue was marked invalid. Briefly (1-2 sentences) explain why "
        f"and what the reporter should do instead.\nTitle: {title}\n\n{body}"
    )


def _duplicate_prompt(title: str, body: str) -> str:
    return (
        f"This issue was marked duplicate. Briefly point the reporter to the "
        f"existing (duplicate) thread.\nTitle: {title}\n\n{body}"
    )


def _enhancement_prompt(title: str, body: str) -> str:
    return (
        f"This is a feature/proposal. Acknowledge it and summarize the request, "
        f"note feasibility or a plan, without making changes.\n"
        f"Title: {title}\n\n{body}\n\n"
        "Produce your response as a single comment below."
    )


def _has_any(text: str, keywords: list[str]) -> bool:
    return any(k in text for k in keywords)
