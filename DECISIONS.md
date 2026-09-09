# Anchor Architecture Decisions

## ADR-000: Product goal is visual long-running multi-agent graphs

The primary acceptance target is a web product that lets users define, publish,
trigger, monitor, approve, recover, and audit multi-agent collaboration graphs over
long time horizons. Backend, persistence, workflow, and UI decisions are evaluated
against this scenario. Chat-only demos and unversioned graph drawings are not
product milestones.

## ADR-000A: Assemble mature OSS; own only the product kernel

Anchor will not reimplement a workflow engine, model gateway, vector database,
telemetry backend, chat primitive, graph canvas, or protocol implementation when a
mature open-source component satisfies the required contract.

Anchor owns the parts that define product behavior and reliability:

- Graph IR, validation, immutable versions, and trigger bindings;
- Canonical State, event ordering, idempotency, outbox, and operation semantics;
- Context and memory policy, provenance, retention, and reset/handoff rules;
- Domain Harness, verifier, completion gate, and eval contracts;
- Adapter contracts, capability negotiation, and the user-facing Graph Builder and
  Run Console.

Everything else is an integration selected by an explicit capability, operational,
license, and recovery test. We prefer a small number of replaceable components over
an abstract wrapper around every dependency.

## ADR-000B: Reuse the graph canvas, own the graph product model

Anchor will use a mature graph editor foundation such as React Flow for rendering,
drag/drop, ports, zoom, selection, and layout integration. Anchor will not embed a
general-purpose product such as Langflow or n8n as its canonical authoring or runtime
system.

The Anchor Graph IR remains the source of truth for node semantics, typed inputs and
outputs, graph validation, immutable versions, triggers, approvals, checkpoints,
and quality gates. External workflow products may be supported later through
explicit import/export adapters, but their internal state and execution records must
not become Anchor business state.

The first editor should support a deliberately small set of reliable node types
(Agent, Tool, Router, Parallel/Join, Verifier, Approval, Wait, and Artifact) rather
than attempting to reproduce every generic automation connector.

The first implemented editor uses React and `@xyflow/react` (MIT) with Vite tooling.
Lucide icons are ISC-licensed. Node/edge semantics are edited as the existing Graph IR;
canvas positions live in a separate draft layout. Publication uses the existing API's
optimistic revision and immutable-version contracts. The development proxy forwards
the browser's Bearer header unchanged and never injects a server-side credential.
See `WEB.md` for authoring capabilities and the explicit execution boundary.

## ADR-001: Canonical state is product-owned

Mission, Task, Run, Decision, Approval, Artifact metadata, Memory metadata, and
their event streams belong to Anchor. Workflow engines and model transcripts are
operational projections, not the business source of truth.

## ADR-002: The first store is SQLite, production is PostgreSQL

SQLite makes the local vertical slice easy to run. The adapter exposes only storage
contracts and keeps the same uniqueness and ordering invariants. PostgreSQL becomes
the production implementation before multiple workers are supported.

## ADR-003: PydanticAI is an adapter, not the domain API

PydanticAI may provide model/tool/output primitives, but domain packages cannot
import its internal types. The adapter must preserve typed output and capability
information instead of converting everything to untyped dictionaries.

## ADR-004: Workflow state is not side-effect state

Prefect may later provide scheduling, pause/resume, retry, and worker control. A
transactional outbox connects it to canonical state. External side effects use an
operation ledger and idempotency key; no framework claim of exactly-once execution is
accepted without a provider-specific proof.

## ADR-005: Context is a versioned projection

Every model call receives a generated ContextPack with a policy version, source
references, budget, and content hash. Context compaction or reset must never mutate
canonical facts silently.

## ADR-000C: Trust forward progress; use adaptive watchdogs

Users must not be required to guess max steps, tokens, cost, or wall-clock values
just to make a graph work. Normal progress is the default success signal. Anchor
therefore treats budgets as optional operator guardrails and uses an adaptive
watchdog for safety.

