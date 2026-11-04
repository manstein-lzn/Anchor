# Run 访问门面任务书（master）

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

F0 已交 `e3a5778`，F1 已交 `06f7b8a`、`c21b382`、`0116074`；F2 已完成，业务围栏为 0，标准 Goose 门槛及入口外补集均通过。G0 合入、原工作树/分支清理及三项独立修正已完成。当前边界、规则度量与验证范围以 [Run 访问边界](../../run-access-facade.md) 为准，下文保留原任务要求，不作为当前待办。

编制：2026-10-19。适用分支：`master`（工作目录 `/home/mansteinl/Anchor`）。来源：用户批准的"先降耦合，再换实现"方案。

## 为什么做

Host 一共 156 个生产文件，其中 37 个直接引用 Run 的内部类型（`RunStatus`、`GraphRunRecord`、`RunStore`、`FileRunStore`、`GraphRunner`、`RunCursor`），合计约 370 处。业务代码（会话、投递、助手、API 投影等）直接读写 Run 的状态字段，并调用 `store.save`。

另一条分支 `constitution-core` 正在用新内核重建 Run 的运行时，它的设计依据见 `docs/anchor-constitution.md`。只要业务代码还直接碰这些内部类型，两边每次合并都会产生**语义冲突**：文本能合上，但等于多了第二个写入者，或者合并后直接编译不过。

本任务在 master 上建一个**门面**：业务代码只通过它读写 Run。master 上门面背后仍是现有实现，**行为零变化**；`constitution-core` 只需把门面背后的实现换成内核。

## 硬性要求

- **master 行为零变化**：现有测试全部通过，**不修改任何测试断言**。对外 HTTP JSON、Pilot 工具输出、Web 显示逐字节不变。这是一次纯重构。
- 遵守 `AGENTS.md`：不用 reset/checkout/clean，只 stage 自己改的路径，每个 commit 编译通过、测试全绿。
- 其他 agent 同时在 master 上做业务修改。按文件小步提交，每次提交前先合入最新 master，避免长时间占用同一批文件。

## F0：围栏（先做，体量小，立刻起保护作用）

1. 在 `anchor-runner-host` 中加一个测试，例如 `run_access_fence`。它扫描 `src/**/*.rs` 的生产代码，统计每个文件中出现的上述六个类型名。
   - 跳过 `tests/` 目录、`*tests.rs` 文件，以及 `#[cfg(test)]` 之后的模块内容。
   - 识别规则写在测试注释里。
2. 文件分两类：
   - **运行时侧（永久允许）**：门面模块本身、`execution.rs`、`main.rs`、`node_host/`、`run_data.rs`。不在名单上的文件，如果你认为也属于运行时侧，写明理由加进名单。`anchor-graph-host` 这个 crate 整体属于运行时侧，不在本围栏范围内。
   - **业务侧（棘轮）**：其余所有文件。当前计数写进一个已提交的清单文件，例如 `rust/anchor-runner-host/run-access-fence.txt`。测试规则如下：
     - 任一文件的计数超过清单值 → 失败；
     - 清单之外的文件出现引用 → 失败；
     - 计数下降时，提示同步降低清单值。
3. 在 `AGENTS.md` 加一小节：业务代码只能通过门面读写 Run，围栏测试会拦截直接引用，确有需要时先扩展门面。
4. 参考基线（2026-10-19 用 grep 粗算，包含测试模块；请用你的扫描规则重新计算）：业务侧较大的有 `application/conversations.rs` 27、`application/run_projection.rs` 21、`api/wecom.rs` 20、`application/session_calls.rs` 19、`application/control.rs` 19、`application/liveness.rs` 16、`application/abandon.rs` 15、`api.rs` 14、`application/plugins.rs` 14、`assistant.rs` 13、`application/recovery.rs` 13、`application/dispatch.rs` 13、`application/admission.rs` 11。

提交：`test(host): fence Run internals behind a ratchet (F0)`

## F1：读取门面

新建模块，例如 `application/run_access/`。它对业务代码提供以下三样东西。

**1. 只读视图 `RunView`。** 内容只有：

