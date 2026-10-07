# Rust Graph host (experimental)

The standalone protocol and Axum service use the same `anchor-runtime` GraphRunner.
The host executes AgentNode / Op.run graphs with paired local fanout/join, gives every node invocation a
separate workspace seeded from its last committed files on feedback revisits, and
publishes immutable file snapshots. Production Python
Graphs and their records are not migrated by this binary.

## Build and check

From `rust/`:

```sh
cargo build --workspace --bins --examples
cargo test -p anchor-runner-host
```

For routine Runtime regression, use the small deterministic Graph suite instead
of rerunning research or RSI Graphs. It exercises the production Host, Rig,
io-harness, Bubblewrap, Plugin/MCP and persistence with local fixture services:

```sh
cargo build -p anchor-wecom-tools -p anchor-docmost-tools --bins
cargo test -p anchor-runner-host --test runtime_contract --test native_plugins -- --test-threads=4 --nocapture
```

It needs no model credentials and records native histories, files, hashes and
executable identities under `.local/runtime-contract/<test-process-id>/`.
Coverage and remaining boundaries are listed in
[the Runtime contract test guide](../../docs/runtime-contract-tests.md).

The unified low-cost entry point runs from the repository root:

```sh
./.venv/bin/python scripts/rust_low_cost_regression.py
./.venv/bin/python scripts/rust_low_cost_regression.py --mode fixture
./.venv/bin/python scripts/rust_low_cost_regression.py --mode live --case serial --case feedback
```

The default `--mode all` builds both native tool binaries and runs the 23 fixture tests first, then the three real-model
Graphs in `tests/fixtures/runtime_regression/`: serial checks read-only producer
and Plugin skill/resource inputs, feedback revisits writer/reviewer twice each,
and parallel joins two Agent branches. The model itself calls `anchor_run` and
`final_result`; live mode does not use a scripted Provider. Together the three
Graphs require 7 Agent invocations, not a fixed number of model requests or tokens.

The fixture layer includes Rust Session metadata persistence across a Host restart
and the independently packaged native WeCom application API / Docmost attachment
MCP servers through the real Host, Harness and Bubblewrap. Business endpoints are
loopback fixtures, not production services. Native Pilot chat, read-only and
Graph create/update/start/control tools, Turn/native/Run associations, idempotency,
stop/restart and SSE replay are covered without real model
calls. This does not complete Pilot questions/deletion confirmation or the WeCom WebSocket
gateway and does not perform production publishing.

`--mode fixture` makes no real-model calls and does not load `.env`; `--mode live`
runs only the real layer. Repeat `--case` to select serial/feedback/parallel in
live/all mode; omitting it selects all three. all/live load provider configuration
from the environment or `.env` and exit 2 if `ANCHOR_MODEL_API_KEY`,
`ANCHOR_MODEL_URL` or `ANCHOR_MODEL_NAME` is missing; this is not a passing run.

`--target-dir` defaults to `CARGO_TARGET_DIR` or
`/tmp/anchor-runtime-contract-target`. `--binary` selects an already-built Host,
otherwise live uses `<target-dir>/debug/anchor-runner-host`; ensure it exists before
a live-only run. `--timeout` defaults to 180 seconds per live Graph, not a token or
request hard limit. `--evidence-root` selects a parent, defaulting to system temp;
every invocation creates a fresh isolated root, with no production data/backend
changes, message delivery or publishing.

Evidence includes public `GET /runs/{id}` native traces, durable Run facts,
workspace/Artifact bytes and hashes, and native provider recordings with actual
usage. Missing usage is unknown, never zero; partial usage is not a complete total.
These commands define protocol and coverage, not a claim that live acceptance has
passed or every feature is covered. The guide separates fixture-only gaps from
boundaries still outside the combined suite; business acceptance remains separate.

To validate a source-free provider-free distribution after a release build:

```sh
./.venv/bin/python scripts/rust_runtime_package_smoke.py \
  --binary rust/target/release/anchor-runner-host \
  --bundle path/to/graph-bundle
```

The script retains its evidence under `.local/rust-runtime-package-*/` and is
also the deployment harness for the later real-provider acceptance.

The packaged pause/restart/resume smoke uses a deterministic Op-only Graph:

```sh
./.venv/bin/python scripts/rust_runtime_recovery_smoke.py
```

For the local provider-free Python/Rust baseline:

```sh
./.venv/bin/python scripts/rust_python_baseline.py --iterations 5
```

