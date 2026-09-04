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

Routing happens in `crates/monkey_core/src/dispatch.rs`: labels `question`, `invalid`, and `duplicate` are matched first (a `?` anywhere in the title also counts as `question`); otherwise the issue title + body is scanned for keywords. Closed issues/PRs and unsupported webhook actions are ignored.

| Event | Action |
|-------|--------|
| Title/body mentions bug keywords (`bug`, `error`, `crash`, `fail`, `broken`, `exception`, `regression`) or doc keywords (`documentation`, `doc`, `typo`, `readme`, `docs`) | Fix path: reproduce, edit code, commit, open PR with `## Repro` / `## Cause` / `## Fix` / `## Verification` + `Fixes #N` (bug keywords win when both match) |
| `question` | One comment; auto-close after `MONKEY_QUESTION_AUTOCLOSE_HOURS` (default 4) unless author reacts with downvote |
| Title/body mentions feature keywords (`feature`, `enhancement`, `proposal`, `suggestion`, `request`), or no rule matched | One comment, no PR |
| `invalid` / `duplicate` | One brief comment |
| Follow-up comment / PR review | Resume the same pi session from `MONKEY_SESSION_DIR` |

## Setup

1. Create a bot GitHub account and fine-grained PAT with Contents, Issues, Pull requests RW + Metadata R.
2. Configure pi on the host (`~/.pi/agent/models.json` and `~/.pi/agent/auth.json`); docker-compose mounts both read-only into the orchestrator container. If your pi config lives elsewhere, set `PI_AGENT_DIR` (the compose mounts resolve to `${PI_AGENT_DIR:-$HOME/.pi/agent}/models.json` and `.../auth.json`).
3. Copy `.env.example` to `.env` and fill in the required values.
4. Start the services:

   ```bash
   docker compose up -d --build
   ```

5. Add a GitHub webhook to `/webhook/github` for: Issues, Issue comments, and Pull request reviews.

## Configuration

Required in both modes:

| Variable | Purpose |
|----------|---------|
| `GITHUB_WEBHOOK_SECRET` | Verifies GitHub webhook signatures |
| `MONKEY_BOT_LOGIN` | Bot account login, used to ignore self-events |
| `MONKEY_REPO_ALLOWLIST` | Comma-separated `owner/repo` allowlist |

Auth is mode-exclusive: `Settings::validate` rejects a configuration that sets both.

**Sidecar mode (recommended; what docker-compose sets up)**

| Variable | Purpose |
|----------|---------|
| `MONKEY_GH_PROXY_URL` | gh-proxy address, `http://gh-proxy:8080` in compose |
| `MONKEY_GH_PROXY_HMAC_KEY` | Shared secret for orchestrator -> gh-proxy calls; required together with the URL |
| `GITHUB_TOKEN` | Held only by the `gh-proxy` container; docker-compose overrides it to empty in the orchestrator's environment |

**Direct PAT mode (validation-only today)**

| Variable | Purpose |
|----------|---------|
| `GITHUB_TOKEN` | Accepted by `Settings::validate`; leave `MONKEY_GH_PROXY_URL` and `MONKEY_GH_PROXY_HMAC_KEY` unset |

No code path sends direct PAT requests yet — every GitHub call (comments, PRs, question auto-close) goes through the HMAC-signed gh-proxy client, so a PAT-only configuration starts cleanly but cannot write back to GitHub. Use sidecar mode until direct PAT support lands.

Optional variables and their defaults are documented in `.env.example`.

## Security

- In sidecar mode, `GITHUB_TOKEN` lives only in `gh-proxy`; docker-compose overrides it to empty in the orchestrator's environment. In direct PAT mode there is no sidecar and the orchestrator holds the token (that mode is validation-only today — see Configuration).
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

`triage` and `status` read `MONKEY_DB_PATH` (default `/data/monkey.db`); `cleanup` reads `MONKEY_WORKSPACES_ROOT` (default `/data/workspaces`). Those paths live inside the containers, so export local values when running the CLI outside compose (the binary does not read `.env` itself — docker-compose injects that file).

Local webhook test helper:

```bash
script/trigger-issue <issue_number> [owner/repo]
```

The repo defaults to `MONKEY_TRIGGER_REPO` (falling back to `nampq11/monkey`); it must be listed in `MONKEY_REPO_ALLOWLIST`, otherwise the orchestrator returns `ok` and silently skips the event.

## Development

```bash
cargo test
./script/clippy
```

## License

MIT
