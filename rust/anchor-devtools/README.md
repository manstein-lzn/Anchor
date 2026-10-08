# Deterministic regression entry points

## Standard Goose fixtures

Configure the pinned Goose 1.53.0 x86_64 musl binary explicitly:

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target \
CARGO_BUILD_JOBS=2 \
cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools -- regression goose-fixture
```

`regression fixture` and `regression goose-fixture` are aliases for the same
`run_goose_fixture` function; both run only the deterministic Goose suites.
Neither alias enables a retired runtime or adds a second Runner.

The CLI checks executable permissions and SHA256
`bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340` before invoking Cargo.
It does not download Goose or load `.env`. It strips inherited `ANCHOR_*`,
`GOOSE_*`, `OPENAI_*`, `ANTHROPIC_*` and `DEEPSEEK_*` configuration and forwards
only the verified Goose path and isolated test evidence directory.

It first builds the official WeCom and Docmost tool binaries, then runs each of
`goose_acp`, `goose_pilot`, `goose_elicitation`, `goose_media`, `goose_conversation`,
`goose_channel`, `goose_compaction`, `goose_pilot_compaction`, `goose_trace`,
`goose_session_calls`, `goose_library` and `native_plugins` with `--no-default-features`,
`--locked` and `-- --ignored --test-threads=4 --nocapture`. No legacy feature is
enabled; non-ignored helper tests are intentionally filtered.
The selected ACP suite includes explicitly retained spike negative cases.
The elicitation suite covers native form answers, false/decline/cancel/true deletion
decisions, changed Graph/resource preconditions, stop and Host restart without
replaying old confirmations. Each scenario is a separate test and evidence record.
The media suite uses a vision-capable model name with the same deterministic local
Provider, not a real vision service. It covers native PNG/JPEG/WebP MCP blocks,
mixed ordering, a PNG result above the old 1MiB ACP frame limit, actual OpenAI
`image_url` payloads, receipts, workspace/Artifacts, malformed-media rejection
and same-session restart with state inspection instead of repeating a fake effect.
Passing it proves the transport path, not real-model image understanding.

The conversation suite checks same-session turns across Host restart, per-user/node
isolation, read-only previous workspaces, skipped nodes, same-Run loops, frozen
image inputs, fail-closed identity/model bindings and whole-Graph cleanup. Unknown
effects are inspected in a new turn after explicitly settling the old Run, without
replaying it. The channel suite uses a private local Unix gateway: two identical
messages receive different native tool-call identities and ACKs; unauthorized
recipients never connect; an effect before a missing ACK is inspected after restart
without resending. No message reaches real WeCom.

Acceptance requires every suite to execute and pass without ignored tests, plus
one valid on-disk scenario evidence report per passed test. Nested reports must
declare a passing status, the pinned Goose digest, zero real model calls, and no
production data. They must match the current Host binary and a completed,
error-free deterministic local Provider transcript. Pilot reports need not use
the older flat report schema. The existing startup-rejection report lacks a Host
digest and production flag; it is accepted only with explicit rejection before
Graph admission/Goose startup, no Provider requests and no fake external effects.

Each execution creates a private evidence directory with the actual commands,
exit codes, per-suite logs, validated scenario paths, test counts and failure
reason. A failed build or evidence check cannot be accepted just because test
counts look successful. These bounded fixtures do not claim every product
capability, live-model compatibility, or production readiness.

## Native operations

`anchor-devtools preflight` checks a protected deployment environment, pinned
runtime inventory, selected binaries, private mutable roots and writer lease.
It does not contact a provider or start the service. See the
[deployment guide](../../docs/rust-production-deployment.md).

`anchor-devtools cutover` inventories old records read-only. It reports blocked
conditions with exit code 2; explicit preparation writes a protected manifest
and backup index, not a backup or migration. See
[cutover preparation](../../docs/rust-production-cutover.md).

`anchor-devtools regression candidate` builds Web and release binaries, reuses
the Host's extracted-runtime fixture and saves actual command/evidence reports.
See [candidate regression](../../docs/rust-production-candidate.md).

Target paths default to `CARGO_TARGET_DIR` or `/tmp/anchor-native-regression-target`;
evidence defaults to a new private directory under the system temporary directory.
Real provider, full business-Graph, public-service and production-cutover
acceptance remain separate. See the
[standard regression guide](../../docs/runtime-contract-tests.md).
