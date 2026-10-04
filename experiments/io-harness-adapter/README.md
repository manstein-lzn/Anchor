# io-harness/Rig conversion spike

This standalone crate is the first adapter boundary. It only converts public
request and response types; it does not run an agent loop, dispatch tools, or
persist Anchor facts. The future runtime must select exactly one loop owner:
io-harness owns context/compaction/recovery, while Anchor owns Graph/Run/
Artifact/Sandbox facts.

The supported conversion is text, positional JSON tool calls/results, and the
four image media types accepted by io-harness. `RigProviderAdapter` delegates
io-harness completions to a Rig `DynModel<Completion>`; io-harness remains the
loop owner. Its streaming method forwards Rig text deltas incrementally while
letting Rig assemble tool arguments and the final response.
Rig provider IDs are not copied into io-harness because io-harness correlates
results by position; deterministic IDs are generated only inside the Rig
request. Rig reasoning, assistant images, and other rich result content fail
closed instead of being flattened.

Rig provider failures are conservatively classified as non-retryable at this
boundary because the two crates do not expose a lossless error taxonomy yet;
automatic retries must wait for a provider-specific mapping.

This directory is now a thin compatibility package. Its local test target has
no tests; the canonical implementation and provider-free tests live in
`rust/anchor-io-harness-runtime`:

```text
CARGO_TARGET_DIR=/tmp/anchor-io-harness-runtime-target \
  cargo test --manifest-path rust/anchor-io-harness-runtime/Cargo.toml
```

This is provider-free conversion evidence. It does not prove a live Rig model,
Anchor ToolPort integration, MCP/Sandbox behavior, or checkpoint recovery.
