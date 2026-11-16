# Codex bridge acceptance evidence

Publication boundary: only this historical summary is checked in. All `.local/`
logs, native receipts, captured exchanges and deployment records below remain
private local originals, not clone dependencies; see the
[local evidence rules](../../docs/development.md#文档归属). Dated commands and
results retain their original identities and do not validate this checkout.

## Native Project Client (2026-11-12)

Deployed 14 MCP tools: the ten convenience tools plus `native_discover`,
`native_call`, `native_events`, `native_respond`. Installed Codex exports 180
request methods, 86 notification methods and 11 server-request methods. All are
discoverable; 52 reviewed project/catalog requests are callable. Host auth,
global configuration, unrestricted filesystem/process RPC, unsandboxed shell,
realtime/sample execution ownership and unreviewed authority remain blocked.
Schema discovery is not a claim of unrestricted/full RPC execution coverage.

The native entry replaces the need for individual feature wrappers; shared turn
admission and sandbox policy replace duplicated execution rules. No new task
scheduler, agent loop or persistent store. Goal/queue fields and event buffers
are reconstructible Codex projections. Queue/add can start execution while idle;
the failed first probe caught that assumption, and admission now treats it as
work with conservative writer reservation.

- `scripts/test.sh -p anchor-codex-bridge --locked`: 60 passed, 6 ignored.
  Log (local-only, not checked in: `.local/codex-native-20261112/bridge-tests-final.log`).
- Real `gpt-6.1-sol` / `max` native workflow: explicit project creation, rich
  text turn, dynamic tool request/native response, wire completion events,
  queued add/list/delete while a request is pending, native settings/name/models
  and history, unchanged disposable checkouts and rejected authority overrides.
  Final (local-only, not checked in: `.local/codex-native-20261112/live-native-final.log`).
  The first failure (local-only, not checked in: `.local/codex-native-20261112/live_codex_native_surface-124041.log`)
  incorrectly assumed queue/add on an idle thread was draft-only; native starts
  it immediately. The corrected fixture keeps the parent turn pending while
  manipulating the queue and verifies exactly two completed turns.
- Historical unloaded btw / load / persistent worktree fork and running-parent
  btw compatibility both passed with the actual local provider:
  lifecycle (local-only, not checked in: `.local/codex-native-20261112/live-lifecycle-final.log`),
  side chat (local-only, not checked in: `.local/codex-native-20261112/live-btw-final.log`).
- Strict crate all-target Clippy, scoped formatting, build and wording pass.
  Clippy (local-only, not checked in: `.local/codex-native-20261112/clippy-final.log`),
  build (local-only, not checked in: `.local/codex-native-20261112/build.log`),
  wording (local-only, not checked in: `.local/codex-native-20261112/wording.log`).
- Switched user Bridge service with no active workers and managers at wait_input;
  normal Plugin install-preflight keeps authorization digest and configuration
  revision 1 unchanged. Online service active, 14 tools and native discovery/model
  catalog reachable; binary hash verified. No Host restart/global Codex config
  edits, Git commit or remote push.
  Deployment (local-only, not checked in: `.local/codex-native-20261112/deployment.json`),
  online (local-only, not checked in: `.local/codex-native-20261112/online-verification.json`),
  Plugin connection (local-only, not checked in: `.local/codex-native-20261112/plugin-connection.json`).

This slice does not verify every callable native operation. Real compaction,
active goal auto-continuation, Feishu-client receipt/approval conversations and
hours-long stability remain acceptance boundaries; the user will exercise the
existing `feishu-anchor-developer` bot. A dynamic-tool response is not a claim of
human-approval or Feishu delivery validation.

## Project Session Lifecycle (2026-11-12)

The project manager now receives ten bounded tools, adding `models`, `history`
and `session` to the existing surface. Shared native load/fork implementations
replace the former inline resume and btw-only fork paths. No second database,
agent loop, scheduler or arbitrary RPC/configuration passthrough was added.
`subscribed` is an in-memory event projection, separating subscription from
native `loaded` residency rather than treating unsubscribe as immediate unload.

- `scripts/test.sh -p anchor-codex-bridge --locked`: 46 passed, 5 explicitly
  ignored (the subprocess fixture and opt-in live probes).
  Log (local-only, not checked in: `.local/codex-lifecycle-20261112/bridge-tests-final.log`).
- `scripts/test.sh -p anchor-codex-bridge --lib live_codex_project_session_lifecycle -- --ignored --nocapture`:
  passed against the actual local provider. The isolated app-server uses a
  process-only zero unload delay, then verifies native unloading before btw;
  the answer contains inherited context while the parent's turn stays unchanged
  and unloaded. Also verifies no-turn load, settings, persistent worktree fork,
  model/history queries and archive/unarchive. No global config change.
  Final log (local-only, not checked in: `.local/codex-lifecycle-20261112/live-lifecycle-final.log`).
- The first lifecycle probe exposed native rejection of combining ephemeral
  fork with `deferGoalContinuation`. The shared adapter now sends that flag only
  for persistent forks. Failure (local-only, not checked in: `.local/codex-lifecycle-20261112/live-lifecycle.log`).
- `scripts/test.sh -p anchor-codex-bridge --lib --locked live_codex_btw_and_reasoning_effort -- --ignored --nocapture`:
  passed with `gpt-6.1-sol` / `max`, inherited context and the original parent
  turn still running. Log (local-only, not checked in: `.local/codex-lifecycle-20261112/live-running-btw.log`).
- Strict crate all-target Clippy, scoped rustfmt, build and wording checks passed.
  Clippy (local-only, not checked in: `.local/codex-lifecycle-20261112/clippy.log`),
  build (local-only, not checked in: `.local/codex-lifecycle-20261112/build.log`).
- Bridge and Plugin deployed through the existing user service and normal
  install-preflight route; no active Bridge worker and related managers waiting
  for input at switch. Online discovery confirms ten tools, unchanged
  authorization digest/configuration revision 1, and no Host restart.
  Deployment (local-only, not checked in: `.local/codex-lifecycle-20261112/deployment.json`),
  connection (local-only, not checked in: `.local/codex-lifecycle-20261112/plugin-connection.json`).

Native compaction is covered by the deterministic lifecycle test only, not a
real-provider compaction probe. Feishu-client delivery, approval roundtrips and
hours-long stability remain unverified by this slice. No repository commit or
remote push was made.

## Earlier Acceptance

Local checks on 2026-11-12, against `codex-cli 0.167.1`. The initial checks below
preceded deployment; the later deployed side-chat slice is recorded separately.
Existing unrelated worktree changes were not reverted.

| Executed check | Result |
| --- | --- |
| `scripts/test.sh -p anchor-codex-bridge --locked` | 44 passed, 3 ignored (subprocess fixture and two explicit live probes) |
| `cargo +stable test --manifest-path rust/Cargo.toml -p anchor-graph-host --test codex_plugin --locked` | 4 passed |
| `cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-codex-bridge --all-targets --locked -- -D warnings` | Passed |
| `cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host --test layer_fence --test run_access_fence --locked` | 4 passed |
| `scripts/test.sh -p anchor-codex-bridge --lib --offline --locked tests::live_codex_readonly_smoke -- --ignored --exact --nocapture` | Passed; local provider read a marker in a disposable repository, checkout stayed clean |
| `scripts/test.sh -p anchor-codex-bridge --lib --locked tests::live_codex_worktree_write_smoke -- --ignored --exact --nocapture` | Passed; actual file contents verified in isolated worktree, absent from main checkout, managed removal verified |
| `scripts/test.sh -p anchor-codex-bridge --lib --locked tests::live_codex -- --ignored --nocapture` | Both real-provider probes passed together against the final native history query |
| `cargo +stable build --manifest-path rust/Cargo.toml -p anchor-codex-bridge --locked`; `rust/target/debug/anchor-codex-bridge --help` | Build and executable entry point passed |
| `scripts/check-sensitive-wording.sh --all` | Passed |

The deterministic fixture is a Rust subprocess using native JSON-RPC over stdio.
Integration calls enter through `anchor-mcp-host` and the authenticated HTTP
service, not a direct manager mock. Coverage includes concurrent workers, bounded
waits, same/overlapping-cwd writer rejection, active worktree cleanup guards,
cross-project scopes, native history pagination and revalidation, delayed history
hydration, uncertain turn admission, explicit approvals/questions, steering,
interruption, transport teardown, and reconnect ABA protection.

Those initial checks created no production service, channel credential, Graph
Tasks schedule, Git push or deployment. They do not prove hours-long stability, external
channel notification delivery, or cooperation with a concurrently running TUI
using the same thread. The isolated scheduled-notification acceptance below uses
actual Codex but a scripted manager model and simulated Feishu platform.

The Bridge replaces no existing production path: it adds the previously absent
external Codex execution capability and reuses the existing Plugin/MCP,
Graph Tasks and channel mechanisms. It adds no Kernel facts or durable task
database; volatile state is an event projection of Codex-owned history.

## Side Chat And Model Parameters (2026-11-12)

The ordinary MCP package now exposes `btw`; `spawn` accepts separate `model` and
`reasoning_effort` fields. No new scheduler, durable store, agent loop, or global
Codex configuration was introduced. The implementation reuses native ephemeral
forks, existing turn execution and status inspection, and unsubscribe for closing.

- `scripts/test.sh -p anchor-codex-bridge --locked --offline`: 45 passed,
  3 ignored before adding the new opt-in live probe. One focused deterministic
  test covers model/effort forwarding, scope, side follow-up/close, and unchanged
  active parent turn. Log (local-only, not checked in: `.local/codex-effort-20261112/bridge-tests.log`).
- `scripts/test.sh -p anchor-codex-bridge --lib --locked --offline
  tests::live_codex_btw_and_reasoning_effort -- --ignored --exact --nocapture`:
  passed in 19.36 seconds with the actual local provider. Native settings report
  `gpt-6.1-sol` and `max`; the side thread answers a marker present only in inherited
  history while the parent's original turn still runs. The isolated fixture then
  closes its side thread and interrupts its own sleeping parent.
  Evidence (local-only, not checked in: `.local/codex-effort-20261112/live-btw-3.log`).
- Earlier live attempts are failures, not passes: the first parent finished
  before verification; the second timed out waiting for buffered command output.
  The probe now observes command-start activity instead of output availability.
  First (local-only, not checked in: `.local/codex-effort-20261112/live-btw.log`),
  second (local-only, not checked in: `.local/codex-effort-20261112/live-btw-2.log`).
- `cargo +stable build --manifest-path rust/Cargo.toml -p anchor-codex-bridge
  --locked --offline`: passed. The user service now runs the candidate in
  `.local/codex-effort-20261112/bin/`; the normal Plugin install-preflight path
  published the updated Skill. Online MCP discovery returns seven tools and
  configuration revision remains 1. No Host restart or Codex global config edits.
  Deployment (local-only, not checked in: `.local/codex-effort-20261112/deployment.json`),
  connection check (local-only, not checked in: `.local/codex-effort-20261112/plugin-connection.json`).
  The first switch was rolled back after probing before the listener was ready;
  the final switch waits for readiness. Both checked for no active Bridge workers.

Feishu-client `/btw` delivery is not yet verified. The inherited context is a
fork-time snapshot, not a continuously synchronized view, and provider latency
remains variable. The pending Host input-file fix and prior broader acceptance
boundaries below are unchanged.

## Integrated Acceptance (2026-11-12)

The user authorized beginning acceptance. The existing Host on port 8077 and its
one Feishu / three WeCom robot bindings were inspected read-only and left running.
Bridge's 44 deterministic tests and both explicitly selected real-provider probes
passed again (the two probes together took 10.68 seconds).

Added an opt-in test at
`rust/anchor-runner-host/tests/goose_feishu/codex.rs`. It reuses the existing native
Goose, Host, provider and Feishu fixtures; no second scheduler or channel transport
was added. It installs the shipped Codex package, saves owner-bound configuration,
imports both shipped Feishu manager/watch Graphs and saves the actual task grant.
An authenticated HTTP MCP Bridge runs real local Codex against a disposable Git
repository. The manager starts a worktree worker without waiting for completion,
then creates a real two-occurrence schedule. The first observer reports running;
the same manager receives a user follow-up before the native worker is released.
After actual marker-file verification, the next observer reports completed and
the manager deletes its schedule. Both proactive sends have confirmed entries in
the real Gateway delivery database. Original Session/native history, distinct
scheduled child Runs, exact task inputs, schedule provenance and immutable report
bytes are checked. The main checkout remains clean; the test removes only its
own disposable worktree.

The final recorded run passed in 49.98 seconds; its initial ordinary reply was
observed after 6,130 ms, before releasing the native worker. This is a test
measurement with a scripted manager, not a production response-time guarantee.
The final report contains runtime/source digests and actual tool exchanges:

- Final hybrid evidence (local-only, not checked in: `.local/codex-acceptance-20261112-0950/runtime/3006587/goose-feishu-live-codex/evidence.json`)
- Final execution log (local-only, not checked in: `.local/codex-acceptance-20261112-0950/live-runtime-final.log`)
- Existing three Feishu scenarios (local-only, not checked in: `.local/codex-acceptance-20261112-0950/feishu-regression.log`): all passed against the fixed Host, using loopback model/platform endpoints.
- Node tools regression (local-only, not checked in: `.local/codex-acceptance-20261112-0950/node-tools-tests.log`): 37 passed, 1 ignored.
- Full Host binary tests (local-only, not checked in: `.local/codex-acceptance-20261112-0950/host-unit-tests.log`): 1,273 passed, 0 failed, 18 ignored; 287.75 seconds. Ignored native/environment cases are not counted as passes.
- Plugin tests (local-only, not checked in: `.local/codex-acceptance-20261112-0950/plugin-tests.log`): 4 passed; Host fences (local-only, not checked in: `.local/codex-acceptance-20261112-0950/fence-tests.log`): 4 passed; strict Clippy (local-only, not checked in: `.local/codex-acceptance-20261112-0950/clippy-final.log`): passed. Scoped rustfmt, whitespace and wording checks also passed.

These `.local` artifacts are private, machine-local evidence, not distributed
fixtures or credentials. The Codex model request count is not measured; the report
uses null rather than the deterministic fixture's usual zero. Actual external
Feishu requests and actual manager-provider requests remain zero.

### Reproduction

Prepare the fixed Goose and sibling Host/network/Gateway helpers using the
[runtime regression guide](../../docs/runtime-contract-tests.md). This live test
is absent unless its non-default feature is selected and also requires explicit
environment opt-in; default Feishu `--ignored` regression stays loopback-only.

```sh
ANCHOR_GOOSE_BINARY="$PWD/.local/bin/goose" \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY="$PWD/rust/target/debug/anchor-net-relay" \
ANCHOR_CODEX_LIVE_ACCEPTANCE=1 \
ANCHOR_TEST_EVIDENCE_ROOT="$PWD/.local/codex-acceptance-20261112-0950/runtime" \
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --features live-codex-acceptance --test goose_feishu --locked --offline \
  codex::live_codex_scheduled_progress_and_completion_keep_the_manager_conversation_usable \
  -- --ignored --exact --nocapture --test-threads=1
```

Also executed:

```sh
scripts/test.sh -p anchor-runner-host --bin anchor-runner-host --locked --offline node_tools::tests
ANCHOR_GOOSE_BINARY="$PWD/.local/bin/goose" \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY="$PWD/rust/target/debug/anchor-net-relay" \
ANCHOR_TEST_EVIDENCE_ROOT="$PWD/.local/codex-acceptance-20261112-0950/feishu-regression" \
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --test goose_feishu --locked --offline -- --ignored --nocapture --test-threads=1
scripts/test.sh -p anchor-runner-host --bin anchor-runner-host --locked --offline -- --test-threads=8
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-graph-host --test codex_plugin --locked --offline
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --test layer_fence --test run_access_fence --locked --offline
cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --bin anchor-runner-host --test goose_feishu --features live-codex-acceptance \
  --locked --offline -- -D warnings
```

### Failure Found And Fixed

The initial full-path test exposed a real `anchor_read` defect: a task Artifact
bound as an exact file produced an empty relative path, rejected by the resource
reader. NodeTools now resolves only that exact authorized source through the
existing component-by-component no-symlink file reader. It does not grant the
source's parent directory. A regression covers both file/directory inputs,
adjacent paths, traversal, write refusal and symlink substitution. No persistent
format, permission expansion or Kernel changes were needed. Production still
runs the previous Host, without this fix.

Retained development logs `live-runtime-1.log` through `live-runtime-4.log` record
failed attempts, not passes: candidate-path normalization, a two-second schedule
overlapping its observer and producing a legitimate blocked result, the exact
file-read defect, and a scripted follow-up missing its current-turn receipt.
The fixture now uses canonical source paths, a 15-second interval, actual mount
reads and current-turn discovery. `live-runtime-5.log` passed before the final
provenance assertions and timing measurement were strengthened. Slow environments
can still cause a valid schedule blockage; it is reported as a failed acceptance,
not silently suppressed.

### Still Required Before Channel Trial Sign-Off

- Choose and explicitly authorize a test robot/recipient; do not reuse existing
  app credentials in a second Gateway or silently replace an active robot.
- Select the candidate Host deployment containing the exact-file-read fix, without
  treating unrelated uncommitted work as permission to upgrade production.
- Exercise an actual manager model and actual Feishu client, including progress,
  completion and explicit approval/question response; this hybrid test does not
  establish model-planning quality or real external delivery.
- Run a longer-duration soak. The approximately 50-second test is not hours-long
  stability or seamless recovery of in-flight Codex shells after Bridge restart.
