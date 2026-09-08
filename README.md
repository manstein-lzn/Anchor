# Anchor

Anchor is a vendor-neutral foundation for long-running, recoverable Agent products.
It treats the model as a probabilistic decision component and keeps durable business
state, audit history, side-effect records, and workflow control outside the model.

The primary product is a user-friendly web application where a domain engineer can
visually define, validate, publish, trigger, observe, approve, and resume a
long-running multi-agent collaboration graph. See `PRODUCT_VISION.md` for the
non-negotiable requirements and acceptance scenarios.

## Design principles

1. Canonical State is the source of truth. Conversation history, summaries, and
   vector memory are projections or retrieval aids.
2. Every state transition is an append-only event with a stream sequence and an
   idempotency key.
3. A model session is disposable. A run can be reconstructed from canonical state,
   events, artifacts, and a versioned context policy.
4. Side effects require an operation id and an execution ledger before retries are
   allowed.
5. Frameworks are adapters. Domain code must not depend on PydanticAI, Prefect, a
   provider SDK, or a specific observability backend.
6. Completion is verified and committed; a natural-language completion claim is not
   sufficient.
7. Anchor owns the product kernel and assembles mature OSS for generic infrastructure;
   it does not reimplement workflow, model gateway, storage, telemetry, or UI
   primitives without a demonstrated gap.
8. The runtime trusts healthy forward progress. Adaptive watchdogs handle stalls,
   disconnections, and repeated side effects; users should not have to guess
   arbitrary numeric limits for ordinary graphs.

## Current slice

The first engineering slice is intentionally small and executable:

```text
Task -> Run -> Harness step -> Event store -> Verify -> Commit
```

It includes a single relational store (SQLite for development, PostgreSQL for production), an in-process workflow test adapter, protocol
boundaries, and atomic trigger admission with a transactional outbox. See
`RUN_ADMISSION.md` for duplicate-delivery and crash-recovery guarantees and limits.
A SQLAlchemy adapter and Alembic migrations now support PostgreSQL; see `POSTGRES.md`.
An authenticated local FastAPI service exposes drafts, publication and run admission;
see `API.md`. Prefect, PydanticAI, MCP and A2A are integration layers that will
be added behind these boundaries after the recovery semantics are tested.

A React Flow web workspace now edits the same Graph IR, saves optimistic drafts,
validates and publishes immutable versions. Open <http://127.0.0.1:5173> while the
local web service is running. See `WEB.md` for connection, examples and development.
The editor publishes graph definitions; execution is performed by the separate
worker service. Trigger registration and event ingress are available through the
authenticated API, while richer trigger authoring UI remains under development.

## Local model profile

The development profile in `.local/runtime.json` points at the same OpenAI
Responses-compatible endpoint used by the local Codex installation and uses the
model name `gpt-5.6-luna`. It contains only a secret reference; the key is read at
runtime from `/home/mansteinl/.codex/auth.json`, which must remain owner-readable
(`chmod 600`). The profile can be loaded with:

`examples/runtime.codex.json` is a safe, credential-free template for recreating
this profile on another development machine. Copy it to `.local/runtime.json` and
adjust only the endpoint, model name, and secret file path; never paste the key
into this repository.

The academic workflow and the final acceptance run use Pi's DeepSeek model
`deepseek-v4.1-flash-expires-on-0910` (`provider: deepseek`, Responses API).
`examples/runtime.deepseek.json` is the credential-free template. The key lives
in Pi's own model config (`/home/mansteinl/.pi/agent/models.json`); bridge it
into Anchor's secret file without printing it:

```bash
.venv/bin/python scripts/import_pi_secrets.py
```

```python
from anchor.runtime.config import load_runtime_config
from anchor.runtime.secrets import JsonFileSecretProvider
from anchor.runtime.model_gateway import build_model_gateway

config = load_runtime_config()
profile = config.models[0]
gateway = build_model_gateway(profile, JsonFileSecretProvider(config.secret_file))
response = await gateway.generate(prompt="Return exactly: OK")
print(response.text)
```

This is a side-effect-free model call. It does not claim a Run, write an event,
or execute a tool. A graph Agent reference is executable only after an explicit
Agent capability registry entry and worker adapter are configured. The worker
uses an atomic result sink before a model response can advance a NodeRun, so a
process interruption leaves durable state recoverable instead of implying
completion.

Agent capabilities may opt into bounded retries with `max_retries`. Transient
HTTP 408/429/5xx, connection failures and node timeouts create a new NodeRun
attempt with exponential backoff while preserving the failed attempt as audit
evidence. The default is zero for backwards compatibility; production bundles
may enable bounded retries explicitly. JSON repair retries default to zero;
they are opt-in via `output_retries` and never replay tools or invent evidence.
Healthy runs are not terminated by preset round counts, cumulative tool-call
limits, or wall-clock budgets. The supervisor and adaptive watchdog observe
liveness and verified progress; repeated identical cycles surface diagnostics
instead of silent failure.

The development artifact implementation is content-addressed under a local
directory and returns references such as `artifact://sha256/<digest>`. Artifact
content is integrity-checked on read and can later be moved behind an S3/MinIO
adapter without changing graph or state contracts.

