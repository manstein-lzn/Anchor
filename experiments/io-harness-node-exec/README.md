# io-harness NodeRequest execution spike

This is the A70 boundary experiment. It maps Anchor's existing `NodeRequest`
into a frozen io-harness `TaskContract`, exposes the admitted Anchor `ToolPort`
through `AnchorToolAdapter`, and maps only a completed io-harness assistant turn
back to Anchor `NodeOutcome`.

The adapter refuses to publish `Completed` for cancellation, step caps, recovery
pauses, provider errors, missing final assistant text, invalid JSON summaries,
or invalid routes. It keeps the io-harness run id on incomplete errors so the
caller can resume the exact durable run. Cancellation is checked before start and
observed during the io-harness run at its durable step boundary. The contract masks io-harness native workspace, shell and exec tools so they
cannot bypass Anchor ToolPort/Sandbox authority. Production `NodeExecutionPort`
wiring, crash-window recovery, host Plugin/MCP binding, and reasoning/rich-content
provider compatibility are still pending.

This directory is a thin compatibility package with no local test target. Run
the canonical implementation and provider-free tests with:

```text
CARGO_TARGET_DIR=/tmp/anchor-io-harness-runtime-target \
  cargo test --manifest-path rust/anchor-io-harness-runtime/Cargo.toml -- --nocapture
```

The unit tests use Rig's scripted provider and a fake Anchor ToolPort. The live
smoke uses the configured external provider for one simple-text case, but does
not claim production GraphRunner, Sandbox, MCP, process crash-window, or
reasoning-content support.


An explicit real-provider smoke is available as `live`. It requires
`ANCHOR_MODEL_API_KEY`, `ANCHOR_MODEL_URL`, and `ANCHOR_MODEL_NAME`; it uses a
temporary workspace and writes evidence only under `.local/` when the caller
provides `ANCHOR_IO_NODE_EXEC_EVIDENCE`. It does not touch Anchor Graph/Run
state.

```text
set -a; . .env; set +a
ANCHOR_IO_NODE_EXEC_EVIDENCE=.local/io-node-exec-live/evidence.json \
  CARGO_TARGET_DIR=/tmp/anchor-io-harness-node-exec-target \
  cargo run --manifest-path experiments/io-harness-node-exec/Cargo.toml --bin live
```
