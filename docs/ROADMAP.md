# Roadmap

## Implemented

- Pi package manifest and native Extension entry;
- one-time Anchor or Normal Pi selection for new interactive sessions;
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
- focused Normal, Planning, resume, fork, projection, persistence, and Update tests.

## Next stage: minimal sufficient cognition

1. Run correction, failure, conflict, recall, restart, fresh-Agent takeover, and
   50-to-100-Update plateau acceptance.
2. Remove the temporary v2 compatibility path after the pre-release State reset
   window closes.

## Remaining host acceptance

- trigger overflow Update with a real model;
- measure projection and Update latency p50/p95.

## Deferred until measured

- a long-lived Python process instead of one process per state boundary;
- asynchronous Update;
- any scheduler, agent pool, vector index, or custom context budget.
