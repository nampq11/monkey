"""Tests for the GHProxy host-tool client (signs + sends to gh-proxy)."""

import pytest

from monkey.host_tools import GHProxy


class _FakeStore:
    def audit_tool_call(self, *a, **k):
        pass


@pytest.mark.asyncio
async def test_push_builds_the_right_request(monkeypatch):
    proxy = GHProxy("http://gh-proxy:8080", "key", _FakeStore(), "acme", "widget", 123)
    captured = {}

    async def fake_call(method, path, *, json=None):
        captured["method"] = method
        captured["path"] = path
        captured["json"] = json
        return {"ok": True}

    monkeypatch.setattr(proxy, "_call", fake_call)

    result = await proxy.push("/data/wt", "farm/abc1234/widget")

    assert result == {"ok": True}
    assert captured["method"] == "POST"
    assert captured["path"] == "/git/push"
    assert captured["json"] == {
        "worktree": "/data/wt",
        "branch": "farm/abc1234/widget",
        "repo": "acme/widget",
    }
