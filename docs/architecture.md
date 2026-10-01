# 当前架构

本文说明当前实现，不是完整产品目标。产品目标和待讨论需求见 [产品与系统架构](product-architecture.md)，当前工作与验收见 [开发台账](pilot-development-plan.md)；Plugin 的当前格式与边界见 [Plugin 设计](plugins.md)；操作方式见 [使用指南](usage.md)。

## 核心模型

Graph 是一个可编辑的 JSON 文件，包含角色 `agents`、命令定义 `ops`、可复用子图 `graphs`，以及节点和边。子图运行前展开；运行时主要面对 Agent Node 和 OpNode。

| 对象 | 当前职责 |
| --- | --- |
| Agent 角色 | 定义模型、指令、网络权限以及读写声明；节点的 `with` 补充本次职责 |
| Agent Node | 在自己的工作区内，由模型使用工具并返回结构化结果完成任务 |
| OpNode | 执行沙箱命令、独立 Graph 调用，或同一 Run 内的配对 fanout/join 控制 |
| Edge | 表达依赖和路由；输入记录关联上游节点及 commit |
| Graph Run | 保存一次运行的调度状态、各节点工作区、执行轮次与记录 |

`Op` 定义恰好包含 `run` 沙箱命令、`call` 结构化工作流调用、`fanout: {"join": "节点 ID"}` 或 `join: {}` 中的一种。Plugin 是独立的共享能力资源，AgentNode 通过节点的 `plugins` 列表直接引用；角色没有 Plugin 继承规则，OpNode 也不挂载 Plugin。

## 文件系统

部署时可使用 `deploy/systemd/anchor.service` 将 HTTP 服务及其 Plugin 通道子进程交给 systemd 管理；凭证仍由服务入口从仓库 `.env` 加载。构建后的网页由同一 HTTP 服务在 8077 提供，Vite 只用于开发。仓库提供 unit 不代表当前运行环境已启用 systemd，实际部署验证见开发台账。

```text
<Anchor 数据根目录>/                 anchor-serve --root 指定
├── library/                         共享资源，与单次运行无关
│   ├── plugins/<plugin-id>/
│   │   ├── plugin.json              名称、描述、工具引用
│   │   ├── skills/<skill>/SKILL.md  按需读取的说明
│   │   └── …                        补充资料；目录可链接到权威来源
│   ├── tools/<tool-id>/
│   │   ├── tool.json                执行入口与环境引用
│   │   └── …                        工具自己的文件
│   └── environments/<name>/         集中准备的环境，可被多个工具引用
├── state/
│   ├── schedules.json               本机定时计划；时间线从计划和 Run 事实派生
│   ├── responses.json               Responses id → key 摘要 / Session / turn
│   └── pilot-*.sqlite               Pilot 会话、提交和 SSE 事件
└── workspaces/<graph-name>/         保留现有目录名称
    ├── graph.json                   Graph、AgentNode、OpNode 及 Plugin 引用
    └── runs/<run-id>/
        ├── run.json                 调度与执行状态
        ├── graph.json               展开后的图记录
        ├── plugins.json             有 Plugin 时记录解析来源及摘要
        ├── <node-id>/.git/           各节点的独立 Git 与工作区
        ├── <node-id>/…               本次任务的产物
        ├── control/                 恢复与完成记录
        ├── .views/                  指定 commit 的只读输入导出
        └── *.trace.jsonl            对话及工具轨迹
```

Graph、AgentNode、OpNode 的定义已经由 `graph.json` 表达，不为每个概念另外建立一套空目录。AgentNode 的工作区在执行时创建；可复用的节点配置仍是 JSON，首期不另建全局节点注册库。知识库目录与格式暂不冻结。

Plugin 挂载到 `/plugins/<id>`，工具入口挂载到 `/tools/<id>/run`，工具文件在 `/tools/<id>/files`，都只读。工具环境保持解释器和脚本原路径，避免破坏 Python venv 的 shebang。工作区仍是 `/workspace`，输入仍是 `/in/<node>`。

现有开发数据根目录为 `.local/demo`。仓库中的 `plugins/` 是随代码维护的 Plugin 源码；数据根目录中的 `library/plugins/` 可以用目录符号链接引用它，不复制第二份。环境既可集中放在 `library/environments/`，也可引用已准备好的安装位置。

