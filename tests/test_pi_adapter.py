"""Tests for the pi RPC adapter, notably child-process env scrubbing."""

import json
import os
import sys
from pathlib import Path

import pytest

from monkey.adapters.pi import _SpawnedPi


def _child_environ(proc: _SpawnedPi) -> dict:
    stdout = proc.proc.stdout
    assert stdout is not None
    chunks = []
    while True:
        chunk = stdout.read1(65536)
        if not chunk:
            break
        chunks.append(chunk)
    proc.close()
    return json.loads(b"".join(chunks).decode())


def test_spawned_pi_scrubs_github_and_monkey_env_vars(monkeypatch: pytest.MonkeyPatch) -> None:
    """All GITHUB_* / MONKEY_* vars must be scrubbed from the pi child env."""
    monkeypatch.setenv("GITHUB_TOKEN", "secret-token")
    monkeypatch.setenv("GITHUB_WEBHOOK_SECRET", "secret-webhook")
    monkeypatch.setenv("MONKEY_GH_PROXY_HMAC_KEY", "secret-hmac")
    monkeypatch.setenv("MONKEY_BOT_LOGIN", "secret-bot")
    monkeypatch.setenv("BENIGN_VAR", "keep-me")

    proc = _SpawnedPi(
        sys.executable,
        ["-c", "import json,os; print(json.dumps(dict(os.environ)))"],
        cwd=Path.cwd(),
    )
    child_env = _child_environ(proc)

    leaked = [k for k in child_env if k.startswith(("GITHUB_", "MONKEY_"))]
    assert leaked == []
    assert child_env["BENIGN_VAR"] == "keep-me"
