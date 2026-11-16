# Harness hub v3: deterministic acceptance

These artifacts record private deterministic fixtures, not production accuracy or
model-quality improvement. No production state, policy, night schedule or live
provider was changed. Tests use the actual Kernel writers and Hub implementation;
safe Run/Grade benchmark inputs are content-free DTOs with owner-selected labels.
An independently owned Host scale fixture below also exercises the real exporter.

Publication boundary: only selected summaries and compact JSON results are
checked in. Bare log filenames and omitted JSON names below refer to local
originals, including `native.json`, not public dependencies; locate them under
the [local evidence rules](../../docs/development.md#文档归属). Original JSON
indexes, path fields and hashes stay unchanged and do not certify this checkout.

## Integrated closeout

The subsequent [launch review](launch-review-20261113/README.md) records new
isolation, delta-citation and deployment-generation findings. Its release gate
is still open; the original S0-S6 acceptance below does not clear those findings.

[`verification.json`](verification.json) indexes the final checks, copied original
logs, retained failures and remaining boundaries. The final Facts/Hub suite has
108 passes / 5 ignored (low-frequency runs are recorded separately); the Host
Dreaming subset has 134 passes / 18 ignored. Workspace all-target compilation,
relevant strict Clippy and the Run-access/Grade-writer fences pass. Web has 409
unit tests plus two desktop/mobile browser fixtures and a successful build.
Distribution interfaces have 88 passes / two unrun pinned-input packaging smokes;
Eval has 15 passes after the owner-writing fixture repair. This is not a full
workspace test or current three-format installation/production rollout claim.

[`ten-input-stable.json`](ten-input-stable.json) preserves the compact original
Host/Runner/Goose evidence: ten distinct inputs/request contexts share one stable
configuration. native.json (local-only, not checked in: `experiments/harness-hub-v3/native.json`) binds four frozen native cases to
their exact private executables: Graph/guidance/Plugin Apply, independently graded
ordinary regression and automatic Withdraw, plus one meta boundary, totaling 401
loopback requests. Meta covers only 1/48 assignments; the full receipt correctly
remains inconclusive. Two later window-status changes were semantic, not formatting.
The separate `current_source_followup` rebuilds Host/Hub/Eval and reruns only the
ordinary Graph full loop: 80 loopback requests, 24/24 experiment Grades, accepted
v3 `not_granted` states, exact baseline restored, no source drift. It does not
retroactively certify the frozen guidance/Plugin/meta cases. All requests are local.

S0-S6 implementation and local deterministic acceptance are closed; the original
[plan](../../docs/archive/harness-hub-2026-11-13/development-plan.md) is archived.
Current contract and remaining S7/performance/rollout work belong to the
[Hub contract](../../docs/harness-data-hub.md) and
[unified ledger](../../docs/pilot-development-plan.md).

## Scope and independently specified answers

`rust/anchor-hub/tests/hub_acceptance.rs` adds:

- A default 20-Run, three-cycle test. The owner labels two initial Grades, one
  cross-owner conflict, two regrades and two late Grades. Expected initial matched
  Grades = 1; later matched Grades = 3; conflicts = 1; late joins = 3; idle new
  samples = 0. The assertions passed. The conflicting owner uses a different task,
  so the conflict remains present even when one owner's boolean result changes.
- An ignored 100 / 1,000 / 10,000 Graph benchmark. The physical input is the same
  10,000 x 32 + 2,500 = 322,500 observation records as the saved pre-S4/S5 facts-only
  benchmark. Each Graph has a distinct stable configuration. It adds safe Run
  views, initially 10% graded coverage, another 10% late Grades, regrading of the
  first cohort, 100 persistent Run updates, and one conflicting Grade owner.
  The persistent Run update repeats the same worker#1 result identity at a new
  position; this DTO load fixture is not a second GraphRunner invocation. Actual
  worker#2 cross-cycle bucketing is covered by the named `longitudinal.rs` test
  `three_cycles_join_late_regrade_without_cloning_persistent_quality_or_reopening_old_work`,
  not established by the 10k result. The final focused rerun passed all three
  longitudinal tests plus the default owner-labelled test (`final-small.log`). All views are preserved through four captures:
  cold, idle, updates, one long history append. At 100/1k, the update phase updates only views (zero new physical
  fact lines); the 100 / 1 appended physical lines apply at 10k. This prevents
  smaller scale probes from mutating the physical baseline for the final 10k run.
- All three source classes are present through four registered sources (the
  fourth is the conflicting Grade owner). Safe source paths deliberately do not
  exist, ensuring Hub uses the supplied closed views instead of raw owner roots.
- The model summary is bounded by 256 KiB; signals/exemplars, coordinates/evidence,
  and longitudinal strata/configuration exposures retain their fixed count caps.
  Counts and omitted totals can grow; first-page content is not a complete ledger.
  This fixture's maximum summary was 15,838 bytes. It does not prove the same bound
  for arbitrary unbounded task labels or every possible analyzer output.

The before/after scorecard in `scorecard.json` links named passing tests rather
than pretending assertions of the original defects are new acceptance. The old
private audit's actual output is preserved in `audit-before.json`; its authoring
source remains at `.local/hub-audit-20261113/src/main.rs`, not required to run the
new regressions. Historical baseline measurements were captured before S4/S5 and
were not recomputed by the new code.

## Recorded 10k cost

| Phase | Before facts-only process ms | New multi-source process ms | Delta parsed | Separate support parsed | Child peak RSS KiB |
| --- | ---: | ---: | ---: | ---: | ---: |
| Cold | 11,343 | 18,336 | 322,500 | 0 | 2,569,584 |
| Idle | 2,339 | 51,948 | 0 | 322,500 | 2,647,216 |
| 100 new | 2,467 | 62,302 | 100 | 322,500 | 2,660,260 |
| Long history, one new | 2,471 | 39,729 | 1 | 322,600 | 2,666,584 |

**This is added-capability cost, not a same-workload speedup claim.** The physical
fact records/bytes match the baseline in all four phases, but safe views and full
history association add work. Idle delta IO is 0 records / 0 history bytes; its
support reconstruction still reads 322,500 records / 354,883,936 history bytes.
Total IO and runtime are not O(new records). The roughly 2.54 GiB maximum child
RSS and repeated full-prefix reconstruction are concrete optimization boundaries.
Subprocess latency varies with machine load and in-process measurement order;
these are single observations, not confidence intervals or a throughput SLA.

The largest request is 49,714,401 bytes, largest response 38,168,812 bytes, and
largest process duration 62,302 ms: below existing 64 MiB and 120 s boundaries for
this **sparse-grade fixture**. All-grade or longer histories may hit the bound;
no claim that arbitrary 10k workloads fit is made. The DTO view, manifest and
request contain full support projections; they are not the compact model input.

- `multisource-scale.log` is the original complete passing run (2 tests, 513.54 s).
- `multisource-scale.json` preserves all printed measurements; byte counts, direct
  load/project/analyze time, response serialization, request serialization,
  process wall time, response deserialization and child-only peak RSS are separate.
- `baseline-pre-s4s5.log` and `baseline-comparison.json` preserve the old facts-only
  timing and like-for-like physical delta counters. The baseline did not measure
  child RSS, so no before/after memory improvement is claimed.
- `/usr/bin/time -f ...%M` wraps only the `anchor-hub window` subprocess. Parent
  fixture generation and parent deserialization memory are not Hub RSS. Fixture
  generation took 152,518 ms; its peak RSS was not measured.
- Pure query-only time is not exposed by `capture`; it is included in the direct
  load/project/analyze time. The separate phase tests below do not pretend to
  decompose that capture run additively.
- `host-safe-export.log` is a **different, small Host owner fixture**, not the 10k
  benchmark: 4 Runs + 4 Grades, 118,339 us export, 8,629 serialized bytes, 3 Run/3
  Grade rows after protected filtering, isolated RSS not measured. The `safe_`
  filter ran 4 tests; two are unrelated safety tests, so only the two fully named
  Dreaming tests in `scorecard.json` establish safe-view behavior.

## Independently measured Host owner export

The architect separately ran the real Host Run/Grade exporter at 100 / 1,000 /
10,000 Graphs; see [`host-scale.json`](host-scale.json) and
host-scale.log (local-only, not checked in: `experiments/harness-hub-v3/host-scale.log`), including exact source/test-binary identity.
Each size runs in a fresh child process, excluding parent fixture generation;
the OS page cache was not dropped. No Provider, Hub or sandbox process is invoked.

| Graphs / Runs / Grades | Export ms | View bytes | Window request bytes | Child peak RSS KiB |
| --- | ---: | ---: | ---: | ---: |
| 100 | 36.162 | 258,663 | 263,171 | 19,376 |
| 1,000 | 363.670 | 2,577,243 | 2,615,951 | 36,664 |
| 10,000 | 4,127.779 | 25,763,043 | 26,143,751 | 227,300 |

At 10k, view serialization took 34.842 ms, request serialization 34.869 ms and
the whole child took 4,298 ms. Peak RSS covers exporter, validation and JSON
serialization, not exporter-only net allocation. Each Run and Grade has one
history record with explicit per-resource grants; outside-scope malformed owner
metadata was not opened and payload sentinels were absent. This full-grade Host
fixture is not the sparse-grade multisource Hub fixture, so their bytes/times
are not an additive end-to-end profile. Physical IO and cold-cache cost remain
unmeasured.

## Separately measured projection, query and refresh resources

Two additional ignored tests ran in **separate filtered test processes** against
a private copy of the completed physical fixture: 322,500 records, 20,008
resources, 354,883,936 history bytes read. Neither process generates its fixture.
`isolated-phases.log`, `refresh-resources.log` and `isolated-resources.json` retain
their measurements and scopes.

| Phase | Time ms | Resident baseline KiB | Phase peak RSS KiB | Scope |
| --- | ---: | ---: | ---: | --- |
| Preload owner records | 3,586 | Not separately sampled | 3,828,900 | Real DataRoot IO and cached records; not projection |
| Project cached records | 1,272 | 3,827,792 | 3,921,528 | Production Projector, includes required clones; no filesystem read |
| Project safe DTO views | 68 | 3,920,656 | 3,923,820 | Not Host export |
| Query + Signals + longitudinal | 4,984 | 3,923,820 | 4,076,796 | 333,501 retained rows, 999 matched Grades, one conflict |
| Serialize model summary | 0 | 3,933,756 | 3,933,756 | 15,834 bytes; below integer-ms resolution, not zero cost |
| Facts-only Hub cold refresh | 4,363 | 1,796 | 2,385,096 | Production Hub facade, physical read + projection + staging |
| Facts-only Hub idle refresh | 2,332 | 2,385,096 | 2,853,244 | Zero added rows; transactional table/cache clone retained |

Memory comes from Linux `VmRSS`/`VmHWM`, with a per-process peak reset through
`/proc/self/clear_refs` before each phase. The cached projection/query process
retains owner inputs, the table and allocator pages; these are **absolute phase
peaks including the reported resident baseline**, not isolated net allocations.
The independent Hub process has no preloaded owner-record cache, explaining why
its baseline differs. The Hub facade does not expose physical IO metrics and
has no safe-view injection API; its refresh measurements are facts-only. Those
limitations must not be filled in from another workload's numbers.

## Commands and build identity

Working tree was dirty across concurrent workflows. `build-identity.txt` records
HEAD plus explicit measured source/binary hashes and compiler/profile, rather than
claiming HEAD alone reproduces the run. After the main benchmark, the owner made
lint-only `is_multiple_of` and `map_or` changes; test formatting was likewise
lint-only. Later parent changes (including more precise applicability status)
are not retroactively assigned these measurement hashes. Re-run on the final
release binary before claiming production performance.

```sh
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target XDG_RUNTIME_DIR=/dev/shm/anchor-bench-tmp scripts/test.sh -p anchor-hub --test hub_acceptance ten_thousand_graph_multisource_multicycle_costs_are_measured -- --ignored --nocapture --test-threads=1
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target XDG_RUNTIME_DIR=/dev/shm/anchor-bench-tmp scripts/test.sh -p anchor-hub --test hub_acceptance labelled_multisource_three_cycle_fixture_has_exact_late_grade_and_conflict_counts -- --nocapture
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target XDG_RUNTIME_DIR=/dev/shm/anchor-bench-tmp scripts/test.sh -p anchor-hub --test hub -- --nocapture --test-threads=1
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target XDG_RUNTIME_DIR=/dev/shm/anchor-bench-tmp scripts/test.sh -p anchor-hub --test window facts_only_discovery_discloses_missing_quality -- --nocapture
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-hub --test hub_acceptance -- -D warnings
```

`scorecard-regressions.log`: 38 passed. `scorecard-missing-quality.log`: 1 passed.
`final-small.log`: 4 passed, 1 intentionally ignored scale test (already executed
above); `clippy.log`: strict targeted Clippy passed after the owning agent repaired two
new core lints; the initial failure remains `/tmp/anchor-hub-acceptance-clippy.log`.
The later isolated tests each passed one test (11.30 s and 6.90 s); the source and
test-binary hashes are recorded separately in `build-identity.txt`.
`final-stage-validation.log` records the final strict Clippy and rustfmt checks,
default test run (one passed, three intentionally ignored tests already measured
individually), exact-path diff check, tracked wording check and owned-artifact
wording/JSON validation.
Native closed-loop results are owned separately in `native.json`; this report
does not infer their result. Full workspace, release, Web and live rollout
acceptance are not established by this scale artifact.

## Explicit remaining measurement boundaries

- Host exporter time/bytes and child peak RSS are measured in the independent
  fixture above; physical IO, cold-cache cost and exporter-only allocation are not.
- Projection-only, query-only and facts-only warm-refresh phase peaks are measured
  above, but **net isolated allocation** and Hub facade physical IO are not.
- Complete capture load/project/analyze time remains a combined measurement;
  fixture-generation RSS is still not measured.
- No live provider, channel, production shadow/cutover or sustained net quality
  gain has been measured by this task.
- Pilot/Op/mid-attempt crash collection remains the separate S7 coverage boundary.

## Original benchmark contract mismatch (preserved failure)

The original ignored `window.rs` 10k benchmark was also run **unchanged** with its
old 8 KiB whole-summary assertion. It failed at `window.rs:1447` after 197.33 s,
before printing the first 100-Graph measurement: the v3 longitudinal fields now
count toward a previously v2-shaped 8 KiB limit. `original-post-s45-failed.log`
preserves that failure; it is not silently counted as passing or a 120 s process
failure. The fixture-generation phase itself is outside the process deadline.

```sh
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target XDG_RUNTIME_DIR=/dev/shm/anchor-bench-tmp scripts/test.sh -p anchor-hub --test window ten_thousand_graph_histories_cold_idle_delta_and_long_history_reads_are_measured -- --ignored --nocapture --test-threads=1
```

The architect updated the test contract, and a **contract-adjusted rerun passed**
(one test, 297.81 s including fixture generation). It keeps 8 KiB for the old core
after removing only the new longitudinal field, checks longitudinal against
16 KiB and the whole model summary against 256 KiB, and verifies the same
longitudinal field shape/caps at 100/1k/10k. This is not an unchanged-original
pass. `original-contract-adjusted.log` and `.json` preserve the rerun: summaries
15,077 / 15,090 / 15,103 bytes; 10k cold / idle / 100-new / one-new wall times
13,349 / 39,694 / 41,674 / 43,014 ms. Physical delta parsed counts remain
322,500 / 0 / 100 / 1. This benchmark is facts-only; it does not replace the
multisource acceptance above or establish child-only RSS.

## Manual phase-test fixture setup

The phase tests deliberately do not regenerate 10k facts in each process. During
the original benchmark, after fixture generation completes and before cleanup,
copy its private `host/state/kernel-facts` directory into a new directory named
`/dev/shm/anchor-bench-tmp/stage.XXXXXX/kernel-facts` (use `mktemp -d` for the
parent). Write only that parent path to `/tmp/anchor-s6-stage-fixture-path`. The
tests enforce this private location and print the actual copied record count.
The recorded copy was completed at 15:56:26 +08:00 on 2026-11-13, before any
physical append, with exactly 322,500 records. Do not point it at production data.

Run each filter in its own process; a broad `--include-ignored` now also selects
these manual tests and requires that explicit setup:

```sh
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target XDG_RUNTIME_DIR=/dev/shm/anchor-bench-tmp scripts/test.sh -p anchor-hub --test hub_acceptance cached_projection_and_query_phases_are_measured_separately -- --ignored --nocapture --test-threads=1
CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target XDG_RUNTIME_DIR=/dev/shm/anchor-bench-tmp scripts/test.sh -p anchor-hub --test hub_acceptance facts_only_hub_cold_and_warm_refresh_resources_are_measured -- --ignored --nocapture --test-threads=1
```

Formatting (`rustfmt --check` for the new test), exact-path `git diff --check`,
`scripts/check-sensitive-wording.sh` on tracked text, and a separate scan of the
new untracked code/artifacts passed. No commit was created.
