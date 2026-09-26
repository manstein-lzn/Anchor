# Anchor 产品与系统架构

状态：**产品真相与目标架构**。

本文记录 Anchor 当前已经确认的产品哲学、对象边界、生命周期和演进顺序。它回答“Anchor 要成为什么，以及各部分为什么这样协作”；代码实现的当前细节见 [当前架构](architecture.md)，用户操作见 [使用指南](usage.md)，Plugin 的文件格式见 [Plugin 设计](plugins.md)。

本文不是把未来功能写成已完成的功能。每个章节都明确当前状态和目标状态；实现开始前先更新这里的契约，完成后再更新当前架构和验收记录。

## 产品方向

Anchor 是一个以 Graph 组织 Agent 工作、以文件系统保存事实、以 Git 保存可追溯快照、以沙箱保护执行边界的 Agent 产品运行时。

用户最终不需要先理解 Graph 编排、节点工作区或模型配置。用户可以和唯一的系统级常驻 Agent **Anchor Pilot** 对话，由 Pilot 帮助理解目标、选择或构建 Graph、启动运行、处理中间确认、查看证据并完成验收。WebUI 仍然存在，但它是观察、编辑和人工接管界面，不是唯一入口。

Anchor 的核心积累不是某次对话，而是可复用、可观察、可组合的资产：

```text
Plugin（能力规范与工具）
        ↓ 挂载
AgentNode（理解与判断）  +  OpNode（确定性程序）
        ↓ 组织
Graph（可运行流程资产）
        ↓ 执行
Graph Run（一次事实记录）
        ↓ 交互
Session（用户与 Anchor Pilot 的长期关系）
```

Pilot 是控制面上的特殊 Graph。它可以使用其他 Graph，但不取代其他 Graph，也不把所有业务流程塞进一个巨大 JSON。普通 Graph、AgentNode、OpNode 和 Plugin 都是可管理资产；Pilot 只是受系统保护的一个 Graph。

## 不可动摇的设计原则

1. **唯一事实来源**：Graph 定义只在 `graph.json`；Plugin、工具和环境只在能力库；Session、Run 和节点历史各自保存自己负责的事实。派生摘要必须能回到来源。
2. **极简对象模型**：Graph、AgentNode、OpNode、Plugin、Run、Session 六类对象足够表达产品。不要为了形式主义增加 Graph 版本平台、临时 Graph 类型或第二套 Agent 内核。
3. **能力渐进披露**：Plugin 先给 Agent 名称和短描述，需要时再读取说明、知识和工具用法。能力挂载可见，实际调用另有运行记录。
4. **智能与机械事实分工**：Agent 负责理解、规划、判断和解释；代码负责权限、状态转换、路径、哈希、提交、幂等和恢复边界。
5. **用户始终可控**：运行可以暂停、恢复、停止；需要用户决定时进入 `waiting_user`；不使用固定 `max_xxx` 伪装研究质量或收敛。资源预算是用户明确的运行约束，不是任务完成判据。
6. **恢复是正常路径**：进程退出、页面关闭、服务重启和长任务暂停都不能让用户丢失会话或运行事实。
7. **快照而非隐式共享**：下游默认读取上游 commit 对应的只读快照；需要时可查询该快照的祖先历史。不同 Run 之间不隐式继承工作区。
8. **透明可预测**：Graph UI、运行记录和 Pilot 对话使用相同的 Graph、Node、Plugin、Run 标识，不在界面背后制造另一套隐藏流程。

## 对象与所有权

