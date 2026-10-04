# Rust Harness 候选调研

核查日期：2026-10-02。目标是寻找能承担 Anchor 长任务上下文、持久化、恢复和工具效果边界的 Rust Harness，再判断它是否能与 Rig 组合。

## 第一候选：io-harness 0.86.0

仓库：[initorigin/io-harness](https://github.com/initorigin/io-harness)，release [v0.86.0](https://github.com/initorigin/io-harness/releases/tag/v0.86.0)，crates.io 下载量在本次查询约 4,790，Apache-2.0，Rust 最低版本 1.95。

社区信号（2026-10-03 查询）：GitHub 约 **4 stars、0 forks、4 open issues、1 subscriber**；仓库创建于 2026-07-22，最近一次 push 为 2026-10-01。v0.78.0 到 v0.86.0 在 2026-09-04 至 2026-09-10 密集发布，说明作者迭代很快，也说明 API 稳定性尚未经过长期社区验证。作为参照，Rig 约 8,792 stars/986 forks，PydanticAI 约 20,355 stars/2,847 forks，PydanticAI Harness 约 940 stars/148 forks。Star 数不能证明技术质量，但 io-harness 当前明显属于作者主导的早期项目，不能按成熟基础设施依赖对待。固定发布包已下载到 `.local/harness-candidates/io-harness-0.86.0`，主线当前 Rust 也是 1.95。`cargo check --lib` 通过；发布包历史首轮库测试为 719 通过、8 个忽略、2 个失败，后续同目录复跑为 720 通过、8 个忽略、1 个失败。剩余失败为 `sandbox::linux::...namespace...`：裸 user namespace 探针正常，但测试在 `/root/nas` CIFS 挂载上执行 remount,bind,ro 失败；`verify::...command_gate...` 连续复跑通过。这里保留失败事实，不把它写成上游全量通过，也不降低上游安全测试要求。

它是真正的长任务 Harness 候选，而不是单纯模型 SDK。源码提供：

- `Store` + SQLite trace、step records、provider calls、memory、summaries、leases、approvals、budgets、sessions 和 checkpoint 数据。
- 每个完成 step 将 trace、预算和 checkpoint 一起提交；`resume`/`resume_tree` 从原始 Run 继续，已完成 step 不重跑。
- 工具有 `ToolEffect::{ReadOnly, Mutating, ...}`、`ToolRecovery::Indeterminate`；未知效果会进入 `AwaitingRecovery`，由操作员选择 `Completed`、`Retry` 或 `Abort`。文档明确承认“journal commit 与外部调用之间仍有窗口”，没有伪称 exactly-once。
- 每次请求按 `ContextBudget` 组装上下文；观察项有替换/失效、单项边界和总预算。默认 compaction 在 ledger 超过份额时用一次模型调用生成摘要，摘要写入 `summaries`，恢复/分支/回放复用已购买的 fold。
- workspace memory 有大小和数量上限、证据权重驱逐、pin、recall 记录和 durable memory。
- SQLite durable lease 保证一个 Run 只有一个 driver；跨平台 sandbox backend（Linux/macOS/Windows）及 MCP stdio/HTTP；工具、权限和拒绝都进入 trace。
- 代码为嵌入式库，`Harness` 绑定 Provider、Store、Policy、Approver、Observer 和默认 TaskContract；没有必须启动的独立 daemon。

主要源码证据：`src/run/memory.rs`（预算、compaction、memory watermark）、`src/run.rs`（run/resume/recovery）、`src/state.rs`（Store、Run/Step/ToolAttempt/Summary）、`src/tools/custom.rs`（effect/recovery）、`src/harness.rs`（嵌入式入口）、`docs/guide/context-and-memory.md`、`docs/guide/durable-runs.md`。

## Rig 组合方式

`io-harness` 当前还会带来独立的依赖边界：它使用 `rmcp 3.x`、`reqwest 0.13` 和 bundled SQLite；Anchor/Rig 当前固定 `rmcp 2.2`、`reqwest 0.12/0.13` 两条线。放在同一进程时 Cargo 可能同时编译两代 RMCP，MCP 类型不能直接互传，二进制体积和安全更新面也要单独评估。这不是阻断组合的语言问题，但属于适配 spike 的必测集成成本。

`io-harness` 的 `Cargo.toml` 没有 Rig 依赖。它定义自己的：

```text
io_harness::Provider
io_harness::CompletionRequest / CompletionResponse
io_harness::Tool
io_harness::TaskContract / run / resume
```

因此它不能直接把 Rig 的 `AgentRun` 放进自己的上下文管理层，也没有现成的 `RigProvider` 或 `RigTool` 适配器。理论上有两种组合方式：

1. **io-harness 作为外层 Agent loop，Rig 作为 Provider。**实现 `io_harness::Provider for RigProvider`，把 io-harness 的消息、工具调用和 usage 转为 Rig completion request/response；再把 Anchor Plugin/Node 工具实现为 io-harness `Tool`。此时 io-harness 的 loop、上下文、SQLite、恢复拥有权威，Rig 主要提供模型/provider。Rig 自己的 `AgentRun`、memory、cassette 和工具循环不再同时驱动。
2. **Rig 作为外层 Agent loop，io-harness 只提供上下文或 Store。**当前 io-harness 没有独立的、面向 Rig `RequestPatch`/每请求历史注入的适配层；它的 compaction、工具 ledger 和 recovery 与自己的 `run` loop 强绑定。要这样组合，需要拆解或新增接口，工作量接近实现一层新 Harness。

所以答案是：**可以组合，但不是无缝组合。推荐验证方案是第一种：以 io-harness 作为 AgentRuntime/loop，Rig 作为 Provider 适配层；Anchor GraphRunner 仍作为更外层 Graph 调度，Anchor Sandbox/Artifact/Run facts 仍是产品事实边界。**

这个 Provider adapter 不是无损转换。io-harness 的 Message/ToolCall/ToolResult 主要是文本和 JSON，工具结果按位置关联；Rig 可以携带 structured/media/reasoning 内容、provider response identity、stream event、output schema 和 tool mode。io-harness 的 `Tool::invoke` 只返回 `String`，`ToolEffect`/`ToolRecovery` 也必须由适配器重新声明。因此第一阶段可以覆盖文本、JSON工具结果、图片和流式文本；io-harness源码已有 `Media` 图片边界（JPEG/PNG/GIF/WebP、大小/像素限制）及 Provider 流式接口。仍需对复杂结构化输出、部分 provider identity 语义明确拒绝或降级，工具结果关联按位置而非Rig provider call id。

io-harness 的默认 `Compaction { at_share: 0.8, keep_recent: 8 }` 使用一次模型调用生成 fold，并把 summary、step、budget 和 checkpoint 写入 SQLite 事务；`ToolRecovery::Indeterminate` 会进入 `AwaitingRecovery`。这些能力比 Rig 当前现成组合更接近 Anchor 需求，但仍需验证其事实与 Anchor Graph/Artifact/NodeExecutionPort 的映射。

需要特别防止三套事实/循环同时存在：

- 不让 Rig AgentRun 和 io-harness run loop 同时处理同一模型/工具调用。
- 不让 io-harness SQLite trace 取代 Anchor Graph Run、Artifact 和外部副作用事实；它应作为 AgentNode 内部执行记录或由 Anchor 绑定 run/node identity。
- 不让 io-harness 自己的 sandbox/Plugin 权限绕过 Anchor。第一阶段应把其工具调用委托到 Anchor Tool/Sandbox port，或明确它只运行在 Anchor 已授权的隔离工作区。

## 其他候选

| 候选 | 判断 |
|---|---|
| `agent-harness-core` v0.13.1 | 更像 Codex/OpenRouter/Telegram/Discord 的运营网关，拥有队列、receipt、session 迁移；不是通用可嵌入 Rig Harness，和 Anchor Graph/Plugin 边界重叠较大。预发布，Rust 1.96。 |
| `mra` 0.1.1 | 轻量 headless agent，Tokio supervision/session/tool use；生态很小（本次 crates.io 下载量约33），未看到与 Anchor 所需 durable compaction/effect recovery 相当的证据。 |
| `swiftide` 0.32.1 | 强项是 indexing、streaming、RAG 和 agentic pipeline，不是带未知副作用恢复的长期 Harness。 |
| `llm-chain` 0.13.0 | LLM chain 编排，缺少 Anchor 所需 durable Run/Tool effect/recovery 组合。 |
| `agentic` 0.0.4 / `aisdk` 0.5.2 | Provider/Agent/MCP 应用层库，未证明完整长期 Harness。 |
| `tinyflows` 0.8.6 | Agentic workflow 管理，需另验恢复与上下文边界；当前证据不足以进入首选。 |

## 推荐下一步

不要立刻把 io-harness 接入生产 Graph。先做独立适配 spike：

1. 在 `/tmp` 建最小 crate，实现一个无网络 scripted `io_harness::Provider`，验证 Rig 模型响应到 io-harness tool call/response 的双向映射。
2. 把一个 Anchor `Op/Agent` 工具包装为 io-harness `Tool`，保留 NodeSandbox、Plugin allowlist、started/completed/unknown effect 语义。
3. 使用 io-harness SQLite Store 跑 30 次工具循环、触发 compaction，杀进程后 resume；验证摘要/ledger、Anchor checkpoint 和工具效果没有双写或重放。
4. 用真实 Provider + 本地 MCP fixture 做一次端到端，再决定是否让 io-harness 成为 Rust `AgentRuntime` 适配器的首选。

这项 spike 的出口是“适配成本和语义边界已证明”，不是把 io-harness 的 README 当作产品验收。当前没有修改生产代码、Graph、依赖或计划。
