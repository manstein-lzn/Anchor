# Anchor Pilot 开发计划与验收台账

2026-09-26。基线：[体验核查](pilot-experience-audit.md)。产品目标以 [产品与系统架构](product-architecture.md) 为准，当前实现以 [当前架构](architecture.md) 为准；本文是开发顺序、阶段契约和验收状态的唯一台账。

## 交付目标与范围

用户可以在一个持久会话中提出问题、查看真实执行过程、补充信息、管理研究工作流和检查有来源的产物。关闭页面或停止服务后，隔几天重新打开同一会话，Agent 能读取工作记录，查询实际状态、检查文件或运行测试，然后继续任务。

2026-09-26 用户决定：恢复以持续保存的工作记录（JSONL）为依据，由 Agent 核查现场后续做。撤销复杂审批扩建、跨存储事务和「未知结果人工处置平台」目标，不把它们作为后续功能的前置条件。缺少工具结果可以如实记录为中断，不能伪造成功，也不必因此锁死整段对话。此决定调整的是目标；现有审批、操作账本和恢复门禁仍在代码中，待 P2 收敛。

当前核心目标：找到并接通 PydanticAI / Harness 已有的持久记录和续聊接口，让用户重新打开原会话后继续工作。保持真实流式展示和必要提问，按需接框架压缩，补上对象直接跳转，并完成真实路径验收。Anchor 负责把这些能力接入现有 Session、Graph、Run、Plugin、文件和沙箱边界。

框架优先：先核对本地固定版本的公开 API、仓库已有调用和最小运行结果，再做必要接线。不能把框架已有的消息存储、恢复、压缩、计划能力重新列成 Anchor 自研系统。JSONL 诉求通过框架原生文件存储承接，不预先设计自有日志协议；需要保留框架同时生成的消息快照和媒体文件，不能误称单个事件 JSONL 就是全部历史。

若确实需要自行实现框架能力，先说明缺口、可复用的替代方案和最小自研范围，得到用户明确同意后才能开始。常规 Anchor 接线不等于自研框架；未确定的产品需求必须等当前目标完成后再讨论决定。

不做：通用审批平台、外部操作 exactly-once 保证、跨存储原子事务、未知结果人工处置平台、自造模型循环、第二个 Graph 调度器、宿主 Shell/FileSystem 绕过沙箱、多租户平台、知识库自动编译、自动环境安装、云端持久工作流集群。默认部署仍是本机单个服务进程。

## 当前目标开发（本轮唯一范围）

本轮只交付以下内容：

1. 用 Harness 原生 `StepPersistence` 和文件存储保存 Agent 的逐步工作记录；保留框架生成的 JSONL、消息快照和媒体文件。
2. 重新打开同一 Session 时，用 Harness 的公开接口找到记录并通过 `continue_run` 续聊；新输入可以进入同一对话。
3. 在确有上下文超窗证据时，接入 Harness 已有的 compaction；不建设 Anchor 自己的记忆系统。
4. 保持必要提问，让用户回答后回到原会话。框架已有 `AskUser`，当前 `session_ask` 已使用 Pydantic deferred tools；接入时复用已有能力，不为更换接口单独重写问答系统。
5. 让聊天中的 Graph、Run、Artifact 引用可以直接进入已有对象页面并返回会话。
6. 用真实 provider、进程中断、服务重启和浏览器路径验收上述行为。未通过真实路径前不写成已完成。

本轮只核对续聊所需的框架压缩配置，不预设摘要格式或记忆机制。计划呈现、系统 Pilot 的实现形态、研究合同、附件、编辑分支和导出方式待后续讨论。

## 后续产品需求记录（暂不决定实现）

以下内容保留为产品需求，当前只记录用户可能需要的能力，不形成开发顺序、技术方案、接口契约或验收前置条件：

- 系统级 Pilot 与 Graph/Run/Artifact 的更深联动；
- 系统级 Pilot 与 Graph/Run/Artifact 的更深联动；
- 长期 Copilot：用户可直接向 Anchor Copilot 提出任务，由 Copilot 选择或临时构建 Graph 执行，并把结果返回给原始用户/来源。临时 Graph 的持久化、可见性和生命周期，以及异步来源的回传方式尚未决定；
- 研究目标、研究合同、证据验收和研究应用体验；
- 长任务的上下文使用展示、可见计划和进度表达（基础压缩按当前目标复用框架）；
- 附件、图片、资源引用、编辑后分支和导出。

当前目标完成后，再逐项讨论这些需求是否需要、边界是什么，以及是否已有 PydanticAI / Harness 接口可以直接复用。

## 分阶段工作包

本表只列已完成或当前推进的工作，保留原编号便于追溯历史记录。旧 P3–P6 已移入上面的产品需求记录，不再作为开发阶段。保护工作区已有改动，不 reset、不清理、不把无关文件纳入提交。

| 阶段 | 交付与主要路径 | 依赖 | 可独立验证与停止条件 | 状态 |
| --- | --- | --- | --- | --- |
| P0 计划与契约 | 本文、需求矩阵与事实来源 | 现状核查 | 目标与现状分开，只有一份开发顺序 | 已按用户决定更新 |
| P1 执行身份与事件流 | 提交去重、后台执行、SSE 重连、真实文本与工具卡片 | P0 | 刷新接回原执行；重复提交不产生第二次执行；停止保留输出 | 已完成（后端 + 浏览器证据） |
| P2 框架记录与续聊接入 | 原生持久记录与续聊、必要提问、按需压缩、对象直接跳转；收敛现有审批门禁 | P1 | 杀进程后重开同一会话可继续；结果缺失时 Agent 可查询核验；对象链接进入已有页面 | R1–R5 修复完成，待真实 provider 长会话复验 |
| P7 当前目标集成验收 | 真实 provider、桌面/移动浏览器、进程中断、相关回归与文档 | P2 | 实际走通保存、关闭、重开、续聊和对象跳转；未验收不报完成 | 持续进行 |
| E1 事件触发与运行看板 | Graph/Run 输入、Bearer 白名单、Webhook、Responses 子集、本机定时和时间线看板 | P2/P7 基础稳定 | 确认触发来源、忙碌/停机错过语义、Responses JSON/SSE、计划编辑删除和浏览器查看；未通过真实 provider 前不报端到端完成 | 功能接线与自动回归完成；真实 provider/停机验收待做 |
| E2 RSI Graph | 每周只读审查 Anchor Run、架构代码、Graph/Plugin 和公开生态，形成可追溯演进提案 | E1/普通 Graph 稳定 | provider-free 全图、公开 API 失败边界、评审反馈回路、计划注册和本地报告；真实 provider、长期递归效果和提案实施单独验收 | 五领域同 Run 并行版已部署并保留原计划；真实验收与当前状态见 A30，长期改进收益单独验收 |

当前顺序只有：核对框架接口 → 接通 Anchor 会话入口 → 真实中断续聊验收。P3–P6 不提供当前实现指导。

2026-09-26 独立验收：五处待修复问题及复现证据见 [P2 独立验收报告](pilot-p2-acceptance-review.md)。历史路径通过不代表全部入口和压缩组合已通过。

## 当前共享契约（2026-09-26 收敛）

### 企业微信助手 Graph 接入（2026-09-30 纠正并冻结）

用户确认助手必须由普通 Graph 执行。此前企业微信 → Pilot 的接线偏离要求，其测试与 provider 证据不能作为助手 Graph 交付证据。本次替换该入口：平台事件 → 绑定来源身份的 Session/Turn → 指定普通 Graph Run → 指定回复节点的结构化结果 → 平台回复，不调用 Pilot Agent 或控制工具。

- 服务端 `.env` 指定 Graph、回复节点与允许访问的企业微信 userid；平台请求不能覆盖 Graph、Plugin、路径或执行权限。首期接私聊文本、图片、文件和混合消息；附件由网关下载并在 Graph 中以只读 `/in/channel` 提供，不能把平台临时 URL 直接交给 Agent。
- 每条已接受消息对应唯一 Graph Run；同一 Session 串行，不同用户的同一 Graph 可以并发。2026-09-30 用户补充：新消息取消旧 Run，等待取消落盘后接上历史启动新 Run；连续补充不得丢失，旧 Run 的迟到回复不得投递。继续使用现有 TurnStore 幂等及 Graph runner，不另建调度器。
- 跨轮读取原生 FileStepStore / continue_run；每位用户、每个 Graph 节点的历史独立。上一轮该节点的提交或取消时保存的未完成文件以只读快照传入新工作区，未完成文件不能当成成功结果。提问作为本轮 Graph 的正常回复，下一条消息开启下一轮并加载历史，不伪装成业务节点中途暂停。
- 会话绑定保存 Graph、回复节点和可信来源；旧 Pilot 通道 Session 保留，使用新命名空间，避免混入旧管理工具历史。进程中断不自动重放通道业务副作用；新消息可以结合上轮工作记录继续。
- 复用现有 Graph/Plugin 沙箱与权限。允许名单中的成员使用运营者为该 Graph 授权的 Plugin 凭证；不宣称已提供任意企业业务系统的逐用户行级权限。首轮接入建议仅允许机器人所有者，业务 Plugin 的用户权限需要各自 API 支持。
- 验收必须覆盖真实普通 Graph、Plugin 工具、两用户并发、跨轮历史和产物、重复事件、拒绝非法来源、重建服务后的续聊，并提供实际 `.env` 配置与启动说明。真实企业微信需要用户机器人凭证后验收。

### 身份与事实来源

- Session 是长期对话；turn 是一次已接收的用户提交/恢复尝试；Graph Run 是独立业务运行，三者不复用 ID。
- 客户端每次提交生成 `request_id`，网络重试沿用。服务端在同一 Session 下幂等接收；同 ID 不同内容返回 409。同 Session 同时只有一个运行中的 turn。
- `conversation_id = Session.conversation_id`；每次模型执行以新 `turn_id` 传给 PydanticAI `run_id`，恢复尝试不复用原 framework run ID。
- 目标是完整可续接的框架工作记录。Harness 已有 FileStepStore（事件 JSONL + 消息快照 + 媒体）和 SqliteStepStore；Pilot 与普通 AgentNode 现在都用原生 FileStepStore（`state/pilot-steps/`）及 continue_run。文件记录直接接原生 FileStepStore，不自造序列化、日志解析或恢复协议；保留现有会话读取能力，不静默丢弃历史。
- P1 的 SQLite turn 表及事件表负责提交去重和传输游标。事件内容直接使用 PydanticAI `VercelAIEventStream(sdk_version=6)` 输出的 chunks。会话历史与续聊步骤均通过 Harness 公开接口读取，SSE 碎片不直接塞回模型历史；不为换存储形式先重写现有消息系统。
- Session 状态、SSE 记录、Run 文件各司其职，不要求与工作记录构成跨存储事务。界面状态不一致时可重读实际记录；不以完成事务改造作为续聊前提。

### API 与流式传输（P1）

```text
POST /sessions/<id>/turns
  {"request_id":"稳定提交 ID", "message":"用户输入"}
  或 {"request_id":"新的恢复尝试 ID", "resume":true}
  → 202 {"turn":{...}}；重试已有 request 返回同一 turn；冲突 409
GET  /sessions/<id>/turns                 → turn 列表（不含模型消息）
GET  /sessions/<id>/turns/<turn>/events   → text/event-stream
  Last-Event-ID 或 ?after=<序号>；只返回指定 Session 的指定 turn
POST /sessions/<id>/stop                 → 请求取消当前执行
```

SSE `message` 的 data 为标准 Vercel chunk，`id` 为已提交到 SQLite 的事件序号；另有 `turn` 命名事件报告 Anchor turn 终态。重连只补游标后的记录。订阅是只读；关闭页面不会启动新模型请求，也不会取消服务端任务。停止必须显式请求。

保留现有同步 `/messages` 与 `/resume` 供原调用方，交互 UI 使用后台 turn API；两条入口共享 Pilot 执行器。P2 续聊时统一加载工作历史，不引入第二套业务工具或调度器。

### 状态、取消、失败

- 当前 turn：`running → completed | failed | stopped | interrupted | waiting_approval | waiting_user`；等待用户回答与进程中断是不同情况。
- 服务启动发现上一进程遗留的 `running` turn，标为 `interrupted`，保留事件、输入意图和错误说明，禁止自动重放副作用。
- 浏览器不把断开连接当作执行失败；连接恢复重新读取状态/历史。提交响应丢失用同一 request ID 查询/重试，不生成第二次执行。
- 当前涉及副作用的失败 turn 会被恢复门禁拒绝；P2 的目标是加载中断前记录和用户的新输入，让 Agent 核查 Run、文件和测试结果后继续，不要求用户先处理操作账本。
- 最终模型消息保存成功后才能将 turn 标为 completed；两者之间崩溃时保守报告中断，不重新声称副作用从未发生。

### P2 的最小实施顺序

1. **核对接口**：已确认下表接口，并用 FunctionModel 验证「工具结果持久化 → 模拟后续模型失败 → 新建 FileStepStore 读取 → continue_run → 带新输入继续」。这仅验证框架接口，不代表 Anchor 接入或真实杀进程通过。
2. **接通入口**：复用 StepPersistence 的记录钩子、FileStepStore 文件后端和 continue_run 的历史加载，将结果传给 Agent 的 message_history，保持 conversation_id。现有 conversation store 的用户输入/完整历史继续通过公开 API 读取；核对实际 persistence run ID，不假定它等于 turn ID。
3. **允许续聊**：加载记录和新的用户输入，让框架处理消息格式，让 Agent 查询现场。使用 inspect_recovery 取得的事实作为线索，不扩建人工处置系统。收敛阻断这条路径的副作用门禁与逐工具审批，保留必要提问和既有权限边界。
4. **补齐当前体验**：保留框架提问能力；按长会话需要接框架压缩；聊天对象引用直接跳转已有 Graph/Run/Artifact 页面。
5. **实际验证**：用真实 provider 和浏览器走中断、重启、续聊，覆盖模型输出中途、工具结果已记录和结果缺失。若出现框架缺口，先记录并与用户讨论，不自行启动替代框架开发。

### 已核对的框架接口

依据本地 `pydantic-ai-slim==2.46.0` / `pydantic-ai-harness==0.36.0` 的源码与随包 README：

| 需求 | 已有公开接口 | Anchor 接入现状 |
| --- | --- | --- |
| 逐步保存工作 | `StepPersistence(store=...)`；`FileStepStore` / `SqliteStepStore` | Pilot 与 AgentNode 都接原生 `FileStepStore`（`state/pilot-steps/`），无需另写记录引擎 |
| 找到同一会话的记录 | `store.list_runs(conversation_id=...)` | 可获取实际 persistence run ID；配置 agent_name 时该 ID 不等于原 turn ID（= `5:pilot<turn_id>` 的 base64），Anchor 因此按 conversation_id 查找而不是拼 ID |
| 加载历史续聊 | `continue_run(..., include_interrupted=True)` → `Agent.run(new_prompt, message_history=..., conversation_id=...)` | Pilot 已接通：新一轮消息带 framework 记录一起发送 |
| 崩溃前的现场 | `StepPersistence(capture_frontier=True)` 的快照 | 工具执行中被 `kill -9` 时只有事件和 `tool_effects` 没有快照；打开 frontier 后新增的 `snapshots/*.json` 是唯一可读现场 |
| 新输入叠在未完成的调用上 | `ModelResponse.state = 'interrupted'` | 框架对「新 prompt + 未处理 tool call」直接报 `UserError`，拒绝静默重放；标成 interrupted 后由框架自己合成 `outcome='interrupted'` 的 tool-return |
| 查看未完成调用 | `step_persistence.recovery.inspect_recovery` | 返回快照和工具事实；给 Agent 核查，不自动决定是否重做 |
| 上下文超窗时压缩 | `SummarizingCompaction` / `SlidingWindowCompaction` | 节点已有使用；按需求配置，不单独建设记忆模块 |
| 必要提问 | `AskUser`；PydanticAI deferred tools | 当前 `session_ask` 已用 deferred tools；`AskUser` 自身不自动完成 Web 问答与进程重启接线 |
| 计划工具（可选） | `Planning` | 框架已提供；当前不接，也不另建 Anchor 计划系统 |

`continue_run` 默认只读完整快照；要看中断处需显式 include_interrupted。它只加载消息，不负责恢复 Graph 调度或判断外部操作是否成功。缺失工具结果的消息处理也有框架支持，但不同中断形态与新输入必须在接入时验证，不能把普通续聊小样例等同于所有故障通过。以上四行已由真实杀进程验收（见推进记录 2026-09-26 续聊实现条目）。

### 当前目标边界

- 正常工具操作遵循用户任务授权，不把每次调用都变成审批流程。缺少授权或涉及需要用户决定的破坏性操作时，通过对话确认；已有明确授权不反复询问。现有审批接口在代码收敛前仍按原始调用校验。
- 工作记录说明「此前尝试了什么、观察到了什么」；当前 Graph、Run、文件和测试结果说明实际现场。结果缺失时由 Agent 核查，不承诺任意外部操作只发生一次，也不自动重放历史命令。
- 工具事件不是 Graph completion；工作记录也不是研究结论或产品验收结论。
- Graph、Run、文件和沙箱继续使用现有 Anchor 边界；本轮不扩展这些边界。
- 未冻结的产品需求只进入后续讨论，不作为当前续聊的隐藏前置条件。

## 已确认的技术边界

1. 已有 PydanticAI / Harness 能力优先复用；不自造日志格式、恢复引擎、记忆系统、计划系统或模型循环。
2. P1 已使用的 SQLite turn 状态和 SSE 游标继续保留；本轮不引入新的存储或调度服务。
3. 固定依赖版本。新接口必须先对照本地 API 和最小运行结果，不能仅因文档存在就声明可用。

## 验收矩阵与证据台账

