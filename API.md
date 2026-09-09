# Product API

The API is a local single-user development surface, not the final graph editor.
All `/api/*` operations and readiness checks require a bearer token. Liveness,
OpenAPI and Swagger documentation are public and contain no run data. There is no
multi-tenant authentication, RBAC, rate limiting or public internet deployment yet.

## Start

Install `.[dev,storage,api]` in `.venv`. Explicitly configure `ANCHOR_DATABASE_URL`
and run `alembic upgrade head`. The current schema revision is `0009_edge_decisions`.

```bash
cd /home/mansteinl/Anchor
.venv/bin/python -m anchor.api --port 8090
```

The launcher binds only `127.0.0.1`. It uses `ANCHOR_API_TOKEN` when provided;
otherwise it creates `.local/api-token` with owner-only read/write permissions.
The token is never printed and is excluded from Git. Swagger's Authorize control
accepts this token as a Bearer credential. Do not put it in a URL or browser query.
Documentation: `http://127.0.0.1:8090/docs`.

The service started during development uses the new relational SQLite database
`.local/api.sqlite`. It does not touch old prototype databases. PostgreSQL is
supported by the same API and is verified separately in the contract suite.
The current background process is a transient user service, `anchor-api-dev.service`.
It is not enabled for automatic startup. Inspect it with `systemctl --user status
anchor-api-dev`; read logs with `journalctl --user -u anchor-api-dev`; stop it with
`systemctl --user stop anchor-api-dev`.

## Lifecycle

| Operation | Endpoint | Semantics |
| --- | --- | --- |
| Save draft | `PUT /api/graphs/{graph_id}/draft` | `expected_revision=0` creates; subsequent saves use the returned revision |
| Read/list drafts | `GET /api/graphs/{graph_id}/draft`, `GET /api/graphs` | Incomplete definitions and canvas layout can be saved |
| Validate | `POST /api/graphs/validate` | Pydantic/schema and static topology validation; no execution or model call |
| Publish | `POST /api/graphs/{graph_id}/publish` | Requires draft revision, assigns version number server-side, retries reuse publication |
| Read versions | `GET /api/graphs/{graph_id}/versions`, `GET /api/graph-versions/{id}` | Published content is separate from mutable drafts |
| Register trigger | `PUT /api/triggers/{id}` | Client-generated UUID makes registration retries idempotent; supports manual, interval, cron, internal-event and webhook configuration |
| Enable/disable | `PATCH /api/triggers/{id}` | Does not cancel previously admitted runs |
| Submit run | `POST /api/triggers/{id}/runs` | Requires `Idempotency-Key`; returns 202 and a durable receipt |
| Read run | `GET /api/runs/{id}`, `GET /api/runs` | `created` before receiver acceptance; `queued` after durable acceptance |
| Read task/nodes | `GET /api/tasks/{id}`, `GET /api/runs/{id}/nodes` | Initial nodes are pending, not executed checkpoints |
| Read execution contexts | `GET /api/runs/{id}/contexts` | Immutable per-generation input snapshots with SHA-256 hashes |
| Read routing decisions | `GET /api/runs/{id}/decisions` | Selected/rejected edge decisions with evaluator version, context hash and evidence reference |
| Read events | `GET /api/runs/{id}/events?after=0&limit=100` | Incremental polling by sequence; not SSE yet |
| Read operations | `GET /api/runs/{id}/operations` | Audit view of registered tool requests and explicit outcomes |
| Reconcile unknown outcome | `POST /api/operations/{id}/reconcile` | Records external evidence; deterministically completes or fails the node without a new attempt |
| Pause/resume run | `POST /api/runs/{id}/pause`, `POST /api/runs/{id}/resume` | Pause fences new claims; in-flight nodes finish and downstream stays ready |
| Archive run | `POST /api/runs/{id}/archive`, `POST /api/runs/{id}/unarchive` | Reversible; only terminal runs; evidence stays queryable by id |
| Filter runs | `GET /api/runs?status=cancelled&include_archived=true` | Operator view; archived runs are hidden by default |
| Storage report | `GET /api/storage` | Read-only: database bytes + artifact bytes, per-graph attribution (exclusive/shared) |
| Storage budgets | `GET/PUT /api/storage/budget` | Runtime-adjustable global and per-graph targets; never terminate a running node |
| Retention preview | `GET /api/retention/preview` | Dry run: candidates oldest-first, protected count, over-budget graphs; deletes nothing |
| Retention sweep | `POST /api/retention/sweep` | Evict oldest finished runs over budget, garbage-collect orphan artifacts, vacuum |
| Retention audit | `GET /api/retention/audit` | Counts and freed bytes per sweep; stores no run content |
| Read/delete memory | `GET /api/memory`, `DELETE /api/memory/{id}` | Provenance-bearing memory with tombstone deletion; audit reads use `include_deleted=true`; filters `status`/`domain` |
| Propose lesson | `POST /api/memory/propose` | Candidate experience with domain and provenance; enters review, not prompts |
| Review lesson | `POST /api/memory/{id}/review` | Promote/reject with reviewer and reason; only promoted knowledge enters future prompts |
| Read artifact | `GET /api/artifacts/{sha256}` | Returns only an integrity-verified content-addressed artifact |
| Export bundle | `GET /api/graph-versions/{id}/bundle` | Portable tamper-evident bundle: version, triggers, capability refs |
| Import bundle | `POST /api/bundles/import` | Verifies hash, creates draft, optionally publishes and rebinds triggers |
| Recover Agent/control lease | `POST /api/leases/{claim_id}/recover` | Explicit, reason-bearing recovery for interrupted Agent or deterministic control nodes; never recovers tool side effects |
| Fail Agent lease | `POST /api/leases/{claim_id}/fail` | Explicitly records a known Agent failure and atomically fails its Run/Task; unavailable for Tool leases |
| List waits | `GET /api/waits?run_id={id}` | Nodes parked in approval/event waits with node type context |
| Approve | `POST /api/waits/{node_run_id}/approve` | Reason-bearing approval; completes the node and opens downstream |
| Reject | `POST /api/waits/{node_run_id}/reject` | Reason-bearing rejection; fails node/run/task with no downstream |
| Resume event | `POST /api/waits/{node_run_id}/resume` | Matched event ingress resumes the wait; mismatches are 409 |

