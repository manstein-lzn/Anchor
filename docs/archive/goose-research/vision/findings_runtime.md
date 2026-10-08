# Goose + ACP 与 Anchor 愿景：Runtime 边界

> 历史调研：结论限于当时检查的版本和边界，当前实现与验收见开发台账。

调研日期：2026-10-06。范围仅含会话/续聊、未知副作用恢复、取消、上下文压缩、人工提问/续答、usage/预算与流式观察；不覆盖 Plugin、权限或部署。只读调研，本文是唯一写入；未运行模型、构建、测试或修改业务代码、架构及进度台账。

## 结论

**就 Runtime 而言，Goose + ACP 是有条件可行的统一候选，但当前不能宣布已满足 Anchor 完整愿景或可直接生产替换。** Goose 有原生持久会话、上下文压缩及 Agent loop，ACP 暴露加载会话、追加 prompt、取消、流式消息/工具观察，并有结构化提问接线。尚缺的关键证据是：中断后能否保留未知操作线索，让 Agent **先查询现场，再继续任务**；此外还有等待用户跨重启、严格预算和断线观察的产品验收。[S1–S8]

**不以 exactly-once、回滚或恢复原工具栈帧作为门槛。** Anchor 已明确要求保存工作记录、重开会话、核查现场后继续，而非把所有副作用纳入事务。不能把 `loadSession=true` 写成“Goose 已自动安全恢复”，也不能因为它不保证 exactly-once 就断言愿景不可实现。接口存在、源码行为、Anchor 接线和端到端验收是四种状态。

## 版本与证据口径

- 固定实际 spike 基线 **Goose v1.53.0**，不研究或混入 main 新能力。现有 release/tag 证据指向 commit `76da81cb964b21cd096db739302329b40c2998b8`；实际二进制 SHA256 为 `bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340`。这些身份来自 `docs/goose-acp-spike.md` 和已有 `/tmp/anchor-goose-acp-spike/evidence/`，本次没有重新执行二进制。
- 已先读 `research_plan.md`、`docs/goose-acp-spike.md`、`research_goose_acp/findings_runtime.md`，并核对产品不变量、Session/工作记录、当前 Runtime 和 Pilot 取消/恢复契约。A112 的“外部效果后杀 Host”发生在 **ToolResponse 已保存、下一模型请求阻塞**之后，不是“效果发生但响应尚未提交”的窗口；原 io-harness 测试不能替 Goose 计账。
- 外部事实仅使用官方 Goose 固定 tag 源码与 ACP 官方协议，实际通过 web 打开核验；同时复用本地源码缓存。对官方 tag 的 `acp/server.rs` 做只读网络读取，其 SHA256 与缓存一致：`56cb79303f6e47c69f4b107df08ff96fc402b854d33b000351ca878cd4afb673`。另一次 commit URL 的 shell 请求超时，不计作核验成功。
- ACP 网页为 2026-10-06 读取的 **v1 在线协议**，不是 v1.53.0 随包协议快照；Goose 是否实现一项能力以固定源码/实际初始化证据为准。

## 能力矩阵：核心实现不等于 ACP 恢复承诺

