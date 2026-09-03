"""Tests for write-back gates and PR body validation."""

import pytest

from monkey.gh_writeback import _has_required_headers, _open_pr_if_gated


GOOD_BODY = """## Repro
always

## Cause
root

## Fix
patch

## Verification
test passes

Fixes #123
"""


def test_required_headers_pass_when_present():
    assert _has_required_headers(GOOD_BODY, 123) is True


def test_missing_section_fails():
    bad = GOOD_BODY.replace("## Fix", "Fix")
    assert _has_required_headers(bad, 123) is False


def test_missing_reference_fails():
    bad = GOOD_BODY.replace("Fixes #123", "Addresses #123")
    assert _has_required_headers(bad, 123) is False


def test_accepts_close_and_resolve_on_the_same_issue():
    assert _has_required_headers(GOOD_BODY.replace("Fixes #123", "Closes #123"), 123) is True
    assert _has_required_headers(GOOD_BODY.replace("Fixes #123", "Resolves #123"), 123) is True


def test_rejects_reference_to_a_different_issue():
    assert _has_required_headers(GOOD_BODY.replace("Fixes #123", "Resolves #456"), 123) is False


class _FakeProxy:
    def __init__(self):
        self.comments = []
        self.pushed = []
        self.prs = []

    async def add_issue_comment(self, body):
        self.comments.append(body)

    async def push(self, worktree, branch):
        self.pushed.append((str(worktree), branch))

    async def open_pull_request(self, body):
        self.prs.append(body)
        return {"number": 1}


class _Outcome:
    def __init__(self, body, branch):
        self.pr_body = body
        self.summary = "Fix the bug"
        self.branch = branch


@pytest.mark.asyncio
async def test_open_pr_requires_a_real_branch():
    proxy = _FakeProxy()
    result = await _open_pr_if_gated(
        proxy, _Outcome(GOOD_BODY, ""), "acme", "widget", 123, worktree="/wt"
    )
    assert result == {"action": "comment_fallback", "reason": "missing_branch"}
    assert proxy.prs == []
    assert proxy.pushed == []
    assert proxy.comments == [GOOD_BODY.strip()]


@pytest.mark.asyncio
async def test_open_pr_pushes_branch_then_opens():
    proxy = _FakeProxy()
    branch = "farm/abc1234/widget"
    result = await _open_pr_if_gated(
        proxy, _Outcome(GOOD_BODY, branch), "acme", "widget", 123, worktree="/wt"
    )
    assert result["action"] == "open_pr"
    assert proxy.pushed == [("/wt", branch)]
    assert proxy.prs == [
        {"title": "Fix the bug", "head": branch, "base": "main", "body": GOOD_BODY}
    ]
