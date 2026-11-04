# H2 任务书：失败结算收尾 + 端到端概念探针

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

当前结论：**H2.1 与 P 串行探针均已验收（2026-10-17）**。S18 不变量覆盖全部 5000 个并行 seed；旧无失败回归保留原门槛，parallel_activations=5048。P 的七类场景、1000 seed 对照和真实 Op.run Host 路径通过；真实 Goose/Provider 未执行，默认旧路径未切换。见[H2.1 验收](../../../experiments/constitution-host-preflight/h2/h21-completion.md)、[P 验收](../../../experiments/constitution-host-preflight/p/verification.md)、[测量与未决](../../../experiments/constitution-host-preflight/p/measurement.md)。

后续读模型探针 R 已本地交付，测量结论为“需要重想”，H3–H6 仍暂缓；见 [R 测量](../../../experiments/constitution-host-preflight/r/measurement.md)与[验证](../../../experiments/constitution-host-preflight/r/verification.md)。本文的 H2.1/P 不再作为当前待办。

编制：2026-10-17（改版）。上位计划：[constitution-host-integration-plan.md](../design-records-2026-11-04/constitution-host-integration-plan.md)。

## 为什么改版

目标是**快速验证概念**，不是生产级迁移。K0–K3 的 20,000 例差分已证明"旧 Runner 的语义可以由内核 + 校验器表达"。还没有证据的是另一半：**接进 Host 后，Host 是否真的变简单**。原 H2.2（2,000 + 2,000 seed Host 级对照）和 H2.3（全量接线调查）服务于"能否安全替换"，现在不做。

本文只有两刀：

- **H2.1 收尾**：把对照规则从"逐情形列举"改成一条不变量，提交。
- **P 探针**：打通一条真实的串行路径，测量 Host 生命周期代码的变化。时限 **3 个工作日**，到期无论做到哪都交回。

## 0. 开工前

1. 读：`AGENTS.md`；上位计划第 0、2 节（C1–C6、§2.1 调度器、§2.2 执行者）；`docs/constitution-refactor-log.md` 的 S6、S17、S18。
2. 从 `21a3c91` 继续。不要重写已有实现，不要 `git checkout` / `reset` / `stash`。
3. 预期基线：kernel 37 通过；runtime 125 通过 / 2 失败（并行随机组与 seed 2122）；runner-host + graph-host 1120 / 0 / 112。不符先报告。

## 1. H2.1 收尾：并行失败对照改为不变量

### 语义（审查决定，2026-10-17，S18；合并 S6、S17）

旧 Runner 有波次屏障，一波收齐后才处理；新模型按提交顺序逐个裁决。因此：

> **并行区内，第一个被内核接受的终结事实决定唯一结算原因。** 终结事实包括节点失败（Settle）和分支轮次上限（look-ahead）。之后到达的终结事实与结果以 `TaskSettled` 被拒，留在历史中。

不得为复刻旧侧的波内顺序而改校验器或内核。

### 对照规则（替换 `differential.rs` 中按情形列举的失败比较）

无失败结局（完成、全部轮次上限）：照旧逐项相等，不变。

出现失败结局时，检查三条：

1. **原因正当**：新 `detail` 是下列之一：
   - 旧侧该 Run 中某个实际节点失败的 `"{node} failed: {reason}"`；
   - 分支轮次上限，且对应节点 `n@limit` 能从旧侧最终状态推出：`n` 是某个已完成分支调用的后继，且旧侧 `passes[n] >= limit`。
2. **事实不丢不重**：旧侧每个节点失败、每个已并入的 Work 结果，在新侧**恰好出现一次**：要么在状态中，要么是历史中的 `TaskSettled` 被拒记录（失败为 Code Settle，同槽位同 reason；结果带产物证据）。
3. **其余相等**：计数、已完成结果、边决定逐项相等，允许的差异只有：
   - 第 1 条中上限结算产生的那一项 `ceased` 及其 `refuse_outgoing` 写下的被拒出边；
   - 旧侧未成为结算原因的失败分支，在新侧不是 Failed（对应第 2 条中的被拒记录）。

seed 495、seed 2122 两个独立用例改为断言这三条。这是审查方批准的断言修改，在工作记录 S18 中注明即可。

### Tally

