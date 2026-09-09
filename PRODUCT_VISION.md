# Anchor Product North Star

## Primary application

Anchor is a user-friendly web product for defining, publishing, and operating
long-running multi-agent collaboration graphs.

An operator or domain engineer can compose a graph visually, configure each agent,
tool, policy, verifier, approval, and trigger, publish an immutable version, and
observe or resume its runs over hours, days, or months.

The graph is not a drawing layer over an opaque framework. It is a versioned,
typed, auditable execution plan whose business state remains recoverable when a
model, tool, worker, API, or UI connection fails.

Anchor is intentionally an assembly product. It uses mature open-source components
for generic infrastructure and concentrates its own implementation on the graph
contract, durable state semantics, context/memory policy, quality gates, and product
experience. Reusing a component is acceptable only when its failure, upgrade, and
license behavior are explicit.

## Two first-class operators: a human and an agent

The graph kernel is operated through two equivalent surfaces over the same
durable state, guards and audit trail:

- the **web console**, where a domain engineer composes, publishes, observes and
  intervenes visually;
- the **agent surface** (`anchor` CLI and an MCP server), where an agent
  authoring and running graphs on the user's behalf gets the same capabilities
  and the same limits.

The agent surface is a client of the authenticated API, never a privileged
shortcut: it cannot bypass an approval gate, steal a lease, or write evidence
directly. Human-only decisions stay human-only by default, because an approval
gate the agent can open itself is not a gate. See `AGENT_SURFACE.md`.

## Product promise

For every published graph, Anchor must make it possible to answer:

- What is this graph supposed to accomplish?
- Which exact graph, agent, skill, model, tool, and policy versions ran?
- What state and evidence did each node receive?
- Which decisions, memories, and artifacts were produced, and where did they come
  from?
- What happened after a retry, crash, timeout, approval, or duplicate event?
- Why was the run considered complete, and which verifiers passed?
- Can the run be resumed, replayed, audited, or safely cancelled?

## Non-negotiable requirements

- Graph definitions are declarative, validated, versioned, and immutable after
  publication.
- A run is pinned to graph, runtime, policy, model, skill, and tool versions.
- Canonical State and append-only events are the recovery source; context and memory
  are projections with provenance and retention rules.
- Every side effect has an operation id, idempotency semantics, authorization, and an
  audit record.
- Long waits release workers and resume from durable state; UI disconnects do not
  terminate work.
- Completion requires deterministic or domain-specific verification, not a model's
  natural-language claim.
- Normal Agent progress should not be interrupted by guessed numeric limits. Safety
  comes from an adaptive watchdog that detects stalls, disconnected workers,
  repeated side effects, and non-progressing cycles.
- Manual, scheduled, and event triggers all pass through the same idempotent task/run
  creation path.
- The product remains usable without a specific model provider, workflow engine,
  vector database, or observability vendor.

## Acceptance scenarios

The first product-grade vertical slice must support:

```text
Visual graph draft
  -> validate
  -> publish version
  -> manual or cron trigger
  -> multi-agent execution
  -> tool call and artifact
  -> approval pause
  -> worker/API restart
  -> resume
  -> verifier and completion gate
  -> traceable result
```

The same graph must be triggerable by a webhook with duplicate delivery and produce
one logical run. A published graph update must not alter an already-running version.

## Composable graphs

A published, immutable Graph Version is a reusable module. A graph can call
another pinned Graph Version as a single node, so teams can solidify proven
scenario graphs and compose them into larger collaboration systems. Composition
rules:

- Only published versions can be referenced, pinned by `graph_version_id` and
  content hash at the caller's publication time.
- Calls carry explicit input/output mappings; the child run's events, artifacts,
  and verification evidence remain traceable from the parent.
- Static validation rejects self-recursion and reference cycles; failure
  propagation (child failure fails the parent node) is explicit and auditable.
- One graph supports multiple trigger bindings (manual, scheduled, internal
event, webhook) through the same idempotent admission path.

## Modular delivery

A scenario graph must be deliverable as a minimal module, designed either in
the Web workspace or as code, then deployed standalone or embedded into a
business scenario. The portable unit is a Graph Bundle: the pinned Graph
Version plus its trigger bindings, capability requirements
(agent/tool/model/verifier refs), input/output schema, and secret references
(never secret values). Delivery tiers, in order of investment:

1. File bundle: import/export a versioned JSON bundle into any Anchor instance.
2. Single-graph deployment: one graph with a minimal runtime behind
   webhook/event triggers (e.g. one container image per scenario).
3. Embedded SDK: business systems trigger runs and collect artifacts through a
   small client without operating workers. A fully self-contained minimal binary
   runner is a long-term option, not a near-term commitment.

## Runtime kernel

The Agent harness is the highest-priority investment, ahead of new delivery
formats:

- Context integrity: long-running, repetitive work must not cognitively drift.
  Deterministic input snapshots and immutable per-generation context records are
  the foundation; on top of them Anchor needs generation diffing, reference
  integrity checks, and consistency gates instead of guessed numeric budgets.
- Experience distillation: run-scoped memories are raw material, not assets.
  Candidate lessons pass review (verifier or human approval) before promotion
  into versioned, referenceable organizational domain knowledge that can flow
  back into graph and capability configuration. The agent improves through
  practice, with every promotion audited and reversible.
- Sandbox and permissions: an agent in a business scenario must not damage its
environment or read beyond its authority. Required before real side effects:
  a permission-checked ToolGateway boundary, sandboxed execution, filesystem
  and network allowlists, per-agent scopes, and approval gates. Experience
  accumulation without this boundary increases risk rather than value.

## Roadmap phases

```text
P0 Sandbox/ToolGateway permission boundary + durable Approval/HumanTask/Wait
P1 Subgraph composition (pin, validation, recursive execution) + Graph Bundle format
P1 Anti-drift context gates on top of immutable snapshots
P2 Experience promotion loop (review -> organizational knowledge -> write-back)
P2 Single-graph deployment (Docker per scenario) + embedded trigger/artifact SDK
Later Minimal-binary runner, vector retrieval, full production hardening
```