### Running the development worker

Copy `examples/runtime.codex.json` to `.local/runtime.json`, then start the
long-lived worker against a migrated database:

```bash
ANCHOR_DATABASE_URL=sqlite:////home/mansteinl/Anchor/.local/api.sqlite \
  .venv/bin/python -m anchor.runtime.worker_service
```

The worker executes only nodes whose `agent_ref` is present in the profile. It
reconstructs prompts from the durable Task and pinned Graph Version, stores model
output as an artifact, and then performs the NodeRun checkpoint transaction.
The input context used by a successful node is stored as an immutable,
generation-numbered snapshot with a SHA-256 hash. The resolver reuses that
canonical snapshot during replay/recovery; inspect snapshots through
`GET /api/runs/{run_id}/contexts` or the Run Console.

Run deterministic Router, Parallel/Join and Artifact nodes through the separate
control worker. It never calls a model and cannot claim Agent/Tool/Verifier nodes:

```bash
ANCHOR_DATABASE_URL=sqlite:////home/mansteinl/Anchor/.local/api.sqlite \
  .venv/bin/python -m anchor.runtime.control_service
```

Run independent Verifier nodes through the separate verifier worker. It claims only
Verifier nodes, checks predecessor artifacts for SHA-256 integrity, evaluates a
deterministic JMESPath expression or a strict-JSON model verdict, persists a
VerificationRecord with content-addressed evidence, and only a persisted `passed`
completes the node. A model transport failure keeps the node running with its lease:

```bash
ANCHOR_DATABASE_URL=sqlite:////home/mansteinl/Anchor/.local/api.sqlite \
  .venv/bin/python -m anchor.runtime.verifier_service
```

Edge conditions use JMESPath over the source `output` and immutable `inputs`.
Every selected/rejected edge is persisted with evaluator version and evidence;
inspect it through `GET /api/runs/{run_id}/decisions` or the Run Console.

For interval and cron triggers, run the scheduler as a separate process:

```bash
ANCHOR_DATABASE_URL=sqlite:////home/mansteinl/Anchor/.local/api.sqlite \
  .venv/bin/python -m anchor.runtime.scheduler_service
```

The scheduler derives a deterministic occurrence key for every time slot and
enters runs through the same idempotent admission/outbox path as manual events.

For conservative lease monitoring, run the supervisor separately:

```bash
ANCHOR_DATABASE_URL=sqlite:////home/mansteinl/Anchor/.local/api.sqlite \
  .venv/bin/python -m anchor.runtime.supervisor_service
```

`ANCHOR_SUPERVISOR_INTERVAL` controls the observation interval (default 10s) and
`ANCHOR_LEASE_STALE_AFTER` controls the heartbeat age used for assessment
(default 30s). The supervisor only reports stale leases. Agent and deterministic
control recovery require explicit operator confirmation through the API/Web Console;
Tool leases with possible external side effects are never automatically retried.

The supervisor also records durable progress observations and raises a
deduplicated diagnostic request when a completed cycle repeats without verified
progress. Inspect them through `GET /api/runs/{run_id}/progress` and
`GET /api/runs/{run_id}/diagnostics`. A diagnostic is not a failure verdict and
does not stop the run. `ANCHOR_EXPIRE_RUN_BUDGETS` is the only switch that lets
the supervisor enforce an explicit graph time budget, and it defaults to false.

Long-term facts can be persisted through `LocalMemoryStore` in `.jsonl` form.
Records include content hashes and optional Run/NodeRun provenance. Deletion is a
tombstone operation: normal reads omit deleted facts while audit reads retain the
deletion record.

The worker service reads `.local/memory.jsonl` (override with
`ANCHOR_MEMORY_PATH`) and includes non-deleted memories belonging to the current
Run in the reconstructed Agent context. Memory is supplementary evidence; the
Task, Run, Graph Version, events, and artifact references remain canonical.

For a long-lived user-level deployment, copy the included units and enable them:

```bash
ANCHOR_ROOT=/home/mansteinl/Anchor bash scripts/install_user_services.sh
```

The units use `Restart=on-failure`, do not embed credentials, and keep the
database/runtime profile paths explicit. Review the generated unit environment
before enabling them on a production host.

## Development

An online academic literature-review graph is available with a
plan/research/review loop, Crossref/arXiv retrieval, source checks and Markdown
delivery. See [the academic workflow guide](examples/graphs/academic-research.README.md)
for installation and topic submission.

```bash
cd ~/Anchor
python3 -m venv .venv
.venv/bin/pip install -e '.[dev]'
.venv/bin/pytest
```

Development uses the same relational store on SQLite. Production deployments must use PostgreSQL and a
durable workflow receiver; SQLite is not a distributed coordination system.

## Planned boundaries

- `anchor.domain`: durable business models and state transitions
- `anchor.state`: canonical state, event log, idempotency, and operation ledger
- `anchor.runtime`: harness and workflow protocols/adapters
- `anchor.api`: API transport, kept separate from the domain
