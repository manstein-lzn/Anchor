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

## ADR-025: Rolling retention evicts finished history under a storage budget

Storage budgets are enforced by a rolling sweep, not by a manual button: the
scheduler periodically evicts the oldest finished runs until the install is
under its budgets, so disk use stays bounded without operator attention.

The sweep is deliberately narrow. It only ever touches terminal runs, never
terminates a running node, and skips a protected set: non-terminal runs, runs
holding an active lease, runs waiting on an approval/event, and runs with an
unresolved `outcome_unknown` operation. Candidates are ordered oldest-first.

Artifacts are content addressed and shared, so reclaiming disk is a
mark-and-sweep pass over surviving references (including references embedded in
context-snapshot JSON), never a per-run file delete. Rows are deleted
child-first in one transaction, then the database is vacuumed (SQLite needs it
to shrink the file; PostgreSQL reclaims internally). Every sweep writes a
`retention_audit` row with counts and freed bytes and stores no run content.
With no budget configured the sweep is a no-op, so retention can never surprise
an install that has not opted in. `ANCHOR_STORAGE_ENFORCE=false` turns
enforcement off entirely, leaving budgets advisory.

## ADR-026: A content plane, joined to the control plane by a recovery closure

Anchor needs a durable, versioned, executable workspace to support code work. The
previous absence was not an oversight of tools but a modelling gap: ADR-012's
"private workspace" is the ephemeral per-tool-call sandbox directory, so the
durable shared workspace was never a first-class entity. Sandbox (a security
boundary) and workspace (a persistence entity) must be modelled separately.

The architecture adds a content plane without making it a second source of truth.
The model is a **Recovery Closure**: the immutable control event history decides
which revisions belong to a run; the content store owns their bytes; the two are
linked by an immutable `content_ref` (`artifact://sha256/...` or
`workspace://<id>@<immutable-revision>/<path>`). A referenced revision that
cannot be resolved or verified fails the run closed. Mutable workspaces, sandbox
state, caches, memory, vector indexes and summaries stay non-canonical
projections. I2 is rewritten accordingly and I9 is split into R0-R3 replay levels.

Decisions that were previously open and are now settled:

- **Git-first for code, CAS-backed for generic trees**, unified by one
  `WorkspaceRevision` abstraction with an Anchor-computed `tree_digest` so the
  semantics do not depend on git's object format.
- **A graph declares the workspace contract; a run instantiates an isolated
  worktree.** A graph never owns a mutable workspace, so concurrent runs of the
  same graph cannot collide.
- **Node-level revision lineage**: each node consumes declared input revisions
  and produces its own output revision. Concurrency safety comes from immutable
  inputs plus explicit merge, so a workspace write lock is resource control, not a
  correctness mechanism.
- **`merge_policy: require_clean`** for v1: a merge conflict fails closed and
  escalates to a human task with both revisions as evidence.
- **Verification runs against the frozen revision** (read-only workspace, separate
  scratch); a rejected revision stays referenced by the failure event and is not
  garbage-collected.
- **Revision content set is the `git add -A` tree**; ignored dependencies and
  build outputs live in a shared cache, and evidence-producing outputs go to
  artifacts.

Commit ordering across the two stores uses prepare/freeze/commit/reconcile with
idempotency keys, because the event store and content store cannot share a
transaction. See `WORKSPACE.md`, `CONTENT_COMMIT_PROTOCOL.md` and
`WORKSPACE_STORAGE.md`.

Evidence: `docs/deep-research-report.md` (external research). Its conclusions
agree with this decision; its citations were delivered as internal markers
without URLs, so it is treated as directional evidence, not as a verifiable
source, until an appendix with URLs is supplied.

## ADR-027: A content reference is an immutable boundary type

The control plane records which content a node consumed and produced; the content
plane owns the bytes. `domain/content.py` is the single place where those two
meet: `ContentRef` parses, validates and serializes both reference kinds
(`artifact://sha256/<digest>` and `workspace://<id>@<immutable-revision>[/<path>]`),
and no other module may parse the prefixes (enforced by `tests/test_architecture.py`).

Two properties are enforced at parse time rather than at read time:

- **Immutability.** A workspace revision must not be a mutable name. `main`,
  `HEAD`, `latest`, `refs/...` and any branch-style ref are refused, because a
  reference that can move later makes replay depend on the environment rather
  than on the recorded history. This is what keeps I2 and I9 honest.
- **Fail closed.** `runtime/content.py` resolves references through injected
  resolvers keyed by content kind. An unregistered kind raises
  `ContentUnavailable` instead of guessing, and a missing artifact raises rather
  than returning empty text. There is no fallback to a live workspace.

`ContentRefError` is deliberately not a `ValueError`: pydantic converts a
`ValueError` raised inside a validator into a generic `ValidationError`, which
would hide the precise reason a reference was refused.

This ADR defines the boundary only. It does not add a workspace backend, a
migration, or an API endpoint; those are W1+. The artifact side of the resolver
is implemented and tested so the fail-closed path is real, not aspirational.

## ADR-028: A project is a read-only content source, never a workspace

Registering a project grants the content plane read access to a repository at
immutable revisions. It does not grant a worktree: a workspace is run-scoped and
is W1 work. Keeping the two apart means read access can ship before write access
without any path that could mutate a user's repository.

- `POST /api/projects` validates the root with `git rev-parse` and refuses a
  directory that is not a git repository, so a bad registration fails at the
  boundary instead of at first read.
- `GitWorkspaceBackend` reads blobs with `git cat-file` and lists with
  `git ls-tree`. It never checks out, never runs hooks and never writes to the
  repository.
- Only full hexadecimal commit ids are accepted. The `ContentRef` boundary
  already refuses mutable names; the backend repeats the check as defence in
  depth and verifies the object is a commit.
- Reading a tree without a path, a directory, a missing blob, a non-UTF-8 file
  and a file over the size limit all raise. Nothing falls back to a working tree.
- The default revision on a project is a convenience for operators and is never
  recorded in a content reference.

The store mixins now declare the members the composition root supplies through
`_StoreHost`, which removed 140 pre-existing mypy errors and keeps new mixins
type-checked.

## ADR-029: Workspace execution is a read-only observation

Running a command at a revision must not be able to change it, or "run the
tests at the revision I verified" would stop being repeatable. Workspace
execution therefore materializes the pinned tree, mounts it read-only, removes
the network and runs an allowlisted command with a timeout and an output cap.

- `GitWorkspaceBackend.materialize` uses `git archive` into a temporary
  directory. It reads the object database and never touches the working tree,
  the index or the config, so materializing cannot mutate the source repository.
- `runtime/sandbox.py` defines the `WorkspaceSandbox` contract with a
  bubblewrap implementation (no network via `--unshare-all`, read-only workspace
  bind, scrubbed environment) and a clearly labelled subprocess fallback for
  development. Writes fail with a read-only filesystem error rather than being
  silently discarded.
- Commands are an explicit allowlist checked before execution, never a shell
  string. The default set is read-only inspection; a caller may widen it, and
  the sandbox still has no network and no write access.
- `execute_in_workspace` resolves the project, materializes, runs and removes the
  temporary tree. Nothing about the run survives in the repository.

This is deliberately narrower than the W1 write path: it proves the
`SandboxProvider` boundary with zero mutation risk before any workspace can be
written.

## ADR-030: Writable workspaces are run-scoped worktrees with a full ledger

A workspace is where work happens, never a source of truth. Only the revisions it
produces, once a control event references them, enter the recovery closure (I2).
The workspace record therefore tracks a lifecycle and a lineage, not the bytes.

- `workspaces` and `workspace_operations` (0019) hold the lifecycle and the
  audit trail. A workspace is `active`, `frozen` or `archived`; only `active`
  accepts writes.
- `WorkspaceManager` is the only write path. `create` forks a git worktree on a
  dedicated `anchor/<workspace_id>` branch; `write_text` and `delete` commit
  immediately and record an operation plus a `workspace.*` event in one
  transaction; `freeze` commits any remaining change and pins
  `current_revision`; `archive` removes the worktree while keeping the branch so
  the revisions stay reachable.
- Paths are validated against traversal and symlink escape, and content has a
  size cap. A frozen or archived workspace refuses writes.
- The ledger row and its event are written atomically, so a mutation cannot be
  half-audited. If the process dies between the git commit and the ledger write,
  the revision is an orphan that the W1.2 reconciler will resolve; the workspace
  revision is never guessed from the live tree.