| 产品要求 | Goose 核心事实 | v1.53.0 经 ACP 可用的表面 | 判断/缺口 |
| --- | --- | --- | --- |
| 保存历史、重开续聊 | `SessionManager` 在 `sessions/sessions.db` 保存会话、消息、provider/model 与 usage；有 `get_session`、`add_message`、`replace_conversation` | 初始化声明 `loadSession=true`；`session/load` 加载同 ID，`session/prompt` 开始新 turn | 可复用；Anchor Session/Turn/Run 关联仍不是 Goose session 的自动映射。[S1–S3] |
| 未知副作用后核查继续 | 保存工具请求/响应；执行外部工具与提交其结果不是同一个原子事实 | load 重放已保存的可见历史；没有通用 pending-effect 查询/核验契约 | 愿景关键未验收，不要求 exactly-once；要验证新 prompt 先查现场、不盲目重做。[S3、S4] |
| 取消 | active run 的 cooperative `CancellationToken` | `session/cancel` 通知；原 prompt 以 `cancelled` 收束 | 不是持久 pause，也不是回滚证明；需保存 Anchor 的停止/中断事实。[S1、S8] |
| 长会话压缩 | 自动阈值压缩、手动 `/compact`；原消息保留但对 Agent 隐藏，摘要供模型续聊 | `session/prompt` 进入同一核心；可提交 `/compact`，无需宿主另写压缩器 | 可复用，不是 ACP 独立的通用 compaction RPC；长会话与恢复联合验收未完成。[S5、S6] |
| 提问并接收回答 | ActionRequired/MCP elicitation 与回答记录 | 协商 `clientCapabilities.elicitation.form` 后发送 form elicitation，处理 accept/decline/cancel | 活进程接线存在；跨重启等待/续答未证明，不可把普通聊天文字当持久 waiting_user。[S7] |
| usage/预算 | 当前上下文 usage 与累计 usage/cost 分开保存 | 标准 context `used/size` 和可选 cost；Goose custom 累计 input/output/cost，另有 message usage | 可观察/计账，不自动强约束累计请求、token 或金额预算。[S1、S2、S8] |
| 流式观察、重连 | AgentEvent 与持久 message 是不同层 | `session/update` 的文字、thought、tool call/update、usage；load 另做 history replay | 展示流不是恢复日志，也不是 Anchor 游标式可靠重放；完整实时/重连尚待接线验收。[S1、S3、S8] |

## 架构判断：公开 ACP、扩展与 fork

以下只依据已核验来源，不再新增搜索；“未发现公开能力”不等于证明所有可能接线均不可行。

| 关键问题 | 已核验的接入选择 | 是否需要私有扩展或 fork |
| --- | --- | --- |
| 未知副作用后安全继续 | 公开 `session/load` + 新 `session/prompt` 能加载历史并启动新轮；**没有已核验的通用 pending-effect inspect/resolve 或自动现场核验 ACP 能力**。特定 tool-confirmation 续跑不等于未知结果恢复 | **不能认定仅靠公开 ACP 已满足产品要求，也不能认定必须 fork。** 先验原生历史 + 既有现场查询工具 + 新 prompt；未知调用线索是否足够、是否避免盲目重放仍未知。若这条路不足，才评估最小 Goose 专有扩展或上游改动，不预先建设恢复引擎。[S1、S3、S4] |
| 自动/手动 compaction | Goose 核心已有自动压缩；手动 `/compact` 可作为普通 prompt 进入 Goose 命令处理 | **复用已有行为不要求 fork，也不要求新增私有 RPC。** `/compact` 是 Goose 专有命令语义，不是跨实现标准 compaction API；宿主精确指定摘要策略、强制触发或读取完整压缩状态是否有合适公开 ACP 控制，未核验。[S5、S6] |
| 人工提问与当前进程内续答 | Goose ACP 已实装 form elicitation 协商及请求/回答；普通文字提问也可用下一条 prompt 回答 | **匹配该接口的客户端不要求 fork Goose。** 不能保证任意 ACP 客户端支持；本次未进一步核实该 elicitation 表面在协议中的稳定性级别。跨重启保留 waiting_user 并回答同一问题，未核验，不能宣布无需扩展即可满足。[S3、S7] |
| usage 展示与累计计账 | 标准 ACP 可见 context 占用和可选 cost；累计 input/output 与逐消息细项可用 Goose custom notifications | **标准 context/cost 展示不要求 fork；读取已实现的 Goose custom 字段不要求 fork，但会依赖 Goose 专有扩展。** 它们不是所有 ACP 后端都会有的可移植契约。[S1、S2、S8] |
| 严格累计预算 | 计量通知不是模型请求前的强制 admission；未核验公开 ACP 可设置 Anchor 的精确累计请求/token/金额限额 | **不能靠标准或 custom usage 保证硬预算。** 受控模型入口可作为“不 fork”的候选强制点，但是否覆盖摘要、重试、续聊以及 token/金额上界尚未验收；若要求 Goose 内部统一强制预算，是否需专有扩展/上游修改仍未知。[S1、S4] |

