"""Tests for the gh-proxy HMAC gate.

Covers the token-holding sidecar's /healthz (no auth) and the signature gate
that guards every other route. The gate must read the raw request *bytes* and
verify them, not pass `request.body` (a coroutine) straight into hmac.
"""

from __future__ import annotations

import importlib
import json
import os
import time

import pytest
from fastapi.testclient import TestClient

from monkey.hmac import hmac_sign

HMAC_KEY = "test-gh-proxy-hmac"
TOKEN = "test-gh-proxy-token"


@pytest.fixture()
def client():
    """A gh-proxy TestClient with a real, valid HMAC key + token.

    The module reads its config from the environment at import time, so set the
    env vars and reload before handing out the client.
    """
    os.environ["GITHUB_TOKEN"] = TOKEN
    os.environ["MONKEY_GH_PROXY_HMAC_KEY"] = HMAC_KEY
    mod = importlib.import_module("monkey.gh_proxy.main")
    importlib.reload(mod)
    with TestClient(mod.app) as c:
        yield c


def _signed_post(client, path: str, body: dict, *, bad_sig: str | None = None):
    data = json.dumps(body).encode()
    ts = int(time.time())
    # The timestamp is bound into the signed payload ("{ts}:{body}"); a
    # signature over the bare body is rejected by the gate.
    sig = bad_sig if bad_sig is not None else hmac_sign(HMAC_KEY, data, ts)
    return client.post(
        path,
        content=data,
        headers={
            "Content-Type": "application/json",
            "x-monkey-sig": sig,
            "x-monkey-ts": str(ts),
        },
    )


def test_healthz_is_unauthenticated(client):
    resp = client.get("/healthz")
    assert resp.status_code == 200
    assert resp.json() == {"ok": True}


def test_missing_signature_is_rejected(client):
    resp = client.post(
        "/issues/acme/widget/1/comment",
        content=json.dumps({"body": "hi"}).encode(),
        headers={"Content-Type": "application/json"},
    )
    assert resp.status_code == 401


def test_bad_signature_is_rejected(client):
    resp = _signed_post(
        client, "/issues/acme/widget/1/comment", {"body": "hi"},
        bad_sig="sha256=" + "0" * 64,
    )
    assert resp.status_code == 401


def test_invalid_timestamp_is_rejected(client):
    ts = int(time.time()) - 300  # outside ±30s skew
    data = json.dumps({"body": "hi"}).encode()
    resp = client.post(
        "/issues/acme/widget/1/comment",
        content=data,
        headers={
            "Content-Type": "application/json",
            "x-monkey-sig": hmac_sign(HMAC_KEY, data, ts),
            "x-monkey-ts": str(ts),
        },
    )
    assert resp.status_code == 401


def test_replayed_signature_with_fresh_timestamp_is_rejected(client):
    """Regression: x-monkey-ts must be bound into the signature. A captured
    (body, signature) pair replayed with a fresh timestamp must 401."""
    data = json.dumps({"body": "hi"}).encode()
    old_ts = int(time.time()) - 3600
    captured_sig = hmac_sign(HMAC_KEY, data, old_ts)
    resp = client.post(
        "/issues/acme/widget/1/comment",
        content=data,
        headers={
            "Content-Type": "application/json",
            "x-monkey-sig": captured_sig,
            "x-monkey-ts": str(int(time.time())),
        },
    )
    assert resp.status_code == 401


def test_valid_signature_passes_gate(client, monkeypatch):
    """A correctly signed request must get past the gate (regression for the
    unawaited `request.body` bug that 500'd every request)."""
    import monkey.gh_proxy.main as mod

    async def fake_gh(method, path, **kw):
        return {"ok": True}

    monkeypatch.setattr(mod, "_gh", fake_gh)
    resp = _signed_post(client, "/issues/acme/widget/1/comment", {"body": "hi"})
    assert resp.status_code == 200
    assert resp.json() == {"ok": True}


def test_git_push_requires_repo(client):
    """`repo` must be validated alongside worktree/branch; a missing one must
    return 422 (not a KeyError -> 500)."""
    for missing in ("worktree", "branch", "repo"):
        body = {
            "worktree": "/tmp/wt",
            "branch": "farm/x",
            "repo": "acme/widget",
        }
        body.pop(missing)
        resp = _signed_post(client, "/git/push", body)
        assert resp.status_code == 422, f"missing {missing} should be 422"


def test_git_push_accepts_valid_fields(client, monkeypatch):
    """A signed /git/push with all three fields must reach the subprocess call
    without a validation error (monkeypatch the push to avoid real git)."""
    import subprocess
    import monkey.gh_proxy.main as mod

    calls = {}

    def fake_run(args, **kw):
        calls["args"] = args
        calls["kw"] = kw

    monkeypatch.setattr(mod.subprocess, "run", fake_run)
    resp = _signed_post(
        client,
        "/git/push",
        {"worktree": "/tmp/wt", "branch": "farm/x", "repo": "acme/widget"},
    )
    assert resp.status_code == 200
    # The remote must embed the validated repo, not raise KeyError on it.
    # args: ["git","-C",worktree,"push","-f",remote,"HEAD:branch"]
    assert calls["args"][5] == "https://x-access-token:test-gh-proxy-token@github.com/acme/widget.git"
