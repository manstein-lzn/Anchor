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

当前顺序只有：核对框架接口 → 接通 Anchor 会话入口 → 真实中断续聊验收。P3–P6 不提供当前实现指导。

2026-09-26 独立验收：五处待修复问题及复现证据见 [P2 独立验收报告](pilot-p2-acceptance-review.md)。历史路径通过不代表全部入口和压缩组合已通过。

## 当前共享契约（2026-09-26 收敛）

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
| A02 | Markdown/表格/代码、输入法、草稿、手机布局 | 可读、可输入、无横向溢出 | 基线 | pass（`e2e/pilot.spec.ts`） |
| A03 | 增量回复与工具开始/结束 | 最终结果之前可见真实变化 | P1 | pass（`tests/test_pilot_turns.py` 真实 PydanticAI 流及写事务期间读取已提交事件；`e2e/pilot.spec.ts` 文本与工具卡片；真实 DeepSeek HTTP/SSE 已贯通，事件库 WAL 修复逐条写入导致的读取锁超时） |
| A04 | 同 ID 并发提交/丢失响应重试 | 只执行一次；不同内容冲突 | P1 | pass（8 路并发同 ID 只建一个 turn；同 ID 不同内容 409；运行中二次提交 409） |
| A05 | 生成中刷新/断线重连/停止 | 接回同一 turn；部分输出保留 | P1 | pass（断线后只补游标之后的事件、工具只执行一次；停止后 turn 为 `stopped` 且增量文本保留） |
| A06 | 跨会话读取、非法游标、归档提交 | 明确拒绝且无执行副作用 | P1 | pass（跨会话 404、`?after=invalid` 400、归档后新提交 409 而已接受请求仍幂等） |
| A07 | 服务中断，隔日重开原会话并发送消息 | 加载框架工作记录；标明中断；Agent 可核查后继续 | P1/P2 | 代码回归通过：带新消息与空 prompt 续聊均读取原生快照并关闭未知工具结果；真实 provider 带新消息证据保留，空 prompt 真实复验待跑 |
| A08 | 必要提问/确认后继续；已授权普通操作无需逐次审批 | 用户决定可保留，回答回到原会话，不被旧审批门禁卡住 | P2 | pass（普通操作已取消逐次审批；`graph_delete` 保留确认；真实 DeepSeek 见 `scripts/verify_pilot_provider.py`：直接建图/启动 Run、拒绝删除、过期确认、`session_ask` 问答） |
| A09 | 工具执行中退出，缺少最终结果 | 加载框架留下的事实；Agent 查询现场再续做，不要求人工处置账本 | P2 | 代码回归通过：原生中断快照经框架合成 interrupted tool return，续聊不会重放；真实 provider 空 prompt 复验待跑 |
| A10 | 长对话需要压缩时仍可继续 | 复用框架压缩；不建设记忆或计划系统 | P2（按需） | 代码回归通过：按原生快照时间选择压缩后的新记录，并向滑窗/摘要能力传递 context_window；真实长会话待 provider 复验 |
| A11 | 系统 Pilot 的保护和联动 | 产品需求记录，边界未冻结 | 后续需求 | 不纳入本轮验收 |
| A12 | 对话与 Graph/Run/Artifact 联动 | 本轮只验收已有对象的直接跳转 | P2/P7 | 代码与前端回归通过：对象跳转服从未保存编辑确认，Artifact 自动打开文件页并展开目标路径；浏览器完整复验待跑 |
| A13 | 研究目标与证据验收 | 产品需求记录，属于研究应用 | 后续需求 | 不纳入 Anchor 核心验收 |
| A14 | 附件、编辑分支与导出 | 产品需求记录，资源边界未冻结 | 后续需求 | 不纳入本轮验收 |

每阶段执行相关后端测试、Ruff/compileall、前端测试/build、真实 HTTP 与浏览器验收。最终必须取得全量 pytest 的明确退出码和总结；运行中或仅看到进度点不算通过。阶段产物不能等同于产品全部完成。

## 推进记录

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