Tests include real Bubblewrap executions. They require Linux, `bwrap`, Git, and usable
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

Original-Graph parity checks use the same release binary:

```sh
./.venv/bin/python scripts/rust_product_parity_smoke.py --binary rust/target/release/anchor-runner-host
./.venv/bin/python scripts/rust_feedback_parity_smoke.py --binary rust/target/release/anchor-runner-host
```

The first uses a controlled local provider and real Bubblewrap to exercise the
unchanged academic Graph's two feedback loops, local inputs and review binding.
The second copies the repository `revise-loop.json` and uses the configured real provider;
it requires the intended draft/review revisits, not merely a completed Run.
The initial Responses attempts missed the revisit (A92). The example now makes
the reviewer choose its route from the file state at pass entry. Python and Rust
Chat, and Rust Responses, passed the corrected 2/2/1 case (A93). The original
example also passed Python and Rust Chat once; this is evidence of instruction
sensitivity, not proof of a Rust scheduler defect.

## WeCom host tools

Nodes explicitly mounting the existing `wecom` Plugin can use
`wecom_send_message`. Configure `ANCHOR_CHANNEL_CONTROL_DESCRIPTOR` with the
absolute path `<platform-root>/state/channels/wecom/control.json` published by
the Python channel supervisor. It is private host configuration, never a Graph
or sandbox input. The host rereads it per send to follow gateway token rotation.
`ANCHOR_WECOM_SEND_USERS` sets recipients and falls back to `ANCHOR_WECOM_USERS`
when empty. The gateway independently enforces the same authorization.

Only the trusted conversation reply node gets `wecom_attach_image`. It accepts
PNG/JPEG under the node's workspace or authorized `/in` and `/previous` mounts.
The existing gateway delivers the prepared `msg_item` with the final response.
Unknown send outcomes are preserved and are not automatically replayed.

Standard Goose sends bind the request ID to the complete invocation key, the
durable native Session and Goose's native tool-call ID from MCP `_meta`. The
identity is verified and saved before calling the gateway; model arguments cannot
provide it. Missing or mismatched identities fail before connecting. Repeated
calls with identical Markdown are distinct sends when their native call IDs differ.
The explicit legacy runtime retains its original attempt identity, not a Goose
fallback. Deterministic acceptance uses a private Unix gateway, not real delivery.

Run real-model acceptance against a local gateway ACK fixture without sending
external messages:

```sh
PYTHONPATH=src ./.venv/bin/python scripts/rust_channel_tools_smoke.py
```

The script verifies deliberate repeated sends, exact reply image bytes and
restart/event deduplication. This is not public WeCom delivery acceptance.

## Host configuration

- `ANCHOR_RUNNER_BUNDLE_ROOT`: a format-1 directory with `manifest.json`,
  `graph.json`, and exactly its declared Plugin resources.
- `ANCHOR_RUNNER_STATE_ROOT`: Rust-owned Run records, completion facts,
  checkpoints and immutable artifacts.
- `ANCHOR_RUNNER_WORKSPACE_ROOT`: disposable node workspaces, isolated by Run and
  full invocation identity. Keep it outside the distributable bundle.
- `ANCHOR_RUNNER_ALLOWED_COMMANDS`: explicit comma-separated executable basenames.
  Op.run executes its original string through `sh -c` and requires `sh` here.
  Authorizing `sh` permits its shell scripts inside the Sandbox; this is not a
  whitelist for each command within a script. The binary supplies `anchor-route`
  inside the sandbox without installing Anchor's Python package.
- `ANCHOR_MODEL_ALIASES`: optional JSON object such as
  `{"models.academic":"research-model","models.review":"review-model"}`.
  Explicit aliases choose that model on the configured endpoint; unknown aliases
  use the default, as in Python. The default wire API is `responses` and default
  model name is `default`. Each new invocation pins a credential-free binding
  digest for recovery; an old invocation without this fact binds its current
  configuration once, without claiming to verify its historical endpoint.
- `ANCHOR_RUNNER_LOCAL_INPUTS_ROOT`: optional operator-owned workspace root.
  `<root>/<graph-name>/local-inputs.json` uses the existing
  `{"node-id":{"name":"/absolute/host/path"}}` format. Standalone use also
  requires `ANCHOR_RUNNER_GRAPH_NAME`. Only the named node receives read-only
  `/local-inputs/<name>` mounts. Grants are outside the Graph bundle and frozen
  per Run; a changed grant requires a new Run.
