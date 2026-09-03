"""Pi coding-agent adapter.

Drives the `pi` binary over stdin/stdout JSONL (RPC mode). Protocol facts are
confirmed from pi's own docs/rpc.md:

  - spawn:        pi --mode rpc --session-dir <dir> --name <name>
  - prompt frame: {"type":"prompt","message":"...","id":"..."}
  - settle event: {"type":"agent_settled"}  -> agent fully done, safe to stop
  - resume:       send {"type":"switch_session","sessionPath":"..."} before the
                  next prompt to continue a prior transcript
  - artifact id:  {"type":"get_session_stats"} -> data.sessionFile / sessionId
  - last text:    {"type":"get_last_assistant_text"} -> data.text

Framing note (from docs): split records on LF only, strip a trailing CR; do not
use a reader that treats Unicode separators as newlines.
"""

from __future__ import annotations

import json
import os
import re
import shutil
from pathlib import Path
from typing import BinaryIO

from .base import EngineAdapter, Outcome


class PiRpcError(Exception):
    pass


class PiAdapter(EngineAdapter):
    def __init__(self, binary: str = "pi") -> None:
        self.binary = binary
        if shutil.which(binary) is None:
            raise PiRpcError(f"pi binary not found on PATH: {binary!r}")

    # -- public API ---------------------------------------------------------

    def run(
        self,
        task: str,
        worktree: Path,
        *,
        session_dir: Path,
        model: str = "",
        thinking: str = "medium",
        provider: str = "",
        timeout_seconds: int = 3600,
    ) -> Outcome:
        session_dir.mkdir(parents=True, exist_ok=True)
        args = ["--mode", "rpc", "--session-dir", str(session_dir), "--name", "monkey"]
        if model:
            args += ["--model", model]
        if provider:
            args += ["--provider", provider]
        args += ["--thinking", thinking]

        with _SpawnedPi(self.binary, args, cwd=worktree) as proc:
            self._prompt(proc, task)
            events = self._drain_until_settled(proc, timeout_seconds)
            running = self._get_running_session(proc)
            last_text = self._get_last_assistant_text(proc)

        return self._build_outcome(session_dir, events, last_text, running=running, worktree=worktree)

    def resume(
        self,
        follow_up: str,
        *,
        session_dir: Path,
        worktree: Path,
        model: str = "",
        thinking: str = "medium",
        provider: str = "",
        timeout_seconds: int = 3600,
    ) -> Outcome:
        session_path = _find_session_file(session_dir)
        args = ["--mode", "rpc", "--session-dir", str(session_dir), "--name", "monkey"]
        if model:
            args += ["--model", model]
        if provider:
            args += ["--provider", provider]
        args += ["--thinking", thinking]

        with _SpawnedPi(self.binary, args, cwd=worktree) as proc:
            if session_path is not None:
                self._switch_session(proc, session_path)
            self._prompt(proc, follow_up)
            events = self._drain_until_settled(proc, timeout_seconds)
            running = self._get_running_session(proc)
            last_text = self._get_last_assistant_text(proc)

        return self._build_outcome(session_dir, events, last_text, running=running, worktree=worktree)

    def session_artifacts(self, session_dir: Path) -> dict:
        session_path = _find_session_file(session_dir)
        if session_path is None:
            return {}
        transcript = _parse_transcript(session_path)
        return {"session_file": str(session_path), "messages": transcript}

    # -- low-level RPC helpers ---------------------------------------------

    def _prompt(self, proc: "_SpawnedPi", message: str) -> None:
        proc.send({"type": "prompt", "message": message, "id": "monkey-1"})

    def _switch_session(self, proc: "_SpawnedPi", session_path: Path) -> None:
        proc.send({"type": "switch_session", "sessionPath": str(session_path)})

    def _get_session_stats(self, proc: "_SpawnedPi") -> dict:
        proc.send({"type": "get_session_stats", "id": "stats-1"})
        return proc.wait_for_response("stats-1")

    def _get_running_session(self, proc: "_SpawnedPi") -> dict:
        data = self._get_session_stats(proc)
        return data.get("data", {})

    def _get_last_assistant_text(self, proc: "_SpawnedPi") -> str:
        proc.send({"type": "get_last_assistant_text", "id": "text-1"})
        resp = proc.wait_for_response("text-1")
        return resp.get("data", {}).get("text", "")

    def _drain_until_settled(self, proc: "_SpawnedPi", timeout_seconds: int) -> list[dict]:
        events: list[dict] = []
        deadline = proc.monotonic() + timeout_seconds
        while proc.monotonic() < deadline:
            try:
                evt = proc.readline(timeout=proc.remaining(deadline))
            except TimeoutExpired:
                break
            if evt is None:
                break
            events.append(evt)
            if evt.get("type") == "agent_settled":
                # Drain anything that raced just after settle.
                proc.drain_quiet()
                break
        return events

    def _build_outcome(
        self,
        session_dir: Path,
        events: list[dict],
        last_text: str,
        *,
        running: dict,
        worktree: Path | None = None,
    ) -> Outcome:
        session_id = running.get("sessionId", "")
        session_file = running.get("sessionFile", "")
        outcome = Outcome(
            session_dir=session_dir,
            status="ok",
            summary=last_text,
            raw_events=events,
        )
        if session_id:
            outcome.artifact_paths = [Path(session_file)] if session_file else []
        outcome.branch = _read_branch(worktree)
        # Map the agent's final text into the fields the write-back step reads.
        # A fix task should emit the structured PR body (## Repro/Cause/Fix/
        # Verification); anything else becomes a single comment.
        if _looks_like_pr_body(last_text):
            outcome.pr_body = last_text
        elif last_text:
            outcome.comment = last_text
        return outcome



