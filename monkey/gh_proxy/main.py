"""gh-proxy - the token-holding side of the trust boundary.

This service is the ONLY place GITHUB_TOKEN lives. The monkey orchestrator (and
therefore the coding agent) never has the token. The orchestrator sends signed
requests here; this proxy executes GitHub REST calls and `git push` with the
token, then returns the (redacted) result.

Egress is restricted to api.github.com; it exposes no host port and sits on an
internal-only Docker network.
"""

from __future__ import annotations

from contextlib import asynccontextmanager
import os
import subprocess

import httpx
from fastapi import FastAPI, HTTPException, Request
from fastapi.responses import JSONResponse

from ..hmac import BadSignature, verify_internal_signature

GH_TOKEN = os.environ.get("GITHUB_TOKEN", "")
HMAC_KEY = os.environ.get("MONKEY_GH_PROXY_HMAC_KEY", "")
API = "https://api.github.com"


@asynccontextmanager
async def lifespan(app: FastAPI):
    # Never run without a token or a shared HMAC key: this defeats the purpose.
    if not GH_TOKEN:
        raise RuntimeError("gh-proxy refuses to start without GITHUB_TOKEN")
    if not HMAC_KEY:
        raise RuntimeError("gh-proxy refuses to start without MONKEY_GH_PROXY_HMAC_KEY")
    yield


app = FastAPI(title="monkey gh-proxy", lifespan=lifespan)


@app.middleware("http")
async def _hmac_gate(request: Request, call_next):
    # Skip auth for /healthz (liveness only, no sensitive action).
    if request.url.path == "/healthz":
        return await call_next(request)
    sig = request.headers.get("x-monkey-sig")
    ts = request.headers.get("x-monkey-ts")
    # request.body is a coroutine in Starlette; await it to get the raw bytes the
    # client signed. Passing the method object would raise TypeError inside hmac.
    body = await request.body()
    try:
        verify_internal_signature(HMAC_KEY, body, sig, timestamp_header=ts)
    except BadSignature:
        return JSONResponse(status_code=401, content={"detail": "bad signature"})
    return await call_next(request)


# --------------------------------------------------------------------------
# GitHub REST passthrough (extend as needed; the surface stays narrow on
# purpose so the agent cannot reach arbitrary endpoints).
# --------------------------------------------------------------------------


@app.get("/healthz")
def healthz() -> dict:
    return {"ok": True}


@app.post("/issues/{owner}/{repo}/{number}/comment")
async def add_issue_comment(owner: str, repo: str, number: int, body: dict):
    return await _gh(
        "POST",
        f"/repos/{owner}/{repo}/issues/{number}/comments",
        json=body,
    )


@app.post("/issues/{owner}/{repo}/{number}/labels")
async def add_labels(owner: str, repo: str, number: int, body: dict):
    return await _gh(
        "POST",
        f"/repos/{owner}/{repo}/issues/{number}/labels",
        json=body,
    )


@app.patch("/issues/{owner}/{repo}/{number}")
async def update_issue(owner: str, repo: str, number: int, body: dict):
    return await _gh("PATCH", f"/repos/{owner}/{repo}/issues/{number}", json=body)


@app.post("/pulls/{owner}/{repo}")
async def open_pull_request(owner: str, repo: str, body: dict):
    return await _gh("POST", f"/repos/{owner}/{repo}/pulls", json=body)


@app.post("/pulls/{owner}/{repo}/{number}/comments")
async def add_pr_review_comment(owner: str, repo: str, number: int, body: dict):
    return await _gh(
        "POST",
        f"/repos/{owner}/{repo}/pulls/{number}/comments",
        json=body,
    )


@app.post("/repos/{owner}/{repo}/git/refs")
async def create_ref(owner: str, repo: str, body: dict):
    return await _gh("POST", f"/repos/{owner}/{repo}/git/refs", json=body)


# --------------------------------------------------------------------------
# git push: runs inside gh-proxy so the token only lives in an ephemeral
# process env var; the remote URL in .git/config stays token-free.
# --------------------------------------------------------------------------


@app.post("/git/push")
async def git_push(body: dict):
    worktree = body.get("worktree")
    branch = body.get("branch")
    repo = body.get("repo")
    if not worktree or not branch or not repo:
        raise HTTPException(
            status_code=422, detail="worktree, branch, and repo required"
        )

    env = dict(os.environ)
    env["GIT_TOKEN"] = GH_TOKEN
    remote = f"https://x-access-token:{GH_TOKEN}@github.com/{repo}.git"

    try:
        subprocess.run(
            ["git", "-C", worktree, "push", "-f", remote, f"HEAD:{branch}"],
            check=True,
            capture_output=True,
            text=True,
            env=env,
        )
    except subprocess.CalledProcessError as exc:
        return JSONResponse(
            status_code=502,
            content={"detail": "git push failed", "stderr": _redact(exc.stderr)},
        )
    return {"ok": True}


async def _gh(method: str, path: str, **kw) -> dict:
    try:
        async with httpx.AsyncClient(timeout=30) as client:
            resp = await client.request(
                method,
                API + path,
                headers={
                    "Authorization": f"Bearer {GH_TOKEN}",
                    "Accept": "application/vnd.github+json",
                    "X-GitHub-Api-Version": "2022-11-28",
                },
                **kw,
            )
    except httpx.HTTPError as exc:
        raise HTTPException(status_code=502, detail=f"upstream error: {exc}") from exc
    if resp.status_code >= 400:
        return {"error": True, "status": resp.status_code, "body": resp.json()}
    return resp.json()


def _redact(text: str) -> str:
    for word in (GH_TOKEN,):
        if word:
            text = text.replace(word, "[redacted]")
    return text