- `ANCHOR_RUNNER_LIBRARY_ROOT`: optional operator-selected Library containing
  existing `tools/<id>/tool.json` registrations and `plugins/<id>` resources.
  HTTP Graph create/save resolves Plugin resources from this Library when set,
  otherwise from the catalog. Resources are copied into the frozen Graph bundle.
  Agent commands, Ops and stdio MCP
  share these explicitly granted read-only tool environments. This setting is
  deployment authority; an editable Graph or Plugin cannot add host paths.
- MCP configuration is read from the admitted Plugin's canonical `plugin.json`
  and optional `.mcp.json`. The removed deployment-level
  `ANCHOR_RUST_MCP_SERVERS` setting is not read. HTTP MCP requires the
  AgentNode's `network:true`;
  credentials and headers are expanded from the Plugin declaration at bind time.
  stdio MCP is launched through Bubblewrap with the Plugin mounted read-only at
  `/plugins/<id>`.

Immutable upstream views also expose a host-generated `.git` for existing Graphs
that bind review decisions to `git rev-parse HEAD`. The Git view derives from the
authoritative Artifact; Agent-provided root `.git` metadata is not published.
Revisiting a node copies its previous committed business files into a new
invocation workspace. Reopening that invocation preserves its pending edits.

For AgentNodes with MCP Plugins, the model receives remote tools directly under
the Python community convention `<plugin>-<server>_<tool>`. The server inventory
from the MCP handshake is the source of tool schemas; no Anchor search/call proxy
is inserted. Plugin Skill files are mounted read-only and their `/plugins/...`
paths are added to the node instructions.

External tools keep their existing language and dependencies. For example, the
unchanged scholarly registration is:

```json
{"entrypoint":"/opt/scholarly/bin/anchor-scholarly","environment":"/opt/scholarly","imports":["/opt/scholarly-src"]}
```

It exposes `/tools/scholarly/run`, mounts the environment and imports read-only,
and supplies its `bin` directory on PATH. Python environment interpreter symlinks
are followed to mount their prefixes. Explicit MCP interpreter paths are retained;
standalone registered entrypoints can use their `/tools/<id>/run` mount. Plugin
resources and formats are unchanged. Agent `anchor_run` uses its Graph network
setting, still subject to host authorization.

The selected Library is a shared deployment toolset: sorted tool IDs determine
PATH/import order; declared imports supply PYTHONPATH unless MCP sets it explicitly.
Use explicit executable paths and MCP environment values when dependencies overlap.
This does not install dependencies or freeze external environments into Run facts;
operators must keep those environments stable during execution and recovery.
The Runtime binary itself does not need Python; a Python Plugin tool does.

For real-provider acceptance using the repository's unchanged academic Graph,
Plugin, and `.local/demo/library/tools/scholarly/tool.json`, run:

```sh
./.venv/bin/python scripts/rust_plugin_reuse_smoke.py
```

The smoke checks a real Crossref search, persisted tool/provider calls and output
Artifacts under `.local/rust-plugin-reuse-*/`. It proves tool reuse, not a full
academic review or all business Graphs.

Agent completion uses the native `final_result` output tool. Rust converts its
arguments into the internal completion record; the model does not need to format
a JSON answer in plain text. Extra fields are ignored; missing/invalid summary or
route is fed back through Harness correction. Mixed business/completion calls
execute the business tools and require a fresh completion after their results.
The smoke also verifies the stored native completion-call projection. Graph and
Plugin authoring do not gain another field or format. Completion-mode text is
buffered until the response is complete; existing stop/recovery boundaries apply.

`Op.run` executes the original shell string, e.g. `cat /in/producer/report.txt > output.txt`,
through `sh -c`, preserving the Python Graph contract. Node network declarations
remain subject to the host's Sandbox policy; shell execution does not grant host filesystem access. AgentNode receives the same Sandbox via the `anchor_run`
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

Standard Host AgentNode execution uses Goose; io-harness/Rig are retained only in the explicit legacy-regression build. The legacy `TaskContract` carries Anchor's `{summary, route?}` output schema; io-harness validates the final text locally and returns schema feedback when needed. Its Rig adapter does not force `response_format` onto providers that reject it, and Anchor still validates the route. The host resolves per-node model references through `ANCHOR_MODEL_ALIASES` on the deployment endpoint. There is no estimated default research-round ceiling. Explicit cumulative `max_steps` currently rejects. Agent `reads` / `writes` declarations are admitted and used to derive input interfaces; they do not yet impose per-file write permissions.

