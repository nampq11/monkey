"""Tests for HMAC verification and internal signature replay protection."""

import hashlib
import time

import pytest

from monkey.hmac import (
    BadSignature,
    hmac_sign,
    verify_github_signature,
    verify_internal_signature,
)

SECRET = "test-secret"
BODY = b'{"action":"opened","issue":{"number":123}}'


def test_valid_github_signature_passes():
    sig = hmac_sign(SECRET, BODY)
    verify_github_signature(SECRET, BODY, sig)  # no raise


def test_wrong_secret_raises():
    sig = hmac_sign("other-secret", BODY)
    with pytest.raises(BadSignature):
        verify_github_signature(SECRET, BODY, sig)


def test_tampered_body_raises():
    sig = hmac_sign(SECRET, BODY)
    with pytest.raises(BadSignature):
        verify_github_signature(SECRET, BODY + b"x", sig)


def test_missing_header_raises():
    with pytest.raises(BadSignature):
        verify_github_signature(SECRET, BODY, None)


def test_unsupported_scheme_raises():
    with pytest.raises(BadSignature):
        verify_github_signature(SECRET, BODY, "sha1=abc")


def test_internal_signature_within_skew_passes():
    ts = int(time.time())
    sig = hmac_sign(SECRET, BODY, ts)
    verify_internal_signature(SECRET, BODY, sig, timestamp_header=str(ts))  # no raise


def test_internal_signature_replay_rejected():
    ts = int(time.time()) - 100  # outside ±30s skew
    sig = hmac_sign(SECRET, BODY, ts)
    with pytest.raises(BadSignature):
        verify_internal_signature(SECRET, BODY, sig, timestamp_header=str(ts))


def test_internal_signature_replay_with_fresh_timestamp_rejected():
    """A captured (body, signature) pair must not verify under a refreshed
    x-monkey-ts: the timestamp is bound into the MAC, so the same signature
    only ever validates against the exact ts it was signed with."""
    captured_ts = int(time.time())
    captured_sig = hmac_sign(SECRET, BODY, captured_ts)
    fresh_ts = captured_ts + 5  # still well inside the ±30s skew window
    with pytest.raises(BadSignature):
        verify_internal_signature(SECRET, BODY, captured_sig, timestamp_header=str(fresh_ts))


def test_internal_signature_without_timestamp_binding_rejected():
    """A signature computed over the raw body alone (the pre-fix format) must
    be rejected now that verification MACs over \"<ts>:<body>\"."""
    ts = int(time.time())
    legacy_sig = hmac_sign(SECRET, BODY)  # body-only MAC, no timestamp bound
    with pytest.raises(BadSignature):
        verify_internal_signature(SECRET, BODY, legacy_sig, timestamp_header=str(ts))
