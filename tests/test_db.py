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


def test_store_enables_wal_and_busy_timeout(tmp_path):
    """The store is opened with WAL + busy_timeout so concurrent writers (webhook
    thread + worker loop on the same file) queue instead of throwing
    'database is locked'."""
    s = _store(tmp_path)
    try:
        mode = s.conn.execute("PRAGMA journal_mode").fetchone()[0]
        assert mode.lower() == "wal"
        busy = s.conn.execute("PRAGMA busy_timeout").fetchone()[0]
        assert busy == 5000
    finally:
        s.close()