- Creating a worktree adds a branch to the source repository but never touches
  its working tree.

The content-plane routes moved to `api/routes_content.py` to keep the
composition root inside its module budget, and were split into project and
workspace registrars to stay inside the complexity budget.

## ADR-031: The content commit is a protocol with an explicit prepared window

The content store and the control store cannot share a transaction, so a content
commit is a sequence of idempotent steps with a recorded window between them:
freeze the bytes, record the prepared revision, run the verifier, commit the
control event, then clear the marker. `prepared_revisions` (0020) makes that
window durable instead of in-process.

`ContentCommitter` owns the protocol:

- `prepare` freezes the workspace and records `(node_run_id, attempt, revision,
  manifest_digest)`; it is idempotent, so a retry cannot overwrite the revision a
  reconciler is already working from.
- `record_verification` persists the verdict into the prepared record, so the
  reconciler can finish the commit without re-running a non-deterministic
  verifier.
- `commit` runs the caller's idempotent control commit and clears the marker.
- `reconcile` converges every crash window: already committed -> clear; verified
  -> commit; not yet verified -> verify then commit; content unavailable ->
  report `inconsistent` and change nothing. A revision whose bytes vanished is
  never guessed from a live workspace (I2).

The manifest digest is Anchor-computed over the canonical `(path, mode, blob id)`
listing, so two materializations of a revision agree. Hashing blob contents
rather than git object ids (to remove the object-format dependency) is a later
refinement and is recorded as such in `WORKSPACE.md`.

Fault injection for all five windows runs in `tests/test_content_commit.py`; the
two invariants it pins are that a control commit never runs twice and an
unavailable revision is never committed.

## ADR-032: Workspace tools are native, not gateway tools

An agent needs to read and write the workspace it is bound to. Routing those
operations through the sandbox gateway would either couple the gateway to the
workspace manager or bypass the workspace ledger, so they are *native* tools:
the kernel executes them and the workspace manager commits and audits every
mutation.

- `WorkspaceToolset` provides `workspace.read`, `workspace.write`,
  `workspace.list` and `workspace.exec`. A node binds a workspace through
  `metadata.workspace_id`; a node without one cannot use the tools.
- `AgentToolLoop` consults native handlers before the gateway. Gateway tools are
  external commands recorded in the tool-operation ledger; native tools are
  in-process mutations recorded in the workspace ledger. The two ledgers stay
  separate on purpose, and a workspace write never appears as a tool operation.
- `workspace.exec` materializes the current revision and runs an allowlisted
  command read-only (ADR-029), so inspection cannot mutate the workspace.

This is the agent-visible capability. Committing a node's *output* as a workspace
revision through the prepared/commit protocol, and reconciling that window in
the supervisor, is the next slice; the tools already produce audited revisions
today.

## ADR-033: A workspace-bound node's output is its revision

A node that declares `metadata.workspace_id` does not publish the model's text as
its output. Its output is the immutable workspace revision the node produced, so
downstream nodes consume a pinned tree rather than prose about one.

- After the model response, the worker runs the prepared/commit protocol:
  `prepare` freezes the workspace and records the revision, the output-format
  check is recorded as the verification result, the node completes with
  `output_ref = workspace://<workspace_id>@<revision>`, and only then is the
  prepared marker cleared.
- The model's text is still persisted as an artifact and referenced from the
  `node.completed` event payload (`response_ref`), so nothing is lost. Artifact
  GC now scans event payloads, otherwise that artifact would be collected as an
  orphan.
- `complete_node_and_propagate` accepts an `event_payload` merge, so extra
  content references are recorded without inventing a second event.
- The supervisor runs a conservative reconciliation pass every cycle: a prepared
  marker is cleared once its node is terminal, an unavailable revision is logged
  as `inconsistent` and left untouched, and a pending node's marker is left for
  that node's own recovery. The supervisor never completes a node on the
  worker's behalf.

A node without a declared workspace keeps the previous behaviour: the model text
artifact is the output.

## ADR-034: bubblewrap is the only sandbox backend; missing means disabled

Isolation is a security boundary, not a feature to grow. bubblewrap is the
smallest stable enforcement available: one unprivileged binary, no daemon, no
container runtime to operate. Anchor therefore keeps exactly one backend and
does not add containers, gVisor or microVMs until a concrete threat requires it.

