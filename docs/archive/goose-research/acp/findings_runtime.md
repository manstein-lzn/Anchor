# Goose Rust Runtime 复用：初步 findings

> 历史调研：结论限于当时检查的版本和边界，当前实现与验收见开发台账。

参考日期：2026-10-06。仅核查官方仓库、固定版本源码、文档和发布记录；未构建、未调用模型、未 clone。本文不代表 Anchor 接入或迁移验收。

## 主结论

**Goose 有真正可进程内嵌入的 Rust Agent loop，值得作为候选；入口不是默认功能的 `goose-sdk`，而是 `goose-agent` + `goose-provider-types`，按需增加 `goose-providers` / `goose-context-management`。** 不能因为完整 `goose` 产品库较大，就认定只能通过 CLI 子进程复用；也不能把 SDK、完整产品 Runtime、ACP 接入当成同一种能力。[S2–S5]

**目前证据不足以认定它比现有内核更能减少 Anchor 的剩余工作。** 它可省 Agent loop、模型适配和压缩的实现；未证明能省掉 Graph/Run、Library/授权/引用链、渠道、预算、Artifact/Sandbox、生产迁移和故障窗口验收。尤其不能把持久化会话 history 当成 pending 外部副作用的安全恢复。这个判断基于能力边界，不是沉没成本；保留低成本、零模型 spike 的价值。[S4–S9；收益比较为推断]

## 五个关键事实

### 1. 固定版本、组成与成熟度

- **确定**：核查时官方 `/releases/latest` 为稳定 `v1.53.0`，`published_at=2026-10-02T18:44:11Z`；tag 指向 `76da81cb964b21cd096db739302329b40c2998b8`。该 commit 的 author 日期为 `2026-09-30T17:51:51Z`、committer 日期为 `2026-09-30T18:19:01Z`；不要把 commit 日期写成发布日期。[S1]
- **确定**：当时 main 是 `540df77c30e3c1f3b51915cb18ae081931c96851`，日期 `2026-10-06T04:24:32Z`，提交题为 ACP client extension selection 的 exact session set 修复。本文 Runtime 能力以 **v1.53.0** 为准，不把该 main 修复算进 release。[S1]
- **确定**：核心、CLI、ACP server 和多个独立 GDK crate 是 Rust；桌面是 Electron/React/TypeScript，SDK 的跨语言绑定为 UniFFI Python/Kotlin，不是 TS Agent loop。根 manifest 声明 `Apache-2.0`、Rust MSRV `1.94.1`；`LICENSE` 是 Apache 2.0。这里不是分发依赖许可证审计。[S2、S3]
- **确定**：v1.53.0 源树中的 GDK crate 版本为 `0.1.0-alpha.11`，不是产品版本 `1.53.0`；`goose-agent/CHANGELOG.md` 将 alpha.11 记为 2026-09-28。`release-plz.toml` 为 GDK 配置独立 `version_group="gdk"`；可核查公共接口和发布机制，但 **alpha 不等于稳定嵌入契约**。[S2、S4]
- **未验证**：未核对 crates.io 实际发布 artifact 与此产品 commit 的字节一致性、所有 GDK tag 的发布时刻和后续 API 兼容性。搜索/文档索引可能落后；落地应固定 commit 或实际可核查的 GDK artifact，不能只用 `latest` 文档推断。[S1、S2]

### 2. 嵌入边界：SDK、Agent kernel、完整产品库不同

- **确定**：`crates/goose-sdk/src/lib.rs` 默认仅 re-export ACP shared wire types；`uniffi` 功能开启 provider 构造/流式调用等跨语言表面。官方 `documentation/docs/gdk/sdk/index.md` 的 Rust 用法也要求 `--features uniffi`。**单独依赖默认 `goose-sdk` 不会获得完整 Agent loop。**[S3]
- **确定**：`goose-agent` 公共模块包含 `machine`、`operation`、`inference`、`tool`、`events`。关键接口为 `StateMachine<S,E>`、`Step`、`Operation`、`InferenceRunner::new(Arc<dyn Provider>, ModelConfig)`、`ToolOperation<S>` / `ToolProvider<S>`、`MachineSession` / `SessionLoader` / `EffectHandler`、`Emitter` / `ConversationEffect`。[S4、S5]
- **确定**：`goose-provider-types::base::Provider` 的主入口是 `stream(model_config, system, messages, tools) -> MessageStream`；`complete` 默认收集该流，`resume` 默认 no-op。`goose-providers` 提供原生模型 transport；`goose-context-management` 独立提供 `summarize`、`compact`、`CompactionInput/Output`、`CompactionModel`、`CompactingProvider`，默认压缩阈值为 context 的 0.8。压缩触发、写回和使用何种模型仍需调用者接线。[S5、S6]
- **推断**：这些接口足以在同一 Rust 进程中组装 Node Runtime，不需要 Goose CLI、TS/Electron 或 ACP provider。Anchor 需要会话/效果持久化适配、工具适配、完成/取消/预算契约，并固定 `rmcp`、message schema、Provider 和 Rust 版本；这不是“一个 SDK 调用替换完整内核”。
- **确定**：完整 `crates/goose` 公共导出 Agent、Session、Config、Provider、Extension、ACP 等产品模块，能提供更多现成行为，但带来 Goose 的 SQLite 会话、配置/权限、extension 和宿主概念。`goose-agent/README.md` 明确把它称为参考组装；采用轻量 crate 不会自动继承完整产品的 approval/extension 行为。[S4、S7、S8]