| 部分 | 内容 |
| --- | --- |
| 相位 | `executing`；`waiting { for: human\|child\|effect, reason }`；`settled { outcome: done\|failed\|escalated\|unknown\|aborted, reason }` |
| 宿主观察 | 是否有 worker 在跑（`active`）；更新时间 |
| Graph 进度 | 只读访问：节点结果、产物引用、输出等，**不暴露** `status`、`error`、`cursor`、`recovery`、`recovery_submissions` |

master 上，相位由现有 `RunStatus` 映射得到（Ready/Running → executing，Paused/Stopped/BudgetStopped → waiting human，WaitingRecovery → waiting effect，WaitingCall → waiting child，Completed/Failed/Aborted → settled）。这个映射**只存在门面内部一处**。

**2. 规则谓词：每条规则一个函数。** 至少包括：

- `is_settled`、`is_quiescent`、`needs_attention`
- `can_stop`、`can_pause`、`can_resume`、`can_abandon`
- 业务代码现在用 `RunStatus` 判断的其他规则，例如"能否投递回复""能否交接""能否删除"

master 上，谓词**照搬现有判断**，旧状态的细分区别（例如 Paused 与 Stopped 的静止判定不同）都留在谓词内部。业务代码只调用谓词，**不再自己判断状态**。

**3. 对外序列化。** HTTP 和 Pilot 用到的状态 JSON 由门面统一生成，master 上输出旧词汇且逐字节不变。

然后迁移所有**只读**的业务消费者：`run_projection`、`liveness`、`queries`，以及 `conversations`、`api/*`、`assistant*`、`plugins`、`dispatch`、`admission`、`deletion`、`graphs` 等中的读取部分。每迁一个文件，围栏清单相应降低。

**交付一张规则表**，写在 `docs/run-access-facade.md`。每条规则列出：

- 谓词名；
- master 上的现有语义（照搬自哪几处代码）；
- 迁移前 Host 中独立判断这条规则的位置数，以及迁移后的位置数（应为 1）。

这张表就是 `constitution-core` 实现同一组谓词时的对照依据。

提交：`refactor(host): read Runs through one facade (F1)`。可以按文件拆成多个提交。

## F2：写入门面

业务代码中所有直接改 Run 的地方，都改为调用门面操作。门面在 master 上的实现，就是把现有代码**原样搬进去**。

当前写入点（2026-10-19 粗查，测试模块除外，以你的核实为准）：

- `store().save(...)`：`conversations.rs`（新 Run）、`session_calls.rs`（stop/yield/execute）、`assistants.rs`（新 Run）、`recovery.rs`（3 处）、`abandon.rs`、`control.rs`（3 处）、`admission.rs`
- `record.status = …`：`session_calls`、`abandon`、`recovery`、`control`

`constitution-core` 的工作记录里有一张完整清单（"H2/H3 直接写入棘轮起点"）。可以用 `git show constitution-core:docs/constitution-refactor-log.md` 查看，行号以 master 为准。

门面操作按**意图**命名，而不是按字段命名。至少包括：

- `admit`
- `stop`、`pause`、`resume`、`abandon`
- `record_recovery_decision`
- 会话调用的 stop、yield、execute、settle
- 子 Run 结算后释放父 Run

完成标准：业务侧围栏清单中，所有文件的计数为 0，清单只剩运行时侧。

提交：`refactor(host): change Runs only through the facade (F2)`。可以拆分。

## 验收（每个 F 交回前执行）

```bash
cd /home/mansteinl/Anchor/rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
# Goose 构建、环境与标准/完整忽略命令见 docs/runtime-contract-tests.md。
# G0 合入后只允许其已记录的三项残留；F1 完成后另一个小提交修正 fixture/断言。
cd ../apps/web && npm test && npm run build
```

另外需要一个逐字节对比：选取现有覆盖 Run 状态 JSON 的 API 测试，证明重构前后输出完全相同。现有测试已经逐字节断言的，直接引用这些测试即可。

## 停止条件

**必须停下交回**：

- 重构需要改变任何行为或测试断言；
- 对外 JSON 无法保持逐字节不变；
- 与并行进行的业务修改发生无法自行解决的冲突。