| 对象 | 负责什么 | 权威存储 | 生命周期 |
| --- | --- | --- | --- |
| Plugin | 一套做事规范、渐进式说明、可选知识和工具入口 | `library/plugins/<id>/` | 独立资产，可被多个 AgentNode 引用 |
| AgentNode | 模型理解、判断和沙箱内执行 | Graph 的 `nodes` + `agents` 引用 | 随 Graph 定义存在 |
| OpNode | 确定性命令或程序步骤 | Graph 的 `nodes` + `ops` | 随 Graph 定义存在 |
| Graph | 节点、边、路由、反馈和目标 | `workspaces/<id>/graph.json` | 普通资产；可编辑、运行、归档或删除 |
| Graph Run | 一次 Graph 执行的状态、工作区、commit 和轨迹 | `workspaces/<graph>/runs/<run>/` | 可暂停、恢复、停止、验收和删除 |
| Session | 用户与 Anchor Pilot 的长期对话及关联运行 | `sessions/<id>/` | 可继续、归档和删除；删除前保留明确的运行引用处理 |
| Anchor Pilot | 系统唯一控制面 Agent/Graph | 保留的系统 Graph 目录与系统注册信息 | 初始化创建，不可删除或改名 |

Agent 的共享 `agents` 配置只是模型、指令、权限和读写声明的复用配置，不是独立的 Agent 资产库。Plugin 直接挂到 AgentNode，不通过角色继承、默认合并或复制实现。

## Anchor Pilot

当前实现是 `src/anchor/pilot.py` 中的 PydanticAI Agent，通过结构化工具调用 Scheduler；尚未创建系统 Pilot Graph 或 `anchor-control` Plugin。下文的系统 Graph 定位与保护规则属于目标设计。

### 定位

Anchor Pilot 是 Anchor 初始化时唯一保证存在的系统级 Graph，内部至少包含一个 Pilot AgentNode，并挂载系统 Plugin（建议 ID：`anchor-control`）。它拥有调用 Anchor 控制接口所需的最高系统权限，但仍受到 Anchor Runtime 的结构化接口、路径边界和用户确认规则约束。

Pilot 可以：

- 通过对话澄清用户目标和验收标准；
- 查询 Graph、AgentNode、OpNode、Plugin 和运行状态；
- 选择已有 Graph，或生成一个普通的 `graph.json`；
- 校验、保存、启动和观察 Graph Run；
- 创建研究合同、请求用户确认并在确认后继续；
- 读取产物、commit、运行轨迹和错误；
- 请求暂停、恢复、停止或重新开始；
- 在证据推翻原问题时提出合同修改，而不是悄悄换题。

Pilot 创建的 Graph 与用户创建的 Graph 完全相同。不存在“临时 Graph”或第二种生命周期。用户不需要的 Graph 可以明确删除或归档。

### 系统保护

系统 Graph 的保护必须在 Runtime 实现，不能依赖前端隐藏按钮：

```json
{
  "id": "anchor-pilot",
  "system": true,
  "deletable": false
}
```

删除、改名、移除系统控制 Plugin 或把 Pilot 变成普通 Graph 的 API 请求都必须拒绝。系统升级可以由 Runtime 原子替换 Pilot 定义，但普通用户操作不能删除它。UI 可以省略删除入口，但 UI 不是安全边界。

### 控制 Plugin

Pilot 的 Graph 任务通过结构化 Anchor 控制工具完成，而不是依靠隐藏提示词或裸 Shell：

```text
graph_list       graph_read       graph_create       graph_validate
graph_update     graph_delete     graph_run          run_status
run_pause        run_resume       run_stop           artifact_read
plugin_list      plugin_read      session_wait       session_ask
run_list
```

这些工具是 Anchor Runtime 的 API 契约。Pilot 可以决定调用顺序和参数，但不能绕过权限、Session、Run 或审计边界。高影响动作（启动、删除、修改研究合同、停止运行）是否需要用户确认由产品策略决定，并必须在 Session 中留下事件。

## Session、对话和 Run

### 两层状态

Session 和 Run 不能合并：

- **Session** 是用户看见的连续关系，保存用户消息、Pilot 回复、确认、状态事件以及关联 Run。
- **Run** 是一次 Graph 执行，保存调度游标、节点工作区、PydanticAI 消息、工具轨迹和 commit。

一个 Session 可以启动多个 Run；一个 Run 只能归属于一个 Session，但普通 Graph 也可以从 API 或 WebUI 直接运行而不绑定 Session。

