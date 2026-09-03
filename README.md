# monkey

Self-hosted GitHub triage bot. Drives a coding-agent engine (default: **pi**) per-issue against a git worktree, then writes back to GitHub through a token-holding sidecar. Full roboomp behavior: classify issue, answer questions, fix bugs, open PRs.

> The test monkey for your coding agents. Engine-agnostic: swap the agent without touching the orchestrator.

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
   |  FastAPI + SQLite + 1 worker           |
   |  holds HMAC key, NEVER GITHUB_TOKEN    |
   |  spawns pi per issue in a git worktree |
   +-----------------+----------------------+
                     | HMAC-signed, internal network
                     v
   +----------------------------------------+
   |  gh-proxy (token-holding sidecar)      |
   |  holds GITHUB_TOKEN                    |
   |  egress ONLY to api.github.com         |
   +-----------------+----------------------+
                     |
                     v
               api.github.com
```

## Full behavior

| Event | Action |
|-------|--------|
| `bug` / `documentation` | Reproduce, fix on a branch, open PR with `## Repro` / `## Cause` / `## Fix` / `## Verification` + `Fixes #N` |
| `question` | One comment; auto-close after `QUESTION_AUTOCLOSE_HOURS` (default 4) unless author reacts with downvote |
| `enhancement` / `proposal` | One comment, no PR |
| `invalid` / `duplicate` | One brief comment |
| Follow-up comment / PR review | Resume the same session via `--continue` from `session_dir` |

## Setup

1. Create a bot GitHub account + fine-grained PAT (Contents, Issues, Pull requests RW + Metadata R).
2. Point `~/.omp/agent/models.container.yml` at your LiteLLM gateway (the engine routes all LLM calls there).
3. Copy `.env.example` -> `.env` and fill in required vars.
4. `docker compose up -d --build`
5. Add a GitHub webhook to `/webhook/github` for events: Issues, Issue comments, Pull requests, Pull request reviews, Pull request review comments.

## Security

- `GITHUB_TOKEN` lives only in `gh-proxy`; the orchestrator **refuses to start** if it sees it in its own env.
- HMAC-SHA256 signed requests between services with a ±30s skew window + constant-time compare.
- `gh-proxy` exposes no host port. The orchestrator sits on an `internal: true`
  network (isolated from the internet); gh-proxy additionally joins an `egress`
  bridge so it can reach `api.github.com`, and only gh-proxy holds the token.
- The agent subprocess env is scrubbed of `GITHUB_TOKEN` / `MONKEY_GH_PROXY_HMAC_KEY`.
- Bad webhook signature returns `401` (never `5xx`).

## CLI

```bash
monkey serve                       # orchestrator (webhook + worker)
monkey gh-proxy                    # token-holding proxy
monkey triage owner/repo#N         # manually triage one issue
monkey status                      # queue state
monkey cleanup owner/repo#N        # remove a worktree
```

## License

MIT
