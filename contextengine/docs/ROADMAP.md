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
- Phase 1, historical-scale real-provider overflow validation, is complete for
  the controlled acceptance probe. In one isolated Pi session, real-provider
  overflow compact committed Bootstrap Checkpoint 0 at `tokensBefore=188,918`,
  then a second historical-scale overflow compact committed Update Checkpoint 1
  at `tokensBefore=191,902`. Both returned complete
  `anchor.compact-receipt.v1` metadata; Checkpoint 1 persisted with the correct
  Checkpoint 0 parent hash, and its receipt matched the Pi session frontier,
  event ID, task ID, version, and content hash. The probe used synthetic Episode
  evidence, so this validates the Anchor/Pi/provider boundary rather than the
  semantic quality of a particular business task.

## Next stage: minimal sufficient cognition

1. Run correction, failure, conflict, recall, restart, fresh-Agent takeover, and
   50-to-100-Update plateau acceptance.
2. Remove the temporary v2 compatibility path after the pre-release State reset
   window closes.

## Remaining host acceptance

- retain the historical-scale overflow probe as a regression record, including
  request size, stop reason, response-validation result, and persistence/receipt
  outcome;
- measure projection and Update latency p50/p95.

## Deferred until measured

- a long-lived Python process instead of one process per state boundary;
- asynchronous Update;
- any scheduler, agent pool, vector index, or custom context budget.
