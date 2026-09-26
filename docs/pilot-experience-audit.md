# Pilot 对话体验与 Pydantic 接入核查

核查日期：2026-09-26。依据当前代码、实际安装的 `pydantic-ai-slim==2.46.0` / `pydantic-ai-harness==0.32.0`，以及本日读取的官方文档。在线文档会随版本更新，不能把文档中的所有能力直接归入本地版本。

结论：可读、可控、过程可见、刷新能恢复的 Agent 对话是 Anchor Pilot 的基础交付标准。当前不足主要是接入没有贯通，不是 Pydantic 缺少基础能力，也不是这种体验必须依赖昂贵模型。更强的模型影响研究与推理质量；流式呈现、工具状态、输入交互和会话恢复首先是工程问题。

## 能力对应与证据

| 用户体验 | 框架已有能力 | Anchor 实际接入 | 需要完成的工作 |
| --- | --- | --- | --- |
| 正常多轮对话、工具调用 | `Agent`、`RunContext`、schema、`message_history` | Pilot 已用；工具名兼容性已修正，真实模型发送和工具查询通过 | 保留真实 HTTP、浏览器和 provider 验收，不能只测模拟 Agent |
| 边生成边显示 | `run_stream_events`、`event_stream_handler`、文本增量 | 未接入。`pilot.respond` 用 `agent.run` 等待最终结果；HTTP 请求等整轮返回 | 传递真正的增量事件，不能将完整文本分片假装流式 |
| 看见正在调用什么工具 | `FunctionToolCallEvent`、`FunctionToolResultEvent`；Harness 步骤事件 | Pilot 历史投影只保留 `UserPromptPart` / `TextPart`，工具消息被过滤 | 显示工具名称、状态、可展开参数与结果；关联实际调用 ID |
| 稳定的聊天前端协议 | `VercelAIAdapter`、`AGUIAdapter` | 均未接入；前端仅调用普通 JSON API | 先选一个协议完成真实流式纵向路径，避免自造又一套模型消息格式 |
| 可直接启动的聊天界面 | `Agent.to_web()` | 已安装版本有此 API；当前未用 | 可作为对照与调试基线。官方定位是本地开发和调试，不能直接代替 Anchor 的 Session、运行关联和批准策略 |
| 用户确认后继续原操作 | `DeferredToolRequests`、`DeferredToolResults`、`ApprovalRequired` | Pilot 用自定义审批字典；前端确认后另发一句“我已确认”，依赖模型再次构造工具调用 | 将批准绑定原始 call ID 和已保存参数，再恢复调用。保留 Anchor 的资源前态检查与权限校验 |
| 真正暂停并等待回答 | deferred tools；Harness `AskUser` 的问答 schema / answerer | `session_ask` 仅改状态并返回普通工具结果，模型本轮仍能继续调用工具 | 定义可持久等待的中断边界；回答恢复同一会话。`AskUser` 默认在工具内等 answerer，并不自动解决进程退出后的恢复 |
| 中断恢复且不重复副作用 | Harness `StepPersistence`、`SqliteStepStore`、`continue_run`、tool-effect ledger | 普通 AgentNode 有步骤恢复；Pilot 只存用户输入和整轮最终消息，并有局部 Graph 修改回执 | Pilot 接入步骤记录，覆盖所有有副作用的工具；未知结果拒绝自动重放，不能宣传 exactly-once |
| 长对话不会悄悄丢上下文 | Harness 压缩、摘要、上下文占用事件 | `node/context.py` 有实现与测试，但默认 `simple/run.py` 未接入 `context_capabilities`；Pilot 构造 Agent 也没有配置这些能力 | 核对模型窗口，启用有证据保留的压缩，向 UI 显示占用与压缩状态；避免仅截断历史 |
| 可见的任务计划 | Harness `Planning`、持久 PlanStore 和计划事件 | Pilot 未接入 | 多步研究显示真实计划与进展；计划是辅助组织，不替代 Graph 调度或研究验收 |
| 可读的消息与易用输入 | Markdown、代码高亮、表格、数学公式是前端责任 | 项目已有安全 Markdown 渲染器，旧 Pilot 没用；本轮复用 | 排版、复制、中文输入法、草稿、导航恢复、滚动与移动端验收 |
| 图、运行、产物在对话中联动 | Anchor 自己的 Graph/Run/Artifact API | 有控制工具和关联 Run，聊天区尚无完整对象视图 | 把实际运行状态和产物引用投影为可点击内容，不能根据模型口述伪造进度 |
| 文件/图片输入、引用与交付 | PydanticAI 有多模态 UserContent；实际可用性取决于模型 | Pilot API 只接受字符串；无聊天附件通路 | Anchor 负责上传、大小/格式校验、存储与沙箱挂载，模型消息引用受控资源。论文/PDF 不能仅靠前端显示附件图标 |
| 失败重试、编辑后分支 | 框架消息历史；Harness `fork_run` | 仅失败后整轮续答，无消息编辑与分支界面 | 明确重试与新分支的身份；已完成副作用不能跟着“重新生成”自动重做 |
| 合理的过程说明 | PydanticAI 能传递 provider 返回的 thinking / 文本 / 工具事件 | 当前只显示最终文本 | 优先显示计划、工具活动和可核验的状态；只有模型明确提供且允许展示的内容才显示，不能编造或承诺完整内部推理 |
| 运行中补充指令、刷新与重连 | 框架提供取消/事件/历史等原语 | 当前没有完整的输入排队、事件游标重连和恢复状态 UI | 服务端拥有执行；浏览器订阅事件。新输入何时生效必须明确且去重 |

