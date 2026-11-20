# Integrated source closeout - 2026-11-20

Follow-up: [Library and release verification](release-verification.md) closes the
two acceptance gaps noted in this baseline record. It does not establish a rollout.

Tested source: `75cdba6`, the merge of local `631ed7f` and remote `1abf7dc`,
plus removal of the duplicate task-result resolution tool registration. The
existing authorized delivery test saw two definitions before the deletion and
passed with one afterward. No delivery, permission, or state format changed.

| Check | Observed result |
| --- | --- |
| Rust workspace, all features and targets | 165 targets; 2,752 passed, 0 failed, 160 default-ignored, no test filter |
| Explicit native regression selection | 38 passed: task inputs/subscriptions 6, WeCom/channel 6, Feishu 6, AgentNode completion 12, media 6, project tools 1, standalone Graph with Web assets 1 |
| Project Manager to real Codex | 1 passed: shipped Manager and observer Graphs, real local Codex app-server and model provider, disposable worktree marker, progress and completion returned to the same Manager session, main checkout unchanged. Goose model and Feishu transport were local fixtures |
| Writer coordination | Workspace tests passed: concurrent admission into different worktrees, refusal of a second overlapping writer, and retention of the shared Host/Bridge lease until a terminal state |
| Web | 407 unit tests, 9 mock browser tests, and production asset build passed |
| Static checks | Strict workspace Clippy and workspace formatting passed |

Native execution used the fixed Goose and the same Host, relay, sandbox helper,
and gateway binaries throughout. The first five Feishu cases stopped before any
model or delivery because the test target lacked the pinned `lark-cli`. The
separate rerun staged the digest-verified official 1.0.100 binary and passed all
six Feishu cases, including the CLI case. The deterministic runs used no
production data or external model or channel requests.

On this baseline, the existing `goose_library` ignored case failed before model execution:
the frozen list contained `demo` and `network`, while the assertion expected only
`demo`. The same failure was observed on the earlier local and remote baselines.
This run did not turn that assertion into a pass or run the full native suite.
The standalone Graph with Web assets recovered from an extracted package with
four deterministic model requests and one workspace effect. This is not a full
format 5 platform install; the separate release candidate verifier still expected
five requests at this baseline.

Separately, read-only production task events and gateway ledgers confirm the
November 20 morning cycle used one producing Run and the same PDF for distinct
Feishu and VIP WeCom results. The file and introduction had confirmed receipts on
both routes. Resolution was recorded at 09:00:24 for Feishu and 09:05:13 for
WeCom (Asia/Shanghai). These are existing receipts from before that day's
AgentNode cutover, not a production run of the joined candidate, and do not
establish that a client opened the message.

No push, production cutover, schedule or permission change, resend, or replay
was performed. This verifies the selected implementation paths, not autonomous
multi-project coordination or real-provider business delivery on the candidate.
Actual commands, binaries, first-run failures, reruns, and scenario receipts are
retained locally under `.local/source-closeout-20261120.vim0cPxR/` and are not
part of the clone.
