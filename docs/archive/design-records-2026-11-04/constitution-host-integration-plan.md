# 宪法内核接入 Host：临时开发计划

> 历史归档（2026-11-04）：保留当时的推导、授权范围、过程与证据，不作为新的实施授权；当前行为见[架构](../../architecture.md)，未关闭事项见[统一台账](../../pilot-development-plan.md)。

编制日期：2026-10-16。当前迁移以 [M1–M5 任务书](../runtime-migration-2026-10-20/constitution-migration-task.md) 为准；本计划的 H3–H6 划分已被取代，约束 C1–C6 与 §2 语义仍有效。

M1–M4 已验收，§10.20 M5 的测量、探针复核与文档收尾完成，迁移任务结束并交回。最终测量基点为 master `e60374f`；全部新 Run 共用 KernelScheduler，C2 save 0 / mutation 0，Kernel 相对 `0859373` 零 diff，30,000 例快照不变。本轮 workspace 2,200 / 0 / 126、fmt/围栏通过；Web 359 及构建和完整固定 Goose 123 / 0 / 0 引用已验 M4，M5 未重跑完整 Goose。最终逐 crate/规则/测试统计、探针重建和宪法复核见 [M5 最终测量](../../../experiments/constitution-host-preflight/final/measurement.md)；W4、真实模型完整业务、渠道实网、部署、时间与保留政策等见其“迁移后待决清单”。旧 worktree/分支保留，不推送。

