# 当前架构

本文说明当前实现，不是完整产品目标。产品目标和待讨论需求见 [产品与系统架构](product-architecture.md)，当前工作与验收见 [开发台账](pilot-development-plan.md)；Plugin 的当前格式与边界见 [Plugin 设计](plugins.md)；操作方式见 [使用指南](usage.md)。

## 核心模型

Graph 是一个可编辑的 JSON 文件，包含角色 `agents`、命令定义 `ops`、可复用子图 `graphs`，以及节点和边。子图运行前展开；运行时主要面对 Agent Node 和 OpNode。

| 对象 | 当前职责 |
| --- | --- |
| Agent 角色 | 定义模型、指令、网络权限以及读写声明；节点的 `with` 补充本次职责 |
| Agent Node | 在自己的工作区内，由模型使用工具并返回结构化结果完成任务 |
| OpNode | 在同样的沙箱边界内执行命令，由程序完成工作 |
| Edge | 表达依赖和路由；输入记录关联上游节点及 commit |
| Graph Run | 保存一次运行的调度状态、各节点工作区、执行轮次与记录 |

`Op` 继续表示命令定义。Plugin 是独立的共享能力资源，AgentNode 通过节点的 `plugins` 列表直接引用；角色没有 Plugin 继承规则，OpNode 也不挂载 Plugin。

## 文件系统

```text
<Anchor 数据根目录>/                 anchor-serve --root 指定
├── library/                         共享资源，与单次运行无关
│   ├── plugins/<plugin-id>/
│   │   ├── plugin.json              名称、描述、工具引用
│   │   ├── instructions.md          按需读取的说明
│   │   └── …                        补充资料；目录可链接到权威来源
│   ├── tools/<tool-id>/
│   │   ├── tool.json                执行入口与环境引用
│   │   └── …                        工具自己的文件
│   └── environments/<name>/         集中准备的环境，可被多个工具引用
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
| [serve.py](../src/anchor/serve.py) | 工作流、运行、文件、控制及 Session 生命周期 API |
| [pilot.py](../src/anchor/pilot.py) | Pilot 模型执行、控制工具与流式事件编码 |
| [session.py](../src/anchor/session.py) | Session 生命周期、消息与审批记录的持久化 |
| [pilot_turns.py](../src/anchor/pilot_turns.py) | turn 提交幂等、执行身份、事件游标与终态 |
| [apps/web](../apps/web) | 编排和运行共用画布，叠加状态与执行次数 |

Graph 通过节点契约交付任务、接收结果，不直接解释 harness 的内部消息或检查点。mini-swe-agent 已移除。当前依赖的唯一配置来源是 [pyproject.toml](../pyproject.toml)，不在文档另维护一份“当前版本矩阵”。历史迁移验证保存在归档中。

## 工作与记录的边界

每次新运行产生独立的运行目录，其中每个节点有自己的工作区与 Git 仓库。同一运行的反馈循环复用该节点工作区；新一轮对话通过文件和输入延续工作。不同运行不会自动继承成果。

节点写 `/workspace`，读 `/in/<上游节点>`。自己的 `.git` 在沙箱中只读，commit 由 Anchor 在沙箱外创建。边保存 commit 引用；运行时将对应文件树导出到 `.views` 再只读挂载，并非物理零复制。目录结构、传递范围和恢复操作见 [使用指南](usage.md)。

输入快照中的 `.git` 是独立的只读历史仓库，HEAD 固定在输入 commit，仅包含该 commit 及其祖先。下游默认读快照，需要时可用 `git log/show/diff` 追溯；上游后续提交和无关分支不在其中。历史通过 Git 按 commit 拉取，不共享上游对象库；按快照缓存，旧版空 `.git` 缓存在再次使用时自动补齐。这会增加磁盘占用，多轮长历史的对象去重暂未实现。

Agent 完成由 PydanticAI 校验的结构化结果表示（`summary`，多出口时加 `route`）。Bash 是普通工作区工具，不是完成仪式。Op 仍用退出码表示执行成败，分支由显式路由表达。完成声明、文件存在、论证质量是不同的事实，不能相互代替。

沙箱默认不联网；Agent 或 Op 的 `network: true` 显式启用网络。工作区之外的工具与输入只读，沙箱不可用时不退回宿主机裸执行。节点执行记录和恢复控制文件位于工作区之外。

## 学术调研与 Plugin 接入

学术调研使用普通 AgentNode。AgentNode 通过 PydanticAI 结构化结果完成；Bash 只是工作区工具。Anchor 尚在开发阶段，维护中的 Graph 示例与测试应使用这一统一契约，不维护旧完成协议的双轨路径。

1. [深度学术调研图](../examples/graphs/deep-academic-research.json)在 investigator、challenger、reviewer 三个 AgentNode 上挂载 `academic-research` Plugin；这些节点按需使用 Plugin 提供的学术证据能力。
2. 模型注册的工具是 `bash`，由模型构造文献命令。角色指令作为指令文本提供，没有从能力库按需加载说明的机制。
3. `NodeSandbox` 提供普通工作区工具，并由 Plugin 显式挂载登记的 scholarly 工具入口；联网能力另由节点权限决定。
4. [scholarly/__main__.py](../src/anchor/scholarly/__main__.py)解析命令，[runtime/research_tools.py](../src/anchor/runtime/research_tools.py)执行搜索、全文读取、引用追踪等操作，以 JSON 返回结果。
5. Agent 消化结果、保存证据并提交文件，Graph 通过质疑和评审组织反馈。

研究方法维护在 [Plugin 说明](../plugins/academic-research/instructions.md)，通过 `/tools/scholarly/run` 使用登记的环境。`library.py` 解析资源，调度层注入简短目录并把只读挂载交给节点。Agent 完成与工具调用分离，结果由 Runtime 持久化。

代码中的 harness `capabilities` 指上下文管理、步骤持久化等运行机制，**不是**业务 Plugin。节点层已有组合验证，但默认 Graph 路径尚未注入 `context_capabilities`；Pilot 已接步骤持久化与框架压缩（见下文）。具体证据与体验基线见 [Pilot 体验核查](pilot-experience-audit.md)。

## 已知边界

Session 已接入服务：`/sessions` 提供创建、列表、读取、消息和事件读取、状态变更、Run 关联、停止、失败后续答与删除。Pilot 使用 PydanticAI 和 Harness 会话存储，WebUI 可创建及恢复对话；Pilot 还通过显式 PydanticAI 工具查询 Graph、Plugin、Run 和产物，并可在 Scheduler 校验后创建、修改、删除、启动及控制 Graph Run。

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

- 服务内同一图一次只运行一个任务，不同图可同时运行；图内节点目前串行调度。CLI 不参与服务内互斥。
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
