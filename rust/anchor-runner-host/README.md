# Rust Graph host (experimental)

The standalone protocol and Axum service use the same `anchor-runtime` GraphRunner.
The host executes AgentNode / Op.run graphs with paired local fanout/join, gives every node invocation a
separate workspace, and publishes immutable file snapshots. Production Python
Graphs and their records are not migrated by this binary.

## Build and check

From `rust/`:

```sh
cargo build --workspace --bins --examples
cargo test -p anchor-runner-host
```

Tests include real Bubblewrap executions. They require Linux, `bwrap`, and usable
user/mount/network namespaces. They fail rather than silently skip isolation.
For opt-in real-model acceptance, configure `ANCHOR_MODEL_API_KEY`,
`ANCHOR_MODEL_URL`, `ANCHOR_MODEL_NAME` and optionally `ANCHOR_MODEL_WIRE_API`
(`chat` or `responses`), then from the repository root run:

```sh
./.venv/bin/python scripts/rust_multinode_smoke.py
```

The Python verification script loads `.env`, prepares a disposable bundle, launches
Rust binaries, and checks artifacts. Graph execution, model calls, the local HTTP
MCP fixture and Sandbox are Rust. It does not use the Python Scheduler/Harness.
Evidence stays under `.local/rust-multinode-*/`. The MCP server is a local fixture,
not a business integration. Model requests use the real configured provider.

## Host configuration

- `ANCHOR_RUNNER_BUNDLE_ROOT`: a format-1 directory with `manifest.json`,
  `graph.json`, and exactly its declared Plugin resources.
- `ANCHOR_RUNNER_STATE_ROOT`: Rust-owned Run records, completion facts,
  checkpoints and immutable artifacts.
- `ANCHOR_RUNNER_WORKSPACE_ROOT`: disposable node workspaces, isolated by Run and
  full invocation identity. Keep it outside the distributable bundle.
- `ANCHOR_RUNNER_ALLOWED_COMMANDS`: explicit comma-separated executable basenames.
  Authorizing `sh` permits its shell scripts inside the Sandbox; this is not a
  whitelist for each command within a script.
- `ANCHOR_RUST_MCP_SERVERS`: deployment-owned JSON map from manifest server ID to
  `{"transport":"http","endpoint":"...","allowed_tools":["..."]}` with optional
  `bearer_token_env` naming a credential environment variable. A node must also
  declare `network:true` to connect HTTP MCP. Reserved or duplicate tool names are
  rejected before connection. `anchor_run` itself remains network-disabled.

`Op.run` strings are parsed as quoted argv using shlex. A shell script must be
explicit, e.g. `sh -c 'cat /in/producer/report.txt > output.txt'`; it does not gain
host filesystem access. AgentNode receives the same Sandbox via the `anchor_run`
JSON tool (`{"command":["cat","/in/producer/report.txt"]}`). Its prompt includes
the configured command names and exact upstream mount destinations.

Writable outputs go under `/workspace`; each selected upstream commit and its authorized immutable ancestors
are mounted read-only at `/in/<node-id>`. Fanout forwards its upstream snapshots. Join publishes `join.json` and forwards
completed branch snapshots; no branch can read a sibling’s live workspace. The
Coordinator supplies typed artifact provenance; model output cannot grant access
to additional commits. BFS chooses the nearest ancestor version for each node,
while conflicting same-depth versions, cycles and cross-Run links are rejected.
New `fs2` manifests persist this provenance; existing `fs1` snapshots remain
readable without inventing missing parent links. Files are independent copies bound to
Run, Graph digest, node and invocation; changing a live workspace does not change
a commit. Symlinks and special output files are rejected, not followed. Snapshot
publication uses file/directory fsync and rename; reads verify the file digests.
The threat model excludes hostile same-user concurrent replacement of host paths.

Host AgentNode execution uses io-harness as the only Agent loop. Its `TaskContract` carries Anchor's `{summary, route?}` output schema; io-harness validates the final text locally and returns schema feedback when needed. The Rig adapter does not force `response_format` onto providers that reject it, and Anchor still validates the route. The host uses one deployment-configured model Provider today; per-node model alias resolution is not implemented. There is no estimated default research-round ceiling. Explicit cumulative `max_steps` and custom Agent `reads` / `writes` policies currently reject instead of being silently ignored.