The watchdog separates worker liveness from task progress. State revisions, new
operation IDs, rewritten hypotheses and unverified artifacts are activity, not proof
of progress. Independently verified changes are stronger evidence. Lack of such
evidence is uncertainty, not permission to stop a run.

Repeated completed cycles with equivalent inputs, results and relevant state trigger
diagnosis, not automatic termination. Declared polling, approval and event waits are
normal. An overdue heartbeat calls for probing worker/lease state, not immediate
lease transfer. Unknown side-effect outcomes require reconciliation before retries.
Context reset and recovery must preserve pending actions and approvals.

Infrastructure heartbeat/request timeouts are distinct from task lifetime limits.
They are transport/worker policies, not graph-authoring parameters. Optional operator
limits remain possible but must be visible and explicitly enabled, not hidden caps.

The current `AdaptiveWatchdog` is an advisory rule-based prototype, not a calibrated
adaptive detector. Its evidence collector, diagnostic execution, durable history,
false-positive evaluation and worker integration are not implemented. Loop exit
expressions currently describe intent; the validator checks topology, not expression
semantics or eventual termination.

## ADR-006: Receiver acceptance is durable queueing, not execution

The transactional outbox delivers a stable `RunDispatch` at least once. The canonical
receiver first stores its complete envelope in `execution_inbox`, verifies the pinned
Task/Run/Graph identities, and idempotently advances the Run to `queued`. Only the
resolved entry node becomes `ready`; all other nodes remain `pending` until dependency
semantics are evaluated by the runtime.

The receiver heartbeat reports infrastructure connectivity only. A worker must later
obtain a lease before Run status becomes `running`, and external side effects require
an operation-ledger record. Neither a receiver heartbeat nor queue acceptance is
evidence of Agent progress or task completion.

Ready-node claims use a worker-generated stable `claim_id`. A lost claim response can
be retried with the same identity and returns the same lease; another worker cannot
reuse it. The claim transaction marks the node and Run `running`, because at this point
a concrete worker has accepted responsibility. Lease heartbeats prove recent worker
contact only. Anchor does not automatically steal an overdue lease: an interrupted
side effect may have succeeded remotely, so reconciliation must occur before retry.

## ADR-007: Side effects require an operation ledger

Every external tool call uses a worker-generated stable `operation_id`. Registration
binds that identity to one active node lease, tool reference, canonical arguments and
SHA-256 request hash. Reusing the ID with changed content fails closed. The worker marks
the operation `running` in canonical state before making the external call, then records
one terminal outcome with a result reference or error code.

`outcome_unknown` is a first-class terminal state, not a spelling of failure. It means
the remote side effect may have happened and requires provider-specific reconciliation.
Anchor does not automatically retry it, even when the worker heartbeat is overdue.
Arguments are retained for recovery and audit, so credentials must be supplied through
a separate secret mechanism and must never be embedded in operation arguments.

## ADR-008: Routing decisions are explicit, versioned evidence

Graph edge conditions use JMESPath, an MIT-licensed, side-effect-free JSON query
language. Publication compiles each condition to reject invalid syntax. Runtime
evaluation receives exactly two roots: `output`, containing the source result parsed
as JSON when possible (otherwise its text), and `inputs`, containing that node's
canonical input snapshot. A condition must return an actual JSON boolean; null,
strings, numbers and collections fail closed instead of using language truthiness.
Arbitrary Python evaluation is forbidden.

Each edge is identified by its index in the immutable pinned Graph Version. Reordering
edges therefore creates a different Graph content hash/version. When a source node
completes, every outgoing edge obtains an immutable per-Run decision containing the
selected flag, reason, condition, evaluator name and exact dependency version,
evaluation-context hash and output evidence reference. Unconditional edges are also
recorded, so a join never infers selection from mutable node state.

A pending node becomes ready only after every incoming edge is decided and at least
one is selected. Every selected predecessor must be completed. If all incoming edges
are decided and none is selected, the node enters the successful `skipped` terminal
state and all its outgoing edges are durably marked not selected; this can cascade
through an unchosen branch. `skipped` is intentionally distinct from `cancelled`:
the former is a normal routing outcome, while the latter means execution was stopped
by an operator or control policy.

