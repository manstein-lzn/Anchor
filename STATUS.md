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
- Content-plane architecture decided (ADR-026): Recovery Closure model, Git-first +
  CAS-backed `WorkspaceRevision`, per-run workspace with node-level revision lineage,
  `require_clean` merge, verification against a frozen revision
- Design docs: `WORKSPACE.md`, `CONTENT_COMMIT_PROTOCOL.md`, `WORKSPACE_STORAGE.md`
- Rolling retention sweep (scheduler): evicts oldest finished runs over budget,
  skips in-flight/waiting/unknown runs, garbage-collects orphan artifacts, vacuums,
  and audits every sweep; no budget means no deletion
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

### Content plane (ADR-027 … ADR-038)

- `domain/content.py` defines `ContentRef` (`artifact://` | `workspace://`), the
  single boundary type; a mutable revision is rejected at parse time and an
  unregistered kind or missing blob fails closed
- Read-only content sources: projects, `GitWorkspaceBackend` (`cat-file`/`ls-tree`,
  never a checkout), materialize plus read-only execution under bubblewrap
- Writable workspaces: run-level worktrees, write/delete/freeze/archive, and a
  complete operation ledger; bubblewrap is the only sandbox backend, and its
  absence disables `workspace.exec` rather than degrading isolation
- Content-commit protocol with an explicit prepared window, five injected crash
  windows, and `inconsistent` (never a guess) when content cannot be read
- Native workspace tools (`workspace.read/write/list/exec`) with their own ledger,
  separate from the gateway tool ledger
- A node's output is its workspace revision; the model text is stored as an
  artifact in the event payload
- Single-writer claim per workspace with pinned reads; failures return as
  messages rather than leaving a node `running` on a stale lease
- Explicit fork and `require_clean` merge for parallel branches, with
  `anchor.join_merge` as the join behavior; a conflict aborts and fails closed
- `scripts/validate_workspace.sh` (6/6), `validate_workspace_lineage.sh` (7/7) and
  `validate_workspace_parallel.sh` (7/7) drive real runs over real repositories

### Agent surface (ADR-039)

- `anchor.client` — one typed operation layer; `anchor.cli` and `anchor.mcp` are
  thin adapters over it, so their semantics cannot drift
- `GET /api/graphs/ir` — the machine-readable authoring reference, so an agent does
  not have to reverse engineer the IR
- `anchor-mcp` — MCP server over stdio (newline-delimited JSON-RPC), one tool per
  operation with a closed schema; human-only and operator-only operations are
  refused by default
- `scripts/validate_mcp.py` — a real stdio client authors, runs and observes a run
  end to end (18/18)

### Deep research (ADR-040 … ADR-043)

- The paper is for the reader and verification is a pipeline property: `gather`
  produces the evidence ledger, `write` produces the manuscript, `review` judges
  substance and craft and names where the fix belongs
- The section skeleton is measured from published surveys, not invented, and the
  deterministic gates enforce it along with craft rules (no audit language, no
  process language, no hashes, bounded paragraphs)
- Research is a campaign with a convergence criterion and no round cap; the ledger
  is what the writer must cite in full, so unverifiable sources are removed rather
  than admitted
- Approval follows the reviewer's own bar: only minor findings plus clean
  deterministic checks approve the paper
- Token accounting: `ModelResponse` carries input/output/cache tokens, the worker
  emits a `model.usage` event per call, and `GET /api/runs/{id}/usage` reports
  gross, cached and billed input per node
- `scholarly.citations` (citation chasing) and `scholarly.read_many` (batch reads)
  keep the model's turn count down, which is what a run's cost is made of

## Not yet implemented

- **Failure fan-out** (done, DEVELOPMENT_PLAN P0.2, ADR-045): a node failure that fails
  its run ends every non-terminal node with `error_code="run_failed"`, releases the
  run's remaining leases, and records `abandoned_nodes` on the `run.failed` event.
  Marking rather than cancelling, because there is no channel to cancel a model call in
  flight and releasing the lease already fences the attempt; a fenced worker raises
  `FencedAttempt` and stops quietly rather than reporting a run that behaved correctly.
  `dispatch_pending` no longer aborts its batch on one undeliverable message, and records
  a durable `run.dispatch_failed` event instead of only a log line. Verified by unit
  tests over the four sibling timings plus
  `scripts/validate_failure_fanout.py` against the running services. **Not done**: 76
  historical `pending` nodes from before the fix are deliberately not backfilled —
  rewriting them would make the event history stop being the truth about what happened.
