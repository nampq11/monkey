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
    # The remote is a plain URL embedding the validated repo (no token), and
    # the push targets the requested branch.
    assert "https://github.com/acme/widget.git" in calls["args"]
    assert "HEAD:farm/x" in calls["args"]
    # Credentials ride along in the env for the dynamic credential helper.
    assert calls["kw"]["env"]["GIT_TOKEN"] == TOKEN


def test_git_push_args_contain_no_secret_material(client, monkeypatch):
    """Regression: the raw token (or any encoding of it) must never appear in
    the git command arguments, which are world-readable via `ps aux` /
    /proc/<pid>/cmdline."""
    import base64
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
    argv = " ".join(calls["args"])
    encodings = {
        "raw": TOKEN,
        "url-encoded": TOKEN.replace("-", "%2D"),
        "base64(x-access-token:token)": base64.b64encode(
            f"x-access-token:{TOKEN}".encode()
        ).decode(),
        "base64(token)": base64.b64encode(TOKEN.encode()).decode(),
    }
    for name, secret in encodings.items():
        assert secret not in argv, f"{name} token leaked into git argv"


def test_credential_helper_supplies_token():
    """The inline credential helper (invoked by git via `sh -c`) must yield
    username=x-access-token and the token from $GIT_TOKEN -- proving auth still
    works even though the token never touches the command line."""
    import subprocess
    import monkey.gh_proxy.main as mod

    env = dict(os.environ)
    env["GIT_TOKEN"] = TOKEN
    env["GIT_TERMINAL_PROMPT"] = "0"
    proc = subprocess.run(
        [
            "git",
            "-c",
            "credential.helper=",
            "-c",
            f"credential.helper={mod._CRED_HELPER}",
            "credential",
            "fill",
        ],
        input="protocol=https\nhost=github.com\n\n",
        capture_output=True,
        text=True,
        env=env,
        check=True,
    )
    out = dict(
        line.split("=", 1) for line in proc.stdout.strip().splitlines() if "=" in line
    )
    assert out["username"] == "x-access-token"
    assert out["password"] == TOKEN
