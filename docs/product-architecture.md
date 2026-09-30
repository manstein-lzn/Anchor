# Anchor 产品与系统架构

状态：**产品真相与目标边界**。未冻结的部分只作为产品需求记录，不作为当前实现指导。

本文记录 Anchor 当前已经确认的产品哲学和对象边界。它回答“Anchor 要成为什么，以及各部分为什么这样协作”；代码实现的当前细节见 [当前架构](architecture.md)，用户操作见 [使用指南](usage.md)，当前开发顺序和验收见 [开发台账](pilot-development-plan.md)。

本文不是把未来功能写成已完成的功能。每个章节都明确当前状态和目标状态；实现开始前先更新这里的契约，完成后再更新当前架构和验收记录。

2026-09-26 收敛决定：用户要的是保存 Agent 工作记录、重开原会话后继续工作。恢复以持续写入的工作 JSONL 为依据，由 Agent 查询现场、检查文件和运行测试后续做；不建设复杂审批平台、跨存储事务或未知结果人工处置平台。开发顺序与验收状态只维护在 [Pilot 开发计划](pilot-development-plan.md)。

进一步约定：先找 PydanticAI / Harness 的现有接口，接入即可，不预先设计自有记录和恢复框架。框架原生 FileStepStore 使用 JSONL 事件加消息快照等文件，按原生格式保留；不为单文件形式重写存储。记忆与计划不列为 Anchor 自研模块，研究目标与证据验收属于研究 Graph / Plugin 的业务。

## 产品方向

Anchor 是一个以 Graph 组织 Agent 工作、以文件系统保存事实、以 Git 保存可追溯快照、以沙箱保护执行边界的 Agent 产品运行时。

用户最终可以与 Anchor Pilot 对话，理解目标、选择或构建 Graph、启动运行、查看产物。WebUI 继续承担观察、编辑和人工接管职责。

长期产品方向：用户可以直接向 Anchor Copilot 提出任务；Copilot 根据意图选择已有 Graph，必要时临时构建 Graph 执行，并把结果沿原始交互来源返回给用户。Graph 有默认输入，单次 Run 可提供输入覆盖默认值。Anchor 通过常驻 HTTP 服务接受不同用户/调用方的消息并返回响应，对外 Agent 交互采用 OpenAI Responses 格式。这里的 Responses 是用户与 Copilot 的请求/响应协议；定时触发和第三方业务事件 Webhook 是其他触发入口，不应与对话 API 混为一谈。Responses 与 Webhook 均参考 OpenAI 的 Bearer API key 请求方式，使用 `Authorization: Bearer <anchor-key>`；Anchor 维护 `anchor-key` 白名单，匹配后允许请求继续执行，不增加角色审批机制。具体接口子集、密钥配置及调用方隔离见下文；这些均为目标契约，尚未实现。

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
2. **极简对象模型**：Graph、AgentNode、OpNode、Plugin、Run、Session 是当前核心对象。不要为了形式主义增加 Graph 版本平台或第二套 Agent 内核；Copilot 临时构建 Graph 的产品需求已提出，其是否需要独立生命周期尚未决定。
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

### 对外 HTTP、鉴权与 Responses API（冻结契约与当前接线）

外部调用方通过 HTTP 与 Anchor Copilot 交互，消息和响应采用 OpenAI Responses 格式；Anchor 内部如何把输入路由到 Copilot、Graph 和 Run 由 Anchor 自己定义。Anchor 实现 Responses 的常用兼容子集，不宣称完整兼容：仅提供 `POST /v1/responses`，固定模型名 `anchor-copilot`，接受文本 `input` 或仅含 user 消息的输入列表；支持 `previous_response_id` 续接同一对话及 `stream` 的普通 JSON / SSE 两种传输。SSE 发出 `response.created`、`response.in_progress`、文本增量和 `response.completed` / `response.failed` 终态事件。返回标准形状的 Response 对象和文本输出；外部工具定义/调用、图像/音频输入、任意模型名及其他未列明字段暂不支持，遇到不支持内容明确返回 400，不静默忽略。`previous_response_id` 不存在或不属于当前 key 时统一返回 404。Responses 是对话协议，不规定 Graph 的业务输入格式。