Run success requires every NodeRun to be either `completed` or `skipped`. Context
input mappings use only the same persisted selected incoming edges used for readiness.
This prevents data from a rejected branch from leaking into a join or Agent prompt.

## ADR-009: Verification is a persisted verdict, not a model claim

A Verifier node is executed only by the dedicated verifier worker through a typed
claim; Agent and control workers cannot take it, and the generic completion path
cannot finish it without a VerificationRecord. The worker rebuilds the
selected-input context, resolves only selected predecessor artifacts, verifies
their SHA-256 integrity, then evaluates one configured adapter: a deterministic
JMESPath expression that must return an explicit boolean, or a model response
that must be strict JSON `{"verdict": "passed" | "rejected", "reason": ...}`.
Ordinary prose, non-JSON output, or schema violations become an `error` verdict.
A model transport exception writes no verdict at all; the node stays `running`
with its lease for supervisor assessment and explicit operator recovery.

Every terminal verdict persists a VerificationRecord with the claim/run/node
identity, verifier version, adapter version, evidence artifact reference, sorted
de-duplicated artifact hashes, and the context hash of the snapshot committed in
the same transaction. The passed NodeRun output points at the evidence artifact.
Only a persisted `passed` completes the node and opens downstream edges;
`rejected`/`error` fail the node/run/task with all downstream edges closed. The
`verification.decided` event precedes the completion/failure event, and capability
validation at publication rejects unknown `verifier_ref` values.

## ADR-010: Evaluation assembles pydantic-evals; the online gate stays dependency-free

Adapter quality is scored with `pydantic-evals` (`Case`/`Dataset`/`Evaluator`),
not a hand-rolled harness. The deterministic JMESPath rule and the strict-JSON
model verdict rule each run as an evals task over a versioned case matrix, so
adapter changes are scored rather than merely asserted. Experience promotion
will reuse the same primitive: candidate lessons face evaluator review before
becoming organizational knowledge.

The boundary is strict: evals lives behind the `evals` extra for offline
scoring only. The online completion gate (`verifier.py` plus
`VerificationCheckpointSink`) gains no new dependency. The strict-JSON parse
rule exists once as the pure `parse_model_verdict`, shared by the worker and
the evals task, so offline scores and online verdicts cannot drift apart.

## ADR-011: Composition by publish-time materialization

A `subgraph` node pins one immutable Graph Version by ID. At publication the
pinned child is expanded inline with `{site}__` namespaced node IDs, and the
stored version contains only ordinary nodes. This is a deliberate choice over
child-run recursion: recursion needs wait-state machinery (parent waits for
child completion) that does not exist yet, while materialization executes
with the current machinery unchanged — claims, propagation, resolution,
verification, artifacts, leases and the mechanical integrity gate all see
normal nodes. Independent child runs with cross-run linkage remain a future
option once durable wait states land; the pin format already supports it.

Consequences: the child must be published before the parent; republishing the
child never alters an existing parent (the pin is a version ID, resolved
once); per-site provenance plus the authoring definition hash are recorded in
version metadata; mapping paths addressing child interior outputs are
renamed to their namespaced form; references to a subgraph node's own
(nonexistent) direct output fail closed at publication. Only published
versions are referenceable; cycles are rejected defensively (content pinning
already makes them impossible by construction).

## ADR-012: Tool execution assembles sandboxes; Anchor owns the policy

Isolation primitives are never hand-rolled. Commands execute behind a
`SandboxBackend` interface: bubblewrap (unprivileged, no network, read-only
root, private workspace, scrubbed environment) is the default enforcement,
with locked-down OCI, nsjail, gVisor or Firecracker as later options behind
the same interface. A `SubprocessBackend` exists for development only and
must never run untrusted tools.

