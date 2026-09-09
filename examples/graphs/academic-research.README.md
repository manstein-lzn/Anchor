# Academic Literature Review

A reusable online literature-review workflow:

```text
request -> plan -> research -> review -> evidence gate -> report.md
             ^                              |
             +--------- revise -------------+
             +-- human input <-- blocked ---+
```

Planning, research and independent review repeat until the review passes and
deterministic source checks succeed. There is no iteration cap. An identical
completed research cycle or an explicit blocked verdict parks the Run for
human input. Approve the human task with a reason in the Run Console to resume;
reject it to end the Run. Transient model transport failures use a bounded
node-level retry policy: the failed attempt and released lease remain in Run
history, while a fresh attempt is queued without opening downstream edges.
Retries never bypass the Run's total execution budget.

## Install

Use the existing migrated database, API and model configuration:

```bash
.venv/bin/pip install -e '.[dev,api,pydantic-ai,research]'
# Set ANCHOR_DATABASE_URL to the intended database; stop its workers before migration.
.venv/bin/alembic upgrade head
.venv/bin/python scripts/academic_research.py install --model-ref models.codex.local
```

Installation merges the three `agents.academic.*` capabilities and two scholarly
tools into `.local/runtime.json`, backs up the prior configuration, publishes
`academic-research`, registers its manual trigger, and writes a portable bundle
to `.local/academic-research.bundle.json`. The graph remains visible and editable
in the Web workspace. Restart idle Agent and control workers after installation.
Never restart workers that own live leases without reconciling their state.
It also creates `models.academic` from the selected model with streaming
enabled. The full streamed response must finish before any node checkpoint;
partial output or a dropped stream never counts as a completed paper.
Use `install --model-name NAME` to select another model on the same configured
provider without changing the general-purpose model profile.
If a provider emits malformed or empty Responses API streaming events, use its
Chat Completions streaming protocol when supported (`--wire-api
chat_completions`). Use `--no-stream` only when the provider can keep a long
non-streaming request alive; the node still checkpoints only one complete response.
The current schema head (`0017_retention_audit`) preserves released leases referenced by
retrieval operations when an interrupted Agent is recovered. Only one active lease per
node is allowed. Take a database backup before upgrading an existing instance.

## Start a Topic

```bash
.venv/bin/python scripts/academic_research.py run \
  --topic 'Your specific academic research question' \
  --language Chinese \
  --scope 'Field, time range, inclusion criteria, expected depth' \
  --minimum-sources 8 --minimum-reads 3
```

These are evidence requirements, not execution budgets. The returned Run ID can
be observed in the Web Run Console, or with:

```bash
.venv/bin/python scripts/academic_research.py status RUN_UUID --wait
.venv/bin/python scripts/academic_research.py download RUN_UUID
```

The final control node writes
`.local/artifacts/reports/RUN_UUID/report.md` and a SHA-256-addressed copy.
The download command verifies the content hash and creates
`.local/reports/RUN_UUID.md`. Different content is never silently overwritten.
The CLI can also be used against another API with `--api-url` and `--token-file`.

## Evidence and Output

- `scholarly.search`: live Crossref metadata or arXiv metadata/abstracts.
- `scholarly.read`: HTML, public PDF, or plain text from returned source URLs.
- Every call is permission-checked, recorded in the operation ledger, and stored
  as an integrity-checked artifact. Replays reuse existing operation outcomes.
- Only public HTTPS destinations are allowed. DNS is resolved and checked, then
  connections are pinned to the validated address with the original TLS name.
  Redirects are independently checked. No proxy credentials, cookies or model
  credentials are forwarded. Subprocess tools retain their network isolation.
- Downloads and extracted excerpts have per-document resource bounds. The tool
  reports truncation and continuation offsets (`next_offset`, `next_page_start`)
  so later results/discussion sections can be read in further calls.
  Encrypted/scanned PDFs, access walls and unsupported
  documents are not treated as read evidence. No paywalls are bypassed.
- A source must match an actual search result and successful tool operation in
  the same Run. Reading evidence must match a retrieved URL for that source.
  Numeric citations and source coverage are checked before exporting.
- The reference list is generated from retrieved metadata. The paper includes
  Abstract, Introduction, Methods, Literature Review, Discussion, Limitations,
  Conclusion, References and a retrieval-evidence appendix.
- `context_mode: full` explicitly preserves long manuscript and JSON inputs for
  these nodes. Other existing graphs retain their compact context behavior.