The important consequence is what happens when bubblewrap is absent. Previously
`worker_service` fell back to an unisolated subprocess with a warning, which is a
silent security downgrade. Now `workspace.exec` is disabled and reading/writing
continues; the fallback backend remains in the tree labelled TEST ONLY and is
never selected by production code. A missing sandbox must reduce capability, not
silently reduce safety.

This mirrors ADR-029: the sandbox is read-only and has no network, so `exec` is
inspection rather than mutation.

The allowlist includes `python3` with `PYTHONDONTWRITEBYTECODE=1`. An interpreter
in a read-only, network-less sandbox can compute over the pinned revision but
cannot mutate it or exfiltrate data, and running the code under test is what
makes workspace validation behavioural rather than string matching.

## ADR-035: Workspace paths are absolute and verified against their repository

Real-model validation found a severe defect: `WorkspaceManager` accepted a
relative `root`, stored a relative worktree path, and `GitWorktree` ran
`git -C <relative path>`. Because git resolves a relative `-C` against the
process working directory and then walks up to find `.git`, the workspace write
landed inside the Anchor repository itself; `git add -A` added nothing (the path
was gitignored), so `commit()` silently returned the Anchor HEAD as the
"revision". The failure surfaced later as `could not get object info` when the
manifest digest was computed against the intended project repository.

Three fixes, each closing one link in the chain:

- **Absolute paths.** `WorkspaceManager` resolves its root, `GitWorkspaceBackend`
  resolves the project root, and `validate_project_root` returns the resolved
  path so the API stores an absolute root. A relative path resolves differently
  in every process and cannot be persisted safely.
- **Worktree verification.** `GitWorktree.verify()` requires
  `rev-parse --show-toplevel` to equal the worktree path and
  `rev-parse --git-common-dir` to equal the project's common dir. It runs after
  `worktree add` and before every commit, so a path that resolves into another
  repository fails loudly instead of committing into unrelated history.
- **Regression tests** cover a relative root, a path inside a foreign repository,
  and absolute-root project registration.

No damage occurred: the stray writes were under a gitignored directory and the
repository stayed clean. The lesson is that a filesystem-backed content plane
must treat paths as untrusted input and prove ownership before mutating.

## ADR-036: A node's declared input records the workspace revision it consumes

B1 in `WORKSPACE.md` requires the control plane to prove which revision a node
saw. The lineage validation found that it could not: both nodes' declared input
snapshots were `{"inputs": {}}`, and the only revision on record was the node's
*output*. A node that read revision R and produced revision R' would leave no
evidence of what it read.

`resolve_node_context` now adds `workspace: workspace://<id>@<revision>` to the
snapshot of any node that declares `metadata.workspace_id`, using the workspace's
current revision at resolution time. Because the snapshot is hashed, the consumed
revision becomes part of the node's reproducibility record.

This is the recording half of B1. Enforcing that the tools cannot read a
different revision than the one recorded requires the concurrency work (W3): with
one writer per workspace the window is currently closed by construction, but the
snapshot is what makes a violation detectable.

## ADR-037: One writer per workspace; reads see the declared revision plus own writes

Concurrency is enforced by isolation plus a single-writer claim, not by locking
the whole run. Three rules, each with a test:

- **One writer.** A workspace records the node that owns its write slot. The
  first write claims it with `expected_revision`, which must equal the revision
  the node declared in its input snapshot; if the workspace moved in between,
  the claim is refused because committing would silently mix lineages. A
  different node's write fails closed. `freeze` releases the claim.
- **Pinned reads.** A node reads the revision recorded in its input snapshot, so
  another node's in-flight writes cannot change what it observes (I2/B1). Once
  the node owns the claim, its reads follow its own lineage, because otherwise it
  could not read back a file it just wrote.
- **Failures are messages, not crashes.** A workspace tool failure
  (`WorkspaceError`, `ContentUnavailable`, `SandboxDenied`) is returned to the
  model as `TOOL FAILED [...]`, so it can adapt — for example write a file
  before reading it back. Previously such an exception escaped the tool loop and
  left the node running with a live lease.

Two structural fixes came out of the same investigation:

