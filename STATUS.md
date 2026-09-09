# Anchor Implementation Status

## Completed

- Repository baseline and MIT project metadata
- Domain `Task` and `Run` models with explicit revisions and workflow version
- Single relational state store (SQLAlchemy) serving PostgreSQL and SQLite; the raw-sqlite prototype was deleted
- Append-only per-stream events with monotonic sequence numbers
- Idempotent duplicate event delivery and fail-closed key reuse
- Framework-independent `GeneralHarness` and `WorkflowService` protocols
- Declarative Graph IR with typed node kinds, edges, triggers, and static validation
- Stored graph versions reject content replacement and invalid hashes; Python DTOs
  remain mutable and are revalidated at publication
- Persisted Graph Version and Trigger records
- Runs require a published Graph Version; NodeRun model is defined
- Explicit Loop topology checks without required numeric budgets; loop execution
  lands separately from the budget-free supervision semantics
- Advisory watchdog distinguishes liveness, declared waits, verified
  progress, repeated-cycle suspicion and uncertainty; does not interrupt runs
- Advisory watchdog backed by persistent progress evidence and diagnostic requests; still does not interrupt runs
- Initial unit tests
- Atomic trigger admission: Task, pinned Run, pending nodes, events, outbox and receipt
- Duplicate occurrence recovery and conflicting-request rejection
- Single-dispatcher outbox pump with stable IDs and explicit at-least-once contract
- Subprocess exit, database reopen, concurrent admission and lost-acknowledgement tests
- PostgreSQL JSONB for structured payloads; the same adapter runs fast contract tests on throwaway SQLite files
- Explicit Alembic initial migration, drift check and disposable migration roundtrips
- Shared relational contracts verified against a real PostgreSQL 17 container
- Optional local PostgreSQL Compose definition; no persistent dev service started
- Draft persistence with optimistic revisions and atomic, idempotent publication receipts
- Authenticated FastAPI draft/validation/publication/manual-trigger/admission/query API
- Cursor-based event polling and sanitized request/storage errors
- HTTP contract tests on both migrated SQLite and PostgreSQL
- Local API running as transient user service on 127.0.0.1:8090 with owner-only token file
- React/React Flow web editor using the existing Graph IR and authenticated API
- Node/edge configuration, drag layout, undo/redo, JSON import/export and static validation
- Optimistic draft saves with explicit conflict handling; read-only publication history
- Desktop and narrow-screen browser tests against a disposable migrated real API
- Durable execution inbox with message/run identity checks and replay-safe acceptance
- Receiver advances Task to ready, Run to queued, and only the graph entry node to ready
- Receiver heartbeat exposed through API readiness without imposing Agent run limits
- Stable, idempotent ready-node claims with worker ownership and lease heartbeats
- Concurrent workers cannot claim the same node; overdue leases are never auto-stolen
- Agent workers claim only Agent nodes; unsupported verifier/tool nodes remain ready
  for their dedicated executor instead of leaking a model-worker lease
- Atomic known-failure transition records `node.failed`, `run.failed`, and
  `task.failed`; terminal success likewise advances both Run and Task
- Failed Runs cannot be revived by an already-running sibling branch, and no new
  downstream node is opened after terminal failure
- Tool operation ledger with stable request identity and explicit unknown outcomes
- Operation registration, start and terminal results are replay-safe canonical events
- Unknown outcomes require evidence-linked, conflict-checked reconciliation
- Web Run Console shows persisted Run, node, event and operation state without synthetic progress
- Capability descriptors and runtime registry for model, Agent and tool references
- Owner-only SecretProvider boundary with environment and JSON-file providers
- Optional PydanticAI Responses/Chat-Completions gateway with local Codex-compatible profile
- Capability-aware Agent worker skeleton with an explicit atomic result-sink boundary
- Long-lived worker loop and explicit local worker service entrypoint
- Durable local memory store with provenance, content hashes, and tombstone deletion
- Worker context resolver reads confirmed Run-scoped memory without exposing deleted records
- Authenticated memory query and tombstone-delete API
- Content-addressed local artifact store and artifact checkpoint sink
- Deterministic node input snapshots, edge mappings, and SHA-256 input hashes
- Immutable per-generation context snapshots persisted atomically with NodeRun completion;
  single-store reads and authenticated API endpoint
