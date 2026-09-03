"""HMAC-SHA256 signature verification for GitHub webhooks and internal gh-proxy calls.

GitHub webhook verification is a constant-time compare of the X-Hub-Signature-256
value against the raw request body; GitHub payloads carry no timestamp, so there is
no replay window here (replays are deduplicated on the X-GitHub-Delivery id). Internal
monkey <-> gh-proxy requests add a timestamp header and enforce a skew window so
replays are rejected. Bad signatures are returned as 401 (never 5xx) so GitHub stops
retrying.
"""

from __future__ import annotations

import hashlib
import hmac
import time


class BadSignature(Exception):
    """Raised when a signature is missing, malformed, or does not match."""


def _constant_time_equal(a: str, b: str) -> bool:
    return hmac.compare_digest(a.encode(), b.encode())


def verify_github_signature(
    secret: str,
    body: bytes,
    signature_header: str | None,
) -> None:
    """Verify an X-Hub-Signature-256 header against the raw request body.

    Raises BadSignature on mismatch. The body must be the *raw* bytes exactly as
    received (do not re-encode a parsed object, or the signature will differ).
    """
    if not signature_header:
        raise BadSignature("missing signature header")
    if not signature_header.startswith("sha256="):
        raise BadSignature("unsupported signature scheme")

    expected = hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    provided = signature_header.removeprefix("sha256=")

    if not _constant_time_equal(expected, provided):
        raise BadSignature("signature mismatch")


def hmac_sign(secret: str, body: bytes, timestamp: int | None = None) -> str:
    """Produce an X-Hub-Signature-256 value for a given body (for tests / proxy).

    For internal monkey <-> gh-proxy calls pass `timestamp` so it is bound into
    the signed payload ("{ts}:{body}"); a signature that ignores the timestamp
    would let captured requests be replayed forever by refreshing x-monkey-ts.
    """
    if timestamp is not None:
        body = f"{timestamp}:{body.decode()}".encode()
    return "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()


def verify_internal_signature(
    key: str,
    body: bytes,
    signature_header: str | None,
    *,
    skew_seconds: int = 30,
    timestamp_header: str | None = None,
) -> None:
    """Verify an internal monkey <-> gh-proxy signed request.

    Unlike GitHub webhooks this also checks a timestamp header to reject
    replay across a ±skew_seconds window. The header encodes the expiry epoch.
    """
    if not signature_header:
        raise BadSignature("missing signature header")
    if not timestamp_header:
        raise BadSignature("missing timestamp header")

    try:
        ts = int(timestamp_header)
    except ValueError as exc:
        raise BadSignature("bad timestamp") from exc

    now = time.time()
    if abs(now - ts) > skew_seconds:
        raise BadSignature("timestamp outside skew window")

    # The timestamp must be part of the MAC, or the skew check above would be
    # decoupled from the signature: any captured request could be replayed
    # forever simply by refreshing x-monkey-ts within the ±skew window.
    expected = hmac.new(key.encode(), f"{ts}:{body.decode()}".encode(), hashlib.sha256).hexdigest()
    provided = signature_header.removeprefix("sha256=")
    if not _constant_time_equal(expected, provided):
        raise BadSignature("signature mismatch")
