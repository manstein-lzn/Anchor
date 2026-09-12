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
validates and publishes immutable versions. Graphs without saved drag positions are
auto-laid out in layers with dagre; edges are routed (bezier forward, orthogonal
below the graph when they run backwards) and labelled only on hover or selection. Open <http://127.0.0.1:5173> while the
local web service is running. See `WEB.md` for connection, examples and development.
The editor publishes graph definitions; execution is performed by the separate
worker service. Trigger registration and event ingress are available through the
authenticated API, while richer trigger authoring UI remains under development.

## Workspace validation

`scripts/validate_workspace.sh` is the end-to-end check for the content plane.
It creates a throwaway repository with a bug, asks a real agent to fix it inside
a workspace, and then asserts the whole chain: the node output is an immutable
workspace revision, the audit ledger is complete, the source repository is
untouched, and the fixed code passes its assertions when executed in the
read-only sandbox.

```bash
./scripts/validate_workspace.sh          # single node: fix a bug, run its test
./scripts/validate_workspace_lineage.sh  # two nodes: coder -> reviewer, lineage
./scripts/validate_workspace_parallel.sh # parallel branches, explicit fork + join merge
.venv/bin/python scripts/validate_mcp.py  # MCP client: author, run, observe, gate
```

It requires the dev services and a real model profile. It is the check that
found a silent path-resolution defect that unit tests could not.

## Quality gates

Architecture and implementation quality are enforced, not assumed. A dependency
direction violation, a new silent `except: pass`, a content reference parsed
outside its boundary type, or any increase in the ratcheted lint/type/size
budget fails the gate.

```bash
.venv/bin/pytest -q tests/test_architecture.py
.venv/bin/python scripts/quality_gate.py
```

See `QUALITY_GATES.md` for the rules, the ratchet policy and the Definition of
Done every change must satisfy.

## Agent surface

Anchor is driven by agents as well as by humans. The `anchor` CLI and the planned
MCP server share one typed operation layer (`anchor.client`) over the same
authenticated API, so orchestration, execution, observation and reconciliation
are available to an agent without becoming a privileged backdoor.

```bash
anchor ir                                   # machine-readable Graph IR contract
anchor capabilities                         # available agent/tool/verifier refs
anchor graph install --file graph.json      # validate, save, publish
anchor run start --trigger <id> --objective "..." --idempotency-key k1
anchor run watch <run_id> --timeout 300     # compact digest
anchor waits                                # what needs a human
```

See `AGENT_SURFACE.md` for the operation surface, the agent-vs-human safety
policy and the observation model.

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
`deepseek-flash` (`provider: deepseek`, Responses API). The previous name carried an
`expires-on-0910` suffix and stopped being accepted on 2026-09-10.
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

Side-effect tools are opt-in and gated: `http.post` runs only when the capability
declares `side_effect`, the node names an `owner_agent`, and a completed approval
precedes it. A transport failure after the request was sent becomes
`outcome_unknown`; the node keeps its lease until an operator reconciles the
operation (`POST /api/operations/{id}/reconcile`), which deterministically
completes or fails the node without a second attempt.

Runs can be paused and resumed without touching in-flight work or artifacts:
`POST /api/runs/{id}/pause` fences new claims, `POST /api/runs/{id}/resume`
returns the run to running. The Web trigger panel registers manual, cron,
interval, internal-event and webhook triggers against one immutable version.

Operators curate history without destroying evidence: terminal runs can be
archived (hidden from the default list, still readable by id), and two
runtime-adjustable storage budgets — one for the install, one per graph — are
reported through `GET /api/storage` and set with `PUT /api/storage/budget`. When
a budget is exceeded a rolling sweep evicts the oldest finished runs, skips
anything still running or waiting for a human, garbage-collects orphaned
artifacts and vacuums the database. `GET /api/retention/preview` is a dry run and
`GET /api/retention/audit` records every sweep. Retention never terminates a
running node and does nothing at all while no budget is set.

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

For a long-lived user-level deployment, install the persistent units and let the script
check them:

```bash
ANCHOR_ROOT=/home/mansteinl/Anchor bash scripts/install_user_services.sh
```

Seven units are installed and enabled, and the script refuses to report success unless all
seven are `active`: a unit that cannot start exits with code 2, prints a JSON reason to its
journal, and is reported by systemd as `failed` rather than retrying forever as
`activating`. Every unit declares its database, artifact, workspace, memory and runtime
profile paths explicitly instead of relying on the working directory, and sets
`StartLimitIntervalSec`/`StartLimitBurst` so a crash loop becomes a visible failure.