**自行判断、登记后继续**：

- 门面命名与文件组织；
- 新增谓词；
- 某个文件属于运行时侧还是业务侧（写明理由）。

## 交回模板

```
阶段：F0 / F1 / F2
commit：<hash 标题>（每个一行）
围栏：业务侧引用 <改前> → <改后>；运行时侧名单 <列表>
规则表：<规则数>，独立判断位置合计 <改前> → <改后>
测试：workspace <通过/失败/忽略>；Goose 忽略测试 <结果或未执行及原因>；Web <…>
JSON 逐字节：<证据>
自行判断并登记的事项：<…>
停止条件触发：<无 / 现场>
```

## 审查结论与补充（2026-10-19）

**F0 通过。** 围栏、`AGENTS.md` 约定、`/runs` 十种状态的逐字节基线均已交付，生产代码未改。两项失败都不是 F0 引入的，现定性如下：

- **wecom-gateway 的 3 个渠道时序用例**：同一时段本机另有一个 workspace 全量测试在跑（审查方在验收分支），属于负载导致的时序抖动；单独复跑均通过。其中 `slow_callback_does_not_block_control_and_older_reply_is_suppressed` 在分支 M1 也出现过一次，登记为已知时序脆弱用例，日后单独修复。
- **Goose 忽略测试**：属于 master 现有的基线问题，锁定值在 `586916d` 中更换过，由 [G0 任务书](goose-baseline-task.md)单独处理。

**F0 当时的审查要求（已执行）：**

1. 立即提交 F0，同时提交 `docs/anchor-constitution.md`、本任务书和 `docs/goose-baseline-task.md`。围栏只有提交之后才能拦住并行的业务修改。
2. 开始 F1。
3. G0 修复之前，Goose 门槛改为"**相对基线没有新增失败**"：以 G0（或 F0 证据）记录的失败名单为基线，F1、F2 交回时对比两份失败名单，不再要求全绿。

## F1/F2 审查结论（2026-10-19）

**通过。** 审查方在 `d968ba5` 上重跑 workspace：122 个 target，1994 通过、1 失败、123 忽略。

- 唯一的失败是已知的时序脆弱用例 `slow_callback_does_not_block_control_and_older_reply_is_suppressed`，单独复跑通过。
- 忽略数比交回报告多 1。多出来的是一个新增的 Goose 类测试（`requires pinned Goose binary, built http_fixture and local Bubblewrap`），在 Goose 门槛的覆盖范围内。
- Goose 与 Web 采信交回证据，审查方未重跑。

门面结果：业务侧内部类型引用 224 → 0；13 条规则的独立判断体 36 → 13。这是宪法第 10 节"同一规则的独立判断点在下降"的第一份正面证据。

**后续：**

- 业务代码继续只通过门面访问 Run。之后需要新能力时，先扩展门面，围栏保持为 0。
- **可选的小任务**：修复上面那个时序用例。它已在全量并发运行中失败三次，分别是 F0、分支 M1 和本次。修复时不得放宽超时来掩盖，要找出与负载相关的竞态，并给出确定性的同步点。
- master 目前领先 origin 11 个提交，是否推送由用户决定。

## constitution-core M3 进行期间的协调（2026-10-19）

分支 `constitution-core` 正在做 M3：所有 Run 默认走内核，同时删除 `GraphRunner`、`RunStore`、`RunStatus`，门面背后的实现会整体替换。master 上的业务开发不受影响，但请遵守以下几条：

1. **尽量不改门面内部。** `application/run_access/` 下的文件，以及 `main.rs` 中的 `write_durable`、`reject_snapshot`，只做必要的小改动。需要新的读取能力时，新增谓词或操作即可，不要改动已有谓词的内部判断。
2. **业务代码只调用门面。** 围栏会拦截直接引用 Run 内部类型的写法，不要为了绕过围栏去调高清单计数。
3. **跑 Goose 标准命令时，继续使用 `/tmp/anchor-goose-regression-target`。** 分支已改用 `-core` 后缀的独立目录，两边互不覆盖构建产物。
4. 已完成的修改按正常流程提交。M3 结束时分支会合并 master，冲突由分支负责解决。