## Goose AgentNode

The standard build defaults to Goose. Use a separate state root, a pinned
Goose v1.53.0 `ANCHOR_GOOSE_BINARY`, its matching `ANCHOR_GOOSE_BINARY_SHA256`,
and explicit `ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1`. The latter permits the
runtime control network; it does not provide OS-level outbound isolation.
Goose calls `ANCHOR_MODEL_URL/API_KEY/NAME` directly using the configured
`ANCHOR_MODEL_WIRE_API` (`chat` or `responses`). Aliases are explicit and unknown
references reject. The existing `.env` is not changed or silently reinterpreted.
The current DeepSeek thinking/Responses combination failed real acceptance;
an explicit Chat configuration passed, without fixing that Responses boundary.

Goose owns native history and context. Anchor retains Graph/Run, Plugin/Sandbox,
Artifact, invocation identity and the latest durable business-tool observation.
Resume loads the same ACP Session, requires fresh tool feedback before completing
an interrupted business invocation, and tells the Agent to inspect unknown effects
rather than blindly repeat them. Native tasks have no Anchor default short wall
time, but cancellation, configured wall time and protocol memory bounds remain.
Configured cumulative request budgets remain unsupported. Pilot and AgentNode
share Goose in the standard dependency closure; static media and trusted cross-Run
conversations have the targeted acceptance described below. Production deployment
and existing data have not migrated.

Run low-cost Goose regression from the repository root with the pinned binary:

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools --locked -- \
  regression goose-fixture --evidence-root /tmp/anchor-goose-regression
```

This executes real Goose with deterministic local Providers, not mocked ACP or
real research tasks. It checks native requests/results, workspace, immutable
Artifacts, Plugin instructions, permissions, cancellation and unknown-effect
resume. It does not load `.env`, call a real model, send production messages or
publish pages. The entrypoint builds official tools and validates one evidence
report per selected scenario across twelve suites, including installation of
frozen Plugin resources and the actual Rust text gateway against a local
WebSocket platform fixture. The explicit `regression fixture` mode retains the
legacy suite; it is not standard Goose acceptance.

The independent Rust [distribution builder](../anchor-distribution/README.md)
packages an operator-reviewed Host, pinned Goose, declared bundle resources and
optional native tools/Web assets. Its separate integration fixture extracts the
archive and resumes the same native Session without replaying a workspace
effect:

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
ANCHOR_TEST_EVIDENCE_ROOT=/tmp/anchor-goose-distribution \
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --no-default-features --locked --test goose_distribution -- \
  --ignored --test-threads=1 --nocapture
```

This source-free local execution is not acceptance on an OS without Python,
a release candidate, public-channel delivery or a production cutover. Archive
creation is kept outside the frequent fixture entrypoint.

## Native Library And Text Transport

Authenticated `POST /plugins/install` accepts `source`, optional `id`, and
`replace` (default false). It installs an HTTPS GitHub tree through
[anchor-library](../anchor-library/README.md); invalid sources reject before
checkout and errors do not expose credentials or subprocess output. Local
directory installation is operator CLI/library-only. Installation and Graph
freezing use the catalog gate and shared cross-process lease; HTTP cancellation
does not release the blocking mutation's ownership. Already frozen Graphs retain
their copied resources after Library replacement. The Python compatibility
installer and OAuth paths have not changed.

[anchor-wecom-gateway](../anchor-wecom-gateway/README.md) is a separate native
text transport compatible with the Host's private Unix descriptor contract.
Configure it with its own native state directory and configure the Host's
`ANCHOR_CHANNEL_CONTROL_DESCRIPTOR` and recipient authorization explicitly.
Unknown deliveries are not automatically resent after reconnect or restart.
The adapter rejects unsupported media and does not take over platform Session,
relations, channel supervision or existing Python ledger data.

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
with completed/unknown branch outcomes. Serial Op.call supports wait/detach through the same Runner. Parallel Op.call and nested calls remain unsupported. `call.session` wait/detach is supported through the Python Session host and existing channel ledger; provider-free and local-ACK restart acceptance cover identity, mapping, yielding, settlement, and deduplication. The standalone Rust timeline
does not manage schedules; an optional Python platform adapter retains them.

## HTTP Run lifecycle