Anchor owns the policy layer, which is deny by default: the tool must be
registered, the agent's capability scope must include it, side-effect tools
are refused until approval machinery lands, credentials in arguments are
rejected, and results leave the sandbox only as gateway-ferried artifacts.
Every execution goes through the operation ledger (register/start/finish);
replays with the same operation identity return the persisted outcome
instead of re-executing. Timeouts and nonzero exits fail closed; read-only
v1 tools have no external-effect ambiguity to reconcile.

## ADR-013: Waits are explicit durable states with shared propagation

Approval, human-task and event waits park as `waiting_approval` /
`waiting_event` NodeRun states instead of executing. No worker claims a
waiting node, and a waiting node blocks terminal Run completion by
construction (it is neither completed nor skipped). Humans advance approval
gates and event ingress resumes event waits through atomic decide/resume
calls that reuse the lease path's propagation tail (`_propagate_completion`),
so downstream, skipped-cascade and terminal semantics cannot diverge between
executors. Decisions are idempotent per node with conflict-checked replays;
rejection fails the node, run and task without opening any edge. Event waits
optionally match a declared `wait_event` metadata key, with mismatches
rejected as conflicts rather than silently absorbed.

## ADR-014: The model decides what to call; the gateway decides whether it runs

Agent tool use is a loop, not a privilege. The model receives one function
tool per allowed `tool_ref` (via pydantic-ai, assembled — not a hand-rolled
tool protocol); every invocation executes through `ToolGateway.execute` with
a deterministic operation identity derived from `(claim_id, tool_ref, call
sequence)`. Retried claims replay persisted outcomes instead of re-executing.
Denials and failures return as tool messages so the model can adjust; the
run never dies on a refused tool. Agents without `tool_refs`, workers
without a loop, and gateways without tool support all fall back to the
plain single-shot path with zero behavior change.

## ADR-015: Bundles are verified files, not live links

Modular delivery starts as a file: one pinned version plus trigger bindings
and capability references, exported and imported as versioned JSON. Import
recomputes the content hash and fails closed on tampering; triggers are
rebound with fresh IDs to the new version; canvas layout stays
authoring-local and never travels. No secrets cross the boundary — webhook
entries carry references only. Single-graph deployment and embedded SDKs
build on this format later; the format must not change underneath them.

## ADR-016: Experience becomes knowledge only through review

Run memories stay working memory; organizational knowledge is earned, not
accumulated. Candidate lessons are proposed (with domain and provenance),
then explicitly promoted or rejected by a reviewer with a recorded reason.
Only promoted records enter future prompts, tagged by domain; rejected
records never do. Tombstone deletion and audit reads apply unchanged.
Promotion writes nothing back into graphs or capabilities yet — that
write-back loop is a separate, explicitly gated milestone, because silent
self-modification would defeat auditability.

## ADR-017: Loops iterate by re-arming attempts, never by budgets

A loop node is a deterministic conditional branch point that the validator
permits on cycles; it executes in the control worker as pass-through, and
its outgoing conditions evaluate against its own resolved snapshot — so
cycle edges carry input mappings that expose body state. There is no
iteration budget: each return to a completed node starts a new attempt row,
and edge decisions are scoped by source attempt so every iteration
re-decides freshly while earlier iterations stay immutable. A selected edge
to a skipped node revives it; simultaneous mapping conflicts resolve by
decision recency, deterministically. Repeated cycles are watchdog-visible
and never silently terminated, per ADR-000C.

## ADR-018: Tool nodes execute under explicit owners; leases never recover them

A `tool` node runs its tool through the ledger-backed gateway with arguments
from its resolved snapshot, executed by the control worker — no model call
involved. The node must declare `owner_agent` metadata naming the authorizing
agent (checked at publication capability validation); without an owner, or
with arguments or scope violations, the node fails closed loudly instead of
stalling. Claimable-by-control and recoverable-like-control are separate
sets (`CONTROL_NODE_TYPES` vs `RECOVERABLE_CONTROL_TYPES`): an interrupted
tool lease stays `unknown` for reconciliation and can never be recovered
through the lease path, no matter which worker claimed it.

## ADR-019: Execution limits are typed; progress is observed, not inferred

ADR-000C said budgets must not govern healthy work. This ADR makes that
executable and records what actually changed.