- JMESPath edge conditions with publication-time syntax checks and strict boolean runtime results
- Immutable per-Run edge decisions with evaluator version, context hash and artifact evidence
- Conditional/multi-predecessor propagation with explicit skipped branches and selected-edge input mapping
- Join readiness waits for every incoming decision and every selected completed predecessor
- Deterministic Router/Parallel/Join/Artifact executor with a dedicated type-filtered claim and service
- Explicit operator-confirmed recovery for Agent and deterministic control leases
- Independent Verifier executor with typed claim, deterministic JMESPath or strict-JSON
  model adapter, content-addressed evidence artifact, persisted VerificationRecord
  bound to artifact/context hashes, and a completion gate no other worker can bypass
- Verifier `passed` completes the node and opens downstream edges; `rejected`/`error`
  persist the record and fail the node/run/task without opening any downstream edge
- Model transport failure during verification keeps the node `running` with its lease;
  no synthetic verdict is written
- Authenticated `GET /api/runs/{run_id}/verifications` and Run Console evidence view;
  readiness reports `verifier_worker_connected`
- Real isolated process E2E with `gpt-5.6-luna`: Agent produce -> Verifier verify ->
  Artifact report reaches terminal Run, plus a rejection-path run that fails closed
- Verifier adapter rules scored through `pydantic-evals` (`evals` extra, offline only):
  deterministic matrix and strict-JSON matrix share the pure `parse_model_verdict`
  with the online worker, so scores and verdicts cannot drift
- Conservative Graph-aware lease supervisor assessment with Agent recovery
  recommendations and Tool unknown-outcome fail-closed semantics
- Typed execution-limit taxonomy (transport / resource capacity / explicit
  operator policy / task behavior); no hidden `max_rounds` or
  `run_timeout_seconds` default, and `ANCHOR_EXPIRE_RUN_BUDGETS` defaults off
- Independent counters for business cycles, node attempts (including fault
  retries) and request retries; a new cycle never consumes fault-recovery budget
- Durable `ProgressEvidence` and deduplicated `DiagnosticRequest` persistence;
  repeated cycles without verified progress are diagnosed, never auto-failed
- Persisted transient-failure recovery schedule (`last_error_class`,
  `next_attempt_at`, Retry-After aware); claim is gated until due, and the plan
  survives a worker or supervisor restart without an in-process sleep
- `GET /api/runs/{run_id}/progress`, `GET /api/runs/{run_id}/diagnostics` and
  operator supersede; Run Console diagnostics and progress-evidence sections
- Explicit side-effect HTTP tool (`http.post`) with an approval-predecessor gate; the
  gateway refuses side-effect tools unless the caller proves approval, and agent
  tool loops can never obtain one
- `outcome_unknown` is never retried: the operator reconciles the operation with
  external evidence and the ledger deterministically completes or fails the node
- Run pause/resume: pause fences new claims while in-flight nodes finish; resume
  returns the run to running (`POST /api/runs/{id}/pause|resume`)
- Web trigger management against one immutable published version; webhook triggers
  carry a secret reference only
- Run archiving (reversible `archived_at`; default list hides archived; evidence stays
  queryable by id) and run-list filtering by status/id
- Runtime-adjustable storage budgets (install-wide + per graph) with a read-only
  `GET /api/storage` footprint report and a Web 存储 view
- Dagre layered auto-layout for graphs without saved drag positions; saved positions win
- Bezier forward edges with per-fan-out curvature, backward edges routed below the graph,
  hover/selection-only labels, and a wide interaction path
