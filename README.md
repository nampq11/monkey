> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.
# monkey

Self-hosted GitHub triage bot. `monkey` receives GitHub webhooks, runs a coding-agent engine (default: **pi**) in an issue-specific git worktree, then writes results back to GitHub through a token-holding sidecar.

> The test monkey for your coding agents. Engine-agnostic: swap the agent without touching the orchestrator.

## Current implementation

This repository is now Rust-only. The old `legacy/` Python/FastAPI prototype has been removed; new work belongs in `crates/`.

| Path | Purpose |
|------|---------|
| `crates/monkey_app` | CLI, webhook server, worker loop |
| `crates/monkey_core` | config, SQLite storage, dispatch, sandbox/worktree management |
| `crates/monkey_engine` | engine adapter trait and pi integration |
| `crates/monkey_github` | GitHub writeback, host tools, gh-proxy service |
| `script/` | local helper scripts |

## Architecture (2 containers, 1 trust boundary)

```
                    INTERNET
                        |
                        v
               GitHub Webhook -> /webhook/github
                        |
                        v
   +----------------------------------------+
   |  monkey (orchestrator)                 |
   |  Axum + SQLite + worker pool           |
   |  holds HMAC key, NEVER GITHUB_TOKEN    |
   |  spawns pi per issue in a git worktree |
   +-----------------+----------------------+
                     | HMAC-signed, internal network
                     v
   +----------------------------------------+
   |  gh-proxy (token-holding sidecar)      |
   |  holds GITHUB_TOKEN                    |
   |  pushes branches and calls GitHub API  |
   +-----------------+----------------------+
                     |
                     v
               api.github.com
```

## Behavior

| Event | Action |
|-------|--------|
| `bug` / `documentation` | Reproduce, fix on a branch, open PR with `## Repro` / `## Cause` / `## Fix` / `## Verification` + `Fixes #N` |
| `question` | One comment; auto-close after `MONKEY_QUESTION_AUTOCLOSE_HOURS` (default 4) unless author reacts with downvote |
| `enhancement` / `proposal` | One comment, no PR |
| `invalid` / `duplicate` | One brief comment |
| Follow-up comment / PR review | Resume the same pi session from `MONKEY_SESSION_DIR` |

## Setup

1. Create a bot GitHub account and fine-grained PAT with Contents, Issues, Pull requests RW + Metadata R.
2. Configure pi on the host (`~/.pi/agent/models.json` and `~/.pi/agent/auth.json`); docker-compose mounts both read-only into the orchestrator container.
3. Copy `.env.example` to `.env` and fill in the required values.
4. Start the services:

   ```bash
   docker compose up -d --build
   ```

5. Add a GitHub webhook to `/webhook/github` for: Issues, Issue comments, and Pull request reviews.

## Configuration

Required for the recommended sidecar mode:

| Variable | Purpose |
|----------|---------|
| `GITHUB_WEBHOOK_SECRET` | Verifies GitHub webhook signatures |
| `MONKEY_BOT_LOGIN` | Bot account login, used to ignore self-events |
| `MONKEY_REPO_ALLOWLIST` | Comma-separated `owner/repo` allowlist |
| `MONKEY_GH_PROXY_URL` | Usually `http://gh-proxy:8080` in compose |
| `MONKEY_GH_PROXY_HMAC_KEY` | Shared secret for orchestrator -> gh-proxy calls |
| `GITHUB_TOKEN` | PAT held only by `gh-proxy` in docker-compose |

Optional defaults are documented in `.env.example`.

## Security

- `GITHUB_TOKEN` lives only in `gh-proxy`; docker-compose clears it from the orchestrator environment.
- HMAC-SHA256 signs requests between services with a ±30s skew window and constant-time comparison.
- `gh-proxy` exposes no host port; only the orchestrator webhook port is published.
- The agent subprocess env is scrubbed of all `GITHUB_*` and `MONKEY_*` variables.
- Bad webhook signatures return `401`.

## CLI

```bash
monkey serve                       # orchestrator (webhook + worker)
monkey gh-proxy                    # token-holding proxy
monkey triage owner/repo#N         # print latest stored event for an issue
monkey status                      # queue state
monkey cleanup owner/repo#N        # remove an issue worktree
```

Local webhook test helper:

```bash
script/trigger-issue <issue_number>
```

## Development

```bash
cargo test
./script/clippy
```

## License

MIT