Limits are now an explicit taxonomy in `anchor.runtime.execution_policy`:
transport protection, resource capacity, explicit operator policy, and task
behavior. Only the last category can affect whether a task is considered
complete, and even there the default is off. `run_timeout_seconds` and
`max_rounds` are no longer implicit defaults read from graph metadata: an
absent value means unbounded, a present value is a visible operator choice.
`ANCHOR_EXPIRE_RUN_BUDGETS` defaults to false, so the supervisor does not
sweep runs against a hidden clock. JSON output repair (`output_retries`)
defaults to zero and never replays tools or invents evidence. Node
`timeout_seconds` remains a physical per-call guard, not a task lifetime.

Three counters are deliberately independent: business cycles, node attempts
(including transient-failure retries) and request retries. A new business
cycle never consumes fault-recovery budget, and a fault retry never counts
as research progress. Academic review no longer blocks on a round counter;
repeated identical research records a mechanical issue and stays in
`revise` so supervision can diagnose it.

Progress is observed, never inferred from activity. The supervisor and
`AdaptiveWatchdog` collect durable `ProgressEvidence` (state revision, phase,
artifact refs, verifier passes, tool operation ids, cycle fingerprint) and
persist it. Fingerprints use completed-cycle inputs, results and tool
operations, not timestamps or random ids. A repeated complete cycle without
new verified progress creates a deduplicated `DiagnosticRequest`; it never
terminates the run. Absence of evidence is uncertainty. Heartbeats and new
UUIDs prove liveness only.

The watchdog remains advisory. It has no permission to repair, reconcile or
transfer a lease; `DefaultDiagnosisPermission` grants only continue, wait and
escalate. Automatic repair requires an explicit capability grant and a
proven-safe action, which does not exist yet. Consequently this ADR does not
claim a calibrated adaptive detector — it claims durable observation, typed
limits and a diagnosis loop that is auditable from the API and Run Console.

## ADR-020: Transient-failure recovery is a persisted schedule

ADR-019 separated business cycles, node attempts and request retries. This ADR
closes the remaining gap: the recovery plan itself must survive a process
restart. `retry_node_and_propagate` therefore persists two columns on
`node_runs`: `last_error_class` on the failed attempt and `next_attempt_at` on
the fresh attempt. Claim queries only return ready nodes whose
`next_attempt_at` is null or due, so an early claim is impossible and a restart
cannot lose the backoff.

The worker no longer sleeps between attempts. It classifies the fault
(`classify_failure`), honors a provider `Retry-After` within a bounded range,
persists the schedule and releases the lease. The `node.retrying` event records
the class and the due time. Backoff remains opt-in through `max_retries`;
when it is zero the attempt fails and waits for an operator, unchanged.

This is deliberately not automatic repair. Scheduling a retry of a safe,
idempotent read is recovery; reconciling an unknown side effect or repairing
canonical state still requires an explicit operator or a future capability
grant. Retry counts never consume business-cycle budget, and a business cycle
never consumes the retry allowance.

## ADR-021: Domain policy is a registered behavior; state is a composed facade

Two structural rules keep the kernel small as domains are added.

First, the generic worker and control worker know only `NodeBehavior`
(preflight, output validation, deterministic control execution) resolved by
`behavior_ref`. Domain plugins such as the academic workflow implement and
register behaviors at the composition root. The kernel no longer imports a
domain module, branches on a role string, or knows tool names: tool evidence
shaping is declared per `ToolCapability` (`evidence_json`, excerpt limits,
preload priority). Adding a domain means registering a behavior and declaring
capabilities, not editing the worker.

Second, `RelationalStateStore` is a facade over cohesive mixins
(`base/graphs/execution/checkpoints/operations/progress`). This is a file-boundary
change, not a new abstraction layer: one transaction core, one event append, one
lock discipline. Persisted records (`ProgressEvidence`, `DiagnosticRequest`) live
in `domain/models.py`; `domain/` and `state/` never import `runtime/`, and pure
propagation logic lives in `domain/propagation.py`. Duplicated generic claim and
lease-guard code was removed, and dead `checkpoint_node_result` was deleted
rather than kept "for flexibility".

