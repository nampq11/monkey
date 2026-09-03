"""Tests for HMAC verification and internal signature replay protection."""

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
    verify_internal_signature(SECRET, BODY, sig, timestamp_header=str(ts))


def test_internal_signature_replay_rejected():
    ts = int(time.time()) - 100  # outside ±30s skew
    sig = hmac_sign(SECRET, BODY, ts)
    with pytest.raises(BadSignature):
        verify_internal_signature(SECRET, BODY, sig, timestamp_header=str(ts))


def test_internal_signature_replay_with_fresh_timestamp_rejected():
    """Regression: the timestamp must be bound into the signed payload.

    An attacker who captures (body, signature) must not be able to replay it
    later with a fresh x-monkey-ts header; swapping the timestamp must break
    the signature itself.
    """
    old_ts = int(time.time()) - 3600
    captured_sig = hmac_sign(SECRET, BODY, old_ts)
    fresh_ts = int(time.time())  # inside the ±30s skew window
    with pytest.raises(BadSignature):
        verify_internal_signature(
            SECRET, BODY, captured_sig, timestamp_header=str(fresh_ts)
        )


def test_internal_signature_without_timestamp_binding_rejected():
    """A signature computed over the bare body (timestamp not bound) must not
    pass internal verification even with a current timestamp header."""
    unbound_sig = hmac_sign(SECRET, BODY)
    with pytest.raises(BadSignature):
        verify_internal_signature(
            SECRET, BODY, unbound_sig, timestamp_header=str(int(time.time()))
        )
