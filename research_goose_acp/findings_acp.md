# Goose + ACP：首轮接口与安全/恢复边界

参考日期：2026-10-06。结论来自官方协议与固定源码静态阅读；未启动模型、未构建、未执行 fixture，不代表 Anchor 已接入或生产验收。

## 版本与取证范围

- 发布基线：`v1.53.0`，tag commit `76da81cb964b21cd096db739302329b40c2998b8`；发布时间 `2026-10-02T18:44:11Z` 由主 Agent 核实。
- 对照 main：`540df77c30e3c1f3b51915cb18ae081931c96851`，`2026-10-06T04:24:32Z`，提交 `fix(acp): treat a client extension selection as the exact session set (#12548)`。下述 extension 修复不在 v1.53.0 的对应源码内。
- 协议参考固定到官方仓库 `06128106cf54b806a491a0fcf2b6bc7022ef57ca`（2026-10-06）。Goose 发布源码使用 `schema::v1`；schema 依赖 `=1.9.1`、ACP Rust SDK 依赖 `2.2.0`，**SDK 版本不是协议 v2**。[S1][S5]
- web 共查询 5 个搜索词，工具未返回可确认的来源/引用 ID；转用 `curl` 读取官方 API、官方 raw 源码与协议仓库。以下 URL/路径是实际核查依据，不提供臆造的 web 引用 ID。

## 已核实的五个关键事实

### 1. 两个方向均有已实现源码，不只是计划

- **Anchor/编辑器作为 ACP client → Goose agent/server**：stdio 入口 `goose acp [--with-builtin NAME,...]`，或独立二进制 `goose-acp [--with-builtin NAME,...]`。路径：`crates/goose-cli/src/cli.rs` 的 `Command::Acp` → `goose::acp::server::run`；独立入口 `crates/goose/src/bin/goose-acp.rs`；处理器位于 `crates/goose/src/acp/server.rs`、`server/dispatch.rs`、`server/new_session.rs`、`server/load_session.rs`。[S2][S3]
- **Goose作为 ACP client → 外部 ACP agent/server**：已有 `AcpProvider`，不是让协议代替普通模型 HTTP API。示例选择方式 `GOOSE_PROVIDER=claude-acp GOOSE_MODEL=default goose session`，外部进程命令为 `claude-agent-acp`；`codex-acp` 等也有 provider 模块。路径：`crates/goose/src/acp/provider.rs`、`crates/goose/src/providers/{claude_acp,codex_acp}.rs`。Goose 启动子进程、initialize/new/prompt、接收工具事件和权限请求；它的 `Provider::resume` 调用的是 **`session/load`，不是 `session/resume`**。[S4]

“源码已实现”不等于本机二进制已验证、Anchor 已接入或真实外部 agent/provider 已跑通。

### 2. 稳定协议、可选能力和 Goose 实现必须分别判断

| 接口/能力 | ACP v1 状态 | v1.53.0 Goose server/client 的实际边界 |
| --- | --- | --- |
| `initialize` | 稳定；先协商版本和能力，未声明能力不能推定支持 | server 保存 client 的 fs/terminal 等能力，声明 `loadSession=true`、HTTP MCP、image/embeddedContext；audio=false。client 发 `ProtocolVersion::V1`。server 返回请求中的版本值，未据此证明其他协议版本兼容。 |
| `session/new`、`session/prompt`、`session/update` | 稳定基线 | 两方向都有处理；new 创建 Goose 持久 session，prompt 执行 Goose Agent 并投影 update。不是 Graph/Run/节点结果协议。 |
| `session/load` | **稳定但可选**，需 `loadSession=true` | server 从会话存储加载并重放历史；client 仅在外部 agent 声明 load 时调用。不保证所有外部 agent 能恢复。 |
| `session/resume` | **已于 2026-04-22 稳定，但可选**，需 `sessionCapabilities.resume`；与 load 不同，不重放历史 | 发布版 server 未声明 resume，dispatcher 未接入它；client 也没有调用它。不能因协议存在此方法就写“Goose 支持”。 |
| `session/cancel` | 稳定基线通知；另有可选通用 `$/cancel_request` | server 取消 active run token；不会回滚已发生的副作用。外部-provider client 本文件未见明确发送 `session/cancel` 的分支，不承诺外部 agent 停止行为。 |
| cwd、MCP、文件/terminal | cwd 是上下文；stdio MCP 属基线，HTTP/SSE 分别可选；client fs/terminal 分别协商 | server 要求 cwd 是绝对且存在的目录；load 可改变持久 cwd。接收 stdio 的 command/args/env 和 HTTP URL/headers，明确拒绝 SSE。developer 可桥接 client fs/terminal；未声明这些能力**不等于禁用 Goose 的本机工具**。 |

