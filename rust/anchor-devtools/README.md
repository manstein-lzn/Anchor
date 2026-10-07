# Deterministic regression entry points

## Standard Goose fixtures

Configure the pinned Goose 1.53.0 x86_64 musl binary explicitly:

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target \
CARGO_BUILD_JOBS=2 \
cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools -- regression goose-fixture
```

The CLI checks executable permissions and SHA256
`bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340` before invoking Cargo.
It does not download Goose or load `.env`. It strips inherited `ANCHOR_*`,
`GOOSE_*`, `OPENAI_*`, `ANTHROPIC_*` and `DEEPSEEK_*` configuration and forwards
only the verified Goose path and isolated test evidence directory.

It first builds the official WeCom and Docmost tool binaries, then runs each of
`goose_acp`, `goose_pilot`, `goose_elicitation`, `goose_media`, `goose_conversation`,
`goose_channel` and `native_plugins` with `--no-default-features`,
`--locked` and `-- --ignored --test-threads=4 --nocapture`. No legacy feature is
enabled; non-ignored helper and legacy Plugin tests are intentionally filtered.
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
the legacy report schema. The existing startup-rejection report lacks a Host
digest and production flag; it is accepted only with explicit rejection before
Graph admission/Goose startup, no Provider requests and no fake external effects.

Each execution creates a private evidence directory with the actual commands,
exit codes, per-suite logs, validated scenario paths, test counts and failure
reason. A failed build or evidence check cannot be accepted just because test
counts look successful. These bounded fixtures do not claim every product
capability, live-model compatibility, or production readiness.

## Legacy fixtures

From the repository root:

```sh
cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools -- regression fixture
```

This entry point does not load `.env` or call a real model. It removes inherited
`ANCHOR_*` configuration before invoking Cargo and provides only the fixture
evidence directory. Host fixtures supply their own deterministic loopback model
and business endpoints.

The entry point explicitly enables the development-only `legacy-regression`
Host feature. The public Rust `run_fixture` API remains available. It runs the
entire `runtime_contract` suite without filters, followed
by exactly these two `native_plugins` tests in one exact-filtered invocation:

- `rust_wecom_stdio_plugin_runs_through_host_harness_and_readonly_package`
- `rust_docmost_stdio_plugin_uploads_only_frozen_input_through_real_sandbox`

`goose_acp` is not selected. The Goose test in `native_plugins`, including any
future tests outside the exact selection, is filtered out rather than counted as
an ignored required fixture. Use `regression goose-fixture` for Goose. The legacy
entry point never passes `--include-ignored` or `--ignored`.

The Runtime suite must have no ignored or filtered tests. Both selected Plugin
tests must pass with no ignored or failed tests. Evidence records the two commands,
their exit codes, the exact Plugin selection, and actual aggregate counts,
including intentionally filtered tests. Missing or invalid scenario evidence
still fails acceptance. Each suite has a separate log alongside `fixture.log`.

Fixture acceptance is not live-model, complete-product, or production acceptance.