所有 HTTP API（包括已有管理 API、Responses 与 Webhook）统一要求 `Authorization: Bearer <anchor-key>`。服务通过环境变量 `ANCHOR_API_KEYS` 配置 JSON 字符串数组，例如 `["key-one","key-two"]`；key 应为至少 32 字节随机生成的秘密，只在服务启动时读取，增删密钥需重启。非空但格式错误、含空 key 或重复 key 时服务拒绝启动。空白配置在 loopback 本机开发模式允许无认证；监听非 loopback 地址时若没有至少一个 key，服务拒绝启动。key 精确匹配白名单即有完整 API 权限，不增加角色、逐请求审批或权限配置。缺失、格式错误或不匹配统一返回 401，不泄露哪一步失败；比较使用常量时间方式，密钥不得写入日志、Run、Session 或错误响应。密钥本身是粗粒度调用者边界，不提供用户资料或细粒度授权。

每个 Responses 会话归属于创建它的 key；续接时必须使用同一个 key，不能仅凭 `previous_response_id` 跨 key 读取对话。撤销 key 后，该 key 创建的对话不再能经 API 访问。Anchor 不据此承诺完整多租户隔离；一个 key 的所有持有者共享同一权限与会话可见范围。长请求通过 `stream:true` SSE 返回进度和文本，客户端断开不取消服务端已接受的处理；`stream:false` 等待最终 Response。Responses 返回的 `id` 可通过 `previous_response_id` 续聊，不额外设计 Anchor 专有会话 ID API。

Webhook 是 Graph Run 触发入口，与 Responses 分开：`POST /v1/webhooks/graphs/<graph-id>`，请求体为 `{"input": {…}}`，其中 `input` 是可选 JSON object，缺省按空对象处理；它作为本次 Run 输入，与 Graph 默认 input 按已确认的递归合并规则解析。端点只启动指定的既有 Graph，不接受调用方覆盖 Graph objective 或指定其他 Graph。每次合法到达独立触发，不做事件去重或自动重试；Graph 空闲时返回 202 和 `run`、`graph` 标识，Graph 正忙时返回 409 和当前运行 ID，不创建 Run。未知 Graph 返回 404，非法 JSON 或非 object 输入返回 400。客户端收到 409 后自行决定是否再次发送；Anchor 不保留请求、不排队。

当前已接入 `ANCHOR_API_KEYS`、Bearer 常量时间白名单校验、非 loopback 启动约束及 WebUI 会话级 key 输入。Responses/Webhook 已接入目标子集；官方字段兼容和真实 provider 端到端仍需验收，不能称为完整 Responses 兼容。

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

### Graph 默认输入与单次 Run 输入

Graph 用 `objective` 描述工作流的固定目的，并可声明 JSON object 形式的默认 `input`。单次 Run 可提供自己的 JSON object；Anchor 将它与默认输入递归合并：对象按键递归合并，数组和其他值（包括 `null`）整体由 Run 值替换；Run 输入可增加默认输入中没有的键。默认输入不是字段 schema，不限制运行时可增加的键。解析后的有效输入属于 Run 事实并随 Run 保存。

每个 AgentNode 在任务上下文中收到有效输入；OpNode 通过只读的 `ANCHOR_INPUT` JSON 环境变量取得同一份输入。解析后输入保存在 `run.json`；手动、定时、Webhook 统一经过同一合并规则。OpenAI Responses 的请求/响应兼容属于对外交互协议，不定义 Graph 的业务输入格式。

### Run 触发与时间线

触发方式包括手动、定时和业务 Webhook。定时支持一次性未来时刻，以及周期规则：固定间隔或本地日历规则（例如每天、每周指定星期、每月指定日期），按运行机器本地时间，不另配时区。Anchor 停机期间错过的计划不补跑；看板应明确显示错过，一次性计划结束，周期计划等待下个未来时点。