- PostgreSQL/vector memory projection and worker recovery supervision
- **Model call recording and replay** (ADR-044). Steps 1-2 are done. Every model
  call is written as an immutable projection with a `model.call` event; the wrapper
  sits on PydanticAI's `Model`, so tool-loop calls are captured and instructions are
  recorded separately (they do not appear in `messages`); `ANCHOR_MODEL_RECORDING`
  defaults to `off`. Replay serves recorded answers by `(node_run_id, attempt,
  sequence)`, reports a changed prompt instead of rejecting it, fails loudly and
  located when a call has no recording behind it, and never falls through to the
  live model — not for a missing recording and not for a streaming call. Verified
  against live `deepseek-flash` calls. **Not done**: recordings are not yet part of
  retention, and a whole-campaign replay has not been run end to end. This is I9's
  R0/R1, promised and until now absent; it is the prerequisite for developing
  context policy, which can only be judged by controlled comparison. See
  `docs/RECORDING_AND_REPLAY.md`.
- Context engine and memory policies beyond the durable input snapshot boundary
- **Persistent services and restart recovery** (done, DEVELOPMENT_PLAN P0.1): seven
  user-level units (api, receiver, worker, control, verifier, scheduler, supervisor) are
  installed and enabled, each declaring every path explicitly and carved with
  `StartLimitIntervalSec`/`StartLimitBurst` so a crash loop is a visible failure rather
  than a permanent `activating`. Every service runs `anchor.runtime.preflight` before its
  loop: no silent database URL fallback, schema migrated, runtime profile parseable,
  artifact root writable, with an unmigrated database distinguished from an unreachable
  one. Verified by `scripts/validate_service_recovery.py`: a worker is stopped mid-node,
  the lease is reported `stale` and `recoverable` rather than silently reclaimed, explicit
  recovery re-queues the node, and the run closes with a continuous event sequence and no
  duplicated operation. **Not done**: lease observation and recovery have no
  `anchor.client` method, so an agent cannot do what this acceptance does.
- **A node-scoped harness** (done): re-run one node attempt against the input it
  actually received, read from its own persisted context snapshot and assembled by
  the same function the worker uses. Measured on a real run: re-running the writer
  takes 40 seconds instead of a 31-minute campaign, and two calls of the same node
  produce different outputs, which is why a campaign cannot judge a context policy.
  A tool-using node is refused rather than executed outside a ledger, so `gather`
  still needs a whole run; giving the harness scratch-run ledgering is the next step
  there. See `runtime/node_harness.py` and `scripts/node_harness.py`.
- A2A gateway (MCP is implemented; see ADR-039)
- OpenTelemetry integration
- Automatic worker/service startup after a host reboot: the dev services are
  transient systemd units, so a long run stalls until an operator restarts them
- A revision bound as an explicit operator policy: the system detects an unchanged
  defect and a paper that meets the bar, but there is no configured ceiling on how
  many times a paper may be revised
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
   execution, including pause/resume/stop, approvals and trigger management.
   Unsaved browser edits are memory-only. See WEB.md for exact limits.
9. The local worker service is intentionally explicit: it requires a runtime
   profile with matching Agent capabilities and does not invent missing references.
10. A tool loop re-sends its whole conversation on every model call, so a run's cost
    is turns times context. Most re-sent tokens are a cached prefix, but the gross
    counter is several times the charge; budget against `billed_input_tokens`.
11. The scholarly adapter paces each source (arXiv 1 request per 3 s, Crossref per
    second). Reading N papers costs at least 3N seconds of wall clock no matter how
    the code is arranged, so a deep run is bounded by the sources' terms, not by us.

## Next milestone

The core product is complete end to end: author a graph, run it durably, watch it,
intervene, and (for the academic graph) receive a reviewed paper. The next phase is
hardening and provider-free verification, not RSI or a broad cognition expansion.

The authoritative execution plan is [`docs/DEVELOPMENT_PLAN.md`](docs/DEVELOPMENT_PLAN.md).
It separates the current product runtime, optional continuity experiments, and future
RSI. No task is complete until its normal, failure, restart and audit evidence is
reported.

```text
P0 Runtime hardening
   - Persistent services and host-restart recovery.
   - Failure fan-out/cancellation and dispatch supervision.
   - Explicit production-boundary failures and diagnostics.

P1 Replay and economics
   - Whole-campaign model recording/replay and retention.
   - Stable-prefix, working-set and billed-token telemetry.
   - Cross-run immutable content cache.

P2 Quality and product completion
   - Provider-free end-to-end CI from recorded model responses.
   - Revision policy, approval surface and protocol stabilization.

P3 Optional continuity experiments
   - Only after P0-P2: behavioral takeover and controlled working-set selection.

P4 Future RSI
   - Offline attribution, candidate strategy evaluation and approved versioned changes.
```
