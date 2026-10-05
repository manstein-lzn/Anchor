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

Host AgentNode execution uses io-harness as the only Agent loop. Its `TaskContract` carries Anchor's `{summary, route?}` output schema; io-harness validates the final text locally and returns schema feedback when needed. The Rig adapter does not force `response_format` onto providers that reject it, and Anchor still validates the route. The host resolves per-node model references through `ANCHOR_MODEL_ALIASES` on the deployment endpoint. There is no estimated default research-round ceiling. Explicit cumulative `max_steps` currently rejects. Agent `reads` / `writes` declarations are admitted and used to derive input interfaces; they do not yet impose per-file write permissions.

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
