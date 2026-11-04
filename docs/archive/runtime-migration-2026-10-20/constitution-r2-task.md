# R2 任务书：收敛对外状态词汇后重测读侧

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

编制：2026-10-17。前置：R0/R 已验收（`cb2219c`、`54e664e`、`b1e9bc1`，见 [R 测量](../../../experiments/constitution-host-preflight/r/measurement.md)）。**时限 2 个工作日**，到期无论做到哪都交回。

当前交回（2026-10-19）：**R2 停止，未验收**。HTTP `pause → stop` 已按本任务接线，但原非状态断言要求当前节点完成并留下产物，实际立即取消后没有完成结果；另有 Session 原 `pause` 拒绝变为 `stop` 接受的冲突。未修改这些断言或控制写入语义。现场、测量和待决入口：[R2 交回](../../../experiments/constitution-host-preflight/r2/measurement.md)、[验证](../../../experiments/constitution-host-preflight/r2/verification.md)、[断言清单](constitution-refactor-log.md#r2停止交回2026-10-19)。本文件后面的原验收要求保持不变。

后续以 [全量迁移任务书 M1](constitution-migration-task.md#1-m1完成-r2对外状态词汇) 为准：pause 保留为独立边界控制，S25 作废；不再用判断点减少 50% 决定是否迁移。本文保留 R2 当时要求，历史停止证据不改写。

## 为什么做这一刀

R 的负结论（判断点 82→90）有前提：任务书要求"对外 JSON 不变"，于是旧产品的 10 种状态全部保留。相位表达不了其中几组区分，规则就只是搬进了适配层。

**用户决定（2026-10-17）：对外状态词汇收敛到内核相位。允许改 API 和 Web。** 本刀在新词汇下，用同一口径重测，判断读侧是否真的变简单。

判定标准不变：所选消费者的判断点**减少 ≥ 50%**，且剩下的判断能归到 Session 层或授权 → 值得全量迁移（H3–H6）；否则需要重想。

## 1. 新的对外状态词汇（已决定）

每个 Run 对外只给这几项，取代旧的 `status`/`phase` 字符串：

| 字段 | 取值 | 来源 |
| --- | --- | --- |
| `phase` | `executing` / `waiting` / `settled` | 内核 Phase |
| `waiting_for` | `human` / `child` / `effect`，仅 waiting 时有 | Waiter 种类 |
| `outcome` | `done` / `failed` / `escalated` / `unknown` / `aborted`，仅 settled 时有 | Outcome 种类 |
| `reason` | 字符串，等待原因或结局 detail，可空 | Waiter reason / Outcome detail |
| `active` | bool，是否有 worker 在跑 | 宿主观察 |
| `actions` | `stop` / `resume` / `abandon` 三个 bool | 只由上面几项推出（见下） |
| `updated` | 时间，可空 | R 的宿主时间观察 |

动作规则（直接对应内核定律，不加别的）：

- `stop`：`phase == executing`
- `resume`：`phase == waiting && waiting_for == human && !active`，或 `phase == executing && !active`（宿主重启后没有 worker，需要重新启动驱动）
- `abandon`：`phase != settled`

Session 层的附加条件（身份、助手是否当前、会话是否被取代等）照旧由调用方在这三项上**再收窄**，不进视图，也不改上述规则。

**没有 pause。** 内核没有暂停，等待人就是暂停。API 的 pause 入口和 Pilot 的 pause 工具改为 stop 的别名（返回中注明），不再维护单独的暂停状态。

## 2. 旧 Run 怎么映射

旧 Run 仍由旧 Runner / RunStore 负责。只在**一个函数**里把旧 `RunStatus` 映射到新词汇，映射允许有损：

| 旧状态 | 新词汇 |
| --- | --- |
| Ready、Running | executing（`active` 由 worker 观察决定） |
| Paused | waiting / human / reason=`paused` |
| Stopped | waiting / human / reason=`stopped` 或原 error |
| BudgetStopped | waiting / human / reason=`budget`（预算是否改为结算属待决 W4，本刀不动） |
| WaitingRecovery | waiting / effect / reason=原恢复说明 |
| WaitingCall | waiting / child |
| Completed | settled / done |
| Failed | settled / failed / reason=原 error |
| Aborted | settled / aborted / reason=原 error |

**旧行为按新规则对齐，不保留旧区分。** 例如：

- 旧规则里 Paused 不算静止、Stopped 算静止；新规则里两者都是"等待人且无 worker"，统一为静止。
- 旧 Ready 与孤立 Running 显示不同；新词汇里都是 executing，用 `active` 区分。

每一处这样的行为变化，在工作记录里登记为 **S 候选（待批）**，写明旧行为、新行为、谁会受影响。这正是本刀要回答的问题：这些区分是否真的需要。**不得**为了保留某个旧区分在视图里加字段或枚举；如果你判断某个区分对用户确实必要，登记下来，作为测量结论的一部分交回。

## 3. 范围

**Host（同 R 的四个消费者，加边界）：**

1. `application/run_projection.rs`
2. `application/liveness.rs`
3. `application/conversations.rs`
4. `application/kernel_runs.rs` 的投影
5. R 留下的临时适配 `legacy_run_view.rs` 和 JSON 边界 `run_json.rs`：按新词汇重写或删除，目标是旧 Run 只剩第 2 节那一个映射函数。
6. API 返回 Run 状态的地方：列表、详情、Pilot 的 `run_status` 工具，改为输出新词汇。

**Web**：消费旧状态字符串的文件改为读新字段，主要有 `RunDetailPage.tsx`、`runPresentation.ts`、`Timeline.tsx`、`execution.ts`、`model.ts`、`PilotTurn.tsx`。用 `grep -rn "waiting_resume\|budget_stopped\|waiting_recovery\|waiting_call\|paused\|stopped" apps/web/src` 复核。中文文案由你决定：等待类用"等待你处理：{原因}"之类的统一表述即可，不要为每个旧状态各写一套。

**不动**：其余 18 个读 `RunStatus` 的 Host 文件（control、abandon、recovery、session_calls、wecom 等）。它们内部仍可使用 `RunStatus`。本刀只改"对外读"和所选消费者。

## 4. 测试

- 现有测试全部通过。断言旧状态字符串的测试，改为断言新词汇；**每一处断言修改在工作记录里逐条列出**（测试名、旧断言、新断言、对应哪条 S 候选）。不得删除测试，不得放宽到不检查状态。
- 第 2 节映射表写单测，覆盖全部 10 个旧状态，加有无 worker。
- 动作规则写单测，覆盖每种 phase / waiting_for / outcome，加有无 worker。
- 一个 HTTP 测试同时覆盖旧 Run 和内核 Run：同样的处境（运行中、等待人、已结算）返回同样的新词汇。
- Web：`cd apps/web && npm test && npm run build` 通过；改动的组件有对应测试。

## 5. 测量

写到 `experiments/constitution-host-preflight/r2/measurement.md`。口径、脚本与 R 完全相同（复用并扩展 `r/measure.py`），**基线取 R 之前的 `1d1ba54`**，使与 R 的 82 直接可比。

| 消费者 | 基线（1d1ba54） | R（54e664e） | R2 | 被相位或定律取代的（逐条） | 保留的（逐条，标归属） |
| --- | --- | --- | --- | --- | --- |
| run_projection | 13 | 13 | | | |
| liveness | 0 | 0 | | | |
| conversations | 59 | 58 | | | |
| kernel_runs 投影 | 10 | 10 | | | |
| 旧 Run 映射 + 边界 | — | 9 + 适配 | | | |
| **合计（去重）** | **82** | **90** | | | |

另列：

- Web 侧：改动文件中按状态字符串分支的数量，改前与改后。同样数 `if` / 三元 / `switch case` / 对象表键。
- S 候选清单汇总：共几条，每条一句话。

然后回答三个问题，每个一段：

1. 按判定标准，结论是什么？
2. 用这个比例外推剩下 18 个文件，并点名其中仍然最难的。说明难点是在 Session 层，还是相位缺信息。
3. S 候选里，哪些你认为用户会真的在意？

## 6. 约束

- C1：`git diff 6ee2b0e -- rust/anchor-kernel` 为空。
- 单一权威不变；时间观察不作为任何控制判断的依据。
- 不新增 `#[ignore]`。断言修改只限状态词汇，且逐条登记。
- 控制写入路径（stop/resume/abandon 的实际执行）不改语义。本刀只改读取与对外表达，pause→stop 别名除外。

## 7. 两级停止条件

**必须停下交回：**

- 需要改 `anchor-kernel`。
- 某个控制写入路径的语义必须改变才能通过（不是读取或表达的变化）。
- 删除测试、加 `#[ignore]`、断言放宽到不检查状态。
- 某个旧区分只有在视图里加字段或枚举才能表达，而你判断它必须保留：写清楚后交回，这本身就是结论。
- 时限到期。

**自行判断、登记后继续：**

- 旧行为向新规则对齐（第 2 节），登记为 S 候选。
- Web 文案与组件组织。
- 某个判断归属不清时，选一个，在测量表写理由。

## 8. 验收命令

```bash
cd /home/mansteinl/code/anchor-constitution-core/rust
cargo build -p anchor-sandbox-bwrap --bins
cargo fmt --all -- --check
cargo clippy -p anchor-file-lease -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host -p anchor-library --all-targets -- -D warnings
cargo test -p anchor-file-lease -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host -p anchor-library --no-fail-fast
git diff 6ee2b0e --stat -- rust/anchor-kernel   # 期望为空
cd ../apps/web && npm test && npm run build
```

## 9. 交回模板

```
刀：R2
commit：<hash> <标题>
测试：Rust 各 crate <通过/失败/忽略>；Web test <…>；Web build <…>；与基线差异 <…>
断言修改：<条数>，见工作记录 <小节>
测量结论：值得迁移 / 需要重想；合计 82 → 90 → <R2>；Web 状态分支 <改前> → <改后>
S 候选：<条数>，一句话列表
clippy / fmt：<…>
C1：<…>
自行判断并登记的事项：<…>
停止条件触发：<无 / 哪条及现场>
需要审查方决定的问题：<…>
```