目标状态分为两份权威事实：PydanticAI Harness `SqliteConversationStore` 保存 Pilot 的模型消息和会话摘要；Anchor 自己只保存生命周期、控制状态和它关联的 Graph Run。消息不再额外复制进 `events.jsonl`。

```text
<Anchor 数据根目录>/
├── sessions/<session-id>/
│   ├── session.json       # Anchor 生命周期、等待原因、关联的 Graph Run ID
│   └── events.jsonl       # 可重放的 Anchor 活动事件，不复制模型消息
├── state/
│   └── pilot-conversations.sqlite # Harness 会话头、PydanticAI 消息与媒体
├── library/
└── workspaces/<graph-id>/
    ├── graph.json
    └── runs/<run-id>/
        ├── run.json
        ├── graph.json
        ├── *.trace.jsonl
        └── <node-id>/...
```

`session.json` 与 `events.jsonl` 的职责必须有限：前者只保留 Anchor 状态和关联，后者记录 Graph/Run 启动、进度、确认、停止、结果引用等产品事件。完整对话只保存在 Harness conversation store；节点内部模型上下文只保存在节点恢复记录。界面将这些事实组合成统一时间线，不把投影再保存成新的事实。

`SqliteConversationStore` 是当前安装的 `pydantic-ai-harness` 已提供的实现：消息通过 `ModelMessagesTypeAdapter` 编解码，可检索会话摘要、按文本搜索、做 revision compare-and-swap，并把较大的媒体放到 SQLite media store。其 deletion 也能在同一 SQLite 数据库内删除 Harness 自己关联的 step records。Pilot 优先复用这些能力；Anchor 不应再实现第二份模型消息存储、标题索引或并发修订机制。

Harness 会话存储的 `run_id`/`outcome` 是其当前对话执行元数据，不能代替 Anchor 的多 Graph Run 关联和状态机；Anchor 生命周期仍由 `session.json`/Anchor 活动事件与 Graph Run `run.json` 管理。Anchor 不直接读写 Harness 的私有 SQL 表。两个存储之间写入失败时，以 Anchor 事件与恢复边界明确协调，不谎称跨 SQLite/文件系统事务原子化。

上述 Session 文件布局、Harness 消息存储、Pilot 消息收发、失败后续答和停止已接入。Pilot 在调用模型前保存用户输入，在整轮成功后保存模型消息；中途工具执行记录的恢复、进程退出后的副作用判定和流式 UI 尚未完成验收。Harness 提供 revision compare-and-swap，但不等于 Anchor 的 Session 文件更新具有事务保护。Harness 当前为 0.x 版本，依赖已固定为 `0.32.0`；升级前必须重验文件兼容、revision 冲突、删除语义及进程恢复。SQLite 会话存储是单机基础设施，不是多主或多机 Session 锁。

节点 trace 是模型消息和工具调用的技术记录；前端把 Harness 对话、Anchor 活动事件和 Run 状态组合为界面，但不能把前端状态当作事实来源。

### Session 状态

最小状态集合：

```text
active → waiting_user → active
active → running → active
active/running → interrupted → resume
active/running → archived
```

目标状态中，`waiting_user` 表示 Pilot 已经形成一个需要用户回答的问题或确认，后端不应继续猜测。当前 `session_ask` 工具会持久化问题并设置该状态，用户回答可继续同一 Session；但普通工具返回并不会暂停本轮模型，真正的 deferred tool 等待边界仍待接入。普通 Graph 节点的外部输入协议也未统一。

Run 状态仍由调度器维护（例如 `running`、`paused`、`stopped`、`finished`、`failed`、`uncertain`），Session 只引用和解释这些状态，不复制一份独立的 Run 真相。

### PydanticAI 的边界

PydanticAI 是 Pilot 和 AgentNode 的模型交互内核，负责：

- 模型请求与响应；
- 结构化工具参数；
- 流式事件；
- `message_history` / `conversation_id`；
- 单个 AgentNode 的模型消息恢复。

