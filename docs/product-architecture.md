# Anchor 产品与系统架构

状态：**产品真相与目标边界**。未冻结的部分只作为产品需求记录，不作为当前实现指导。

本文记录 Anchor 当前已经确认的产品哲学和对象边界。它回答“Anchor 要成为什么，以及各部分为什么这样协作”；代码实现的当前细节见 [当前架构](architecture.md)，用户操作见 [使用指南](usage.md)，当前开发顺序和验收见 [开发台账](pilot-development-plan.md)。

本文不是把未来功能写成已完成的功能。每个章节都明确当前状态和目标状态；实现开始前先更新这里的契约，完成后再更新当前架构和验收记录。

2026-09-26 收敛决定：用户要的是保存 Agent 工作记录、重开原会话后继续工作。恢复以持续写入的工作 JSONL 为依据，由 Agent 查询现场、检查文件和运行测试后续做；不建设复杂审批平台、跨存储事务或未知结果人工处置平台。开发顺序与验收状态只维护在 [Pilot 开发计划](pilot-development-plan.md)。

进一步约定：先找 PydanticAI / Harness 的现有接口，接入即可，不预先设计自有记录和恢复框架。框架原生 FileStepStore 使用 JSONL 事件加消息快照等文件，按原生格式保留；不为单文件形式重写存储。记忆与计划不列为 Anchor 自研模块，研究目标与证据验收属于研究 Graph / Plugin 的业务。

## 产品方向

Anchor 是一个以 Graph 组织 Agent 工作、以文件系统保存事实、以 Git 保存可追溯快照、以沙箱保护执行边界的 Agent 产品运行时。

用户最终可以与 Anchor Pilot 对话，理解目标、选择或构建 Graph、启动运行、查看产物。WebUI 继续承担观察、编辑和人工接管职责。

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

系统级 Pilot 是保留的产品目标，它可以使用其他 Graph，但不取代其他 Graph。是否以及如何把当前 Pilot 接入普通 Graph 执行链，待当前目标完成后讨论，不作为本轮迁移任务。

## 不可动摇的设计原则

1. **唯一事实来源**：Graph 定义只在 `graph.json`；Plugin、工具和环境只在能力库；Session、Run 和节点历史各自保存自己负责的事实。派生摘要必须能回到来源。
2. **极简对象模型**：Graph、AgentNode、OpNode、Plugin、Run、Session 六类对象足够表达产品。不要为了形式主义增加 Graph 版本平台、临时 Graph 类型或第二套 Agent 内核。
3. **能力渐进披露**：Plugin 先给 Agent 名称和短描述，需要时再读取说明、知识和工具用法。能力挂载可见，实际调用另有运行记录。
4. **智能与机械事实分工**：Agent 负责理解、规划、核查现场和决定如何继续；代码负责保存和加载记录、权限、路径、提交及请求去重，不替 Agent 建立业务恢复决策平台。
5. **用户始终可控**：运行可以暂停、恢复、停止；需要用户决定时进入 `waiting_user`；不使用固定 `max_xxx` 伪装研究质量或收敛。资源预算是用户明确的运行约束，不是任务完成判据。
6. **恢复是正常路径**：用户消息、模型输出、工具调用与结果及时保存。进程退出后加载已保存的历史，缺少结果如实标明，允许 Agent 核查后继续；不以外部操作 exactly-once 为目标。
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
| Anchor Pilot | 对话控制入口；系统级保护为后续产品需求 | 当前由 Pilot Agent 与 Session 承载 | 系统 Graph 的存储和生命周期接法待讨论 |

Agent 的共享 `agents` 配置只是模型、指令、权限和读写声明的复用配置，不是独立的 Agent 资产库。Plugin 直接挂到 AgentNode，不通过角色继承、默认合并或复制实现。

## Anchor Pilot（产品需求记录，未冻结）

用户希望系统级 Pilot 持续可用，并能通过对话管理工作流。系统保护、控制能力的组织方式及它与普通 Graph 的生命周期关系，留待当前续聊目标完成后再讨论；不预定系统目录、注册字段、控制 Plugin 或升级机制。

当前代码中的 Pilot 是可用的 PydanticAI Agent 和 Session 入口；它不是已经确定的系统 Pilot Graph，也不代表上面这些产品决策已经完成。

## Session、对话和 Run

### 两层状态

Session 和 Run 不能合并：

- **Session** 是用户看见的连续关系，保存用户消息、Pilot 回复、确认、状态事件以及关联 Run。
- **Run** 是一次 Graph 执行，保存调度游标、节点工作区、PydanticAI 消息、工具轨迹和 commit。

一个 Session 可以启动多个 Run；一个 Run 只能归属于一个 Session，但普通 Graph 也可以从 API 或 WebUI 直接运行而不绑定 Session。

当前实现把 Pilot 模型消息、工作记录、Session 生命周期和 turn/SSE 传输分开保存。下图是当前存储：工作记录是 Harness 原生文件格式，续聊按它恢复。

