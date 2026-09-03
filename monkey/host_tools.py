"""Host tools - the ONLY surface allowed to mutate GitHub.

These call the gh-proxy (which holds the token) so the coding agent never sees
GITHUB_TOKEN. Every invocation is audited into the tool_calls table with
credential-redacted args/results.
"""

from __future__ import annotations

import time
from typing import Any

import httpx

from .hmac import hmac_sign


class GHProxy:
    def __init__(self, base_url: str, hmac_key: str, store, owner: str, repo: str, number: int) -> None:
        self.base = base_url.rstrip("/")
        self.key = hmac_key
        self.store = store
        self.owner = owner
        self.repo = repo
        self.number = number

    async def _call(self, method: str, path: str, *, json: dict | None = None) -> Any:
        body = (json or {}).get("_raw1", b"")
        # Sign the canonical request body with HMAC + timestamp for replay safety.
        ts = int(time.time())
        payload_json = (json or {}).copy()
        payload_json.pop("_raw1", None)
        serialized = _dumps(payload_json).encode()
        sig = hmac_sign(self.key, serialized)
        headers = {
            "x-monkey-sig": sig,
            "x-monkey-ts": str(ts),
        }
        async with httpx.AsyncClient(timeout=30) as client:
            resp = await client.request(
                method, self.base + path, headers=headers, json=payload_json
            )
        result = _redact(resp.text)
        self.store.audit_tool_call(self.owner, self.repo, self.number, path, _redact(serialized.decode()), result)
        if resp.status_code >= 400:
            raise RuntimeError(f"gh-proxy {method} {path} -> {resp.status_code}: {result}")
        try:
            return resp.json()
        except ValueError:
            return resp.text

    async def add_issue_comment(self, body: str) -> Any:
        return await self._call("POST", f"/issues/{self.owner}/{self.repo}/{self.number}/comment", json={"body": body})

    async def add_labels(self, labels: list[str]) -> Any:
        return await self._call("POST", f"/issues/{self.owner}/{self.repo}/{self.number}/labels", json={"labels": labels})

    async def update_issue(self, body: dict) -> Any:
        return await self._call("PATCH", f"/issues/{self.owner}/{self.repo}/{self.number}", json=body)

    async def open_pull_request(self, body: dict) -> Any:
        return await self._call("POST", f"/pulls/{self.owner}/{self.repo}", json=body)

    async def push(self, worktree: str, branch: str) -> Any:
        """Push a worktree branch to the remote (token never leaves the proxy)."""
        return await self._call(
            "POST",
            "/git/push",
            json={
                "worktree": str(worktree),
                "branch": branch,
                "repo": f"{self.owner}/{self.repo}",
            },
        )


def _dumps(obj) -> str:
    import json

    return json.dumps(obj)


def _redact(text: str) -> str:
    # Best-effort redaction of obvious secrets in output.
    import re as _re

    return _re.sub(r"gh[pousr]_[A-Za-z0-9]+|github_pat_[A-Za-z0-9_]+", "[redacted]", text)