| 编号 | 用户/故障路径 | 预期 | 阶段 | 当前结果 |
| --- | --- | --- | --- | --- |
| A01 | 新建 → 你好 → 回复 → 刷新 | 消息持久、工具名 provider 合法 | 基线 | pass（此前真实 provider / browser） |
| A02 | Markdown/表格/代码、输入法、草稿、手机布局、发送即时显示 | 可读、可输入、无横向溢出；慢提交不吞消息 | 基线 | pass（`e2e/pilot.spec.ts`；2026-09-28 `e2e/pilot-lifecycle.spec.ts` 覆盖慢建会话、历史尚未落盘时的消息保留；真实 5173 桌面/手机只读复核通过） |
| A03 | 增量回复与工具开始/结束 | 最终结果之前可见真实变化 | P1 | pass（原有 PydanticAI/真实 DeepSeek HTTP/SSE 证据保留；2026-09-28 浏览器回归新增思考、准备工具参数、执行工具的区分及耗时显示，不展示原始推理文本；最终历史读取失败保留流式回复。模型响应速度未宣称改善） |
| A04 | 同 ID 并发提交/丢失响应重试 | 只执行一次；不同内容冲突 | P1 | pass（8 路并发同 ID 只建一个 turn；同 ID 不同内容 409；运行中二次提交 409；2026-09-28 浏览器回归覆盖重开时按服务端 request_id 核销丢失响应的待重试提交、明确拒绝后允许修改内容重发） |
| A05 | 生成中切换会话/刷新/断线重连/停止 | 接回同一 turn；部分输出保留；离开只取消订阅 | P1 | pass（原有游标续传与停止回归保留；2026-09-28 真实 HTTP 长连接夹具反复切换 8 次，仅当前会话保留一个订阅，无 stop 或重复提交；慢历史/提交响应不串会话，刷新回到 Pilot 原会话。夹具模型事件为合成，不计新 provider 验收） |
| A06 | 跨会话读取、非法游标、归档提交 | 明确拒绝且无执行副作用 | P1 | pass（跨会话 404、`?after=invalid` 400、归档后新提交 409 而已接受请求仍幂等） |
| A07 | 服务中断，隔日重开原会话并发送消息 | 加载框架工作记录；标明中断；Agent 可核查后继续 | P1/P2 | 代码回归通过：带新消息与空 prompt 续聊均读取原生快照并关闭未知工具结果；真实 provider 带新消息证据保留，空 prompt 真实复验待跑 |
| A08 | 必要提问/确认后继续；已授权普通操作无需逐次审批 | 用户决定可保留，提问和回答在聊天历史可见，不被旧审批门禁卡住 | P2 | 既有普通操作/删除确认/provider 证据保留；2026-09-28 修复 `session_ask` 回答因保存为 ToolReturnPart 而被历史过滤的问题，提问也纳入助手消息。真实 HTTP + 框架回归覆盖提问、回答及重开读取；真实 5173 会话刷新后恢复此前漏显的用户回答。新提示词行为尚未做 provider 验收 |
| A09 | 工具执行中退出，缺少最终结果 | 加载框架留下的事实；Agent 查询现场再续做，不要求人工处置账本 | P2 | 代码回归通过：原生中断快照经框架合成 interrupted tool return，续聊不会重放；真实 provider 空 prompt 复验待跑 |
| A10 | 长对话需要压缩时仍可继续 | 复用框架压缩；不建设记忆或计划系统 | P2（按需） | 代码回归通过：按原生快照时间选择压缩后的新记录，并向滑窗/摘要能力传递 context_window；真实长会话待 provider 复验 |
| A11 | 系统 Pilot 的保护和联动 | 产品需求记录，边界未冻结 | 后续需求 | 不纳入本轮验收 |
| A12 | 对话与 Graph/Run/Artifact 联动 | 本轮只验收已有对象的直接跳转 | P2/P7 | 代码与前端回归通过：对象跳转服从未保存编辑确认，Artifact 自动打开文件页并展开目标路径；浏览器完整复验待跑 |
| A13 | 研究目标与证据验收 | 产品需求记录，属于研究应用 | 后续需求 | 不纳入 Anchor 核心验收 |
| A14 | 附件、编辑分支与导出 | 产品需求记录，资源边界未冻结 | 后续需求 | 不纳入本轮验收 |
| A15 | 定时/Webhook 触发与运行时间线 | 支持周期及一次性定时；停机或 Graph 忙时错过的计划清楚显示、不补跑；Graph 忙时拒绝所有来源且不创建 Run；Webhook 不去重；统一 Run 输入 | E1 | 后端全量回归、桌面/移动浏览器看板、时间线筛选、计划 Modal、Run 预览与详情路径通过；看板已收敛为与 Graph/Pilot 一致的全屏工作区，并有独立浏览器回归覆盖计划/运行定位、窄屏滚动和无横向溢出；运行条与计划点统一使用限制在视口内的原生 Popover 详情，长名称换行；左右边缘、窄屏横向滚动、键盘聚焦和提示关闭回归通过；色条/圆点与按钮共用可见尺寸，周围空白不触发悬停或点击，相邻短 Run 双向命中通过；图例仅保留实际运行、计划时点与已错过；空心圆表示计划、带斜杠圆表示错过；Graph 颜色按当前名称集合使用 OKLab 最大间距分配；真实 provider 与真实停机恢复验收待做 |
| A16 | Bearer 白名单与 Responses 子集 | 所有 API 统一 key；同 key 的 previous_response_id 续聊；普通 JSON/SSE 文本请求 | E1 | 伪 provider HTTP JSON/鉴权测试与代码回归通过；真实 provider 和官方兼容细节未验收，不声明完整兼容 |
| A17 | 从 Graph 页面查看执行 | 选择 Graph 后直接看到当前/最近状态、运行次数；运行历史按需展开，按钮与画布位置保持不变，点击记录直达执行画布与节点/产物详情 | E1 前端体验 | 2026-09-28 运行概览融入 Graph 页头，原生浮层展示此图全部运行历史；工作台 E2E 覆盖桌面/手机展开位置与画布尺寸不变、超过四条历史、键盘/Esc/外部点击、详情直达和空状态。真实 provider 不属于此 UI 入口验收 |
| A18 | 开发服务访问 | 5173 页面及其后端代理可访问 | 开发环境 | 2026-09-28 两端无监听后通过现有 dev.sh start 恢复；首页、/graphs、/timeline?days=30、/sessions 经 5173 请求均 HTTP 200；仅验证本机访问，未验证用户侧端口转发 |
| A19 | 社区 Plugin 生态兼容任务交接 | 背景与目标见独立说明，具体方案和兼容验收待探索 | 独立任务背景 | 已生成 [任务背景](plugin-ecosystem-task-background.md)；仅完成文档交接，未实现或验收生态兼容 |
| A20 | 安装来源 Plugin 并统一 Anchor bundle 格式；兼容 Skills/资源与 MCP | 安装结果根目录 `plugin.json`；其他来源资源保留；不记录来源类型；明确暂不支持 hooks/commands/agents | 社区 Plugin 兼容 | 安装 API、根清单转换、Skill/资源浏览器 E2E、MCP stdio Bubblewrap 发现及调用回归通过；HTTP/SSE 环境变量鉴权配置与显式 OAuth 授权入口已接入。OAuth 外部 provider、真实社区 MCP 端到端未验收 |

| A21 | 每周四 09:00 本机工作周报与独立反馈 | 普通 Graph：项目理解、写作配图、独立评审与反馈、正式组装、Docmost 发布 | 完整真实 Graph 通过；用户内容评价待反馈 | 2026-09-30 完整 Run `20260930T151745-09281894662446388e3f9c0d41b79b40` 经三轮评审修正事实状态后发布到 MsteinL/Anchor周报，真实 SVG 上传和页面创建通过；末尾标准 detach 调用企业微信助手并收到平台 ACK。证据 `.local/graph-call-proof/weekly-wecom-live/evidence.json`；计划仍为周四 09:00，通知链已启用。 |
| A22 | Pilot 历史会话改名与删除 | 每行“…”菜单；改名持久化；确认后删除空闲会话；当前聊天及关联 Run/产物边界明确 | Pilot 会话管理 | 后端 336 项、前端 23 项单测与 E2E 13 条通过（provider opt-in 1 条跳过）；隔离真实 HTTP 浏览器覆盖持久改名、取消/失败保留、删除其他/当前会话、草稿与刷新、手机及键盘；本机页面只读核查菜单可用。未触发新 provider 请求 |
| A23 | 企业微信 Plugin 与 WebSocket 通道 | 长连接、事件去重、回复投递、自动监管、主动发送、附件处理及图文流式回复 | Plugin/通道联动 | 已确认真实私聊发送者标识保存在 Session，可从平台回调取得，无需先找管理员；按姓名查联系人尚未接入。 唯一受监管网关及已有真实文本私聊证据保留；新接运行时主动 Markdown 发送工具、真实附件提取/原生 BinaryContent、指定节点 summary 的持续 SSE、PNG/JPEG 回复。隔离真实 provider Graph 两轮通过（`.local/wecom-capability-proof/20260930T133215/evidence.json`）；真实 SDK 本地 WebSocket 发送/ACK/流式/图文及取消/重放回归通过；最终后端全量 412 项通过。四项真实企业微信公网验收、多平台监管、语音/卡片/反馈仍待做。详见 [能力矩阵](wecom-sdk-capability-audit.md) |
| A24 | 多用户 Graph 助手工作中枢 | 同图并发、独立原生历史、Plugin 调用、新消息打断并接续 | 近期目标 | 新增跨轮图片失败隔离、延迟 ACK/媒体接纳顺序、重启后旧事件抑制回归；真实 provider 图片/文件两轮接续通过。 普通 Graph 接线、两用户隔离、跨轮产物、消息打断、三消息交接、本地 stdio MCP 测试通过；真实 provider 基础链路通过。Pilot 的 `/sessions` 列表仅显示 Pilot 会话，企业微信 Graph 会话不混入 Pilot 选择器；相关隔离回归通过。跨轮跳过节点、模型初始化失败后的快照回退和 SDK 实际连接/重连回归通过；全量 359 项通过。逐业务用户数据授权、企业微信审批 API、RSI 尚未交付  本机 wecom-assistant 已挂 Docmost 并启用 network；原生工具接线下 21 项工具发现、刚发布周报的真实只读 MCP 调用通过（非模型对话验收）。 |
| A25 | 从工作证据改进 Plugin、Graph 与 Anchor | 普通 Graph 生成候选改动；可追溯证据、对比验证、按授权发布及效果观察；私有记录不跨用户泄露 | 远期效果验收 | RSI Graph walking skeleton 已能生成带证据、风险、验收和回滚条件的 `evolution.json`，但不自动修改或发布；真实 provider、候选实施、保留案例对比和长期效果仍未验收 |
| A27 | Graph 组合与独立调用及 Web 表达 | 保留内联；节点创建独立 Run，可辨调用模式、输入结果与具体执行关系；周报通知作为案例 | 已实现并部署；隔离真实模型/浏览器验收通过，真实周报→Docmost→企业微信提醒通过 | Op.call wait/detach、持久接纳/恢复、输入与产物、独立并发、会话串行/让出、Web 编辑/关系/父子导航通过。后端 479 项、前端单测 27 项、浏览器 17 项通过；1 个真实 provider 浏览器 opt-in 用例跳过，另有隔离真实模型报告→助手→同会话追问通过。证据 `.local/graph-call-proof/20260930T144935/evidence.json`、`browser-release/`、`deployment.json`。已按用户后续授权配置本机周报调用并跑通真实平台 ACK；证据 `.local/graph-call-proof/weekly-wecom-live/evidence.json`，后续定时运行沿用通知；命令 Uncertain 仍需核查，未提供历史链级联删除。 |
| A26 | 关闭开发会话后服务仍可用 | 宿主服务管理、自动重启、开机启动；只保留一个企业微信网关 | 常驻部署 | 已提供 systemd unit 并通过 `systemd-analyze verify`；当前执行环境 PID 1 为 Bash、systemd offline，无法激活宿主服务。临时恢复 8077，首页及鉴权 API、5173 代理均 200，唯一网关归属于 Anchor。宿主启用、关闭 Codex 后访问、崩溃重启及开机验收仍待做 |
| A28 | MCP 工具定义按需发现 | 挂载大量 MCP 工具时首轮隐藏完整定义；模型通过 Tool Search 按需揭示匹配工具；不改变连接、沙箱和运行记录边界 | Plugin/MCP 能力 | PydanticAI `defer_loading()` 已接入所有 Anchor MCPToolset；本地 provider 回归确认 MCP 定义首轮为 `withheld`、`search_tools` 可见且直接调用仍可执行。原生 provider Tool Search、中文检索效果、真实上下文/成本收益尚未验收 |
| A29 | CodeMode AgentNode 评估与自动接线 | AgentNode 自动批量调用普通 Plugin/MCP 工具；中间结果可压缩；副作用、恢复和嵌套轨迹边界明确 | Plugin/MCP 能力 | 评估报告见 [CodeMode 评估](codemode-evaluation.md)。已固定 `pydantic-ai-harness==0.36.0` + `pydantic-monty==1.0.0`；真实 provider 自动使用 Harness `CodeMode(tools='all', dynamic_catalog=true)`，无 Monty 时透明回退；Graph 不增加 CodeMode 配置。相关运行时测试及全量 480 项回归通过，Ruff、compileall、diff 检查通过；真实 Docmost/企业微信业务指标仍待做 |
| A30 | 每周 RSI Graph 运行与递归提案 | 同 Run 五领域审查 + 独立事实/提案双评审；动态证据；历史提案连续性；本地报告 | 18节点已部署，混合模型内容验收中 | 五专项用默认模型，综合/双评审用同服务 Pro 别名；原每周四09:00计划保留。此前单评审/默认模型完整运行内容失败样本均保留；Pro 对同一坏稿明确拦住不安全回滚与预算归因。双区域完整图和负向测试通过，HTTP断流修复后最终全量630项通过；最终真实验收 `.local/rsi-parallel-1bglowk_/` 正在运行。长期递归收益、候选实施、跨用户授权和完整社区覆盖未验收 |
| A31 | 同一 Run 内配对 fanout / join 局部并行 | 一一配对校验、分支并发、join 正确收束、乱序完成、循环轮次隔离、失败/停止/崩溃恢复、结果来源及画布表达 | 首期完成并更新本机开发服务 | Schema、调度、Web 作者和观察入口已接入；真实 DeepSeek 并行 Agent→join→综合通过，证据 `.local/parallel-provider-alot062w/evidence.json`；真实 API/浏览器、沙箱命令并行通过，证据 `.local/parallel-browser-h5M9HM/`。故障回归覆盖停止/暂停/失败、原生完成后进程退出、跨轮身份和部分文件接续。该阶段后端全量 563 项、前端 32 单测、20 浏览器用例通过，1 个既有真实 Pilot opt-in 跳过；build/Ruff/compileall/diff 检查通过。支持独立串行分支和区域外循环；嵌套、区域内路由/循环与部分成功策略不支持；RSI 应用验收另见 A30 |
| A32 | Anchor 总体架构指导与边界规则 | 新能力有稳定归属、事实所有者、调用契约和分层验收；保持 Run/Node/沙箱等既有核心边界；完整平台与最小 Graph 执行包复用同一 Runner | 目标指导已记录；独立分发闭包尚未实现 | `AGENTS.md` 与产品架构采用 Hexagonal/Ports-and-Adapters、Clean Architecture 依赖方向、DDD 限界上下文、按需 C4/ADR 作为轻量指导；另记录 Graph+明确 Plugin 资源+Runtime 的独立交付目标。当前架构确认 `anchor-graph` CLI 存在，但依赖闭包/打包格式/运行时兼容清单不存在；`serve.py` 与 `simple/run.py` 的边界压力也已记录。本阶段只做静态审视与文档治理，没有重构或声称问题已修复 |
| A33 | Rust/Rig AgentNode 纵向切片 | 共享 Runtime Kernel 以 Rig `AgentRun` 为可持久化协议状态；Completion/Tool 通过端口注入；取消、路由校验、工具调用与 checkpoint 恢复 | 单节点真实 provider smoke 通过；真实沙箱、平台接线、流式/中断、独立分发未验收 | 实验分支 `codex/rust-rig-runtime` 新增 `NodeRequest`、`CompletionPort`、`ToolPort`、`NodeExecutor`、OpenAI-compatible `chat/responses` 构造和 `examples/real_node.rs`。真实现有模型配置的结构化结果 + 工具调用 smoke（2 requests，summary=`fixture-ok`）证据保留。新增 AgentNode 路由契约回归：多出口缺失 route 拒绝且反馈可操作，0/1 出口仍可省略，非法 route 仍拒绝；Rust 测试 29 项、Clippy `-D warnings`、fmt、diff 检查通过。当前仍是实验实现，不替代 Python Runtime，也未声称真实平台端到端完成 |
| A34 | Rust/Rig 重构计划与持久化边界 | 迁移阶段、唯一 Runner/Kernel 目标、端口隔离和分层验收；CheckpointStore 原子保存与身份校验 | 计划已冻结；R2 checkpoint 边界与 pending model/tool 续行通过，未知副作用和宿主接线进行中 | 计划见 [Rust/Rig Runtime 重构计划](rust-rig-migration-plan.md)。R1 真实单节点 smoke 保留；R2 新增 `CheckpointStore`、`FileCheckpointStore`、checkpoint pending step 和 `execute_with_store`，测试共 13 项通过，Clippy/fmt/diff 通过。尚未开始 Graph Runner 或 Python 宿主接线 |
| A35 | Rust/Rig Sandbox port 与 Bubblewrap host adapter | Kernel 仅通过端口表达执行意图；host adapter 依据授权配置隔离执行并保留输出/取消边界 | Kernel 契约、真实 Bubblewrap 只读挂载 smoke 与 fake-helper 负向测试通过；真实网络隔离、host 路径竞态、超时/取消和 Python/平台接线仍未验收 | Kernel `sandbox.rs` 定义 request/result/port/no-op；新增 `rust/anchor-sandbox-bwrap`，强制配置 workspace/source/tool/spill 授权 roots、命令白名单、可选网络权限和 mount destination roots。Bubblewrap 启动前 canonicalize host path，拒绝 workspace 越界 symlink 和受控 namespace mount 覆盖；通过 info-fd 确认 sandbox 启动后才报告 Completed，env 值不进 argv，host spill 路径脱敏；spill 目录必须由宿主预创建，适配器先 canonicalize 并校验授权根，不创建请求指定路径。有界双流捕获、spill 配额、incomplete、超时与取消已由 fake helper 测试覆盖；本机真实 bwrap 测试验证只读输入挂载及 workspace_readonly 写入被拒。`cargo test --workspace`：Kernel 27 项、adapter 8 项通过；Clippy `-D warnings`、fmt、diff 检查通过。尚未接入 Python Sandbox/平台；当前证据不覆盖网络 namespace、host 文件系统并发篡改竞态或真实 adapter 超时/取消 |
| A36 | Rust/Rig provider/stream 中断续行边界 | provider timeout 或流式取消后 pending model step 可序列化、重载，并绑定新 provider 续行；完成态不残留 pending step | provider-free 确定性中断/续行通过；真实进程退出、真实 provider 断线及平台接线仍未验收 | `NodeExecutor::execute_with_store_and_policy` 同时应用超时策略并在 I/O 边界自动保存 checkpoint；测试通过该 API 验证 timeout 后重载及新 provider 恢复。另覆盖部分 stream event 后取消、checkpoint 编解码和新 provider 续行。`Done` 时清除 pending step 并保存终态。Kernel Rust 测试 27 项、Clippy `-D warnings`、fmt 通过；该证据不代表真实进程/provider 故障恢复 |
| A37 | Rust 串行 Graph Runner 架构冻结 | 复用展开快照与现行 Run 事实；Run state 与 Agent checkpoint 分离；Crash reconciliation、输入 commit 固定；明确 R5/R6/R7/R8 边界 | 契约已冻结 | 见 [Rust/Rig Runtime 重构计划](rust-rig-migration-plan.md) 的 R5 契约：R5 只做 Rust Kernel Runner + Python oracle provider-free 语义对照，宿主接线留 R8；R5 明确拒绝 fanout/join，R6 实现；`Op.call`/Plugin 通过声明能力的专用执行 port，未支持时 admission 拒绝，R7 接入；Run cursor 与节点完成事实不一致时 fail closed，不重放未知副作用。A38 跟踪实现阶段 |
| A38 | Rust/Rig 串行 Graph Runner 首个 vertical slice | 消费 Python 展开快照；持久 Run/cursor；节点与 artifact 端口；串行路由、预算、恢复与控制边界 | 基础 Kernel slice/provider-free 测试通过；R5 阶段仍进行 | `rust/anchor-runtime/src/graph.rs` 新增严格 `GraphSnapshot` admission、Python `graph.to_dict()` fixture、稳定 Run/digest/invocation key、原子 `FileRunStore` 与 OS advisory lease、`NodeExecutionPort`/`ArtifactPort`/`RunControl`，并对 fanout/join、Plugin、Graph call 和未声明能力拒绝。fake ports 回归覆盖上游精确 commit、路由/未选分支、回边/ceiling/module reentry、预算同 invocation 续行、完成与失败事实 crash window、Uncertain、pause/stop 与失败不推进。Agent/Op.run 的 model/network/wall-time/command传给 host port；未创建真实 NodeExecutionPort/ArtifactPort、平台或 CLI adapter。Rust workspace 48 runtime + 8 Bubblewrap tests，Clippy/fmt/diff 通过；Python Runner 对照基线 66 项通过。完整 Python oracle 差分和 R5 阶段验收仍未完成 |
| A39 | Rust Graph Runner provider-free Python oracle 与 Run 格式兼容 | 对齐可映射的路由、循环上限、预算停止、模块重入，并安全迁移已有 Run 记录 | 十个 oracle 场景及 format v1→v2 定向迁移通过；更广覆盖仍进行 | `scripts/generate_r5_python_oracle.py` 调用 Python `anchor.simple.run.run` 生成 `tests/fixtures/r5-python-oracle.json`；Rust fixture 测试比较状态、有效输入、节点调用顺序、passes、ceased、cursor 与边决定。十种场景覆盖多出口、自环 ceiling、budget stop、模块重入、两节点 back-edge、diamond convergence、pause-before-dispatch、单出口失败、未启动 SCC 闭包及嵌套模块+外层 back-edge。对照修正启动计数、自环序号、失败终止事实、跳过传播与模块 activation ceiling 边界。Run record 升为 format 2；format 1 的 cursor 在校验旧 identity 后补记已启动 invocation/pass，无 cursor 只迁格式；迁移写回由 Runner 持 lease 时完成。Rust workspace 57 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、diff 检查通过。场景覆盖仍有限，也不等于真实宿主/Provider 验收 |
| A40 | Rust FileRunStore 跨进程 lease 退出释放 | 并发进程只允许一个 Run lease 持有者；持有进程非析构退出后可重新接管 | 本机本地文件系统的子进程测试通过；完整 Runner 崩溃恢复及其他文件系统仍待验收 | Rust test binary 子进程先持 lease 并通过管道报告就绪；竞争进程得到 `RunBusy`；holder 用 `process::exit` 绕过 Rust 析构退出后父进程成功重取 lease。Rust workspace 49 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、diff 通过；Python `test_simple_run.py` + `test_node_controlflow.py` 66 项通过。证据限于本机 temp filesystem/OS advisory lock，不代表网络文件系统或端到端 Graph Run 崩溃恢复 |
| A41 | Rust Graph Runner 已知失败的终止事实 | 确定失败应保留启动计数、终止当前 Run、不推进边且不伪造成功提交；未知结果仍保留 cursor | 已知失败与完成事实恢复通过；与 Python 单出口失败边记录有意不同 | Rust `NodeExecutionOutcome::Failed` 与已持久化 `CompletionFact::Failed` 清除 cursor、保留 pass/invocation、写入 `failed` 与节点错误，且不产生 selected edge 或执行下游；`Uncertain` 保留 cursor 并 fail closed。测试模拟“失败事实已持久化、Run 终态保存失败”后恢复，确认节点不重放。oracle 显示 Python 当前会在失败时结算单出口边；Rust 有意不继承该事实，差异由命名断言固定。workspace 49 runtime + 8 Sandbox tests、Clippy/fmt/diff 通过 |
| A42 | 串行 Graph 跳过传播与汇合 | 未选分支显式传播 false 到下游；所有入边确定后，有选中输入的 merge 正常运行；旧轮输出在新轮输入拒绝后失效 | Python 与 Rust provider-free 回归及七场景 oracle 通过；fanout/join 并行语义仍留 R6 | Python Runner 与 Rust Kernel 都在单 Run 调度边界固定点传播未选边，不启动被跳过节点；diamond 中执行 `start → left → merge`、跳过 `right`，并以精确提交输入只带入 `left`。Rust 同时覆盖多级跳过链、未进入模块循环不虚构决定、回边新 false 输入撤销旧 selected 输出、模块 activation ceiling 的本轮入口/出口事实。Python `test_node_controlflow.py` 45 项；Rust 53 runtime + 8 sandbox；Clippy/fmt/diff 通过。该能力是串行条件分支收束，不启用 AgentNode fanout/join 并行 |
| A43 | Rust GraphRunRecord 与 RunStore 跨字段完整性校验 | 持久记录的结果、commit、计数和边决定必须能回指到冻结 Graph 与源结果；无效记录不得读取或保存 | provider-free 损坏记录拒绝测试通过；更多运行故障路径仍可扩展 | `GraphRunRecord::validate` 检查结果节点/Run/digest/invocation/commit身份、序号唯一与范围、计数边界、边是否属于快照、decision序号及其source result引用。selected edge必须精确引用完成结果；所有引用实际结果的决定序号必须晚于该结果；false propagation和ceiling拒绝允许明确的合成事实。`FileRunStore::load` 迁移后校验、`save` 落盘前校验；损坏身份、重复/越界序号、未知边、悬空选择及过期 self-loop decision 均被拒绝。Run format未改变 |
| A44 | Rust Graph Runner 节点边界 pause | 用户暂停发生在 dispatch 前时不创建节点调用、pass、边决定或活动 cursor，并保留可恢复 Run | Python oracle 与 Rust provider-free 对照通过；活动节点中的暂停仍由 Node/RunControl 专项路径验收 | 新增 Python `pause_before_first_dispatch` oracle：`status=paused`、`reason=asked`、无执行/pass/edge/cursor；Rust 使用既有 `RunControl.pause_requested` 比对同一 Run 事实。十场景 oracle与 workspace 57 runtime + 8 Sandbox tests、Python相关子集、Ruff/Clippy/fmt/diff通过 |
| A45 | Rust 路由契约失败后的恢复事实 | 节点已完成但 route 无效时，终止该 Run、清除确定失败 cursor、不结算边；保存失败中断后恢复不得重跑节点 | 持久化写入失败注入及恢复测试通过 | 非法 route 不再保留看似可续行的 cursor；若 failed Run 保存失败，旧 cursor 留在持久记录，但重入时从已持久化 completion fact 重验 route 后终止且不再次 dispatch。Rust workspace 54 runtime + 8 Sandbox tests、Clippy/fmt/diff通过 |
| A46 | 未启动循环 SCC 的安全跳过传播 | 仅在 SCC 未执行、所有外部入口均已否决或来自已证明 inactive SCC 时传播 false；合流仍执行且活动/未决入口不被误收束 | Python/Rust provider-free 回归及 10 场景 oracle 通过；并行区域仍受原有 fence 保护 | 新增闭合环释放 diamond merge、selected/undecided ingress 保护、双 SCC 级联、确定性顺序及二层嵌套 module 经外层 back-edge 重入用例；环内/出边 false 作为持久事实写入，不伪造节点结果。Python control-flow 子集 48 项；Rust workspace 57 runtime + 8 Sandbox tests；oracle 10 场景、Ruff、Clippy `-D warnings`、fmt、diff 检查通过 |
| A47 | Rust Run 结果顺序与 cursor 输入提交校验 | 结果必须按执行 sequence 递增；cursor 输入 commit 必须逐项匹配当前 selected ingress 的 source result | FileRunStore 损坏记录 load/save 拒绝测试通过；无需 Run format 升级 | 新增对逆序 loop results，以及 cursor input commit 缺失、多余、身份不匹配的拒绝测试；既有 Failed+cursor uncertain 表示仍允许。Rust workspace 57 runtime + 8 Sandbox tests、Clippy、fmt、diff检查通过 |
| A48 | Rust Graph Runner 跨进程恢复故障窗口 | cursor 已持久化但节点未执行、节点完成事实、节点失败事实或 artifact commit 后 Runner 异常退出；新 Runner 依据 FileRunStore 与 host facts 恢复，不重放确定完成/失败的节点 | provider-free 子进程故障注入通过；真实 host/provider 与平台接线仍待验收 | 四个子进程窗口分别在首次读取 `NotStarted` 后、持久 Completed fact 后、持久 Failed fact 后、持久 artifact commit 后立即 `process::exit`。父进程验证 cursor 与 lease 可重取；未执行场景以同一 invocation key dispatch 一次并 Completed；Completed/artifact 场景不重复 dispatch/freeze；Failed 场景恢复为 Failed、清 cursor、不结算边、不 freeze 且不重 dispatch。Rust workspace 61 runtime + 8 Sandbox tests、Python 70 项相关子集、10 场景 oracle、Clippy `-D warnings`、fmt、Ruff、diff 检查通过；test-only durable ports 不代表真实 provider 或生产 host adapter 端到端恢复 |
| A49 | Rust RunStore 已提交但调用反馈失败 | RunStore 原子写入已经提交、调用方收到 Err 时禁止用旧内存快照继续；必须重载 durable Run 后恢复，且完成事实、dispatch 与 artifact freeze 不重复 | provider-free post-commit feedback 故障测试通过；真实 host/provider 与平台接线仍待验收 | 测试包装 FileRunStore：底层保存含结果的最新 Run 成功后，wrapper 故意返回 Err；旧快照重试返回 `RunConflict`，从 FileRunStore reload 后由新 Runner 恢复为 Completed，唯一 invocation/result/dispatch/freeze 均为 1。只验证 FileRunStore + test ports，不模拟真实目录 sync/磁盘故障，也不代表生产 host/provider 恢复验收 |
| A50 | Rust fanout/join 拓扑准入与 activation 持久事实 | 展开快照中的 fanout/join 一一配对、分支拓扑明确；持久 activation 绑定 Run/fanout invocation、分支路径与游标；旧 Run 可明确迁移 | Provider-free schema/facts 切片通过；并行调度另由 R6 记录 | Rust admission 拒绝孤立/复用配对、分支交叉/循环/嵌套及外部 join 输入；新增分支 activation 状态/路径/identity、cursor/pass/输入 commit 校验，Run format 2→3 显式迁移。该切片保留为 R6 基础，不再描述 Runner 拒绝调度 |
| A51 | Rust fanout/join 单区域调度与控制事实 | 同一 Run 内单 Coordinator 局部并行；分支内串行、分支间并发；join 等全部分支完成后才生成 | 核心 provider-free 两分支切片通过；完整 R6 仍未通过，因并行 wave 进程崩溃恢复和真实 host/provider/平台接线仍待验收 | fanout/join 都由 Coordinator 生成确定性 manifest 并冻结为普通 RunResult/artifact；join 不要求 NodeExecutionPort `op_run` capability，后续 AgentNode 读取 join commit。分支 cursor 先持久化，FuturesUnordered 按完成到达顺序逐个 settle/save；Failed/Uncertain fail-closed，已启动 futures drain，Cancellation 不宣称同伴 exactly-cancel。并行分支已复用 node `max_rounds` 和 module activation ceiling。当前测试验证 join manifest 含两支结构化事实、activation 保存精确 branch commit；尚未单独断言 manifest 每个 commit 与 branch state 的逐项相等。Rust graph tests 45 项、runtime 74 项、Clippy/fmt/diff、Python 70 项相关子集已通过；并行 wave 进程崩溃窗口和真实接线仍未完成。模块拆分设计见 `docs/rust-graph-module-design.md` |

