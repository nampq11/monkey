"""Tests for write-back gates and PR body validation."""

import pytest

from monkey.gh_writeback import _has_required_headers


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


def test_accepts_close_and_resolve_references():
    assert _has_required_headers(GOOD_BODY.replace("Fixes #123", "Closes #123"), 123) is True
    assert _has_required_headers(GOOD_BODY.replace("Fixes #123", "Resolves #456"), 123) is True