- 保留 `node_failed`（串行、并行各 > 100）、`multi_failure_waves`（并行 > 0）。
- 新增 `ceiling_preempted_failures`（并行 > 0）：结算原因为上限、但旧侧有节点失败的 Run 数。
- 已有阈值不下调，不改注入概率与 seed 范围。

### 完成标准

三组差分全绿（5,000 并行组全部跑完），贴出完整 Tally。更新工作记录：S18 补 Tally 数值，H2.1 一节改为完成。

提交：`feat(validator): settle a Run on a known node failure (H2.1)`

## 2. P 探针：一条真实串行路径 + 测量

### 要回答的问题

同一组操作（执行、stop、resume、abandon、未知效果、重启、状态投影），走"内核 + 校验器 + 调度器"时，Host 需要多少手写判断，和旧路径相比如何。

### 范围

- **做**：串行 Graph，Work 节点，经现有 `NodeExecutionPort` / `ArtifactPort` 执行；Run 由内核 `FileStore` 负责。
- **不做**：fanout/join、Op.call、常驻助手打断、Web、旧 Run 迁移。遇到时返回具名 `GraphError::Unsupported`。
- **旧路径不动**：现有 Run、现有测试仍走旧 Runner / RunStore，必须照旧全绿。

### 实现要点（形状由你决定）

1. **调度器** `rust/anchor-runtime/src/graph/scheduler.rs`，对 `Kernel<S: Store>` 泛型。循环：`load` → 相位非 Executing 则退出 → `GraphValidator::frontier` → 执行 → `submit` → 重复。
   - `NodeExecutionRequest` 和旧 Runner 共用同一个构造函数，不许复制。
   - Work 结果用 `PrincipalKind::Agent`，Settle 用 `Code`；成本取 `model_requests`，只经 `submit` 的 `cost` 参数盖章（C5）。
   - 产物先 `freeze`，`commit` 放进提交并附 `Evidence::Resource`。
   - 派发前先查 `completion_fact`：`Completed` 直接提交；`Failed` 提交 Settle；`NotStarted` / `Resumable` 执行。
   - `StaleVersion` 重读重提；`TaskSettled` / `TaskWaiting` 不重试。
2. **单一权威**：内核 Run 存在独立根目录（例如 `<state>/kernel-runs/`），从不写入 `FileRunStore`。接纳时选择哪条路径由你决定，但**不得用全局环境变量**，且不得让现有测试换路径。生命周期入口按"Run 在哪个存储里"分派。
3. **生命周期走内核**（C4，内核路径里不得出现 `stop_requested` / `pause_requested`）：
   - stop：人的 `Request{control:"stop"}`；被接受后 Host 取消在跑的执行。
   - resume：人的 resume，然后重新启动调度器。
   - abandon：人的 abandon。
   - **未知效果**（执行返回 `WaitingRecovery`，或 `completion_fact` 为 `Uncertain`）：Code 带 Observation 提交 stop，Run 进入等待人；人核查后 resume 或 abandon。**不自动重放。**
   - 重启：新进程读相位与开放空位即可继续，不需要游标。
4. **状态投影**：现有 API 查询对内核 Run 返回状态时，从 `Phase` + `GraphRunRecord` 投影。

### 测试（runner-host，经应用层，不只测调度器）

1. 2–3 节点串行 Graph 跑完：产物、成本、最终相位正确。
2. 执行中 stop：Run 进入等待；在跑结果以 `TaskWaiting` 被拒并留在历史；resume 后完成。
3. 等待中 abandon：结算为放弃，之后提交被拒。
4. 重启：第一个节点提交后中断进程（fixture 报错或 drop），新实例继续；第一个节点不再执行，中断的节点经 `completion_fact` 以同一 invocation 恢复。
5. 未知效果：进入等待人，不重放；分别测 resume 和 abandon。
6. 节点失败：Settle 结算。
7. 轻量对照：复用差分测试的串行生成器和 Scripted 端口，1,000 个 seed，调度器与旧 Runner 的派发序列和最终事实相等。

执行端口优先用真实 node_host 加确定性 Goose fixture（`tests/support/goose_fixture.rs`、`docs/runtime-contract-tests.md`）。环境不具备时用 fixture 端口，并在报告里如实写"真实 Goose：未执行"。

### 测量（探针的主要产出）

写入 `experiments/constitution-host-preflight/p/measurement.md`。每个操作对比旧路径与内核路径，数字附 `文件:行`：

