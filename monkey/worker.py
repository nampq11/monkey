"""Worker: claims pending events and drives the engine adapter.

Serialization is per (owner, repo, number) so two events on the same issue never
run concurrently against the same worktree. A single background worker
(asyncio task) is sufficient for v1's low volume; the in-process _inflight set
prevents double-claiming.
"""

from __future__ import annotations

import asyncio
import json
import logging
from pathlib import Path

from .config import get_settings
from .db import Store
from .dispatch import classify_and_build_task
from .gh_writeback import write_back
from .sandbox import cleanup_workspace, ensure_workspace

log = logging.getLogger("monkey.worker")

_INFLIGHT: set[tuple[str, str, int]] = set()
_LOCK = asyncio.Lock()


async def worker_loop(store: Store, adapter) -> None:
    settings = get_settings()
    while True:
        rows = store.get_pending(limit=50)
        for row in rows:
            key = (row["owner"], row["repo"], row["number"])
            async with _LOCK:
                if key in _INFLIGHT:
                    continue
                _INFLIGHT.add(key)
            try:
                await _handle(store, adapter, row, settings)
            except Exception as exc:  # noqa: BLE001
                log.exception("failed handling %s", row["delivery_id"])
                store.fail(row["delivery_id"], str(exc))
            finally:
                async with _LOCK:
                    _INFLIGHT.discard(key)
        await asyncio.sleep(1)


async def _handle(store, adapter, row, settings) -> None:
    payload = json.loads(row["payload"])
    owner, repo, number = row["owner"], row["repo"], row["number"]

    # 1. sandbox: per-issue worktree.
    repo_url = _repo_url(owner, repo)
    worktree = ensure_workspace(
        Path(settings.workspaces_root), repo_url, owner, repo, number
    )

    # 2. build the task prompt from the event + classification.
    task = classify_and_build_task(row["event_type"], payload, settings)
    if task is None:
        store.done(row["delivery_id"])
        return

    # 3. drive the engine adapter (fresh session for a first event).
    session_dir = Path(settings.session_dir) / f"{owner}__{repo}__{number}"
    outcome = await asyncio.to_thread(
        adapter.run,
        task.prompt,
        worktree,
        session_dir=session_dir,
        model=settings.models[0] if settings.models else "",
        thinking=settings.thinking,
        provider=settings.provider,
        timeout_seconds=3600,
    )

    # 4. persist artifacts + propagate the task kind for write-back.
    _write_outcome(session_dir, outcome, kind=task.kind)
    # 5. write back to GitHub via the token proxy.
    try:
        await write_back(outcome, task.kind, owner=owner, repo=repo, number=number, store=store)
    except Exception as exc:  # noqa: BLE001
        log.exception("write-back failed for %s", row["delivery_id"])

    store.done(row["delivery_id"], str(session_dir))
    # Free disk for this issue now that it's done.
    cleanup_workspace(Path(settings.workspaces_root), owner, repo, number)


def _repo_url(owner: str, repo: str) -> str:
    return f"https://github.com/{owner}/{repo}.git"


def _write_outcome(session_dir: Path, outcome, *, kind: str = "") -> None:
    session_dir.mkdir(parents=True, exist_ok=True)
    artifact = {
        "kind": kind,
        "status": outcome.status,
        "summary": outcome.summary,
        "pr_body": outcome.pr_body,
        "comment": outcome.comment,
        "branch": outcome.branch,
    }
    (session_dir / "outcome.json").write_text(
        json.dumps(artifact, indent=2), encoding="utf-8"
    )