- `_workspace_output` and the result sink now run inside the failure handling
  that fails the node. A content or completion error used to escape
  `execute_claimed_once` entirely, leaving the node `running` and the lease
  stale — the worst state, because supervision sees a live node that will never
  finish.
- The validation scripts stop the run before deleting their throwaway
  repository. Deleting it while a run was still executing destroyed the
  workspace worktree's git directory and produced a confusing failure.

Parallel *writing* nodes still need separate workspaces; the fork/merge model
remains W3.2.

## ADR-038: Parallel branches fork explicitly; a join merges under require_clean

Concurrency is expressed as data, not as a lock. A graph author forks a
workspace for each parallel writer and names a join workspace:

- `WorkspaceManager.fork` creates an independent worktree from a source
  workspace's revision and records a `fork` operation. Two writers never share a
  tree, so the single-writer claim is never contended.
- `WorkspaceManager.merge` merges an immutable revision into a workspace with
  `require_clean` as the only policy. A conflict aborts the merge
  (`git merge --abort`) and raises, leaving the target at its previous revision;
  choosing a side silently would lose work. The merge itself is a ledger entry
  and a commit.
- `anchor.join_merge` is a core behavior registered by the composition root. It
  takes the target from the node's own declared input (`snapshot["workspace"]`),
  collects every branch workspace revision in the snapshot, and merges each. A
  stable `uuid5(run_id, "join:<node>")` gives the merge a write claimant without
  changing the behavior protocol.

The parallel validation found one more gap: a **control** node that mutates a
workspace did not publish its revision, so a join's output was a JSON artifact
while its workspace had moved. `ControlNodeWorker` now uses the same
prepared/commit protocol as the agent worker: a control node that declares
`metadata.workspace_id` freezes the workspace, records the behavior result as an
artifact in the completion event, and completes with the workspace revision as
its output reference.

Parallel *automatic* forking (a `parallel` behavior that creates branches) is
deliberately not implemented: explicit forks keep the topology, the workspaces
and the merge point visible in the graph.

## ADR-039: The MCP server is an adapter, not a backdoor

`anchor-mcp` exposes the operation surface to agents over stdio JSON-RPC. It is
a thin adapter over `AnchorClient`, so every call goes through the authenticated
HTTP API, the same guards and the same audit trail. It has no database access,
no third-party dependency, and no privileged path.

- One tool per client operation, each with a **closed** JSON Schema
  (`additionalProperties: false`), so an agent cannot invent fields.
- Human-only operations (`approve_wait`, `reject_wait`, `resume_wait`) and
  operator-only operations (`retention_sweep`, `set_budget`) are refused with
  `error: "human_only_operation"` unless the operator starts the server with
  `ANCHOR_MCP_AGENT_CAN_APPROVE=1`. An approval an agent can grant is not an
  approval.
- Every failure becomes an `isError: true` result carrying the stable code, HTTP
  status, path and `retryable`, so an agent branches on data, not prose.
- `AnchorClient` now resolves its base URL from `ANCHOR_API_URL` so an MCP client
  configures the server through the environment.
- Transport is newline-delimited JSON-RPC (`initialize`, `tools/list`,
  `tools/call`, `ping`). The official SDK was declined: the needed protocol
  surface is small and stable, and the project keeps its dependency set minimal.

## ADR-040: The paper is for the reader; verification is a pipeline property

The first deep-research run produced a defensible audit: 57% of body paragraphs
carried audit language, 116 evidence-level tags, 16 process references, and
35.5% of the document was apparatus (references plus a hash appendix). It was
not a paper. The system had executed its contract exactly — the contract was
wrong.

A paper format is a Pareto-frontier communication device, not an audit trail.
Verification belongs in the pipeline, where it is automatic, not in the writer's
attention, where it displaces the work that only a writer can do.

- **Split the roles.** `gather` (tools) produces a structured evidence ledger
  and no prose. `write` (no tools) produces the reader-facing manuscript from
  that ledger and cites by number. `review` judges substance *and* craft and
  names a `target` (`evidence` | `manuscript`), so the loop redoes only what is
  actually deficient. `check` routes on that target.
- **Make "reader-facing" judgeable.** Deterministic craft checks reject
  evidence-level tags, process language, content hashes, paragraphs over 1200
  characters, and Methods over 2500 characters in the body. A rule that cannot
  be checked does not exist.
