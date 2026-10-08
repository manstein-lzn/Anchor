# 当前架构

> 历史快照：保留当时设计与证据，不代表当前实现或待办。当前入口见 [文档目录](../../README.md)。

2026-10-06 后续目标已确定为 [Goose-only Runtime](../../goose-runtime-migration.md)，预算控制暂不实施。Rust Host/Kernel 使用 Goose ACP，io-harness/Rig 的依赖、执行器和实验原型已移除。AgentNode、Pilot、恢复、渠道 Session、Library、OAuth 与 source-free package 均有分片证据；最新候选包用确定性本地 Provider 完成 Host/Web/Goose/Plugin Graph、重启及副作用幂等验收，未调用外部业务服务。此分支尚未切换生产服务、配置或数据，也未在目标发行版验证 systemd/动态库依赖；真实 Docmost、WeCom 和公共学术服务验收按计划暂缓。实现与验收边界见 [Rust 部署指南](../../rust-production-deployment.md)、[候选回归](../../rust-production-candidate.md) 和 [开发台账](../../pilot-development-plan.md)。

本文说明当前实现，不是完整产品目标。产品目标和待讨论需求见 [产品与系统架构](product-architecture.md)，当前工作与验收见 [开发台账](../../pilot-development-plan.md)；Plugin 的当前格式与边界见 [Plugin 设计](../../plugins.md)；操作方式见 [使用指南](usage.md)。

## 核心模型

2026-10-06 已确认后续目标为 Rust 平台服务端和官方工具/集成，WebUI 保持现有实现。仓库当前同时保留既有 Python 部署实现与 Rust-native 生产候选；两者不是可在同一数据根双写的双活服务。未获准切换前，不能把候选实现或本地验收等同于生产已替换。目标与迁移规则见 [Rust 平台交付决定](rust-platform-target.md)。

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

Rust source-free release 的 `deploy/systemd/anchor.service` 直接启动 Rust Host，凭证从 `/etc/anchor/anchor.env` 加载；Host 按已挂载 Graph Plugin 的 `channel.json` 自动发现、启动、重启和停止 WeCom Gateway，默认不应再启用独立 Gateway unit。构建后的网页由 Rust Host 在 8077 提供，Vite 只用于开发。Python `anchor.serve` 仍作为兼容入口存在，但不在 Rust release 的 service lifecycle 中。仓库提供 unit 和本地候选包不代表已切换生产数据或完成目标机 systemd 验收，部署边界见 [Rust 部署指南](../../rust-production-deployment.md)。

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
既有 Python 部署：WebUI → serve.py / Session / Turn → PydanticAI + Python Harness
Rust 生产候选：WebUI → Rust Host API / Session → 共享 GraphRunner
  → Goose ACP AgentNode / Bubblewrap OpNode → Rust Host 持有 Run / Artifact / Plugin / Session 事实