每阶段执行相关后端测试、Ruff/compileall、前端测试/build、真实 HTTP 与浏览器验收。最终必须取得全量 pytest 的明确退出码和总结；运行中或仅看到进度点不算通过。阶段产物不能等同于产品全部完成。

## 推进记录

- 2026-10-02：R6 Rust fanout/join 单区域调度接入。fanout 与 join 均由唯一 Graph Coordinator 生成确定性 manifest，并通过 ArtifactPort 冻结为普通 RunResult/commit；join 不要求 NodeExecutionPort 的 `op_run` capability，后续 AgentNode 消费 join commit。不同分支用 FuturesUnordered 并发，分支内沿静态路径串行；cursor 先保存，完成按到达顺序逐个 freeze、记录结果和边、更新 branch progress 并保存。provider-free 测试覆盖并发峰值 2、乱序完成、join 两支结构化事实、失败阻止 join、BudgetStopped/Cancelled reload 不重放已完成分支，以及 format3 FileRunStore activation roundtrip。Graph tests 43 项、Clippy/fmt/diff 已通过；当前仍是 fake Node/Artifact ports，未完成真实 host/provider、平台接线和进程崩溃恢复，Cancellation 只保证 drain/fail-closed，不宣称同伴 exactly-cancel。

- 2026-10-02：R6 切片独立验收与组织评审。Rust runtime 74 项、Graph 45 项、Clippy `-D warnings`、fmt、diff 检查及 Python 相关子集 70 项通过；确认 fanout/join 核心控制事实和局部并行路径成立，但不把它记为完整 R6。代码审查发现并行路径曾绕过普通 Runner 的 node `max_rounds` 与 module activation ceiling，已抽出分支准备逻辑并补回归：同一并行区域只计一次 module activation，重入超过 ceiling 或分支 node ceiling 时 fail-closed 且不调度 join。并行 wave 的进程崩溃窗口和真实 host/provider/平台接线仍未验收。随后按事实所有者和变化原因完成首轮模块拆分：`graph/model.rs`、`admission.rs`、`state.rs`、`store.rs`、`ports.rs`、`runner.rs`、`logic.rs` 及独立测试文件；没有新增第二个调度器。设计见 `docs/rust-graph-module-design.md`。

2026-10-02：扩展 A48 的 R5 跨进程恢复边界，未改生产执行逻辑。新增子进程在 Run cursor 已由 `FileRunStore` 持久化、`completion_fact` 首次观察到 `NotStarted` 后退出；新 Runner 验证沿用相同 InvocationKey 执行一次并完成。另新增 durable `Failed` fact 写盘后退出；恢复后 Run 为 `Failed`、cursor 清除，未重复 dispatch、未写结果/边决定、未 freeze artifact。两例均在恢复前显式验证 lease 可重新取得。Rust workspace 61 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、Python `test_simple_run.py` + `test_node_controlflow.py` 70 项、10 场景 oracle 生成及 Rust 差分、Ruff 与 diff 检查通过。均为 provider-free test ports 与本机子进程证据，不代表生产 host adapter/provider 恢复验收。

2026-10-02：推进 R5 未选分支闭环与 Run 持久事实校验。Python 与 Rust Runner 使用 SCC fixed point 收束从未运行且所有外部 ingress 均为 false（或来自已证明 inactive SCC）的循环组件；将组件内部回边及出边记为合成 false 事实，从而释放另一路有 selected 输入的 merge。selected/未决 ingress、entry SCC、历史已运行成员及 fanout 活动区保持保守，不批量改写。新增双 SCC 级联、故意逆序声明组件、闭合环 join、未决 ingress 和 Python 9 场景 oracle 对照。`GraphRunRecord::validate` 另核对每节点结果 sequence 严格递增，并要求 cursor `input_commits` 精确匹配当前 selected 入边对应的 source commit。Python `test_simple_run.py` + `test_node_controlflow.py` 通过；Rust workspace 57 runtime + 8 Sandbox tests、Ruff、Clippy `-D warnings`、fmt、diff 检查通过。此为 provider-free Runner 语义与损坏记录校验，不是完整进程崩溃恢复或宿主接线验收。

2026-10-02：扩展 R5 oracle 覆盖递归模块展开与回边组合。新增 `outer → inner` 二层 module，根图经 `after → use_outer` 回边重入两次；Python Runner 生成的展开快照保留两个 module scope ceiling，Rust oracle 差分核对完整调用顺序、passes、ceased、cursor 与边决定。生成器输出 10 场景；`test_graph_modules.py`、`test_simple_run.py`、`test_node_controlflow.py` 相关子集通过；Rust workspace 57 runtime + 8 Sandbox tests 通过。仅为 provider-free 展开快照/调度对照，不代表模块解析已迁入 Rust 或真实宿主验收。

2026-10-02：新增 R5 RunStore post-commit feedback 故障回归，不改生产逻辑。测试包装 FileRunStore，在含完成结果的 Run 已原子落盘后故意向 Runner 返回 Err；验证旧内存快照重试因与 durable 事实冲突而返回 `RunConflict`，必须 load 最新记录后由新 Runner 恢复至 Completed，且 invocation/result/dispatch/artifact freeze 均只发生一次。Rust workspace 62 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、10 场景 Python oracle 重生成及差分、Python `test_simple_run.py` + `test_node_controlflow.py` 70 项、diff 检查通过。该 provider-free 故障注入不模拟真实目录 sync/磁盘故障；真实生产 host/provider/platform 接线仍待做。

2026-10-02：补齐 R5 Graph Runner 跨进程恢复的核心故障窗口测试。子进程在 Run cursor 已持久化后，分别于 Node port durable Completed fact 落盘后、Artifact port durable commit 落盘后立即异常退出；父进程由 FileRunStore 读取旧 cursor，以新的 Runner 和 durable test ports 恢复。两种窗口均无第二次 node dispatch，恢复成 Completed，且结果指向唯一稳定 commit。测试只验证 Runner + FileRunStore + test ports 协作及本机进程退出行为；真实 provider、生产 Node/Artifact host adapter、真实工作区提交和平台重启仍待验收。Rust workspace 59 runtime + 8 Sandbox tests、Python `test_simple_run.py`/`test_node_controlflow.py` 70 项、10 场景 oracle、Clippy、fmt、Ruff 和 diff 检查通过。

2026-10-02：对齐 Rust/Rig AgentNode 与 Python `NodeRuntime` 的路由契约。多出口节点必须显式返回一个合法 `route`；0/1 出口仍允许省略 route，显式提供的非法 route 继续拒绝。同步修正 `NodeRequest` 提示，避免把多出口 route 描述为可选；`InvalidResult` 对缺失 route 返回允许出口列表。新增多出口缺失拒绝及 0/1 出口可省略回归，保留非法 route 负向用例。`cargo test -p anchor-runtime-rig` 29 项、Clippy `-D warnings`、fmt、diff 检查通过。此为 A33 provider-free 契约回归，不改变既有真实 provider smoke 证据，也未扩大到 Graph Runner。

2026-10-02：冻结 R5 Rust Graph Runner 边界。Rust Runner 消费 `graph.json` 同形的展开快照，不重新实现模块解析；Run 状态记录身份/快照摘要/cursor/边决定/提交历史，与 AgentNode Rig checkpoint 分离；节点完成事实与 Run 状态对账，冲突或未知外部副作用 fail closed；下游只接精确 commit。R5 为串行 Kernel Runner 与 Python oracle provider-free 对照，fanout/join 明确拒绝并留 R6，Graph call/Plugin 由声明能力的执行 port 承接并留 R7，Python/CLI 接线留 R8。`max_steps` 与 Rig `max_turns` 不视为等价。该条是设计冻结，不表示已有 Graph Runner 代码或验收通过。

2026-10-02：实现 R5 首个 Rust 串行 Graph Runner vertical slice。新增 `graph.rs` 的严格展开快照 admission（含 `_module_rounds`/路径/重复边/未知字段校验）、snapshot digest 与 invocation identity、独立 GraphRunRecord、原子 FileRunStore+OS advisory lease、节点/Artifact/RunControl ports。fake-port运行测试覆盖串行分支、exact commit输入、model/network/time/Op.run策略传递、必须/非法route、回边及 node/module ceilings、预算停止同key续行、完成和失败事实的RunStore提交窗口恢复、Uncertain fail-closed、pause/stop、fanout/Plugin/Graph-call/未支持预算能力拒绝。新建 fixture 由 Python `graph.parse()` + `graph.to_dict()` 从 `examples/graphs/one-search.json` 生成并由 Rust admission读取。复核修正 ceiling 只在节点存在新鲜选中输入后才拒绝，防止提前截断合法下一节点；terminal route/failure 事实在 RunStore 写入错误窗口可恢复且不重放；Run cursor identity 校验不匹配时在 dispatch 前拒绝。Rust workspace 45 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、diff 检查通过；Python `tests/test_simple_run.py tests/test_node_controlflow.py` 66 项通过。仍未完成完整 Python oracle 差分、真实平台/CLI接线、真实 NodeExecutionPort/ArtifactPort或进程死亡后 advisory lease释放验收；R5 保持进行中，未宣称完整 Graph Runner 或 Rust 迁移完成。
2026-10-02：补齐 R5 首轮 Python Runner oracle 对照与 Run 格式迁移。新增可重复生成器 `scripts/generate_r5_python_oracle.py`，直接运行 Python `anchor.simple.run.run`（仅以确定性 fake 替代节点/provider），生成三种场景 fixture：多出口选路、self-loop ceiling、budget stop。对照发现并修复两个差异：budget stop 必须在 cursor 创建时已记录 pass/invocation；边决定序号必须晚于结果序号，才能识别 fresh self-loop，同时 `result_sequence` 保留精确上游 commit 选择。GraphRunRecord 升级到 format 2，format 1 记录经旧 cursor identity 校验后显式迁移；迁移加载只在内存完成，Runner 持 lease 后再写回。Rust workspace 48 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、diff 检查通过；三例 oracle 已通过。该有限对照不是完整 Python 语义等价或真实宿主/Provider验收，R5 仍进行中。
2026-10-02：扩充 R5 Python oracle 到五种调度场景，增加 module scope 重入 ceiling 与两节点 back-edge ceiling。均由真实 Python `run()` 生成最终 passes/ceased/cursor/边决定，Rust 差分测试全通过；生成器复跑、Ruff、Rust workspace 48 runtime + 8 Sandbox tests、Clippy、fmt、diff 检查通过。覆盖面仍有限，未宣称 R5 阶段完成。
2026-10-02：补充 FileRunStore 跨进程 advisory lease 退出释放验证。子进程持锁期间竞争进程观测到 `RunBusy`；holder 绕过 Rust 析构退出后父进程成功重新取锁。Rust workspace 49 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、diff 检查及 Python Runner 相关 66 项通过。该结果只覆盖本机临时文件系统锁行为，不等于完整 Graph Runner 进程崩溃/恢复端到端验收。
2026-10-02：明确 Rust 已知节点失败的终止语义。与 Python 当前在确定失败后仍记录单出口边不同，Rust 对已知 `Failed` 保留启动计数、清 cursor、写失败原因、不结算边；`Uncertain` 则保留 cursor。新增 oracle 有意差异断言，以及已持久化失败事实后 Run 保存失败再恢复的 no-replay 测试。Rust workspace 49 runtime + 8 Sandbox tests、Clippy、fmt、diff 检查通过；该语义归属由 Runner 的失败安全事实所有，不追求对 Python 偶然状态的逐字段复制。
2026-10-02：修复串行条件分支传播。被所有入边否决的节点不会执行，其出边以 `selected=false` 递归持久化，确保 diamond merge 在全入边可决后运行；Python 回归确认只接收选中上游的精确 commit。Rust 同步实现固定点传播、对未运行模块循环保持保守，以及已运行节点新一轮全 false 输入时撤销旧输出选择；模块 activation ceiling 记录本轮入口和模块出口未产出。七场景 Python oracle、Python `test_node_controlflow.py` 45 项、Rust workspace 53 runtime + 8 Sandbox tests、Clippy/fmt/diff 检查通过。该变化不启用 fanout/join 并行，R6 边界保持不变。

2026-10-01：运行看板颜色改为集合感知分配。旧方案逐名哈希，rsi 与 deep-academic-research 色相仅差约 10°。现在将当前 Graph、历史运行与计划中的名称合并去重排序，在 OKLab 感知空间用最远点策略选择最大间隔 HSL 色；同一名称集合映射稳定，Graph 增删可能重排旧颜色。图运行记录状态仍用边框/纹理表达，不复用颜色编码。前端单测 32 项、E2E 20 项通过（1 项既有 opt-in 跳过），生产构建通过。

2026-10-01：纠正 RSI 与周报计划同刻的问题。核对 /schedules 与 state/schedules.json，周报计划 ID 6ae103ac-2794-4e93-954d-fa490358fb36 一直存在且 enabled，仍为每周四 09:00；10-01 09:00 确已触发，但该 Run 在 understand 因 AttributeError: NodeRequest.code_mode 中断，计划随后推进到 10-08。此错误不是计划删除。RSI 原先也被设为每周四 09:00，属本次 RSI 部署的时段选择错误；经 API 只删除旧 RSI 计划并新建周四 10:00，保留周报计划 ID、规则与 next_at 不变。当前周报下次 10-08 09:00，RSI 下次 10-08 10:00。未重跑周报，避免未确认地触发其 Docmost/WeCom 发布副作用。

2026-10-01：A30 最终 Pro Run `.local/rsi-parallel-_iyqcmvh/` 流程完成并通过当时的双评审与门禁，但独立抽查仍发现三处范围错误：把 invocation=1 写成历史首次 fanout、未区分隔离验收 Run 与 production completed、把 code_mode 失败归因穷举为未提交工作树/重启。该 Run 的内容状态改记为不通过，报告与证据保留。review Op 新增确定性范围门禁，覆盖这些无界历史/因果措辞；旧报告复算现返回 revise。相关负向测试 16 项、Ruff 通过；尚未重新运行完整 provider 图，因此不宣称最终内容发布通过。

