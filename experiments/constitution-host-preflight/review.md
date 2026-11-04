# Host 接入：停止条件审查产物

日期：2026-10-16。源码基线：`d0f9b61`（计划基点 `7c196d8` 之后仅增加接入计划）。主 Agent 负责集成判断，两个 subagent 分别核查内核表达/反例及 Host 所有权/写入路径。**H1 / H2 / H3 / H4 / H5 / H6 均未实施**。

## 交回原因与建议

[实际公开 API 观察](evidence.json)复现了两个必须修改内核才能解决的缺陷：

1. 预算 10，三次各报告成本 6 的 `ContentInvalid` 提交被打回后，`spent=0`，相位仍为 `Executing`。`Kernel::adjudicate` 在局部资源上累计成本，却在拒绝/打回分支丢弃该状态。
2. 文件历史已有残缺尾行时，后续 `stop` 接受且状态版本变为 1，但 `history` 长度仍为 0。仅在私有 tempfile fixture 内恢复旧状态文件，模拟记录落盘而状态写入丢失，重新构造 Kernel 后得到版本 0、`Executing`；停止事实未从记录恢复。此为文件/重启模拟，未进行真实断电试验。

另有待定契约：Code 报告未知效果时，`Request{control:"stop"}` 被 `HumanOnly` 拒绝。GraphValidator 只接受普通节点/调用写入，也不能表达该等待。这不是把 Code 盖章为 Human 的理由。H4 中断亦未表达：`NodeSubmission` 无 interruption，`record_result` 固定写 `None`。普通成功结果不能代替中断事实。

触发[计划](../../docs/archive/design-records-2026-11-04/constitution-host-integration-plan.md) C1 和第 5 节：**“需要修改 `anchor-kernel` 才能继续”**。依约停止实现，交付可编译的预检/证据提交；未添加任意覆盖 Run 的 Validator、停止轮询或身份绕过。

建议审查放行的具体范围：

- 内核通用成本累计：已发生且由宿主观测的成本不能因内容被打回而消失；明确 rejected/stale/重试及重启的计费与版本语义，同一份已上报成本重试不能重复计费。
- 内核文件存储：残缺尾行后仍能可靠追加完整记录，并在状态写入丢失后恢复已接受改变；不得改写既有完整裁决记录。修复须覆盖续写、重启和后续再次提交。
- 通用宿主阻塞观察：建议允许真实 Code 从 Executing 提交有观察引用的等待事实，进入已有 Waiter；Human 的停止/恢复/放弃权保持明确，Code 不可释放人的等待。不加入 Graph 或 Goose 专属内核分支。
- 校验器扩展范围：明确允许 H4 的 Code 中断事实，保留 interruption Artifact、suppressed 与投递账本；这需要修订计划中“节点失败结算为唯一允许扩展”的限制。

以上均为待审建议，未实现，未新增 S10 起的行为变更。

## 实际验证

以下从仓库根执行；没有使用真实 provider，没有加载 `.env`。

| 命令 | 实际结果 |
| --- | --- |
| `cargo test --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime --locked` | 退出 0；Kernel 17、Runtime 122，合计 139 通过，0 失败，0 忽略；doc tests 均 0 |
| 上述命令内的三组差分 | 串行 10,000、并行 5,000、调用 5,000 例均通过；旧 Runner 对照仍在 |
| `cargo clippy --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime --all-targets --locked -- -D warnings` | 退出 0 |
| `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` | 退出 0 |
| 独立 probe 的 fmt/check/clippy/run | [README](README.md) 中五条命令均实际执行、退出 0；run 的原始 stdout 为 [evidence.json](evidence.json)，退出 0 只表示反例观察执行成功 |
| `git diff --check` | 退出 0 |

Kernel/Runtime 测试、Clippy、fmt 的原始终端输出仅保留在本次会话，没有单独日志文件；不冒称仓库内存在可获取的原始日志。probe 原始 JSON 已入库。

未执行 Host/GraphHost all-targets、workspace Clippy、固定 Goose `goose_persistent_assistant` / 全套确定性回归、Web 与真实 provider 验收。原因是接入前已触发停止条件，未改变对应实现；`bwrap` 可用，`ANCHOR_GOOSE_BINARY` 本次未设置，未检查或构建固定 Goose。不能把本次 139 项定向测试替代计划中 1257 / 112 的 Host 基线或 H1–H6 验收。

