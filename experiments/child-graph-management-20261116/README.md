# Creator-Owned Graph Management

Date: 2026-11-16. Source acceptance completed; the scoped format-5 release is
now deployed locally on 8077/5173. See the [rollout record](rollout-20261116/README.md).

Publication boundary: this dated record is not validation of this checkout.
Original logs, native receipts and source snapshots remain local and are not
checked in; see the [local evidence rules](../../docs/development.md#文档归属).

## Change

`graph_tasks_delete` completes the existing creator's Graph management tools.
It reuses content-bound Graph deletion, not a new store or cleanup service.
The existing creation grant and ownership scope authorize the operation without
per-Graph approval. Original and current grants are both required; Session and
non-session Run boundaries remain unchanged.

Retained schedules, including paused plans and aliases, must be cancelled
explicitly. Existing unfinished-Run, pending-result, delivery, and call-reference
protections remain in effect. The Host supplies the actual global workspace
root; the current node's workspace is not used as a cleanup root. The Web policy
label and the Graph-task Skill disclose the management and deletion scope.

## Executed Verification

The commands below use the repository's existing shared local target directory,
debug information disabled, incremental compilation disabled, and four build jobs.

| Command | Result |
| --- | --- |
| `scripts/test.sh -p anchor-runner-host --bin anchor-runner-host --offline --locked graph_tasks -- --test-threads=2` | 19 passed; ownership, frozen/current grants, revocation while waiting, cancellation, retained schedules and aliases, unfinished Runs, result handling, and actual cleanup |
| `scripts/test.sh -p anchor-runner-host --all-features --all-targets --no-fail-fast --offline --locked -- --test-threads=2` | 1,464 passed, 0 failed, 132 default ignored, 0 explicitly filtered; 47 targets, including existing architecture fences |
| `scripts/test.sh -p anchor-runner-host --test goose_persistent_assistant --offline --locked task_inputs::creator_manages_and_deletes_its_graph_after_handling_the_result -- --exact --ignored --test-threads=1` | 1 passed; explicitly runs the otherwise ignored native fixture |
| `cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-runner-host --all-features --all-targets --offline --locked -- -D warnings` | Passed |
| `cargo +stable fmt --manifest-path rust/Cargo.toml --all --check` | Passed |
| `scripts/check-sensitive-wording.sh --all` and `git diff --check` | Passed |
| `npm --prefix apps/web test -- GraphTaskPolicyEditor` | 401 tests in 50 files passed; the package script's `src` argument selects the full Web suite |
| `npm --prefix apps/web run build` | Passed; existing large-chunk warning remains |

The native test uses pinned Goose v1.53.0, the actual Host, Kernel/Runner, ACP,
authorized MCP tools, and a child Op. Only model transport uses the deterministic
local Provider. It creates, reads, edits, and runs an owned Graph, consumes its
real report, then deletes the Graph, child Run metadata, and private workspace.
Assertions preserve the parent Run, its native Session, and its copied report.
It makes no real-model or external-channel requests and uses no production data.

Local raw logs, the pre-edit baseline, and native evidence are retained under
`.local/child-graph-management-20261116/`. They are machine-local, not distributed
evidence. The first task-unit build found type-conversion and call-site errors;
those were fixed before the successful runs recorded above.

## Boundaries

The source checks above did not change production. The subsequent authorized
rollout restarts only the Host and upgrades the matching Graph-task Skill; it
also includes the two upstream history-watcher and Run-detail fixes, not other
parallel worktree changes. The deployed Host's inotify descriptors fall from
116 to 1, and the nine previously resource-blocked Kernel checks pass without
changing kernel quotas.

No production Graphs or history were deleted, and no production schedules or
permission settings were changed. No new real-provider business execution,
external-channel message delivery or whole-workspace acceptance is claimed.
Existing concurrent worktree edits are retained; nothing was staged or committed.
