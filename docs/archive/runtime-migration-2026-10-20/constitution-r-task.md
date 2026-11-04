# R 任务书：统一 Run 读模型探针（含锁释放收口）

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

当前结论（2026-10-17）：**R0 与 R 已完成本地交付，待审查；读取侧结论为“需要重想”**。R0 `cb2219c` 已同步 master `6394e28`；R 实现 `54e664e`。包含保留适配的同口径判断点82→90，未达到减少50%的门槛。原生读模型可运行，旧消费者仍需原record兼容，严格单View输入未验收。四crate1299通过/0失败/112原有忽略，另Library74、FileLease3通过；Kernel零diff。当前证据见[测量](../../../experiments/constitution-host-preflight/r/measurement.md)与[验证](../../../experiments/constitution-host-preflight/r/verification.md)。后续待审查旧读取契约，本文保留本刀验收口径，不自动启动H3–H6。

编制：2026-10-17。前置：P 探针已验收（`ff09f2c`、`1d1ba54`，见 [measurement.md](../../../experiments/constitution-host-preflight/p/measurement.md)）。**时限 2 个工作日**，到期无论做到哪都交回。

## 为什么做这一刀

P 证明了**写入与生命周期**一侧：stop/resume/abandon/终态/迟到结果由内核定律取代，停止轮询全部消失。没证明的是**读取**一侧：Host 非测试代码中仍有约 **197 处 `RunStatus`**，分布在 25 个文件。P 的做法是把 `Phase` 投影回 `RunStatus`（`kernel_runs.rs:173–233`），`queries.rs:59–76` 甚至复用内核 `FileStore` 的文件名编码来取更新时间。读取侧耦合没有减少。

本刀回答一个问题：**消费者改为直接读"相位 + Graph 进度 + 宿主观察"之后，它们的 `RunStatus` 分支能删掉多少，剩下的是什么。** 这个结果决定是否做全量迁移（H3–H6）。

判定标准，现在就定好：

- **值得迁移**：所选消费者的状态分支减少 ≥ 50%，且剩下的判断能说清楚属于 Session 层或授权，不是"相位里缺信息"。
- **需要重想**：分支数持平或增加，或者大量判断只能靠再造一个类似 `RunStatus` 的枚举才能表达。

## 0. 开工前

1. 读：`AGENTS.md`；`docs/anchor-constitution.md`（相位、等待者、结局）；P 的 `measurement.md`（尤其"最别扭的一处"和"未决"）；`kernel_runs.rs`、`run_projection.rs`、`liveness.rs`、`queries.rs`。
2. 从 `1d1ba54` 开始，基线：四 crate 1295 通过 / 0 失败 / 112 忽略。不符先报告。
3. 不要用 `git checkout` / `reset` / `stash`。只 stage 自己改的路径。

## 1. R0 文件锁释放收口（小，先做）

`469d81f` 修了两处锁：fork 出来的子进程在 exec 之前会继承锁的 fd，只靠 drop 释放时，锁会被这个副本拖住，出现误报"已被占用"。但还有两个缺口：

- `run_data.rs` 的 `remove_legacy_conversation` 只在成功路径上显式解锁，第 127–176 行的 `?` 提前返回仍然走隐式 drop。
- 同类 flock 在生产代码里还有约 15 处，都有同样的风险：`credential_store.rs`、`runtime_configuration.rs`、`model_configuration.rs`、`plugin_dependencies.rs`、`plugin_candidates.rs`、`api/schedules.rs`、`kernel_runs.rs`、`channel_supervisor/management.rs`、`goose_acp/pilot.rs`、`anchor-library` 的 `lib.rs`、`reclaim.rs`、`development.rs`、`oauth.rs` 等。用 `grep -rn "flock\|try_lock()" --include=*.rs` 复核，排除测试和内存 Mutex。

做法：在一个共享位置（例如 `anchor-library`，或 runner-host 和 library 都依赖的最低层 crate）提供**唯一的**文件租约类型，在 `Drop` 中显式解锁。把上面所有生产站点改成用它，`goose_acp/conversation.rs` 和 `run_data.rs` 也一起改用它，删除各处手写的解锁代码。在交回中写出改动前后的站点数。行为不变，不需要新的语义条目。这一处也要同步到 master，提交请单独成一个 commit，方便 cherry-pick。

提交：`fix: release every file lease explicitly before close (R0)`

## 2. R 读模型

### 2.1 定义读模型（形状由你决定，语义必须满足）

在 runner-host 中定义一个只读的 Run 视图，例如 `RunView`，它由三部分组成：

- **内核事实**：`Phase`，包括等待者种类与原因、结局种类与 detail；`GraphRunRecord` 中的 Graph 进度。
- **宿主观察**：是否有 worker 在跑；更新时间。
- **派生的可用动作**：只由相位加"是否在跑"推出，规则直接对应内核定律：
  - Executing 时可以 stop；
  - 等待人且没有 worker 在跑时可以 resume；
  - 未结算时可以 abandon。

  Session 层的附加条件（身份、助手是否当前、会话是否被取代等）**不放进视图**，由各自调用方叠加。

视图里**不得出现** `RunStatus`，也不得出现与之一一对应的新枚举。如果某个消费者确实需要一个相位里没有的区分，在测量里登记"缺什么"，不要造枚举来补。

**更新时间**是宿主观察，不是内核事实：