## 结构检查与迁移基线

`git diff --exit-code 7c196d8 -- rust/anchor-kernel` 退出 0：内核代码及依赖零变更。其余是停止时的现状，**未达到迁移验收要求**。

| 计划 4.2 检查 | 实际结果 |
| --- | --- |
| 内核零业务 / 零改动 | 内核零 diff，依赖未改变 |
| Host 对 Run 直接写入 | 仍有 16 处 / 10 个生产文件；其中 5 次初次接纳、11 次后续改写 |
| 无停止轮询 | 未达成；Rust 源码中 `stop_requested` 命中 11 文件，`pause_requested` 命中 10 文件 |
| 删除 RunStatus | 未达成；Rust 源码中命中 51 文件；生产 `RunStatus::` 引用详见下表 |
| 删除旧格式兼容 | 未达成；`migrate_format` / `legacy_environments` / `can_upgrade_legacy_authorization` / `GraphCallStatus::Deleted` 分别命中 4 / 1 / 4 / 4 个 Rust 文件 |
| 删除旧 Runner | 未达成；`runner.rs`、`parallel.rs` 都存在 |

上述模式文件数覆盖 `rust/anchor-*/**/*.rs`、排除 target，包含测试；直接写入与下表另排除测试。文件数不等于独立业务规则数。

| 生产范围 | `RunStatus::` 引用 / 文件 | `.is_terminal()` 调用 |
| --- | --- | --- |
| RunnerHost | 121 / 19 | 24 |
| GraphHost | 18 / 1 | 0 |
| Runtime | 49 / 3 | 1 |
| 合计 | 188 / 23 | 25 |

生产计数口径：遍历三个 crate 的 `src/**/*.rs`，排除路径含 `test` 及 `differential.rs`，移除外置 `#[cfg(test)] mod name;`，在首个内联 `#[cfg(test)] mod name {` 前截断，然后计数。为文本统计，未使用 Rust AST。25 个终态调用复用同一个 `RunStatus::is_terminal` 定义，不能算作 25 个独立规则实现。生命周期前置条件的逐规则语义统计未完成；没有迁移前后下降数据，不宣称阶段完成。

16 处直接保存位置（相对 `src/`，基线行号）：RunnerHost `execution.rs:312`、`main.rs:663`、`application/abandon.rs:164`、`admission.rs:103`、`assistants.rs:227`、`control.rs:126/137/146`、`conversations.rs:855`、`recovery.rs:139/145/150`、`session_calls.rs:184/211/392`；GraphHost `graph_call.rs:532`。五次首次接纳位于 admission、assistants、conversations、main、graph_call，可映射到 `Kernel::create`；不得用任意状态覆盖提交冒充其余路径的 C2 收敛。

按生产 crate 的新增 / 删除行数：anchor-kernel **0 / 0**，anchor-runtime **0 / 0**，anchor-runner-host **0 / 0**，anchor-graph-host **0 / 0**。唯一新增 Rust 代码是独立反例 probe；另含其锁文件、原始 JSON 和审查说明，所以本次新增多于删除。它不属于完成的迁移阶段，也不新增运行时机制。

现有测试增删改为 **0**，删除测试清单为空；没有更改断言、删除兼容测试或更换差分对照。正式测试数相对任务开始无变化，Host 基线本次未重跑。单独预检提交不标为 H1；commit 身份见本次交付信息。

## 继续实施时保留的边界

同一 state root 共享唯一 Kernel，保留 Host 进程所有权/租约；不能为每个 PreparedExecution 新建独立 writer。未知执行从 Goose/宿主事实核查，不因 frontier 仍开放而自动重放。接受停止后再取消执行者，覆盖迟到 Op 结果的竞态；probe 的迟到 Code 观察不单独列为必须改内核的理由。退役/删除保留执行登记与未结算投递等 Session 前置条件，不能把 `Waiting` 当成执行者已静止。

完成内核边界审查后才重新进入 H1，依序保留 H2 的旧 Runner 对照、H3 的逐规则删除/保留理由、H4 的实际固定 Goose 场景与 H5 的差分基线处理。当前计划仍是未关闭事项，未归档为完成。
