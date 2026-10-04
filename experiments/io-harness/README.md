# Anchor io-harness feasibility spike

This standalone crate uses exactly io-harness 0.86.0. It lives outside Anchor,
does not import Anchor code, does not use Rig, and never calls a real provider.
Run `CARGO_TARGET_DIR=/tmp/io-harness-v2-target cargo test`.

The experiment assigns the agent loop, context assembly and compaction, SQLite
trace, checkpoints, and recovery to io-harness. A fake Anchor Tool adapter is the
only execution boundary. Anchor Graph/Run/Artifact facts would remain owned by
Anchor in a production integration; these internal records do not replace them.

Scope: text, PNG image input, streaming text, and simple JSON tool arguments and
JSON encoded tool-result strings. Complex structured output is excluded.

Future Rig integration would implement `io_harness::Provider` using Rig's
completion API. Rig would supply models, credentials, transport and model
responses; io-harness would continue owning the loop. Rig AgentRun/tool loops
must not simultaneously execute the same calls. Translate request messages,
tool schemas, image media, usage, and streaming deltas explicitly.

This is a lossy boundary: io-harness `ToolCall` stores name and JSON arguments
but no original provider call id. Results correlate by the call's position in
the preceding assistant message; vendor ids are regenerated. Provider response
identity and complex reasoning/media events cannot be claimed as preserved.
`Tool::invoke` returns only `String`; simple JSON can be serialized into that
string, but typed structured results and result media require a separate
Anchor artifact contract or an explicit unsupported response. Tool effect and
replay safety must be declared from the actual Anchor port capability, never
inferred from arbitrary tool names. Permissions stay at Anchor's tool port;
io-harness's own tool or sandbox capabilities must not expand host authority.

The tests below are provider-free feasibility evidence only. They do not prove
Rig adaptation, Anchor integration, provider acceptance, or production readiness.

An opt-in text provider smoke is available with an explicitly configured
OpenAI-compatible endpoint. It uses an in-memory Store and never reads Anchor's
`.env` automatically:

```text
ANCHOR_IO_HARNESS_URL=https://provider.example/v1 \
ANCHOR_IO_HARNESS_API_KEY=... \
ANCHOR_IO_HARNESS_MODEL=model-name \
cargo run --manifest-path experiments/io-harness/Cargo.toml --bin io-harness-live
```

This live smoke does not establish image support, Rig conversion, Graph
integration, or production suitability.