- 由宿主唯一的内核写入口（P 中的 `submit_with_retry` 及其调用方）在提交被接受时记录到宿主自己的观察文件。
- **不得**读取内核存储的文件布局或 mtime，删除 `queries.rs` 中对 FileStore 文件名编码的复用。
- 如果你认为必须在内核 `Stamp` 里加时间才行，属于停止条件，交回。

### 2.2 旧 Run 怎么办

旧 Run 仍由旧 Runner / RunStore 负责。给旧 Run 提供一个**临时**的 `RunStatus → RunView` 适配函数，放在一个文件里，标注"迁移完成后删除"。这样消费者只面对 `RunView` 一种输入。适配函数的行数单独计数，不算进消费者的简化里。

### 2.3 迁移的消费者（只改这几个）

1. `application/run_projection.rs`：HTTP 和 Pilot 共用的状态投影。
2. `application/liveness.rs`
3. `application/conversations.rs`：Session 层的代表，用来检验 Session 层是否也会变简单。

`kernel_runs.rs` 中 P 留下的 `Phase → RunStatus` 投影（`record()` 中的映射和 `projection()`）改为基于 `RunView`，能删的删掉。其他读 `RunStatus` 的文件不动。

**对外 JSON 不变**：HTTP 返回给 Web 的字段和取值保持原样。如果需要转换，放在序列化边界的一个函数里，单独计数。Web 不在本刀范围。

### 2.4 测试

- 现有测试全部照旧通过，旧 Run 的 API 返回逐字节不变。
- 为 `RunView` 的动作派生写单测：覆盖每种相位加"是否在跑"的组合。
- 为内核 Run 补一个经过 HTTP 的读取测试，覆盖运行中、等待人、已结算三种状态，验证 `phase` 和 `actions` 字段，并验证更新时间来自宿主观察。可以在 P 的 `api::tests::kernel_probe` 上扩展。

## 3. 测量（本刀的主要产出）

写到 `experiments/constitution-host-preflight/r/measurement.md`。计数口径沿用 P 的 `measure.py`（`if` / match arm / 直接赋值），并扩展出可以复现的脚本。

| 消费者 | 改前 `RunStatus` 分支与相关判断 | 改后判断 | 被相位或内核定律取代的（逐条） | 保留的（逐条，标明属于 Session 层、授权还是相位缺信息） | 新增胶水行数 |
| --- | --- | --- | --- | --- | --- |
| run_projection | | | | | |
| liveness | | | | | |
| conversations | | | | | |
| kernel_runs 投影 | | | | | |

另外单列三项：旧 Run 适配函数的行数、JSON 边界转换的行数、宿主时间观察的行数。

然后回答三个问题，每个一段：

1. 按上面的判定标准，结论是"值得迁移"还是"需要重想"？
2. 剩下 22 个读 `RunStatus` 的文件，按本刀的比例粗估：能删多少，哪些文件预计最难（逐个点名）。
3. `RunView` 里有没有某个字段，你觉得其实是在偷偷重造 `RunStatus`？

## 4. 约束

- C1：`git diff 6ee2b0e -- rust/anchor-kernel` 为空。
- 单一权威：每个 Run 只在一个存储里。时间观察文件只是观察，不作为任何控制判断的依据。
- 不新增 `#[ignore]`，不弱化已有断言。
- 不改 Web，不改对外 JSON。

## 5. 两级停止条件

**必须停下交回：**

- 需要改 `anchor-kernel`，包括给 `Stamp` 加时间、给 `Store` 加公开的文件时间接口。
- 旧 Run 的 API 返回发生变化，或者需要改 Web 才能通过。
- 删除或弱化断言、加 `#[ignore]`。
- 某个消费者只能靠一个与 `RunStatus` 一一对应的新枚举才能表达。这本身就是测量结论，写清楚后交回。
- 时限到期。

**自行判断、登记后继续：**

- `RunView` 的具体字段和文件组织。
- 某个判断归属不清（Session 层还是相位）：选一个，在测量表的"保留"列写明理由。
- R0 中个别锁站点因为语义特殊（例如需要阻塞等待）不适合统一类型：保留原样，登记原因。

## 6. 验收命令

```bash
cd /home/mansteinl/code/anchor-constitution-core/rust
cargo build -p anchor-sandbox-bwrap --bins
cargo fmt --all -- --check
cargo clippy -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host -p anchor-library --all-targets -- -D warnings
cargo test -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host -p anchor-library --no-fail-fast
git diff 6ee2b0e --stat -- rust/anchor-kernel   # 期望为空
grep -rn "RunStatus" rust/anchor-runner-host/src/application/{run_projection,liveness,conversations,kernel_runs}.rs   # 贴出结果
```

## 7. 交回模板

```
刀：R0 / R
commit：<hash> <标题>（R0 单独一个）
R0：锁站点 改前 <n> → 改后 <n>；保留未改的站点及原因 <…>
测试：kernel <…>；runtime <…>；runner-host <…>；graph-host <…>；library <…>；与基线差异 <…>
测量结论：值得迁移 / 需要重想，一句话理由 + 表格总计
RunStatus 剩余：四个文件各 <n> 处（贴 grep 结果）
clippy / fmt：<…>
C1：<…>
自行判断并登记的事项：<…>
停止条件触发：<无 / 哪条及现场>
需要审查方决定的问题：<…>
```
