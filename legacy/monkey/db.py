"""SQLite event queue + audit store.

Dedup on X-GitHub-Delivery via INSERT OR IGNORE, plus a persistent queue for
events and an audit trail for every host-tool invocation. Concurrency is
serialized on (owner, repo, number) at the worker layer, not here.
"""

from __future__ import annotations

import sqlite3
import time
import uuid
from pathlib import Path

SCHEMA = """
CREATE TABLE IF NOT EXISTS events (
    delivery_id   TEXT PRIMARY KEY,
    event_type    TEXT NOT NULL,
    owner         TEXT NOT NULL,
    repo          TEXT NOT NULL,
    number        INTEGER NOT NULL,
    payload       TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',  -- pending|running|done|error
    session_dir   TEXT,
    created_at    REAL NOT NULL,
    updated_at    REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS tool_calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    owner      TEXT NOT NULL,
    repo       TEXT NOT NULL,
    number     INTEGER NOT NULL,
    tool       TEXT NOT NULL,
    args       TEXT NOT NULL,   -- credential-redacted
    result     TEXT NOT NULL,   -- credential-redacted
    created_at REAL NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_status ON events(status);
CREATE INDEX IF NOT EXISTS idx_events_owner_repo_num ON events(owner, repo, number);
"""


class Store:
    def __init__(self, path: str | Path) -> None:
        self.path = str(path)
        # /data (and similar) may not exist when running without a mounted
        # volume (e.g. bare `docker run` or local dev). Create it so sqlite can
        # open the file.
        Path(self.path).parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(self.path, check_same_thread=False)
        self.conn.row_factory = sqlite3.Row
        # The webhook receiver (uvicorn thread) and the worker loop share the
        # same SQLite file. WAL plus a busy timeout lets concurrent writers
        # queue instead of throwing "database is locked". WAL also allows
        # readers to proceed while a write is in flight.
        self.conn.execute("PRAGMA journal_mode = WAL;")
        self.conn.execute("PRAGMA busy_timeout = 5000;")
        self.conn.execute("PRAGMA foreign_keys = ON;")
        self.conn.executescript(SCHEMA)
        self.conn.commit()

    def close(self) -> None:
        self.conn.close()

    # --- events -----------------------------------------------------------

    def enqueue(
        self,
        delivery_id: str,
        event_type: str,
        owner: str,
        repo: str,
        number: int,
        payload: str,
    ) -> bool:
        """Insert an event. Returns True if new, False if already seen."""
        now = time.time()
        try:
            cur = self.conn.execute(
                "INSERT OR IGNORE INTO events "
                "(delivery_id, event_type, owner, repo, number, payload, created_at, updated_at) "
                "VALUES (?,?,?,?,?,?,?,?)",
                (delivery_id, event_type, owner, repo, number, payload, now, now),
            )
            self.conn.commit()
            return cur.rowcount > 0
        except sqlite3.IntegrityError:
            return False

    def get_pending(self, limit: int = 100) -> list[sqlite3.Row]:
        cur = self.conn.execute(
            "SELECT * FROM events WHERE status='pending' ORDER BY created_at LIMIT ?",
            (limit,),
        )
        return cur.fetchall()

    def claim(self, delivery_id: str) -> bool:
        cur = self.conn.execute(
            "UPDATE events SET status='running', updated_at=? WHERE delivery_id=? AND status='pending'",
            (time.time(), delivery_id),
        )
        self.conn.commit()
        return cur.rowcount > 0

    def done(self, delivery_id: str, session_dir: str | None = None) -> None:
        self.conn.execute(
            "UPDATE events SET status='done', session_dir=?, updated_at=? WHERE delivery_id=?",
            (session_dir, time.time(), delivery_id),
        )
        self.conn.commit()

    def fail(self, delivery_id: str, error: str) -> None:
        self.conn.execute(
            "UPDATE events SET status='error', updated_at=? WHERE delivery_id=?",
            (time.time(), delivery_id),
        )
        self.conn.commit()

    # --- audit ------------------------------------------------------------

    def audit_tool_call(
        self, owner: str, repo: str, number: int, tool: str, args: str, result: str
    ) -> None:
        self.conn.execute(
            "INSERT INTO tool_calls (owner, repo, number, tool, args, result, created_at) "
            "VALUES (?,?,?,?,?,?,?)",
            (owner, repo, number, tool, args, result, time.time()),
        )
        self.conn.commit()


def new_id() -> str:
    return uuid.uuid4().hex
