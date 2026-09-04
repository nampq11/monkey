"""Engine adapter interface.

The orchestrator is engine-agnostic: it only knows how to hand a task to an
adapter and read back the result. Swapping the coding-agent engine (pi today,
omp/claude-code/codex later) means adding a new adapter, not touching the
orchestrator, sandbox, dispatch, or gh-proxy logic.

Every adapter must provide:
  - run(task, worktree, session_dir) -> Outcome   (fresh session in a worktree)
  - resume(session_dir, follow_up) -> Outcome     (continue a prior session)
  - session_artifacts(session_dir) -> dict        (PR body, comments, diagnosis)
"""

from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from pathlib import Path


@dataclass
class Outcome:
    """What an adapter reports after a run.

    `session_dir` points at the persisted transcript so a follow-up comment or
    review can resume the exact same reasoning (mirrors roboomp's session_dir).
    """

    session_dir: Path
    status: str = "ok"  # ok | error | needs_human
    summary: str = ""
    pr_body: str = ""  # candidate PR body (bug/documentation path)
    comment: str = ""  # single comment to post (question/enhancement/invalid)
    branch: str = ""  # branch name the agent worked on (fix path)
    artifact_paths: list[Path] = field(default_factory=list)
    raw_events: list[dict] = field(default_factory=list)


class EngineAdapter(ABC):
    """Contract between the orchestrator and a coding-agent engine."""

    @abstractmethod
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
        """Start a fresh agent session and drive it in `worktree`.

        The agent must persist its transcript into `session_dir` so `resume`
        can pick it up later. Returns once the agent has fully settled.
        """

    @abstractmethod
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
        """Continue an existing session (from a follow-up comment/review)."""

    @abstractmethod
    def session_artifacts(self, session_dir: Path) -> dict:
        """Extract artifacts (PR body text, final comment, diagnosis) from a session."""