## 执行链与代码位置

```text
WebUI / anchor-graph
  → 图解析、子图展开与调度
  → NodeRequest / NodeOutcome 节点边界
      → Agent：PydanticAI + harness → 工具与结构化结果
      → Op：执行配置的命令
  → NodeSandbox / Bubblewrap
  → 节点工作区产物 → Anchor 提交 Git → 下游只读输入
```

| 文件或目录 | 职责 |
| --- | --- |
| [library.py](../src/anchor/library.py) | Plugin／工具文件解析、只读绑定、目录披露与资源摘要 |
| [simple/graph.py](../src/anchor/simple/graph.py) | 图定义、子图展开、读写声明校验 |
| [simple/run.py](../src/anchor/simple/run.py) | 调度、任务上下文、轮次、commit 输入和运行状态 |
| [simple/node_bridge.py](../src/anchor/simple/node_bridge.py) | 将调度调用交给 Agent / Op 运行时 |
| [node/__init__.py](../src/anchor/node/__init__.py) | 节点请求和结果契约 |
| [node/adapter.py](../src/anchor/node/adapter.py)、[node/agent_runtime.py](../src/anchor/node/agent_runtime.py) | Agent 执行、Bash 工具、完成信号和执行记录 |
| [node/op_runtime.py](../src/anchor/node/op_runtime.py) | 命令节点执行，不依赖模型循环 |
| [node/recovery.py](../src/anchor/node/recovery.py)、[node/context.py](../src/anchor/node/context.py) | 恢复判断、上下文管理和输出记录 |
| [runtime/execenv.py](../src/anchor/runtime/execenv.py)、[runtime/sandbox.py](../src/anchor/runtime/sandbox.py) | 命令环境、工具路径、只读挂载、网络隔离和取消 |
| [serve.py](../src/anchor/serve.py) | 工作流、运行、文件、控制、Session、Webhook、Responses、定时与时间线 API |
| [pilot.py](../src/anchor/pilot.py) | Pilot 模型执行、控制工具与流式事件编码 |
| [session.py](../src/anchor/session.py) | Session 生命周期、消息与审批记录的持久化 |
| [pilot_turns.py](../src/anchor/pilot_turns.py) | turn 提交幂等、执行身份、事件游标与终态 |
| [channel/__init__.py](../src/anchor/channel/__init__.py) | 长驻平台通道的规范化事件和落盘去重账本契约 |
| [apps/web](../apps/web) | 编排和运行共用画布，叠加状态与执行次数 |

Graph 通过节点契约交付任务、接收结果，不直接解释 harness 的内部消息或检查点。mini-swe-agent 已移除。当前依赖的唯一配置来源是 [pyproject.toml](../pyproject.toml)，不在文档另维护一份“当前版本矩阵”。历史迁移验证保存在归档中。

## 架构审视与演进方向（2026-10-01）

当前实现已有有价值的核心边界：Graph 定义与 Run 记录分开；Runner 经 `NodeRequest` / `NodeOutcome` 和 `simple/node_bridge.py` 调用节点；Agent 与 Op 执行共用沙箱和运行结果契约；Session、Turn、Library、Graph Run 分别有自己的持久化或资产归属。新功能应延续这些边界，而不是另建执行链。

静态代码审视也发现两处边界压力，作为后续渐进治理方向，不表示本次已重构：

- `src/anchor/serve.py` 同时放置 `Scheduler`、请求处理器、HTTP 路由、Session/Turn 用例、计划和响应处理。`graph_calls.py` 及 `channel/` 中的部分流程还直接访问 Scheduler 的锁、可变状态和内部运行方法。入口协议、应用协调与运行时状态因此耦合在一起。
- `src/anchor/simple/run.py` 同时承载恢复/状态处理、任务和输入组装、节点执行准备及主调度循环。Call、fanout/join 等扩展需要接入同一 Runner 主路径，变化容易集中到它。

目前已有 `anchor-graph` 命令，可不启动 Web 服务而单独执行一个 workspace；这证明了单进程 Graph 执行路径，但还不是干净的独立分发单元。该入口默认从 Anchor 源码根加载 `.env` 和 `.local/runtime.json`，Python 包的必装依赖也包含研究/PDF/HTML 能力；目前没有 Graph 与精确 Plugin 资源闭包的 bundle 格式或 Runtime 兼容清单。因此“可单图运行”与“只安装这个 Graph 所需的最小内容”是两个不同状态。

