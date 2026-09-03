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
    sig = bad_sig if bad_sig is not None else hmac_sign(HMAC_KEY, data)
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
    data = json.dumps({"body": "hi"}).encode()
    resp = client.post(
        "/issues/acme/widget/1/comment",
        content=data,
        headers={
            "Content-Type": "application/json",
            "x-monkey-sig": hmac_sign(HMAC_KEY, data),
            "x-monkey-ts": str(int(time.time()) - 300),
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
