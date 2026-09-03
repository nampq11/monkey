"""GitHub webhook receiver.

Verifies HMAC, dedups on X-GitHub-Delivery, and enqueues the event for the
worker. This endpoint is the ONLY thing exposed to the internet.
"""

from __future__ import annotations

import json
from typing import Annotated

from contextlib import asynccontextmanager

from fastapi import FastAPI, Header, HTTPException, Request
from fastapi.responses import JSONResponse

from .config import get_settings
from .db import Store
from .hmac import BadSignature, verify_github_signature


@asynccontextmanager
async def lifespan(app: FastAPI):
    get_settings()  # raises if required config missing
    yield


app = FastAPI(title="monkey", lifespan=lifespan)


def _env_store() -> Store:
    # lite lifecycle: one Store per process, created lazily.
    global _STORE
    if _STORE is None:
        _STORE = Store("/data/monkey.db")
    return _STORE


_STORE: Store | None = None
@app.get("/healthz")
def healthz() -> dict:
    return {"ok": True}


@app.post("/webhook/github")
async def github_webhook(request: Request):
    settings = get_settings()
    store = _env_store()

    body = await request.body()
    signature = request.headers.get("x-hub-signature-256")
    delivery_id = request.headers.get("x-github-delivery")
    event_type = request.headers.get("x-github-event", "")

    try:
        verify_github_signature(settings.github_webhook_secret, body, signature)
    except BadSignature:
        # 401, never 5xx, so GitHub stops retrying a bad secret.
        return JSONResponse(status_code=401, content={"detail": "bad signature"})

    if not delivery_id:
        raise HTTPException(status_code=400, detail="missing x-github-delivery")

    try:
        payload = json.loads(body)
    except json.JSONDecodeError:
        raise HTTPException(status_code=400, detail="invalid json")

    owner, repo, number = _parse_target(event_type, payload)
    if owner is None or repo is None or number is None:
        # Non-issue/pr/runnable event (e.g. ping) - acknowledge, no work.
        return {"ok": True, "skipped": "not an actionable event"}

    # Allowlist check.
    if f"{owner}/{repo}" not in settings.allowlist:
        return {"ok": True, "skipped": "repo not in allowlist"}

    # Ignore events authored by the bot itself or other bots to avoid feedback loops.
    sender = (payload.get("sender") or {}).get("login", "")
    if settings.bot_login and sender.lower() == settings.bot_login.lower():
        return {"ok": True, "skipped": "bot-authored event"}
    if sender.endswith("[bot]"):
        return {"ok": True, "skipped": "bot-authored event"}

    is_new = store.enqueue(
        delivery_id, event_type, owner, repo, number, body.decode("utf-8", "replace")
    )
    return {"ok": True, "new": is_new}


def _parse_target(event_type: str, payload: dict) -> tuple[str | None, str | None, int | None]:
    """Extract (owner, repo, issue/pr number) for the events we care about."""
    repo = payload.get("repository") or {}
    owner = (repo.get("owner") or {}).get("login") or (repo.get("owner") or {}).get("name")
    repo_name = repo.get("name")

    def _num():
        n = payload.get("issue") or payload.get("pull_request") or payload.get("review") or {}
        return n.get("number")

    if event_type in ("issues", "pull_request", "issue_comment", "pull_request_review"):
        return owner, repo_name, _num()
    return None, None, None