Anchor Runtime 负责：

- Session 持久化与事件顺序；
- Graph、Run、Workspace、Git、沙箱和 Plugin；
- 用户确认、`waiting_user` 和跨 Graph 调度；
- 权限、幂等、停止、恢复和审计。

不使用 `pydantic_graph` 替代 Anchor Graph。它可以在 Pilot 内部表达小型类型化决策流程，但不能成为第二套拥有工作区、Git、沙箱和 Run 生命周期的 Graph。

### AgentNode 的工具与完成协议

当前 AgentNode 以 `bash` 作为工作区工具，并通过 PydanticAI 结构化输出完成节点。没有 Bash 调用也可以完成；模型不再通过 `anchor-done` / `anchor-route` 完成节点。

它解决了真实问题：Graph 不能把任意一句“完成了”当成调度事实；多出口节点必须选择合法路由；提交记录必须在运行恢复时可判定。旧实现将这些不变量绑在 Bash 和 shell 命令上，当前 AgentNode 已改为结构化完成；Pilot 与普通 AgentNode 的完整生命周期统一仍是后续工作。

目标是**统一 AgentNode 契约，而不是给 Pilot 开特例或放弃显式完成**：

1. AgentNode 可使用零个或多个工具。Bash 是 AgentNode 可用的工作区能力，不是完成节点的强制仪式；Plugin 可向模型暴露有名称和参数 schema 的结构化工具。
2. 工具调用和节点完成是两种不同的事实。PydanticAI 的最终结构化结果形成 Anchor `NodeOutcome`；没有工具调用也可以完成，但绝不从“工具调用过”“写过文件”或任意中间消息推断完成。Runtime 必须先把该结果写入 Anchor 可恢复记录，再把控制权交还调度器。
3. 最终结果至少明确表示提交内容，并在有多个合法后继时提供其中一个合法 route。只有一个后继时由 Graph 决定；没有合法 route、结果不完整、模型中断或运行不确定时，Runtime 不推进边。
4. 只回答用户的 AgentNode 可以返回文本结果，不必创建伪造文件。要求交付文件或证据的 Graph 由拓扑、声明和专门验证节点表达质量与证据判据；模型说“完成”本身不是质量证明。
5. 请求用户补充输入或批准不是普通完成。Runtime 必须将它记录为可恢复的 `waiting_user` 状态；用户输入到达后恢复同一 Session/逻辑工作，而不是让节点在 HTTP 请求中阻塞等待。
6. Pilot 和普通 AgentNode 使用相同的模型、工具、结果、取消与恢复契约。Pilot 的 Session 可以在一次 AgentNode 结果后继续存在；某个节点结果结束不等于用户 Session 或被启动的子 Graph Run 已结束。
7. 所有有副作用的 Plugin 工具仍经 Anchor 控制的执行器及其沙箱/权限边界运行。把 Python 函数注册成 PydanticAI Tool 不应自动赋予它宿主机权限。

这让显式的 Anchor 结果协议取代“必须运行 shell 命令”作为 Graph 的终结信号，同时保留明确状态、合法路由、运行记录和崩溃恢复所需的事实。模型最终结果、Anchor 完成事实、Git freeze、Graph 边决策必须有明确的恢复顺序；不能把 PydanticAI 消息 checkpoint 当成 Graph completion checkpoint。验收须注入“结果已持久化但尚未 freeze”的进程退出，恢复应从已记录结果继续 freeze/settle，不再调用模型或重做工具副作用。

Anchor 仍处于开发阶段，不为已被替代的内部 AgentNode 协议保留兼容分支。改造时同步更新所有受维护的 Graph 示例、测试和本地演示 Graph；旧协议代码与测试一起删除，不留下双轨运行。对已有用户工作区不做静默改写或删除；如果格式不匹配，新运行应清楚拒绝并给出显式重建/迁移指引，但不因此保留旧执行器。

### Pydantic 体系的使用策略

Anchor 不追求“所有类都改成 Pydantic”。正确的分层是：

