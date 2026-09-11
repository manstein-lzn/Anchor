# Academic Literature Review

A reusable online literature-review workflow. It researches a topic in rounds,
writes a reader-facing survey, and has the result independently reviewed:

```text
request -> plan -> coverage -> gather -+-> write -> review -> check -+-> report.md
                    ↑                  |                         |
                    |                  +-- continue              |
                    +--------------------------------------+    |
                            check -+-> plan (evidence gap) --+    |
                                   +-> needs_input (blocked) ------+
```

## Roles

| Node | Type | What it owns |
|---|---|---|
| `plan` | agent | research questions, search strings, inclusion criteria |
| `coverage` | loop (`academic.coverage_gate`) | merges rounds, drops unverifiable sources, decides saturation |
| `gather` | agent + tools | one round: searches, citation lookups, batch reads, terse structured notes |
| `write` | agent | the reader-facing manuscript; it has no tools |
| `review` | agent + tools | substance **and** craft, and where a fix belongs |
| `check` | loop (`academic.review_gate`) | deterministic evidence and craft checks, then routing |
| `needs_input` | human task | an operator decision after a `blocked` verdict |
| `report` | artifact (`academic.report`) | writes the paper plus its generated references |

## Why gathering is a campaign

One search-and-read turn cannot cover a field. `gather` reports only what a round
added; `coverage` merges rounds into one ledger and decides whether to continue.
Research ends on **convergence**, not on a round count: the gatherer reports
saturation, or a round adds nothing, or three consecutive rounds each add less
than 5% of the corpus. An evidence gap found in review returns to planning, which
re-enters the gate and resumes from the accumulated ledger (ADR-042).

## What the ledger is

Each source carries its retrieved id, the search or citation lookup that produced
it, and optionally the read that studied it. `runtime/evidence.py` is the single
source policy: the gate uses it to decide admission and the review validator uses
it to decide citation, so a source cannot be admitted and then be uncitable. A
source whose provenance does not verify is removed and reported as
`dropped_unverifiable`; citation numbers stay stable when that happens, leaving a
gap.

Every result number must rest on at least one source read in full. A number whose
source was only read at abstract level is rejected: an abstract reports a figure
without the baseline, benchmark or measurement detail that makes it checkable.

## What the paper must be

The manuscript follows the skeleton survey papers have converged on — Abstract,
Introduction, Survey Methodology, a thematic body of two to six sections named by
the field's own axes, optional Comparative Analysis and Open Problems, Threats to
Validity, Conclusion — and the deterministic gates enforce it. The body may not
contain evidence-level labels, process language, content hashes, paragraphs over
1200 characters or a Methodology section over 2500 characters. References and the
retrieval-evidence record are generated: the first from verified metadata, the
second exported beside the paper as `provenance.md`, never inside it (ADR-040,
ADR-041).

## How review ends

The reviewer reports each finding as major or minor and says whether the fix
belongs to the writer (`target: manuscript`) or needs more evidence
(`target: evidence`). The loop returns to whichever of those is deficient, so it
does not redo a search to fix a sentence. When the reviewer reports only minor
findings and the deterministic checks pass, the gate approves and records
`approved_with` — "accept with minor revisions", so a thorough reviewer cannot
withhold approval from a paper that already meets the stated bar (ADR-043). A
defect that survives three identical revisions parks the run for a human instead
of spending the budget on an unchanged failure.

## Install

Use the existing migrated database, API and model configuration:

```bash
.venv/bin/pip install -e '.[dev,api,pydantic-ai,research]'
# Set ANCHOR_DATABASE_URL to the intended database; stop its workers before migration.
.venv/bin/alembic upgrade head
.venv/bin/python scripts/academic_research.py install --model-ref models.deepseek
```

Installation merges the academic capabilities and scholarly tools into
`.local/runtime.json`, backs up the prior configuration, publishes
`academic-research`, registers its manual trigger, and writes a portable bundle
to `.local/academic-research.bundle.json`. It also creates the dedicated
`models.academic` profile, inheriting the source profile's `max_tokens` — a
reasoning model spends part of that budget on thinking, and without an explicit
value a structured response can be truncated mid-string. Restart idle agent and
control workers afterwards; never restart workers that own live leases without
reconciling their state.

## Run a topic

```bash
.venv/bin/python scripts/academic_research.py run \
  --topic 'Your specific research question' \
  --language Chinese \
  --scope 'Field, time range, inclusion criteria, expected depth' \
  --minimum-sources 24 --minimum-reads 14

.venv/bin/python scripts/academic_research.py status RUN_UUID --wait
.venv/bin/python scripts/academic_research.py download RUN_UUID
```

`minimum_sources` and `minimum_reads` are evidence requirements, not execution
budgets. The final control node writes
`.local/artifacts/reports/RUN_UUID/report.md`; `download` verifies the content
hash and creates `.local/reports/RUN_UUID.md`.

## Watch the cost

A tool loop re-sends its whole conversation on every model call, so a run's spend
is turns times context. `anchor run usage RUN_UUID` reports gross, cached and
billed input per node, and the number to budget against is the billed one:

```bash
.venv/bin/anchor run usage RUN_UUID
```

A representative campaign — thirteen gather rounds, 59 sources, 39 read in full,
24 pairs of contradictory evidence — took 31 minutes and about 2.2 CNY.

## Tools

- `scholarly.search` — Crossref or arXiv metadata and abstracts
- `scholarly.citations` — follow the citation graph forwards (`cited_by`) or
  backwards (`cites`) from a seed paper
- `scholarly.read_many` — read up to eight documents in one call, one bounded
  excerpt each, fetched in parallel while still pacing each source
- `scholarly.read` — one document, or a later page of one

Batching exists to keep the model's turn count down. Every call is
permission-checked, recorded in the operation ledger and stored as an
integrity-checked artifact; identical replays reuse the recorded outcome. Only
public HTTPS destinations are allowed, DNS is resolved and pinned, and redirects
are checked independently.

## Limits and honest boundaries

This is an assisted narrative review with traceable evidence, not a guarantee of
exhaustive coverage or publication-ready correctness. Abstracts and metadata
cannot support detailed experimental numbers; arXiv items are preprints; a
retrieved page may be only a landing page. `Threats to Validity` states these
limits in the paper itself. See `docs/limits.md` for the full limit inventory and
`DECISIONS.md` (ADR-040 … ADR-043) for the reasoning behind the current shape.
