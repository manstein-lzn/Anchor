# Anchor Architecture

## Product-first constraint

The final product is a visual graph authoring and operations experience for
long-running multi-agent work. Backend abstractions are only useful if they support
that workflow without sacrificing recoverability, auditability, quality gates, or
vendor neutrality. A component that is easy to demo but cannot preserve graph
versions, node state, or side-effect semantics is not an acceptable shortcut.

## Runtime layers

```text
domain/          graph IR, propagation, conditions, content refs, projects
    ↑
state/           canonical state, event stream, leases, ledgers (one transaction core)
    ↑
runtime/         workers, control/verifier/scheduler/supervisor services, gateway,
                 sandbox, workspaces, content, behaviors, evidence policy
    ↑
api/             composition root: auth, routes, dependency wiring

adapters (may import anything below):
  cli.py, client.py   the typed operation layer and its CLI
  mcp.py              MCP server over stdio
```

`test_architecture.py` enforces the direction (`domain <- state <- runtime <- api`),
the content-reference boundary, the node-dispatch ratchet and a module size budget.

Services are separate processes, each a composition root:

| Entry point | Role |
|---|---|
| `runtime/worker_service.py` | claims agent nodes, runs the model tool loop |
| `runtime/control_service.py` | control nodes: routing, joins, behaviors |
| `runtime/verifier_service.py` | verifier nodes |
| `runtime/receiver.py` | admits dispatched runs |
| `runtime/scheduler_service.py` | trigger scheduling and retention sweeps |
| `runtime/supervisor_service.py` | lease assessment and reconciliation, advisory |
| `api/__main__.py` | HTTP API (also the web console's backend) |

Each worker service is a composition root: it builds the capability registry,
the model gateways, the behavior registry, the tool loop and the result sink, then
runs `run_worker_loop`. `worker_loop` decides what to do next; `execute_claimed_once`
executes exactly one claimed node. A failed heartbeat fails the node and releases
the lease rather than stranding it, and a cancelled iteration is logged with its
traceback.

The runtime worker follows a strict two-phase boundary: claim a ready node,
invoke a resolved Agent capability, then hand the response to an atomic result
sink. A model response alone never advances a NodeRun. If the process dies
before the sink commits, the lease remains subject to reconciliation rather than
being silently retried.

## State module layout

`RelationalStateStore` is a composition facade over cohesive mixins; there is
still exactly one transaction core, so atomicity is unchanged:

```text
state/base.py         connections, BEGIN IMMEDIATE/advisory locks, append-only
                      events, heartbeats, decode/values helpers
state/graphs.py       drafts, immutable versions, triggers, admission/outbox/inbox
state/execution.py    node runs, leases/claims, verification, edge decisions
state/checkpoints.py  shared completion/failure/retry tail and human waits
state/operations.py   tool operation ledger
state/progress.py     durable progress observations and diagnostics
state/prepared.py     the content-commit prepared window
state/projects.py     read-only content sources (a project is not a workspace)
state/workspaces.py   workspaces and their operation ledger
state/storage.py      storage report and budget queries
state/retention.py    retention audit records
state/schema.py       the SQLAlchemy schema
state/protocols.py    the interfaces the kernel depends on
state/relational.py   the public store facade
```

`domain/` never imports `state/` or `runtime/`; `state/` never imports
`runtime/`. Pure propagation logic lives in `domain/propagation.py`.

## Domain policy boundary

The kernel (worker, control worker, tool loop) knows only interfaces:

- `runtime/behaviors.py` defines `NodeBehavior` (preflight, output validation,
deterministic control execution) and a reference registry.
- Domain plugins such as `runtime/academic.py` implement and register behaviors
at the composition root (`worker_service`, `control_service`, API validation).
- Tool evidence shaping (JSON evidence, excerpt limits, preload priority) is
declared per `ToolCapability`, not hardcoded per tool name.

This keeps academic policy a plugin: the generic worker no longer imports a
domain module or branches on a role string.

Supporting modules that keep one policy in one place:

| Module | Owns |
|---|---|
| `runtime/evidence.py` | source verification: id in the search result, reading matches the same document. The coverage gate and the review validator both call it, so admission and citation cannot disagree. |
| `runtime/json_output.py` | extracting the JSON object a model meant to return, tolerating a prose prefix or a code fence |
| `runtime/academic_rounds.py` | the research campaign: merge evidence rounds, drop unverifiable sources, decide saturation |
| `runtime/join_merge.py` | `anchor.join_merge`, the join behavior for parallel branches |
| `runtime/sandbox.py` | bubblewrap as the only sandbox backend; absent means `workspace.exec` is disabled |
| `api/routes_usage.py` | token spend, separating gross prompt tokens from the cached share |

## Model calls and cost

A model call goes through `runtime/model_gateway.py`, which owns the wire
protocol, the output budget (`max_tokens` per model profile) and usage capture.
An agent with tools runs through `runtime/agent_tools.py`, which binds model tool
calls to the ledger-backed gateway and bounds what a tool result contributes to
the context.

Two properties matter operationally:

- **A tool loop re-sends its whole conversation on every call.** Cost is
  therefore turns times context, not bytes read. Batch tools
  (`scholarly.read_many`) exist to keep the turn count down, and evidence
  shaping (`model_excerpt_chars`, `excerpt_list_length`) bounds what each result
  adds.
- **Most re-sent tokens are a cached prefix.** `ModelResponse` records input,
  output and cache-read tokens, the worker emits a `model.usage` event per call,
  and `GET /api/runs/{id}/usage` reports gross, cached and billed input per node.
  A gross counter alone overstates the bill several-fold.

A failed call is classified by `FailureClass`: only transient HTTP and network
faults are retried, at the durable node layer, with the attempt and its released
lease left in run history.

## The academic workflow graph

The reference graph is `examples/graphs/academic-research.json`, installed by
`scripts/academic_research.py`:

```text
start -> plan -> coverage -> gather -+-> write -> review -> check -+-> report
                   ↑                 |                            |
                   +-- continue -----+                     revise/major
                                                                 |
                            check -+-> plan (evidence gap) ------+
                                   +-> needs_input (blocked)
```

- `coverage` is the campaign gate (ADR-042): it merges rounds, drops
  unverifiable sources, and ends research on saturation or diminishing returns.
- `check` routes on the reviewer's `target`: `manuscript` returns to the writer,
  `evidence` returns to planning, `blocked` parks the run for a human.
- The writer's contract is the converged survey skeleton plus deterministic craft
  gates (ADR-040, ADR-041); approval follows the reviewer's own bar (ADR-043).

## Content plane

Anchor has a second plane beside canonical state: a durable, versioned,
executable workspace. `WORKSPACE.md` defines the boundary. The model is not "two
sources of truth" but a **Recovery Closure**: the control event history decides
which revisions belong to a run, the content store owns their bytes, and the two
are linked by an immutable `content_ref` (`domain/content.py`,
`runtime/content.py`).

Implemented:

- `runtime/workspace.py` — the Git backend: `cat-file`/`ls-tree` reads, worktree
  fork, absolute paths verified against their repository
- `runtime/workspaces.py` — workspace lifecycle, the single-writer claim, fork,
  and `require_clean` merge
- `runtime/workspace_tools.py` — the native `workspace.read/write/list/exec` tools
- `runtime/sandbox.py` — read-only, network-free execution under bubblewrap
- `runtime/content_commit.py` + `state/prepared.py` — the prepare/freeze/commit
  protocol and its crash windows

A graph *declares* a workspace contract; a run *instantiates* an isolated
worktree; each node consumes declared input revisions and produces its own output
revision. Concurrency safety comes from immutable inputs plus explicit merge, so a
workspace write lock is a resource control, not a correctness mechanism.
Supporting documents: `CONTENT_COMMIT_PROTOCOL.md`, `WORKSPACE_STORAGE.md`.

## Build-versus-assemble boundary

```text
Anchor owns:
  Graph IR / versioning / validation
  Canonical State / event invariants / idempotency
  Context and memory policy
  Domain Harness / verifiers / completion gates / evals
  Capability registry and worker result checkpoint contract
  Product Graph Builder / Run Console

Reuse through adapters:
  PydanticAI       Agent/model primitives
  Prefect/Temporal workflow execution
  PostgreSQL        durable relational storage
  MinIO/S3          artifacts
  MCP/A2A           capability and agent protocols
  React Flow        graph canvas
  dagre             layered auto-layout for unsaved graphs
  assistant-ui      chat and streaming primitives
  OpenTelemetry     telemetry protocol and backends
```

An OSS component is accepted only after a capability and failure-mode test. “Popular”
is a discovery signal, not a reliability or license guarantee. The dependency list
must remain small enough to operate on one host in the standalone deployment.

Langflow and n8n can be useful reference products or optional interoperability
targets, but they are not the Anchor source of truth. Embedding either one as the
primary runtime would introduce a second graph model, state store, permission model,
and version lifecycle. That would make recovery and audit semantics ambiguous.

## Product graph lifecycle

```text
Graph Draft
  -> Static validation
  -> Eval / dry run
  -> Immutable Graph Version
  -> Trigger binding
  -> Task / Run
  -> Node checkpoints and events
  -> Verification / approval
  -> Artifact and completion
```

The frontend edits a declarative Graph IR. It never edits a live workflow run and
never generates arbitrary runtime code as the product's primary representation.

## Run safety model

The target runtime observes liveness independently of model/tool step completion.
A slow step must not look like a dead worker. Progress assessment is separate:
unchanged observations are inconclusive and do not stop execution. Known waits are
normal; repeated completed cycles request diagnosis. Recovery acts only after
checking leases and reconciling possible external side effects.

The current watchdog is advisory and rule-based, with no worker integration or
automatic recovery. A production adaptive detector requires history, calibration,
and tests for both false alarms and missed failures.

Numeric limits are optional policy overrides and emergency controls. They are not
required fields in Graph IR and are not the normal way users prevent loops.

## Source of truth

PostgreSQL is the production canonical store. It owns Mission, Task, Run, Decision,
Approval, Artifact metadata, Memory metadata, and the append-only event stream.
Prefect (when enabled) owns workflow operational state only. A model transcript is
never the only recovery source.

## Recovery contract

Every externally visible transition must happen in one database transaction:

```text
validate expected revision
append event with next stream sequence
update canonical projection
insert idempotency/operation record
insert transactional outbox record
commit
```

An outbox dispatcher may then notify a workflow engine. This prevents a committed
business transition from being lost when the process dies before publishing an
event.

## v0.1 non-goals

- no custom workflow engine;
- no vector database implementation;
- no marketplace or automatic self-improvement;
- no full-mesh multi-agent protocol;
- no claim of exactly-once external side effects.