HTTP admission writes the frozen Run and its immutable Graph identity before
returning 202. The metadata holds the Graph name, snapshot digest, original bundle
path, creation time and trigger source; it does not duplicate execution status.
Runs of different Graph names can execute concurrently, including identical
Graph definitions. Manual triggers for a Graph with an active, paused or orphaned
unfinished Run return 409. Paused and stopped Runs remain resumable and block
a new trigger for that Graph. Continue the existing Run to completion, or
explicitly stop and delete its record before starting another.

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

## Native Pilot Session slice

The standard Host defaults to Goose; `ANCHOR_RUNNER_AGENT_RUNTIME=goose` is optional.
Its normal/build/dev dependency closure excludes io-harness and Rig. Kernel defaults
also exclude them; the explicit `legacy-regression` feature retains old fixtures,
not a production fallback. Without Agent configuration, Op execution still works.
Goose does not take over existing legacy Agent state or migrate deployment data.
Pilot uses the same pinned ACP
transport, session initialization/loading, model configuration and authorized MCP
bridge as AgentNode. The existing fifteen Pilot tools remain available, with native
`ask_user` and confirmed `graph_delete` in the MCP context, without
node `final_result`, filesystem or terminal access. Goose owns the loop and native
history; no hidden Graph or separate Pilot loop is introduced. This path does not
apply the legacy 12-step/24000-token/90-second limits; cancellation is retained.
New messages and resumed Turns load the same opaque Goose Session. Legacy records
are not migrated or reused, and missing/mismatched retained Goose facts reject
instead of silently starting a replacement Session.

Platform schema v4 adds `Turn.goose {scope,session}`; the existing `native`
integer association remains unchanged and cannot coexist on the same Turn.
ACP text chunks and actual ToolPort results persist through the existing Turn
events and SSE cursor replay. `goose.json` stores only runtime identity and the
latest tool observation; full native history stays inside `process/`. Restart
marks unfinished business Turns interrupted without automatic model execution.
Fixtures cover follow-up, restart, SSE replay, mutation-state inspection and cancel;
an isolated real Chat check also reads a random Graph, starts its tiny Op Run and
reads the immutable Artifact. Native form questions, condition-bound deletion,
stop/restart interruption and a targeted real Rust/Goose browser flow are also
verified; native compaction/Plugin/restart fixtures and a real short text-session
check are recorded in A121. Full browser and arbitrary long-session acceptance
remain pending. Run traces project public ACP messages/tool results while a prompt
is active, then use the existing saved display evidence and unresolved observations,
without reading Goose's private database. The process-local projection reuses the
bounded notification tail and validates the native Session; it is not a second
recovery log. Fixed Goose does not expose token-wise `final_result.summary`
arguments, so assistant drafts are not canonical replies. Standard MCP inline image
results now cross the Kernel and Goose bridge as native MCP Image blocks, rather
than serialized JSON text. Image blob resources use their inline bytes; their URI
does not grant filesystem/network access and is never dereferenced. Text/JSON/image
ordering is preserved after a small receipt header. Goose's native OpenAI formatter
emits image-only user messages and tool placeholders for vision-capable models;
for non-vision models it explicitly omits images. Anchor does not silently select
a different model or claim all configured models support vision.

Only fully decoded static PNG/JPEG/WebP are supported: canonical base64, matching
MIME, 10MiB per image, 20MiB total, eight images, 20 million pixels and 128MiB
decoder allocation. Corruption, animation and unsupported MIME fail the complete
typed result without partial text/images. The codec policy is shared with channel
attachment validation. Raw MCP and explicit legacy adapters retain their existing
contracts. ACP frames are bounded at 32MiB, the notification tail at 64MiB.
Invocation facts and tool-call observations retain image size/MIME/SHA256, not
another base64 copy; native Goose history and saved ACP display records retain
the original media. Image-bearing public Run traces expose native `contents`.
The deterministic Graph suite covers transport, history, Artifacts and restart;
the UI preserves native Text/Image order and pairs tools by `tool_call_id`, with
thinking folded separately. Inline static PNG/JPEG/WebP previews, image decoding,
original-image viewing and desktop/mobile layout have targeted UI-fixture browser
coverage; URLs/resources/SVG are not embedded. A separate actual Rust/Goose text
browser check covers active trace and reopening the node after refresh. Real
vision-provider, complete Goose media-to-UI and public-channel acceptance remain
pending (A122).