| 层 | Pydantic 适合负责什么 | Anchor 的使用方式 |
| --- | --- | --- |
| Pydantic BaseModel / TypeAdapter | 外部输入、持久化 JSON、事件、配置和结构化结果的校验 | Session、控制 API、事件和运行快照的公共边界应逐步采用；文件事实仍由 Anchor 管理 |
| PydanticAI Agent | 模型、工具、依赖、输出校验、消息历史和流式迭代 | AgentNode 和 Anchor Pilot 的模型交互内核 |
| PydanticAI messages | 可恢复的模型请求、响应和工具配对 | 节点 trace 与 Pilot 对话历史的模型层记录 |
| pydantic-ai-harness | 上下文压缩、步骤持久化、会话头、消息媒体存储、技能渐进披露 | 单个 Agent 的对话与步骤基础设施；不承担 Anchor Graph 状态 |
| PydanticAI Toolsets / UI adapters | 工具分组、延迟发现、前端协议事件映射与工具确认 | Pilot 可复用结构化工具和事件能力；协议需与 Anchor 前端/HTTP 栈适配 |
| pydantic-graph | 一个进程内的类型化小流程 | 仅在 Pilot 内部有明确收益时使用，不替代 Anchor Graph |
| PydanticAI durable execution integrations | Temporal、Prefect、DBOS 等外部持久执行协调 | 当前文件工作区调度没有这些平台依赖，不引入第二个 Run 调度器 |
| pydantic-settings | 环境变量和本地配置解析 | 仅在配置来源变复杂时引入；当前运行配置仍由现有 JSON/secret 机制负责 |
| pydantic-evals | 模型输出和 Agent 行为评估 | 作为研究质量与 Pilot 验收工具，不作为生产运行状态机 |

在当前开发环境中，`pydantic-evals`、`pydantic-settings` 和 `pydantic-graph` 可导入（其中部分为传递依赖），但 Anchor 没有直接使用它们；不可因环境中安装了包就让它们成为 Anchor 架构依赖。`ag-ui-protocol` 未安装。应以 `pyproject.toml` 的直接依赖和可复现安装结果为准。

当前实现的事实是：`Agent`、`RunContext`、工具、输出验证器、`agent.iter`、`message_history`、`ModelMessagesTypeAdapter` 已使用；普通 AgentNode 接入 `conversation_id` 与 `StepPersistence`。Compaction 在节点层有实现和测试，默认 Graph 路径与 Pilot 尚未启用。Pilot 已接入 PydanticAI 普通对话、Harness 消息持久化、WebUI 会话列表、中断后续答、取消当前模型调用，以及显式 Anchor 控制工具（Graph、Run、Plugin、产物查询和 Graph/Run 控制）。它已通过 turn API 贯通 Session 对应的执行身份（每次执行用新的 `run_id`），用 `run_stream_events` 加 Vercel AI 事件流驱动增量文本与工具卡片（含 `tool-approval-request`），并把事件按序号落库供 SSE 重连。需要审批的工具用 `requires_approval` 声明，运行以 `DeferredToolRequests` 暂停、以 `DeferredToolResults` 恢复；`session_ask` 用 `CallDeferred` 暂停并把用户的回答作为该调用的结果返回。所固定的 Harness 版本还带有 `SqliteConversationStore`、技能目录的 deferred capability loader。PydanticAI 还有 AG-UI adapter 与 deferred-loading toolsets 尚未采用；Anchor 可按产品需要逐步采用，但不能把框架能力本身说成已落地功能。

相反，Graph 解析、节点边界、运行状态和恢复引用仍以 dataclass/手写 JSON 为主；`anchor.domain.models` 中部分历史 Pydantic 模型没有调用方，且 `Run.graph_version_id` 与“Graph 就是可编辑 JSON、不做 Graph 版本平台”的现行决定不一致。它们不应被直接当作 Pilot / Graph 的新权威模型，应在涉及该域时再清理或迁移。

下一步不是全面重写，而是只把跨模块、跨进程的稳定契约迁移到 Pydantic：