| 操作 | 旧路径手写判断数 | 内核路径手写判断数 | 被内核定律取代的判断（逐条列出） | 新增胶水行数 |
| --- | --- | --- | --- | --- |
| 执行循环 | | | | |
| stop | | | | |
| resume | | | | |
| abandon | | | | |
| 未知效果 / recovery | | | | |
| 重启 | | | | |
| 状态投影 | | | | |

"手写判断"按 `RunStatus` 分支、前置条件 `if`、停止/暂停轮询、直接字段赋值来数。起点参考：旧路径 `control.rs`、`abandon.rs`、`recovery.rs` 中的 `RunStatus::` 分支，`runner.rs` / `parallel.rs` 中的 6 处停止/暂停轮询，`execution.rs:283–305` 的失败兜底写入。

另外回答三个问题，每个一段：

1. 哪些判断**没有**被定律取代、内核路径仍需手写？为什么（属于 Session 层，还是定律缺失）？
2. 若把全部 Host 迁过去，按本探针的比例粗估可删行数与新增行数。
3. 探针中最别扭的一处设计是什么。

### 完成标准

- 旧路径全部测试照旧通过；新增测试 1–7 通过（真实 Goose 未执行时如实说明）。
- `measurement.md` 填完。
- 提交：`feat(host): kernel-path probe for serial Runs (P)` 和 `docs(probe): measurement (P)`。

## 3. 约束

- C1：`git diff 6ee2b0e -- rust/anchor-kernel` 为空。
- C3：校验器不得出现 I/O、async、端口调用。
- C4：调度器与内核路径的生命周期代码中零处 `stop_requested` / `pause_requested`。
- C5：成本只经 `submit` 参数；Agent 不得提交 Settle。
- 不新增 `#[ignore]`；已有断言、阈值不下调。H2.1 中审查方批准的断言替换除外。

## 4. 两级停止条件

**必须停下交回：**

- 需要改 `anchor-kernel`（包括新增 `Reason`、`Waiter`、`Phase` 变体）。
- 需要改校验器中 H2.1 以外的生产裁决逻辑。
- 删除或弱化断言、下调阈值、加 `#[ignore]`。
- §1 不变量第 2 条不成立，即有事实丢失或重复。
- 违反宪法：结算可逆、自动重放未知效果、从提交内容读身份或成本、一个 Run 同时有两个权威。
- 时限到期。

**自行判断、登记后继续：**

- 差异只来自旧侧波次顺序，且满足 §1 不变量。
- 探针中 Host 侧的接口形状、文件组织、胶水写法。
- 某个操作在内核路径上暂时做不到、但不违反上面任何一条：返回具名 `Unsupported`，在 `measurement.md` 里登记，继续做下一个。

登记格式：工作记录中新增"S 候选（待批）"或在 `measurement.md` 的"未决"一节写一行：现场、理由、影响。审查方在本刀结束时统一处理。

## 5. 验收命令

```bash
cd /home/mansteinl/code/anchor-constitution-core/rust
cargo build -p anchor-sandbox-bwrap --bins
cargo fmt --all -- --check
cargo clippy -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host --all-targets -- -D warnings
cargo test -p anchor-kernel
cargo test -p anchor-runtime -- --nocapture differential   # 贴出每组 Tally
cargo test -p anchor-runtime
cargo test -p anchor-runner-host -p anchor-graph-host
git diff 6ee2b0e --stat -- rust/anchor-kernel              # 期望为空
grep -rn "stop_requested\|pause_requested" rust/anchor-runtime/src/graph/scheduler.rs   # 期望为空
```

## 6. 交回模板

```
刀：H2.1 / P
commit：<hash> <标题>
测试：kernel <…>；runtime <…>；runner-host <…>；graph-host <…>
  与基线差异：<逐项；无则写"无">
差分 Tally：serial <…>；parallel <…>；calls <…>（node_failed、multi_failure_waves、ceiling_preempted_failures 必填）
探针测试 1–7：<逐项 通过/失败/未执行>
真实 Goose：<执行并通过 / 未执行及原因>
测量：<一句话结论 + 表格总计>
clippy / fmt：<…>
结构检查：C1 <…> C3 <…> C4 <…> C5 <…>
自行判断并登记的事项：<列表；无则写"无">
停止条件触发：<无 / 哪条及现场>
需要审查方决定的问题：<…>
```