2026-10-01：A30 实现验证收尾。最终全量后端回归 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal -o addopts=''` 为 631 passed、退出码 0（57.34 秒，4 条既有 SDK 提示）；Ruff 与 diff 检查通过，8077/5173 在线且 rsi 无活动 Run。确定性 review 安全门禁在旧发布稿上复算返回 revise，证明危险回滚不会被模型 publish 覆盖。尚未为该门禁重新发送完整 provider Run，因此 A30 内容状态保持“上一完整 Run workflow finished、内容抽查失败；修复待下一次真实 Run”，不宣称本周报告内容已通过。

2026-10-01：A30 发布后独立抽查发现 RSI-009 回滚仍写“回退到当前全量暴露”，与保留权限保护冲突；此前双评审漏检，因此该完整 Run 的 workflow 通过、内容仍不通过。已将 `scripts/rsi/review.py` 增加确定性危险回滚门禁（全量暴露、整文件不脱敏、关闭/绕过保护等直接 revise），新增负向测试，相关 RSI 子集 63 项通过。原 Run `.local/rsi-parallel-1bglowk_` 与报告保留，不手改历史；下一次真实 Run 才能验证该门禁在发布路径中生效。

2026-10-01：A30 断流修复阶段全量收口：630 passed、退出码0（57.93秒，4条既有SDK提示），Ruff/diff通过。冻结后重跑混合模型完整图 `.local/rsi-parallel-1bglowk_/`（Run `20261001T122158-ab16cce3eaf045c993c83cef6f407985`），research正常提交，16 PyPI+23 npm成功；36 GitHub仓库活动仍partial且本窗口无可用release/issue条目，不能称完整社区取证通过。五专项继续并发，后续综合和双评审待验收。

2026-10-01：A30 新版真实启动暴露 HTTP 断流边界。`.local/rsi-parallel-aq8afxdq/` 已启动五分支，但 research 在 GitHub chunked 响应中收到 http.client.IncompleteRead，旧 _get 未捕获 HTTPException，导致分支失败并按既有语义取消同伴；没有进入综合/评审。修复为与超时/HTTP错误一致记录该端点的不可用证据，分页标 partial，保留其他成功来源；不将不完整字节解析成有效响应。25 项研究子集通过，包括真实异常类型的断流注入与分页降级。保留失败 Run。18 节点已部署、桌面/手机浏览器加载无错误，截图/证据 `.local/rsi-upgrade-proof/browser-reviews-evidence.json`；混合模型完整内容验收继续。

2026-10-01：A30 真实模型反例对照与按角色选模。双评审默认 deepseek-flash 的反例 `.local/rsi-review-pair-w3a83hia/` 虽返回 revise 并捕获更多架构问题，仍把“整份不脱敏”误判为正确，不能计关键反例通过。同服务可用 deepseek-v4-pro；隔离对照 `.local/rsi-review-pair-q2jegnql/` 明确将不安全回滚、错误 inspect/research 预算归因等列为 open，返回 revise。新增可选 ANCHOR_MODEL_ALIASES，只给同一 .env 端点/凭证增加模型名别名，禁止覆盖 models.default；既有 models.academic 等未配置引用保持默认回退，Pilot 不变。629 项全量通过、退出码 0（57.51 秒，4 条既有 SDK 提示），相关模型/节点测试与 Ruff 通过。部署18节点：五专项默认 flash，analyst/fact-review/proposal-review 选 models.rsi-quality→pro；review Op 增加只读 code 授权，原计划未改。备份 `.local/rsi-upgrade-proof/20261001T201844/`；确认无活动 Graph/Session 后重启8077/5173。该反例对照不代表完整18节点报告验收，下一步冻结配置实跑。

2026-10-01：A30 单评审内容验收再次失败，转为两个独立评审。完整 `.local/rsi-parallel-qnt4mhri/` 真正执行修改报告反馈后 finished，但发布稿仍建议无法保真时关闭脱敏，且误解 per-Run invocation/各节点 commit、扩大 GitHub 失败范围、混淆 inspect 模型预算与 research 时长；内容不通过已保存在 content-acceptance.json，报告原样保留。新增第二组成对区域 fact-review/proposal-review，分别聚焦事实/研究与方案/验收/风险/回滚；普通 review Op 校验 analysis 与各自 join commit，确定性合并意见，任一拒绝不能被另一方覆盖。无需改核心运行时。完整图测试覆盖两组区域及两种反馈，新增聚合负向用例；最终全量 624 passed、退出码 0（55.89 秒，4 条既有 SDK 提示），Ruff 通过，独立契约复审无阻断。真实坏稿双评审验证进行于 `.local/rsi-review-pair-w3a83hia/`，新版尚未以完整新 Run 验收或部署。

2026-10-01：A30 评审反例真实复验通过。复用失败稿及同一冻结证据，在 `.local/rsi-review-regression-y58rugxk/` 只替换新版 reviewer；真实模型返回 revise，将历史状态矛盾、错误 API 状态验收、脱敏提案验收不一致列为 open，未再把已知错误当 limited 放行。这是反例检查，不代表所有语义错误必被检出。新版分析/评审提示通过 API 部署，备份 `.local/rsi-upgrade-proof/20261001T193244/`，原计划未改；冻结后启动完整真实验收 `.local/rsi-parallel-qnt4mhri/`，采集 errors=[]，五专项已同时活动。之前完整运行的内容失败仍保留，当前等待最终报告独立抽查。

2026-10-01：A30 区分流程成功与内容失败。`.local/rsi-parallel-2eqh8sx8/` 真实 Run finished、峰值 5 活动分支、14 节点各一次，gate/publish 均执行；但评审把已知事实矛盾与不可达验收列为 limited，另有未采集验收扩大为未验证、不同图同刻触发无据推为冲突、脱敏字节变化扩大为源码损坏等问题。独立抽查判定内容不通过，记录 content-acceptance.json，不修改原报告/Run 终态、不冒充端到端质量成功。提示明确已查证错误必须 open/revise；分析优先核对原生记录、采集范围与因果机制。真实模型针对该失败稿的反证复验进行于 `.local/rsi-review-regression-y58rugxk/`（复用冻结证据，非完整新一周采集）；16 项相关测试通过。验收脚本将 workflow_passed 与内容需人工/独立抽查明确分开。

2026-10-01：A30 反馈修复部署与回归通过。最终后端全量 614 passed、退出码 0（56.73 秒，4 条既有 SDK 提示），Ruff/diff 检查通过。通过鉴权 API 更新本机 RSI 为 14 节点，原计划不变，备份与部署证据 `.local/rsi-upgrade-proof/20261001T190628/`。冻结定义与执行脚本后启动真实验收 `.local/rsi-parallel-2eqh8sx8/`，已观测同 Run 五分支同时活动，尚未宣称发布通过。

2026-10-01：A30 反馈可达性修复。第二次真实验收 `.local/rsi-parallel-d_2eqsc3/` 确认五分支并发，但发现专项引用使用无效 shell 花括号路径，gate 正确拒绝；进一步确认 gate→fanout 的反馈文件被既有回边血缘规则截断，分支未收到明确修正要求。因此经原生 stop 正常停止，终态 stopped，未发布、不计通过。RSI 新增普通 audit-context Op 保存反馈，再进入 fanout；不改核心运行时。专项提示改用具体证据文件，门禁给出无效原路径及修正格式。16 项相关测试通过，包括全部五专项第二轮实际读取 REVISE 及具体 required_change、报告修订与最终发布；该测试不代表真实 provider 内容验收。

2026-10-01：A30 早期内容验收暴露收敛问题。`.local/rsi-parallel-dbwvj73m/` 五个专项各完成一次、产生有价值的证据缺陷发现；但综合稿大量复制实验计数与修订流水账，六轮评审不断指出计数/引用/过程自述问题，且采集器和部署提示已在此期间修复，该轮无法代表最终版本。因此人工中断自建验收进程并将其如实记为 interrupted/acceptance_superseded、not_passed，保留所有历史，不以轮数阈值或伪造 gate PASS 结束。部署提示改为决策摘要、细节引用 findings、后续只对反馈/diff重点复查，非关键过程措辞允许 limited；事实与来源错误仍阻断。用最终冻结定义重新启动 `.local/rsi-parallel-d_2eqsc3/`，该次期间不再修改执行代码/提示。显式验收脚本新增信号转为普通 Run stop 并等待结算；相关 Graph/门禁/全图反馈测试及 Ruff 通过。此条记录的是失败样本和修正方向，尚未宣称最终提示的质量或速度收益通过。

2026-10-01：A30 采集/研究/门禁修复完成验证。最终后端全量 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal -o addopts=''` 613 passed，退出码 0（56.72 秒，4 条既有 SDK 弃用提示）；Ruff、compileall、diff 和文档围栏检查通过。真实沙箱采集验证 `.local/rsi-collector-proof-p1262aza/proof.json`：源码+Plugin 共 151 份 Python 快照语法有效、部署 RSI 为 13 节点、采集 errors=[]、历史成功报告从 publish commit 读取。覆盖测试增加 camelCase/多行/属性/下标/bytes/拼接/注释凭证脱敏、历史工作区被修改/删除仍读原 commit、动态 requirements 与不支持格式说明、重复提案 ID 拒绝；独立复审未发现这些修复的剩余阻断项。真实全图首轮 review 抓到 Plugin 条数及 API 错误计数问题，已沿 gate→analyze 修订；该次 provider 内容验收仍等待最终评审，不以全量回归代替报告发布。

2026-10-01：A30 并行 RSI 接入与部署。用稳定领域分工代替单 inspect：Run、代码/架构、Graph、Plugin、research→依赖审查五个分支；join 后跨域综合，review/gate 分别支持改稿和重新专项调查。采集改为动态文件/Graph/Plugin/全历史索引与按需机械投影，新增领域/提案索引、环境来源说明、计划与调用关系；研究从声明和注册表发现包/上游，保留限流及不支持格式。门禁绑定 join 分支 commit、报告 reviewed_commit、证据路径、窗口、历史 ID 与唯一性。独立代码审查与实际 RSI 发现并修复脱敏破坏语法/凭证边界、历史内容错误绑定未提交工作区、依赖清单静默遗漏等问题；历史改为按记录 commit 读 Git，Python 改为 AST 字面量+注释处理。真实完整 RSI 已并发完成五领域并进入评审，首次评审明确要求纠正插件覆盖条数与 API 错误计数后回 analyze，未重跑专项。源码较早全量 612 项通过；最终修复继续复验。部署通过鉴权 Graph API 保存 13 节点并同步只读授权，计划未改，备份 `.local/rsi-upgrade-proof/20261001T180037/`；桌面/手机画布验证见同目录上级截图及 browser-evidence.json。尚不能把进行中的真实 Run 写成已发布或长期自进化完成。

2026-10-01：按用户要求以 RSI 作为同 Run 局部并行示范，开始 A30 第二阶段。实现目标冻结为 `collect → audit-fanout → [run-audit | code-audit | graph-audit | plugin-audit | research → dependency-audit] → audit-join → analyze → review → gate → publish`，gate 可回到 analyze。动态发现源码/变化、部署 Graph 全定义、Plugin 资源、Run 历史索引和依赖元数据；专项节点只优先读取领域索引，统一写 findings.json/md 并列出已读、未覆盖与证据不足项。综合先读分支结论，需要时追溯原始证据；提案继续保留历史 ID，不添加固定模型请求/Graph 轮数上限。采集、研究与 Graph/验收分别实现后合流。此条为开工契约，尚无新图验收证据；保持已有每周计划，发布仅指本地报告，不宣称提案已实施。