- **Keep provenance, move it backstage.** References and the retrieval-evidence
  appendix are generated from verified sources, as before. The writer never
  narrates them.

Result on the same topic: 7.2 minutes instead of 25.8, 38 KB instead of 91 KB,
three rounds instead of five, `gather` ran once instead of three times, and zero
audit terms in the body.

## ADR-041: The section skeleton is measured, not invented

The previous section list (Abstract, Introduction, Methods, Literature Review,
Discussion, Limitations, Conclusion) was assembled by hand. It is an IMRaD tail
bolted onto a survey body, and it matches no real paper: "Literature Review" is
the whole paper, "Methods" is ambiguous, and "Limitations" is not where survey
papers put validity.

The skeleton is now taken from published practice instead of invented. Measured
by fetching the papers and reading their headings:

- `1801.04405` (ACM Computing Surveys, compiler autotuning): Introduction →
  domain background → thematic axes (characterization, models, prediction types,
  search, target domain) → influential papers → Discussion & Conclusion. No
  Methods section, no "Literature Review" section.
- `1304.1002` (literature survey): Introduction → Methodology → Results →
  Related work → Threats to validity → Conclusion.
- `1808.04836` (survey study): Introduction → background → Study Design →
  Results → Threats to Validity → Related Work → Conclusion.
- `2002.12418` (systems paper): Introduction → Related Work → System →
  Evaluation → Conclusion.

Three regularities hold across all of them: there is no catch-all "Literature
Review" section; survey methodology is short and separate; validity is stated as
"Threats to Validity" before the conclusion.

The writer now follows that skeleton — Abstract, Introduction, Survey
Methodology, 2-6 thematic sections named by the field's own axes, optional
Comparative Analysis and Open Problems, Threats to Validity, Conclusion — and
`structure_errors` enforces it: required anchors, at least two thematic
sections, no catch-all bucket, anchor order, and an abstract cap. The body of the
resulting paper uses four axes (representation, supervision signal, decision
granularity, integration point), each with the same internal shape: problem,
approaches, evidence, judgment.

## ADR-042: A research campaign has no round cap and must still end

A literature review is not one search. The gather step is now a campaign: each
round runs searches and citation lookups, reads a batch of documents, and reports
only what it added; a deterministic coverage gate merges the rounds into one
ledger and decides whether to research again or write. `plan -> coverage ->
gather -> coverage -> ... -> write`, with an evidence gap returning to planning,
which re-enters the gate and resumes from the accumulated ledger.

There is no round cap. Research continues while a round adds evidence and the
gatherer does not report saturation, and it ends when a round adds nothing or the
last three rounds each added less than 5% of the corpus. That is a convergence
criterion: it fires on diminishing returns, never on a pre-set count. Exhausting
a field's literature is not the goal and is not reachable; covering its
load-bearing work is.

Three supporting rules keep the campaign honest:

- **The ledger is what the writer must cite in full.** A source whose evidence or
  reading cannot be verified makes that contract unsatisfiable — cite it and the
  citation fails, omit it and a source is uncited — so the gate removes it and
  reports it as `dropped_unverifiable`. Growth is measured against the verified
  ledger, so a dropped source neither counts as progress nor keeps a round alive.
- **Citation numbers never shift.** Removing a source leaves a gap rather than
  renumbering, because the writer is revising a manuscript that already cites the
  old numbers; a renumber made the manuscript point at the wrong sources, and
  eighteen write attempts could not repair it.
- **The gate and the validator share one source policy** (`runtime/evidence.py`).
  When admission and citation disagree, a source can be admitted and be
  uncitable, which is the same dead end from the other side.

Measured on one topic: thirteen rounds, 59 sources, 39 read in full, 24 pairs of
contradictory evidence, 31 minutes, 2.21 CNY.

## ADR-043: Approve a paper that meets the stated bar

The reviewer's instructions say `pass` requires "no unresolved major issues on both
dimensions". Measured over nine revisions of one paper, it reported zero major
issues on three separate rounds and still returned `revise` with six, six and four
minor findings. The paper met the bar the reviewer was given; the reviewer
withheld approval anyway. A careful reviewer will always find something minor, so
the revision loop could not end, and it cost 4.6 CNY to discover that.

