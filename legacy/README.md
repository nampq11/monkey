# Legacy Python implementation

This directory holds the original Python (FastAPI) prototype that the Rust
workspace in `crates/` replaces. It is kept for reference only and is excluded
from the Docker build context (see `.dockerignore`).

- `monkey/` - the old package (`dispatch.py`, `sandbox.py`, `hmac.py`, ...).
  Each module was ported to Rust; use it as a historical reference, not as a
  source of truth.
- `pyproject.toml`, `uv.lock` - the old Python project manifest and lockfile.

Do not build on top of this code. New work belongs in `crates/`.