1. **Session 对话基础已接入**：Harness `SqliteConversationStore` 保存消息；`session.json` / `events.jsonl` 保存 Anchor 生命周期。API 提供会话创建、列表、读取、消息发送、失败后续答、事件、状态、Run 关联、确认和删除；WebUI 提供创建、选择、对话、停止、续答和确认入口。确认并发、异常恢复与浏览器端到端验收仍待补齐；流式输出仍待实现。
2. 用 PydanticAI `run_stream_events` 提供模型/工具事件；Anchor 把必要的产品生命周期事件持久化。仅在能减少前端适配代码时，才采用 Vercel AI 或 AG-UI adapter，并先核对其依赖和当前 HTTP server 的代价。
3. 用 PydanticAI 的 deferred tool approval 表达“模型提出动作、用户批准后继续”；但批准记录、等待状态和恢复请求归 Anchor Session，不能只留在内存事件流。
4. 先为 AgentNode 定义工具无关的结构化终结结果与多出口 route 校验；为 final result 崩溃边界补恢复记录和故障注入测试，并在同一改动中把维护中的 Graph 示例和测试切换到新协议，删除 `anchor-done` / `anchor-route` Agent 协议实现。
5. 用 Pydantic FunctionToolset / 结构化函数工具实现 Anchor 控制操作；入参用 schema 校验，Runtime 再做权限和状态校验。Plugin 工具经受控执行器执行，不允许工具函数意外获得宿主权限。
6. 为需要外部输入的节点定义持久 `waiting_user` → resume 转换，明确它与一轮用户对话完成、Graph Run 完成之间的差异。
7. 只把跨模块、跨进程的稳定 Anchor 契约迁移到 Pydantic；内部调度 dataclass 保持不变，Graph/Run 文件格式由 Anchor 校验器负责，不让 Pydantic 对象扩散到沙箱热路径。
8. 先复用 PydanticAI deferred capability 原语实现 Plugin 渐进披露；不直接套 Harness `Skills` 来替代 Plugin，因为 Skills 只装载 `SKILL.md` 指令，不管理 Anchor 的知识附件、工具环境、沙箱挂载和资源摘要。

这样可以获得结构化校验、清晰的 API 契约和可恢复消息，而不会出现“两套 Graph、两套状态机”或为每个内部对象增加序列化负担。

## Graph、工作区和 Git

Graph 仍然就是一个 JSON 文件，不建立不可变 Graph 版本平台。保存 Graph 是编辑当前资产；运行时把当时的 Graph 定义写入 Run 目录，作为该次运行的事实快照。

每次 Run 创建一个新的大工作区，下面再为每个 AgentNode/OpNode 创建自己的节点工作区。不同 Run 不共享可写文件；同一 Run 的反馈循环复用节点工作区并继续自己的 Git 历史。

节点完成后由 Anchor 在沙箱外创建 commit。下游收到的是上游 commit 标识，Runtime 将该 commit 的文件树和只读 `.git` 历史挂载到 `/in/<node>`：

- HEAD 固定在指定 commit；
- 可查看该 commit 及祖先的 `log/show/diff`；
- 不暴露上游之后的提交或无关分支；
- 下游默认读取快照，需要时再主动查询历史；
- 上游工作区后续变化不会影响已交付输入。

commit 是运行事实和输入快照标识，不是 Graph 版本，也不要求 Agent 理解 Git 才能完成任务。

## Plugin 与能力积累

Plugin 是挂载给 AgentNode 的能力资产，包含名称、描述、说明、可选知识资源和工具引用。它不是所有系统对象的垃圾桶，也不是第三种执行节点。

运行时向 Agent 提供短目录：名称、描述和说明入口；完整内容保留在唯一 Plugin 目录，按需只读读取。工具可以使用独立环境，环境不复制到节点工作区，也不要求所有工具共享一个 Python 版本。