- React Flow controlled-node fix: `onNodesChange` + preserved `measured` so dragging
  never drops connections (error #015)
- Read-only active lease API with optional Run filtering and Web Console lease view
- Explicit Agent lease failure API and Run Console action; Tool leases cannot
  bypass unknown-outcome reconciliation through this endpoint
- Standalone supervisor service and user-level systemd unit with deduplicated alerts
- Real local-database short-window validation for scheduler and supervisor services
- Real isolated single-node and two-node process E2E with `gpt-5.6-luna`: receiver,
  worker service, artifact checkpoint, downstream readiness, and terminal Run completion
- Isolated process recovery E2E: interrupted Agent worker leaves its lease active,
  supervisor marks it stale/recoverable, explicit recovery requeues it, and a new
  worker completes artifact, context snapshot and terminal Run
- Responsive Graph Builder viewport fitting uses declared node dimensions and the
  measured canvas; desktop/mobile Playwright coverage is stable across breakpoints

## Not yet implemented

- Failure fan-out/cancellation of in-flight sibling branches and dispatch supervision
  beyond receiver retry logging
- PostgreSQL/vector memory projection and worker recovery supervision
- Context engine and memory policies beyond the durable input snapshot boundary
- MCP/A2A gateways
- OpenTelemetry integration
- Production identity/authorization, approval UI, and artifact service

## Current risks

1. SQLite is single-process development storage only.
2. The in-process runtime does not claim crash recovery or exactly-once side effects.
3. The public protocols are intentionally small and will gain versioning before a
   compatibility promise is made.
4. Old raw-sqlite prototype files (pre-single-store) are not migrated; do not reuse them.
5. Database failover, production permissions and advanced workflow execution remain unverified.
6. This workstation lacks Docker Compose; contract tests used a temporary Docker
   container. The Compose definition has not been validated by the Compose CLI.
7. Local API is single-user development only; HTTP 202 means persisted admission,
   not active Agent execution. Live API uses .local/api.sqlite, not the test container.
8. The Web editor authors/publishes Graphs and the Run Console monitors canonical
   execution, but trigger management, approvals and pause/resume are not complete.
   Unsaved browser edits are memory-only. See WEB.md for exact limits.
9. The local worker service is intentionally explicit: it requires a runtime
   profile with matching Agent capabilities and does not invent missing references.

## Next milestone

```text
P0 Sandbox/ToolGateway v1 landed (ADR-012): deny-by-default policy +
   BubblewrapBackend (no net, read-only root, private workspace, scrubbed
   env) + ledger-first execution with replay-safe outcomes; side-effect
   tools refused pending tool-level approval.
   Approval/HumanTask/Wait durable states landed (ADR-013): waiting states
   block terminals, no worker claims them, decide/resume share the
   propagation tail; `GET /api/waits` + approve/reject/resume endpoints +
   Run Console actions.
   Agent tool-use loop landed (ADR-014): pydantic-ai function tools bound to
   ledger-backed gateway execution with deterministic operation identities;
   denials return as messages; TestModel proves the loop offline.
P1 Subgraph composition landed as publish-time materialization (ADR-011):
   pinned child expanded inline with namespaced IDs, zero execution changes,
   provenance in version metadata. Web Builder authors subgraph nodes.
   Remaining: independent child runs (needs durable wait states).
   Graph Bundle file tier landed (ADR-015): export/import endpoints with
   hash verification and trigger rebinding + Web export/import; layout
   stays local, secrets never travel.
   Loop execution landed (ADR-017, migration 0011): control pass-through,
   per-attempt re-arming, attempt-scoped decisions, skipped revival,
   deterministic recency mapping. No iteration budgets.
   Tool node execution landed (ADR-018): control-claimed gateway calls under
   explicit owner_agent with snapshot arguments; denials fail closed; tool
   leases stay unknown and unrecoverable via lease path.
   Real loop E2E with gpt-5.6-luna: iteration, exit, revival, terminal Run.
   Execution policy landed (ADR-019, migration 0013): typed limit taxonomy,
   no hidden round/wall-clock defaults, independent cycle/attempt/request
   counters, durable ProgressEvidence + deduplicated DiagnosticRequest,
   progress/diagnostics API and Run Console views. Automatic repair and
   calibrated adaptive detection remain explicitly unimplemented.
P1 Anti-drift context gates: mechanical layer landed; semantic baseline
   landed as offline evals coverage tripwire (LLM judge is the explicit next
   step, same interface). Experience promotion loop landed (ADR-016):
   propose -> review -> promoted knowledge enters prompts, write-back
   explicitly deferred. (`runtime/integrity.py`:
   snapshot hash recompute, dense generations, pin hash, decision structure,
   evidence readability, verification binding; prompt resolver refuses corrupt
   context with the lease left for supervision). Semantic judge layer next.
   (Prefect stays the considered durable-execution option for later; rejected
   pydantic-graph: no persistence, no skipped-cascade/join-wait, no audit
   records, subgraphs explicitly TODO. See HANDOVER spike record.)
P2 Experience promotion loop: run memory -> review -> versioned organizational
   knowledge -> write-back into graphs/capabilities, audited and reversible.
P2 Single-graph deployment (Docker per scenario) + embedded trigger/artifact SDK.
Later Minimal-binary runner, vector retrieval, full production hardening.
```

See API.md and WEB.md for current limits. Product direction details live in
PRODUCT_VISION.md (Composable graphs, Modular delivery, Runtime kernel).