# -- process wrapper & framing -------------------------------------------


class TimeoutExpired(Exception):
    pass


def _find_session_file(session_dir: Path) -> Path | None:
    files = sorted(session_dir.glob("*.jsonl"))
    return files[-1] if files else None


def _read_branch(worktree: Path | None) -> str:
    """Return the worktree's checked-out branch name, or '' if unavailable."""
    if worktree is None:
        return ""
    import subprocess

    try:
        proc = subprocess.run(
            ["git", "-C", str(worktree), "rev-parse", "--abbrev-ref", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        )
    except (subprocess.CalledProcessError, OSError):
        return ""
    return proc.stdout.strip()


def _looks_like_pr_body(text: str) -> bool:
    """True if the text contains the structured sections required for a PR."""
    if not text:
        return False
    sections = ("## Repro", "## Cause", "## Fix", "## Verification")
    return sum(1 for s in sections if s in text) >= 2


def _parse_transcript(session_path: Path) -> list[dict]:
    out: list[dict] = []
    try:
        with open(session_path, "r", encoding="utf-8") as fh:
            for line in fh:
                line = line.rstrip("\r\n")
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    except FileNotFoundError:
        pass
    return out


class _SpawnedPi:
    """Minimal subprocess wrapper around `pi --mode rpc`.

    Only sends/reads whole JSONL records, LF-delimited, ignoring records split
    on Unicode separators (pi's docs warn generic line readers are unsafe).
    """

    def __init__(self, binary: str, args: list[str], *, cwd: Path | None = None) -> None:  # noqa: ANN101
        import subprocess

        # Scrub secrets so the agent can never read the token or the proxy HMAC
        # key (which it could use to sign its own requests to gh-proxy).
        # Filter by prefix: any GITHUB_* or MONKEY_* variable is sensitive
        # (tokens, webhook secrets, HMAC keys, DB URLs, ...).
        env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith(("GITHUB_", "MONKEY_"))
        }
        self.proc = subprocess.Popen(
            [binary, *args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            bufsize=-1,
            env=env,
            cwd=str(cwd) if cwd is not None else None,
        )
        self._buf = bytearray()

    def __enter__(self) -> "_SpawnedPi":
        return self

    def __exit__(self, *exc) -> None:  # noqa: ANN002
        self.close()

    def close(self) -> None:
        try:
            if self.proc.stdin:
                self.proc.stdin.close()
            self.proc.terminate()
            self.proc.wait(timeout=5)
        except Exception:
            self.proc.kill()

    def send(self, obj: dict) -> None:
        if self.proc.stdin is None:
            raise PiRpcError("stdin closed")
        line = json.dumps(obj) + "\n"
        self.proc.stdin.write(line.encode())
        self.proc.stdin.flush()

    def readline(self, timeout: float) -> dict | None:
        assert self.proc.stdout is not None
        while True:
            newline = self._buf.find(b"\n")
            if newline != -1:
                raw = bytes(self._buf[:newline])
                del self._buf[: newline + 1]
                return self._decode(raw)
            if not self._read_more(timeout):
                return None

    def _read_more(self, timeout: float) -> bool:
        assert self.proc.stdout is not None
        import select

        r, _, _ = select.select([self.proc.stdout], [], [], timeout)
        if not r:
            raise TimeoutExpired()
        chunk = self.proc.stdout.read1(65536)  # os.read of up to 64KB
        if not chunk:
            return False
        self._buf.extend(chunk)
        return True

    def drain_quiet(self) -> None:
        """Best-effort clear of anything still queued, without blocking forever."""
        try:
            while self._read_more(1):
                pass
        except TimeoutExpired:
            return

    @staticmethod
    def _decode(raw: bytes) -> dict | None:
        raw = raw.strip()
        if not raw:
            return None
        try:
            return json.loads(raw.decode())
        except (ValueError, UnicodeDecodeError):
            return None

    def wait_for_response(self, req_id: str, timeout: float = 10.0) -> dict:
        deadline = self.monotonic() + timeout
        while self.monotonic() < deadline:
            evt = self.readline(self.remaining(deadline))
            if evt is None:
                continue
            if evt.get("type") == "response" and evt.get("id") == req_id:
                return evt
        raise TimeoutExpired(f"no response for id {req_id!r}")

    @staticmethod
    def monotonic() -> float:
        import time

        return time.monotonic()

    @staticmethod
    def remaining(deadline: float) -> float:
        import time

        return max(0.0, deadline - time.monotonic())


# expose select-compatible flush helper for tests
def _peek_line(buf: bytearray) -> tuple[bytes, bytearray] | None:  # pragma: no cover
    nl = buf.find(b"\n")
    if nl == -1:
        return None
    raw = bytes(buf[:nl])
    rest = bytearray(buf[nl + 1 :])
    return raw, rest