Plugin 的运行绑定摘要写入 Run，记录当时解析到的来源和内容摘要。恢复时若来源已改变，Runtime 必须拒绝静默继续并说明原因；不能用当前 Plugin 内容覆盖历史事实。

能力资产的积累路径是：

```text
一次任务中的有效做法
  → 用户/开发者整理为 Plugin 说明与工具入口
  → Plugin 被多个 AgentNode 挂载
  → Graph 通过这些 AgentNode 形成可复用流程
  → Run 的证据和结果反过来验证 Plugin 是否有用
```

知识库是后续能力，不先冻结形式。接入时必须保持来源定位、证据保留和唯一查询入口，不能让每个 Plugin 私自发明一套不可追溯的索引。

## 研究合同与深度研究

深度学术调研是普通 Graph，通过挂载 `academic-research` Plugin 获得文献检索和核验证据能力。它不是特殊 Agent 内核，也不是固定主题脚本。

目标用户旅程：

```text
用户提出方向
  → Pilot 澄清目标、范围、受众、证据标准和结束条件
  → 生成 research-contract.md
  → 用户确认或修改合同
  → Pilot 选择或构建普通研究 Graph
  → 保存 Graph 并启动 Run
  → 研究、反馈、证伪、重新框架化
  → Pilot 读取证据和产物，请用户验收
```

研究合同至少记录核心问题、范围、排除范围、交付形式、证据标准、结论边界、结束条件和修改方式。研究过程中证据推翻原问题时，Graph 可以停在 `waiting_user` 请求合同修改；不得偷偷改变题目并把它当作原目标的完成。

研究完成来自目标、证据、反馈和用户验收。Graph 可以有反馈边，但固定轮数、请求次数或 `max_xxx` 不能作为“已经理解”或“研究已经收敛”的替代品。

## API 与用户界面契约

WebUI 和 Pilot 使用同一套资源 API。首期控制面最小契约如下：

```text
GET    /sessions
POST   /sessions
GET    /sessions/<id>
GET    /sessions/<id>/messages
GET    /sessions/<id>/events
POST   /sessions/<id>/turns
GET    /sessions/<id>/turns
GET    /sessions/<id>/turns/<turn>/events
POST   /sessions/<id>/messages
POST   /sessions/<id>/resume
POST   /sessions/<id>/stop
POST   /sessions/<id>/status
POST   /sessions/<id>/confirm
POST   /sessions/<id>/reject
POST   /sessions/<id>/runs
DELETE /sessions/<id>

GET    /graphs
GET    /graphs/<name>
POST   /graphs
PUT    /graphs/<name>
DELETE /graphs/<name>
POST   /trigger

GET    /runs
GET    /runs/<id>
POST   /runs/<id>/pause
POST   /runs/<id>/resume
POST   /runs/<id>/stop
DELETE /runs/<id>
GET    /runs/<id>/files/<node>
GET    /runs/<id>/files/<node>/<path>

GET    /plugins
GET    /plugins/<id>
```

接口返回稳定的 ID、状态和错误，不返回需要前端猜测的隐式状态。流式体验可以基于 SSE 或 WebSocket，但事件必须最终落到 Session/Run 的文件事实中；断线重连通过事件序号继续，而不是依赖浏览器内存。

## 信任边界和失败语义

- Pilot 的模型输出不是权限凭证；所有控制动作由 Runtime 再校验。
- Plugin 说明不是安全策略；工具挂载、网络、文件访问和删除权限由 Runtime 强制执行。
- Session 事件追加必须保持顺序，重复请求要有幂等键或明确返回已处理结果。
- 不能判断副作用是否发生时，Run 进入 `uncertain`，由 Pilot 请求用户处理，不能自动重跑并假设没发生。
- 服务重启后恢复的是已记录的状态和可恢复的模型对话；正在执行的外部副作用不承诺 exactly-once。
- 普通 Graph 删除必须拒绝仍在运行的 Graph；Pilot 删除永远拒绝。
- 默认服务面向本机，正式多用户部署前必须增加身份、授权和 Session 隔离，不能把本地路径模型直接当成多租户安全模型。