```text
<Anchor 数据根目录>/
├── sessions/<session-id>/
│   ├── session.json       # Anchor 生命周期、等待原因、关联的 Graph Run ID
│   └── events.jsonl       # 可重放的 Anchor 活动事件，不复制模型消息
├── state/
│   ├── pilot-conversations.sqlite # Harness 会话头、PydanticAI 消息与媒体
│   ├── pilot-steps/               # Harness FileStepStore：每个执行的 JSONL 事件、工具效果、消息快照与媒体
│   └── pilot-turns.sqlite         # 提交去重、执行状态与 SSE 游标
├── library/
└── workspaces/<graph-id>/
    ├── graph.json
    └── runs/<run-id>/
        ├── run.json
        ├── graph.json
        ├── *.trace.jsonl
        └── <node-id>/...
```

当前 `session.json` 保留 Anchor 状态、关联和审批/操作账本，`events.jsonl` 记录产品活动，不能仅靠这个活动日志重建模型历史。模型对话目前在 Harness conversation store；节点内部记录保存在节点恢复目录。界面呈现这些事实，不是恢复依据。

`SqliteConversationStore` 是当前安装的 `pydantic-ai-harness` 已提供的实现，消息通过 `ModelMessagesTypeAdapter` 编解码。框架的 StepPersistence、FileStepStore / SqliteStepStore 和 `continue_run` 已提供记录与历史加载能力；当前目标是接通公开接口，不另写日志格式、存储引擎或恢复协议。

Harness 的 `run_id`/`outcome` 不能代替 Anchor 的多 Graph Run 关联。Run 状态和产物继续以实际运行目录为准；工作记录保存 Agent 曾经观察和尝试的内容。恢复时由 Agent 对照现场核查，不要求模型历史、界面状态与外部操作形成原子事务，也不直接读写 Harness 的私有 SQL 表。

上述 Session、消息存储、流式 UI、必要提问和「重开原会话后继续」已有实现；代码回归覆盖带新消息与空 prompt 的原生快照续聊，真实 provider 的空 prompt、长会话压缩和完整浏览器复验按开发台账单独记录，未提前视为通过。验收范围是本机单进程；多进程与多租户不在其中。固定依赖版本见 `pyproject.toml`，升级时再验记录兼容性。

节点 trace 是模型消息和工具调用的技术记录；前端把 Harness 对话、Anchor 活动事件和 Run 状态组合为界面，但不能把前端状态当作事实来源。

### 工作记录与续聊目标

当前续聊目标是：保存工作记录；重新打开 Session；让 Agent 读取历史并查询现场；然后继续任务。具体接线、验证顺序和是否需要最小适配只记录在 [开发台账](pilot-development-plan.md)，不在产品架构中另行规定。

验收用实际杀进程、重启和续聊验证，不能用「已有日志文件」或旧审批测试通过代替。此目标不要求更换 Graph 调度器，也不改变沙箱和文件权限。

### Session 状态

最小状态集合：

```text
active → waiting_user → active
active → running → active
active/running → interrupted → resume
active/running → archived
```

`waiting_user` 表示 Pilot 需要用户回答问题，后端不应继续猜测。当前 `session_ask` 已通过 `CallDeferred` 真正暂停，回答作为原工具调用的结果返回。`interrupted` 允许加载历史后继续对话：用户可以直接发新消息，旧副作用门禁已删除，AI 核查现场后继续。普通 Graph 节点的外部输入协议尚未统一。

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

不使用 `pydantic_graph` 替代 Anchor Graph，不引入第二套拥有工作区、Git、沙箱和 Run 生命周期的 Graph。

### AgentNode 的工具与完成协议

当前 AgentNode 以 `bash` 作为工作区工具，并通过 PydanticAI 结构化输出完成节点。没有 Bash 调用也可以完成；模型不再通过 `anchor-done` / `anchor-route` 完成节点。

Graph 的节点完成、模型工具调用和文件质量是不同的事实。结构化结果与合法路由仍由现有节点契约负责，文件操作仍受沙箱约束；保存模型历史不能替代 Graph 的运行记录。

Pilot 与普通 AgentNode 的生命周期统一、普通节点等待用户输入、Plugin 结构化工具等保留为后续需求。当前不规定适配器迁移、状态机或检查点顺序，也不把它们作为 Pilot 续聊的前置工作。

### Pydantic 体系的使用策略

本项目的已确认原则只有三条：

- Agent 的模型消息、工具、流式事件和步骤记录优先使用 PydanticAI / Harness 的公开接口。
- Anchor 继续负责 Graph、Run、Session、文件、Git、沙箱、权限和产品事实。
- 上下文压缩直接复用 Harness，具体配置在接入时核对。其他组件、UI adapter 和计划呈现留待当前目标完成后讨论；环境中可导入的包不自动成为架构依赖。