Approval is therefore not left to the model's self-restraint. When the model
reports only minor findings and the deterministic checks pass, the gate approves
and records `approved_with`. That is "accept with minor revisions" — the decision
an editor makes so a thorough reviewer cannot block a paper that already meets the
stated standard. Major findings, and every deterministic finding, still block.

## ADR-044: Model calls are recorded as a projection, and replay matches by position

Context management is the product's core: which content reaches an agent, how it is
compressed, how it is recalled. A context policy is a *policy*, and a policy can
only be judged by comparison, which requires the variable to be controlled. Today
one campaign costs 31 minutes and 2.21 CNY and is entirely non-deterministic — the
model decides what to search, what to read, what to conclude. Change a context
policy and the outcome moves, and there is no way to tell whether the policy acted
or the model simply behaved differently this time. The improvement cannot be
attributed, so it cannot be measured, so it cannot be developed. `PRODUCT_VISION.md`
already promised R0 (event-history forensic replay) and R1 (deterministic
simulation with stubbed model results) under I9 and called them the only replay
levels that are guarantees. Neither was implemented.

Therefore every model call's request and response is recorded, and a recorded run
can be replayed deterministically. Three constraints make it work:

- **A recording is a projection, never state.** I2 already says caches, memory,
  vector indexes and summaries must never independently determine recovery. A
  recording is evidence for inspection and comparison; deleting one changes no
  recovery semantics. Without this stated, a recording would drift into being
  treated as the authority on what a model said.
- **Replay matches by structural position** `(node, attempt, sequence)`, not by a
  prompt hash. The whole point is to change the prompt; matching on it would make
  replay fail on exactly the change being studied.
- **Divergence fails loudly and located.** A replay whose call sequence does not
  match stops and names the call that diverged. Silently continuing would turn
  "replay succeeded" into a false signal, which is worse than no signal.

The wrapper goes on PydanticAI's `Model`, not on Anchor's `ModelGateway`. One
`generate_with_tools()` call covers an entire agent turn because PydanticAI runs
the tool loop inside `agent.run()`, so recording at the gateway would capture one
final answer and lose the growing context inside the loop — the very thing context
work needs to observe. `WrapperModel` is the library's own extension point, and all
three `durable_exec` backends use it.

The boundary is explicit: **replay cannot A/B a context policy that changes the
prompt.** The prompt changed, so the model must be called for real; serving the old
response measures nothing. This ADR buys observability and deterministic
reproduction. Cutting iteration cost needs a node-scoped harness — a frozen input
snapshot plus the ability to run one node, which does not exist today — and that is
a larger, separate decision.

Plan and acceptance criteria: `docs/RECORDING_AND_REPLAY.md`.

## ADR-045: A failed branch ends every node the run will never reach, by marking

A failed run left its unreachable nodes in `pending`. Measured before the fix: 76 such
nodes across the failed runs in one database, with no terminal state and nothing recording
why. From outside that is indistinguishable from a run still waiting for a worker, which is
the worst property a stalled run can have.

When a node failure fails its run, the run's non-terminal nodes are now set to `cancelled`
with `error_code="run_failed"`, the run's remaining leases are released, and the `run.failed`
event records how many nodes it ended. The shape is copied from `stop_run`, which already
did exactly this for operator stops; failure was the path that had been left out.

Two consequences worth stating:

- **Marking, not cancelling.** There is no channel to cancel a model call a worker is already
  making, and building one is a larger change than this defect justifies. Releasing the
  lease is what fences the attempt: completion requires an unreleased lease, so a fenced
  worker cannot commit a result into a failed run. A `FencedAttempt` is raised for that case
  specifically — distinct from `ConcurrencyConflict` — so a worker stops quietly instead of
  trying to fail a node that already has a terminal state, which would raise again and leave
  a traceback describing a run that behaved correctly.
- **A completed sibling is untouched.** It is evidence that work really happened, and a
  failed run legitimately contains completed nodes. Only non-terminal nodes are ended, and a
  duplicate delivery of the same failure is idempotent rather than a second fan-out.

One dispatch defect came with it: `dispatch_pending` aborted its whole batch when a message
could not be accepted, so a single row nobody could accept stalled every dispatch queued
behind it, forever, with only a log line to show for it. Each message is now attempted
independently and a failure records a durable `run.dispatch_failed` event keyed by the
stream the dispatch names — the run usually does not exist yet, which is often what failed.