## 阶段性实施顺序

本轮交付范围（2026-09-26）：完成下列可复现用户路径，不扩展暂缓的知识库编译与自动环境安装。

| 工作包 | 占比 | 验收边界 |
| --- | ---: | --- |
| Session 与操作恢复 | 35% | 并发无覆盖、请求去重、确认展示完整内容与拒绝、重启后未知副作用禁止自动重放 |
| 系统 Pilot 与研究合同 | 25% | 系统定义参与实际 Pilot 执行且不可普通修改/删除；合同确认、运行关联、修改等待与结果验收 |
| 流式 UI 与等待恢复 | 20% | 增量文本、事件游标重连、刷新后恢复状态、确认/拒绝/回答继续同一 Session |
| 集成与验收 | 20% | 真实 HTTP 与浏览器流程、故障注入、负向权限检查、现行文档同步 |

这些工作包共享 Session 协议，当前顺序实施：Session 事务与操作回执 → 控制工具/系统定义/合同 → 流式 UI → 故障与浏览器验收。先冻结以下边界：消息只存 Harness；Session 文件保存生命周期、请求 ID、待确认提案及操作回执；确认绑定完整提案与资源前态；不确定操作必须人工明确处置；浏览器断开不取消服务端任务。任何新增流式文本缓存均为可重建投影，不成为第二份对话事实。

这是一条收敛路径，不是同时铺开的一堆功能：

1. **架构契约**：保留本文为唯一产品架构真相，补充 Pilot 系统 Graph 标记和禁止删除的后端测试。
2. **统一 AgentNode 终结契约**：支持工具无关的结构化结果、无工具直接回复和多出口路由校验；同步更新维护中的 Graph 示例和测试，删除旧 Bash sentinel 完成协议，并完成最终结果的崩溃恢复验证。
3. **Session 对话（基础已接入）**：已有生命周期、Harness 消息持久化、普通对话 UI、失败后续答、Run 关联、删除和确认存储；仍需补齐确认并发与进程恢复验收。
4. **Pilot 控制面（基础已接入，未完成产品验收）**：Pilot 已通过 PydanticAI 结构化工具接入 Graph/Run/Plugin 查询、产物读取、运行启动与控制；动作调用 Scheduler，Graph 创建、修改、删除检查 Session 授权。确认机制的剩余实现缺口见[当前架构](architecture.md)。流式事件和系统 Pilot Graph 尚未实现。
5. **已有 Graph 入口**：Pilot 已能查询现有 Graph、启动和控制 Run；进度展示与通用 `waiting_user` 恢复仍待实现。
6. **Graph 构建入口**：Pilot 能生成、校验、保存普通 `graph.json`，在启动前展示合同和图摘要。
7. **研究闭环**：接入 research contract、研究结果验收、证据推翻后的用户确认和可恢复长任务。
8. **真实产品验收**：浏览器关闭、服务重启、网络断开、节点中断、重复消息、停止和不确定副作用都走一遍真实路径。

每一阶段必须同时更新当前架构、使用说明、测试和已知边界。没有真实 E2E 的接口不能描述成已完成产品能力。

## 明确不做

- 不增加临时 Graph 类型；Pilot 创建的就是普通 Graph。
- 不把所有能力、工具和外部交互都抽象成 OpNode 或一个巨大的 Plugin。
- 不用 PydanticAI Graph 替换 Anchor Graph。
- 不建立无需求支撑的 Graph 发布版本、不可变模型版本或模型版本平台。
- 不把隐藏提示词当作控制面 API。
- 不把前端缓存、模型上下文或 trace 摘要当作唯一事实。
- 不用固定轮数、固定请求数或人工 `max_xxx` 判断复杂任务完成。

2026-09-26 的逐项接入证据与最低交互验收标准见 [Pilot 对话体验与 Pydantic 接入核查](pilot-experience-audit.md)。

后续开发顺序、共享契约和验收状态统一维护在 [Pilot 开发计划](pilot-development-plan.md)。
