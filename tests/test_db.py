"""Tests for DB dedup and queue transitions."""

from pathlib import Path

from monkey.db import Store


def _store(tmp_path: Path) -> Store:
    return Store(tmp_path / "test.db")


def test_enqueue_new_event_inserts(tmp_path):
    s = _store(tmp_path)
    is_new = s.enqueue("d1", "issues", "acme", "widget", 1, "{}")
    assert is_new is True


def test_enqueue_dedup_returns_false(tmp_path):
    s = _store(tmp_path)
    s.enqueue("d1", "issues", "acme", "widget", 1, "{}")
    is_new = s.enqueue("d1", "issues", "acme", "widget", 1, "{}")
    assert is_new is False
    assert len(s.get_pending()) == 1


def test_claim_and_finish_flow(tmp_path):
    s = _store(tmp_path)
    s.enqueue("d1", "issues", "acme", "widget", 1, "{}")
    assert s.claim("d1") is True
    # second claim fails (already running)
    assert s.claim("d1") is False
    s.done("d1", "/data/sessions/x")
    assert len(s.get_pending()) == 0


def test_audit_tool_call_recorded(tmp_path):
    s = _store(tmp_path)
    s.audit_tool_call("acme", "widget", 1, "/issues/1/comment", "{}", "{}")
    row = s.conn.execute("SELECT * FROM tool_calls").fetchone()
    assert row["tool"] == "/issues/1/comment"
