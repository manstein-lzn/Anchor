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

依据本地 `pydantic-ai-slim==2.46.0` / `pydantic-ai-harness==0.32.0` 的源码与随包 README：

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
| A15 | 定时/Webhook 触发与运行时间线 | 支持周期及一次性定时；停机或 Graph 忙时错过的计划清楚显示、不补跑；Graph 忙时拒绝所有来源且不创建 Run；Webhook 不去重；统一 Run 输入 | E1 | 后端全量回归、桌面/移动浏览器看板、时间线筛选、计划 Modal、Run 预览与详情路径通过；看板已收敛为与 Graph/Pilot 一致的全屏工作区，并有独立浏览器回归覆盖计划/运行定位、窄屏滚动和无横向溢出；运行条与计划点统一使用限制在视口内的原生 Popover 详情，长名称换行；左右边缘、窄屏横向滚动、键盘聚焦和提示关闭回归通过；色条/圆点与按钮共用可见尺寸，周围空白不触发悬停或点击，相邻短 Run 双向命中通过；图例仅保留实际运行、计划时点与已错过；空心圆表示计划、带斜杠圆表示错过；真实 provider 与真实停机恢复验收待做 |
| A16 | Bearer 白名单与 Responses 子集 | 所有 API 统一 key；同 key 的 previous_response_id 续聊；普通 JSON/SSE 文本请求 | E1 | 伪 provider HTTP JSON/鉴权测试与代码回归通过；真实 provider 和官方兼容细节未验收，不声明完整兼容 |
| A17 | 从 Graph 页面查看执行 | 选择 Graph 后直接看到当前/最近状态、运行次数；运行历史按需展开，按钮与画布位置保持不变，点击记录直达执行画布与节点/产物详情 | E1 前端体验 | 2026-09-28 运行概览融入 Graph 页头，原生浮层展示此图全部运行历史；工作台 E2E 覆盖桌面/手机展开位置与画布尺寸不变、超过四条历史、键盘/Esc/外部点击、详情直达和空状态。真实 provider 不属于此 UI 入口验收 |
| A18 | 开发服务访问 | 5173 页面及其后端代理可访问 | 开发环境 | 2026-09-28 两端无监听后通过现有 dev.sh start 恢复；首页、/graphs、/timeline?days=30、/sessions 经 5173 请求均 HTTP 200；仅验证本机访问，未验证用户侧端口转发 |
| A19 | 社区 Plugin 生态兼容任务交接 | 背景与目标见独立说明，具体方案和兼容验收待探索 | 独立任务背景 | 已生成 [任务背景](plugin-ecosystem-task-background.md)；仅完成文档交接，未实现或验收生态兼容 |
| A20 | 安装来源 Plugin 并统一 Anchor bundle 格式；兼容 Skills/资源与 MCP | 安装结果根目录 `plugin.json`；其他来源资源保留；不记录来源类型；明确暂不支持 hooks/commands/agents | 社区 Plugin 兼容 | 安装 API、根清单转换、Skill/资源浏览器 E2E、MCP stdio Bubblewrap 发现及调用回归通过；HTTP/SSE 环境变量鉴权配置与显式 OAuth 授权入口已接入。OAuth 外部 provider、真实社区 MCP 端到端未验收 |

| A21 | 每周四 09:00 本机工作周报与独立反馈 | 普通 Graph：项目理解与选题、写作、读者评审；问题退回、逐项复验、通过后组装；评审通过后使用 Docmost Plugin 同步图文到 MsteinL/Anchor周报；不设 blocked | 周报 Graph | 计划已启用；旧真实 Run 20260928T132435 已交付，但用户否定其流水账式内容；已重构读者视角指令，相关 11 项测试通过，新版真实内容质量待验收；Docmost 同步节点已接入，已整理报告已创建为目标父页面下的子页面并插入上传的 SVG；上传工具与图文替换回归通过；完整 Graph 真实 provider 验收待执行 |
| A22 | Pilot 历史会话改名与删除 | 每行“…”菜单；改名持久化；确认后删除空闲会话；当前聊天及关联 Run/产物边界明确 | Pilot 会话管理 | 后端 336 项、前端 23 项单测与 E2E 13 条通过（provider opt-in 1 条跳过）；隔离真实 HTTP 浏览器覆盖持久改名、取消/失败保留、删除其他/当前会话、草稿与刷新、手机及键盘；本机页面只读核查菜单可用。未触发新 provider 请求 |
| A23 | 企业微信 Plugin 与 WebSocket 通道 | 长连接、事件去重、回复投递、自动监管及附件落盘 | Plugin/通道联动 | `channel.json` 已由 Library 识别；Graph 挂载 Plugin 后由 Anchor `ChannelSupervisor` 自动启动单一网关并在服务退出时停止；文本/图片/文件/混合事件已规范化，附件安全下载到 `state/channels/wecom/events/<event>` 并只读挂载 `/in/channel`。网页登录的 401 通过页面内 API key 对话框输入并重试，避免依赖浏览器原生 prompt。原有真实文本 provider、HTTP、Graph、本地 MCP、并发、重启和真实企业微信文本私聊证据保留。媒体真实企业微信投递、模型视觉理解和多平台监管仍待真实 provider/平台验收 |
| A24 | 多用户 Graph 助手工作中枢 | 同图并发、独立原生历史、Plugin 调用、新消息打断并接续 | 近期目标 | 普通 Graph 接线、两用户隔离、跨轮产物、消息打断、三消息交接、本地 stdio MCP 测试通过；真实 provider 基础链路通过。Pilot 的 `/sessions` 列表仅显示 Pilot 会话，企业微信 Graph 会话不混入 Pilot 选择器；相关隔离回归通过。跨轮跳过节点、模型初始化失败后的快照回退和 SDK 实际连接/重连回归通过；全量 359 项通过。逐业务用户数据授权、企业微信审批 API、RSI 尚未交付 |
| A25 | 从工作证据改进 Plugin、Graph 与 Anchor | 普通 Graph 生成候选改动；可追溯证据、对比验证、按授权发布及效果观察；私有记录不跨用户泄露 | 远期需求，暂不实现 | 用户明确待积累足够数据后再执行改进 Graph；不是企业微信助手的前置工作，无 RSI 效果或真实 provider 验收 |

每阶段执行相关后端测试、Ruff/compileall、前端测试/build、真实 HTTP 与浏览器验收。最终必须取得全量 pytest 的明确退出码和总结；运行中或仅看到进度点不算通过。阶段产物不能等同于产品全部完成。

## 推进记录

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