**给主轨的收敛判断：** 不应现在以“必须 fork”否决 Goose，也不应以“ACP 隔离后全部原生可用”批准完整迁移。compaction 和活进程提问已有可复用入口；细粒度 usage 会引入 Goose 专有协议依赖；未知副作用后的核查继续、跨重启提问和严格预算是产品门槛。先做未知结果的最小续聊验收，再判断缺口是否需要扩展或上游修改。这个判断不要求全面源码审计，也不要求 exactly-once。

## 核心门槛：loadSession 到底恢复什么

1. **确定：加载的是原生会话及已保存工作记录。** `handle_load_session` 调用 `get_session(id, true)`，重新准备 Agent、绑定会话、加载保存的 provider 会话标识。客户端 replay 筛选 `user_visible` 消息，发送文字、图像、工具请求/响应等；这不是把 ACP 显示碎片反灌模型。隐藏的模型上下文仍由 Goose 原生会话管理。[S2、S3]
2. **确定：有一种窄范围的待确认工具续跑。** load 检查 `pending_tool_confirmations` / `has_unapplied_tool_confirmation_response`；仅在全局 state machine 开启并命中该条件时调用 `resume_state_machine_turn`，否则重发待确认请求。它检查的是确认记录，不是“外部效果发生但结果未知”。因此 load 在该模式下也不能被假定为永远只读、绝不推进执行。[S3]
3. **确定：普通新 prompt 与恢复旧 turn 不是同一入口。** 初始化声明 load，但未声明 session resume capability；不能把协议其他实现的 `session/resume` 算作 Goose 已公开支持。ACP `on_prompt` 生成新 run ID、追加用户消息并进入 `Agent::reply`；它不暴露一个通用的 pending-effect inspect/resolve API。实验 state-machine 路径由 `_meta.goose.unrolledAgentLoop` 或全局开关选择；不能把该路径的能力无条件算作实际 spike 默认行为。[S1、S3、S4]
4. **不能推断：请求有记录就代表工具未执行，或缺失响应就能安全重试。** `ToolExecutionOperation` 从当前 kickoff 后的 history 找 pending request，实际执行后才形成结果 effect；提交动作与外部系统的效果之间存在故障窗口。它不提供“先询问外部系统再决定”的产品保证；这里也不据此断言所有模式必然重放。[S4]
5. **对愿景的推断：可以尝试用原生 load + 新 prompt 实现核查后继续。** 让模型带旧任务线索和用户补充查询已有 Run、文件/外部状态，再选择下一步，符合现有产品需求。真正要验的是未回答工具对在所选 loop/provider 中如何呈现、是否阻断新输入/被过滤，以及查询能否先于再次写入。若未知线索只剩模型不可见的 history，不能把 UI 能看到旧调用当作模型能核查；需验证最小事实传递，不预先建设新恢复引擎。

**是否覆盖 pending tool effect：否，现有证据只覆盖已保存历史和特定待确认续跑，不覆盖通用未知结果核验。是否因此阻塞整个技术方向：未定；阻塞的是“完整产品已可替代”的结论，最小恢复切片是下一项必验。**

## 其余边界

### 取消不是持久暂停

`on_cancel` 查 active-run token 并取消；没有在这个方法中写持久 paused checkpoint。`forward_agent_stream` 察觉取消停止转发/退出，返回 cancelled 语义。ACP v1 要求尽快终止模型和工具、允许取消后终态前发送更新；协议要求不等于工具或外部系统已回滚。Anchor 仍须区分 stopped、interrupted、waiting_user，确认工具结果收束，不在重新启动时自动补跑未知写操作。[S1、S8]

### 可以复用 compaction，但不要自己恢复隐藏历史

