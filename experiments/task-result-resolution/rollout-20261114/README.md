# Explicit Task Result Resolution: Production Rollout

The matching Host, Feishu/WeCom Gateways and installed Feishu/graph-tasks Skills
are deployed on the existing production root. A task-result summary alone no
longer consumes the result. Actual channel sends require Gateway confirmations;
intentional silence requires an explicit API action and reason. This does not
prove that an Agent's choice of silence satisfies the business request.

Publication boundary: only this summary, `acceptance.json` and
`history-integrity.json` are checked in. Raw logs, full source/deployment/Gateway
inventories, native histories and one-shot operator files remain local originals;
see the [local evidence rules](../../../docs/development.md#文档归属).
Unlinked evidence locators do not promise public files or validate this checkout.

## Delivered And Preserved

- Archive SHA-256:
  `a40c6fa8b8e6a1ba8c8a26b7065df18c5fbd23931b96ebb4efd0f1fd6e313cb3`.
- Running Host SHA-256:
  `8e20afe07d80ab35b2e724ee1aeb455c26f7329231fe643f7221755c173be2b0`.
- The frozen source contains 1,522 files and 34 explicit deltas from the
  previously deployed source. Unrelated parallel work is excluded. Two deltas
  update existing native Feishu/Codex test fixtures to call the resolution API;
  the Codex scenario was compiled, not executed in this rollout.
- Exactly six runtime files change: Host, platform launcher, both Gateway
  binaries, Feishu Skill and graph-tasks Skill. The launcher embeds the matching
  public resources. Web, Goose, other Plugins, factory Graph, guidance,
  Dreaming evaluator and bootstrap bytes remain unchanged.
- A graceful idle cutover and cold backup precede the explicit Library
  `upgrade-system` operations for `feishu`, `wecom` and `graph-tasks`. Only four
  installed Library files change: both Gateway binaries and the two Skills.
  The existing UI and independent Codex Bridge are not restarted.
- All 11 resident Runs resume in place at `wait_input`; all 13 Sessions and
  191 Runs remain. Credentials, model configuration, Catalog, channel grants,
  schedules and subscriptions are unchanged. Dreaming stays ready at control
  v11 with its nightly 03:00 schedule enabled; no Dreaming cycle is started.
- All 2,441 old Session events and 31,356 old Turn events remain exact. Startup
  appends only 11 ordinary in-place auto-resume facts. No new user, task or
  resolution event is synthesized. All original Gateway delivery rows remain
  exact; no historical result or message is replayed or migrated.

The old immutable release and cold backup are retained privately. No stale
state snapshot is restored. Code rollback is not exercised and must not erase
new accepted facts or blindly downgrade a running business turn.

## Executed Acceptance

| Check | Actual Result And Boundary |
| --- | --- |
| Isolated offline build | Host, both Gateways, Distribution/platform and Library compile with the lockfile in a fresh target; optimized dev profile, private stripped copies only |
| Package install/inventory | Fresh immutable install; exact six runtime changes and three Plugin digest updates verified |
| Source-free startup/restart | Two launches from unrelated cwd/home, no model credentials, identical factory seed/disabled fresh schedule/Plugins, exact compiled Web assets |
| Channel tools | 19 passed on the frozen candidate, including missing/unknown/partial receipts, pre-claim send intent, explicit silence, interruption and bounded large selections |
| Pre-completion guard | 1 passed; unresolved completion remains correctable in the existing native loop |
| Architecture fences | Four targets, 7 passed; no fence allowance increased |
| Native task inputs | 5 passed with the real pinned Goose, ACP/MCP and sandbox; model and private transport are deterministic local fixtures |
| Native Feishu schedule | 1 passed with the actual local Feishu Gateway, Scheduler and producer; text/file/introduction sends precede verified resolution in the original Session |
| Ordinary Feishu replies | Private and authorized group roundtrips: 2 passed with local platform/model endpoints; original Run/native history retained without duplicate replies |
| Production recovery | Original Run/Session/channel bindings, schedules/subscriptions, Dreaming and Bridge preserved; all six channels authenticated |
| Live Gateway protocol | Authenticated read-only queries against all six actual production Gateway processes; 79 existing confirmed send facts returned, zero new sends and unchanged ledgers |

These are 35 candidate tests plus two launcher starts, not a new full-workspace
suite or real-provider business acceptance. The preceding
[source-phase report](../README.md#verification) retains its actual full-suite,
Clippy, retry and unrelated formatting boundaries; those runs are not relabeled
as newly executed here.

## Probe Corrections And Remaining Boundaries

The first package is rejected before production because a shared relocated
Cargo cache embeds old public Skill bytes. It is never deployed. A fresh target
build produces the verified six-file closure. A feature-selection attempt is
also refused because Distribution has no such feature. Both are recorded in
operator-notes.json (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/operator-notes.json`).

The first post-switch assertion incorrectly requires an identical Session event
count, overlooking normal startup auto-resume facts. Automatic downgrade stops
before mutation to preserve accepted facts. A separate read-only collector
verifies every old Session/Turn row plus exactly the 11 expected same-Run resume
facts, unchanged ledgers, actual Gateway binaries and all preservation checks.
Its first attempt uses an older `run_ids` list for one historical resumed Run;
the corrected check uses the unchanged authoritative assistant binding. No
product patch, additional restart, fact rewrite or resend resolves either probe
issue. Original failing logs are retained alongside the successful collector.

No real model-provider call, new public Feishu/WeCom notification or next
production schedule cycle is exercised by this rollout. Existing public-send
receipts prove the new live query works, not that the new result-resolution
cycle has already delivered publicly. No business Graph is rerun and no grant
is expanded for a smoke test. Unknown effects still require inspection, not
automatic retries. The scope does not cover arbitrary CLI effects or report
quality.

## Evidence

- Source deltas (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/source-identities.json`), build (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/build.log`),
  harness build (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/test-build.log`), release inventory (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/release-verification.json`).
- Startup (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/startup.json`), [candidate acceptance](acceptance.json),
  actual commands/results (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/acceptance.log`), native task cases (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/native-task-inputs.log`),
  native Feishu schedule (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/native-feishu-schedule.log`).
- Deployment (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/deployment.json`), [Session/Turn integrity](history-integrity.json),
  actual Gateway queries (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/gateway-protocol.json`),
  successful collector (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/verification.log`), final live health (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/post-switch-health.json`).
- Initial switch assertion (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/first-switch.log`),
  initial read-only collector (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/first-collector.log`),
  operator notes (local-only, not checked in: `experiments/task-result-resolution/rollout-20261114/operator-notes.json`).

Private scripts, raw snapshots, native histories, full source identities and
cold backups remain in `.local/task-result-rollout-20261114/`; credentials and
business content are not copied into these exported evidence files.
The actual operator commands after the fresh build are:

```sh
python3 .local/task-result-rollout-20261114/prepare-release.py
python3 .local/task-result-rollout-20261114/accept.py
python3 .local/task-result-rollout-20261114/deploy.py
python3 .local/task-result-rollout-20261114/verify-live.py
```

The deployment command switches the release but fails its overly strict
post-switch assertion; the collector completes read-only verification. It does
not rerun deployment. These private one-shot scripts are evidence, not a new
product deployment framework or commands to retry blindly.