List pagination bounds protect a query response, not Agent lifetime or iterations.
The graph editor will manage revisions and occurrence IDs automatically; users should
not have to type version numbers or retry keys in the eventual UI.

Drafts allow incomplete JSON to support editing. Publication validates the full
GraphDefinition, identity, topology and JMESPath condition syntax. Capability validation
resolves configured Agent/Model/Tool references through its separate endpoint. Static
validation does not execute conditions or prove task quality. Layout does not enter
execution content.

Edge conditions are JMESPath expressions evaluated against:

```json
{
  "output": {"approved": true, "score": 9},
  "inputs": {"minimum": 7}
}
```

For example, `output.approved && output.score >= inputs.minimum` selects an edge.
JSON literals use JMESPath backtick syntax, such as ``output.approved == `false```.
The result must be a boolean; missing/non-boolean data fails the completion transaction.
Plain-text model output remains a string in `output`. Raw evaluation data is already
represented by the content-addressed output artifact and immutable input snapshot;
the routing record stores their deterministic context hash and evidence reference.

All incoming edges must have durable decisions before a node becomes ready. At least
one must be selected; otherwise the node becomes `skipped` and the rejected path is
cascaded without a worker lease. A successful Run may therefore contain both
`completed` and `skipped` NodeRuns. Input mappings consume only selected edges.

## Failure behavior

- 401: missing or invalid credentials.
- 404: missing resource.
- 409: stale draft, conflicting publication or idempotency-key reuse.
- 422: invalid request/graph or disabled trigger.
- 503: storage unavailable/not ready, without exposing SQL or connection credentials.

A saved draft uses optimistic concurrency. A lost save response can cause a retry
to receive 409; reload the draft and compare, never overwrite silently. Publication
and run submission instead retain durable receipts for retry after a lost response.

Manual run submission cannot activate stored cron/webhook/internal-event triggers
or bypass filters. The local durable receiver accepts the run into `execution_inbox`,
marks only the graph entry node `ready`, and acknowledges the outbox. A configured
Agent worker can claim Agent nodes, call the model gateway, persist an artifact and
advance downstream nodes. The dedicated control worker executes Router/Parallel/Join/
Artifact nodes deterministically and writes real JSON artifact/context checkpoints.
The dedicated verifier worker claims only Verifier nodes, evaluates a deterministic
JMESPath expression or a strict-JSON model verdict against integrity-checked
predecessor artifacts, persists a VerificationRecord with evidence, and only a
persisted `passed` completes the node and opens downstream edges.
Approval/HumanTask/Wait, Loop, ToolGateway, pause/resume and MCP/A2A remain
outside the current implementation. There is no endpoint that pretends to have
completed those operations.

`/health/ready` reports independent `execution_connected`, `worker_connected`,
`control_worker_connected` and `verifier_worker_connected` heartbeat states. Heartbeat freshness detects process
connectivity; it is not an Agent lifetime, step, token, cost, or completion budget.

## Progress and diagnostics

The supervisor records immutable progress observations and raises a durable
diagnostic request when a completed cycle repeats without verified progress.
Neither is a failure verdict; healthy work continues while the evidence is
inconclusive.

```http
GET  /api/runs/{run_id}/progress
GET  /api/runs/{run_id}/diagnostics
POST /api/runs/{run_id}/diagnostics/{diagnostic_id}/supersede
```

`progress` returns the persisted `ProgressEvidence` stream (state revision, phase,
artifact refs, verifier passes, tool operation ids, cycle fingerprint). `diagnostics`
returns only open requests; the supersede endpoint records an operator decision
without changing Run state. Counters are deliberately separate: business cycles,
node attempts (including fault retries) and request retries must not be conflated.

## Verification

`tests/test_api.py` runs against migrated SQLite and isolated PostgreSQL schemas,
covering authentication, concurrent publication, stale drafts, repeat submissions,
API restart, node/event reads and sanitized error responses. See POSTGRES.md for
`ANCHOR_TEST_POSTGRES_URL` setup.

With a live local server, this creates a clearly named smoke graph and pending run:

```bash
.venv/bin/python scripts/api_smoke.py
```

It performs no model/tool call and does not print credentials. Each invocation
creates a separate smoke graph; it is a development check, not a business workflow.