普通手动/API/Webhook 调度同一 Graph 同一时刻只允许一个 Run；通道绑定的独立用户会话允许同图并发。Graph 正忙时手动/API/Webhook 立即得到 409；定时触发错过该时点且不创建 Run。Anchor 停机期间错过的计划也不补跑。计划规则保存在 `state/schedules.json`，运行看板由计划规则和实际 `run.json` 事实派生；未来展示 7 天，历史按天分页，区分已计划、实际 Run、Graph 忙碌错过和停机错过。页面现已提供计划创建/删除和 Graph 输入入口；完整浏览器验收待完成。

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

## 多用户工作中枢与 RSI（2026-09-30 需求澄清，未冻结实现）

用户确认的目标：Anchor 作为工作中枢，通过企业微信等入口与多个用户并发沟通，由助手 Graph 的 AgentNode 调用不同 Plugin 查询数据和执行获授权的业务操作。每个用户独立维护对话、数据上下文和历史；工作记录落盘后，可以整理数据、经验和教训，并根据高频需求新增或优化 Plugin、Graph，乃至 Anchor 内部实现，形成 RSI（自进化）闭环。

这一目标扩展了此前单机 Pilot 续聊的产品范围；当前已接入企业微信来源的多会话 Graph，但完整逐用户业务授权仍待各业务 Plugin 实现。助手以 Graph 为执行主体；Pilot 是已有控制入口，不作为业务助手必须经过的另一层 Agent。企业微信首个场景是同事与机器人私聊，不依赖群聊。

2026-09-30 近期范围澄清：先实现企业微信 WebSocket Plugin 与 Anchor 助手 Graph，并补齐这条链路必需的基础能力。RSI 留作积累足够工作数据后的普通 Graph，不作为本次实现或基础设施扩建的前置条件。

2026-09-30 通道能力决定：企业微信只是第一个平台适配器。Anchor 需要一个窄的常驻通道能力，用来维护平台连接、接收事件、发送回复，并把平台身份映射为 Anchor 的会话来源；平台差异留在各自适配器中。MCP Plugin 继续承载按需调用的业务工具；长连接主动消息由已挂载 Plugin 的受限运行时工具委托既有网关，不承担连接生命周期。

### 近期链路的代码核查

结论：已在现有架构中补齐通道到普通 Graph 的接线、同图并发和消息打断接续；默认助手可用于私聊接入，业务权限和审批仍需按具体 Plugin 扩展。

| 能力 | 当前依据与缺口 |
| --- | --- |
| Graph 调用业务 Plugin | `simple/run.py` 解析挂载，`node/adapter.py` 接框架 MCP 工具；已有实现，可复用 |
| 企业微信长连接 | SDK、事件规范化、去重、指定普通 Graph 接线已实现；真实 provider/本地 Plugin 验收通过，真实企业微信首条私聊已确认 Graph 完成及平台回复 ACK，多轮/打断/多用户待继续实测 |
| 常驻通道生命周期 | MCP stdio 随节点执行打开与关闭；带 `channel.json` 的 Plugin 由 Anchor 服务层 `ChannelSupervisor` 按平台启动一个受监管 WebSocket 子进程，不能每个会话各连同一个机器人；凭证只注入该服务进程 |
| 多用户会话与历史 | 通道 Session 绑定 Graph、回复节点及可信身份，AgentNode 按外部会话和节点独立加载原生历史，空快照或上轮跳过该节点时向前查找 |
| 同一 Graph 多 Run | 通道 Run 使用 Turn UUID；不同用户同图并发，管理修改/删除受活动 Run 保护；普通手动/API/Webhook 保持单 Run 限制 |
| 追问、接续与对外回复 | 指定回复节点 summary 作为本轮答复；追问正常结束当前 Run，下一条输入新建 Run；执行中新输入取消旧 Run 并接续历史和未完成工作 |
| 逐用户授权 | Session 绑定可信平台来源、成员和会话，服务端成员允许名单控制入口；管理 API key 和业务 Plugin 凭证仍为运营者权限，尚无逐用户业务数据授权链 |
| 消息接收与发送可靠性 | 事件去重、回复落盘和重复事件投递重试；旧 Run 被新消息替代后抑制其业务答案。SDK 本地协议/断线重连已验证；默认 SSE 持续推送正文，断订阅不取消 Run，兼容 JSON 模式仍有 120 秒等待上限；首次接纳顺序持久化以抑制旧重放，真实平台回复窗口待验收 |

