#!/bin/sh
# Entrypoint: run the token-holding gh-proxy as a sibling process, then the
# orchestrator. The monkey service refuses GITHUB_TOKEN in its env; gh-proxy
# takes it. Run both via the command arg (default: monkey serve).
set -e

# gh-proxy needs the token; start it in the background on :8080.
if [ -n "$GITHUB_TOKEN" ] && [ -n "$MONKEY_GH_PROXY_HMAC_KEY" ]; then
    python -m monkey.cli gh-proxy &
    GH_PROXY_PID=$!
    echo "[entrypoint] gh-proxy started (pid $GH_PROXY_PID)"
fi

# Run the requested command (default: monkey serve).
exec "$@"

# On exit, stop gh-proxy.
if [ -n "${GH_PROXY_PID:-}" ]; then
    kill "$GH_PROXY_PID" 2>/dev/null || true
fi
