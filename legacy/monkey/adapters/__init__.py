"""Engine adapters.

get_adapter() returns the configured engine adapter. See base.EngineAdapter for
the contract. Swap the coding-agent engine here without touching the orchestrator.
"""

from __future__ import annotations

from .base import EngineAdapter, Outcome
from .pi import PiAdapter


def get_adapter(name: str = "pi") -> EngineAdapter:
    """Return a configured adapter by name. Default: pi."""
    if name == "pi":
        return PiAdapter()
    raise ValueError(f"unknown adapter: {name!r}")


__all__ = ["EngineAdapter", "Outcome", "PiAdapter", "get_adapter"]