来源：[S3][S4][S5][S6]。cwd 的有效根目录有协议层 SHOULD 边界要求，但不是 OS 沙箱；原生 `developer/edit.rs::resolve_path` 接受绝对路径，或直接将相对路径拼到工作目录，没有由该函数强制限制为 cwd 内。

ACP **v2 在 2026-07-20 公告中以 Draft 发布**；本轮不据此断言后来是否稳定，也不能借 v2 的新语义解释该 Goose 发布版。Goose Cargo 还显式启用 `unstable_end_turn_token_usage`、`unstable_session_fork`。稳定可选能力、unstable feature 和 Goose 的 `_meta`/自定义方法是三类不同契约。[S1][S5]

### 3. v1.53.0 的 client extension 选择不是精确工具白名单

- 发布版 `server.rs::initial_session_extensions` **先装 builtin，再叠加 client 选择**。无显式 builtin 参数时默认选择 `developer`（配置明确禁用它则跳过）；显式 builtin 参数仍会加入对应工具。client 选择实际字段为 `session/new._meta.enabledExtensions`，不是标准 ACP 能力。[S3]
- 因此发布版 `_meta.enabledExtensions: []` 可能仍保留 developer，`[memory]` 也可能同时保留 developer。不传该字段时，还合并配置启用的 extensions、项目 plugin MCP 和请求的 `mcpServers`；**`mcpServers: []` 不代表清空工具**。传 client 选择或 recipe 时，请求 MCP 不走这个默认合并分支。[S3]
- **main 的 #12548**：无 recipe 且提供 `enabledExtensions` 时，直接返回 client 的完整选择；空列表不装 extension，memory-only 不附带 builtin/配置/request MCP。recipe 路径保留 builtin 叠加规则。不要将这个修复倒推为 v1.53.0 的安全边界。[S7]
- **load 不是权限撤销**：发布版以保存的 extensions 为基础，加非空请求 MCP（同名替换）；空 MCP 列表不会清除原工具。main 这条“初始选择”修复本身不能证明已有 session 被撤权。[S3][S7]
- `session/request_permission` 让 client 在 server 提供的选项中选 allow/reject once/always；Goose server 映射为工具确认，失败映射为 Cancel。它不是“每个工具必经 Anchor”的保证。Goose external-provider client 的 Auto 自动 AllowOnce、Chat 自动 RejectOnce，Approve/SmartApprove 转为确认；Claude 的 Auto 同时映射为 `bypassPermissions`。默认模式和绕过模式不能被误认为 Anchor 的宿主授权。[S3][S4]

另一个兼容限制：Goose client 的 `extension_configs_to_mcp_servers` 只转 stdio/普通 HTTP，跳过 socket-backed HTTP、builtin 等其他配置；原有 Plugin/工具授权不会仅靠配置转换完整迁移。[S4]

### 4. 恢复存在，但“未决副作用不重复”和“事件等于工作日志”均未获证明

- 发布版 load 会重放会话、恢复保存的 provider session、重发未决权限请求；state-machine 路径启用时，未决确认或已保存但未应用的确认回复还会触发 `start_resumed_state_machine_turn`。**不能把 load 当作无副作用的纯历史读取**。[S6]
- 工具执行代码先 dispatch，再构造工具结果；pending 判定以尚无工具回复的 request 为候选。没有从 ACP 或这些源码证明外部副作用与结果持久化原子提交、跨重启幂等去重或 exactly-once。**风险推断**：工具已改变外部状态而回复未保存的崩溃窗口，仍可能形成“已执行但看起来未完成”；不能承诺恢复绝不重复。也不能声称“每次 load 都重放所有工具”。[S8]
- `session/update` 是客户端展示流，工具 status/rawInput/rawOutput/content 等不是完整事务日志。Goose load 明确只重放 `is_user_visible` 的 `user_visible_content`，并支持私有 `_meta.replayTail` 截尾；因此连“完整原始会话存储”都不能由收到的 update 推定。它不能替代运行配置、内部状态、沙箱事实、产物/commit、授权和未知工具结果的持久工作记录。[S6]

### 5. ACP 可以是适配器，单独接入不足以保留 Anchor 契约

Anchor 当前/目标文档把 Graph/Run、节点工作区、上游只读快照、产物与 commit、网络/文件/工具授权、取消与未知副作用恢复事实交给 Host/Runtime。ACP 的 sessionId/cwd/工具展示事件不提供这些所有权或执行不变量。