### 3. 持久化循环可复用；未知副作用仍是恢复缺口

- **确定**：`goose-agent/src/machine.rs` 的 `run` 每轮 `load -> step -> apply_effects`，重载会话而不缓存；会话存储由调用者实现。`Operation` 支持把动作记录写入 message metadata，供根据持久 history 重建决策。完整 Goose 的 `SessionManager` 使用 `sessions/sessions.db`、SQLite WAL，公开 `add_message` / `replace_conversation`；不是轻量 Agent crate 内置的通用 StepStore。[S4、S7]
- **确定**：`goose-agent/src/tool.rs` 按当前 kickoff 后的 ToolRequest 与 ToolResponse ID 找未回答请求，再调用 `ToolProvider::call`。它执行工具后构造结果 message；同批多个结果最后组成 effect，随后才由 machine 的 `apply_effects` 持久化。**工具外部执行与 effect 落盘不构成一个原子事务。**[S4、S5]
- **推断，重要**：工具已产生外部副作用但 ToolResponse 尚未提交时杀进程，恢复同一 kickoff 可再次看到未回答请求并重放；批内前部工具成功、后部失败/中断也可能留下这种窗口。新 kickoff 又不等价于续跑原 pending；`inference.rs` 会从发给 provider 的历史中过滤旧 turn 未回答工具请求。不能因此宣称 exactly-once、自动核验现场或可靠副作用恢复。[S4、S5]
- **确定**：取消为 cooperative；原生同步工具通过 `spawn_blocking` 执行，取消等待不证明阻止了已经开始的副作用；WASM 同步工具直接执行，不能中途取消。取消工具结果中的 interrupted 文本不是“副作用未发生”的证据。[S4、S5]
- **未验证**：未跑任何进程 kill/重启矩阵、lease/CAS、多写者、工具幂等键、外部事务/查询式核验、压缩与 pending 联合场景。完整产品 state-machine 的 toolcalling 有按 history 计算 pending 的代码，但本次没有足够证据证明它另行关闭了上述窗口。Anchor 或具体工具仍需定义 unknown-result 的核查/继续策略。[S7]

### 4. 权限、沙箱和 lean ACP-only 的准确边界

- **确定**：轻量 `ToolOperation` 调用宿主提供的 Rust/MCP 工具接口，本身不是 OS 沙箱。Goose 提供 `Auto`、`Approve`、`SmartApprove`、`Chat` 模式；完整产品具有 `ops_tool_approval.rs` 等步骤，权限模式不能替代 Anchor 的冻结授权、workspace 隔离和 Plugin 能力边界。[S5、S8]
- **确定**：v1.53.0 的 lean ACP-only 不是“只代理外部 Agent”。官方 PR #11961 和 `crates/goose/src/bin/goose-acp.rs` 表明，它是精简的 Goose stdio ACP server；PR 明确保留 persistent SQLite sessions、providers、external MCP 和 in-process developer extension。它属于 Goose 原生运行时的瘦入口，不能与 Goose 内部外部 ACP provider 混淆；PR 的尺寸数据未在本次构建复测。[S8]
- **推断**：ACP 接入可复用完整 Goose 产品行为，但改变进程、权限和会话所有权边界；进程内 GDK 更适合保留 Anchor 的事实所有权，不过应显式补 approval/提问、MCP 生命周期、Sandbox 和恢复适配。两者都不能靠协议或 history 直接交付 Anchor 的剩余产品闭环。

### 5. 零模型测试可行，而且存在录制/回放 Provider