## Entrypoints and recovery

`anchor-runner-host serve` starts the experimental API. Set
`ANCHOR_RUNNER_LISTEN=127.0.0.1:8078` when the legacy service uses 8077. Non-loopback
binds require `ANCHOR_API_KEYS`; Graph/Run management APIs and file projections
are initial slices, not a complete React backend. File endpoints now read the
requested node's latest committed snapshot; no committed result means no file
projection. Downloads stream, previews read at most 1 MiB. Existing legacy JSON
artifacts can supply completion metadata but cannot supply file snapshots.

Without `serve`, stdin/stdout uses 4-byte big-endian length-prefixed JSON v1.
`start_bundle` accepts `version`, `request_id`, `run_id`, and `input`; paths and
permissions come only from host configuration. Repeating a completed Run with
identical snapshot/input returns its durable state without executing again.

Agent and Op nodes persist execution facts before side effects. A durable io-harness run id lets the same Agent invocation re-enter Harness recovery after host restart. If an external tool effect is indeterminate, the host records a recovery observation in the same Agent context and does not replay the tool; the Agent inspects the workspace and available external state before continuing. A `.started` fact without a Harness cursor, legacy Rig-only uncertainty, or conflicting backend completion facts stays fail-closed. The internal recovery endpoint remains for compatibility, while normal users use the ordinary resume control.

Paired, non-nested fanout/join is supported, with real host tests for overlapping
branch execution, immutable inputs, join collection, failure and process crash
with completed/unknown branch outcomes. Op.call remains rejected at this host
boundary. Timeline/schedules, Session/channel services, stdio MCP Sandbox launch
and React acceptance remain separate work.

## HTTP Run lifecycle

HTTP admission writes the frozen Run and its immutable Graph identity before
returning 202. The metadata holds the Graph name, snapshot digest, original bundle
path, creation time and trigger source; it does not duplicate execution status.
Runs of different Graph names can execute concurrently, including identical
Graph definitions. Manual triggers for a Graph with an active, paused or orphaned
unfinished Run return 409. A paused Run must be resumed or explicitly stopped
before another manual trigger for that Graph.

`POST /runs/{id}/pause` requests a pause after the current node/parallel wave
settles. `resume` starts the same shared Runner from the saved snapshot and input,
even after service restart or Graph edits. It verifies Plugin identities against
the original bundle resources; missing/changed resources reject recovery. The
response acknowledges a control request, not that the Run has already settled.
`stop` cancels active work or marks an inactive Run stopped without dispatching it.
Completed and failed Runs reject control. An interrupted started node with no
terminal fact remains uncertain and is never automatically replayed.

One OS lease permits one writing host per state root. HTTP holds it for the service
lifetime; framed standalone execution holds it while executing. Another HTTP or
standalone writer is rejected; framed `status` remains read-only and available.
A process exit releases the lease. This is single-host ownership, not a distributed
scheduler. HTTP workers can run different Graphs concurrently inside that host.
Older standalone Runs without identity metadata appear as `graph=unknown` in the
list and cannot be controlled through HTTP. An unfinished unowned Run blocks new
HTTP admission and Graph mutation; complete/reconcile it through its original
entrypoint or use a separate deployment root. The API does not guess its Graph
name from a matching digest.

Run detail keeps committed upstream nodes submitted even when a later node fails.
Unknown exit status is null and unknown update time is empty, not synthesized from
whole-Run status or creation time. The full timeline/trace product projection is
still pending.

Opt-in real provider and TCP acceptance, using disposable directories:

```sh
./.venv/bin/python scripts/rust_lifecycle_smoke.py
```

This checks pause, process restart, frozen-snapshot continuation, Plugin drift,
writer exclusion, real Agent/MCP/Sandbox output, and kill-before-terminal refusal
to replay. It is not a test of arbitrary Agent tool recovery, production migration
or browser parity.