当前已接通长期 Session 与普通助手 Graph 的直接绑定：一条新消息启动一个 Run，同一用户补充消息取消旧 Run，保存历史和未完成文件后接续；不同用户可以并发执行同一 Graph。模型历史复用 Harness 原生记录和压缩，Plugin 仍通过既有节点沙箱调用。首期由通道回复来源会话。机器人 SDK 主动推送已通过 Plugin 挂载的运行时工具接线，沿现有长连接发送获授权的 Markdown 通知；接收对象范围仍需平台验证。附件文本提取、原生图片输入、summary 持续输出和图文回复已接入，并经隔离真实 provider 验证，四项真实企业微信平台验收待做。企业微信审批能力需另外接入。能力对照见 [SDK 核查](wecom-sdk-capability-audit.md)。

### 通道能力与平台适配器

通道能力只解决传输和投递，不决定业务答案：

```text
平台适配器（WeCom / Feishu / DingTalk）
        ↓ 规范化事件、身份和投递目标
常驻通道能力（连接、心跳、重连、去重、收发）
        ↓
Anchor 会话与助手 Graph
        ↓
平台适配器发送回复或主动消息
```

最小通道契约应包含：接收事件的稳定 ID、可信发送者、会话/会话类型、文本或附件引用、平台回复目标、确认/失败结果，以及发送幂等所需的关联信息。平台的签名校验、加密、心跳、重连和速率限制由适配器或通道运行时处理；Graph 不读取平台原始协议。

长连接不等于所有平台都必须使用 WebSocket。企业微信智能机器人提供 WebSocket；飞书等平台也提供类似的长连接或流式事件接入；钉钉可能按产品线提供 Stream、长轮询或 HTTP 回调。个人微信则不能简单视为一个可接入的官方机器人平台，通常受账号形态、开放接口和合规限制约束，不能把个人号自动化当作默认能力。