- **确定**：`InferenceRunner::new` 接收 `Arc<dyn Provider>`，所以可以注入确定性的 `Provider::stream`；`SessionLoader` / `EffectHandler` 与 `ToolProvider` 允许记录 history、tool 调用和 workspace 结果。该结论来自公开类型/源码，不是已完成 Anchor fixture 验收。[S4、S5]
- **确定**：`goose-test-support` 当前导出 MCP fixture、session-ID 检查和 OTel 辅助；`goose-test` 的 playback 是 **MCP stdio** 录制回放，不能误称模型 replay SDK。[S9]
- **确定**：真正模型录制回放位于完整产品库的公开模块 `goose::providers::testprovider::TestProvider`，有 `new_recording(inner, file_path)`、`new_replaying(file_path)` 和 `finish_recording`。记录 input/output，replay 匹配对 message role/content 作 hash 并规范化部分 metadata；不是强校验全部 system/tools 的测试替代品，也没有证明覆盖 chunk 时序。它可以作为候选测试接口，但直接使用会引入完整 `goose` 依赖，而非仅 `goose-agent`。[S9]
- **推断/建议**：低成本对比可只替换模型传输，保持 Host → GraphRunner → Node port → Goose loop → 真工具/沙箱路径：用本地确定性 HTTP Provider 检查 provider adapter，或直接 `Provider` fake 检查 loop。后者不会覆盖 HTTP/parser。重点验收完成工具、提问续答、预算/取消和“副作用已发生、结果未提交”窗口；检查真实 node history、tool result、workspace，而不是仅数模型轮次。本次没有实现或运行这些测试。

## 对 Anchor 剩余工作的含义

| 可省 / 可借用 | 仍需 Anchor 持有或接线 |
| --- | --- |
| 已有源码实现的 loop 状态机、tool dispatch、流式推理、provider transport、compaction、测试 replay 接口 | Graph/Run/Node 完成语义、Artifact/workspace、Plugin/Library 授权与引用链、Session/渠道归属、预算、Sandbox、未知副作用核验、迁移/生产验收 |
| 完整 Goose 的会话/权限/extension 行为；但带入产品宿主耦合 | 是否采用这些行为及其持久事实/权限契约，不能直接替代 Anchor |

**初步排序**：把轻量 GDK Rust crates 列为有效候选，比较剩余产品契约和失败窗口的净减少量；目前不建议仅凭现成 loop/compaction/history 宣称应替换现有内核。F2–F6 的具体验收归主 Agent 核对，本子任务不重复 Anchor 架构分析。

## 官方证据与读取方式

本次共尝试 5 条 web search query；web 回包为空或不可确认，没有可供引用的可靠 web 引用 ID。以下证据均实际通过 `curl` 读取官方 API/raw；raw 超时处改用 GitHub Contents API 的 base64 内容。URL 的 source path 固定在 v1.53.0（上文 main 修复除外）。未依赖第三方结果；没有将读取失败的页面当成源码证据。

- **S1 版本事实**：https://api.github.com/repos/aaif-goose/goose/releases/latest ；https://github.com/aaif-goose/goose/releases/tag/v1.53.0 ；https://api.github.com/repos/aaif-goose/goose/git/ref/tags/v1.53.0 ；https://api.github.com/repos/aaif-goose/goose/commits/76da81cb964b21cd096db739302329b40c2998b8 ；https://api.github.com/repos/aaif-goose/goose/commits/540df77c30e3c1f3b51915cb18ae081931c96851
- **S2 组成/版本/许可证**：https://github.com/aaif-goose/goose/blob/v1.53.0/Cargo.toml ；https://github.com/aaif-goose/goose/blob/v1.53.0/LICENSE ；https://github.com/aaif-goose/goose/blob/v1.53.0/ui/desktop/package.json ；https://github.com/aaif-goose/goose/blob/v1.53.0/release-plz.toml ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-agent/CHANGELOG.md
- **S3 SDK 实际表面**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-sdk/src/lib.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-sdk/Cargo.toml ；https://github.com/aaif-goose/goose/blob/v1.53.0/documentation/docs/gdk/index.md ；https://github.com/aaif-goose/goose/blob/v1.53.0/documentation/docs/gdk/sdk/index.md
- **S4 loop/持久化 Ports**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-agent/README.md ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-agent/src/machine.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-agent/src/operation.rs
- **S5 工具/推理/Provider**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-agent/src/tool.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-agent/src/inference.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-provider-types/src/base.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-providers/src/lib.rs
- **S6 压缩**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-context-management/README.md ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-context-management/src/lib.rs
- **S7 完整产品及 session**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/lib.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/session/session_manager.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/state_machine/mod.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/state_machine/effects.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/state_machine/ops_toolcalling.rs
- **S8 权限及 lean ACP**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-provider-types/src/goose_mode.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/state_machine/ops_tool_approval.rs ；https://api.github.com/repos/aaif-goose/goose/pulls/11961 ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/bin/goose-acp.rs
- **S9 测试接口**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-test-support/src/lib.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-test-support/src/session.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-test/src/mcp/stdio/playback.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/providers/testprovider.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/providers/mod.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-cli/src/scenario_tests/scenario_runner.rs

读取来源示例：`https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose-agent/src/tool.rs`；Contents API 示例：`https://api.github.com/repos/aaif-goose/goose/contents/crates/goose/src/providers/testprovider.rs?ref=v1.53.0`。完整产品未承诺稳定公共 API、完整沙箱/恢复矩阵等未经充分读取或执行验证的判断，均不作为已确认事实。
