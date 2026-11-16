# Creator-Owned Graph Management: Local Rollout

2026-11-16 11:48 (Asia/Shanghai): the local Host and Web endpoints on 8077/5173
now use a complete format-5 release with creator-owned Graph deletion. Existing
creation authorization covers management; Session/Run ownership boundaries,
current/frozen grant intersection and deletion protections remain unchanged.
No production Graph was used as a deletion test.

Publication boundary: only this summary and `result.json` are checked in.
Raw logs, native receipts, backups and per-resource production inventories stay
local under the [local evidence rules](../../../docs/development.md#文档归属).
Historical paths and hashes do not certify this checkout or require those files.

## Scope And Identity

The release starts from the exact frozen source of the deployed Dreaming-retirement
release. It adds only the child-management delta and upstream commits `c4a440c`
(shared history watcher) and `b74c3c9` (Run-detail loading). Other local WIP and
the separate upstream Plugin-install/scheduler lock changes are excluded.

- Archive SHA256: `e44e7ed8d9c1eed0d46044fa2db26b18c85726ab22a216fb12f644be6fd6f041`.
- Installed Host SHA256: `6f667049e69a440f562701587fe8a7acf2c0415d34eeb4ee0da47e4d5cb38371`.
- The existing builder and fresh installer validate the complete format-5 archive;
  no manifest is hand-edited and no Host is placed in an older release.
- Only the `graph-tasks` Skill changes in the installed Library. Fixed Goose,
  Gateway, relay/helper, other Plugin resources, guidance and evaluators retain
  their existing bytes. Host/Hub/Eval/launcher and Web are built from the frozen candidate.

## Verification

| Scope | Result |
| --- | --- |
| Candidate Host unit tests, four architecture fences and four existing integration targets | 1,209 passed, 0 failed, 3 default ignored; 9 targets |
| Exact archive Host and matching tools, fixed Goose and deterministic local Provider | 4 explicitly selected native tests passed; child lifecycle including deletion, observed tool receipt, unknown-effect recovery and persistent-assistant restart |
| Source-free installed launcher | Three startups passed; two empty-model starts and static validation with private catalog/Library/model copies; no Run, schedule or channel state copied |
| Web | 407 tests in 51 files, build and one mocked slow-detail/retry browser case passed; existing large-chunk warning |
| Host/Kernel strict Clippy | All features and targets passed with warnings treated as errors |
| Formatting | All changed Rust files passed; whole-workspace check still reports four unchanged formatting exceptions inherited from the deployed frozen source |
| Kernel after the old Host releases the quota, before changing deployment paths or Skill | 7 library/history tests and 2 watch-budget tests passed, serially with isolated tmpfs fixtures |
| Production static checks | All 38 Graphs readable; 37 ordinary Graphs admitted, retired Dreaming rejected with 422; all 13 Plugins available and valid |
| HTTP/Web | Health and readiness pass; HTML and entry JS/CSS on both 8077 and 5173 match the new release; Dreaming API remains 404 |

The initial source-free assertion incorrectly expected the retained retired Graph
to pass; it was corrected to require rejection, without product changes. The
first native invocation lacked the fixture's flattened tool layout; the successful
rerun uses exact archive bytes in that layout. The initial patch exclusion pattern
was corrected before source files changed. Failed-attempt logs remain available.

## Production Preservation

The old Host was stopped gracefully only after checking all 11 active assistants
were waiting for input and no Pilot Turn was running. Lifecycle/writer locks
protect a cold backup before the Kernel gate and Skill upgrade. The old complete
release remains intact; no state snapshot was restored or automatically downgraded.

- All 11 assistants resume their original Runs; 13 Sessions and 214 Run IDs remain.
- All 2,796 prior Session events and 31,432 Turn events remain; only 11 normal
  in-place assistant-resume events are appended.
- Six enabled channels authenticate again; all 113 existing send receipts remain
  confirmed under read-only queries, with zero new sends or historical resends.
- All 14 retained plans, including 13 enabled plans, and subscriptions are unchanged.
  Enabled targets pass static admission; no production business Graph is rerun.
- Graph catalog, credentials, model settings, grants, Goose histories and retired
  automation history are preserved. Only three deployment resource paths change.
- Bridge and Web proxy are not restarted. No permissions are widened and no
  Dreaming plan or old experiment is restored.

The old Host held 116 inotify descriptors and a fresh `inotify_init1` failed with
errno 24. The new Host holds one descriptor, fresh allocation succeeds and the
system limit remains 128. This is a file-watcher resource fix, not a Graph-count limit.

## Evidence And Limits

The public [result](result.json) contains counts and artifact identities only.
Private cold backup, source hashes, logs, native receipts and preservation checks
are retained under `.local/child-graph-management-rollout-20261116/`. Its
`prepare-release.py`, `probe.py`, `deploy.py` and `post-check.py` record the actual
one-shot operations, not a new deployment system or replayable upgrade contract.

No real-model business execution, external-channel message delivery, future
scheduled business cycle, fresh DEB/RPM transaction, full workspace suite or
unrelated Plugin-install/scheduler concurrency acceptance is added by this rollout.
Static admission and channel authentication do not establish business quality.