Conversation Runs bind a Goose scope to the trusted bundle path, Session and full
node ID. An isolated `work/.goose-process/conversations/gc1-<digest>/process/`
holds the native history across turns; `scope.json` only binds the pinned binary,
model/endpoint, native Session and latest invocation. The stable scope lease
serializes execution and cleanup. Missing facts, symlinks, lineage changes or
model/endpoint drift fail closed instead of creating a replacement history.
The invocation fact is durable before the scope advances; a saved Session can
reconcile the corresponding interrupted index write.

New turns and same-Run node revisits load the bound Session but remain distinct
invocations, with separate business workspaces and immutable Artifacts. Prior
files are frozen read-only under `/previous`; prior records remain available via
`anchor_conversation_history`. An unsettled old Run must first be stopped through
the existing Run control before admitting a successor; this does not call a model.
Unknown side effects require fresh state inspection and a new business receipt,
not blind resending or an internal recovery approval workflow. A successor blocks
ordinary old-Run resume. Only an admitted pending `call.session` can include trusted,
fully settled successors when resuming its original Run and native Session. This
does not rewrite logical predecessors or `/previous`, and still requires fresh
business receipts. Actual Goose fixtures cover wait/detach, local ACKs, restart,
identity rejection and yield/foreground/resume; the complete channel supervisor
and public delivery are not thereby accepted (A122).
Individual conversation-Run deletion remains refused; whole-Graph
deletion removes its shared scopes while keeping other Graphs.

Authorized frozen channel images enter ACP as native image prompt blocks and reach
the deterministic vision formatter without host-path grants. This is transport
acceptance, not proof the default model can see images. The short real
`deepseek-flash` Chat acceptance covers two text turns, Host restart, the same
native Session, previous-file inspection and four exact Artifacts (A119).

Schema v5 adds owned Question/Answer facts without changing Turn statuses. A
pending question keeps the same running Turn and sets the Session to `waiting_user`.
`GET /sessions/{session}/turns/{turn}/questions` returns saved questions;
`POST /sessions/{session}/turns/{turn}/questions/{question}/answer` accepts
`{"action":"accept","content":{...}}`, `{"action":"decline"}` or
`{"action":"cancel"}`. Answers are validated and persisted transactionally with
the Session snapshot and existing delivery events. Identical retries return the
saved answer without another Turn or model execution; conflicting answers fail.

MCP native elicitation is relayed by Goose over ACP. Only flat primitive/enum form
schemas that survive native MCP conversion are accepted; remote references,
nested forms and URL elicitation are not supported. `graph_delete` accepts only
the Graph name; the Host captures the exact precondition and requests a fresh
native `confirm:boolean` response. Only an explicit true can delete, and target
drift or cancellation rejects it. Restart interrupts pending questions rather
than replaying confirmations or pretending Goose's in-process wait survived.
Saving an answer is not proof its ACP response or a destructive tool completed.

The targeted browser test uses the real Rust Host, pinned Goose and a local
scripted Provider, including refresh of a pending form and reject/confirm deletion:

```sh
ANCHOR_TEST_GOOSE_HOST_BINARY=/absolute/path/to/anchor-runner-host \
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
npm --prefix apps/web run test:e2e -- pilot-goose-elicitation.spec.ts
```

Build `apps/web` first. `ANCHOR_BROWSER_BINARY` optionally selects an installed
Chromium executable. This test makes zero real-model calls and is not the entire
browser/product acceptance suite.

The legacy Pilot behavior described next requires `--features legacy-regression`;
only that explicit development build defaults to the legacy backend.

The standalone Host serves ordinary Pilot Sessions without a Python backend.
`POST /sessions` accepts an empty body from the existing Web composer or an
object containing optional `id` and `title`. The native endpoints are:

- `GET /sessions/{session}/messages` and `/turns` for history and Turn metadata.
- `POST /sessions/{session}/turns` with `request_id` and either `message` or
  `resume:true`; the same input keeps the same Turn, different input conflicts.
- `GET /sessions/{session}/turns/{turn}` and `/events?after=...`; SSE also accepts
  `Last-Event-ID`, replays durable chunks and ends with `event: turn`.
- `POST /sessions/{session}/stop`; stopping is requested at the next safe native
  boundary, not an immediate HTTP transport abort.

