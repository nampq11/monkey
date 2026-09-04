# pi RPC protocol fixtures

These files are captured output from a real `pi` process, not hand-written
JSON. Hand-written fixtures only prove the parser matches somebody's reading of
the docs; these prove it matches the engine.

## `pi-rpc-session-0.84.4.jsonl`

| | |
|---|---|
| Engine | `@earendil-works/pi-coding-agent` **0.84.4** |
| Captured | 2026-09-04 |
| Command | `pi --mode rpc --session-dir <tmp> --name contract-probe` |
| Input | one `prompt` command: "Reply with exactly the two characters: OK. Do not use any tools." |
| Shape | 20 lines, complete run through `agent_settled` |

The prompt was deliberately tool-free so the session contains no file reads, no
shell output, and no repository content.

### Redaction

Nine opaque provider blobs were replaced with `REDACTED_OPAQUE_PROVIDER_BLOB`:

- `thinkingSignature` (up to ~1.6 KB of provider-specific base64)
- `textSignature`
- `responseId`

Only the *values* were replaced. Every field name, nesting level, and event
type is byte-for-byte what pi emitted, which is all the contract test depends
on.

### What this fixture is expected to catch

The session exercises three event shapes the typed protocol models
(`response`, `agent_end`, `agent_settled`) and six it deliberately leaves
unmodelled (`extension_ui_request`, `agent_start`, `turn_start`,
`message_start`, `message_update`, `message_end`, `turn_end`). Those must
degrade to `PiEvent::Unknown`, not to a parse error.

If a future pi renames or reshapes any modelled event, the counts asserted in
`test_pi_contract.rs` change and the test fails. That is the gate on bumping
the engine pin in the `Dockerfile`.

### Regenerating

```sh
(printf '{"type":"prompt","message":"Reply with exactly the two characters: OK. Do not use any tools.","id":"monkey-probe-1"}\n'; sleep 90) \
  | pi --mode rpc --session-dir /tmp/pi-fixture --name contract-probe > capture.jsonl
```

Keep stdin open (the `sleep`) or pi sees EOF and exits before `agent_settled`.
Then scrub `thinkingSignature`, `textSignature` and `responseId`, and grep the
result for credentials before committing.