可逆方向：把 Goose 限定为一个受控 AgentNode 的外部执行适配器，GraphRunner/Run/Artifact/Sandbox 和结构化完成结果仍归 Anchor；MCP 只暴露 Anchor 授权工具，fs/terminal 回调由 Anchor 校验且在沙箱内执行，Goose 进程与其他工具也必须受 OS 边界约束，不能只依赖 cwd 或权限弹窗。恢复继续保存事实并由 Agent 核查现场，不引入“未知结果强制审批”。**这只是 spike 方案，不是已证明能替换 io-harness + Rig。**

## 最小可逆 fixture spike（建议，未执行）

复用 Goose `crates/goose/tests/acp_fixtures/` 和 `acp_server_test.rs` 的确定性 provider/本地传输模式；仅小型单节点 Graph + 假 MCP，不运行业务大图或真实模型。

1. **接口基线**：固定 v1.53.0，在独立临时配置/数据目录验证 initialize → new → prompt → cancel → 重启 → load；断言 resume 未声明，结构化 summary/route 由 Anchor 显式验证，Run/产物标识不被 sessionId 替代。
2. **授权与版本差异**：覆盖默认配置、`enabledExtensions=[]/[memory]`、`mcpServers=[]`、recipe、已有 session load；分别对发布版和固定 main 做工具列表断言。尝试越界绝对路径、`../`、符号链接、native shell/其他 extension、未授权 MCP，要求沙箱/宿主拒绝，不能以“没有权限请求”判安全。
3. **未知副作用**：假工具在持久外部计数器追加 invocation 标识，分别在执行前、执行后回复保存前、回复保存后切断连接/杀进程；重启加载并核查计数及调用事实。恢复路径不得未经现场核查自动重复未知操作；“没有重复”只覆盖已注入窗口，不宣称通用 exactly-once。另验事件丢失/截尾不影响完整工作记录和产物恢复。

未解决项：真实发布二进制能力及远端 agent 的取消语义；所有故障窗口；完整权限映射/撤权；其他 extensions 和 MCP 子进程的绕过面；结构化完成、Graph 路由与产物契约的实际接线。main 对照聚焦 #12548，不代表已审计 release 到 main 的所有权限变化。均未做运行验收。

## 可核查来源

- **S1 版本/依赖**：<https://github.com/aaif-goose/goose/releases/tag/v1.53.0>；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/Cargo.toml>；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/Cargo.toml>。
- **S2 CLI**：<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose-cli/src/cli.rs>（`Command::Acp`）；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/bin/goose-acp.rs>。
- **S3 server 边界**：<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/acp/server.rs>（`on_initialize`、`initial_session_extensions`、`prepare_session_for_activation`、`apply_acp_extension_overrides`、`on_cancel`）；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/acp/server/new_session.rs>（`meta_goose_extensions`）；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/acp/fs.rs>；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/agents/platform_extensions/developer/edit.rs>（`resolve_path`）。
- **S4 client**：<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/acp/provider.rs>（`handle_requests`、`Provider::resume`、`permission_decision_from_mode`、`extension_configs_to_mcp_servers`）；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/providers/claude_acp.rs>；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/documentation/docs/guides/acp-providers.md>。
- **S5 官方协议**：<https://github.com/agentclientprotocol/agent-client-protocol/tree/06128106cf54b806a491a0fcf2b6bc7022ef57ca/docs/protocol/v1>（`initialization.mdx`、`session-setup.mdx`、`tool-calls.mdx`、`cancellation.mdx`）；<https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/06128106cf54b806a491a0fcf2b6bc7022ef57ca/docs/announcements/session-resume-stabilized.mdx>；<https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/06128106cf54b806a491a0fcf2b6bc7022ef57ca/docs/announcements/acp-v2-draft.mdx>。
- **S6 load/replay**：<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/acp/server/load_session.rs>（`messages_for_acp_replay`、`handle_load_session`、`start_resumed_state_machine_turn`）。
- **S7 main 修复**：<https://github.com/aaif-goose/goose/commit/540df77c30e3c1f3b51915cb18ae081931c96851>；<https://raw.githubusercontent.com/aaif-goose/goose/540df77c30e3c1f3b51915cb18ae081931c96851/crates/goose/src/acp/server.rs>（`initial_session_extensions` 与新增选择断言）。
- **S8 副作用窗口**：<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/agents/state_machine/ops_toolcalling.rs>（`pending_tool_requests`、`ToolExecutionOperation`）；<https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/agents/agent.rs>（`resume_state_machine_turn_inner`）。
- **Anchor 本地事实/目标**：`docs/pilot-development-plan.md`（恢复边界）、`docs/architecture.md`（工作与记录、Rust-native 当前状态）、`docs/product-architecture.md`（能力归属、信任边界）。
