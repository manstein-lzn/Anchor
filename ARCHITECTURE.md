# Anchor Architecture

## Product-first constraint

The final product is a visual graph authoring and operations experience for
long-running multi-agent work. Backend abstractions are only useful if they support
that workflow without sacrificing recoverability, auditability, quality gates, or
vendor neutrality. A component that is easy to demo but cannot preserve graph
versions, node state, or side-effect semantics is not an acceptable shortcut.

## Runtime layers

```text
Transport/API
    -> WorkflowService
        -> GeneralHarness
            -> ModelClient / ToolGateway
        -> Canonical State + EventStore
```

The model, workflow engine, tool transport, and UI are replaceable. The domain
contracts are not allowed to import their implementation types.

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

## Content plane (design proposal)

Anchor today has one source of truth: Canonical State and its projections, where
node output is an immutable artifact blob. Supporting code-development work
requires a second plane — a durable, versioned, executable workspace — without
losing replayability or audit.

`WORKSPACE.md` defines the proposed two-plane boundary: the control plane owns
"what happened" (events, leases, decisions, operations), the content plane owns
"what exists" (git-versioned workspace, immutable artifacts), and the two are
linked by a pinned `content_ref`. That document is a design proposal awaiting the
external research in `docs/AGENT_ARCHITECTURE_RESEARCH_BRIEF.md`; no
implementation starts before the boundary is agreed.

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