K0.1 审查见 [结论](../../../experiments/constitution-host-preflight/k01/review.md)；已批准的 K0.2 FileStore 删除与资源租约见 [工作记录](../runtime-migration-2026-10-20/constitution-refactor-log.md#k02filestore-删除与资源租约)。Kernel 自 `0859373` 零 diff。H2.1、P、R 的历史度量不代表当前实现；M3 逐规则审计按独立判断体计数。

- 当前工作目录：`/home/mansteinl/Anchor`，分支 master；旧 constitution-core worktree 只读保留。
- 起点 commit：`7c196d8`
- 设计依据：[anchor-constitution.md](../../anchor-constitution.md)
- 工作记录：[constitution-refactor-log.md](../runtime-migration-2026-10-20/constitution-refactor-log.md)（每一刀完成后必须更新）

本文是临时计划，只服务于这一阶段。阶段完成并验收后，结论并入工作记录，本文可删除。

---

## 0. 给开发 agent 的必读背景

开工前按顺序读：

1. [AGENTS.md](../../../AGENTS.md)：仓库纪律（不 reset/checkout/clean，只提交实际改动的路径）。
2. [anchor-constitution.md](../../anchor-constitution.md) 第 1–5 节：宪法与原语。
3. [constitution-refactor-log.md](../runtime-migration-2026-10-20/constitution-refactor-log.md)：已完成的 K0–K3、语义变更 S1–S9。
4. 代码：`rust/anchor-kernel/`（新内核）、`rust/anchor-runtime/src/graph/validator.rs`（Graph 校验器）、`validator/differential.rs`（差分测试）。

### 已完成的部分（不要重做）

| 刀 | 内容 | 证据 |
| --- | --- | --- |
| K0 | `anchor-kernel`：六原语，`submit` 只做裁决/记录/结算/测量；任务生命周期（stop/resume/abandon）是内核定律；文件存储先写记录再写状态 | 17 项测试 |
| K1 | `GraphValidator`：Run 是内核任务，校验器 `open` 开放空位、`judge` 裁决提交 | 串行差分 10,000 个随机 Graph |
| K2 | fanout/join = 同时开放多个空位，控制节点只能由宿主代码提交 | 并行差分 5,000 个 |
| K3 | Op.call = 内核"等待子任务"，宿主代码绑定/释放/结算 | 调用差分 5,000 个 |

### 这一阶段要做的事

**把 Host 从旧 `GraphRunner` 迁移到"内核 + Graph 校验器 + 调度器"，然后删除旧 Runner。**

旧 Runner 是推模型：它选节点、调用 `NodeExecutionPort::execute`、等待、再决定下一步。新模型是拉模型：调度器读取开放的空位，交给执行者；执行者完成后提交；内核裁决。

### 不可违反的设计约束

这些是验收时会逐条检查的硬条件：

- **C1 内核零业务。** `anchor-kernel` 不得依赖 `anchor-runtime`、Graph、Goose、HTTP、Session 或任何业务 crate。不得为本阶段在内核里新增特定于 Graph 或 Host 的分支。K0.1 仅授权审查决定中的 D1、D2、D3、D5；其他内核改动仍须停止、记录理由并等待审查。
- **C2 唯一改变入口。** Run 的状态只能经 `Kernel::submit` 改变。Host 中任何直接写 Run 记录的代码（`store.save`、直接改 `GraphRunRecord` 字段后保存）都必须删除或改为提交。
- **C3 校验器不调用任何人。** `GraphValidator` 保持纯函数：不执行 I/O、不冻结产物、不调用端口。
- **C4 停止/暂停/恢复/放弃只走内核生命周期。** 不得在调度器或执行路径中新增"检查是否请求停止"的轮询标志；停止就是一次 `Request{control:"stop"}` 提交，执行者的取消由 Host 在提交被接受后触发。
- **C5 不读模型自述。** 执行者提交的身份、成本由宿主盖章，不从提交内容读取。
- **C6 分支不背兼容包袱。** 旧 Run 记录格式（format 12/13 迁移、旧授权升级、`legacy_environments`、`Deleted` 调用标记、旧 cursor 升级）一律删除，读到旧格式时**具名拒绝**（错误信息说明是旧格式、需离线迁移）。生产数据迁移不在本阶段。

---

## 1. 现状地图（编制时核对，实施前请重新确认）

| 位置 | 作用 | 本阶段处理 |
| --- | --- | --- |
| `anchor-runtime/src/graph/runner.rs`（1,034 行） | 旧推模型 Runner | 删除 |
| `anchor-runtime/src/graph/parallel.rs`（481 行） | 旧并行执行器 | 删除 |
| `anchor-runtime/src/graph/store.rs` | `RunStore` / `FileRunStore` | 由内核 `Store` 取代后删除 |
| `anchor-runtime/src/graph/state.rs` | `GraphRunRecord`、`RunStatus`、`migrate_format` 等 | `RunStatus` 删除；`migrate_format` 与旧格式字段删除；`GraphRunRecord` 保留为校验器状态，删除 `cursor`、`status`、`error`、`recovery`、`recovery_submissions` 等不再由它拥有的字段 |
| `anchor-runner-host/src/execution.rs`（887 行） | 构造 `GraphRunner` 并运行 | 改为调度器 |
| `anchor-runner-host/src/node_host/execution.rs` | `NodeExecutionPort` 实现（Goose/Op/host operation） | 改为"执行一个空位并返回提交内容"的执行者 |
| `anchor-graph-host/src/graph_call.rs` | 独立 Graph 宿主、`GraphCallPort` | 改为宿主代码提交 `CallSubmission` |
| `anchor-runner-host/src/application/{control,abandon,assistants,recovery,deletion,...}.rs` | 停止、恢复、放弃、退役、删除的前置条件 | 改为内核生命周期提交 + 读取内核相位 |
| 约 25 个非测试文件按 `RunStatus::` 分支 | 状态投影与前置条件 | 改为读取 `Phase` |

---

## 2. 新增概念（实现形状由开发 agent 决定，语义必须满足）

### 2.1 调度器 `Scheduler`（放在 Host，不放在内核）

- 输入：一个 Run 资源 id。
- 循环：`kernel.open(run)` → 对每个开放空位，若尚无执行者在跑，派给对应执行者 → 收到执行者结果后 `kernel.submit` → 再 `open`。空位为空且相位非 Executing 时退出。
- 版本过期（`StaleVersion`）：重读版本后重新提交，按最新状态裁决。不得绕过。
- 并发：同一 Run 的多个空位可以并发执行（并行分支）；**不要求波次屏障**。
- 结算后迟到的提交被拒（`TaskSettled`），保留在历史中，调度器不得重试。
- 进程重启：调度器从内核读取 Run 相位与开放空位即可恢复；不需要游标文件。已经开始但结果未知的执行（Goose 原生会话中断）按现有"未知效果先核查"语义处理，见 2.3。

### 2.2 执行者 `Executor`

- 输入：`Dispatch`（节点、调用序号、输入产物）+ 冻结的授权。
- 输出：`NodeSubmission`（Agent / Op.run / host operation）或 `CallSubmission`（Op.call，宿主代码）。
- 产物冻结在执行者内完成（`ArtifactPort::freeze`），`commit` 作为证据放进提交。校验器只核对 commit 的节点和序号。
- Agent 执行用 `PrincipalKind::Agent` 提交；fanout/join/call 用 `PrincipalKind::Code`。
- 现有 `NodeExecutionOutcome` 的映射：

| 旧结果 | 新处理 |
| --- | --- |
| `Completed` | 提交 `NodeSubmission` |
| `Failed` | 提交结算：`Settle`，结果为失败（Graph 校验器须支持节点失败结算并加差分测试或单测；另按审查决定 D4 允许 Code 中断事实扩展） |
| `Cancelled` / `Suspended` / `Interrupted` | 不提交节点结果；Run 已由人的 `stop` 进入等待，执行者只需收束 |
| `BudgetExhausted` | 由内核预算定律处理：执行者如实上报成本，内核超预算即结算 |
| `WaitingRecovery`（未知外部效果） | Run 进入等待人（`Waiter::Human`，原因标明未知效果），人核查后 `resume` 或 `abandon` |
| `Yielded` / 中断控制产物（常驻助手打断） | 见阶段 H4 |

### 2.3 中断与未知效果

宪法第二条：未知是正当终点，不自动重放。执行者在已开始、结果未知时：不提交节点结果，Run 进入"等待人"，记录原因与尝试信息（替代旧 `recovery` 字段）。不新增新的内核状态。

---

## 3. 实施阶段

每个阶段结束必须：编译通过、全部测试通过（或在工作记录中逐项登记修改理由）、更新工作记录、单独 commit。

### H1：旧格式清理（已验收）

**决定（2026-10-16）：C6 已由用户明确确认。分支完全不兼容旧格式；旧格式数据在生产迁移时另行处理，不在本阶段。**

- 删除 `migrate_format`、`legacy_environments`、旧授权升级（`can_upgrade_legacy_authorization` 及调用）、`Deleted` 调用标记、旧 cursor 升级。读到旧格式时具名拒绝，错误信息说明是旧格式、需离线迁移，并加对应测试。
- 涉及的旧测试：仅为旧格式兼容存在的测试删除，**在工作记录中逐个列出删除的测试名**。
- **建立直接写入基线清单**：在工作记录中列出 H1 结束时 Host 里每一处 `store.save(run)` / 直接改 `GraphRunRecord` 字段后保存的位置（文件:行 + 用途）。此清单是 H2/H3 的棘轮起点，数量只能减少，不得增加。
- H1 **不要求**接入 `anchor_kernel::FileStore`，也不要求清零直接写入。FileStore 接入随 H2 的调度器一起落地；C2 零直接写入在 H3 结束时验收。
- 中间态单一权威：迁移前 Run 仍由旧 RunStore 负责，不得双写，不得把旧状态包装成内核已裁决事实。

H1 结果、删除测试清单及 16 save / 36 mutation 基线见[工作记录](../runtime-migration-2026-10-20/constitution-refactor-log.md#h1旧格式清理实现完成)；验证范围见[H1 证据](../../../experiments/constitution-host-preflight/h1/verification.md)。

### H2：调度器 + 执行者，替换 `GraphRunner`

- 实现 2.1、2.2。
- `execution.rs` 与 `graph_call.rs` 不再构造 `GraphRunner`。
- 节点失败结算（2.2 表中 `Failed` 行）在校验器中实现。
- **对照组**：在删除旧 Runner 之前，三组差分测试必须继续通过；另外新增一组"Host 级"差分或回放测试：用确定性 fixture 节点，同一 Graph 分别经旧 Runner 与新调度器执行，比较派发序列与最终 Graph 事实。

### H3：生命周期前置条件改读内核

- `control.rs`、`abandon.rs`、`assistants.rs`、`recovery.rs`、`deletion.rs` 及其他按 `RunStatus` 分支的位置，改为读取 `Phase` 并通过内核生命周期提交。
- 删除 `RunStatus`。
- 每个原先手写的前置条件，若能由内核定律推出（例如"已结算不可恢复"），删除手写代码；若是 Session 层事实（例如退役需要无未结算投递），保留在 Session 层。
- 记录：每个文件删除了哪些判断、哪些保留及理由。

### H4：常驻助手的打断与中断产物

- 旧路径：新消息打断 → `Yielded` → `Interruption` 控制产物 → 沿普通 route 到 reply。
- 新路径要求：打断是宿主代码的提交，产生中断事实；不把旧回复记为成功；沿用现有 suppressed 与投递账本语义。
- 必须让 `goose_persistent_assistant` 相关确定性测试通过（需要固定 Goose 二进制与 bwrap，按 `docs/runtime-contract-tests.md` 准备环境；无法运行时如实记录"未执行"，不得声称通过）。

### H5：删除旧 Runner

- 删除 `runner.rs`、`parallel.rs`、`RunStore` 及只为它们存在的类型（`RunControl`、`RunCursor`、`CompletionFact` 等，以实际可删为准）。
- 差分测试此时失去对照组：把差分测试保留为"新侧自洽性测试"或删除，二选一并在工作记录说明。保留 20,000 例随机 Graph 的派发轨迹快照作为回归基线是推荐做法。

### H6：测量探针接入（小切片）

- Host 为内核注册一个 `Probe`，把每个测量点写入一个只读投影（JSONL 即可）。
- 不新增任何对外 API。目的仅是证明"一个探针覆盖全部行为"。

---

## 4. 验收标准（审查时逐条核对）

### 4.1 测试

| 项 | 要求 |
| --- | --- |
| `anchor-kernel` 测试 | 全部通过 |
| `anchor-runtime` 测试 | 全部通过 |
| Host + GraphHost all-targets | 全部通过；与基线（1257 通过 / 112 忽略）相比的每一处增删改都在工作记录中有条目 |
| 差分测试 | H2 完成前三组全部通过；H5 的处理方式有说明 |
| 固定 Goose 确定性回归 | 实际执行并通过，或如实记录未执行及原因 |
| clippy `-D warnings`、fmt | 通过 |

基线中 12 项网络测试失败是环境问题：运行前先 `cargo build -p anchor-sandbox-bwrap --bins`。

### 4.2 结构

| 检查 | 方法 | 期望 |
| --- | --- | --- |
| 内核零业务（C1） | `anchor-kernel/Cargo.toml` 依赖；`git diff 7c196d8 -- rust/anchor-kernel` | 依赖不变；内核改动为零或每处都有工作记录条目 |
| 唯一入口（C2） | 搜索 Host 中对 Run 存储的直接写入 | 零处 |
| 无停止轮询（C4） | 搜索 `stop_requested`、`pause_requested` | 零处 |
| `RunStatus` 已删除 | 搜索 `RunStatus` | 零处 |
| 旧格式已删除（C6） | 搜索 `migrate_format`、`legacy_environments`、`can_upgrade_legacy_authorization`、`GraphCallStatus::Deleted` | 零处；旧格式读取有具名拒绝测试 |
| 旧 Runner 已删除 | `runner.rs`、`parallel.rs` 不存在 | 是 |

### 4.3 度量（写入工作记录）

- 同一规则的独立判断点数量：终态判断、生命周期前置条件，迁移前后各多少处。
- 删除与新增的行数（按 crate）。
- 不以行数为通过条件，但新增显著多于删除时必须解释。

### 4.4 语义变更

所有行为变化必须登记到工作记录的语义变更表，延续编号 S10 起。未登记的行为变化视为缺陷。

---

## 5. 停止条件（遇到即停，交回讨论）

- 需要修改 `anchor-kernel` 才能继续（C1）。
- 某个现有测试无法通过，且无法归入"旧缺陷"或"宪法带来的有意改变"。
- 某个 Host 行为在新模型中找不到对应的原语表达。
- H2 完成后，Host 测试失败数量超过 20 且无法在合理时间内收敛。
- 任一阶段完成后，同一规则的独立判断点数量没有下降。

遇到停止条件时：提交当前可编译的状态（可以是 WIP commit），在工作记录写明卡在哪里、尝试过什么、需要什么决定。

---

## 6. 交回审查时请提供

```text
完成阶段：H1 / H2 / H3 / H4 / H5 / H6（未完成的说明到哪一步）
commit 列表：每个阶段一个或多个，附一句话说明
测试结果：实际执行的命令与通过/失败/忽略数；固定 Goose 回归是否执行
结构检查：4.2 每一项的实际搜索结果
度量：4.3 的数字
语义变更：新增的 S 编号列表
删除的测试：名称与理由
停止条件：是否触发，触发点
未决问题：需要架构决定的事项
```

---

## 7. 已知的待决定事项（不阻塞开发，审查时讨论）

- **S4**：退役之后是否还允许 abandon。H3 实施时暂按"已结算不可逆"处理，即不允许，并登记为语义变更。
- 调度器是否应放在一个独立 crate（例如 `anchor-scheduler`）而非 Host 内部。本阶段放在 Host，H5 后再评估。
- 测量投影的落盘格式与位置。H6 只做最小实现。
