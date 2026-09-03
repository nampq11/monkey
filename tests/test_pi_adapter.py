"""Regression tests for the pi adapter's child-environment scrubbing.

The spawned `pi` process must never see GITHUB_* or MONKEY_* variables
(tokens, webhook secrets, HMAC keys, DB URLs, ...), but benign variables
must be preserved.
"""

from pathlib import Path

from monkey.adapters.pi import _SpawnedPi


def test_spawned_pi_scrubs_github_and_monkey_env_vars(monkeypatch):
    monkeypatch.setenv("GITHUB_TOKEN", "ghp-super-secret-token")
    monkeypatch.setenv("GITHUB_WEBHOOK_SECRET", "super-secret-webhook")
    monkeypatch.setenv("MONKEY_GH_PROXY_HMAC_KEY", "hmac-key-secret")
    monkeypatch.setenv("MONKEY_DB_URL", "postgres://secret")
    monkeypatch.setenv("MONKEY_BOT_LOGIN", "robot-bot")
    monkeypatch.setenv("BENIGN_VAR", "keep-me")

    # `/usr/bin/env` dumps its own environment: a stand-in for `pi` that lets
    # us assert exactly what the child process received.
    proc = _SpawnedPi("/usr/bin/env", [], cwd=Path("/tmp"))
    try:
        out = proc.proc.stdout.read().decode()
    finally:
        proc.close()

    leaked = [
        line
        for line in out.splitlines()
        if line.startswith(("GITHUB_", "MONKEY_"))
    ]
    assert leaked == [], f"sensitive vars leaked to child: {leaked}"
    assert "BENIGN_VAR=keep-me" in out.splitlines()