代码依据：[`pilot.py`](../src/anchor/pilot.py) 的 `_agent` / `respond` / `_text`，[`serve.py`](../src/anchor/serve.py) 的 `pilot_message`，[`session.py`](../src/anchor/session.py)，[`node/adapter.py`](../src/anchor/node/adapter.py)，[`node/context.py`](../src/anchor/node/context.py)，[`simple/node_bridge.py`](../src/anchor/simple/node_bridge.py)，[`Pilot.tsx`](../apps/web/src/Pilot.tsx)。

## 恢复与确认仍存在的实质缺口

- Session 的 `operation` 只有一个槽位，并以操作内容作为 intent key；它不是按 turn / call ID 的完整操作账本。相同操作在之后合法地再次执行，可能与重试混淆；其他操作也可能覆盖旧记录。
- `graph_run` 和运行控制尚未走上述修改回执。因此“所有控制工具都能安全恢复”的说法不成立。
- 请求多个确认仍可能覆盖同一个 `approval`；`session_ask` 和确认请求都没有让模型执行真正暂停。
- `expected_sha256` 前态比较与 Graph 写入不是跨所有写路径的原子 CAS，不能宣称完全消除了并发修改窗口。
- Session JSON 与事件文件之间不是一个事务；Harness 的会话 revision 校验不能替代 Anchor 的状态事务。
- Pilot 的框架 `conversation_id` / `run_id` 尚未与 Anchor 的 Session / 单轮执行统一传递。只给数据库记录一个会话 ID 不等于打通了执行身份。

这些缺口需要先于自动恢复与高影响操作的完整产品验收收口。追加一个 loading 图标或换一套 CSS 不解决它们。

## 建议采用的实现边界

1. **框架负责模型与工具执行。** 优先复用 PydanticAI 的完整事件流、deferred tools 和 Harness 的步骤记录。Pilot 与普通 AgentNode 复用这些执行语义，不强行共用要求文件工作区和沙箱的整条节点适配器。
2. **Anchor 负责产品事实。** Session 身份、Graph/Run 关联、审批权限、资源前态、研究合同、交付验收和未知副作用处置仍归 Anchor。框架的批准事件不是业务授权，也不是文件系统事务。
3. **UI 负责投影。** 优先评估已提供的 Vercel AI 协议适配器与 React 客户端；以一个真实文本 + 工具 + 批准纵向样例验证当前 HTTP 栈适配成本，再决定是否引入 ASGI。避免直接替换整个服务，也不再用模型消息文本冒充结构化状态。
4. **不把全部 Harness 能力无差别启用。** Shell/FileSystem、SubAgents、Memory、BrowserUse、外部搜索服务都有各自边界和成本；已有沙箱和 Plugin 不应被通用宿主工具绕开。Codex/Claude Code 在这里是交互质量参照，不要求复制它们的全部产品范围或内部架构。

## 最低验收标准

必须以完整路径验收，而不是按界面组件计数：

- 新建 → 输入中文、多行文字 → 发送 → 增量回复 → Markdown/代码/表格正常显示。
- 调用工具 → 看到真实开始/完成/失败 → 刷新后记录仍可查。
- 请求批准或补充信息 → 执行确实暂停 → 拒绝无副作用 → 批准/回答只恢复对应调用。
- 生成中停止 → 已有输出和状态清楚可见 → 继续不偷偷重复已完成副作用。
- 切换对话、断网、刷新和服务重启 → 历史与待处理事项保留 → 不能让一次提交变成两次执行。
- 长会话触发压缩 → 保留目标、用户决定和产物证据 → 不用固定轮数宣告研究完成。
- 从对话进入实际 Run / Artifact，并能返回同一会话；手机屏幕下输入区和控制按钮可用。

## 本轮交付与边界

本轮先落地现有界面的 Markdown、复制、搜索、由首条输入生成的持久标题、当前位置与未发送草稿恢复、中文输入法处理、自适应输入高度和滚动控制。以上不意味着流式、步骤恢复、研究合同或系统 Pilot Graph 已完成。

后续顺序应为：事件流与调用身份 → 暂停/批准/步骤恢复 → 上下文与计划 → Graph/Run/产物联动。验收清单和架构文档必须跟随代码更新，不能把“依赖提供”写成“Anchor 已具备”。

官方参考：

- [Agent 执行与流式事件](https://pydantic.dev/docs/ai/core-concepts/agent/)
- [UI Event Streams](https://pydantic.dev/docs/ai/integrations/ui/overview/)
- [Deferred Tools](https://pydantic.dev/docs/ai/tools-toolsets/deferred-tools/)
- [Web Chat UI：明确定位为本地开发和调试](https://pydantic.dev/docs/ai/guides/web/)
- [Harness 能力地图](https://pydantic.dev/docs/ai/harness/)

本次尝试读取 OpenAI 官方 Codex 功能页收到 HTTP 403；不据此判断其内部实现，也不声称上述方案与 Codex/Claude Code 内部架构相同。
