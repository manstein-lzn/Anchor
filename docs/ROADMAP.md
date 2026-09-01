# Roadmap

## Implemented

- Pi package manifest and native Extension entry;
- no-prompt Normal startup with lazy first-compaction Anchor bootstrap;
- lazy first-compaction bootstrap for sessions that did not enter Planning;
- zero-intrusion Normal mode and explicit `/anchor start` activation;
- read-only Grill-style Planning with native question input;
- proposal sealing after the complete assistant turn and explicit review;
- atomic, idempotent Task and Checkpoint 0 creation;
- one Task per Pi session identity, independent of transcript branches;
- RecoveryView context projection without independent trimming;
- serial Update at Pi's compact boundary with stale-version rejection;
- SQLite reduced to Task and immutable Checkpoint truth;
- `anchor.cognition.v3` Situation/Experience/Intent/Knowledge Index;
- stable cognition item IDs and provenance;
- `anchor.transition.v1` coverage and demotion validation;
- exact `checkpoint:<version>:item:<id>` recall through immutable Checkpoints;
- Active `anchor_recall` tool and `/anchor recall` command;
- bounded Contract-plus-cognition Context projection;
- request-local schema-constrained Bootstrap and Update submission;
- exactly-one-call, closed-schema, malformed-submission, and persistence-boundary tests;
- focused Normal, Planning, resume, fork, projection, persistence, and Update tests.

## Phase assessment

- Phase 2, structured Bootstrap/Update submission, is implemented and covered
  by deterministic tests. This includes closed schemas, mandatory tool choice,
  exactly-one-call validation, and no State write for rejected submissions.
- Phase 1, historical-scale real-provider overflow validation, is partially
  complete. A controlled 138,137-input-token real-provider Update completed at
  the provider boundary but was rejected during `response-validation` because
  the model included an invalid Transition Certificate item; no State write was
  attempted. This proves the failure path and observability, but successful
  historical-scale Update continuation remains unproven.

## Next stage: minimal sufficient cognition

1. Run correction, failure, conflict, recall, restart, fresh-Agent takeover, and
   50-to-100-Update plateau acceptance.
2. Remove the temporary v2 compatibility path after the pre-release State reset
   window closes.

## Remaining host acceptance

- repeat the historical-scale Update after correcting the provider-facing
  Transition Certificate behavior, and record request size, stop reason,
  response-validation result, and persistence/receipt outcome for a successful
  continuation;
- measure projection and Update latency p50/p95.

## Deferred until measured

- a long-lived Python process instead of one process per state boundary;
- asynchronous Update;
- any scheduler, agent pool, vector index, or custom context budget.
