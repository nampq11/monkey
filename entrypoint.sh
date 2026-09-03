#!/bin/sh
# Entrypoint: exec the container's command (default: monkey serve).
# gh-proxy runs as a SEPARATE service in docker-compose (the 2-container trust
# boundary), so this script does not start it here - it only passes through to
# the command so the same image can run the orchestrator or the proxy.
set -e

exec "$@"
