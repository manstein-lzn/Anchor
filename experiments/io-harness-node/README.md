# io-harness Anchor ToolPort vertical spike

This compatibility package points to the canonical runtime crate, which proves
the narrow node boundary before production host integration. io-harness owns
the Agent loop and compaction/recovery boundary;
Anchor owns tool definitions, permissions, and execution through `ToolPort`.

`AnchorToolAdapter` exposes one Anchor tool as one io-harness `Tool`. JSON tool
results are serialized to the text result shape io-harness accepts. The default
is `Mutating + Indeterminate`, so a started external effect cannot be silently
replayed. A read-only/replayable declaration is available only as an explicit
fixture choice. Rich image tool results are rejected at this boundary; images
remain an explicit model input/output artifact capability rather than being
silently flattened into tool text.

The test uses Rig 0.43's scripted `MockCompletionModel` through the independent
`RigProviderAdapter`, then runs the real io-harness loop. It proves one tool
call, JSON result visibility in the next model request, and a finished run. It
also reopens the SQLite Store and resumes the same io-harness run id without
replaying the completed Anchor tool. The backend requires the caller to provide
the frozen contract and run id; it does not reload current Graph definitions.
It does not prove production GraphRunner, Sandbox, MCP, process crash recovery,
or a real external provider.

This wrapper has no local test target. Run the canonical implementation and
provider-free tests with:

```text
CARGO_TARGET_DIR=/tmp/anchor-io-harness-runtime-target \
  cargo test --manifest-path rust/anchor-io-harness-runtime/Cargo.toml -- --nocapture
```