Each service checks its environment before entering its loop
(`anchor.runtime.preflight`): the database URL is configured with no silent fallback, the
schema is migrated, the runtime profile parses, and the artifact root is writable. A
missing URL and an unmigrated database are reported as themselves, because telling an
operator to run migrations against a path they mistyped sends them the wrong way.

To check that a run survives losing the process that holds its lease:

```bash
.venv/bin/python scripts/validate_service_recovery.py
```

It stops the worker mid-node, waits past the stale threshold, asserts the lease is reported
stale rather than silently reclaimed, recovers it explicitly, and waits for the run to
close — asserting a terminal status, a continuous event sequence, and no duplicated
operation. Evidence is written to `.local/reports/recovery-<run_id>.json`.

Credentials are never embedded: secrets resolve from the file named by `secret_file` in the
runtime profile.

Services run the code that was on disk when they started. After changing anything under
`src/`, restart the units — an acceptance can otherwise pass or fail against a
process running yesterday's code, which is how a fan-out fix appeared not to work.

## Development

An online academic literature-review graph is available: it researches a topic in
convergent rounds, follows the citation graph, writes a reader-facing survey, and
has it independently reviewed before publication. See
[the academic workflow guide](examples/graphs/academic-research.README.md) for
installation and topic submission.

```bash
.venv/bin/python scripts/academic_research.py install --model-ref models.deepseek
.venv/bin/python scripts/academic_research.py run --topic '...' --language Chinese \
  --scope '...' --minimum-sources 24 --minimum-reads 14
.venv/bin/anchor run usage RUN_UUID          # what the run actually cost
```

To capture what a model was actually asked — the literal messages and instructions —
run a worker with `ANCHOR_MODEL_RECORDING=record`. Each call is written as an
immutable projection and attributed to its node attempt by a `model.call` event; the
same run can then be replayed with `ANCHOR_MODEL_RECORDING=replay`, which serves the
recorded answers or fails loudly at the first call that was never recorded. It is
off by default, so production installs nothing. See
[docs/RECORDING_AND_REPLAY.md](docs/RECORDING_AND_REPLAY.md) for the boundary —
replay reproduces an existing run's attempts, it does not re-run a graph.

A representative run — thirteen gather rounds, 59 sources, 39 read in full — took
31 minutes and about 2.2 CNY, and produced a 279-line paper with 59 references.

```bash
cd ~/Anchor
python3 -m venv .venv
.venv/bin/pip install -e '.[dev]'
.venv/bin/pytest
```

Reading order for the design: `README.md` (this file) → `ARCHITECTURE.md` →
`WORKSPACE.md` and `AGENT_SURFACE.md` for the two planes → `DECISIONS.md` for why
→ `docs/limits.md` for every limit and its default → `STATUS.md` for what is not
done yet.

Development uses the same relational store on SQLite. Production deployments must use PostgreSQL and a
durable workflow receiver; SQLite is not a distributed coordination system.

## The cognition layer (design in progress)

Long-running agent work fails in four ways: the context explodes, attention is
diluted, the agent is led astray by whatever is most recent, and repeated
compaction drifts its understanding of the task. `docs/COGNITION_ARCHITECTURE.md`
is a working white paper on addressing all four with state rather than with
summarisation — including the measurements that motivate it (a revision round whose
prompt was **60% literal duplication**; two identical calls producing different
output), the model it builds on, and the three questions it does **not** yet answer.

`contextengine/` holds the archived earlier project those ideas come from, kept
byte-identical. Its `PROVENANCE.md` explains what it is and why it is here.

To have another agent review the thinking rather than the code, hand it
`docs/COGNITION_REVIEW_BRIEF.md` — a reading order, the background that is not
in the documents, the misreadings to avoid, and the questions worth answering.

That thinking has since been reviewed (`docs/COGNITION_ARCHITECTURE_REVIEW.md`) and
measured (`experiments/cognition_takeover/`). The measurements are worth reading before
the design, because they undercut part of it: the handoff criterion does not predict
whether a successor can do the work, the material's shape does not change the artefact,
and the cost question turns out to be about prefix cache stability rather than context
size. Each experiment's design flaws are recorded alongside its results.

## Planned boundaries

- `anchor.domain`: durable business models and state transitions
- `anchor.state`: canonical state, event log, idempotency, and operation ledger
- `anchor.runtime`: harness and workflow protocols/adapters
- `anchor.api`: API transport, kept separate from the domain
