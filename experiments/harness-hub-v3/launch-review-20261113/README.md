# Dreaming launch review: 2026-11-13

This review uses private fixtures and the working tree. It does not deploy a
release, enable a schedule, change production policy, call an external Provider,
or certify production benefit. Concurrent Bridge work is separately authorized.

Publication boundary: only this historical summary is checked in from the review.
All JSON indexes, native receipts, source inventories and logs named below remain
local originals in `experiments/harness-hub-v3/launch-review-20261113/`, located
under the [local evidence rules](../../../docs/development.md#文档归属).
They are not clone dependencies or acceptance of this checkout.

## Release gate

**Dreaming repair acceptance passed; prior shared-layer blocker cleared on
2026-11-14.** The recheck passed all seven dependency/configuration fence tests,
three Bridge authorization tests, six project authorization tests and normal
Host compilation. The layer classifier, layer allowances and Run-access
allowances are unchanged from the failing check. Authorization now flows from
the application through injected callbacks; project ToolPort assembly belongs
to composition, and ChannelPlatform is shared from a neutral module. Exact
commands and identities are in `recheck-20261114.json`. This clears the four
reported dependency edges, not a new full-release or production acceptance.

The following results and original blocker describe the 2026-11-13 repair:
The final Host Dreaming suite passed 127 tests, with 19 explicitly gated tests
skipped; all three compiled-Hub continuation tests passed separately. The
current-source bounded native Apply/Grade/Withdraw case also passed. Normal
Host compilation, platform binary build and strict Host Clippy passed.
Independent read-only review found no remaining confirmed Dreaming publication
blocker in the reviewed scope. These results do not certify production rollout
or sustained benefit.

The shared Host layer fence originally failed on four unapproved dependency edges in
parallel work: `codex_bridge_tools.rs` (service-to-application),
`goose_acp/pilot_interaction/channel_management.rs` (service-to-application),
and `project_tools.rs` (service-to-application and service-to-adapter). These
paths were not edited by this repair. No allowance was increased. Configuration,
Run-access and Plugin-library fences passed; the latter two were executed
separately because the first combined command stopped at the layer failure.
The 2026-11-14 recheck above confirms those edges are repaired without changing
the fence. The original failure logs remain intact.

Cross-Change publication now binds exact predecessor Execute and result
identities through Intent v2 in the existing Submit history, not wall clocks or
a new store. Full immutable target overlap determines ownership; a successful
successor permanently ends the older observer, even during later withdrawal.
Partial overlap does not release recovery assets needed by other Graphs.
Old v1 roots retain their original bytes and unknown order. Qualified v2 Apply
may explicitly succeed those roots. Automatic recovery cannot merge multiple
unknown v1 roots; only an explicit existing management request, subject to
target CAS and final publisher checks, can do so through Withdraw. The same
guard covers already accepted old Withdraw operations. No migration or blanket
pause of the automatic improvement loop is substituted for this repair.

## Findings and validation scope

- Safe Run/Grade views ignored `exclude_runs`. The Hub now uses one
  namespace-aware predicate for physical and safe-view projection, without
  rewriting frozen snapshots or their identity. Focused Hub suites: 38 passed,
  four intentionally ignored; strict Hub Clippy passed.
- Retired experiment Grades could become `Unknown` after metadata and manifests
  were collected. A real independent Evaluator fixture reproduced four leaked
  experiment Grades plus one legitimate standalone Grade (five versus one).
  Receipt-derived ownership fixes that case. The final evidence subset passed
  12 tests with four separately gated tests skipped. It covers receipt-after-
  metadata ordering, contaminated Unknown/Production replay rejection and the
  valid retained Development boundary. Real Eval writes the receipt and Grades;
  metadata/artifact cleanup is deterministic retirement simulation, not full GC.
- A retained old deployment could claim a successor's ordinary Run and issue
  stale Withdraw. Final causal tests cover equal/backwards clocks, exact
  predecessor digests, cross-surface and full-target overlap, pending/partial
  recovery, durable observer retirement and restart. Independent review also
  caught an automatic multi-v1-root recovery bypass; the corrected automatic
  and explicit-management paths passed the unified Host suite.
- Legacy v1/v2 retained-window identity replay passed one Host test. The
  compiled Hub continuation pair exposed an incomplete old fixture (missing
  admission metadata), then a genuine delta-citation starvation failure: old
  support citations fill the cap before new evidence is selected. Metadata
  was repaired; the expected ten citations were not weakened to five. The Hub
  selection now precedes the cap. A second failing 2001-Grade regression exposed
  Health's upstream 2000-reference cap; internal Signals now uses full support,
  while public Health report bounds stay unchanged.

## Incremental analysis and upgrade checks

The corrected analyzer is v5. Retained v4 windows keep the original Health cap,
cap-before-filter behavior and pagination ordering; fresh captures/advances use
v5. This replaces silent recomputation under the same analyzer identity, not an
additional store or copied analyzer. Remove compatibility after local retained
v4 windows and continuations retire. Legacy safe snapshots whose rows actually
intersect exclusions fail explicitly rather than replaying visibility defects
or changing old offsets. Unrelated exclusions remain replayable.

- Final Facts/Hub suites: 112 passed, seven explicitly gated tests skipped.
  Strict Facts/Hub Clippy passed. These do not include a new scale benchmark.
- Two explicit retained-executable tests passed: full historical support and
  every old page remain identical, and unsafe old Run/Grade snapshots fail on
  capture/page/evidence while nonintersecting exclusions preserve replay.
- Retained v4 executable: `/tmp/anchor-s6-current-profile/anchor-hub`, SHA-256
  `c978886cec5b9e6e7727059d10cadc2431a13f2c6878ba7899017dbfbf51bce4`.
- The strengthened real-v4 continuation fixture persists offset 40 of 48
  distinct signals, reopens that Artifact and resumes through the new Hub.
  Its focused test passed with original manifest/FrozenInput digests and the
  complete old fingerprint set unchanged; v5 continuation passed separately.
  All three unified compiled-Hub tests passed, including the unchanged ten
  pending-evidence assertion. Dependency fence outcomes are recorded above.
- An earlier Host Clippy attempt failed during implementation. Final normal
  Host compilation and strict Host Clippy now pass; the earlier failure remains
  in the evidence archive rather than being relabeled.

Original and intermediate commands and outputs remain in
`initial-log-index.json`, `repair-log-index.json` and `integration-log-index.json`.
`final-log-index.json` records final commands, logs and the layer blocker;
`final-source-identities.json` binds 951 source inputs, and
`final-native-binaries.json` binds 17 executables/bootstrap inputs.
Earlier failures remain intact.

## Bounded final native acceptance

`final-native.json` records the final-source ordinary Graph scenario through
real pinned Goose ACP, Host/GraphRunner, authorized MCP, Bubblewrap, compiled
Hub and independent Eval. Only model transport is a deterministic local HTTP
fixture. The case passed in 125.32 seconds with 80 loopback model requests,
zero external model requests, zero fixture failures and no unused replies.

- Independent evaluation completed and graded all 24 assignments (12 pairs),
  with scope, noninferiority, improvement and holdout gates passing under the
  fixture-only preregistered plan. This is plumbing acceptance, not a production
  effect-size or statistical-power claim.
- Intent v2 Apply succeeded. A separately graded ordinary regression caused
  automatic Withdraw; its predecessor names the exact original Apply Execute
  at position 4 and successful result at position 8, including both digests.
  Exact base bytes were restored; a failed production-observation conclusion
  is the expected trigger in this controlled regression, not a test failure.
- Existing GC/restart and revoked-policy cycle replay checks passed without
  extra model requests. This does not extend the separate receipt-retirement
  simulation into a full experiment-metadata GC race/crash acceptance.
- All captured source hashes remained unchanged through build and native
  verification. Only private executable copies were stripped; Cargo originals
  and pinned Goose/bootstrap inputs were verified unchanged.

The full Plugin/guidance/meta native matrix, external Provider behavior, final
production installation/switch, nightly operation and sustained net benefit
were not reaccepted by this case. Production files, policy, schedules and
permissions were not modified, and no commit was created.

## Existing lifecycle boundary

A separate read-only audit found no supported path that removes both experiment
Run metadata and its manifest after Grade writes but before the EvaluationReceipt
is durable. Such a Change remains `testing`; existing manifest, Run deletion,
Plugin uninstall and Artifact-GC references protect the interval. This is a code
audit, not a crash-injection result. Abandoned `testing` work can retain storage
indefinitely; manual store deletion and corruption are outside this guarantee.

Receipt-derived filtering conservatively protects whole Run IDs, not just one
Grade position. Reusing an old experimental ID for ordinary work does not
declassify its historical Grades. Receipt scans, generation reconciliation and
the expanded internal Health support have not been scale-benchmarked in this
repair round. Earlier CPU/memory measurements do not cover these changes.

Exact commands, logs, source identities and final bounded test outcomes are
recorded in this directory. Scoped formatting and engineering wording checks
passed. Earlier S0-S6/native evidence remains bound to its original sources and
is not retroactively signed.
