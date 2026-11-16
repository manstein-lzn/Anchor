# Explicit task result resolution

Date: 2026-11-14. Implemented and deployed with the matching Host and installed
Feishu, WeCom and graph-tasks System Plugins. The exact package passes bounded
local acceptance and live read-only recovery/protocol checks. Schedules and
grants are unchanged; no historical message is resent. See the
[production rollout](rollout-20261114/README.md) for package identities, actual
checks and the remaining real-provider/public-delivery boundary.

Publication boundary: original `.local/` incident records, logs and full native
receipts remain private and are not checked in. Only selected summaries and
compact rollout results are published under the
[local evidence rules](../../docs/development.md#文档归属); dated failures,
commands and binary identities below are not revalidated by this checkout.

## Incident and replacement

The read-only incident audit (local-only, not checked in: `.local/scheduled-delivery-audit-20261114/evidence.json`)
found nine morning scheduled Runs completed. Six notifications were consumed
with `task.result.handled` even though their assistant only wrote a final summary
and never called the channel send API. Actual deliveries and an intentionally
silent NO_EVENT monitor are distinct cases; scheduler contention retried and was
not the cause of those six misses. Subsequent user-requested reports must not be
blindly resent.

This change removes unconditional task-result consumption after a final summary.
The existing Graph task FIFO, scheduler, Goose loop and Gateway ledger remain
the owners of their existing facts; there is no second queue or delivery ledger.

- `task_result_resolve({action:"delivered"})` queries the original authenticated
  Gateway for stable result delivery IDs. At least one actual send, all submitted
  requests, and both members of every file/introduction pair must be confirmed.
  The model cannot supply recipient identities or receipt references.
- `task_result_resolve({action:"silent",reason:"..."})` records explicit silence,
  not delivery. It requires a nonblank reason and no submitted or recorded sends.
  Whether silence satisfies the business request remains Graph/Plugin policy;
  the Host does not guess notification requirements from natural language.
- Session events record required send references *before* socket submission and
  an immutable result decision after verification. These references contain no
  delivery statuses. They close the timeout window in which a Gateway has
  accepted a socket but has not yet claimed the send. Missing is not silence.
  New requests and resolution exclude each other in the Session transaction.
- A prepared completion callback rejects unresolved `final_result` calls before
  Goose native completion freezes, leaving the same loop available to correct
  the error. `session.reply` and `finish_task_result` validate again before a new
  handled event. Ordinary user replies do not change.
- Both gateways expose an authenticated, read-only `delivery_status` operation
  over existing `kind=send` ledger facts. Host batches bounded queries without
  dropping IDs. Gateway send claim timing and unknown-outcome rules do not change.

Decisions survive interruption and restart; confirmed logical deliveries keep
their existing identities. Further sends for a resolved result fail closed.
Previously handled history is not rewritten as new delivery evidence.

## Verification

This section retains the source-phase checks. Exact-package acceptance and the
later production switch are recorded separately in the rollout above.

Logs and raw local fixture evidence live in
the task evidence directory (local-only, not checked in: `.local/task-result-resolution-20261114`).
These checks are not a claim of full-product joint acceptance.

| Check | Actual result and evidence |
| --- | --- |
| Session default suite | 192 passed across 15 targets, including doc tests; log (local-only, not checked in: `.local/task-result-resolution-20261114/session-final-tests.log`) |
| Host complete bin suite after send-intent fix | 1,346 passed, 1 process-reaping timing assertion failed, 22 default-ignored; log (local-only, not checked in: `.local/task-result-resolution-20261114/host-final-unit-tests.log`). The failing test reran alone unchanged: 1 passed; retry (local-only, not checked in: `.local/task-result-resolution-20261114/cancellation-retry.log`) |
| Final channel tools suite | 19 passed, covering no-send refusal, pending-before-claim, unknown/partial ACKs, explicit silence, interruption identity, and 261 delivery IDs over bounded queries; log (local-only, not checked in: `.local/task-result-resolution-20261114/final-channel-tests.log`) |
| Both gateways and shared ledger query contract | 4 targeted tests passed, including authentication, bounded input, only requested send facts, and no external send; log (local-only, not checked in: `.local/task-result-resolution-20261114/final-gateway-status-tests.log`) |
| Architecture fences and resource retries | Four fence targets: 7 passed. `briefing_handoff` and `standard_host`: 8 passed on disk after shared tmpfs space failures; log (local-only, not checked in: `.local/task-result-resolution-20261114/fences-and-resource-retry.log`). Original resource failures remain in the earlier run (local-only, not checked in: `.local/task-result-resolution-20261114/host-session-tests.log`) |
| Real pinned Goose with local deterministic Provider | Five task-input cases passed across the batch and an isolated retry: assigned schedule success/failure, delegated completion, immutable file FIFO with explicit silence, two private subscriptions, and user interruption. Batch: 4 passed, 1 initial-event HTTP timeout; log (local-only, not checked in: `.local/task-result-resolution-20261114/native-final-tests.log`). Delegation isolated retry: 1 passed; log (local-only, not checked in: `.local/task-result-resolution-20261114/native-delegation-retry.log`) |
| Final-source assigned-schedule native acceptance | 1 passed after bounded-query changes, including a false delivered final being rejected and corrected before actual file/intro sends; log (local-only, not checked in: `.local/task-result-resolution-20261114/final-native-schedule-test.log`) and evidence (local-only, not checked in: `.local/task-result-resolution-20261114/native-final-evidence/1490763/goose-persistent-assigned-schedule/evidence.json`) |
| Strict Clippy | All targets of Host, Session, shared transport and both gateways pass `-D warnings`; log (local-only, not checked in: `.local/task-result-resolution-20261114/final-clippy.log`) |
| Formatting, wording, whitespace | Changed Rust files pass scoped rustfmt; wording and `git diff --check` pass. Workspace fmt check still fails only in 11 paths outside this task; workspace differences (local-only, not checked in: `.local/task-result-resolution-20261114/final-workspace-fmt.log`) |

The full Host result precedes the final additional large-selection test; the
final channel suite runs that test on the final source. Counts are not presented
as a single all-passing full-suite run. No source change was made for either the
process-reaping timing failure or the native initial-event timeout.

Commands for final checks used the stable toolchain and
`CARGO_TARGET_DIR=/tmp/anchor-dreaming-rollout-target`:

```sh
TMPDIR=/tmp cargo +stable test --manifest-path rust/Cargo.toml \
  -p anchor-runner-host --bin anchor-runner-host channel_tools::tests -- --test-threads=1
cargo +stable clippy --manifest-path rust/Cargo.toml \
  -p anchor-runner-host -p anchor-platform-session -p anchor-channel-transport \
  -p anchor-feishu-gateway -p anchor-wecom-gateway --all-targets -- -D warnings
TMPDIR=/tmp cargo +stable test --manifest-path rust/Cargo.toml \
  -p anchor-channel-transport -p anchor-feishu-gateway -p anchor-wecom-gateway delivery_status
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check
scripts/check-sensitive-wording.sh
git diff --check
```

Native checks use `XDG_RUNTIME_DIR=/run/user/1001`, `TMPDIR=/tmp`, the real pinned
Goose binary and the existing relay. Only model transport and private channel
control are fixture substitutes in these native cases; separate channel tests
exercise the actual local WeCom Gateway and shared send ledger.

```sh
ANCHOR_GOOSE_BINARY="$PWD/.local/project-direct-20261113/fixture-bin/goose" \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY="$CARGO_TARGET_DIR/debug/anchor-net-relay" \
ANCHOR_TEST_EVIDENCE_ROOT="$PWD/.local/task-result-resolution-20261114/native-final-evidence" \
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --test goose_persistent_assistant task_inputs:: -- --ignored --test-threads=1
```

The isolated retries select the exact delegation or assigned-schedule test
name instead of `task_inputs::`. The evidence files retain Host/test binary
identities; the pinned Goose SHA-256 is
`17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640`.

Workspace fmt differences are in `anchor-facts/src/{lib,tests}.rs`,
`anchor-hub/src/table.rs`, Host `api/dreaming/tests.rs`,
`application/dreaming/{cycle,evaluation}.rs`, `application/plugins.rs`,
`application/run_access/admission.rs`,
`application/schedule_subscriptions/delivery.rs`, `node_tools.rs`, and
`anchor-sandbox-bwrap/src/lib.rs`. They were not reformatted by this task.

## Deployment and business boundaries

- Compatible Host and Gateway binaries, installed Feishu/graph-tasks Skills and
  the matching platform launcher were deployed together on 2026-11-14. All six
  actual live Gateways accept authenticated `delivery_status` queries; no new
  Feishu application scope is needed. An old Gateway fails closed.
- No real-model-provider acceptance, public Feishu/WeCom send, or next scheduled
  production cycle was exercised here. Local ACKs do not prove public delivery.
- The Host verifies channel-tool effects, not the quality or completeness of the
  report, the business validity of silence, arbitrary CLI/external effects, or
  whether every selected file was required by a natural-language request.
- Unknown or missing-after-submission effects require inspection; this change
  intentionally does not add automatic retries or promise arbitrary exactly-once
  external effects. Original schedules and historical handled facts stay intact.