The shared `anchor-platform-session` store owns business metadata and UI delivery;
io-harness owns Agent history and unfinished tool facts. Startup interrupts
leftover business Turns without replay. Pilot uses native Sessions directly, not
a hidden Graph. It keeps nine read-only tools and adds Graph creation/update and
Run start/pause/resume/stop tools for explicit user requests. Mutating calls require
the owner's running Turn; HTTP and Pilot reuse the same application locks, staging
and frozen resources. Native FS/exec tools are masked. Deletion confirmation,
questions, Graph/channel Session
admission, complete Responses and retained native-history cleanup remain follow-up
work; the existing Python platform still provides its original full Pilot.

Legacy Turn metadata exposes `native {scope,session,run}` and associated `runs`.
Exact platform schema v1/v2/v3 upgrades transactionally to v4 without converting legacy history. Native identity is
bound before the first provider request. Accepted Graph Runs persist their Pilot
source before dispatch; startup repairs missing business associations without
replaying execution or writing late Turn delivery. This is reconciliation, not a
cross-store atomic transaction. The RunApplication dispatches on its owning Host
executor, never the temporary Pilot executor, so a reply does not kill the Graph.
Harness masking still enforces denied builtin calls; because a native mask keeps
schemas for cache stability, Pilot separately sends only its fixed ToolPort
catalogue through Rig. This does not widen permissions or change the AgentNode
catalogue, native loop or the 24000-token budget.

The optional real-provider check only reads a tiny local Graph twice:

```sh
./.venv/bin/python scripts/rust_native_pilot_smoke.py --binary /tmp/anchor-runtime-contract-target/debug/anchor-runner-host
```

Provider environment must already be configured; this driver does not load dotenv
or build implicitly. It does not replace `scripts/rust_pilot_smoke.py`, which still
checks the Python Pilot compatibility path. Routine fixtures include the native
Pilot cases without real model calls. `apps/web/e2e/pilot-native.spec.ts` additionally
uses the current React build, a real Rust Host and a deterministic HTTP Provider
when `ANCHOR_TEST_NATIVE_PILOT_BINARY` is set.

## Existing platform with Rust execution

Keep the Rust host running with its own catalog, state and workspace roots. Point
`ANCHOR_RUNNER_LIBRARY_ROOT` at the existing platform Library to reuse Plugin and
tool registrations. Start the existing platform with:

```sh
ANCHOR_RUNTIME_BACKEND=rust ANCHOR_RUNTIME_URL=http://127.0.0.1:8766 \
  python -m anchor --root /path/to/platform --config /path/to/runtime.json --port 8765
```

Set the Rust host's `ANCHOR_RUNNER_LISTEN` to the matching upstream address.
Optional `ANCHOR_RUNTIME_API_KEY` authenticates upstream calls;
`ANCHOR_RUNTIME_TIMEOUT_SECONDS` defaults to 15. The adapter does not retry
mutations or fall back to Python execution. Keep this deployment opt-in while
full channel capabilities, OAuth execution credentials and summary/image support remain pending.
Legacy Python Runs are read-only; an ID collision returns 409.
No existing production catalog or historical state is automatically migrated.

Python owns schedules, Library and the existing Pilot Session/Turn dialogue;
Rust owns Graphs, Runs and Artifacts. Pilot tools use the public Runtime ports for
Graph validation, creation, Run control and artifact reads. The Pilot model loop
still uses the existing Python Harness; this does not make the entire platform a
standalone Rust binary.

`POST /graph-validation` accepts `{"definition": {...}}` without saving or
executing. It checks authoring syntax, expanded nodes, installed Plugins and
supported static Host constraints; success does not validate provider credentials
or runtime grants. Invalid requests return 400; semantic errors return 422.

Manual and scheduled triggers use the same admission. A trigger's optional objective is
frozen into that Run only. `control_requested` shows the current stop/pause
request until the node settles; stopped Runs can continue without replaying
committed node results. The schedule history uses Run file mtime for updates,
so file copies/migrations and old paused intervals limit historical precision.

Run the opt-in real Pilot acceptance with the repository venv and built host:

```sh
./.venv/bin/python scripts/rust_pilot_smoke.py
```

It uses the configured Pilot provider in disposable roots and retains native
model/tool evidence. The default path pauses an Op-only Rust Run, restarts both
services, resumes that same Run and compares its artifacts. It sends no business
messages. `--skip-controls` explicitly leaves pause/resume unverified.

