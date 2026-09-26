# 当前架构

本文说明当前实现，不是完整产品目标。Anchor Pilot、Session、恢复和分阶段实施契约见 [产品与系统架构](product-architecture.md)；Plugin 的当前格式与边界见 [Plugin 设计](plugins.md)；操作方式见 [使用指南](usage.md)。

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

代码中的 harness `capabilities` 指上下文管理、步骤持久化等运行机制，**不是**业务 Plugin。节点层已有组合验证，但默认 Graph 路径尚未注入 `context_capabilities`，Pilot 也未启用上下文压缩。具体证据与体验基线见 [Pilot 体验核查](pilot-experience-audit.md)。

## 已知边界

Session 已接入服务：`/sessions` 提供创建、列表、读取、消息和事件读取、状态变更、Run 关联、停止、失败后续答与删除。Pilot 使用 PydanticAI 和 Harness 会话存储，WebUI 可创建及恢复对话；Pilot 还通过显式 PydanticAI 工具查询 Graph、Plugin、Run 和产物，并可在 Scheduler 校验后创建、修改、删除、启动及控制 Graph Run。

Pilot 对话执行走 turn API。客户端为每次提交生成 `request_id`，服务端在一张 SQLite 表里原子接受提交并分配 `turn_id`，随后在后台线程执行；同一个 Session 下同 ID 同内容复用已存在的 turn，所以丢失响应后的重试不会触发第二次模型调用。模型输出经 PydanticAI 的 Vercel AI 事件编码器转成 chunks，按序号追加到同一数据库；`GET /sessions/<id>/turns/<turn>/events` 只是这些记录的只读 SSE 投影，`id` 即游标，`Last-Event-ID`/`?after=` 决定补发起点，重连或重新订阅都不会重新调用模型和工具。执行在服务端进行，关闭页面既不取消任务也不启动新请求，停止必须显式调用。Harness 保存权威模型消息，Anchor 保存提交意图、执行身份、终态与传输游标；恢复尝试使用新的 `turn_id`，不复用原 framework run ID。进程启动时遗留的 `running` turn 会被标记为 `interrupted` 并保留事件，不自动重放。

这三类记录仍分散在 Session JSON/JSONL、Harness 存储和 turn 数据库里，跨存储没有统一事务；实例内锁也只对单进程有效。涉及副作用的失败 turn 当前保守拒绝自动重放，正式的原始调用账本、资源前态比较与等待边界属于后续阶段。

确认功能已有 `POST /sessions/<id>/confirm`、`/reject` 和 WebUI 入口。待确认记录包含动作、目标、完整提案和 Graph 当前版本摘要；确认后先持久化操作意图，再执行副作用，重启发现 pending 操作时报告 uncertain，禁止自动重放。接口不再维护动作白名单，确认与拒绝都由存储层比对待确认记录的动作和 key，因此新增受控动作不需要同步改 API。当前锁是单进程实例锁，多进程部署仍需事务存储；真实浏览器和跨进程验收仍待补齐。

Pilot 会先保存用户输入。需要审批的工具（Graph 创建/修改/删除、Run 启动与控制）用 PydanticAI 的 `requires_approval` 声明，模型调用它们时本次运行以 `DeferredToolRequests` 结束，Anchor 把每个待确认调用的 `tool_call_id` 与原始参数写入 Session；确认或拒绝只记录决定，随后的一次 `resume` turn 带着 `DeferredToolResults` 让框架用原参数执行或拒绝该调用。参数由框架持有，客户端不能替换成别的动作。`session_ask` 走同一机制的外部执行分支：工具体抛 `CallDeferred`，运行停在问题处，用户的下一条消息作为该调用的结果回到模型，因此提问之后不会再有工具在同一轮里先跑掉。工具执行前先写 pending 操作、完成后记录结果、未知结果返回 uncertain，因此同一次已确认调用在崩溃重试后不会执行第二次；账本按 `tool_call_id` 保存在 `Session.operations`，一次暂停里的多个调用各自保留结果，不会互相覆盖。暂停时还把目标资源的前态（Graph 的 sha256 与是否存在）写进待确认记录，执行前在同一把进程锁内重新比较，所以用户在确认期间改过的 Graph 会被拒绝，而不是被模型提案覆盖。仍然保留的 P2 缺口：Session、Harness 与 turn 库之间的跨存储状态事务、未知结果的明确处置入口、系统 Pilot Graph。

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