截至 2026-09-30 的官方文档核查：飞书开放平台提供[使用 WebSocket 长连接接收事件](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/server-configure/websocket)的配置入口，适合自建应用/机器人接收事件；飞书官方 SDK 也提供事件订阅能力。微信官方公开文档中的[微信客服消息](https://developers.weixin.qq.com/doc/service/guide/product/kf/intro.html)和[普通消息接收](https://developers.weixin.qq.com/doc/service/guide/product/message/Receiving_standard_messages.html)面向服务号等开放主体，通过推送/HTTP 接口收发消息，未发现面向普通个人微信号的官方 WebSocket 机器人接口。这里的“个人微信”若实际指企业微信中的成员账号，应按企业微信应用或智能机器人接口处理，不能与普通微信个人号混同。

WebSocket 的共同特点是：连接长期保持、客户端和平台双向发送事件、需要应用层心跳和断线重连、平台通常限制同一机器人连接数，并且消息可能重复、乱序或在连接断开窗口内丢失。因此通道层需要持久化接收事件、按事件 ID 去重、记录处理状态、限制并发和背压；回复发送还要处理平台的时间窗口、速率限制、超时与重试。它只解决实时传输，不提供会话历史、权限、业务幂等或 Graph 恢复。

实现顺序上，先抽取这一窄通道契约并完成企业微信适配器；验证两位用户并发私聊、断线重连、重复事件和回复重试后，再接入第二个平台。不要先建设统一账号体系、通用消息编排或分布式消息平台，除非第二个平台的真实差异证明现有契约不够。

### 在线工作与上下文

- 共用 Graph 和 Plugin 定义，每位用户的 Session、执行历史和工作区独立。一个用户可以关联多个任务和 Run，不把所有工作混为一段无限增长的模型上下文。
- 用户身份、调用者、委托范围和数据访问范围由服务端验证并传递到工具边界；共享企业微信接入凭证或 Anchor API key 不能代替终端用户身份。历史摘要、检索结果、文件与 Plugin 外部数据同样遵守授权边界。
- 建议先实现不同 Session 并发、同一 Session 的对话更新有序；长业务 Run 可独立执行并关联原会话。等待一个用户回答不能占住其他用户的执行机会。共享业务对象的写入冲突和重试按具体 Plugin/API 处理，不能只靠会话隔离。
- 持久化沿用框架原生工作记录、现有 Session/Run 和 Git 产物；按需加载历史与框架 compaction，不建设第二套记忆或恢复系统。记录保留来源、工具结果、产物、用户纠正及执行所用能力版本的可追溯引用。

### 定期任务完成后通知助手用户（2026-09-30 需求讨论，方案未冻结）

后续讨论调整：用户明确要求先解决通用 Graph 组合，并确认保留内联子图与独立 Graph 调用两种能力。下文“任务结果订阅”是先前候选建议，未获采纳为实现方向，不作为本轮实现合同。周报通知将作为节点发起独立 Graph 调用的验证案例；冻结设计见 [Graph 组合与独立调用](graph-composition-design.md)，当前实现与验收分别见架构和开发台账。

用户提出具体场景：已有 weekly-work-report 定期任务完成后，由 Anchor 助手在企业微信提醒本人，并讨论该需求与跨 Graph 联动的关系。此前已经记录长任务关联原会话、结果沿来源返回的目标，但尚未确定或交付通用的 Run 完成订阅与跨 Graph 事件触发。

当前周报末节点为 Docmost 发布；计划只保存 Graph、规则和输入，不保存通知订阅及目标用户。子图是执行前展开到同一 Run 的流程组合，不等同于一个独立 Graph Run 完成后触发另一个 Graph。主动发送工具已有接线，但通知生成、目标绑定、助手上下文更新尚未接通。

候选方案：在任务/计划上配置完成通知订阅，绑定一个已确认的用户会话及通道目标。源 Run 达到指定终态后，由现有服务依据持久 Run 事实派发完成事件，携带来源 Graph/Run、终态、报告标题和获授权产物引用；目标助手 Graph 在该用户上下文中处理事件，并通过现有唯一网关主动投递。用户追问“这份周报”时可以沿引用读取该报告。只需固定提示的通知可直接模板化发送并保留同样的来源关联，无需每条都调用模型。

待冻结的边界：

- 订阅保存“哪个任务、何种结果、通知哪个用户/会话”，不把成员 ID 硬编码到周报提示词，也不以“当前只有一个会话”推定任务归属；定时来源本身没有天然的聊天收件人。
- 完成事件属于系统来源，不伪装为用户输入。后台事件不能打断当前对话；同会话串行处理，用户新消息优先，尚未处理/投递的完成事件不能被静默丢弃。需要扩展现有 Turn 接纳规则，而不是直接调用当前会取消旧 Run 的用户消息入口。
- 周报生产、Docmost 发布、通知投递分别记录事实。通知失败不能重跑周报或修改它的完成状态；确定未发出的任务可重试，ACK 不确定仍沿用现有“不自动重复发送”的语义。以源 Run 和订阅标识去重，重启后可从既有 Run 事实核对遗漏，不建设第二个 Graph 调度器或独立消息总线。
- 企业微信连接归服务监管；挂载发送能力不应自动变成另一个入站助手绑定。当前 ChannelSupervisor 会将两个 Graph 挂载同一通道判为冲突，必须先拆开“唯一入站 Graph 绑定”与“多个获授权 Graph 使用同一连接”的含义。
- 目标助手只取得本订阅授权的产物引用/只读材料，不默认共享源 Graph 的全部工作区和隐藏对话。周报首例可把 Docmost 发布成功作为成功提醒条件，正文含标题、时间范围及页面链接；失败提醒条件另行明确。

这些是设计建议，不代表已调整任务、创建通知订阅或开始向成员发送消息。应先用周报到本人助手的闭环验证，再决定是否推广为其他 Graph 的结果触发能力。

### 通过普通 Graph 组织改进

建议将 RSI 实现为后台普通 Graph，挂载读取获授权记录、开发和验证所需的 Plugin：读取工作证据 → 识别重复需求和失败 → 形成可验证的改进假设 → 修改候选资产 → 对比验证 → 按发布授权生效 → 观察后续结果。总结经验不等于已实现改进；生成代码不等于改进有效。

优先调整已有 Plugin 的说明、工具或 Graph；只有证据指向通用执行缺口时才修改 Anchor 核心。经验和知识作为可追溯的 Graph 产物或 Plugin 资源保存，派生内容可重建，不另立通用记忆平台。用户私有事实保持原访问范围，跨用户复用经验不能直接复制聊天或业务原文；模型概括本身不构成脱敏或公开授权。

候选改动在隔离工作区验证，使用来源明确的案例及未参与修改的回归案例比较任务结果、人工纠正、耗时和成本；有副作用的历史调用不得直接对生产系统重放。运行中使用的 Graph/Plugin 版本须可追溯，发布不能静默替换正在执行的依赖；重启和回滚也需考虑数据兼容性。发现需求、生成候选、通过验证、获准发布、线上改善是不同状态。

### 尚待确定与验证

- 产品决定：哪些类别的改动可以在既定授权内自动发布；私有记录参与改进的范围与保留期限；预期并发量和响应目标。当前目标表达不等于允许任意在线自改或跨用户读取。
- 工程验证：企业微信规范事件到 Session/Turn/普通 Graph 的直接链路、同图并发、原生历史和本地 MCP 已验证；真实 provider 和实际服务进程重启通过。真实企业微信连接、逐用户业务授权和审批闭环仍待验收。
- 建议首个验收链路：两个用户并发调用真实业务 Plugin，历史与权限互不串用，重启后继续工作；随后由改进 Graph 从授权记录生成一个 Plugin/Graph 改动，用保留案例证明效果。实际容量及完整 RSI 发布闭环单独验收。

本节是新产品需求与建议方案；当前实现仍以 architecture.md 为准，开发与验收状态只记入开发台账。

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
PUT /sessions/<id>                        # 修改会话名称
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

- 不把所有能力、工具和外部交互都抽象成 OpNode 或一个巨大的 Plugin。
- 不用 PydanticAI Graph 替换 Anchor Graph。
- 不建立无需求支撑的 Graph 发布版本、不可变模型版本或模型版本平台。
- 不把隐藏提示词当作控制面 API。
- 不把前端缓存、模型上下文或 trace 摘要当作唯一事实。
- 不用固定轮数、固定请求数或人工 `max_xxx` 判断复杂任务完成。
- 不扩建通用审批、跨存储原子事务或未知结果人工处置平台；不承诺任意外部操作 exactly-once。

P0 时的历史问题与框架核查见 [Pilot 对话体验与 Pydantic 接入核查](pilot-experience-audit.md)，其中旧待办不代替当前验收标准。

后续开发顺序、共享契约和验收状态统一维护在 [Pilot 开发计划](pilot-development-plan.md)。


### 已冻结的 Graph 联动方向

实现采用普通 OpNode 的 `call` 定义，支持 wait / detach；多个来源各自产生并发独立 Run。内联子图继续属于同一个 Run。定义关系由 Graph 文件派生，具体调用关系落在父节点原生记录和子 Run trigger，不增设通知订阅对象。上节订阅方案仅保留为已否决的历史候选，不能据此继续增加订阅服务。

助手场景通过操作员选择的 `call.session` 接入既有会话。系统任务与用户输入有独立来源，同一 Session 串行且用户消息优先；后台任务保留调用身份与原生恢复记录。目标只读取明确映射的输入及选定产物，继承的是绑定用户自己的会话历史。完成投递沿用 Plugin 网关，不建立另一套 Pilot 或消息执行引擎。实施状态及真实平台验证边界见 A27。