```

两条实现线共享产品语义，但不是同一执行后端，也不得对同一数据根双写。Rust 使用 Goose 执行 Agent，依赖与源码均不再保留 io-harness/Rig 后端。生产替换仍须单独完成目标机部署与旧数据/配置切换决策；Python 历史验收不自动证明 Rust 行为，反之亦然。Rust 当前模块、部署和已验证边界见 `rust/anchor-runner-host`、`rust/anchor-runtime` 及 [Rust 部署指南](../../rust-production-deployment.md)。

下表中的 `src/anchor` 路径说明既有 Python 实现及兼容入口，不表示 Rust 生产候选仍依赖这些模块。

| 文件或目录 | 职责 |
| --- | --- |
| library.py（历史路径 `../src/anchor/library.py`） | Plugin／工具文件解析、只读绑定、目录披露与资源摘要 |
| simple/graph.py（历史路径 `../src/anchor/simple/graph.py`） | 图定义、子图展开、读写声明校验 |
| simple/run.py（历史路径 `../src/anchor/simple/run.py`） | 调度、任务上下文、轮次、commit 输入和运行状态 |
| simple/node_bridge.py（历史路径 `../src/anchor/simple/node_bridge.py`） | 将调度调用交给 Agent / Op 运行时 |
| node/__init__.py（历史路径 `../src/anchor/node/__init__.py`） | 节点请求和结果契约 |
| node/adapter.py（历史路径 `../src/anchor/node/adapter.py`）、node/agent_runtime.py（历史路径 `../src/anchor/node/agent_runtime.py`） | Agent 执行、Bash 工具、完成信号和执行记录 |
| node/op_runtime.py（历史路径 `../src/anchor/node/op_runtime.py`） | 命令节点执行，不依赖模型循环 |
| node/recovery.py（历史路径 `../src/anchor/node/recovery.py`）、node/context.py（历史路径 `../src/anchor/node/context.py`） | 恢复判断、上下文管理和输出记录 |
| runtime/execenv.py（历史路径 `../src/anchor/runtime/execenv.py`）、runtime/sandbox.py（历史路径 `../src/anchor/runtime/sandbox.py`） | 命令环境、工具路径、只读挂载、网络隔离和取消 |
| serve.py（历史路径 `../src/anchor/serve.py`） | 工作流、运行、文件、控制、Session、Webhook、Responses、定时与时间线 API |
| pilot.py（历史路径 `../src/anchor/pilot.py`） | Pilot 模型执行、控制工具与流式事件编码 |
| session.py（历史路径 `../src/anchor/session.py`） | Session 生命周期、消息与审批记录的持久化 |
| pilot_turns.py（历史路径 `../src/anchor/pilot_turns.py`） | turn 提交幂等、执行身份、事件游标与终态 |
| channel/__init__.py（历史路径 `../src/anchor/channel/__init__.py`） | 长驻平台通道的规范化事件和落盘去重账本契约 |
| [apps/web](../../../apps/web) | 编排和运行共用画布，叠加状态与执行次数 |

Graph 通过节点契约交付任务、接收结果，不直接解释 harness 的内部消息或检查点。mini-swe-agent 已移除。当前依赖的唯一配置来源是 pyproject.toml（历史路径 `../pyproject.toml`），不在文档另维护一份“当前版本矩阵”。历史迁移验证保存在归档中。

## 架构审视与演进方向（2026-10-01）

当前实现已有有价值的核心边界：Graph 定义与 Run 记录分开；Runner 经 `NodeRequest` / `NodeOutcome` 和 `simple/node_bridge.py` 调用节点；Agent 与 Op 执行共用沙箱和运行结果契约；Session、Turn、Library、Graph Run 分别有自己的持久化或资产归属。新功能应延续这些边界，而不是另建执行链。

静态代码审视也发现两处边界压力，作为后续渐进治理方向，不表示本次已重构：

- `src/anchor/serve.py` 同时放置 `Scheduler`、请求处理器、HTTP 路由、Session/Turn 用例、计划和响应处理。`graph_calls.py` 及 `channel/` 中的部分流程还直接访问 Scheduler 的锁、可变状态和内部运行方法。入口协议、应用协调与运行时状态因此耦合在一起。
- `src/anchor/simple/run.py` 同时承载恢复/状态处理、任务和输入组装、节点执行准备及主调度循环。Call、fanout/join 等扩展需要接入同一 Runner 主路径，变化容易集中到它。

目前已有 Python `anchor-graph` 命令和 Rust format-1 Graph bundle：前者从 workspace 直接运行 Python Graph，后者由 Rust Host 加载 `graph.json`、`manifest.json` 和精确 Plugin 资源闭包。`scripts/package_rust_runtime.py` 已能把 release binary 与一个已准入 bundle 组成不含 Python/Anchor 源码的确定性归档；Rust bundle loader 与 Graph CRUD 现通过 `GraphSnapshot::from_authoring` 编译 Python 作者 JSON 的已覆盖子集，包括模块展开、反馈/并行结构、Plugin 引用、`layout` 往返和接口校验。Graph PUT 会从 canonical Plugin catalog 解析被引用的 Plugin，只将经过 catalog 校验的非密钥资源复制进 Graph bundle，并生成资源摘要 manifest。Python 全部作者校验规则和所有既有生产 Graph 的替代验收仍未完成。因此“作者定义兼容加载”和“可由普通用户完整运行所有已有 Graph”是不同状态。

普通用户的 Graph 编辑格式与 Rust 执行 IR 必须分层，具体契约见 [Rust Graph 作者模型与 Runtime IR 契约](rust-graph-authoring-contract.md)。当前 `GraphSnapshot::from_authoring` 是首个 Python 作者格式编译入口；它不是完整 Python parser 的替代品，API 保存/加载只完成兼容子集。Python Graph 仍是生产作者路径。

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

长驻平台通道不运行在 AgentNode 的 MCP 生命周期内。Plugin 的 `channel.json` 由 Library 识别；Rust source-free Host 启动后由 `ChannelSupervisor` 扫描 Graph 节点挂载，按平台只启动一个受监管的 native Gateway，服务正常退出和 SIGTERM 时停止它：主线程信号处理仅发起异步 HTTP shutdown，随后统一清理网关并恢复原信号处理；SIGKILL 不经过此清理。Python legacy 部署仍由 `plugins/wecom/ws_gateway.py` 用 SDK 维护长连接；两条部署路径不应同时写同一状态根。`POST /v1/channels/wecom/events` 按配置的 Graph、回复节点和来源/成员/会话建立独立 Session/Turn；直接调用既有普通 Graph runner，不调用 Pilot。Pilot 与通道共享 Session 持久化实现，但 `/sessions` 只列出没有 Graph/通道绑定的 Pilot 会话，避免企业微信聊天混入 Pilot 会话选择器。`.env` 的 `ANCHOR_WECOM_USERS` 默认拒绝，平台不能选择 Graph 或 Plugin。每条消息对应独立 Run，同一会话新消息取消旧 Run 并等待其退出后接上原生 FileStepStore / continue_run；不同用户的同一 Graph 可以并发。文本、图片、文件和混合消息的附件由网关下载到 `state/channels/wecom/events/<event>`，Graph 以只读 `/in/channel` 读取，路径经过服务端目录校验。按节点查找最近可读历史，上一轮产物及取消时未完成工作通过只读 `/previous` 传递。旧事件重投不重跑，已被新消息替代的回复不回传业务答案；外部副作用不回滚。Run 状态原子发布，Graph 修改/删除及历史 Run 删除受活动执行保护。API 鉴权沿用 `ANCHOR_API_KEYS`，网关使用其中一把密钥。默认助手挂载 `wecom`；其他业务 Plugin 仍需显式挂载，审批 API 未实现。附件经有界文本提取/图片校验后进入原生模型输入，原件继续只读保留。指定回复节点的结构化 summary 经 TurnStore/SSE 持续输出，网关对同一个 stream_id 节流更新，最终可附 PNG/JPEG。长连接不支持 `stream.msg_item`；网关将内部图片字节按官方 init/chunk/finish 接口上传为 `media_id`，再用原回调回复独立图片消息。每张图片沿用 EventLedger 保存发送结果，已确认不重复发送、未知 ACK 不自动重放；上传失败允许重试，上传期间被新消息替代则抑制发送。宿主按节点挂载注入受限 FunctionToolset：主动发送经鉴权 Unix socket 委托唯一网关，回复图片只允许当前节点可读路径；这两个通道工具不受节点 network 开关影响，MCP/业务命令仍受原沙箱限制。主动发送按工具调用 ID 去重、ACK 确认，结果未知不自动重试；入口和出站成员名单分开配置。现有事件账本保存首次接收顺序与会话身份，重连/重启后也抑制旧消息重放。配置与真实验收边界见 [企业微信助手接入](../../wecom-assistant.md)。

## 学术调研与 Plugin 接入

学术调研使用普通 AgentNode。AgentNode 通过 PydanticAI 结构化结果完成；Bash 只是工作区工具。安装可选 Monty 依赖后，CodeMode 作为内部优化自动折叠普通 Plugin/MCP 工具，Harness 控制工具和 Bash 保持原生，Monty 不获得宿主文件、环境变量或时钟访问。Anchor 尚在开发阶段，维护中的 Graph 示例与测试应使用这一统一契约，不维护旧完成协议的双轨路径。

1. [深度学术调研图](../../../examples/graphs/deep-academic-research.json)在 investigator、challenger、reviewer 三个 AgentNode 上挂载 `academic-research` Plugin；这些节点按需使用 Plugin 提供的学术证据能力。
2. 模型注册的工具是 `bash`，由模型构造文献命令。角色指令作为指令文本提供，没有从能力库按需加载说明的机制。
3. `NodeSandbox` 提供普通工作区工具，并由 Plugin 显式挂载登记的 scholarly 工具入口；联网能力另由节点权限决定。Python 路径的 MCP 使用 PydanticAI `MCPToolset`；Rust 路径直接接手同一 Plugin 清单，按 `<plugin>-<server>_<tool>` 注册 MCP 工具，不再引入 Anchor 搜索/调用代理。
4. scholarly/__main__.py（历史路径 `../src/anchor/scholarly/__main__.py`）解析命令，runtime/research_tools.py（历史路径 `../src/anchor/runtime/research_tools.py`）执行搜索、全文读取、引用追踪等操作，以 JSON 返回结果。
5. Agent 消化结果、保存证据并提交文件，Graph 通过质疑和评审组织反馈。

研究方法维护在 [academic-research Skill](../../../plugins/academic-research/skills/academic-research/SKILL.md)，通过 `/tools/scholarly/run` 使用登记的环境。`library.py` 解析资源，调度层注入简短目录并把只读挂载交给节点。Agent 完成与工具调用分离，结果由 Runtime 持久化。

代码中的 harness `capabilities` 指上下文管理、步骤持久化等运行机制，**不是**业务 Plugin。节点层已有组合验证，但默认 Graph 路径尚未注入 `context_capabilities`；Pilot 已接步骤持久化与框架压缩（见下文）。具体证据与体验基线见 [Pilot 体验核查](pilot-experience-audit.md)。

## 已知边界

### Rust-native Runtime 与产品对齐

共享 Kernel 位于 `rust/anchor-runtime`，crate 名为 `anchor-runtime`。Graph 编译、Run 调度、Artifact 与 Sandbox/ToolPort 契约不依赖 Agent 框架；平台 Host 和独立 Graph 宿主调用同一个 GraphRunner。AgentNode 与 Pilot 统一使用 Goose ACP，授权工具通过原生 RMCP 暴露。io-harness/Rig 执行 crate、适配器、可选 feature 和实验原型已从当前源码移除，历史实现及验收可按开发台账与 Git 历史回查。

Host 以 `api` 协议层、`application` 接纳/控制层和 `execution` Runner 接线分工。接纳先保存冻结 Run 与不可变来源元数据，不以相同 digest 猜测 Graph 身份；恢复核对 snapshot/input、Plugin 资源和原 invocation。部署级 writer lease 防止同一状态根双写，暂停和停止在安全边界收束。已完成节点不重跑；旧框架记录不能作为 Goose 的恢复游标，未知外部效果必须核查现场，不能自动重放。

`anchor-platform-session` 保存 Session/Turn、来源归属、Run 关联、Question/Answer 和 UI delivery events，Agent 消息与 compaction 由 Goose 持有。Host 提供 Session 生命周期、幂等 Turn 提交、历史、SSE、停止和续答；普通 Pilot 沿用本机操作员规则，Responses/渠道身份由可信入口绑定。启动恢复按保存的 Run 事实补齐关联与渠道投递，真正孤立的 running Turn 才标为中断，不自动调用模型或重发平台消息。HTTP/Pilot 共用 Graph 修改 guard、admission lease、资源冻结和条件删除契约。

官方 WeCom、Docmost 和 scholarly 工具使用 Rust 二进制与原生 MCP。Host 冻结附件字节、hash、size 和 MIME，仅通过只读 `/in/channel`、`/previous` 或 Plugin 输入挂载交给节点；工具和凭据服从宿主授权。WeCom Gateway 的发现与生命周期由 Host 的 `ChannelSupervisor` 负责，控制 descriptor/socket 在私有状态根管理；真实公网媒体下载与富媒体回传仍未实现。源码内的小型确定性 Graph 回归与真实业务内容验收分开记录。

Python 平台兼容入口仍可代理 Rust Host，但不属于 Rust source-free 发行闭包，也不能与 Rust 服务对同一 Run/Session 状态根双写。生产数据保留、迁移、回滚与目标机部署独立验收，当前候选通过不表示已经切换生产。

#### Goose AgentNode 标准构建（生产部署未切换）

标准 Host 默认选择 Goose，AgentNode 通过固定 Goose v1.53.0 的公开 ACP 直接执行模型，仍使用同一 Rust GraphRunner、OpNode、Host ToolPort、Plugin/MCP、Bubblewrap 与 Artifact；Pilot 接同一 ACP 层。`ANCHOR_RUNNER_AGENT_RUNTIME=goose` 可显式指定，但不再必需；标准构建拒绝 io-harness 模式。Agent 需要固定 binary/hash、显式共享控制网络授权和模型配置；无 Agent 配置仍可执行 Op。旧 invocation 在配置、恢复事实和执行入口均不能冒充 Goose，旧 state root 不自动迁移；未配置 Agent 时允许 Op 使用保留旧事实的根，不接管旧 Agent。生产部署、凭据和数据未切换。实现入口为 `rust/anchor-runner-host/src/goose_acp.rs`。

模型接入读取 `ANCHOR_MODEL_URL/API_KEY/NAME/WIRE_API/ALIASES`，支持显式 chat/responses 路径，Goose 自己调用配置的 Provider，不增加 Anchor 模型代理。未知别名拒绝；model binding 摘要包含 endpoint/wire/实际 model，不含密钥，恢复时不可静默改变。HTTPS 和明确的 HTTP loopback 配置可接入。原生路径没有 Anchor 默认 30 秒任务时限，配置的 wall time、用户取消、协议帧和内存上界仍有效。累计预算未启用，显式请求累计预算会拒绝；图片和跨轮会话接入见后续对应章节。

绑定的 Plugin 复用冻结资源、只读挂载与授权 MCP；prompt 指向可重读的 SKILL/说明文件，Agent 工作写在节点 workspace。模型必须观察业务工具返回的随机回执，再独立调用 `final_result(summary, route, observed_receipt)`；canonical 完成删除辅助回执，保持原 Graph 输出。MCP 真实错误保留 `isError`，不把异常投影成成功。ACP 通知和工具投影保留有界尾部并标记丢弃数量，不因任务较长而使用 spike 的累计事件/调用限制结束任务；完整历史仍归 Goose。

`state/goose-acp/<identity>.json` 保存 version-2 invocation、session、binary/model binding 与完成事实。Goose 本轮助手消息可能在工具返回后才落盘，所以 Anchor 在业务工具执行前以原有 durable fact 写入最近调用名称/参数，工具结算后补结果；它不是另一套历史日志。普通恢复使用同一 ACP Session 和 invocation，提供原生历史及现场观察，要求先核查再继续；存在旧业务观察而本次尚无工具结果时完成被拒绝。中断返回可恢复的 `Stopped` 并保留 cursor，不包装成用户审批或直接 terminal failure；重新运行清除过期 Run error。关闭不提前释放尚未收束的业务 handler，避免同宿主的重叠恢复。

小型真实 Goose + 确定性本地 Provider 已覆盖直接模型传输、回执/完成、无默认短时限取消、命令/路径越权、Rust WeCom Plugin 的 SKILL/MCP，以及副作用发生但 ToolResponse 尚未保存时杀 Host，重启同一 Session 后先查询外部现场再完成。主片还取得隔离的真实 Provider Chat 短图证据，检查逐节点 Artifact、原生历史与 workspace；具体命令、快照和失败均记录于 A114。原有 DeepSeek Responses 配置出现 thinking 上下文回传 400，不能算已兼容；显式 Chat 验收不改 `.env`，不否认 Provider 官方支持 Responses，也不证明已修复该组合。

真实 Provider 的累计网络请求数未由本层代理计量；native `provider_requests` 为 null，`observed_model_requests`/`model_requests` 仅为当前 prompt 留存的 usage 通知数，不含中断前旧 prompt，尾部截断时还可能少于完整通知流。常规 fixture 的零真实模型来自实际脚本与消费校验，不能仅靠 loopback 标记推导。Run trace API 已从保存的公开 ACP 证据与最近工具观察投影文本、命令、结果和未知结果，验证 invocation/Session 身份，拒绝 symlink、损坏和超限文件；不读取 Goose 私有数据库，不把投影当恢复日志。原生问答/删除确认、MCP 图片和 Graph/channel 切片见 A117–A119；完整实时 trace/浏览器、真实 vision、compaction/Plugin/恢复组合仍待验收。共享控制网络不等于 OS outbound 隔离，生产切换、历史迁移和外部发布均未执行。

#### Goose Pilot 标准构建（G2 未全部完成）

标准 Host 的 Pilot 与 AgentNode 默认共用 Goose，不增加独立 Pilot 后端开关。两者复用 `goose_acp/session.rs` 的公开 ACP 初始化、新建/加载会话以及 `configuration.rs` 的模型配置、固定 binary 校验和 Bubblewrap 进程接线；共用 ACP transport 和授权 MCP bridge。Pilot 保留原有十五个产品工具，并通过 MCP 上下文增加 `ask_user` 和 `graph_delete`；不暴露节点 `final_result`、宿主文件系统或终端能力。普通文字回复以 ACP `end_turn` 完结；没有 Anchor 固定 step/token/任务时限，用户取消仍有效。Goose 自己拥有 Agent loop、原生历史和 Provider；Pilot 不是隐藏 Graph。

平台 schema v4 新增 `Turn.goose {scope,session}` 和 owner-aware `bind_goose`，Goose Session ID 保持不透明字符串，不伪造为 legacy 整数 ID。一个平台 Session 的多个 Turn 复用同一 Goose Session；另一个平台 Session/owner 不能认领该 scope，同一 Turn 的 `native` 与 `goose` 互斥。已有 legacy history 不自动转换，原 Goose 事实缺失或 binary/model binding 改变时拒绝新 Turn，不静默换会话或回退执行器。

`<state-root>/platform/pilot/<scope>/goose.json` 只保存版本、binary/model binding、Goose Session 身份和最近一次工具观察，原生历史仍在该 scope 的隔离 `process/` 目录。调用前保存名称/参数且结果为未知，结算后补结果；新输入公开 `session/load` 同一会话，并要求核查最近事实和现场而非重放操作。scope/process 为私有目录，fact/lease 拒绝 symlink；同 scope 非阻塞文件 lease 和既有接纳 gate 保持串行，关闭等待真实 MCP handler 收束，再完结平台 Turn。重启只标记残留 Turn 中断，不自动调用模型。

ACP notification 在 prompt RPC 尚未返回时同步写入平台 UI delivery events，持久化失败使连接 fail closed。文本和工具结果沿现有 chunk 格式投影；`/messages` 按时间顺序读取平台 Turn 的显示事件，不把它们作为另一个恢复历史。SSE `Last-Event-ID` 重放不再次执行，工具权限、Session owner、Graph/Run 来源及 Artifact 仍由已有产品端口持有。小型 fixture 和真实 Chat 两轮（读取随机图 → 启动隔离 Op 图 → 核查 Run/Artifact）见 A115/A116；原生提问/必要删除确认的真实短会话和定向浏览器组合见 A117。长会话压缩、完整浏览器流程以及全部 Run 控制的 Goose 组合仍未验收。

平台 schema v5 在已有业务数据库中新增 Question/Answer 事实，不改变 `TurnStatus` 或唯一 running Turn 的约束。提问时同一 Turn 仍拥有活跃执行，Session 进入 `waiting_user`；用户回答的 CAS、Session 状态和已有 Session/Turn delivery events 在同一事务提交。相同回答重试只返回保存事实，不再次驱动模型；不同回答冲突。API 沿用既有 owner 规则：普通 operator Pilot 是共享本机会话，`responses-` 私有会话按 API key 隔离；本片不改为多租户产品。

Goose 固定版本对应公开 `CreateElicitationRequest/Response` schema；Pilot 初始化广告 form capability，MCP `peer.create_elicitation` 经 Goose 转为 ACP `elicitation/create`。回答保存后才写 ACP response，等待时仍读取 notification/EOF，部分帧不会因回答同时到达而丢失。只接平坦 primitive/enum form，拒绝外部 `$ref`、嵌套或会在原生 MCP 转换中丢失约束的 schema；URL mode 不接受。Graph AgentNode 尚不广告 form capability。本片没有另一套 Agent loop、问答恢复引擎或 Agent 日志。

`graph_delete` 先捕获既有精确内容 precondition，原生问题显示目标、删除范围和摘要。只有用户 `accept` 且 `confirm:true` 才调用同一条件删除应用接口；模型参数不能提供确认。内容/资源变化、拒绝、取消或执行失活均不删除，取得 Graph lease 后再次检查活跃授权。停止/重启在业务事务中废弃 pending Question，新输入加载原 Goose Session 后核查现场；旧确认不重放。Goose pending 是进程内等待，保存回答不等于 ACP 已送达或删除已完成，不承诺跨进程原样恢复。

标准依赖树出口已由 A116 关闭，旧实现和可选框架 feature 已移除；生产 backend/数据未切换。G2c 不代表媒体、渠道、压缩组合、Goose-only 发行版或生产替代全部完成。

#### Goose MCP 图片结果（确定性传输切片）

标准 Kernel 工具结果增加 inline `Image {data,mime_type}`，不引入 Provider 或图像解码依赖。MCP adapter 负责用成熟 base64/image crates 验证并映射原生 Image 和 image blob resource；后者只使用已有 blob，URI 从不解引用、也不授予宿主路径/网络权限。Goose bridge 再校验直接 Rust 工具的媒体边界，用原生 MCP Image block 回传，不能把 base64 包进普通 JSON 文本后称为图片支持。

仅支持静态 PNG/JPEG/WebP，要求 canonical base64、声明 MIME 与真实字节相符、完整解码；单张 10MiB、每次结果总计 20MiB/8 张、20 million pixels 和 128MiB 解码内存上界。动画、损坏、GIF/HEIF/HEIC/SVG/未知 image MIME 明确失败，整个 typed 结果不返回部分成功。raw MCP 结果保留完整内容；渠道附件复用同一 codec 校验，原有数量/上传/冻结权限仍归原层。

含图片结果在小型 metadata/receipt header 后保持 Text/Json/Image 原序；非媒体结果保持原 JSON envelope。Anchor invocation fact 和 `tool_calls` 只保存 MIME/解码字节数/SHA256，避免将大 base64 注入恢复提示；Goose 原生历史及原有 ACP 显示记录保留原图片，不新增 Agent 日志或恢复引擎。带图片的公开 Run trace 另保留 native `contents`。Web 按原序呈现 Text/Image，以 `tool_call_id` 配对交错结果，thinking 单独折叠；只嵌入有界 inline PNG/JPEG/WebP，拒绝 URL/resource/SVG，浏览器解码失败显示不可用，原图查看复用既有 Modal。A122 的 UI 投影浏览器 fixture 检查真实解码/像素与桌面/移动端，不等于实际 Goose 图片到 UI 的完整组合或真实 vision 验收。ACP 单帧 32MiB、通知尾部 64MiB，保留截断标记、取消和 fail-closed；trace 文件读取上界 160MiB。

固定 Goose OpenAI formatter 对 vision-capable model 把工具图片转换为后续独立 user `image_url` 消息，工具文本显示原生 placeholder；非视觉模型会明确省略图片。Anchor 不擅自替换模型，不把可用 MCP 图片接口等同于任意模型都能看图。小型 fixture 用视觉模型名称、真实 Goose/Host/Graph/Plugin/Sandbox 和确定性本地 Provider 检查 bytes/history/receipt/workspace/Artifact 与同 session 恢复；真实视觉 Provider、媒体输入/渠道及完整图片 UI 组合仍待验收。

## 保持的设计原则

复用现有运行链与沙箱边界；图仍是 JSON，节点仍拥有独立工作区，成果仍通过 commit 关联。研究是否结束应由目标、证据和反馈决定，不能用固定 `max_xxx` 充当质量或收敛判断。用户主动停止和显式资源预算是另外的控制需求。

这些原则约束 Plugin 接入，不要求恢复历史上已删除的服务、数据库或图版本发布体系。

## 本机工作周报

`weekly-work-report` 的采集、理解、写作和评审仍是普通业务 Graph；通过评审后的 Docmost 同步节点挂载 `docmost` Plugin。Graph 工作区中的 `local-inputs.json` 由本机操作员按节点 ID 授予具名只读路径，挂载到 `/local-inputs/<name>`；Graph JSON/API 本身无权增加该授权。采集 Op 读取两个 sessions 目录和普通采集脚本，将当次窗口的证据交给后续节点。授权路径随 Run 保存，恢复时授权改变则拒绝继续。图按程序采集→项目理解与选题→写作→读者视角独立评审→门禁分流运行；表达问题退回写作，项目理解与判断问题退回理解。证据缺口通过限定结论处理，不设 blocked 分支。正文围绕项目实质变化，来源单独保留，评审先检查可理解性再核查关键事实。四项评审通过、阻断问题解决且评审对应当前稿件 commit 后才组装 Markdown、来源附录和 SVG；Docmost 发布节点通过附件接口上传 SVG 并插入页面，失败时不更新正文；使用方式见 [每周工作报告](../../weekly-work-report.md)，真实验收以台账 A21 为准。图片下载保留 attachment，并提供图片 MIME 与隔离 CSP，使 Markdown 图片预览可用且不开放脚本或外部资源执行。
`weekly-work-report` 的采集、理解、写作和评审仍是普通业务 Graph；通过评审后的 Docmost 同步节点挂载 `docmost` Plugin。Graph 工作区中的 `local-inputs.json` 由本机操作员按节点 ID 授予具名只读路径，挂载到 `/local-inputs/<name>`；Graph JSON/API 本身无权增加该授权。采集 Op 读取两个 sessions 目录和普通采集脚本，将当次窗口的证据交给后续节点。授权路径随 Run 保存，恢复时授权改变则拒绝继续。图按程序采集→项目理解与选题→写作→读者视角独立评审→门禁分流运行；表达问题退回写作，项目理解与判断问题退回理解。证据缺口通过限定结论处理，不设 blocked 分支。正文围绕项目实质变化，来源单独保留，评审先检查可理解性再核查关键事实。四项评审通过、阻断问题解决且评审对应当前稿件 commit 后才组装 Markdown、来源附录和 SVG；Docmost 发布节点通过附件接口上传 SVG 并插入页面，失败时不更新正文；使用方式见 [每周工作报告](../../weekly-work-report.md)，真实验收以台账 A21 为准。图片下载保留 attachment，并提供图片 MIME 与隔离 CSP，使 Markdown 图片预览可用且不开放脚本或外部资源执行。Rust 已用 reject-only Docmost 替身完成真实 provider 的七节点无发布验收；该证据不等于生产页面发布。


## 本机 RSI Graph

仓库提供 [rsi Graph](../../../examples/graphs/rsi.json) 和 安装脚本（历史路径 `../scripts/setup_rsi.py`）。它复用同一计划存储和 Graph 反馈机制：`collect → audit-context → audit-fanout → 五个专项分支 → audit-join → analyze → review-fanout → 两个独立评审 → review-join → review → gate → publish`。五个领域为 Run、架构代码、Graph、Plugin、依赖/社区；最后一个分支先执行联网 research Op，再执行 dependency-audit。整个流程属于同一个 Run。gate 区分修改综合稿（回 analyze）与重新专项审查（回 audit-context 保存反馈，再 fanout），不设固定模型请求或 Graph 轮数上限。 review 是校验评审 commit 并合并意见的普通 Op，事实/研究与方案/验收/回滚分工，任一未通过不能被另一方覆盖。

collect 动态发现可授权源码与未忽略新文件、全部部署 Graph、已安装 Plugin/工具/Skill/MCP/通道资源和全历史 Run 索引。本周 Run 及按需历史投影包含错误、恢复和 commit/输入关系；不会采集完整聊天/trace/推理。领域索引引导重点读取，源码及 Plugin 证据保留哈希与脱敏标记。Python 字面量和注释脱敏保留可解析语法，目录排除、二进制和授权缺失都有覆盖记录。历史提案从成功 publish 的记录 commit 读取，模型先读简洁 previous-index，再按 ID 回查完整历史。

research 从本次冻结 manifest、可选/开发依赖和包注册表项目链接发现目标，公开 API 主机限定 GitHub/PyPI/npm；记录版本、发布说明与 issue 信号以及限流/失败，未覆盖 Discussions/私有/非 GitHub 社区，不等同于穷尽互联网。Python 清单来自采集解释器，不冒充服务环境。专项输出 findings 与实际读取/未覆盖范围；综合以分支结论为起点，按需追溯证据。gate 检查当前分析评审 commit、join 分支 commit、证据可读取、提案 ID 唯一与历史连续性；这些机械检查不证明报告语义正确。结果写入 Run 的 publish/，包含 audit-manifest.json、报告、提案、来源、评审和门禁。

该 Graph 不写源码、不修改 Graph/Plugin、不重放历史副作用，也不把计划或模型回答当成完成事实。源码和数据根通过 Graph 工作区的 `local-inputs.json` 由操作员只读授权；网络响应有主机白名单、超时和大小限制，失败会进入证据文件。当前实现和真实 provider/长期递归效果的验证状态见 [RSI Graph](../../rsi.md) 和开发台账。

## 独立 Graph 调用

同 Run 的局部并行与独立调用分别表达，具体见下节及 [组合设计](../../graph-composition-design.md)。

下列完整调用与 `session` 语义目前对应 Python 宿主；Rust 宿主已完成 A99 所述的 wait 子图 `input_map/files/result` 转交、真实本机 Plugin/MCP child 执行及 A82 的 wait/detach 生命周期与父子运行投影，并由 A86 补齐本地 child/Artifact 恢复硬化。Rust 仍未接通嵌套调用、真实业务 MCP 或跨存储故障窗口；`call.session` 已接通 wait/detach 与本地 ACK 验收，具体差异以开发台账与 Rust 迁移计划为准。

保留文件内子图展开，同一 Run 内执行；新增 `ops.<name>.call` 在普通 OpNode 上建立独立 Run。`wait` 等待成功并复制显式选择的结果，`detach` 在持久接纳后返回，子 Run 继续独立运行。多来源调用同一个 Graph 可并发；手动/定时入口原有繁忙拒绝规则保留。

调用身份由来源 Graph、Run、完整节点 ID、全局执行轮次确定。控制记录位于 `control/.graph-calls/<identity>/graph-call.json`，目标保存 `admission.json`、冻结 `graph.json`、`run.json` 和只读 `call-inputs/`。服务恢复接纳后尚未启动的运行；Runner 恢复使用 Run 自己的定义快照。模型与命令的执行、提交、恢复仍由现有 Node/Harness 接口承担。已开始但无法确认结果的命令保留 `Uncertain`，不通过创建新 Run 自动重放。

输入常量及 JSON Pointer 映射只传递显式选择的数据；文件仅取调用节点可见的已提交上游快照，目标读取 `/in/call/`。wait 结果复制到调用节点 `result/`，每轮引用写入 `call.json` 并随节点提交；网页从原生历史投影具体父子关系。跨 Graph 祖先递归调用被拒绝，图内反馈循环不受此限制。wait 取消只停止自己的子 Run；detach 接纳后不随父 Run 停止。

`call.session` 是操作员在定义中选择的已有通道会话。后台 Graph 与该会话用户消息串行，后台不能打断聊天；用户新消息可使后台保留原 Run 并让出执行。恢复完成后由既有网关以稳定发送 ID 投递正文，网关沿用原账本处理 ACK 去重和未知投递结果。后台模型完成不等于消息已投递，Rust 的等待调用只在投递状态明确 failed 时失败；网关 ACK 后先在 Session 事件账本写入同一 Run/request ID 的 `graph.call.delivered` 事实，再结算 Rust Run，结算失败保持 `pending` 并按该事实重试，不会因会话随后归档或收件人策略变化而再次发送。完整调用祖先中的会话身份约束跨用户访问，输入参数不能授予会话权限。同一 Plugin 通道可由多个 Graph 挂载，服务只启动一个平台网关。

`/graph-relations` 从已保存定义派生关系；`/graphs` 提供全部 `active_runs`；Run 详情提供逐轮 `calls` 和 `trigger` 来源；`/channel-sessions` 提供已认证操作员可选的通道会话。Rust Host 接纳 Run 时会在同一 Graph admission lease 内读取定义并持久化展开快照；更新 Graph 使用相同短 lease，因此只改变之后接纳的 Run，活动或暂停 Run 仍按自身快照恢复。Graph 创建、编辑和删除共用短 catalog mutation gate；删除会检查当前可运行的 Graph 定义及所有未结束 Run 的冻结快照，存在 `op.call` 引用时返回冲突。Completed、Failed、Aborted 等终态历史 Run 不阻止删除；普通 Stopped Run 仍可恢复，所以会阻止删除；会话 Graph 的整图清理允许已停止且 wait 子链收束的 Run，并清理对应原生会话。目标 Graph 还有其他未结束 Run 时返回冲突。成功删除会清除目标 Graph 自己的 Run 与文件，保留其他 Graph 的 Run，不级联删除或改写调用方。删除已完成调用方时，已删除的 callee 历史不阻止清理。
 A129 后，`/channel-sessions/inbound` 接纳的 channel Turn 可由 `/conversation-runs` 在同一 owner/session/inbound 边界接入 Graph Run；Host 校验 Graph、回复节点、正文、附件冻结 manifest 和 `previous_run`，再持久化 `channel_inbounds`、Turn、Session 与 Run 的关系。Graph 接纳失败会收束仍为 running 的 Turn；Run 完成、失败或停止会投影对应 Turn 终态，启动恢复也会补齐已有 channel Run 的终态。`/runs` 与 `/runs/{id}` 的 `trigger.channel` 公开 `session/inbound/turn`，不公开 owner。该切片仍不包含 gateway 发送循环、完整 `call.session` 交接或公网 WeCom/Docmost 投递。

## 同一 Run 内的局部并行

`fanout` Op 只声明配对 join 节点，普通边声明至少两条独立串行分支。分支中的 Agent/Op 属于同一 Run，各自使用既有工作区、沙箱、Harness 记录和 Git 提交。首期拒绝区域内分支选择/循环、嵌套并行、交叉边、外部进入分支及重叠工作区；区域外的路由和整体反馈循环继续有效。

原调度线程准备输入、保存活动执行身份、接收完成结果及提交状态；工作线程仅调用原 Node 执行接口。`run.json.active` 保存活动节点，`parallel` 保存当前 fanout/join、展开轮次和本轮完成 commit。全部分支成功后 join 写 `join.json`，绑定每个分支的节点、commit、摘要和文件；下游沿用只读输入及祖先快照读取分支产物。分支失败取消同伴且不放行 join；暂停等待当前活动节点结算，停止请求取消并等待活动执行退出。已发生的外部操作不回滚。

恢复保留同一展开轮次和节点执行身份，先读取原生 completion fact，已完成节点不重复执行；命令结果未知仍按原有 Uncertain 规则处理。带并行区域的 Graph 用 `control/.parallel-nodes/<身份摘要>/` 与 `.parallel-traces/<节点摘要>/<执行轮次>.trace.jsonl` 避免名称/轮次冲突；独立调用仍使用 `.graph-calls`。API trace 键使用 `[节点ID,执行轮次]` 的 JSON 字符串，前端兼容旧串行 trace 键。会话停止保留未完成分支文件快照供下一 Run `/previous` 使用；历史步骤路径按各次 Run 自己的 Graph 快照解析。旧 Run 缺省新字段，无需迁移。

WebUI 的“添加节点 → 并行分支”生成配对控制节点及两个 Agent 分支，画布展示配对关系和多个活动节点。示例见 [parallel-audit.json](../../../examples/graphs/parallel-audit.json)。已取得真实 DeepSeek 并行 Agent、join、综合文件的运行证据，以及真实后端浏览器和进程退出恢复验证；具体测试与部署边界以台账 A31 为准。

#### Goose Graph/channel 会话与发送身份（A119）

Graph/channel 继续用既有可信 `ConversationSource` 和前驱链；宿主对规范 bundle 来源、Session 与完整节点 ID 求 scope，不采纳模型 input 里的身份/授权字段。`work/.goose-process/conversations/gc1-<digest>/process/` 保存 Goose 原生历史，`scope.json` 仅保存 binary/model-endpoint/native Session/latest invocation 绑定；Agent 日志和上下文仍归 Goose。标准 invocation Fact v2 增加可选 `conversation_scope`，独立节点旧事实不受影响；缺少已保留会话索引、前驱事实、身份或模型绑定时拒绝替代历史，不迁移 legacy Session。

scope 的外部稳定私有文件 lease 串行化执行和删除，拒绝 symlink；Fact 先持久化，再推进 index，模型调用前两者已落盘。公开 `session/new/load` 返回 Session 后，先保存 Fact 再更新 index；只允许从对应可信 Fact 补齐中断的 index 写入，不能替换已知不同 Session。同 Run 循环优先找同节点前一 invocation；跨 Run 跳过节点时找可信链最近的原生事实。新轮 evidence 的 `continuation` 与同 invocation `resume` 明确区分，业务 workspace/Artifact 仍逐 Run/调用独立；前驱现场冻结为只读 `/previous`，历史工具沿已有 trace 端口读取。

前驱 `running` 不被猜成已停止。旧 Host 被杀后，先通过原 `/stop` 将旧 Run 结算且不调用模型，才接纳新消息；新轮保留未知业务观察，Agent 核查现场并取得当轮回执后继续。后继存在时普通旧轮 resume 仍拒绝；仅已接纳且 pending 的 `call.session` 可在可信后继全部结算后继续，具体边界见 A122。单条会话 Run 删除仍拒绝，整 Graph 删除租约下清理所有相关 scope，其他 Graph 保留。

固定 Goose 的 MCP `_meta` 提供 `agent-session-id` 和 `agent-tool-call-request-id`。Bridge 校验真实 Session 与 durable Fact 一致、身份唯一且有界，在外部调用前保存 `tool_observation.native_tool_call`，再用 task-local 小契约交给已有 ChannelTools。发送 request ID 绑定完整 InvocationKey、原生 Session/tool-call ID 和工具名；缺失/错绑/模型伪造拒绝连接，同正文两次独立原生调用生成不同 ID。私有 Unix descriptor、userid 授权、取消、ACK 和未知结果不自动重发契约不变；legacy 仅显式路径保留原 attempt 身份。

授权 reply node 的冻结附件沿现有 `/in/channel` 只读输入；图片经共享 codec 校验，以 ACP 原生 image block 送入当前 prompt，不授予上传来源宿主路径。确定性 suite 验证真实 image_url 字节，不表示默认模型有视觉能力。A119 七套 44 场景/44 份证据、真实 Chat 两轮/重启/四份 Artifact 通过；真实渠道发送、vision、摘要增量、媒体 UI、长会话压缩/Plugin/恢复组合和生产切换仍未验收。

#### Goose 原生压缩与现场恢复（A121）

固定 Goose v1.53.0 的普通 ACP 循环在 prompt 入口检查 context usage 阈值；工具回合遇 Provider 的 `context_length_exceeded` 时，也由 Goose 原生摘要后继续同 prompt。Anchor 不开启实验 `unrolledAgentLoop`，不实现另一套窗口/摘要/恢复，也不以强制 stop/resume 作为长任务超窗恢复的要求。确定性 fixture 仅控制模型响应、usage 和标准 HTTP 错误；测试只读原生 SQLite，产品恢复仍走公开 ACP。

压缩保留原始消息及用户可见性，只将旧消息移出 agent-visible context；原生摘要和 continuation 仅供 Agent 使用，不替换平台 Session 的显示消息、Question/Answer、SSE 游标或业务事实。摘要中出现旧工具结果不等于取得新的业务回执；恢复后原 receipt 不能完成节点，Agent 重读只读 Plugin、workspace/外部现场后取得 fresh receipt，再走原有 final_result/Artifact 契约。取消摘要或进程中断不虚构 completion，不重放既有副作用；原生 Session/invocation 身份不变。

`goose_compaction` 的四场景和 `goose_pilot_compaction` 的三场景已纳入 Rust 低成本回归，核查真实 Host/Runner/Goose/MCP/Sandbox、原生可见性、现场、Artifact 及取消/重启；A121 的九套取得 51 场景/51 份证据和真实 Chat 三轮压缩/重启短验收，A122 历史入口扩展为十一套 57 场景；A124 当前统一入口为十二套 59 场景，并纳入 Rust 文本 gateway ACK 和安装 Plugin 小图。它们不证明无限长度或多次压缩、摘要 Provider 错误、提问/媒体组合均可用。Pilot 用户输入仍包装为业务 prompt，`/compact` 未直接暴露为原生 slash command；没有将该未实现入口标为通过。完整媒体 UI、渠道 Session supervisor、公网投递、独立发行候选版及生产边界继续按台账验收。

#### Goose Run 实时 trace 与后台接续（A122）

AgentNode 的 `session/prompt` 使用既有 observed transport，活跃 `GET /runs/{id}` 从进程内 RAII 投影读取已收到的原生 ACP 文本、思考、媒体和工具状态。Registry 以 canonical invocation fact 路径隔离并校验真实 Session；复用 transport 的 4096 条/64MiB notification tail，丢尾明确标记，不成为另一套 Agent 历史或恢复引擎。Guard 持有到既有 evidence/fact 保存完成，之后回到持久显示投影；取消/协议错误也保留已观察通知。固定 Goose 未提供 `final_result` 参数逐 token 更新，因此普通 assistant draft 不是 canonical completion 或渠道 summary 增量。

后台 `call.session` yield 后，同 Session 的前台 Run 可以成为最近的原生执行者。应用层仅对 pending call、匹配 Graph call/context/binding，复用 `conversation_chain` 找出可信且已结算的后继；Node adapter 只把它们加入原生前驱候选，不改逻辑 `previous_run` 或只读 `/previous`。Scope 从授权候选中匹配实际 latest native fact，仍校验完整 invocation、Session、binary/model binding。恢复保持原 Run/invocation/Session/frozen input，旧 receipt 拒绝，Agent 核查现场取得 fresh receipt 后才完成；错误身份和普通旧 Run 越过后继的恢复仍拒绝。四个真实 Host/Goose 本地 Provider 场景已验收，但完整渠道 supervisor、公网投递和生产切换没有因此关闭。

#### Rust Library 安装与学术读取（A123 确定性切片）

`anchor-library` 负责操作员 Plugin 安装，不执行 Plugin、不授予路径/网络/OAuth 权限，也不修改已有 Graph bundle。Rust Host 的 `POST /plugins/install` 接收 `source/id/replace`，仅接受无凭据的 HTTPS GitHub tree URL；本地目录安装只在操作员 CLI/library API 提供。原清单或 `.codex-plugin/plugin.json` 经现有 FilePluginCatalog 验证并扁平化，资源先在同文件系统私有暂存目录落盘，再使用 Linux NOREPLACE/EXCHANGE 发布；失败回滚，无法确认持久性时保留暂存并返回 storage error。Git argv 不经 shell，不使用宿主凭据/config，进程超时后 kill/reap。

Host 安装与 Graph 冻结共用 catalog mutation gate；blocking 安装任务持有 guard，因此 HTTP 取消不提前解锁。独立安装 CLI 还持有 `plugins/.install.lock` 的跨进程独占 lease，Graph 冻结取得同文件的非阻塞共享 lease；安装中返回 conflict，不生成混合资源快照。冻结后的 Graph 仍读取自己的资源和摘要，Library 替换不改变已有 Graph/Run。普通 catalog 展示读取不持有该 lease，非协作的宿主目录修改不受保护。仓库/文件/深度总额度尚未设置，不将 Git 超时当资源配额；当前安装仅适合操作员审查的来源。Rust Host 的 OAuth 路径现基于 owner-scoped Library 记录提供状态/撤销、protected-resource 与 authorization-server metadata discovery、动态 public-client 注册或 manifest `oauth_client_id`、短时持久 state/PKCE S256、公开 callback 和 authorization-code exchange；authorization/refresh 请求传递 RFC 8707 resource。Run metadata 冻结 API caller owner，schedule 使用 `local`，Graph child 继承 parent owner；Agent MCP connect 前从同一 Library 取 token，过期时 refresh，缺授权、owner 不匹配或节点无 `network:true` 均 fail closed。pending transaction 与 token 文件使用私有权限和原子持久化，callback 一次性消费，token 不进入 Graph bundle/API projection，MCP Debug 会脱敏。Host 侧 discovery、registration、code exchange、refresh 禁用环境代理和 HTTP redirect；每次请求先解析 DNS，再将仅含公网 IP（本地开发时显式 loopback 除外）的地址固定到该次 HTTP client，阻断私网与 DNS rebinding 绕过。确定性本地 provider fixture 验证 discovery、registration、PKCE exchange、owner 隔离、callback 重放拒绝及 loopback refresh；真实 provider 兼容、真实公网 TLS、生产 callback/proxy 和 refresh-provider 行为仍未验收。`ANCHOR_OAUTH_REDIRECT_URI` 必须配置为指向 Host `/oauth/callback` 的 HTTPS URL，loopback HTTP 仅用于本机开发。Python 兼容入口的安装/OAuth 所有权未改，真实 GitHub 安装也未由本片验收。

`anchor-scholarly` 保留独立官方 CLI 归属，增加 HTML/PDF/text 读取、八项有序批量读取及 OpenAlex 引用链，不调用 Python。实际成熟 parser 提取、40 页/24,000 Unicode 字符窗口、损坏 PDF、逐项失败、公共 HTTPS/DNS pinning/redirect 边界已用本地 transport 验证；格式、重复 URL 顺序及共享 I/O deadline 与 legacy 的差异见 crate README。PDF 内部解压和 parser CPU/memory 不具有硬配额，I/O 超时不终止解析任务。活跃 Plugin、`/tools/scholarly/run`、真实公共 API/TLS 和模型内容验收尚未替换或关闭。

#### Rust 文本渠道 transport 与发行 builder

`anchor-wecom-gateway` 是独立入口适配器，只持有长连接及投递事实，不创建 Session/Turn、Run 或 Agent loop。固定 WebSocket subscription、heartbeat、匹配 ACK、有界退避重连与私有 Unix NDJSON control 共用既有 Host sender 契约；只接受私聊文本及纯文本 mixed，群聊、媒体 callback、mixed 非文本项和 webhook 媒体显式拒绝，不静默丢失附件。Host webhook 已绑定 channel Session/Turn 与 Graph，更新消息替换并停止前一 Run；Host 返回 superseded 或 Gateway 发现旧回复时均不投递旧回复，receipt settlement 保留 suppressed 事实。Rust Host 已按已挂载 Plugin 自动发现单一平台声明，校验必需凭据，启动并监控 Gateway，异常退出后延迟重启，并在正常 shutdown/SIGTERM 时发送 SIGTERM、等待回收和清理 control descriptor/socket；已有不同 descriptor 配置会 fail closed。Host 默认从 state root 的 `channels/wecom/control.json` 读取 descriptor，显式环境路径只作为同一路径的兼容覆盖，不能与自动发现结果冲突。附件下载/提取/上传及富媒体回复仍未接线。

新 native 私有状态目录为 0700，descriptor/socket/database 为 0600；稳定文件 lease 保证单 owner，NOFOLLOW 拒绝 symlink/special/hardlink。SQLite 在实际发送前绑定 request ID、内容/身份摘要和 wire ID 为 unknown；只有匹配成功 ACK 才 confirmed。未知、超时、拒绝、断连、SIGKILL 与晚到 ACK 不触发自动重发，confirmed 相同请求幂等返回。未知不等于失败且无 exactly-once 承诺。持久事实绑定 bot/endpoint，control token 轮换保留事实；旧 Python events.sqlite 拒绝接管并保留。回调去重后中断不重新执行 webhook；未发送 ready 回复在重启后核查当前入站授权与较新消息身份。

Host 启动恢复先把遗留 channel delivery 的 `sending` 收敛为 `unknown`，再扫描已持久化的终态 Run，按保存的 Run 结果和受信 channel metadata 幂等补建 WeCom reply delivery；该过程不重新执行 Graph、AgentNode 或模型请求。只有仍属于当前 inbound 的完成 Run 才能建立 delivery；已被更新消息替代的 inbound 不发送旧回复，若其 Turn 尚未结算则随后按孤立 running Turn 规则中断。Gateway 重启只重试处于 `unknown` 且没有 durable reply claim 的 inbound；已有 claim 的入站 fail-closed，不重新调用 webhook。该切片关闭 Run 完成到 Host 建立 delivery 的崩溃窗口和无 reply claim 的 webhook 恢复窗口，但不提供外部投递 exactly-once，也不等于公网 WeCom 或完整渠道 supervisor 验收。

`anchor-distribution` 是操作员 Rust builder，调用同一 FileGraphBundleLoader，不执行输入 binary 或新增 Runner。包只带给定 ELF Host、固定 Goose v1.53.0、精确声明的 Graph/Plugin 资源和显式工具/Web；资源原子私有快照与发布前身份/内容复查，归档顺序、uid/gid、mtime、mode 和 gzip header 确定。逐文件 SHA256、平台、Plugin pins 和环境变量引用记录在 runtime-manifest；它不是签名，也不能从任意 ELF 证明 Host 无 legacy feature。标准 Host 的 normal/build/dev 依赖闭包另行验证。

源码/凭据/状态路径、symlink/hardlink/special、未知 bundle 文件或空目录均拒绝；JSON/TOML/YAML 的已知凭据字段结构化检查，保留未展开的环境引用。该过滤不是任意文本/二进制秘密发现，输入需要操作员审查；ELF 共享库、系统 Git/Bubblewrap 与外部服务不自动打包，工具放入 bin 不自动扩大授权。输出用 NOREPLACE 原子发布，不覆盖并发赢家；发布后 durability 不确定明确返回 uncertain。实际解包 Host/Goose 的小图停止、重启、同 Session 核查续行和只读 Artifact 已验收；这不是无 Python OS、release candidate、真实模型或生产切换验收。