2026-10-01：A31 阶段收口。修复后的全量 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal -o addopts=''` 明确退出码 0，563 passed（55.83 秒，4 条既有 SDK 弃用提示）；前端 32 单测、20 浏览器 E2E 通过，1 条既有真实 Pilot opt-in 未运行；生产 build、Ruff、compileall、文档链接/围栏与 diff 检查通过。独立复审确认三个阻断问题及身份/历史兼容问题已修复，无剩余范围内阻断项。真实 DeepSeek 验收见 `.local/parallel-provider-alot062w/evidence.json`；真实浏览器成对创建、双节点执行、产物、非法配对拒绝及桌面/手机截图见 `.local/parallel-browser-h5M9HM/`，两类证据分别记账。确认本机 Graph/Session 无活动任务后执行 `scripts/dev.sh restart`，8077/5173 首页及鉴权 Graph API 均返回 200。未改 RSI Graph 和定时计划，未触发业务 Plugin 或对外消息；生产规模/大量分支、嵌套并行和真实 provider 杀进程恢复未在本阶段验证。

2026-10-01：用户明确授权开始并要求持续完成局部并行开发。按已有 Node/Harness 接口接入同 Run fanout/join，新增 Op schema 和一一配对/区域合法性校验；原调度线程保存活动节点与本轮完成 commit，工作线程仅执行既有节点接口；新增 Web 配对创建/编辑、运行观察、示例和显式 provider 验证脚本。独立审查复现并修复停止后的会话分支文件丢失、并行工作区前缀重叠、分支轮次限制后恢复异常；补齐摘要执行身份和跨 Graph 编辑后的历史路径兼容。provider-free 回归、真实 DeepSeek 两分支及综合、真实后端浏览器和真实子进程退出恢复均通过；证据与当前阶段全量状态见 A31。第一次全量发现两项旧串行工厂签名回归，修复为仅在需要时传取消回调，相关节点/并行回归通过，正在复跑阶段全量。不改变 RSI 部署图、计划或业务 Plugin。

2026-10-01：用户澄清并确认并行能力为同一 Graph Run 内的 `fanoutOp → 多个并行 AgentNode → joinOp`，且 fanout/join 必须一一配对。纠正先前将分支转成独立子 Run 的建议，记录到组合设计与产品架构；当前架构明确该能力未实现，新增 A31 验收项。仅完成设计方向记录，具体 schema、失败/恢复及区域拓扑规则尚未冻结，未修改代码或运行配置；文档检查见后续验证，不将已有独立调用测试计为本项通过。

2026-09-30：实现企业微信 WebSocket 通道第一步。新增 `anchor.channel` 的 `ChannelEvent` 与 SQLite `EventLedger`；新增 `plugins/wecom/ws_gateway.py`，使用官方 `wecom-aibot-python-sdk==1.0.2`，支持单常驻连接、事件规范化、重复事件跳过、失败重试和 Anchor 回调回复；增加 `channel.json`、channels 可选依赖、Plugin/使用文档和 2 项测试。验证：`pytest tests/test_channel_gateway.py tests/test_wecom_plugin.py -q`（5 passed）、compileall、Ruff 和 `git diff --check` 通过。尚未接助手 Graph Session，也未进行真实企业微信连接或 provider 端到端验收，对应 A23。

2026-09-30：企业微信 WebSocket、旧版回调桥和 MCP 独立入口启动时统一调用 Anchor 现有 `load_dotenv()`，从当前工作目录读取 `.env`，不覆盖显式 shell 环境变量；MCP API 地址改为请求时读取，使 `.env` 生效。补充 `.env.example`、Plugin/使用说明及 WebSocket 入口配置测试。尚未使用真实企业微信凭证连接；对应 A23。

2026-09-30：企业微信会话入口接线。新增 `/v1/channels/wecom/events`，按来源、成员和会话稳定绑定 Anchor Session，使用企业微信事件 ID 作为 Turn 幂等键，复用现有 Pilot Turn、Harness 持久化和恢复机制，同步返回文本；确认仍保留 Anchor 原有边界。通道、Plugin、Pilot Turn 与触发 API 相关 31 项测试通过；真实 provider 验证见本日后续记录，独立业务助手 Graph 和真实企业微信平台连接尚未验收，对应 A24。

2026-09-30：用户收敛近期范围为企业微信 WebSocket Plugin、助手 Graph 及必要基础能力，RSI 延后至积累足够数据。核查实际 Plugin、Graph/Node、Session/TurnStore、调度和鉴权代码，记录当前链路缺口及复用边界，更新 A24/A25；未修改运行代码。相关现有回归 `pytest tests/test_wecom_plugin.py tests/test_session.py tests/test_pilot_turns.py -q` 通过、退出码 0，`git diff --check` 通过；这些只证明原有应用 API/HTTP 回调和 Pilot 基础行为，不证明 WebSocket、多用户 Graph 或真实企业微信联动完成。

2026-09-30：用户确认企业微信之外还需要支持其他平台。确定近期只抽取窄的常驻通道能力：平台适配器负责协议、签名/加密和平台收发，通道层负责连接生命周期、事件去重与投递状态，Graph/Plugin 负责会话和业务；不把 MCP 工具生命周期当作长连接宿主。记录 WebSocket 的重连、单连接、重复事件、速率和回复窗口边界，以及个人微信不能默认视为官方可接入机器人平台。暂未修改运行代码或声称跨平台能力已实现。

2026-09-30：核查飞书与普通个人微信的官方公开能力。飞书开放平台有 WebSocket 长连接接收事件入口；微信公开文档的公众号/微信客服能力使用 HTTP 推送和 API，未找到普通个人微信号的官方机器人 WebSocket 接口。补充官方文档链接及主体边界说明；未修改运行代码或宣称飞书/微信适配器已实现。

2026-09-30：用户明确 Anchor 的目标为多用户并发工作中枢，助手通过 Graph 调用 Plugin，并从独立落盘的工作记录整理经验，改进 Plugin、Graph 及 Anchor 内部实现。产品架构新增需求章节，区分已确认目标、建议方案、发布授权和工程验证缺口；新增 A24/A25。仅完成需求记录，未修改运行代码、触发业务操作或宣称多用户/RSI 已实现。

2026-09-28 服务恢复记录：用户报告无法打开 5173，现场确认 5173/8077 均无进程监听；使用 `./scripts/dev.sh start` 启动现有前后端，本机首页及 Graph、时间线、Session 代理接口均返回 HTTP 200。日志未能确定此前进程退出原因；未改产品代码，未将此次访问检查计为 provider 或停机恢复验收。对应 A18。

2026-09-29：新增 `plugins/wecom` 企业微信 Plugin。stdio MCP 提供发送文本、发送 Markdown、查询成员；独立 `bridge.py` 校验回调签名、使用 AES 解密 XML，并把消息转换为 Anchor Graph Webhook 输入。新增 `tests/test_wecom_plugin.py`，本地 API 夹具、MCP 协议、回调解密和 Webhook 转发通过；未宣称真实企业微信端到端完成。对应 A23。

以下按时间保留当时的实现与决定；旧条目中的「剩余工作」不再独立生效，当前范围以上文阶段表及最新收敛决定为准。

- 2026-09-26：P0 建立；开始 P1。
- 2026-09-26：P1 完成。证据：`tests/test_pilot_turns.py`（提交幂等、并发、SSE 游标续传、真实工具事件、断线后重读、停止保留部分输出、跨会话/非法游标/归档拒绝）、`tests/test_session.py`、`apps/web/e2e/pilot.spec.ts`（真实 Vite 代理、Markdown/输入法/移动端、流式文本、工具活动、重连去重）、前端 19 个单测与 build、Ruff、compileall。下一阶段 P2；A07 的工具副作用窗口与 A09 一并收敛。
- 2026-09-26：开始 P2。第一步把 `graph_run`、`run_pause/resume/stop` 从直接执行改为经 `_approved_mutation`：先写一次性确认请求，用户授权后消费审批、把操作意图落库再执行副作用，同一意图重复调用由账本回答已记录结果，因此同一 Run 不会被启动或控制两次。`/sessions/<id>/confirm|reject` 的硬编码动作白名单删除，改为由 `grant_approval`/`reject_approval` 绑定待确认记录（白名单是重复校验，且会挡掉新的 Run 动作）。证据：`tests/test_pilot_tools.py` 新增两个用例（未确认不启动、确认后只启动一次、Run 控制同理）；全量 `pytest -q` 退出码 0、283 项通过，Ruff、compileall、前端 19 个单测、build 与 3 个浏览器 E2E 通过。P2 剩余：Pydantic `DeferredToolRequests`/`DeferredToolResults` 取代自定义审批字典（使审批真正结束模型运行）、`session_ask` 成为真实暂停边界、按 `tool_call_id` 的账本取代内容哈希、账本单槽位改为多条、存储事务与历史迁移。
- 2026-09-26：P2 第二步，换成 PydanticAI 原生 deferred 审批。7 个有副作用的工具（`graph_create/update/delete`、`graph_run`、`run_pause/resume/stop`）声明 `requires_approval=True`，Agent 输出类型加上 `DeferredToolRequests`；模型调用它们时本次运行立即结束，Anchor 把框架给出的 `tool_call_id` 与原始参数写进 Session（流式参数是 JSON 文本，落库前解析成对象供 UI 展示）。`/confirm`、`/reject` 只记录决定，UI 随后发起一次 `resume` turn 携带 `DeferredToolResults`，由框架用原参数执行或拒绝：客户端不再能用自定义消息替换动作，审批也不再是「返回一个字典让模型继续」。操作账本改按 `tool_call_id` 记账（`SessionStore.begin_operation`），重放已确认调用返回记录结果而不是再次执行；账本不再要求事先存在的审批记录。旧版单槽位 `approval` 不指向任何框架调用，读取时丢弃并回到 `active`。证据：`tests/test_pilot_tools.py`（未批准不执行、批准后执行一次、重放不再执行、拒绝可被模型读到）、`tests/test_pilot_turns.py` 新增端到端用例（turn 停在 `waiting_approval`、等待期间拒绝新消息、确认后 `resume` 完成并关联 Run）、`apps/web/e2e/pilot.spec.ts` 新增审批流程用例（横幅、输入禁用、确认后只发一次 resume）。全量 `pytest -q` 退出码 0、285 项通过；Ruff、compileall、前端 19 个单测、build 与 4 个浏览器 E2E 通过。P2 剩余：`session_ask` 成为真实暂停边界、账本单槽位改为多条、资源前态与状态事务收敛、跨存储历史迁移。
- 2026-09-26：P2 第三步，`session_ask` 成为真正暂停边界。工具体改为抛出 `CallDeferred`（框架的外部执行分支），模型调用 `session_ask` 时本次运行立即结束，提问写入 Session 的 `waiting_reason` 与新的 `questions` 列表，turn 停在 `waiting_user`。用户下一条消息不再作为新的用户发言，而是用 `DeferredToolResults(calls={tool_call_id: 回答})` 作为该调用的返回值恢复运行，所以模型看到的是对提问的答复，同一轮里后面的工具不会先执行。`SessionStore.set_pending_approvals` 收敛为同时接收审批与提问的 `set_pending`，`clear_approvals` 收敛为 `clear_pending`。证据：`tests/test_pilot_turns.py` 新增端到端用例（提问后运行即结束、会话停在 `waiting_user` 且原因就是问题、回答以 `ToolReturnPart` 回到模型、历史里没有多出一条用户消息）。全量 `pytest -q` 退出码 0、286 项通过；Ruff、compileall、前端 19 个单测、build 与 4 个浏览器 E2E 通过。P2 剩余：操作账本仍是单槽位（多个互不相同的已执行操作会互相覆盖）、资源前态比较与跨存储状态事务、系统 Pilot Graph。
- 2026-09-26：P2 第四步，操作账本从单槽位改为按 `tool_call_id` 记账。`Session.operations` 是字典，`begin_operation(session_id, action, call_id)` / `finish_operation(session_id, call_id, result)` 不再用内容哈希匹配，也不再需要两个参数传同一个值；一次暂停里批准多个调用时各自保留结果，重放或崩溃重试读自己那条记录。旧版单槽位 `operation` 在读取时迁入 `operations`（它的内容哈希不可能再次生成，只留作诊断）。证据：`tests/test_session.py` 用两个不同的调用分别 `started → completed`，确认第二个不会覆盖第一个，且 `finish_operation` 对不存在的调用报错；`tests/test_pilot_tools.py` 新增用例让模型一次提出两个 `graph_run`，批准后人两个 Run 都启动，且两条账本记录各自可读。全量 `pytest -q` 退出码 0、287 项通过；Ruff、compileall、前端 19 个单测、build 与 4 个浏览器 E2E 通过。P2 剩余：资源前态比较（确认后目标已变化时要拒绝）、未知结果的明确处置入口、跨存储状态事务、系统 Pilot Graph。
- 2026-09-26：P2 第五步，补回资源前态比较。迁移到 deferred 审批后工具体只在用户批准之后才运行，版本快照也就只能在批准之后取，等于失去了「确认期间资源被改过」的保护，这一步把它补回来：运行暂停时由 `anchor.pilot.approval_precondition` 记录目标 Graph 的 sha256 与是否存在，写进待确认记录；用户批准后执行前用 `_stale` 重新比较，不一致就拒绝并提示重新确认。前态检查、副作用与账本写入放在同一把进程锁里（`_SIDE_EFFECTS`），因为框架会把同一次暂停里的多个 deferred 调用并发解析——两条针对同一 Graph 的已批准修改里，第二条会看到第一条的结果并被拒绝，而不是互相覆盖；这也是 `_recorded(..., verify=...)` 存在的理由。证据：`tests/test_pilot_turns.py` 新增用例（暂停后在画布改掉 graph.json，再确认并 resume，文件保持用户版本且模型收到「不要覆盖」的结果）；`tests/test_pilot_tools.py` 新增用例（一次提出两个 `graph_update`，批准后只落到一次写，账本只有一条记录）与两条调用各自记账的用例。全量 `pytest -q` 退出码 0、289 项通过；Ruff、compileall、前端 19 个单测、build 与 4 个浏览器 E2E 通过。P2 剩余：跨存储状态事务、未知结果的明确处置入口、系统 Pilot Graph。
- 2026-09-26：接手后先补 P2 真实 provider 验证。新增显式运行的 `scripts/verify_pilot_provider.py`，使用现有 runtime/secret 配置与独立数据目录，不替换模型或工具。首次 DeepSeek 流在事件写入期间触发 SSE 读取 `database is locked`；turn 库改用 SQLite WAL，并新增写事务持锁时仍能读取已提交事件的回归。修复后 `deepseek-flash` 的 10 个 turn 全部通过：创建前不落图、批准后用原调用创建；启动前无 Run、批准后唯一 Run 在真实沙箱生成 `result.txt`；拒绝删除保留原图；确认期间经 PUT 改图后拒绝旧提案；`session_ask` 真暂停，回答以原调用的工具结果持久化；每次提交重试保持同一 turn。证据目录 `.local/pilot-provider-yu3iwzgt/`（Session、Harness、步骤、turn/SSE 和 Run 记录），Run `20260926T104613`。相关子集、全量 `pytest -q -n 8 --dist worksteal`（292 项，退出码 0）、Ruff、compileall、前端 19 单测、8 浏览器 E2E 与 build 均通过。此次真实 provider 验收走 HTTP/SSE；浏览器审批路径仍是 mock SSE，不据此宣称真实浏览器全链路通过。P2 仍进行中，下一步补未知结果的明确处置入口及跨存储状态协调；系统 Pilot 属 P4。
- 2026-09-26：按用户决定收敛文档，撤销复杂审批扩建、跨存储事务和未知结果人工处置目标。P2 改为「逐步保存工作 JSONL → 加载中断历史继续同一会话 → Agent 核查现场后续做」，再收敛现有逐工具审批与阻断续聊的门禁；不先搭建替代审批系统。同步更新产品架构、当前架构、使用指南和 AGENTS.md，历史体验核查标为 P0 快照，旧推进记录保留但不作为当前待办。A07–A09 按新目标重列为部分通过/未验收，旧真实 provider 审批证据不转算新目标通过；P5 用目标文件与对话确认，不另建合同审批状态机。此次只修改文档，未改变运行行为；完成文档交叉引用、目标/现状一致性及 diff 检查，未重跑代码测试。下一步按 P2 最小实施顺序接通记录与续聊。
- 2026-09-26：用户进一步明确「先找 PydanticAI / Harness 接口，再接入」。核对本地固定版本源码和随包说明，确认 StepPersistence、FileStepStore、continue_run、inspect_recovery 及 Agent.run(message_history=...) 已覆盖记录/读取/续聊基础；普通 AgentNode 已有相关调用。用临时 FileStepStore 和 FunctionModel 跑通工具结果持久化、后续模型失败、新存储实例读回历史并带新输入继续（退出码 0），同时确认 agent_name 会使 persistence run ID 不同于 turn ID。P3 独立记忆/计划工作包撤销，P5 研究业务移出核心验收，P4/P6 不自动排入本轮；A07 标明仅框架接口小样例通过，A09 真实中断仍未验收。同步架构、使用指南及 AGENTS.md；本次未改运行代码，未把接口验证写成 Anchor 集成完成。
- 2026-09-26：按用户要求再次整理范围。P2 的持久记录、续聊、必要提问、按需框架压缩、对象跳转和真实中断验收列为当前目标；P3–P6 改为只保留产品需求记录，不再给出具体实现指导、依赖或当前验收前置条件。同步 A10–A14 的范围与未验收状态，删除产品架构中未确定的系统 Graph 注册、生命周期迁移和研究合同文件方案；历史核查不再提供开发顺序。自研框架能力须先说明缺口和替代方案、取得用户同意。本次仅修改五份文档，未改变运行行为；本地 Markdown 链接、代码围栏及 `git diff --check` 通过，未重跑代码测试。
- 2026-09-26：P2 续聊实现。**先核对框架，再接线**，四个结论决定了实现方式：(1) Pilot 的 `StepPersistence` 后端从 `SqliteStepStore` 换成原生 `FileStepStore`（`state/pilot-steps/`：`run.json`、`events.jsonl`、`tool_effects.jsonl`、`snapshots/*.json`、`media/*`）；(2) 只开持久化不够——工具执行中被 `kill -9` 只留下 `events.jsonl` 与 `tool_effects.jsonl`，没有快照，`continue_run` 直接 `LookupError`；打开 `capture_frontier=True` 后才有可供下一个进程读取的 `snapshots/*.json`；(3) `continue_run(include_interrupted=True)` 读回的中断历史末尾是未完成的 tool call，直接叠新 prompt 会被框架以 `UserError: Cannot provide a new user prompt when the message history contains unprocessed tool calls` 拒绝（框架宁报错不静默重放，这点是对的），因此只在 `is_provider_valid(...)` 为假时把末尾响应标成框架自己的 `state='interrupted'`，其余交回框架：它合成 `outcome='interrupted'` 的 tool-return，模型因此看到「调用过、结果未知」；(4) `agent_name` 让持久化 run ID 变成 `5:pilot<turn_id>` 的 base64，不等于 turn ID，所以续聊按 `conversation_id` 查而不是拼 ID。接线：`pilot.attempt_history` 只在框架记录比已存对话更长时接手（进程被杀或用户停止的回合不会走到保存），`respond` 在带新输入时把它接在 message_history 前面；`Scheduler.create_turn` / `pilot_message` 允许 interrupted 会话接新消息；`pilot_turns.unsafe_to_retry` 与两条「副作用门禁」删除。审批收敛：`graph_run`、`run_pause/resume/stop`、`graph_create`、`graph_update` 取消逐次审批（用户请求即授权），只有 `graph_delete` 保留框架 deferred 确认；`session_ask` 不变。压缩：接入框架 `SlidingWindowCompaction`（可配 `SummarizingCompaction` + summarizer 模型），默认按 200 条消息 / 上下文 60% 触发，配置键 `pilot_compaction`。对象跳转：`#anchor/graph|run|artifact/...` 引用由 `apps/web/src/links.ts` 解析，点击进入已有页面，右上角「返回会话」回到原 Session（`Pilot` 的会话选择改为受 App 控制）。证据：`tests/test_pilot_turns.py` 真实子进程 SIGKILL 后重启续聊两例（工具结果已保存读到 `recorded-result`；结果缺失得到 `outcome='interrupted'`）与文件记录用例、`tests/test_pilot_tools.py` 重写（普通操作直接执行并记账、删除仍需确认、未知结果不重放）、压缩用例、`apps/web/src/links.test.ts`、e2e `pilot.spec.ts` 引用跳转用例。全量 `pytest -q -n 8 --dist worksteal` 297 项退出码 0；Ruff、compileall、前端 23 单测、9 个 e2e、build 通过。真实验收：`scripts/verify_pilot_provider.py`（直连 DeepSeek：直接建图/启动 Run、链接、删除确认与拒绝、过期确认、`session_ask`、提交去重、文件记录，证据 `.local/pilot-provider-qxmyajmk/`）、`scripts/verify_pilot_resume.py`（真实 SIGKILL 两次 + 重启续聊，证据 `.local/pilot-resume-u3pvap_a/`）、浏览器 `apps/web/e2e/real-provider.spec.ts`（真实 provider + 杀进程 + 刷新 + 续聊 + 链接与返回，证据 `.local/pilot-browser-acceptance/`）。未验证：真实长会话的压缩触发、多进程/多租户、系统 Pilot。
- 2026-09-26：P2 独立验收暂不通过。全量后端独立复跑 300 passed in 99.78s、退出码 0（`.local/pilot-p2-review-pytest.xml`）；前端 23 单测、9 E2E（1 条 opt-in 真实浏览器测试跳过）、build、Ruff、compileall 均通过。真实 DeepSeek HTTP/SSE 复跑通过（`.local/pilot-provider-hntv2f1d/`，Run `20260926T140327`），真实 SIGKILL 两例复跑通过（`.local/pilot-resume-ga78jp5d/`，准备 Run `20260926T135924`）。额外诊断复现 R1 空 prompt 续答漏读中断记录、R2 压缩后按消息长度错误舍弃新历史、R3 聊天链接丢失未保存编辑、R4 Artifact 链接未定位文件、R5 模型窗口未传给框架压缩；前三者影响恢复或编辑保留。详见 `docs/pilot-p2-acceptance-review.md`，同步调整 P2、A07/A09/A10/A12 状态。仅记录验收、未修改产品代码，修复继续交开发 Agent。
- 2026-09-26：修复独立验收报告 R1–R5。续聊同时读取新消息和空 prompt 的原生快照，使用快照时间而非消息长度选择压缩后记录；中断工具调用交给 PydanticAI 生成 interrupted 返回；压缩能力传递 profile.context_window；聊天对象跳转复用未保存编辑确认，Artifact 引用进入文件页并展开目标路径。相关后端回归 34 项通过，前端 23 项单测、9 条 mock E2E（另 1 条 opt-in 跳过）、build、compileall、Ruff 和 diff 检查通过；补充 E2E Artifact 内容断言。真实 provider 验证 `scripts/verify_pilot_provider.py` 通过，证据 `.local/pilot-provider-e8bo319e/`；该脚本覆盖实际对话、Graph Run、对象链接、必要提问、删除确认/拒绝与资源变更保护。空 prompt 真实续聊、真实长压缩及真实 provider 浏览器复验仍待运行，未提前宣称通过。
- 2026-09-27：确认定时计划的停机语义：Anchor 不补跑停机期间错过的时点，避免过期输入在错误时间产生副作用；一次性计划显示为错过并结束，周期计划显示错过的发生并保留后续计划。A15 记录该产品契约，调度实现留待事件触发阶段。
- 2026-09-27：收敛事件触发与输入契约。Graph 忙时手动、定时和 Webhook 触发均直接拒绝，不排队、不合并、不创建 Run、不持久化拒绝记录；周期定时覆盖间隔型和日历型规则，按本机时间。Graph 默认 `input` 与 Run 输入采用递归对象合并、非对象值整体替换；AgentNode 获取有效 JSON 输入，OpNode 获取只读 JSON 输入，Run 保存最终输入。同步更新 A15；未修改代码。
- 2026-09-27：补充确认忙碌时点的看板语义与 Webhook 重试语义：计划时点因 Graph 正在运行而未触发时，看板显示原因但不创建 Run/拒绝记录；Webhook 不按事件内容去重，空闲时到达的每个请求均为新触发。产品契约已足以进入实现拆分，低层接口与持久化方式留在实现阶段决定。
- 2026-09-27：确认 Webhook 与 Responses API 均参考 OpenAI 的 Bearer API key 方式，Anchor 使用 `anchor-key` 白名单，密钥匹配后允许请求执行；不引入角色审批机制。只更新产品需求，尚未实现。
- 2026-09-27：补齐 HTTP 目标契约：所有 API 统一使用启动时读取的 `ANCHOR_API_KEYS` Bearer 白名单；非 loopback 监听必须配置 key。冻结 `POST /v1/webhooks/graphs/<graph-id>` 的 JSON 输入和 202/400/401/404/409 语义；Responses 采用固定 `anchor-copilot` 常用子集、文本输入、同 key 的 `previous_response_id` 续聊及 JSON/SSE 返回。外部工具、图像/音频和完整多租户隔离不纳入此契约。仅更新产品文档，未实现；Responses 官方文档页面在本环境被访问拦截，因此未将未核实的字段细节写成兼容承诺。
- 2026-09-27：用户明确开始推进事件触发实现。接入 Graph 默认输入与 Run 覆盖输入，OpNode 通过只读 `ANCHOR_INPUT` JSON 环境变量读取；增加 Webhook 与统一 Bearer 白名单、按 key 绑定的 Responses 文本 JSON/SSE 子集；加入本机时间一次性/间隔/日历定时、错过不补跑、Graph 忙时跳过及运行时间线看板（未来 7 天、历史分页、跨日时长）。相关后端子集、前端 23 单测和 build 通过。Responses 目前仅通过伪执行器 HTTP 测试，真实 provider 验证、真实停机/重启与浏览器 E2E 仍待完成；不得视为 E1 全面验收通过。
- 2026-09-27：继续完成 E1 自动验收收敛。修正流式浏览器夹具按实际 `after` 游标续传（开发模式重复挂载不再错误跳过首批事件）；时间线点击 Run 进入原运行详情并可返回，避免看板与运行画布争抢布局；修正时间线对已结束 Run 覆盖计划时点的忙碌识别。新增相应回归。全量后端 pytest（`-n 8 --dist worksteal`）、Ruff、compileall、前端 23 单测/build 通过；浏览器 E2E 最终 9 条通过、1 条真实 provider opt-in 跳过。真实 provider 与实际停机/恢复仍未验收。
- 2026-09-27：现场启动旧版服务后发现前端 Vite 代理遗漏 `/timeline` 与 `/schedules`，导致浏览器请求在代理层 404，页面显示“服务未连接”。补齐代理并重启当前服务；直接后端和浏览器实际请求均验证成功，时间线显示历史运行与未来日期。该问题属于部署进程/代理未随代码更新，不改变 E1 契约。
- 2026-09-27：重构运行看板前端。页面改为“运行摘要 → 筛选与日期导航 → 每日 24 小时时间线 → 图例”的单列层级；实际 Run 用持续时间横条，计划用时间点，当前时刻、错过原因和空白日期折叠均可见。点击时间线条先打开运行/计划预览，再进入既有节点与产物详情；定时计划移入 Modal，桌面与移动端均验证无页面横向溢出。为适配预览层，更新三条浏览器测试路径。`npm --prefix apps/web run test:e2e` 最终 9 条通过、1 条真实 provider opt-in 跳过；截图验收覆盖 1440×1000 与 390×844，控制台无错误。未改变后端契约；真实 provider 与停机/重启仍待 E1 完整验收。
- 2026-09-27：继续检查并修复看板现场问题。原 `.timeline-entry` 为了扩大点击区域继承了旧版运行条背景，导致短运行被误绘制为覆盖整天；现在点击层与时长条分离，真实运行条只显示 `started` 到 `updated` 的时长。用 10 个并行 Graph 的浏览器夹具验证同日多泳道可读；9/23–24 真实数据显示实际持续时间而非 24 小时。看板与 Pilot 顶部栏移除无关的 Graph 选择器和运行按钮，Graph 图编排与运行详情仍保留自己的上下文操作。相关 E2E、前端单测/build 通过；A15 仍待真实 provider 与停机恢复验收。
- 2026-09-27：再次做整体体验复核并收敛时间导航。历史页不再重复显示未来 7 天，日期范围标题与实际可见范围一致；增加“最近活动”定位入口，避免默认定位今天时让历史运行看起来消失；运行看板隐藏无关的详情面板最大化按钮，保持全局页面语义。桌面/移动截图、前端单测 23 项、E2E 9 条（1 条真实 provider opt-in 跳过）和 build 通过。
- 2026-09-27：修复 Graph 层执行详情不可达。图编排页新增 Graph 运行概览，直接显示当前/最近状态、运行次数、最近时间和最近 4 次运行；点击“查看当前运行”“查看最近运行”或任意历史条目直接进入已有执行画布与节点/产物详情，未运行的 Graph 显示空状态并保留看板入口。复用现有 `/runs` 与 `/runs/<run>`，未新增后端契约；前端单测 23 项、工作台 E2E 2 条、build 和 `git diff --check` 通过。
- 2026-09-27：统一页面上下文层级。顶栏现在只保留 Anchor、图编排、运行看板、Pilot 和服务状态；Graph 选择器与“运行工作流”移入图编排/运行详情的页面头部，Pilot 与全局看板不再出现无关控件。工作台 E2E 2 条、前端 build 通过，截图确认 Graph 页面控件仍然可见且层级清晰。
- 2026-09-28：重新设计图编排运行概览。删除独立大卡片，将当前/最近状态、开始时间和详情入口融入 Graph 页头；历史按钮固定在状态栏右侧，复用浏览器原生 Popover 展示此图全部运行记录，打开不改变按钮或画布位置，支持 Esc 与外部点击关闭；手机画布保留可用高度。实际服务桌面/手机截图复核通过，无页面横向溢出或脚本错误；前端 23 单测与全套 E2E 9 条通过（真实 provider opt-in 1 条跳过），浮层调整后工作台 2 条复验通过。未修改后端、未进行真实 provider 验收；对应 A17。
- 2026-09-28：修复 Pilot 对话生命周期问题。发送先显示用户消息，再请求建会话/提交；取消会吞掉乐观消息的提交后立即刷新，并用已接收 turn 的 prompt 补齐尚未落盘的历史。会话导航不再受当前执行的 busy 锁定；旧历史请求用 AbortController 取消，异步结果按当前会话/导航检查。SSE 离开页面或 StrictMode 清理时实际关闭连接，异常 EOF 延迟重连，普通 API 有 30 秒超时；切换不会停止服务端任务。保存顶层页面选择，刷新恢复原 Pilot 会话；历史读取失败保留最终流式回复，并允许重新加载。复用已有 request_id 处理不确定提交，明确拒绝后可修改重发。对应 A02–A05。
- 2026-09-28：核查慢回复现场。只读检查会话 `aa756e5b-812e-40b0-a8d1-f55b91db2d65` 的事件：turn `10c6cdda-7afa-4059-af09-30042dde1959` 从 03:08:57 UTC 到 03:19:51 UTC，最终等待用户；包含大量模型思考、Graph 参数生成及校验返工。前端原先忽略思考事件、把工具参数开始生成误标为执行中；现在区分阶段，显示已用时间及长时间无进展。没有改模型配置、提示词或中断用户任务；本次 UI 修复不代表模型生成耗时已降低。
- 2026-09-28：Pilot 修复验收：前端 23 单测、7 条 Pilot 专项 E2E、全套 E2E 11 条通过（真实 provider opt-in 1 条跳过）及 build 通过；后端全量 `pytest -q -n 8 --dist worksteal` 313 项通过、退出码 0，`git diff --check` 通过。新增长连接测试使用真实浏览器、Vite 代理和持续 HTTP/SSE，模型输出及持久化延迟为合成；同时只读打开真实 5173 会话并刷新，恢复原会话与 10 条消息，桌面/手机截图无脚本错误、手机无横向溢出。未发新的 Pilot provider 请求，未将此次测试计为真实生成速度或停机续聊验收。
- 2026-09-28：用户指出最近输入仍未显示，复核确认上条验收遗漏真实内容完整性：那 10 条消息仅含首条用户消息。最新 turn 已收到「PPT 只是例子，能否构建 plugin」的澄清，框架将其保存为 `session_ask` 的 ToolReturnPart，`_entries` 只投影 UserPromptPart，导致完成/重开后漏显；原测试甚至将遗漏写成预期。现只调整展示投影，成功的提问工具返回显示为用户消息，提问调用里的问题显示为助手消息；不修改原生存储或给模型重复追加输入。修正 HTTP/框架测试，验证完整问答顺序及重开读取。确认无活动 turn/Run 后重启服务，真实 5173 会话刷新后显示两条用户输入，丢失的那条恰好出现一次，截图与控制台复核通过。同步澄清「生成 Graph 参数」实际是模型自行展开完整图定义用于 graph_validate，并非用户要求；收紧 Pilot 指令，讨论/能力咨询先回答问题，不擅自生成整图、不猜 Plugin ID，已授权实施仍直接执行。提示词调整未发新 provider 请求验证，不宣称已解决模型慢回复。对应 A08。
- 2026-09-28：问答历史修复验收收尾：相关后端 34 项、后端全量 313 项通过（退出码 0），Pilot 浏览器专项 7 条通过，Ruff、compileall 与 diff 检查通过；服务 8077/5173 均在线。真实会话恢复截图 `/tmp/anchor-pilot-missing-answer-restored.png`；本轮没有修改或重发用户输入。
- 2026-09-28：按用户要求生成 [社区 Plugin 生态兼容独立任务背景](plugin-ecosystem-task-background.md)，供新的 Agent 理解原生兼容 OpenAI/Codex 与 Anthropic/Claude 插件的产品意图、工程起点与讨论脉络。本文不冻结技术方案、实施顺序或验收条件；此前少量 Skill 包装建议不作为任务限制。对应 A19，仅交接背景，未改运行代码、未开展兼容验收。
- 2026-09-28：用户澄清安装产物不保留 `.codex-plugin`，而由安装器把来源清单放在 bundle 根目录 `plugin.json`，其他资源原样保留，运行时不记录来源类型字段。实现已调整为只读根清单；随仓库 Plugin 和测试夹具使用该布局。Skill 渐进披露、只读挂载、摘要核验和 UI 预览已接入；MCP 配置由 PydanticAI MCPToolset 接入 AgentNode，资源摘要包含 MCP 声明。相关后端、前端单测和构建通过。安装入口、hooks、commands、agents 和真实 MCP 端到端未完成；MCP stdio 子进程尚未证明受 Bubblewrap 隔离。对应 A20，不代表完整宿主兼容。
- 2026-09-28：排查 `resource must be a file ...: instructions.md` 后确认这是请求旧插件布局文件名导致的路径拒绝；academic-research 已迁移至 `skills/academic-research/SKILL.md`，当前 UI 使用详情 API 聚合 Skill 内容。对旧资源名增加明确迁移提示并验证新 Skill 路径。对应 A20。
- 2026-09-28：继续完成 A20 接线：修复 Bubblewrap 子进程空环境下缺少 PATH，stdio MCP 真实 sandbox 测试覆盖工具发现/调用；安装 API 和前端 GitHub 子目录安装入口可用；HTTP/SSE token 环境变量不进入 Plugin 摘要，OAuth 声明暴露显式授权按钮；更新 Plugin 使用说明。Ruff、compileall、Plugin 与 Node 控制流相关测试通过（55 项）。OAuth provider 授权及真实社区 MCP provider 仍未验证，不计作完整 MCP 生态端到端通过。
- 2026-09-28：新增 Docmost MCP Plugin 定义，端点为 `https://docmost.cwise.dev/mcp`，使用运行服务环境中的 `DOCMOST_API_KEY` 构造 Bearer Authorization；Skill 要求仅按用户目标搜索/读取并引用页面。Pilot system instructions 指向共享 Plugin 库管理及 Graph 挂载流程。新增解析回归验证密钥不进入 catalog 记录；未设置或读取真实 API key，未宣称远端握手通过。对应 A20。
- 2026-09-28：按用户提供的 `.env` 完成 Docmost Plugin 设计并进行真实只读 MCP 工具发现；Docmost 返回 20 个工具（搜索、页面/空间读取及评论/编辑类工具）。Skill 明确默认只搜索和读取，禁止无请求的遍历、修改和删除；API key 未输出或写入文件。真实工具调用和 Graph/Pilot 端到端挂载尚未验收。
- 2026-09-28：用户明确授权 AgentNode 使用 Docmost MCP 暴露的全部能力。更新 Docmost Skill，允许搜索、读取、创建、修改、移动、评论和删除；仅保留按用户目标执行、不可逆操作确认、权限边界和密钥保密约束。真实工具发现仍已通过，具体写操作未执行。
- 2026-09-28：用户确定社区 Plugin 首期范围为 Skills/资源与 MCP；Codex hooks、commands、agents 暂不支持。记录 MCP 当前实际接线（PydanticAI stdio/URL MCPToolset、AgentNode 作用域 AsyncExitStack）及未验收边界：stdio 沙箱、鉴权/秘密/OAuth、取消生命周期和真实插件端到端。对应 A20；解析配置不计为兼容通过。

- 2026-09-28：按用户要求新增 weekly-work-report Graph（理解取证→写作配图→编辑核验→文件组装），使用现有周定时规则。Codex JSONL 与 DSH 最新代际 JSONL/zstd 只读投影，按事件时间过滤七天窗口、保留原始行号；概览与详细证据分开，工具截断显式标记。library/local-inputs.json 是本机操作员授权，不从可安装 Plugin 清单授予宿主目录访问；仅该 Plugin 挂载两个 sessions 目录。已安装 Graph/计划；首个执行被现有 .env 上下文窗口千分位格式阻断，已规范为整数后复跑，未据此宣称端到端完成。对应 A21。

- 2026-09-28：A21 真实执行暴露 DeepSeek 思考模式拒绝强制 tool_choice；在共享 model_for 复用 PydanticAI 的 DeepSeekProvider 原生兼容 profile（保留网关 deepseek-flash wire name），覆盖 Pilot/Graph 共同入口并增加回归。另验证旧下载 MIME 导致 SVG 在 Chromium 中 naturalWidth=0，改用图片 MIME、attachment、隔离 CSP 和 nosniff 后 naturalWidth=300；其余文件继续二进制下载。只读沙箱实测历史可读、写入遭拒、认证文件不可见。相关回归通过，真实报告 Run 20260928T122930 仍在执行，未记为完成。

- 2026-09-28：用户质疑为单一周报创建 Plugin。撤回该包装，移除本任务的 Plugin 清单与安装注册（保留既有失败 Run 证据）；采集改为普通 Op，脚本归 scripts/weekly_work_report，只读授权归 Graph 工作区 local-inputs.json 并限定到 collect 节点。已强化写作要求：正文不列会话/事件/测试数与提交哈希，以最新可靠记录更新现状。首轮 Run 20260928T122930 在编辑阶段遭模型网关内容过滤拒绝，保留原始材料和错误，不将草稿标为已完成；对应 A21。

- 2026-09-28：A21 去除 Plugin 后验证完成：五节点普通 Graph，无 plugins 引用；真实 sessions 采集、只读挂载、节点输入快照、最终组装通过（`.local/weekly-report-pipeline-proof/runs/pipeline-proof/`，三个语言节点用脚本模型，不能充当报告质量或真实 provider 证据）。`pytest -q -n 8 --dist worksteal -o addopts=''` 329 passed in 36.81s、退出码 0，Ruff 通过。服务已加载新实现，计划仍 enabled，下次 2026-10-01 09:00，Plugin catalog 无 weekly-work-report。真实首轮编辑失败未消除，报告不标为完成。

- 2026-09-28：用户确认加入高质量独立反馈。将原直接编辑节点替换为独立 reviewer，普通 gate Op 校验四项质量判断、逐问题解决依据、历史问题不遗漏和当前稿件 commit 后复用 anchor-route 分流；可退回 write/understand，无法解决时 blocked 失败保留反馈，只有 publish 组装正式稿件。保持冻结窗口，不重采集、不新增 Plugin、不改运行时。相关 12 项测试通过，覆盖两次不同退回后的最新材料、未解决停止、假通过/旧稿/遗漏旧问题拒绝。对应 A21，真实 reviewer 与全量回归进行中。

- 2026-09-28：周报 Graph 在 WebUI 中运行时不再弹出通用 JSON 输入框；该 Graph 的数据源和窗口由采集节点及默认输入确定，点击“运行”直接提交空/默认对象。其他 Graph 保持可编辑 JSON 运行输入。前端 build 通过。

- 2026-09-28：用户指出完整工作记录不应因证据边界而整体停止交付。移除 weekly-work-report 的 blocked 节点和决策：证据不足、记录冲突或无法视觉核验改为在正文/review.md 中限定结论后继续 publish；只有可通过补证或改写解决的事实、结构、推理和表达问题退回 understand/write。相关回归 11 项通过。

- 2026-09-28：按用户反馈复核运行看板视觉。移除重复的旧版时间线 CSS，保留单一规则；看板改为全屏白色工作区，摘要收敛为状态栏，时间导航/筛选/时间线共享明确层级，压缩日期行并修正运行点击层与实际时长条的定位。桌面与 390px 移动截图确认无页面横向溢出；前端 23 项单测、工作台 E2E 2 条、build 和 `git diff --check` 通过。未改变后端契约，A15 仍待真实 provider 与停机恢复验收。

- 2026-09-28：按用户明确的受众视角重构周报 Graph。理解节点先重建项目目标与阶段、选择两三项实质变化，写作按重要性组织并将来源移出正文，图示解释状态与关系而非事件时间线；独立评审先复述读后重点，再核查关键事实，准确但不可读的流水账必须退回。沿用现有反馈回路和门禁，不新增节点、Plugin 或运行时机制。旧真实 Run 20260928T132435 的流程成功不能作为内容质量通过依据。对应 A21；新版真实验证待执行。

- 2026-09-28：继续收敛运行看板视觉与交互。清理旧版时间线样式覆盖，统一运行/计划的时间轴定位与标签展示；看板在桌面、平板、390px 和 320px 视口使用独立滚动面板，避免页面级横向溢出；计划点与小时刻度对齐，短运行条按真实时长绘制。新增 `apps/web/e2e/timeline.spec.ts` 覆盖计划/运行预览、筛选、四种视口、弹窗和布局边界。前端单测 23 项、E2E 12 条通过（真实 provider opt-in 1 条跳过）、build、diff 检查通过；未改变后端契约，A15 仍待真实 provider 与停机恢复验收。

- 2026-09-28：按用户反馈继续压缩并行运行的视觉密度。时间线默认隐藏 Graph 文本，运行条和计划点按 Graph 名称稳定分配颜色；悬停或键盘聚焦显示 Graph、状态、时间和耗时，点击仍打开详情。保留 aria-label 与原生 title，避免颜色成为唯一信息。时间线专项 E2E 增加默认隐藏、悬停显示和多 Graph 颜色断言；前端单测 23 项、E2E 12 条通过（真实 provider opt-in 1 条跳过）、build、diff 检查通过。未改变后端契约。

- 2026-09-28：修正时间线并行泳道与悬停命中。条目按实际时间区间贪心复用泳道，只有时间重叠才增加行高；每个 Run/计划按钮只覆盖自己的时间区间，悬停、置顶和点击不会再被同泳道的前后条目互相遮挡。新增重叠 Run 的双向悬停层级断言；前端单测 23 项、完整 E2E 12 条通过（真实 provider opt-in 1 条跳过）、build、diff 检查通过。未改变后端契约。

- 2026-09-28：修复运行看板悬停同时出现两个浮窗：运行条和计划点此前同时设置原生 title 与自定义标签，现移除共用按钮的 title，保留自定义详情、aria-label 与键盘聚焦展示。时间线 Playwright 专项 1 条通过；对应 A15，仅前端提示修复，不计真实 provider 验收。

- 2026-09-28：用户指出上次移除 title 后未覆盖边缘提示裁切，补齐修复与验收。详情移出滚动泳道，复用已有原生 Popover，按实际尺寸限制在视口内并选择上下位置；长 Graph 名称完整换行，保持单一提示、aria-describedby、键盘聚焦、鼠标移入阅读与 Esc 关闭，滚动/窗口缩放/点击详情时收起。新增浏览器回归覆盖 00:01、23:45 Run 和 23:59 计划点，在 1440/390/320px 下检查浮层与全文边界及交互；截图人工复核通过。工作台旧断言改按无障碍名称定位运行按钮。前端单测 23 项、完整 E2E 13 条通过（真实 provider opt-in 1 条跳过）、build 通过。对应 A15，本次仅验证前端，不计真实 provider/停机恢复验收。

- 2026-09-28：修复 Run 悬停范围、底色与色条长度不一致。移除按钮独立的 1.2% 最小命中宽度及 48px 透明高度，覆盖全局按钮最小高度和 hover 背景；按钮与色条共用一个尺寸（极短 Run 保留 5px 可见下限），计划按钮与 10px 圆点居中对齐。悬停仅加深标记本身，不画额外矩形或外扩光晕。新增实际鼠标坐标回归，逐项验证标记四周空白不出现提示、不打开详情，相邻短 Run 前后切换定位正确；四种视口检查按钮与标记边界完全相等，原边缘提示回归保持通过。前端 23 项单测、完整 E2E 13 条通过（真实 provider opt-in 1 条跳过）、build 与 diff 检查通过。对应 A15，仅前端交互验证。

- 2026-09-28：用户指出未来计划圆点被旧图例误读为错过。只读核查当前服务：weekly-work-report 的 2026-10-01 09:00 记录为 planned，浏览器提示为“已计划”，并非调度状态错误。修正颜色与图例冲突：保留 Graph 稳定颜色，图例使用中性样式明确“颜色区分 Graph”；实际运行用条、未来计划用空心圆、错过用带斜杠圆，不再把 Graph 的红/橙色当作状态。时间线 E2E 2 条通过，覆盖同 Graph 计划与错过同色但形状不同及状态文本；移动截图、build、diff 检查通过。对应 A15，未触发实际计划或开展新 provider 验收。

- 2026-09-28：按用户要求移除看板图例中的“颜色区分 Graph”说明，仅保留三个标记图例。同步更新现有断言，时间线 E2E 2 条通过。对应 A15，纯文案调整。

- 2026-09-28：按用户要求为每条 Pilot 历史 Session 添加“…”操作菜单，复用原生 Popover 与现有 Modal，提供改名和确认删除。新增仅接受 title 的 PUT 接口，名称不能为空、最长 120 字符；手动名称不会被首条消息覆盖。删除不再要求用户先归档，执行中仍由服务端拒绝，活动检查与删除共享调度锁。前端删除当前会话回到新对话并清理该会话草稿/待重试缓存，删除其他会话保留当前输入；防止在途历史响应恢复旧列表。关联 Graph/Run/产物和底层步骤文件保持原有保留语义。后端全量 336 passed、退出码 0；前端单测 23 项、完整 E2E 13 条通过（真实 provider opt-in 1 条跳过），最终会话专项复验通过；Ruff/compileall、build 与 diff 检查通过。确认无运行中的 Graph/Pilot 后重启开发服务，真实 5173 菜单与改名弹窗只读复核通过，没有修改或删除用户现有会话。对应 A22，本次未调用真实 provider。

- 2026-09-29：按用户要求升级 weekly-work-report Graph。保留本地评审与离线产物组装，在 publish 后新增挂载 `docmost` Plugin 的网络 Agent 节点：以正式报告一级标题作为子页面标题，精确解析 `MsteinL` 空间和 `Anchor周报` 父页面，按同名子页面创建或 replace 更新，写出 `docmost.json` 页面元数据；不重复创建，不删除或移动其他页面，本地 SVG 和来源清单不上传。已通过 Docmost MCP 真实工具清单与目标路径核查，Graph 相关回归通过；使用最近一份已整理报告实际创建子页面并复核空间、父页面和正文，尚未执行完整 Graph 真实 provider 生成验收。对应 A21。
- 2026-09-29：纠正先前对 Docmost 图片能力的判断：Docmost 原生支持通过 `/api/files/upload` 上传页面附件。新增受限 stdio MCP 工具，仅接受 `/in/publish/assets/` 下的 SVG/PNG/JPEG/WebP，并以页面 ID 绑定上传；周报同步节点将返回的附件 URL 替换到正文图片链接，更新同名报告时可替换现有附件。已有报告 SVG 已上传并插入 Docmost 页面；MCP 参数协议回归及上传、周报测试共 14 项通过，Graph JSON 合法。完整周报 Graph 真实 provider 验收仍待执行。对应 A21。
- 2026-09-29：根据 Docmost 页面实际阅读效果调整周报发布：页面标题栏承担一级标题，发布节点去掉正文第一行标题，避免重复；写作提示增加具体、平实的同事汇报口吻，减少模板化转折和空泛总结。已更新现有两篇周报页面，保留日期、图片、图注和正文层级；相关 Graph 回归通过。对应 A21。
- 2026-09-29：将 AI 味调研结论接入 weekly-work-report Graph：写作节点要求先写事实、对象、动作和结果，优先主动语态与直接动词，不强求各节同构或等长，保留真实取舍；独立评审增加模板化开头/结尾、元话语、抽象名词、隐藏责任主体和机械三段式检查，但只在影响理解或掩盖取舍时退回，避免机械禁词审查。新增对应回归断言，周报测试通过。对应 A21。

- 2026-09-30：完成企业微信通道到 Pilot 的本地接线与回归。修复将同步 HTTP 函数直接 await 导致阻塞/报错的问题，转为 `asyncio.to_thread`；模型处理失败与平台投递失败分开记录，首发投递失败保留已保存回复，重复事件不重跑模型；退出时调用 SDK 公开 disconnect。通道使用现有 API key 白名单；修复 waiting_approval 提示、超过 256 条事件的回复截断，以及追问已回答后旧事件重投错误读取新 Session 状态的问题。相关 31 项测试通过，最后补充的原提问回放专项 9 项通过；最终 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal -o addopts=''` 351 passed in 44.48s、退出码 0，Ruff、compileall 与 diff 检查通过。未改前端，未新增消息存储、恢复引擎或 Graph 调度器；独立业务助手 Graph 尚未实现。对应 A23/A24。

- 2026-09-30：用根目录 `.env` 中现有模型配置完成隔离的真实 provider 验证，证据位于 `.local/wecom-channel-proof/20260930T060131/evidence.json`。网关异步 HTTP 转发 → Bearer 认证 → 两个独立 Session/Pilot 并发回复通过；重复事件没有创建第二个 Turn；重建 Scheduler 后第三次模型调用读取原会话代号，未混入另一会话的代号，原生快照及可读历史落盘。此验证使用规范化测试事件，没有企业微信公网 WebSocket，也没有调用真实业务 Plugin；Scheduler 重建不等同于杀进程/重启服务验收。当前 `.env` 未配置 `WECOM_BOT_ID`、`WECOM_BOT_SECRET`，真实平台连接、逐用户授权、同图多 Run 和企业微信审批业务仍待下一阶段。对应 A24。

- 2026-09-30：纠正助手入口为指定普通 Graph，每条消息对应 Run，复用 Session/Turn、原生 FileStepStore 与 continue_run。按用户新决定支持后续消息取消旧 Run，保存中断文件快照并接续全部补充输入；修复 Graph 修改/删除和 Run 历史删除竞态、RunState 原子写入、空快照回退、节点跨分支续聊。通道与企业微信相关 16 项通过；真实 provider、HTTP、沙箱 stdio MCP、两用户并发、事件去重及真实服务进程重启验收通过，证据 `.local/wecom-graph-proof/20260930T150216/evidence.json`。这不证明真实企业微信公网联动或真实财务操作已验收。对应 A23/A24。

- 2026-09-30：补齐企业微信助手部署模板、配置自检与 `docs/wecom-assistant.md`，安装 `.local/demo` 助手 Graph，根目录 `.env` 保留现有配置并生成匹配的内部 API 密钥，自检仅缺 Bot ID、Secret、成员 userid。实际 SDK 对本地 WebSocket 服务验证认证、流式处理提示/最终回复、断线重连及重复事件不重跑；发现 SDK 1.0.2 正常关闭未安排重连，用公开生命周期/状态接口补齐。修复企业微信 stdio MCP 在沙箱内误导入宿主 Anchor，实测工具发现通过。独立只读复核后，相关 74 项通过，最终全量 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal -o addopts=''` 358 passed in 50.09s（SDK 两项弃用提示），Ruff、compileall、diff 检查通过。跨轮节点缺席、初始化失败、取消文件传递和连续三消息交接均有回归；未进行真实企业微信公网或财务审批验收，未重启现有用户服务，未提交或推送。对应 A23/A24。

- 2026-09-30：最终只读复核发现进度回复 ACK 延迟可能使消息倒序提交；网关改为先启动处理再等待进度 ACK，补受控延迟回归，避免旧消息反向打断新消息。最终独立复核无剩余阻断项；通道 15 项通过，最终全量 359 passed in 50.11s，Ruff、compileall、diff 检查通过。真实企业微信自检仍仅缺 Bot ID、Secret、成员 userid，待用户配置后私聊验收。对应 A23/A24。

- 2026-09-30：用户启动报 8077 地址已占用。核实旧 Anchor PID 4096975 使用同一 `.local/demo` 数据根且没有活动 Graph，终止旧进程后按用户命令配置 `examples/runtime.env.json` 后台启动当前代码，PID 296551，日志追加至 `.local/dev/anchor-serve.log`。Bearer `/graphs` 返回 200 且存在 `wecom-assistant`；通道非法事件返回 400，未启动模型或发送消息。未启动第二个网关，未宣称企业微信实连完成。对应 A23。

- 2026-09-30：用户报告私聊正常后只读核查真实运行。Anchor PID 296551、网关 PID 296625 在线；网关有出站 443 已建立连接。首条真实企业微信事件关联 `channel-a567840c-e3d1-42a8-a3c6-7125924f4886`，Turn completed、普通 `wecom-assistant` Graph finished、assistant 节点完成；真实模型 3 次请求，1 份原生 events.jsonl、9 份消息快照。176 字符最终回复与 Graph submission 一致，SDK 收到平台 ACK 后账本 completed，接收到确认约 5.80 秒，无投递错误。证据 `.local/wecom-graph-proof/20260930-live-private-chat/evidence.json` 不包含凭证、成员 userid 或消息正文。本次未发送测试消息或改动运行服务；真实平台跨轮记忆、连续消息打断、多用户及业务审批仍待分别实测。对应 A23/A24。
- 2026-09-30：修复运行看板 Graph 颜色碰撞。此前颜色来自 8 色固定调色板，`assistant` 与 `weekly-report` 的名称哈希可能落入同一槽位；现在用 Graph 名称的完整哈希混合生成稳定 HSL 颜色，并增加前端单元测试。`npm --prefix apps/web test` 24 passed、`npm --prefix apps/web run build` 通过；时间线两条 E2E 单独重跑均通过。

- 2026-09-30：同次检查期间用户又完成两轮私聊，现共三轮均为同一 Session、独立普通 Graph Run；三次平台投递 completed、无错误且回复匹配 Graph 结果。原生轨迹中的用户输入数依次为 1/2/3，确认真实平台路径跨轮历史加载；未进行指定事实记忆问答或真实打断/多用户实验。证据追加到上述 live-private-chat 文件，对应 A23/A24。

- 2026-09-30：用户反馈 `wecom-assistant` 与周报颜色视觉上仍接近。确认 `weekly-work-report` 与第一版哈希色相仅相差约 5°，补充 Murmur 风格 avalanche 混合，当前两者色相约相差 133°；Graph 运行历史中的状态圆点也改为使用 Graph 色，失败状态继续显示红色。前端测试 24 passed、构建通过；时间线 E2E 第一条通过，第二条首次因页面加载超时失败、单独重跑通过。

- 2026-09-30：继续修复看板反馈：运行历史列表的状态圆点此前只按状态着色，导致不同 Graph 的已完成 Run 仍同色；现改为圆点使用 Graph 名称颜色，失败状态保留红色，运行中保留 Graph 色及运行提示环。前端单元测试 24 passed，构建通过。
- 2026-09-30：用户仍看到 `wecom-assistant` 与周报颜色相同；复核确认源码按 Graph 名称生成的两个实际色值不同，但旧部署 bundle 或不支持空格 HSL 语法的浏览器会回退到同一个默认色。改用兼容的逗号 HSL 语法，并让内置页面 HTML 每次重新验证以获取最新哈希 bundle；`npm --prefix apps/web test` 24 passed、build 通过、diff 检查通过。重启 8077 服务后页面返回 `Cache-Control: no-cache, must-revalidate`，当前服务加载新 bundle。对应 A23/A24。

- 2026-09-30：将企业微信长连接正式纳入 Plugin/服务生命周期：Library 校验 `channel.json`，Anchor 服务扫描 Graph 节点挂载并由 `ChannelSupervisor` 启动、重启和停止单一 WebSocket 网关；`setup.py` 为新数据根复制完整 Plugin。扩展 `ChannelEvent` 与 TurnStore 保存通道附件，网关支持企业微信 image/file/mixed 消息，调用 SDK 下载并解密后落盘，Graph 以只读 `/in/channel` 读取，服务端限制来源目录、单事件 16 个附件、单文件 20 MiB、单事件 50 MiB。现有通道与 Plugin 回归通过，Ruff 通过；尚未用真实企业微信媒体消息或视觉模型验收，不能宣称模型已理解图片。对应 A23。
- 2026-09-30：用户首次打开更新后的网页时遇到 `prompt() is not supported`。原因是 API key 轮换后，管理 API 返回 401，网页调用浏览器原生 `window.prompt()` 输入密钥；部分宿主不支持该 API。改为页面内密码输入对话框，并让同时失败的请求共用一个输入、提交后各自重试；Pilot SSE 认证也复用该对话框。前端单测 24 项、API key 专项浏览器 E2E、生产构建及 diff 检查通过。服务端需重启加载新 bundle；对应 A23。
- 2026-09-30：用户发现 Pilot 面板显示企业微信助手聊天。根因是 Pilot 列表接口直接返回共享 SessionStore 中的所有会话，而企业微信对话虽由普通 Graph 执行，也持久化为 Session/Turn。现在 `/sessions` 只列未绑定 Graph/通道的 Pilot 会话，通道历史仍保留在各自 Session/Run 存储中；新增回归验证 Pilot 会话仍可见、企业微信会话不进入列表。对应 A24。

- 2026-09-30：用户报告关闭 Codex 会话后后端不可达。核查发现 8077 无监听、旧企业微信网关孤立存活；当前环境 PID 1 为 Bash，systemd offline 且 Docker daemon 不可达，无法在此激活宿主服务。新增 `/root/Anchor` 部署用 systemd unit，配置退出重启、开机安装目标、SIGINT 清理及 cgroup 子进程管理，并补充操作文档；`systemd-analyze verify` 与 diff 检查通过。停止确认身份的孤立网关后临时启动 Anchor PID 358872，唯一网关 PID 358873 为其子进程；8077 首页、鉴权 /graphs 及 5173 /graphs 代理均返回 200。临时进程不等于系统常驻部署；未验证关闭 Codex、宿主重启、故障重启或新增真实 provider/平台对话。对应 A26。

- 2026-09-30：按用户要求只读核查企业微信助手 5 轮对话及对应 Run。前四轮尚未挂 Plugin，最后一轮已挂 wecom；最后一轮通过 Bash 手动运行 server.py 的 tools/list 得到静态工具定义，未实际调用业务 API，不能据此称工具可用。当前 .env 仅配置 Bot ID/Secret，WECOM_CORP_ID/AGENT_ID/SECRET 缺失，Library 实际解析 wecom MCP 为 0；节点 network=false，未挂其他业务 Plugin，审批尚未实现。本地 SDK 1.0.2 提供 WSClient.send_message(chatid, body)，网关当前仅接 reply_stream，未向 Graph 暴露主动发送工具；平台主动发送权限及目标范围未实测。未修改权限或凭证、未触发模型及发送消息。对应 A23/A24。

- 2026-09-30：核对安装 SDK 1.0.2、WecomTeam 上游 commit `6bcb59a9a636c566f4c6ea5268b228e3def1611a` 的 README/API 矩阵、Node/Python 对照及示例，形成 `docs/wecom-sdk-capability-audit.md`。静态调用与离线规范化核查确认：语音转写未提取且后端拒绝 voice；未订阅业务事件；仅回复处理提示和最终文本；主动发送、图文输出、欢迎语、卡片、反馈均未接。图片/文件本地链路已接，真实媒体/视觉待验收。`pytest tests/test_wecom_plugin.py tests/test_channel_gateway.py -q -o addopts=''` 25 passed（2 条 SDK 弃用提示）、diff 检查通过。未修改运行代码或配置、未重启、未发消息、未触发模型调用。本次只完成审计，不宣称 SDK 全能力交付；对应 A23/A24。

### 2026-09-30 企业微信四项能力实现契约

用户授权开发：长连接主动发送、真实附件处理、持续流式输出、图文回复。沿用普通助手 Graph 和唯一网关，不接 Pilot，不扩大为通讯录/审批或群聊。

- 主动发送由已挂载企业微信 Plugin 的节点调用运行时提供的工具，委托现有网关执行 SDK `send_message`；节点不获得 Bot Secret 或管理 API key。服务端校验接收成员允许名单、当前 Run 取消状态；平台 ACK 才能记为已接受，超时不自动重复发送。
- 附件原件继续只读挂载；文本/PDF/常用 Office 文件做有界提取，图片经内容校验后使用框架原生 BinaryContent，不能将本地路径当成模型可见图片。当前模型能否接收图像需真实 provider 验证。
- 普通节点增加可选的可信输出观察者和运行时 toolset 接线；模型循环、持久化、历史仍由固定版本 PydanticAI/Harness 承担。仅将指定回复节点的结构化 summary 增量发送到平台，不泄露思考、任意工具参数或其他节点输出。
- 流式传输复用 TurnStore 事件和 SSE，兼容原 JSON 通道入口；断开订阅不重启 Graph。平台回复按同一 stream_id 有节流地更新，终态以已验证 summary 为准；新消息取消旧 Run 后禁止继续投递其业务输出。
- 图文回复由 Plugin 工具选择当前节点可读的 PNG/JPEG，校验路径/格式/大小后暂存为本 Run 回复产物，网关以 SDK `msg_item` 回传；不能读宿主任意路径或跨用户产物。主动发送首期 Markdown，图文用于当前对话回复。
- 工作包：节点框架接线约 25%，通道/工具约 40%，附件约 15%，集成与验收约 20%。节点与附件在独立 worktree 开发，主线负责调用契约、网关、Graph 接线和最终验收。分包测试 → 主线交叉测试 → 真实 provider/平台验收。平台实发测试需用户明确指定测试对象并授权，当前开发授权不自动当作发消息授权。

- 2026-09-30：按用户确定的四项优先级完成主线接线：挂载 wecom 的 Graph 节点使用受限运行时工具委托唯一网关主动发 Markdown；附件有界解析并以原生 BinaryContent 传图；公开 summary 经 TurnStore/SSE 持续更新；指定回复节点准备 PNG/JPEG，终态按 SDK msg_item 回传。复用原生持久化/续聊和既有事件账本，增加出站调用去重、成员允许名单、未知 ACK 不重发、只读图片路径及取消检查。独立复审发现的慢附件接纳倒序、初始 ACK 延迟、旧 ready/failed 重放覆盖新消息已修复，首次顺序与事件身份保存在原账本。未获响应的失败图片仅在下一轮上下文投影中替换，原生记录不改。相关分包与集成回归通过，最终全量 412 项通过；对应 A23/A24。
- 2026-09-30：四项能力隔离真实 provider 验收通过，证据 `.local/wecom-capability-proof/20260930T133215/evidence.json`。普通 Graph 接收有效 PNG、TXT、DOCX，返回红色/ALPHA-47/128.50 CNY、通过真实图片工具附回字节一致原图，HTTP SSE 在完成前输出正文，下一轮保留三项事实。实际 SDK 对本地 WebSocket 另验证主动发送帧及 ACK、持续正文、最终图文。没有启动真实 Bot 验收网关或向成员发送测试消息；本地模拟平台不等于企业微信公网四项验收。对应 A23/A24。

- 2026-09-30：四项能力最终收尾：`./.venv/bin/python -m pytest -q -n 8 --dist worksteal` 全量 412 项通过、退出码 0（SDK 的 websockets 弃用提示保留）；Ruff 与 diff 检查通过，无前端改动。独立复审无剩余确定阻断项，另复演旧失败事件在新 Run 进行中重投，不再压掉新业务回复。确认当前 0 个活动 Run/Turn 后，将与旧仓库一致、未经操作员修改的已安装 wecom Plugin 文件备份并升级，保留 Graph 与凭证，重启当前 Anchor PID 410211，唯一网关 PID 410212 为其子进程；8077 首页/鉴权 API 与 5173 代理返回 200，本机控制 socket 权限 0600，配置自检通过。部署证据 `.local/wecom-capability-proof/20260930T133215/deployment.json`，未向真实成员发送测试消息；仍是当前环境临时进程，未新增 systemd 常驻部署验收。对应 A23/A24，四项公网验收留待用户私聊。

- 2026-09-30：按用户对 userid 获取的疑问只读核查：当前入口名单为 `*`、出站名单未设置因此继承入口；已有一个真实企业微信通道身份保存在 Session.channel.sender_id。纠正接入文档中必须先找管理员取得账号的表述，说明回调 body.from.userid 可直接提供已接触用户标识，不把它与姓名或自建应用通讯录账号未经核对混用。未修改名单、权限、凭证或运行代码，未发送消息；联系人姓名映射/搜索尚未实现。对应 A23。

- 2026-09-30：按用户关于定期周报完成后企业微信提醒的提问，核查现有 schedule、weekly-work-report、Scheduler._run、通道监管和用户 Turn 接纳。此前仅记录结果回传与长任务关联会话目标，未实现完成订阅；当前周报以 Docmost 节点结束，计划没有通知目标。确认跨 Graph 挂载同一通道冲突、直接复用用户输入会取消当前助手 Run 两处架构缺口，记录源 Run 完成事件→订阅目标会话→助手 Graph→主动投递的候选方案及直接模板通知路径。明确候选未冻结、未修改现有运行代码或任务、未发企业微信消息；仅文档 diff 检查，对应 A27。

- 2026-09-30：用户确认保留内联与独立 Run 两种 Graph 组合，并要求从整体架构和 Web 易理解性设计。核查内联仅引用文件内 graphs、前端 Op 尚为只读命令、独立 trigger 无调用去重/持久接纳保证且按 Graph 拒绝并发、恢复重新读取工作区定义等具体边界；形成 docs/graph-composition-design.md，提出调用 Op、两种完成条件、输入与只读产物、逐轮调用身份、状态/停止/恢复和三级前端表达。旧通知订阅候选明确不作为实现合同。默认并发策略询问中，未冻结 schema、未开始实现或修改当前任务；仅文档检查，对应 A27。

- 2026-09-30：用户答复确认独立 Graph 调用的默认并发策略：多个任务调用同一个 Graph 时各自产生独立 Run，允许并发；助手同一用户会话仍串行。同步设计草案和 A27，明确实现需更新活动 Run 索引、全部活动运行的编辑/删除保护与 Graph 页面多运行呈现；既有手动/定时繁忙规则不因本答复静默改变。无运行代码变更、未运行测试，仅文档 diff 检查。

- 2026-09-30：用户授权按独立 Graph 调用设计完成开发。冻结 Op.call 与 wait/detach、输入映射、选定产物、稳定调用身份及 Web 投影契约；使用开发编排 skill 划分 runner/schema、服务调度、Web 三个隔离工作包，主线负责通道 Session 集成及真实验收。先将已验证的企业微信四项能力代码提交为 9f04c6b，保留未提交部署/文档改动；未推送。对应 A27，目前实施中，不能称功能完成。

- 2026-09-30：A27 首轮集成 runner/schema、持久调用服务、Web 编辑/关系/父子 Run 展示；增加共享通道与后台 Session 绑定，同会话后台等待前台、新消息让后台原 Run 暂停后恢复。独立复核发现并修复循环节点控制路径碰撞（a 第 2 轮与 a-2 第 1 轮）、多级调用来源会话校验、投递失败误报 finished；新调用控制使用完整节点及全局轮次摘要。相关 72 项通过，调用/会话集成 61 项通过。保持原生命令恢复的 Uncertain 语义，已开始但不确定完成的命令不自动重放。最终回归与真实后端浏览器验收进行中。
- 2026-09-30：A27 隔离真实 provider 验收：报告 Op 生成文件→wait 调用绑定既有 Session 的助手 Graph→模型读取 /in/call/report.md→测试网关记录一次主动通知→同 Session 新用户 Run 正确追问报告代号 QUARTZ-93 和金额 731.25。证据 `.local/graph-call-proof/20260930T144935/evidence.json`；真实模型与文件沙箱已执行，投递为显式测试替身，未开启实际企业微信通道或发真实消息。第一次验收脚本读取不存在的 detail 方法导致记录失败，纠正为公开 run 查询后完整重跑通过；未掩盖该脚本失败。

- 2026-09-30：A27 真实后端浏览器闭环通过：页面创建调用节点、输入映射/选定文件、连线保存，wait 真实命令文件回传与未选输入隔离，detach 两个并发子 Run、全部活动运行呈现、父子跳转、关系定位/定时标记、调用链过滤及 390px 窄屏检查。首次完整浏览器回归暴露既有秒级 Run ID 的同秒复用，改为时间戳加 UUID 并增加固定时钟两次运行回归；没有放宽测试或用等待规避。最终浏览器 17 passed、1 skipped（单独的 opt-in provider 浏览器用例）；证据归档 `.local/graph-call-proof/browser-release/`。
- 2026-09-30：A27 最终独立复核确认调用控制身份、跨会话祖先校验、投递失败状态已修复；另发现同 Session 排队任务误显 running，租约绑定具体 Run 后修复并回归。最终全量 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal` 479 项通过、退出码 0；前端 27 单测、生产 build、Ruff 和 diff 检查通过。SDK 弃用提示及 Vite 大 chunk 提示保留。原生不确定命令不会自动重放；没有引入另一套模型/恢复引擎。
- 2026-09-30：A27 部署：确认无活动 Run 后停止旧 Anchor 410211，启动当前代码 Anchor 516328，唯一企业微信网关 516329 为其子进程。8077/5173 首页与新 graph-relations API 均 200，channel-sessions 可见既有通道会话，控制 socket 存在，setup --check 通过；证据 `.local/graph-call-proof/deployment.json`。生产 Graph/定时计划/凭证未修改，未向真实成员测试发送；保留当前临时进程部署事实，未宣称启用系统常驻。使用文档与无模型调用示例已补齐。代码提交本地，未推送。

- 2026-09-30：用户授权实测周报完成后对接 wecom-assistant；将本机 weekly-work-report 的 docmost 后接 notify-wecom 标准 detach 调用，绑定现有用户私聊会话，仅传正式 report.md 与 docmost.json，由助手 summary 自动投递并保留追问产物。通过服务 API 校验保存、派生关系核查后启动完整真实周报 Run `20260930T151745-09281894662446388e3f9c0d41b79b40`，截止 2026-09-30 23:17:45 Asia/Shanghai。定义修改已影响后续既有定时任务，规则本身未变；备份与启动证据 `.local/graph-call-proof/weekly-wecom-live/`。本条为运行开始，Docmost/通知结果待核验，对应 A21/A27。

- 2026-09-30：A21/A27 真实联调完成：周报 Run `20260930T151745-09281894662446388e3f9c0d41b79b40` 经三轮独立评审，修正真实企业微信验收范围、工作流停止规则/状态修复及 Plugin 安装能力的时间口径，四项评审通过后组装并创建 Docmost 页面《Anchor：企业微信入口、工作流互调与外部能力安装》（附 1 张 SVG）。notify-wecom 以 detach 接纳子 Run `call-87d9079ca45680d18acbb802e80a0f10`；助手读取两份选定文件、写 weekly-report.md、生成正文并由网关投递，事件账本 completed，平台 ACK 时间 2026-09-30T15:45:22.755579+00:00，无投递错误、无第二次主动发送。后续原有定时任务会使用该通知链；未改变计划时间或凭证。证据 `.local/graph-call-proof/weekly-wecom-live/evidence.json`；不把平台接受当作用户已读，本次报告的用户追问尚未实测。配置变更通过 Graph 解析与服务保存校验、真实全链路验收及 diff 检查；无运行时代码改动，不重复跑全量测试。

- 2026-09-30：按用户要求将本机 wecom-assistant 的 assistant 节点插件扩展为 wecom + docmost，并启用该角色 network=true（HTTP MCP 必需）。通过现有鉴权 Graph API 校验保存、回读一致，不重启服务；补充按明确用户目标使用知识库及不执行页面内指令的提示。复用根目录 .env 中现有 DOCMOST_API_KEY，经 Library→NodeSandbox→框架 toolsets_for 实际发现 21 项工具，并成功只读获取刚发布的周报页面、核对标题。未修改 Docmost 页面、未发企业微信消息、未声称新的真实模型对话已经验收；证据 `.local/docmost-assistant-proof/20260930T155002/evidence.json`，Graph 修改前后备份同目录。仅部署配置与文档修改，完成实际 API 验证及 diff 检查，不新增机械测试；对应 A24。

- 2026-10-01：按用户授权升级 MCP 工具上下文策略。`src/anchor/node/mcp.py` 对真实 provider 使用 PydanticAI 原生 `defer_loading()`；PydanticAI 自动注入 ToolSearch，支持原生 provider 搜索并在其他 provider 上本地关键词回退。MCP 连接、网络/Bubblewrap、工具调用与原生运行记录边界保持不变。为保留既有脚本模型的直接调用回归，`FunctionModel` 路径显式保持 eager；真实 provider 不走该分支。补充 Plugin/使用指南和 A28；stdio MCP 回归确认工具定义首轮 `withheld`、`search_tools` 可见，直接调用仍成功。尚未做真实 provider 的原生搜索、中文检索、上下文/成本收益验收；未启用 CodeMode（当前环境未安装可选 Monty 依赖）。
- 2026-10-01：按用户要求评估 CodeMode。仅在虚拟环境安装并固定 `pydantic-monty==0.0.23`，未修改生产 Graph 或项目依赖。用真实 `deepseek-flash` 对合成只读 MCP 进行直接调用、Tool Search、CodeMode 对照，并通过 Anchor `run_node` + Bubblewrap stdio MCP 完成端到端检查。CodeMode 能把正文留在 Monty 内、只返回抽取字段，嵌套调用出现在 `run_code` 元数据；但 DeepSeek 有时会重复 `run_code`、请求 Bash 或因返回 schema 不清晰重试。发现最新 Monty 1.0 与 Harness 0.32.0 的 `max_duration_secs` 字段不兼容。详见 [CodeMode 评估](codemode-evaluation.md)，对应 A29；未接入生产 Graph，暂不全局启用。
- 2026-10-01：继续核对上游版本和官方 CodeMode 用法。PyPI 元数据显示 Harness 0.35 仍要求 Monty `<1`，0.36 起切换为 `>=1,<2`，0.52 固定 PydanticAI 2.52；隔离环境验证 `pydantic-ai-slim 2.46 + harness 0.36 + monty 1.0` 可以完成合成 Tool Search/CodeMode 调用。官方建议使用工具名单/谓词/metadata 选择安全工具、保留嵌套元数据、避免无必要的 MountDir/OS access。实际 Docmost 21 项工具目前没有 output schema 或读写注解，不能直接安全接入 CodeMode。Anchor 仍不升级生产依赖，升级 0.36 或 0.52 需要单独回归与版本冻结。
- 2026-10-01：按用户授权升级并回归 CodeMode 依赖。`pyproject.toml` 将 Harness 固定到 `0.36.0`，新增可选 `codemode` 依赖 `pydantic-monty>=1,<2`；当前虚拟环境为 PydanticAI 2.46、Harness 0.36、Monty 1.0。导入检查、Tool Search + `run_code` 合成调用、`tests/test_plugins.py`/节点相关回归、Pilot 回归及全量 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal`（479 项）均通过；Ruff、compileall、diff 检查通过。全量第一次受后台递归检索进程造成的磁盘 I/O 饱和而出现超时，清理残留进程后重跑通过，未发现确定性功能回归。CodeMode 仍未接入生产 Graph，也未全局启用；A29 只完成版本兼容和回归证据，真实 Docmost/企业微信业务灰度仍待后续安全 allowlist、结构化 schema 和真实 provider 验收。
- 2026-10-01：按用户授权接入受限 CodeMode AgentNode 能力。Graph 节点新增显式 `code_mode` allowlist（必须挂载 Plugin），校验拒绝 `all`、Bash、`run_code`、Tool Search、宿主挂载和 OS 访问；运行时在现有 MCP toolset/NodeSandbox 后懒加载 Harness `CodeMode`，未列出的工具保持普通调用。新增 Graph round-trip/非法策略测试和真实 Monty 1.0 合成 AgentNode 调用；全量 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal` 共 484 项通过，Ruff、compileall、diff 检查通过。真实 Docmost/企业微信 provider 灰度仍未做，CodeMode 未默认启用。
- 2026-10-01：根据用户反馈撤销 Graph 层 CodeMode allowlist 和资源参数，避免把 Harness 内部能力变成用户与 Plugin 开发者的配置负担。AgentNode 现在在真实 provider 路径自动创建 `CodeMode(tools='all', dynamic_catalog=True)`；Harness 自己保留 Tool Search/控制工具，Anchor Bash 保持原生；Monty 未安装时透明回退。Plugin 继续使用普通 MCP/PydanticAI 工具，不需逐工具适配；相关合成能力测试改为内部自动接线测试。真实 Docmost/企业微信 provider 灰度与嵌套副作用恢复观测仍待做。
- 2026-10-01：使用当前 `.env` 的 DeepSeek Flash 对自动接线做最小真实 provider smoke test。真实 AgentNode 自动创建 CodeMode capability，模型在同一节点内使用原生 Bash 和 `run_code`，2 次请求后完成结构化结果；未挂载业务 Plugin、未执行外部副作用。全量回归 480 项、Ruff、compileall、diff 检查均通过。真实 Docmost/企业微信业务指标与嵌套副作用恢复观测仍待做。
- 2026-10-01：用户执行 `scripts/dev.sh restart` 时发现旧手动 Anchor 进程未被 PID 文件管理，先停止旧 Anchor/企业微信网关并用当前代码重新启动；随后修复 `scripts/dev.sh` 的健康检查，改用公开根路径 `/`，避免配置 `ANCHOR_API_KEYS` 后把 `/graphs` 的 401 误判为启动失败。当前 8077 API、5173 Web UI 和唯一企业微信网关已恢复；带 API key 的 `/graphs` 返回 200，`wecom-assistant` 可见。对应 A26，未修改 Graph、计划或凭证。
- 2026-10-01：按用户反馈重写 GitHub 首页 README。新的首屏先说明 Anchor 的定位和实际收益，再给出 Graph/Run/Plugin/Pilot 心智模型、当前能力、可复制的本地启动路径、企业微信接入、CodeMode 自动接线、数据边界和分层文档入口；移除与当前 `.env`、`dev.sh` 和 API 触发方式不一致的旧命令。完成链接检查和 `git diff --check`，未修改运行代码。
- 2026-10-01：按用户要求新增普通 `rsi` Graph、`scripts/rsi/collect.py` 只读证据投影、`scripts/rsi/research.py` 固定 GitHub/PyPI/npm 公共 API 取证、确定性 `gate.py`、安装脚本和使用文档。Graph 顺序为 `collect → research → inspect → analyze → review → gate → publish`，失败评审最多回到分析三轮；提案只写 `evolution.json`，不自动改代码、Graph、Plugin 或重放副作用。隔离 provider-free 全图真实执行通过，公开 API 返回 GitHub 2 个仓库、PyPI 8 个包、npm 15 个包，`publish/` 产出五份文件；相关单测 43 项通过、Ruff 和 `git diff --check` 通过。通过当前 `/schedules` API 在 `.local/demo` 注册 RSI 每周四 09:00 计划，下一次为 2026-10-08 09:00（计划 ID 见本机 `state/schedules.json`）。真实模型 provider、长期递归质量、跨用户授权和提案实际实施效果尚未验收，对应 A25/A30/E2。
- 2026-10-01：RSI 阶段完成收口验证。按项目约定执行 `./.venv/bin/python -m pytest -q -n 8 --dist worksteal`，全量后端回归通过（含新增 RSI、示例 Graph、调度和沙箱路径）；`ruff check scripts/rsi scripts/setup_rsi.py tests/test_rsi.py`、`compileall` 和 `git diff --check` 通过。没有进行真实模型 provider 运行，因此不把全量测试或 provider-free Graph 结果写成真实 RSI 内容质量或长期自进化验收。
- 2026-10-01：修复 RSI 在 WebUI 点击“运行工作流”时报 `prompt() is not supported` 的入口错误。原因是通用 Graph 触发逻辑对 `rsi` 调用了宿主不支持的 `window.prompt()`；现与 weekly-work-report 一样直接提交默认输入。`npm --prefix apps/web test`（27 项）和生产 `build` 通过，当前 8077 `/graphs` 可见 `rsi`，无需修改 Graph/调度契约。
- 2026-10-01：首次真实 RSI 运行在 `inspect` 节点耗尽显式 `max_steps: 24`，控制记录为 `requests_used=24/24`，终态 `LimitsExceeded`。确认该字段映射为 AgentNode 的累计模型请求预算；对需要逐份阅读证据的 inspect/analyze/review 不适合固定小上限。先移除 RSI 三个 Agent 的 `max_steps`，随后确认 Graph 节点原有 `max_rounds` 也只是无证据的固定安全阀，已一并移除 RSI 所有节点的固定轮次。当前仅由评审门决定是否继续，运行级时间/成本/人工停止仍是后续应补的独立控制。原失败 Run 保留，不伪造完成；需新建 Run 复验真实 provider 和内容质量。
- 2026-10-01：根据用户对 Anchor 逐步增加功能后缺少总体指导思想的担忧，复核产品架构、当前架构与核心实现，并形成 A32 架构指导。产品架构补充入口适配、应用协调、Graph Runner、Node Runtime、Sandbox、Library/Plugin、业务 Graph 和 WebUI 的职责归属，新设计评审问题，以及单 Run 协调、局部并行、框架优先和唯一事实等执行不变量；当前架构如实记录 `serve.py` 内部状态被 Graph Call/通道流程直接访问、`simple/run.py` 承担多种执行职责两处压力，并明确渐进收紧契约的方向。用户进一步要求借鉴业界方法后，将 Ports and Adapters、Clean Architecture 依赖方向、DDD 限界上下文、按需 C4/ADR 收敛为轻量默认，不照搬微服务、完整 DDD 或其他额外机制；同步到 `AGENTS.md`，使后续开发 Agent 可直接遵循。用户补充希望 Graph+显式 Plugin 资源+Runtime 可独立交付，因此目标架构记录平台与精简执行包共享同一 Runner、凭证与主机授权留在部署环境；现状说明 `anchor-graph` CLI 已可单 workspace 执行，但依赖闭包和可分发 bundle 尚未实现。仅文档变更，未改代码、未跑测试；架构审查不是代码重构验收。
- 2026-10-02：在 `codex/rust-rig-runtime` 实验分支继续推进 Rig 验证，新增 provider-free Rust AgentNode 执行边界：`NodeRequest` 仅承载节点输入，`AgentCheckpoint` 保存 Anchor 身份、`RunSpec` 与 Rig `AgentRun`，`CompletionPort`/`ToolPort` 隔离 provider、沙箱和宿主权限，`NodeExecutor` 按 `CallModel → CallTools → Done` 驱动并支持边界取消、结构化 summary/route 校验和 checkpoint 续行。新增测试覆盖工具循环、非法 route、初始取消、pending tool 重载后继续和请求构造；`cargo test --workspace` 10 项通过，Clippy `-D warnings`、fmt、diff 检查通过。没有接入真实 provider、Python 宿主、沙箱或平台调度，也没有把实验代码写成 Rust 已替代现有 Runtime。
- 2026-10-02：为同一 Rust AgentNode 切片启用 Rig HTTP provider feature，并增加 `examples/real_node.rs` 的 OpenAI-compatible `chat/responses` smoke 入口。使用现有环境配置实际运行一次带工具调用的节点：模型先调用确定性 `fixture_tool`，再返回结构化结果，终态 `status=Completed`、`requests=2`、`summary=fixture-ok`。凭证只进入 live provider client，没有写入 checkpoint；本次只证明单节点真实模型边界可用，尚未证明 Rust Sandbox、Graph Runner、流式取消、平台集成或独立分发。
- 2026-10-02：在实验分支建立 [Rust/Rig Runtime 重构计划](rust-rig-migration-plan.md)，冻结 R0–R9 阶段、唯一 Runner/Kernel、端口隔离、边界持久化、局部并行和分层验收原则。R1 真实单节点已通过，当前推进 R2 持久化边界；计划不改变生产 Python 路径，也不把接口存在或 provider smoke 误记为迁移完成。
- 2026-10-02：继续推进 R2，新增 `CheckpointStore` 端口和 `FileCheckpointStore` 原子文件适配器。保存内容仅为版本化 `AgentCheckpoint`，文件名严格校验，缺失/删除具有明确语义；测试覆盖 round-trip、缺失、删除和路径逃逸拒绝。Rust 测试 11 项、Clippy `-D warnings`、fmt、diff 检查通过。边界保存接线、重启续行、并发写入语义和宿主集成仍未完成。
- 2026-10-02：继续推进 R2，`AgentCheckpoint` 增加已发出的 `pending_step`，`NodeExecutor::execute_with_store` 在模型请求前、响应后、工具批次前后和终态保存 checkpoint；provider 在模型响应前失败时，重载 checkpoint 后可以重新提交同一个 pending model step 并继续完成。新增真实协议状态测试，Rust 测试 12 项、Clippy `-D warnings`、fmt、diff 检查通过。工具副作用批次的未知结果不宣称 exactly-once，并发写入和宿主恢复策略仍待冻结。
- 2026-10-02：补齐 R2 的工具失败路径：工具批次执行失败时保留 `CallTools` pending step，恢复后可用新的 ToolPort 继续并完成后续模型请求。Rust 测试增至 13 项，Clippy `-D warnings`、fmt、diff 检查通过；仍明确不提供未知外部副作用的 exactly-once 保证。
- 2026-10-02：进入 R3 第一小步，为 `NodeExecutor` 增加可选 `ExecutionPolicy`，模型请求和工具执行均可设置超时；超时返回明确的 `Timeout` 错误并保留 pending step，默认策略不改变既有行为。新增慢模型超时回归，Rust 测试 14 项、Clippy `-D warnings`、fmt、diff 检查通过；尚未接入流式事件、真实取消传播或平台观测。
- 2026-10-02：继续推进 R3，新增 `StreamingCompletionPort` 和 `stream_completion`：宿主逐事件观察 Rig 流，只有 provider finish 后才得到完整响应；取消/超时会丢弃半成品并保留调用方的 pending model 状态。provider-free 流式文本与取消回归加入后，Rust 测试 16 项、Clippy `-D warnings`、fmt、diff 检查通过。真实 provider 流式、请求观测和平台事件接线仍未验收。
- 2026-10-02：使用现有模型配置运行 `examples/real_node.rs` 的真实 provider 流式 smoke：先完成真实工具调用节点，再观察到 7 个文本 chunk，最终文本为 `{"summary":"stream-smoke"}`；示例只输出最终文本，不输出 reasoning。Rust 测试 16 项、Clippy `-D warnings`、fmt、diff 检查通过。该证据仍是独立 Node/Provider 切片，不代表平台 SSE、真实进程中断或 Graph 接入已完成。
- 2026-10-02：补齐 R3 的请求/响应观测端口，新增 `CompletionObserver` 与 `ObservedCompletionPort` 装饰器；只报告 provider 标签和成功/失败结果，不接触 prompt、密钥或平台日志协议。确定性观测回归加入后，Rust 测试 17 项、Clippy `-D warnings`、fmt、diff 检查通过。R3 仍缺真实进程中断证据，未接入平台 SSE。
- 2026-10-02：扩展 R4 契约以覆盖既有 Python `SandboxSpec` 的 `workspace_readonly`、`tool_dirs`、env、`max_output_bytes`、spill directory/quota/mount 和 `incomplete` 语义。环境值与宿主 spill 绝对路径从 Debug 输出中脱敏；文档区分 Kernel 请求意图与 host-owned 实际路径、凭证、取消句柄和授权，并要求未来 Bubblewrap adapter 对所有字段实现或拒绝，不得静默忽略。新增字段校验与脱敏负向测试；仅契约/no-op，无真实执行。
- 2026-10-02：补齐 R3 中断后的确定性 checkpoint 续行证据：模型 timeout 后保存并重载文件 checkpoint，再由新 provider 完成同一 pending model step；流式收到部分内容后由宿主取消，确认半成品不进入 Rig 状态，checkpoint 编解码后仍可由新 provider 续行。测试还发现完成的 `Done` step 未清空 `pending_step`，现终态保存前清除该字段。`cargo test --workspace` 23 项、Clippy `-D warnings`、fmt 通过。仅为 provider-free 确定性测试，未模拟进程退出或真实 provider 断线，因此 R3 仍未完成；未接平台 SSE，也未改 Graph/Sandbox 语义。
- 2026-10-02：为真实宿主补充公开的 `NodeExecutor::execute_with_store_and_policy`，使超时策略和 checkpoint store 可在同一次执行中组合；旧 `execute_with_store` 委托给该 API 并使用默认策略。timeout 恢复测试改为只调用此 API，由执行器在模型 I/O 前自动保存 pending checkpoint，测试不再手工保存。`cargo test --workspace`、Clippy `-D warnings`、fmt 和 diff 检查通过。该改动未改变恢复语义，真实进程/provider 断线仍待验收。
- 2026-10-02：推进 A35/R4 Sandbox host adapter，新增独立 `rust/anchor-sandbox-bwrap` crate，Kernel 不获取进程执行能力。host policy 显式配置 workspace、只读输入、tool dir、spill 根目录，命令白名单、网络开关及 sandbox mount destination roots；真实路径经 canonicalize 授权，拒绝越权 workspace readonly symlink、未授权输入/工具/输出目录、受控 namespace 覆盖和未授权网络请求。adapter 并发 drain stdout/stderr，限制 preview 和 spill 总量，写入失败/reader 错误会标 `incomplete`；超时、取消会 kill 并 wait。环境值通过清理后的子进程环境传入，不进入 argv；Bubblewrap 用 info-fd 确认隔离容器已启动，setup 失败返回错误。fake helper 测试覆盖策略拒绝、symlink、输出配额、超时/取消和 setup failure；本机真实 Bubblewrap smoke 验证只读输入挂载及 `workspace_readonly` 写入被拒。`cargo test --workspace`：Kernel 27 项、adapter 8 项通过，Clippy `-D warnings`、fmt、diff 检查通过。网络隔离、真实 bwrap timeout/cancellation、同用户并发改写路径的 TOCTOU、Python/平台接线仍未验收，因此 R4 未完成。
- 2026-10-02：复核 spill host path 授权顺序后，明确 spill 目录必须由宿主预创建；adapter 先 canonicalize 并校验授权根，不创建请求指定路径。新增授权根内 symlink 指向根外时拒绝且不在目标处创建目录的回归。adapter 测试增至 8 项，`cargo test -p anchor-sandbox-bwrap`、`cargo fmt --check`、`git diff --check` 通过。
- 2026-10-02：补强 R5 持久 Run 事实完整性。`GraphRunRecord::validate` 核对结果与节点/Run/digest/invocation/commit身份、全局 sequence、invocation计数、边快照成员及 decision对源结果的引用；`FileRunStore` load迁移后校验、save前校验。损坏记录测试覆盖错误身份、重复/越界序号、未知边及悬空 selected edge；format 1 edge引用在迁移时补齐后仍经完整校验。Rust workspace 54 runtime + 8 Sandbox tests、Clippy、fmt、diff通过；无持久格式升级。R5阶段继续进行。
- 2026-10-02：新增 pause-before-dispatch Python oracle 场景。真实 Python Runner 在首节点前暂停时保持 `paused/reason=asked`，无执行、passes、edge decisions 或 cursor；Rust 使用现有 RunControl 对照通过。八场景 oracle、Python相关 67 项、Rust workspace 54 runtime + 8 Sandbox tests、Ruff、Clippy、fmt、diff检查均通过。该测试未覆盖节点已活动时的真实provider中断/取消。
- 2026-10-02：收敛已完成节点返回非法 route 的失败记录。该结果是确定的节点 completion，不再以 cursor 表示可续行；若 RunStore 写入 failed 失败，恢复会再次读取既有 completion fact、重验 route 并终止，不会重跑节点。定向写入失败测试、Rust workspace 54 runtime + 8 Sandbox tests、Clippy、fmt、diff检查通过。
- 2026-10-02：补强 R5 edge freshness 持久化校验。任何引用实际节点结果的边决定，其序号必须严格晚于被引用结果；`result_sequence=0` 的传播/ceiling 合成 false 决定不受此规则影响。新增 stopped self-loop 有效记录，以及将 selected decision 序号改为等于源结果序号后 FileRunStore save/load 均拒绝的回归。Oracle 生成器直接记录 Python `RunState.executed`，但 Rust 差分目前用 NodeExecutionPort dispatch 顺序作代理；固定十场景中两者一致，这不等于普遍语义等价证明。R5 oracle 对照范围据此说明，不扩展 source 最新 invocation 约束。Run format 未改变；workspace、Clippy `-D warnings`、fmt、Ruff、Python 相关子集与 diff 检查结果随本条验证更新。