This is an assisted narrative review with traceable evidence, not a guarantee
of exhaustive database coverage or publication-ready scholarly correctness.
Crossref records may lack abstracts; arXiv items are labeled as preprints.
The reviewer checks claim support and limitations, while deterministic checks
prove provenance and format. A retrieved page can still be only a landing page
or excerpt; it is not automatically considered full-text scientific evidence.

Capability prompts live in `examples/academic-capabilities.json`; the Graph IR
is `examples/graphs/academic-research.json`. Secrets remain runtime references.

## Bounded Execution and Graph Monitoring

Open a saved workflow in the web workspace. Workflows with runs open directly
on the execution graph; the Execution tab also allows starting a pinned
published version. The graph shows latest attempts, selected edges for those
attempts, elapsed time, tool-operation counts and interrupted heartbeats.
Select a node for earlier attempts, inputs, outputs, tool evidence and recovery.
The editor remains separate from the immutable version used by a run.

Default academic limits are 30 minutes total, 3 review cycles, 3 minutes for
planning, 10 minutes for research and 5 minutes for review. The total budget
includes queueing and human waits and is checked by the supervisor every
10 seconds, including when no worker is executing. These are stopping limits,
not a promise that an approved paper can always be produced within 30 minutes.
Graph metadata configures total time and rounds; capabilities configure node
timeouts. JSON format repair has at most one retry, without replaying tools.
Planner, researcher and reviewer capabilities allow up to two retries for
transient 408/429/5xx or connection/timeout failures, with 15s/45s/120s backoff.
Research retries reuse identical successful tool operations from the durable
ledger (same Run, node and arguments); failed retrievals may be attempted
again. Non-idempotent tools never enter this automatic retry path.
Rejected text and failure context remain in the artifact/context history.

Academic tools are source-paced and host-serialized. Crossref starts at most
one request per second and arXiv at most one every three seconds. A 429 honors
`Retry-After`, retries once, then opens a shared in-process source cooldown so
queued calls fail fast instead of amplifying the limit. The researcher is
bounded to 16 calls per attempt (8 search and 8 read) with at most two calls in
flight; the reviewer is bounded to 6 (3 and 3). Complete evidence remains in
content-addressed artifacts while model-facing excerpts are size bounded.

Before a scholarly review API call, deterministic format and citation checks
run locally. Invalid evidence produces an explicit revision result with
`review_origin: deterministic_preflight`, not a fabricated peer review.
Known academic execution failures end the run instead of leaving a running
lease. Uncertain side-effect operations still require reconciliation.
Transient provider HTTP 504 responses are retried when the capability policy
allows it, then shown as an explicit model failure if the retry budget is
exhausted; they are not misreported as total-budget expiry.

Stop cancels further execution but cannot recall requests already sent to an
external provider. In-flight tool results may still be recorded. Existing
materials remain accessible. Draft downloads are explicitly marked unapproved;
only completed runs expose the approved report download.

To upgrade an unedited installation while preserving its layout and old runs:

```bash
.venv/bin/python scripts/academic_research.py install --model-name gpt-5.6-terra \
  --wire-api chat_completions --upgrade-budgets
```

Restart workers and the API after deployment. The upgraded graph is a new
immutable version; old run pins and evidence are not rewritten.

## Local Acceptance (2026-09-07)

The installed graph completed a two-round online RAG/DPR comparison with two
specified papers and at least one PDF read. Run
`0976d87a-5aa4-428c-8210-b16a6207a19c` completed with 139 monotonically sequenced
events, 27 tool operations (26 successful), clean integrity checks and no
outstanding leases. The verified Markdown is
`.local/reports/0976d87a-5aa4-428c-8210-b16a6207a19c.md`.

This deliberately small example tests the execution path; it is not the user's
full research topic. The first review requested a revision, and the second
round completed after correcting the shared reference-generation contract.
The run also records explicit recovery from provider HTTP 504 and malformed
stream responses. Local academic agents now use `gpt-5.6-terra` on the existing
configured provider; the general-purpose `gpt-6-astra` profile is unchanged.
Human intervention was needed during environment debugging, so this acceptance
does not establish unattended recovery from provider faults.

Validation: 316 backend tests including PostgreSQL passed; eight browser
workflow tests passed. Stream interruption, retrieval access checks, full
context, revision, human waits, reference provenance, Markdown export, and
lease-history migration/recovery have focused regression coverage.