`compact_messages` 使用现有 provider 做摘要，保留原消息并设为 Agent 不可见，摘要设为 Agent 可见/用户不可见；自动压缩读取 `GOOSE_AUTO_COMPACT_THRESHOLD`。普通 `Agent::reply` 接了自动压缩及命令，state-machine 也有 `CompactionOperation`；后者在等待工具响应时避免自动压缩。手动 `/compact` 由同一会话 prompt 进入已有命令实现，受 slash-command 配置和 provider 是否自行管理 context 影响。[S5、S6]

所以 **通过 ACP 仍可让 Goose 自己承担压缩**，不需要抽出 GDK 再组装一套上下文系统。但摘要请求也消耗模型 usage；压缩后标准 used 降低不表示预算恢复。未知调用、等待回答、压缩失败/取消和重启组合未验收，尤其不能把 state-machine 的保护逻辑当默认 legacy 路径已通过证据。

### 提问支持分两层

- 同一会话“文字问问题→下一条 prompt 回答”已有续聊通路；它不自动提供 Anchor 的 typed `waiting_user` 生命周期。
- 结构化路径：固定 ACP adapter 检查客户端 form capability，发 `CreateElicitationRequest`，将 accept/decline/cancel 写入原生 elicitation 回答流程；不支持 form 的客户端会导致取消该提问，URL elicitation 在此实现中不支持。[S7]
- `load_session` 的 replay 没有通用 elicitation 分支，续跑条件针对 tool confirmation。源码存在回答落盘不等于等待中的请求/回调能跨进程恢复；对“等待时关服务，重开后仍能答同一问题”的证据不足。不能用权限确认续跑替代人工问题续答验收。[S3、S7]

### Usage 可观察，强预算不能靠事后取消保证

标准 `usage_update.used/size` 表示 **当前上下文占用/容量**，可选 `cost` 为累计金额；Goose custom `SessionUsageUpdate` 提供 `accumulated_input_tokens`、`accumulated_output_tokens`、`accumulated_cost`，`MessageUsage` 另可标识压缩 usage。`PromptResponse.usage` 构自 session 当前 usage，不能直接当整个 turn 或节点的精确累计账单。[S1、S2、S8]

`on_prompt` 的 SessionConfig 使用 `max_turns: None`，并未从标准 prompt 接一个 Anchor 精确预算。核心有 max-turns 限制，但轮次上限不是实际 transport 请求、重试、摘要请求或金额的统一硬上限。usage 事件在核心保存计量之后发出；客户端看见超额再 cancel，已经发出的请求不会变成零消费。[S1、S4]

**结论：只凭 ACP usage 不能承诺严格预算。** 如果愿景保留严格累计请求额度，最小验收应在真实模型传输入口检查“超额的请求是否根本没转发”，覆盖摘要/重试/续聊；精确 token/金额强约束还需核对所用 provider 的预先限额、保守预留与缺失 usage 策略。本次没有验收这些边界，也不把 `max_tokens` stop reason 当硬预算已实现。

### 流式观察不是持久 SSE 游标

固定 server 将 AgentEvent 的消息、思考、工具变化和 usage 转成 ACP 通知；某些精细计量依赖 Goose custom capability。`session/load` 回放可见原生历史，不回放所有实时通知、隐藏摘要或未提交 chunk。Anchor 的 Session/Turn 状态与浏览器游标续传仍需原有宿主能力接线，不能把 ACP notification 当事实数据库。[S1、S3、S8]

A112 的模型 proxy 使用有界整响应缓冲，文件也明确实时摘要/SSE 未验收：**官方有流式表面 ≠ 当前 Anchor spike 已做到实时逐 token UI**。关闭浏览器不取消执行、重连不触发新模型请求、重复 history 与 live update 的去重，都须实际验证。

## 最小验收建议（建议，不是本次执行结果）