演进应先收紧跨模块契约，再按不变量逐步抽取。服务侧的目标是让 HTTP、CLI 和通道作为入口适配器调用应用用例，通道与 Graph Call 通过明确的 Run/Session 能力接口协调，而不直接操作 Scheduler 内部状态。Runner 侧的目标是保留单 Run 的唯一协调者和现有恢复语义，同时把节点执行配置/分发从主调度循环中隔离。只有在调用关系和验收边界明确后才移动代码；不为了行数拆文件，也不做一次性服务层或 Runner 重写。

独立交付的目标设计见 [产品与系统架构](product-architecture.md) 的“可独立交付的 Graph 执行单元”。它要求平台服务和精简部署共用 Runner 与 Run 格式；当前代码尚未实现依赖裁剪或 Graph/Plugin 打包。

后续跨模块设计依据见 [产品与系统架构的“新能力放置规则”](product-architecture.md#架构指导新能力放在哪里)。该目标设计与本页记录的当前实现分开维护。

## 工作与记录的边界

每次新运行产生独立的运行目录，其中每个节点有自己的工作区与 Git 仓库。同一运行的反馈循环复用该节点工作区；新一轮对话通过文件和输入延续工作。不同运行不会自动继承成果。

Graph 可声明 object 默认 `input`；触发时的 object 与其递归合并（数组、null 和其他非对象整体替换），最终输入随 `run.json` 保存。Agent 节点在任务上下文看到它；Op 节点读取 `ANCHOR_INPUT` JSON 环境变量。运行看板由 Run 的 `started` / `updated` 和 `run.json.trigger`，以及 `state/schedules.json` 派生，不额外保存拒绝事件；前端按运行摘要、筛选、每日 24 小时刻度和计划/实际状态组织时间线，点击时间线条先看运行预览，再进入既有节点与产物详情，可返回时间线。定时只按本机时间到点触发；重启和 Graph 忙时错过的时点不补跑。HTTP 管理 API、Webhook 和 Responses 统一由 `ANCHOR_API_KEYS` Bearer 白名单保护；loopback 空白 key 仅为本机开发兼容。Responses 当前只实现文本请求、文本结果、`previous_response_id` 同 key 续聊及 JSON/SSE 子集，尚未以真实 provider 做产品验收，不能称完整兼容。

节点写 `/workspace`，读 `/in/<上游节点>`。自己的 `.git` 在沙箱中只读，commit 由 Anchor 在沙箱外创建。边保存 commit 引用；运行时将对应文件树导出到 `.views` 再只读挂载，并非物理零复制。目录结构、传递范围和恢复操作见 [使用指南](usage.md)。

输入快照中的 `.git` 是独立的只读历史仓库，HEAD 固定在输入 commit，仅包含该 commit 及其祖先。下游默认读快照，需要时可用 `git log/show/diff` 追溯；上游后续提交和无关分支不在其中。历史通过 Git 按 commit 拉取，不共享上游对象库；按快照缓存，旧版空 `.git` 缓存在再次使用时自动补齐。这会增加磁盘占用，多轮长历史的对象去重暂未实现。

Agent 完成由 PydanticAI 校验的结构化结果表示（`summary`，多出口时加 `route`）。Bash 是普通工作区工具，不是完成仪式。Op 仍用退出码表示执行成败，分支由显式路由表达。完成声明、文件存在、论证质量是不同的事实，不能相互代替。

沙箱默认不联网；Agent 或 Op 的 `network: true` 显式启用网络。工作区之外的工具与输入只读，沙箱不可用时不退回宿主机裸执行。节点执行记录和恢复控制文件位于工作区之外。

长驻平台通道不运行在 AgentNode 的 MCP 生命周期内。Plugin 的 `channel.json` 由 Library 识别；Anchor 服务启动后由 `ChannelSupervisor` 扫描 Graph 节点挂载，按平台只启动一个受监管的 WebSocket 子进程，服务退出时停止它。`plugins/wecom/ws_gateway.py` 用 SDK 维护长连接，把可信事件转换为 `ChannelEvent`，用 `EventLedger` 去重并保存回复。`POST /v1/channels/wecom/events` 按配置的 Graph、回复节点和来源/成员/会话建立独立 Session/Turn；直接调用既有普通 Graph runner，不调用 Pilot。Pilot 与通道共享 Session 持久化实现，但 `/sessions` 只列出没有 Graph/通道绑定的 Pilot 会话，避免企业微信聊天混入 Pilot 会话选择器。`.env` 的 `ANCHOR_WECOM_USERS` 默认拒绝，平台不能选择 Graph 或 Plugin。每条消息对应独立 Run，同一会话新消息取消旧 Run 并等待其退出后接上原生 FileStepStore / continue_run；不同用户的同一 Graph 可以并发。文本、图片、文件和混合消息的附件由网关下载到 `state/channels/wecom/events/<event>`，Graph 以只读 `/in/channel` 读取，路径经过服务端目录校验。按节点查找最近可读历史，上一轮产物及取消时未完成工作通过只读 `/previous` 传递。旧事件重投不重跑，已被新消息替代的回复不回传业务答案；外部副作用不回滚。Run 状态原子发布，Graph 修改/删除及历史 Run 删除受活动执行保护。API 鉴权沿用 `ANCHOR_API_KEYS`，网关使用其中一把密钥。默认助手挂载 `wecom`；其他业务 Plugin 仍需显式挂载，审批 API 未实现。附件经有界文本提取/图片校验后进入原生模型输入，原件继续只读保留。指定回复节点的结构化 summary 经 TurnStore/SSE 持续输出，网关对同一个 stream_id 节流更新，最终可附 PNG/JPEG。宿主按节点挂载注入受限 FunctionToolset：主动发送经鉴权 Unix socket 委托唯一网关，回复图片只允许当前节点可读路径；这两个通道工具不受节点 network 开关影响，MCP/业务命令仍受原沙箱限制。主动发送按工具调用 ID 去重、ACK 确认，结果未知不自动重试；入口和出站成员名单分开配置。现有事件账本保存首次接收顺序与会话身份，重连/重启后也抑制旧消息重放。配置与真实验收边界见 [企业微信助手接入](wecom-assistant.md)。

## 学术调研与 Plugin 接入

学术调研使用普通 AgentNode。AgentNode 通过 PydanticAI 结构化结果完成；Bash 只是工作区工具。安装可选 Monty 依赖后，CodeMode 作为内部优化自动折叠普通 Plugin/MCP 工具，Harness 控制工具和 Bash 保持原生，Monty 不获得宿主文件、环境变量或时钟访问。Anchor 尚在开发阶段，维护中的 Graph 示例与测试应使用这一统一契约，不维护旧完成协议的双轨路径。

1. [深度学术调研图](../examples/graphs/deep-academic-research.json)在 investigator、challenger、reviewer 三个 AgentNode 上挂载 `academic-research` Plugin；这些节点按需使用 Plugin 提供的学术证据能力。
2. 模型注册的工具是 `bash`，由模型构造文献命令。角色指令作为指令文本提供，没有从能力库按需加载说明的机制。
3. `NodeSandbox` 提供普通工作区工具，并由 Plugin 显式挂载登记的 scholarly 工具入口；联网能力另由节点权限决定。MCP 工具集通过 PydanticAI `defer_loading()` 挂载，模型首轮只获得 `search_tools`，需要时再揭示匹配的工具定义；stdio/HTTP 生命周期、网络和沙箱边界不变。
4. [scholarly/__main__.py](../src/anchor/scholarly/__main__.py)解析命令，[runtime/research_tools.py](../src/anchor/runtime/research_tools.py)执行搜索、全文读取、引用追踪等操作，以 JSON 返回结果。
5. Agent 消化结果、保存证据并提交文件，Graph 通过质疑和评审组织反馈。

研究方法维护在 [academic-research Skill](../plugins/academic-research/skills/academic-research/SKILL.md)，通过 `/tools/scholarly/run` 使用登记的环境。`library.py` 解析资源，调度层注入简短目录并把只读挂载交给节点。Agent 完成与工具调用分离，结果由 Runtime 持久化。

代码中的 harness `capabilities` 指上下文管理、步骤持久化等运行机制，**不是**业务 Plugin。节点层已有组合验证，但默认 Graph 路径尚未注入 `context_capabilities`；Pilot 已接步骤持久化与框架压缩（见下文）。具体证据与体验基线见 [Pilot 体验核查](pilot-experience-audit.md)。

## 已知边界

Session 已接入服务：`/sessions` 提供创建、列表、读取、消息和事件读取、状态变更、Run 关联、停止、失败后续答与删除。Pilot 使用 PydanticAI 和 Harness 会话存储，WebUI 可创建及恢复对话，历史会话旁的“…”菜单支持改名与确认删除。`PUT /sessions/<id>` 只更新 title；空闲会话可直接删除，服务端在同一调度锁内拒绝正在回复的会话并执行删除，避免与新 turn 并发。删除保留关联 Graph/Run/产物及框架步骤文件，不代表磁盘记录的彻底擦除。Pilot 还通过显式 PydanticAI 工具查询 Graph、Plugin、Run 和产物，并可在 Scheduler 校验后创建、修改、删除、启动及控制 Graph Run。

Pilot 对话执行走 turn API。客户端为每次提交生成 `request_id`，服务端在一张 SQLite 表里原子接受提交并分配 `turn_id`，随后在后台线程执行；同一个 Session 下同 ID 同内容复用已存在的 turn，所以丢失响应后的重试不会触发第二次模型调用。模型输出经 PydanticAI 的 Vercel AI 事件编码器转成 chunks，按序号追加到同一数据库；`GET /sessions/<id>/turns/<turn>/events` 只是这些记录的只读 SSE 投影，`id` 即游标，`Last-Event-ID`/`?after=` 决定补发起点，重连或重新订阅都不会重新调用模型和工具。执行在服务端进行，关闭页面既不取消任务也不启动新请求，停止必须显式调用。Harness 保存权威模型消息，Anchor 保存提交意图、执行身份、终态与传输游标；恢复尝试使用新的 `turn_id`，不复用原 framework run ID。进程启动时遗留的 `running` turn 会被标记为 `interrupted` 并保留事件，不自动重放。

turn 数据库使用 SQLite WAL，让 SSE 读取已提交事件时不与逐条增量写入争抢数据库排他锁。Session JSON/JSONL、Harness 存储和 turn 数据库没有统一事务；实例内锁只对单进程有效。这是当前边界，不再作为跨存储事务改造的待办。

续聊按框架记录接通：Pilot 的 `StepPersistence` 用原生 `FileStepStore`（`state/pilot-steps/`，每个持久化 run 一份 `run.json`、`events.jsonl`、`tool_effects.jsonl`、`snapshots/*.json`、`media/*`），并打开 `capture_frontier=True`——进程在工具执行中被杀时不会走到「已结算」边界，没有 frontier 快照就没有任何可读现场。新一轮消息先看框架记录：记录比已保存对话更长（进程被杀或用户停止的回合走不到保存）时用它，否则用 conversation store。`continue_run(include_interrupted=True)` 读回的历史末尾可能是未完成的 tool call，而框架拒绝在未处理调用上叠加新 prompt，所以 `pilot._close_unfinished` 只把这种响应标成框架自己的 `state='interrupted'`，由框架合成 `outcome='interrupted'` 的 tool-return：模型看到「调用过、结果未知」，不会重放那次调用。`Scheduler.create_turn`、`pilot_message` 允许中断会话接新消息，旧 `unsafe_to_retry` 门禁已删除。

确认功能已有 `POST /sessions/<id>/confirm`、`/reject` 和 WebUI 入口。待确认记录包含动作、目标、完整提案和 Graph 当前版本摘要；执行前持久化操作意图，遇到已有未完成操作时返回 uncertain。确认与拒绝由存储层比对原调用。这套现有审批通过了真实 DeepSeek HTTP/SSE 验证，浏览器审批测试仍用 mock SSE；它不是新续聊方案的验收证据。

审批已按 2026-09-26 的收敛决定缩减：用户请求即授权，`graph_run`、`run_pause/resume/stop`、`graph_create`、`graph_update` 直接执行，不再逐次确认；只有 `graph_delete` 仍声明 `requires_approval`，以 `DeferredToolRequests` 暂停，确认或拒绝后通过 `DeferredToolResults` 恢复原调用。`session_ask` 通过 `CallDeferred` 暂停，用户的下一条消息作为工具结果返回。操作账本按 `tool_call_id` 保存在 `Session.operations`，删除的前态在暂停时记录、执行前比较；结果不确定的操作不会被重放。

当前 Pilot 尚不是系统 Graph。聊天中的 Graph、Run、Artifact 引用可以直接打开已有页面并返回原会话：Pilot 在回复里写 `#anchor/graph/<graph>`、`#anchor/run/<run>`、`#anchor/artifact/<run>/<node>/<path>`，`apps/web/src/links.ts` 解析，`App` 拦截点击切换到对应视图并显示「返回会话」。系统 Pilot、计划呈现、研究应用和附件等后续需求不构成当前实现；是否及如何开发以用户后续决定为准。当前工作与验收见 [唯一开发台账](pilot-development-plan.md)。

本地框架接口核对补充：StepPersistence、FileStepStore / SqliteStepStore、continue_run、inspect_recovery 与 `capture_frontier` 已存在；普通 AgentNode 使用 FileStepStore 与 continue_run，Pilot 现在同样如此。配置 agent_name 后，框架 persistence run ID 由 agent_name 与执行 ID 派生（`5:pilot<turn_id>` 的 base64），不能假定等于 Anchor turn ID，因此续聊按 `conversation_id` 查记录。真实杀进程续聊已通过验收，证据见开发台账 A07–A09。

Harness 的 FileStepStore 原生记录包括 `events.jsonl`、`tool_effects.jsonl`、`snapshots/*.json`、`media/*` 和 `run.json`，不等于一个 transcript 文件。当前 Pilot 的模型历史在 `state/pilot-conversations.sqlite`，工作记录在 `state/pilot-steps/`，界面事件在 `state/pilot-turns.sqlite`；`sessions/<id>/events.jsonl` 只是产品活动日志。

Pilot 已启用框架压缩：默认 `SlidingWindowCompaction`（200 条消息或上下文 60% 触发），配置 `pilot_compaction` 可调阈值、关闭，或加上 `SummarizingCompaction` 与 summarizer 模型。压缩写入的是继续对话所用的历史，并留下 receipt 说明此前内容已不是原话；被丢弃的消息仍留在该 run 更早的快照里。Planning、AskUser capability 和编辑分支未接；现有提问通过 CallDeferred 实现。接口存在不等于对应产品交互已接入。

以下是现状说明，不是另一份升级待办：

- 手动/定时入口按 Graph 拒绝忙碌时的新任务；独立调用和不同通道会话允许同图并发。单个 Run 在配对 fanout/join 区域内并行执行分支，其余节点串行推进。CLI 不参与服务内互斥。
- 图拓扑和节点身份静态；新运行不会以已有 commit 自动命中结果缓存。子图可展开，WebUI 尚不能进入子图内部编辑。
- 调度仍读取当前图。带 Plugin 的运行会核对展开定义、子图轮次配置与资源摘要，变化时拒绝恢复并保留旧记录；无 Plugin 的旧路径继续沿用原恢复方式。恢复前不要修改图结构。
- 不能判断副作用是否发生时，运行会报告不确定或失败，不保证任意命令可以自动安全重跑，也不保证 exactly-once。
- 图轮数和模型请求次数可以不设上限，但命令超时、异常重试及同一轮最多 4 次恢复尝试仍是现有边界。资源限制不能代替任务的完成判断。
- 读写声明校验不等于运行结束时统一检查所有产物，也不证明研究结论正确或长任务必然收敛。
- 尚无完整的外部事件、人工等待和运行中输入注入流程；没有服务级鉴权，默认面向本机使用。
- Plugin 已能引用、只读挂载和在 UI 查看；知识库编译、自动环境安装、活跃资源热更新及完整调用归因尚未实现。资源变化在节点／恢复边界检查；活跃命令期间禁止操作者原地更新共享资源。

## 保持的设计原则

复用现有运行链与沙箱边界；图仍是 JSON，节点仍拥有独立工作区，成果仍通过 commit 关联。研究是否结束应由目标、证据和反馈决定，不能用固定 `max_xxx` 充当质量或收敛判断。用户主动停止和显式资源预算是另外的控制需求。

这些原则约束 Plugin 接入，不要求恢复历史上已删除的服务、数据库或图版本发布体系。

## 本机工作周报

`weekly-work-report` 的采集、理解、写作和评审仍是普通业务 Graph；通过评审后的 Docmost 同步节点挂载 `docmost` Plugin。Graph 工作区中的 `local-inputs.json` 由本机操作员按节点 ID 授予具名只读路径，挂载到 `/local-inputs/<name>`；Graph JSON/API 本身无权增加该授权。采集 Op 读取两个 sessions 目录和普通采集脚本，将当次窗口的证据交给后续节点。授权路径随 Run 保存，恢复时授权改变则拒绝继续。图按程序采集→项目理解与选题→写作→读者视角独立评审→门禁分流运行；表达问题退回写作，项目理解与判断问题退回理解。证据缺口通过限定结论处理，不设 blocked 分支。正文围绕项目实质变化，来源单独保留，评审先检查可理解性再核查关键事实。四项评审通过、阻断问题解决且评审对应当前稿件 commit 后才组装 Markdown、来源附录和 SVG；Docmost 发布节点通过附件接口上传 SVG 并插入页面，失败时不更新正文；使用方式见 [每周工作报告](weekly-work-report.md)，真实验收以台账 A21 为准。图片下载保留 attachment，并提供图片 MIME 与隔离 CSP，使 Markdown 图片预览可用且不开放脚本或外部资源执行。


## 本机 RSI Graph

仓库提供 [rsi Graph](../examples/graphs/rsi.json) 和 [安装脚本](../scripts/setup_rsi.py)。它复用同一计划存储和 Graph 反馈机制：`collect → audit-context → audit-fanout → 五个专项分支 → audit-join → analyze → review-fanout → 两个独立评审 → review-join → review → gate → publish`。五个领域为 Run、架构代码、Graph、Plugin、依赖/社区；最后一个分支先执行联网 research Op，再执行 dependency-audit。整个流程属于同一个 Run。gate 区分修改综合稿（回 analyze）与重新专项审查（回 audit-context 保存反馈，再 fanout），不设固定模型请求或 Graph 轮数上限。 review 是校验评审 commit 并合并意见的普通 Op，事实/研究与方案/验收/回滚分工，任一未通过不能被另一方覆盖。

collect 动态发现可授权源码与未忽略新文件、全部部署 Graph、已安装 Plugin/工具/Skill/MCP/通道资源和全历史 Run 索引。本周 Run 及按需历史投影包含错误、恢复和 commit/输入关系；不会采集完整聊天/trace/推理。领域索引引导重点读取，源码及 Plugin 证据保留哈希与脱敏标记。Python 字面量和注释脱敏保留可解析语法，目录排除、二进制和授权缺失都有覆盖记录。历史提案从成功 publish 的记录 commit 读取，模型先读简洁 previous-index，再按 ID 回查完整历史。

research 从本次冻结 manifest、可选/开发依赖和包注册表项目链接发现目标，公开 API 主机限定 GitHub/PyPI/npm；记录版本、发布说明与 issue 信号以及限流/失败，未覆盖 Discussions/私有/非 GitHub 社区，不等同于穷尽互联网。Python 清单来自采集解释器，不冒充服务环境。专项输出 findings 与实际读取/未覆盖范围；综合以分支结论为起点，按需追溯证据。gate 检查当前分析评审 commit、join 分支 commit、证据可读取、提案 ID 唯一与历史连续性；这些机械检查不证明报告语义正确。结果写入 Run 的 publish/，包含 audit-manifest.json、报告、提案、来源、评审和门禁。

该 Graph 不写源码、不修改 Graph/Plugin、不重放历史副作用，也不把计划或模型回答当成完成事实。源码和数据根通过 Graph 工作区的 `local-inputs.json` 由操作员只读授权；网络响应有主机白名单、超时和大小限制，失败会进入证据文件。当前实现和真实 provider/长期递归效果的验证状态见 [RSI Graph](rsi.md) 和开发台账。

## 独立 Graph 调用

同 Run 的局部并行与独立调用分别表达，具体见下节及 [组合设计](graph-composition-design.md)。

保留文件内子图展开，同一 Run 内执行；新增 `ops.<name>.call` 在普通 OpNode 上建立独立 Run。`wait` 等待成功并复制显式选择的结果，`detach` 在持久接纳后返回，子 Run 继续独立运行。多来源调用同一个 Graph 可并发；手动/定时入口原有繁忙拒绝规则保留。

调用身份由来源 Graph、Run、完整节点 ID、全局执行轮次确定。控制记录位于 `control/.graph-calls/<identity>/graph-call.json`，目标保存 `admission.json`、冻结 `graph.json`、`run.json` 和只读 `call-inputs/`。服务恢复接纳后尚未启动的运行；Runner 恢复使用 Run 自己的定义快照。模型与命令的执行、提交、恢复仍由现有 Node/Harness 接口承担。已开始但无法确认结果的命令保留 `Uncertain`，不通过创建新 Run 自动重放。

输入常量及 JSON Pointer 映射只传递显式选择的数据；文件仅取调用节点可见的已提交上游快照，目标读取 `/in/call/`。wait 结果复制到调用节点 `result/`，每轮引用写入 `call.json` 并随节点提交；网页从原生历史投影具体父子关系。跨 Graph 祖先递归调用被拒绝，图内反馈循环不受此限制。wait 取消只停止自己的子 Run；detach 接纳后不随父 Run 停止。

`call.session` 是操作员在定义中选择的已有通道会话。后台 Graph 与该会话用户消息串行，后台不能打断聊天；用户新消息可使后台保留原 Run 并让出执行。恢复完成后由既有网关以稳定发送 ID 投递正文，网关沿用原账本处理 ACK 去重和未知投递结果。后台模型完成不等于消息已投递，投递失败会使等待调用失败。完整调用祖先中的会话身份约束跨用户访问，输入参数不能授予会话权限。同一 Plugin 通道可由多个 Graph 挂载，服务只启动一个平台网关。

`/graph-relations` 从已保存定义派生关系；`/graphs` 提供全部 `active_runs`；Run 详情提供逐轮 `calls` 和 `trigger` 来源；`/channel-sessions` 提供已认证操作员可选的通道会话。历史调用引用和活动运行参与编辑/删除保护，不另建关系数据库。

## 同一 Run 内的局部并行

`fanout` Op 只声明配对 join 节点，普通边声明至少两条独立串行分支。分支中的 Agent/Op 属于同一 Run，各自使用既有工作区、沙箱、Harness 记录和 Git 提交。首期拒绝区域内分支选择/循环、嵌套并行、交叉边、外部进入分支及重叠工作区；区域外的路由和整体反馈循环继续有效。

原调度线程准备输入、保存活动执行身份、接收完成结果及提交状态；工作线程仅调用原 Node 执行接口。`run.json.active` 保存活动节点，`parallel` 保存当前 fanout/join、展开轮次和本轮完成 commit。全部分支成功后 join 写 `join.json`，绑定每个分支的节点、commit、摘要和文件；下游沿用只读输入及祖先快照读取分支产物。分支失败取消同伴且不放行 join；暂停等待当前活动节点结算，停止请求取消并等待活动执行退出。已发生的外部操作不回滚。

恢复保留同一展开轮次和节点执行身份，先读取原生 completion fact，已完成节点不重复执行；命令结果未知仍按原有 Uncertain 规则处理。带并行区域的 Graph 用 `control/.parallel-nodes/<身份摘要>/` 与 `.parallel-traces/<节点摘要>/<执行轮次>.trace.jsonl` 避免名称/轮次冲突；独立调用仍使用 `.graph-calls`。API trace 键使用 `[节点ID,执行轮次]` 的 JSON 字符串，前端兼容旧串行 trace 键。会话停止保留未完成分支文件快照供下一 Run `/previous` 使用；历史步骤路径按各次 Run 自己的 Graph 快照解析。旧 Run 缺省新字段，无需迁移。

WebUI 的“添加节点 → 并行分支”生成配对控制节点及两个 Agent 分支，画布展示配对关系和多个活动节点。示例见 [parallel-audit.json](../examples/graphs/parallel-audit.json)。已取得真实 DeepSeek 并行 Agent、join、综合文件的运行证据，以及真实后端浏览器和进程退出恢复验证；具体测试与部署边界以台账 A31 为准。