当前实现已使用 `Agent`、工具、输出校验、`message_history`、流式事件、`SqliteConversationStore` 和步骤持久化。Pilot 的中断工作记录续聊仍未完成真实验收；框架已有的其他能力不能据此写成 Anchor 已交付功能。

相反，Graph 解析、节点边界、运行状态和恢复引用仍以 dataclass/手写 JSON 为主；`anchor.domain.models` 中部分历史 Pydantic 模型没有调用方，且 `Run.graph_version_id` 与“Graph 就是可编辑 JSON、不做 Graph 版本平台”的现行决定不一致。它们不应被直接当作 Pilot / Graph 的新权威模型，应在涉及该域时再清理或迁移。

具体缺口、先后顺序与验收只见 [Pilot 开发计划](pilot-development-plan.md)，本节不再维护第二份实现路线。

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

## 研究应用需求（待讨论，不属于 Anchor 核心机制）

已有深度学术调研示例通过普通 Graph 和 `academic-research` Plugin 工作。后续产品需求是让研究目标清楚、重要变更可追溯、结论有证据、用户能检查交付。这些属于研究应用，不是 Anchor 通用运行时的职责或续聊前置条件。

是否需要称为“研究合同”的独立对象、如何记录目标、何时询问用户以及怎样呈现证据验收，均未确定。当前不指定文件名、字段、暂停状态或审批流程，待本轮完成后讨论。

研究完成来自目标、证据、反馈和用户验收。Graph 可以有反馈边，但固定轮数、请求次数或 `max_xxx` 不能作为“已经理解”或“研究已经收敛”的替代品。

## API 与用户界面契约

WebUI 和 Pilot 使用同一套资源 API。下列为当前控制面入口；`confirm`/`reject` 只服务于仍然需要用户决定的操作（当前是删除 Graph）。

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

接口返回稳定的 ID、状态和错误。当前 Pilot 使用持久化事件和 SSE 游标重连；聊天中的 Graph、Run、Artifact 引用以 `#anchor/...` 链接直接打开已有页面并可返回原会话，不增加对象视图系统。更深的交互联动保留为后续需求。

## 信任边界和失败语义

- Pilot 的模型输出不是权限凭证；所有控制动作由 Runtime 再校验。
- Plugin 说明不是安全策略；工具挂载、网络、文件访问和删除权限由 Runtime 强制执行。
- Session 事件追加必须保持顺序，重复请求要有幂等键或明确返回已处理结果。
- 工具结果缺失时保留中断事实，Pilot 可查询实际 Run、文件或测试结果后继续。现有 Run 的 `uncertain` 状态可以作为核查线索，不因此要求一套人工处置平台，也不默认重发旧命令。
- 服务重启后恢复的是已记录的状态和可恢复的模型对话；正在执行的外部副作用不承诺 exactly-once。
- 普通 Graph 删除必须拒绝仍在运行的 Graph；系统 Pilot 的保护范围与接法待后续讨论。
- 默认服务面向本机，正式多用户部署前必须增加身份、授权和 Session 隔离，不能把本地路径模型直接当成多租户安全模型。

## 阶段性实施顺序

唯一开发范围与 A01–A14 验收状态见 [Pilot 开发计划](pilot-development-plan.md)。当前接通框架工作记录和续聊，保留必要提问，按需接框架压缩，补上聊天对象跳转并做真实验收。系统 Pilot、可见计划、研究业务、附件、分支和导出等只保留产品需求，当前目标完成后再与用户讨论决定，不自动进入下一阶段。

P1 流式与提交去重已完成；P2 按 2026-09-26 的用户决定收敛，Framework 记录、重开续聊、必要提问、按需压缩与对象跳转都已通过真实 provider / 杀进程 / 浏览器验收（状态与证据见开发台账 A07–A12）。跨存储事务和未知结果人工处置不再是任何阶段的前置条件。没有真实验收的能力不描述成已经交付。

## 明确不做

- 不增加临时 Graph 类型；Pilot 创建的就是普通 Graph。
- 不把所有能力、工具和外部交互都抽象成 OpNode 或一个巨大的 Plugin。
- 不用 PydanticAI Graph 替换 Anchor Graph。
- 不建立无需求支撑的 Graph 发布版本、不可变模型版本或模型版本平台。
- 不把隐藏提示词当作控制面 API。
- 不把前端缓存、模型上下文或 trace 摘要当作唯一事实。
- 不用固定轮数、固定请求数或人工 `max_xxx` 判断复杂任务完成。
- 不扩建通用审批、跨存储原子事务或未知结果人工处置平台；不承诺任意外部操作 exactly-once。

P0 时的历史问题与框架核查见 [Pilot 对话体验与 Pydantic 接入核查](pilot-experience-audit.md)，其中旧待办不代替当前验收标准。

后续开发顺序、共享契约和验收状态统一维护在 [Pilot 开发计划](pilot-development-plan.md)。