复用小型 fixture Graph + 固定 Goose binary + 本地确定性 provider，仅替换模型传输；继续经过真实 Host/GraphRunner/NodePort/Goose/工具路径，不跑大型业务 Graph。

1. **最高优先级：未知结果后核查继续。** 一个写入工具落外部可查询标记后、返回结果前阻塞；杀进程并重开同 Goose session。显式新 prompt 要求核查，确定性模型先读标记，再继续；断言写调用不盲目重复、未知线索进入模型输入、不会把未知操作写成成功。另覆盖请求已保存但工具未开始、结果已提交、批内部分完成三个窗口。不要求 exactly-once；若 load 自行推进确认续跑，必须把它纳入观察和断言。
2. **普通续聊/提问跨重启。** 原会话新增输入与必要的空 prompt 路径分别验；form 提问 accept/decline/cancel 及等待时进程重启。检查回答关联同一问题、旧回调/重复回答不产生第二次业务操作，waiting_user 不被误写成 interrupted 或 completed。
3. **取消及后续继续。** 分别在模型流、工具执行、等待回答时 cancel；保留实际工具结果或 unknown，随后重开并查询现场。不把 cancelled 当副作用未发生，也不把浏览器断开当 cancel。
4. **压缩联合回归。** 触发自动及 `/compact`，验证原生 history 与可见 replay 保留事实、摘要供下一轮使用；重启后仍能查询任务状态；覆盖摘要失败、pending 工具/提问、usage 在压缩后不被当成归零预算。
5. **预算和观察。** 在模型入口计数验证第 N+1 个请求没有转发，包含摘要/重试；验证累计 usage 缺失的处理。再验逐 chunk 更新、取消后终态前更新、重连无新执行、history/live 去重。只有这些通过后才讨论真实 provider 的低频端到端验收。

## 官方来源与可定位入口

以下 Goose URL 均固定 `v1.53.0`；本文按源码行为判断，不引用第三方评论。URL 已通过实际 web 打开核验；S1 另与现有本地缓存核对。

- **S1 ACP server**：`on_initialize`、`use_state_machine_from_meta`、`on_prompt`、`on_cancel`、`build_prompt_usage`、`build_usage_updates`、`forward_agent_stream`。
  https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/acp/server.rs
- **S2 原生会话**：`Session`、`SessionUsageTotals`、`SessionManager::{get_session,add_message,replace_conversation,record_usage_metrics}`。
  https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/session/session_manager.rs
- **S3 加载与有限续跑**：`messages_for_acp_replay`、`handle_load_session`、`start_resumed_state_machine_turn`。
  https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/acp/server/load_session.rs
- **S4 Agent/工具执行**：`Agent::reply`、`reply_with_state_machine_inner`、`resume_state_machine_turn_inner`；`ToolExecutionOperation::run` 及 `messages_since_kickoff` 后的 pending 选择。
  https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/agent.rs
  https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/agents/state_machine/ops_toolcalling.rs
- **S5 压缩算法/上下文**：`compact_messages`、`check_if_compaction_needed`、`CompactionResult`。
  https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/context_mgmt/mod.rs
- **S6 压缩接线**：`COMPACT_TRIGGERS`、`handle_compact_command`、`CompactionOperation`。
  https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/agents/execute_commands.rs
  https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/agents/state_machine/ops_compaction.rs
- **S7 ACP form elicitation**：`client_supports_form_elicitation`、`send_form_elicitation`、`record_acp_elicitation_response`。
  https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/crates/goose/src/acp/server/elicitation.rs
- **S8 ACP v1 在线协议**：Session Updates、Session Usage Updates、Cancellation、Continue Conversation。2026-10-06 读取，未将协议中的可选能力一概归于固定 Goose。
  https://agentclientprotocol.com/protocol/v1/prompt-turn

**未验证范围**：未知结果精确故障窗口的可继续路径、跨重启提问、长期压缩联合恢复、严格预算、完整实时/重连 UI、真实 provider。没有把源码测试的存在、本次静态核查或其他后端回归写成这些边界已通过。