## ADR-046: A run declares which run it replays, and substitutes by call ordinal

Replay matched recordings by `(node_run_id, attempt, sequence)`, which ties it to an existing
run's attempts. A freshly admitted run generates new node run ids, so it could never be
replayed — which is exactly what "record a campaign, replay it in a process with no provider"
requires.

A replay is now a run of the same graph executed by a process told which run it replays
(`ANCHOR_REPLAY_OF`). Its Nth model call is served by the recorded run's Nth call. The ordinal
is free: recordings are loaded from `model.call` events, and the store returns events in
sequence order, so the position in that list *is* the call order. The recorded `node_id` is
**checked**, not assumed — a call made by a different node than the recording belongs to is a
located divergence naming both sides, because serving one node another node's answer and
calling it a successful replay is the failure this guards against.

The premise is that the engine's path is a pure function of the graph, the inputs and the
model's answers. Replay substitutes the last of the three. That is why it is worth building:
if the path reproduces, then a change in behaviour is attributable to a prompt or a context
policy rather than to the engine, and without it every comparison of context strategies rests
on an unverified assumption. `scripts/validate_replay_run.py` demonstrates it — a recorded run
and a replay driven against a profile pointed at a dead port produce the same status, the same
node path and the same execution events, differing only in `model.call` versus
`model.call_replayed`, which is required so a replay never overwrites what it reads.

`request_digest` is recorded and compared, but a mismatch is *reported*, not fatal: changing
the prompt is the point of the exercise. Only a missing recording or a node mismatch fails.

### A cross-loop HTTP client looks exactly like a provider fault

The acceptance initially "failed" with `ModelAPIError: Connection error.` on every second
call, alternating reliably, which invited the conclusion that the provider was flaky. It was
not. `build_model_gateway` holds one `httpx.AsyncClient`, which binds to the event loop it is
first used in; driving a gateway built outside the loop with a fresh `asyncio.run` per node
makes every other call fail instantly. `scripts/node_harness.py` documents this in a
docstring and gets it right by doing all its async work inside one `_execute` coroutine. The
acceptance ignored its own project's note.

The consequence is worth stating because of how it presents: an instant `Connection error`
is a client-side symptom, not a network one, and it is indistinguishable in a journal from a
provider outage. Any script that builds a gateway and then calls `asyncio.run` per attempt
has this bug. The services do not: each runs `asyncio.run(serve())` once.

## ADR-047: The prompt reports what it was made of, because its size does not decide its cost

Measured on our provider: a cached input token costs 1/50th of an uncached one, and two thirds
of a real campaign's bill was output rather than input. So the alarming gross prompt counter is
not the bill, and the size of a prompt is not what makes it expensive — whether its prefix is
*stable* is. Nothing recorded that, so the question could not be asked.

Each model call now reports three hashes: the prefix (the instructions actually sent as the
system prompt), the declared input (the task frame plus the snapshot), and the working set (run
memory and promoted knowledge). A segment whose hash never changes is one the provider could
cache; one that changes on every call could not, and the token counts cannot tell the two
apart. When a caller assembles its own prompt the report says the segments are unavailable
rather than implying the prompt had no parts.

The split into new and re-sent content comes from consecutive input counts, using the fact that
each call re-sends every earlier prompt: of the tokens in a call, `min(this, previous)` were
already sent and the excess is growth. Written that way it sums to the gross count exactly,
including when a prompt shrinks — memory gets trimmed and a snapshot gets corrected, and a
decomposition assuming monotonic growth would stop adding up while still looking plausible. A
first attempt used `max(delta, 0)`, which failed exactly that way; a test caught it.

The segments come from the assembler rather than being reconstructed from the rendered text,
because a reconstruction would eventually measure something other than what was sent. This
made `PromptFactory` return a `ResolvedPrompt` dataclass instead of a widening tuple: it was
already at four positional fields, and a fifth read by position is a silent bug waiting.

The prefix is filled in by the worker, not the resolver, because the instructions come from the
capability the worker resolves and the worker is what sends them. Hashing anything else would
report a stable prefix for a prompt that never had one.