Production model calls save per-invocation attempts under
`state/io-harness/store/np1-<key digest>.recordings/`. They contain the Harness
request, adapted Rig request, typed response, native Harness Record, and outcome.
Actual final_result arguments are retained before projection. Files are 0600 and
new directories 0700. These contain business/model content, exclude transport
raw documents, and are not a complete HTTP/partial-stream capture. Requests are
saved before sending; incomplete attempts remain visible and do not drive
recovery. Deleting an ordinary Run also removes its recordings; conversation Runs
are deleted with the whole Graph scope. See the
[io-harness adapter README](../anchor-io-harness-runtime/README.md).

Local acceptance for this platform adapter:

```sh
npm --prefix apps/web run test:e2e -- platform-rust.spec.ts rust-stop-final.spec.ts --workers=1
./.venv/bin/python scripts/rust_platform_plugin_smoke.py --binary rust/target/release/anchor-runner-host
```

The browser checks use isolated service roots and a controlled completion provider.
The second command uses the configured real model and Crossref, reusing the original
Plugin and scholarly tool. Neither publishes or sends business messages.

### Text Graph conversations through the platform

`POST /conversation-runs` accepts `graph`, deterministic `run`, trusted `session`,
expanded `reply_node`, `input`, and optional `previous_run`; it returns 202 with
`{run, graph}`. Identical accepted requests are idempotent, conflicting IDs or
unsettled predecessors return 409. Invalid contracts return 400/422. Each Session
serializes its turns; different Sessions can run the same Graph concurrently.
Ordinary triggers retain their exclusive admission rule. `GET /graphs/:name`
adds the expanded `node_plugins` mapping used by the existing channel supervisor.

Python retains source authorization, Session/Turn, event deduplication and gateway
delivery. Rust owns the Run and artifacts; native io-harness Sessions retain
per-node history. New input stops the prior turn, waits for execution to settle,
and suppresses superseded replies. An older conversation Run and its continuous
wait-call descendants cannot resume after a successor is admitted; detach tasks
keep their independent lifecycle.

The host grants readonly `/previous` from the nearest same-node predecessor,
including unfinished files. `anchor_conversation_history` queries actual prior
node records, including calls whose result is unknown; these are not replayed or
represented as successful. Neither capability permits another Session's history.
Individual conversation Run deletion is rejected. Whole Graph deletion, after
execution checks, removes its native conversation scopes and per-Run records.
Python Session deletion is blocked while associated Rust conversation Runs remain.

The channel slice reuses the original WeCom Graph and Plugin. Trusted downloaded
attachments are read once by the Python ingress, frozen into the Rust Run with
hash/MIME metadata, and exposed to Agent and Op sandboxes as readonly
`/in/channel` files. PNG/JPEG/WebP images are also passed as native model media
when the selected wire model is listed in `ANCHOR_MODEL_IMAGE_MODELS`; the list
uses actual wire names and never silently drops images. Limits are 16 files,
20 MiB per file, 50 MiB total, 8 images, 10 MiB per image, 20 MiB total images,
and 20 million pixels. Retries compare source descriptors and the frozen
manifest, so they can succeed after the original downloaded files are removed
without rereading them. Incremental summary remains separate work; proactive send/reply-image host tools and `call.session` use the existing channel host, with `call.session` wait/detach now connected. Optional unconfigured MCP
servers are omitted; undeclared tool references are still invalid.

The trusted admission port accepts optional `attachments`, each containing
`name`, `data_base64`, and optional `media_type`; it never accepts a host path.
Run detail exposes only `name`, `sha256`, `size`, and `media_type`. Upload order
is retained, including image order. Animated images are rejected. This route
has a 72 MiB encoded body limit; other routes retain their existing limits.
For the configured vision model, set for example:

```sh
export ANCHOR_MODEL_IMAGE_MODELS='["deepseek-flash"]'
./.venv/bin/python scripts/rust_channel_media_smoke.py --binary rust/target/release/anchor-runner-host
```

This acceptance uses isolated services, files and normalized events. It checks
actual image recognition, exact provider image bytes, event retries after source
deletion, two users and service restart. It does not send WeCom business messages.

```sh
./.venv/bin/python scripts/rust_channel_smoke.py --binary rust/target/release/anchor-runner-host
./.venv/bin/python scripts/rust_channel_control_smoke.py --binary rust/target/release/anchor-runner-host
```

The first requires configured model environment variables and checks two users,
deduplication, service restart, native history and previous files. The second uses
a controlled model, real services and sandboxes, supersession and an actual Rust
process kill during a tool. Both use disposable roots and normalized events;
neither starts a production gateway or sends WeCom business messages.