## ADR-022: Canvas layout is layered and edges are routed, not decorated

The graph canvas must stay readable as graphs grow and as runs poll. Three
decisions follow from that.

Auto-layout is assembled, not hand-rolled: graphs without saved drag positions
are laid out with dagre (`@dagrejs/dagre`, MIT) in left-to-right layers, keyed
by topology so polling and metadata edits cannot move the canvas. A user's drag
position is always authoritative. The execution panel lays out for its own node
size rather than reusing builder coordinates.

Edges are routed by direction. Forward edges are bezier curves whose curvature
varies by fan-out order so siblings from one source do not overlap. Backward
edges (target left of the source) are routed below the graph in two orthogonal
segments, borrowing the idea from n8n's `getEdgeRenderData` (n8n is fair-code,
so the algorithm was reimplemented, not copied). Routing state is conveyed by
colour, width and dash; text labels appear only on hover or selection, because
labelling every edge is the largest source of visual noise. A wide invisible
interaction path keeps thin edges clickable.

Controlled React Flow nodes require `onNodesChange` and a stable
`nodeTypes`/`edgeTypes` identity. Rebuilding node objects on every render drops
React Flow's `measured` flag, which makes dragging fail with error #015 and
drops every connection. The canvas therefore keeps nodes in `useNodesState`,
carries `measured` across document sync, skips syncing during an in-flight drag,
and declares edge types as a module constant. This is a rendering contract, not
a cosmetic preference.

## ADR-023: Unknown side effects are reconciled by a human, never retried

A side effect whose outcome cannot be proven must not be retried automatically.
The ledger already recorded `outcome_unknown`; this ADR closes the loop with a
usable operator path and a real producer.

The producer is an explicit side-effect tool (`http.post`). It runs only when
the tool capability declares `side_effect`, the node names an `owner_agent`, and
the node has a completed `approval`/`human_task` predecessor. The gateway still
refuses side-effect tools by default, so an agent tool loop can never obtain one.
Timeouts and transport failures after the request was sent become
`outcome_unknown` with `transport_unknown`; a definite HTTP error stays `failed`.

The node stays `running` with its lease. The operator reconciles the operation
with external evidence (`POST /api/operations/{id}/reconcile`), and the ledger
then deterministically resolves the node: a reconciled success completes it with
the recorded result artifact, a reconciled failure fails it. Neither path creates
a second attempt row, so the side effect cannot be silently repeated. The tool
lease remains unrecoverable through the generic lease-recovery path.

Run control is explicit and reversible: `pause` stops accepting new claims while
in-flight nodes finish and their downstream stays ready; `resume` returns the run
to running. `stop` remains the terminal operator action. Triggers are authored in
the Web UI against one immutable published version, and webhook triggers carry a
secret reference, never a secret value.

## ADR-024: History is curated, never silently destroyed

Operators need a clean run list and a bounded disk, but runs are the root of the
evidence chain. Two separate, deliberately simple mechanisms cover that:

- **Archive** is a reversible `archived_at` flag on terminal runs. The default
  `GET /api/runs` hides archived runs; `include_archived=true`, `GET
  /api/runs/{id}` and every child collection keep working. Only terminal runs can
  be archived so an active lease can never be hidden from supervision. The
  change appends a `run.archived`/`run.unarchived` event.
- **Storage budgets** are two adjustable monitoring targets stored in the
  database (`storage_budgets`): one for the whole install (database file plus
  artifact directory) and one per graph (attributed artifact bytes). They are
  retunable at runtime through `PUT /api/storage/budget` with no restart.

A budget is never an execution budget: it cannot terminate a running node, and
nothing is deleted automatically. `GET /api/storage` reports the real footprint,
including shared artifact bytes separately, because artifacts are content
addressed across runs and graphs. Automatic eviction, garbage collection and
retention windows remain deliberate future work; when they arrive they must be
policy-driven, audited, and never destroy a run that is still in flight.
