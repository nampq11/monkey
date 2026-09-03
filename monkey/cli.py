"""Monkey CLI entrypoint.

Commands:
  monkey serve                    run the orchestrator (webhook + worker)
  monkey gh-proxy                 run the token-holding proxy service
  monkey triage <owner/repo#N>    manually triage one issue
  monkey status                   show queue state
  monkey cleanup <owner/repo#N>   remove a worktree
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys

from .config import get_settings
from .db import Store
from .worker import worker_loop


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(prog="monkey")
    sub = parser.add_subparsers(dest="cmd", required=True)

    sub.add_parser("serve", help="run orchestrator (webhook + worker)")

    sub.add_parser("gh-proxy", help="run token-holding proxy service")

    tri = sub.add_parser("triage", help="manually triage owner/repo#N")
    tri.add_argument("target", help="owner/repo#N")

    sub.add_parser("status", help="show queue state")

    cl = sub.add_parser("cleanup", help="remove a worktree")
    cl.add_argument("target", help="owner/repo#N")

    args = parser.parse_args(argv)

    if args.cmd == "serve":
        _serve()
    elif args.cmd == "gh-proxy":
        _gh_proxy()
    elif args.cmd == "triage":
        _triage(args.target)
    elif args.cmd == "status":
        _status()
    elif args.cmd == "cleanup":
        _cleanup(args.target)


def _serve() -> None:
    import uvicorn

    settings = get_settings()
    store = Store("/data/monkey.db")
    from .adapters import get_adapter
    from .webhook import app

    adapter = get_adapter()

    async def _run_worker():
        await worker_loop(store, adapter)

    # Run webhook + worker concurrently in one process.
    config = uvicorn.Config(app, host="0.0.0.0", port=8000, log_level="info")
    server = uvicorn.Server(config)

    async def _main():
        from .webhook import _env_store
        # Reuse the same store in the webhook app - patch it in.

        task = asyncio.create_task(_run_worker())
        await server.serve()
        task.cancel()

    asyncio.run(_main())


def _gh_proxy() -> None:
    import uvicorn

    from .gh_proxy.main import app

    uvicorn.run(app, host="0.0.0.0", port=8080)


def _triage(target: str) -> None:
    owner, repo_number = _split_target(target)
    store = Store("/data/monkey.db")
    # For manual triage, read the payload from the store's newest event for this issue.
    rows = store.conn.execute(
        "SELECT * FROM events WHERE owner=? AND repo=? AND number=? ORDER BY created_at DESC LIMIT 1",
        (owner, repo_number[0], repo_number[1]),
    ).fetchone()
    if rows is None:
        print("no event found for", target)
        sys.exit(1)
    print(json.dumps(dict(rows), indent=2, default=str))


def _status() -> None:
    store = Store("/data/monkey.db")
    cur = store.conn.execute(
        "SELECT status, count(*) AS n FROM events GROUP BY status"
    )
    for row in cur.fetchall():
        print(f"{row['status']}: {row['n']}")


def _cleanup(target: str) -> None:
    from .sandbox import cleanup_workspace

    owner, repo_number = _split_target(target)
    from pathlib import Path

    cleanup_workspace(Path("/data/workspaces"), owner, repo_number[0], repo_number[1])
    print("cleaned", target)


def _split_target(target: str) -> tuple[str, tuple[str, int]]:
    repo, _, n = target.partition("#")
    owner, _, name = repo.partition("/")
    return owner, (name, int(n))


if __name__ == "__main__":
    main()
