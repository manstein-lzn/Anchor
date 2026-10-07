# 当前架构

2026-10-06 后续目标已确定为 [Goose-only Runtime](goose-runtime-migration.md)，预算控制暂不实施。标准 Rust Host/Kernel 已默认 Goose，正常 build/test 依赖闭包不含 io-harness/Rig；旧源码仅在显式 legacy feature 下保留。AgentNode、Pilot 续聊以及原生提问/删除确认切片已有 A114–A117 证据；A118 的 MCP 图片结果和 A119 的 Graph/channel 跨轮、原生发送身份、冻结图片输入通过真实 Goose/确定性 Provider 验收。A119 另取得默认 `deepseek-flash` Chat 两轮/Host 重启/Artifact 证据；真实 vision 与真实渠道投递仍未验，不再只是框架候选验证。完整产品对齐、无 Python 隔离发行和生产配置/数据切换仍未完成。

本文说明当前实现，不是完整产品目标。产品目标和待讨论需求见 [产品与系统架构](product-architecture.md)，当前工作与验收见 [开发台账](pilot-development-plan.md)；Plugin 的当前格式与边界见 [Plugin 设计](plugins.md)；操作方式见 [使用指南](usage.md)。

## 核心模型

2026-10-06 已确认后续目标为 Rust 平台服务端和官方工具/集成，WebUI 保持现有实现。该决定不是当前纯 Rust 平台已完成；现有 Python/混合部署边界继续按本文描述。目标与迁移规则见 [Rust 平台交付决定](rust-platform-target.md)。

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
Python 平台/Pilot：WebUI → serve.py/Session/Turn → PydanticAI + Harness
Rust Host Graph：React/Rust API → anchor-runtime GraphRunner
  → io-harness HostNodes AgentNode / Bubblewrap Op
两条实现线 → Anchor Graph/Run/Artifact/Sandbox/Plugin 事实与对象页面
```

Python Pilot 与 Rust Host 当前并存；它们共享产品不变量，但不对同一 Run 双写。Rust 迁移按 R7→R8→R9 推进，不能把 Python P2/P7 或 Rust A79/A80 的单条证据直接称为另一条实现线已完成。

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

长驻平台通道不运行在 AgentNode 的 MCP 生命周期内。Plugin 的 `channel.json` 由 Library 识别；Anchor 服务启动后由 `ChannelSupervisor` 扫描 Graph 节点挂载，按平台只启动一个受监管的 WebSocket 子进程，服务正常退出和 SIGTERM 时停止它：主线程信号处理仅发起异步 HTTP shutdown，随后统一清理网关并恢复原信号处理；SIGKILL 不经过此清理。`plugins/wecom/ws_gateway.py` 用 SDK 维护长连接，把可信事件转换为 `ChannelEvent`，用 `EventLedger` 去重并保存回复。`POST /v1/channels/wecom/events` 按配置的 Graph、回复节点和来源/成员/会话建立独立 Session/Turn；直接调用既有普通 Graph runner，不调用 Pilot。Pilot 与通道共享 Session 持久化实现，但 `/sessions` 只列出没有 Graph/通道绑定的 Pilot 会话，避免企业微信聊天混入 Pilot 会话选择器。`.env` 的 `ANCHOR_WECOM_USERS` 默认拒绝，平台不能选择 Graph 或 Plugin。每条消息对应独立 Run，同一会话新消息取消旧 Run 并等待其退出后接上原生 FileStepStore / continue_run；不同用户的同一 Graph 可以并发。文本、图片、文件和混合消息的附件由网关下载到 `state/channels/wecom/events/<event>`，Graph 以只读 `/in/channel` 读取，路径经过服务端目录校验。按节点查找最近可读历史，上一轮产物及取消时未完成工作通过只读 `/previous` 传递。旧事件重投不重跑，已被新消息替代的回复不回传业务答案；外部副作用不回滚。Run 状态原子发布，Graph 修改/删除及历史 Run 删除受活动执行保护。API 鉴权沿用 `ANCHOR_API_KEYS`，网关使用其中一把密钥。默认助手挂载 `wecom`；其他业务 Plugin 仍需显式挂载，审批 API 未实现。附件经有界文本提取/图片校验后进入原生模型输入，原件继续只读保留。指定回复节点的结构化 summary 经 TurnStore/SSE 持续输出，网关对同一个 stream_id 节流更新，最终可附 PNG/JPEG。长连接不支持 `stream.msg_item`；网关将内部图片字节按官方 init/chunk/finish 接口上传为 `media_id`，再用原回调回复独立图片消息。每张图片沿用 EventLedger 保存发送结果，已确认不重复发送、未知 ACK 不自动重放；上传失败允许重试，上传期间被新消息替代则抑制发送。宿主按节点挂载注入受限 FunctionToolset：主动发送经鉴权 Unix socket 委托唯一网关，回复图片只允许当前节点可读路径；这两个通道工具不受节点 network 开关影响，MCP/业务命令仍受原沙箱限制。主动发送按工具调用 ID 去重、ACK 确认，结果未知不自动重试；入口和出站成员名单分开配置。现有事件账本保存首次接收顺序与会话身份，重连/重启后也抑制旧消息重放。配置与真实验收边界见 [企业微信助手接入](wecom-assistant.md)。

## 学术调研与 Plugin 接入

学术调研使用普通 AgentNode。AgentNode 通过 PydanticAI 结构化结果完成；Bash 只是工作区工具。安装可选 Monty 依赖后，CodeMode 作为内部优化自动折叠普通 Plugin/MCP 工具，Harness 控制工具和 Bash 保持原生，Monty 不获得宿主文件、环境变量或时钟访问。Anchor 尚在开发阶段，维护中的 Graph 示例与测试应使用这一统一契约，不维护旧完成协议的双轨路径。

1. [深度学术调研图](../examples/graphs/deep-academic-research.json)在 investigator、challenger、reviewer 三个 AgentNode 上挂载 `academic-research` Plugin；这些节点按需使用 Plugin 提供的学术证据能力。
2. 模型注册的工具是 `bash`，由模型构造文献命令。角色指令作为指令文本提供，没有从能力库按需加载说明的机制。
3. `NodeSandbox` 提供普通工作区工具，并由 Plugin 显式挂载登记的 scholarly 工具入口；联网能力另由节点权限决定。Python 路径的 MCP 使用 PydanticAI `MCPToolset`；Rust 路径直接接手同一 Plugin 清单，按 `<plugin>-<server>_<tool>` 注册 MCP 工具，不再引入 Anchor 搜索/调用代理。
4. [scholarly/__main__.py](../src/anchor/scholarly/__main__.py)解析命令，[runtime/research_tools.py](../src/anchor/runtime/research_tools.py)执行搜索、全文读取、引用追踪等操作，以 JSON 返回结果。
5. Agent 消化结果、保存证据并提交文件，Graph 通过质疑和评审组织反馈。

研究方法维护在 [academic-research Skill](../plugins/academic-research/skills/academic-research/SKILL.md)，通过 `/tools/scholarly/run` 使用登记的环境。`library.py` 解析资源，调度层注入简短目录并把只读挂载交给节点。Agent 完成与工具调用分离，结果由 Runtime 持久化。

代码中的 harness `capabilities` 指上下文管理、步骤持久化等运行机制，**不是**业务 Plugin。节点层已有组合验证，但默认 Graph 路径尚未注入 `context_capabilities`；Pilot 已接步骤持久化与框架压缩（见下文）。具体证据与体验基线见 [Pilot 体验核查](pilot-experience-audit.md)。

## 已知边界

### Rust-native Runtime 与产品对齐

2026-10-05 用户已授权按 [产品对齐工作包](rust-rig-migration-plan.md#当前状态2026-10-06) 分批推进，替代此前仅评估二进制交付的冻结范围。Python `serve.py` 默认仍使用 legacy Runtime；显式配置 `ANCHOR_RUNTIME_BACKEND=rust` 时，Graph CRUD、Run 接纳/控制、Artifact 读取以及 Plugin catalog/detail/file 只读路由经公共 HTTP 委派同一 Rust Host。Plugin 安装和授权仍由既有 Python Library 路径持有；这项读取路由接线不表示 Library 写入所有权已迁移。显式 Rust backend 下，计划 CRUD、tick 与 timeline 投影也由 Rust Host 负责并复用既有 `state/schedules.json`；Python legacy backend 仍由 Python Scheduler 管理计划。Python 继续持有 Session/Turn，不为 Rust Run 启动 Python Runner。普通 Pilot、文字 Graph/通道 Session、渠道附件/原生图片输入、主动发送和 `call.session` 基础 wait/detach 已通过公共端口或真实 provider 验收；通道摘要增量仍待接通，A128 仅接入 Rust OAuth owner-scoped 状态/撤销事实并明确拒绝 token body，浏览器同意、真实 provider exchange/refresh 和 OAuth MCP 执行仍待接通。Rust 核心以二进制 Runtime 与原 Graph/Plugin 组合交付，外部工具可以继续使用 Python/Node。以下区分当前代码能力与已验收边界，不能将组件接线等同于整个平台替换。

`rust/anchor-io-harness-runtime` 是 io-harness 0.86.0 的 canonical workspace crate，提供 Rig Provider adapter、Anchor ToolPort/SQLite backend、NodeRequest 执行器和 Graph `NodeExecutionPort`。`HostNodes` 的生产 AgentNode 已切至 io-harness 唯一 loop；Op.run 仍走 Anchor Bubblewrap。io-harness 管理 Agent 上下文、compaction、SQLite checkpoint 与工具效果恢复；Rig 仅作模型 Provider transport；Graph/Run/Artifact/Plugin/Sandbox 事实继续归 Anchor。旧 `experiments/io-harness-*` 入口是兼容 wrapper，不含第二份实现。NodePort 在专用 blocking thread/current-thread Tokio runtime 执行非 `Send` Harness Store，异步解析冻结 PluginBinding 并组装 owned ToolPort。Harness 原生文件/shell/exec 等内建工具被 `ToolMask` 屏蔽；Anchor `anchor_run` 经 Bubblewrap，输入只读挂载精确 Artifact snapshot。AgentNode 完成经 Rig 原生 `final_result` 输出工具提交，Provider adapter 将参数确定性地转换成内部完成值，再由 Harness schema 本地校验和反馈纠错；普通文本即使是合法 JSON 也不能完成新的模型回合，不要求 Provider 支持 `response_format`。GraphRun format 7 的 `WaitingRecovery` 保留原 Graph cursor，并按 `InvocationKey + io-harness run/attempt ID` 展示未决工具名、step 与开始时间；工具参数不由 Harness attempt journal 保存。普通 `/resume` 在 Run/Graph lease 下为每个未决 attempt 写入明确的恢复 observation，说明工具结果未记录、外部效果未知并要求 Agent 先核查现场，然后继续同一 Harness run；不自动重放工具，也不把底层 Retry/Completed/Abort 暴露为用户流程。内部 `/recovery` API 保留兼容性，但不是主要用户入口。若 Run 已保存为 `running`、宿主却在注册活动任务前退出，重复 `/resume` 或详情页“继续”可恢复同一 cursor。恢复上下文与 Harness 账本不构成外部副作用 exactly-once。provider-free Graph/NodePort、HTTP 恢复投影、前端普通继续 E2E 及真实 provider Completed 恢复 smoke 已验证；跨存储故障注入仍未验收。多个并行 attempt 会逐项写入同一恢复上下文；Run 终止时未决调用只读保留。真实 `deepseek-chat` 多节点 Op→Agent→Op、HTTP MCP 本机 fixture、Bubblewrap 和 Artifact 验收通过，证据 `.local/rust-multinode-lyk86avx/evidence.json`；A79 Completed 证据为 `.local/rust-recovery-cqifxcyz/evidence.json`。精确累计 `max_provider_requests`、渠道图片输入见 A97；通用 Graph 图片接纳、外部业务 MCP 和 reasoning/富内容 provider 兼容仍未完成。逐节点模型选择通过部署级 alias registry 接入，具体规则见下文。
`rust/anchor-io-harness-runtime` 是 io-harness 0.86.0 的 canonical workspace crate，提供 Rig Provider adapter、Anchor ToolPort/SQLite backend、NodeRequest 执行器和 Graph `NodeExecutionPort`。`HostNodes` 的生产 AgentNode 已切至 io-harness 唯一 loop；Op.run 仍走 Anchor Bubblewrap。io-harness 管理 Agent 上下文、compaction、SQLite checkpoint 与工具效果恢复；Rig 仅作模型 Provider transport；Graph/Run/Artifact/Plugin/Sandbox 事实继续归 Anchor。Rig 的 retry verdict 由 Provider adapter 保留：`is_retryable()` 为真的 transport error 映射为 io-harness `Transport`，不可重试的 request error 映射为 `Request`，从而由 io-harness 原生有界策略重试明确可重试的请求。该边界不把 terminal 错误变成重试，也不改变未知工具副作用的恢复规则；工具效果不确定时继续 fail closed，不因 Provider transport retry 自动重放工具。旧 `experiments/io-harness-*` 入口是兼容 wrapper，不含第二份实现。NodePort 在专用 blocking thread/current-thread Tokio runtime 执行非 `Send` Harness Store，异步解析冻结 PluginBinding 并组装 owned ToolPort。Harness 原生文件/shell/exec 等内建工具被 `ToolMask` 屏蔽；Anchor `anchor_run` 经 Bubblewrap，输入只读挂载精确 Artifact snapshot。AgentNode 完成经 Rig 原生 `final_result` 输出工具提交，Provider adapter 将参数确定性地转换成内部完成值，再由 Harness schema 本地校验和反馈纠错；普通文本即使是合法 JSON 也不能完成新的模型回合，不要求 Provider 支持 `response_format`。GraphRun format 7 的 `WaitingRecovery` 保留原 Graph cursor，并按 `InvocationKey + io-harness run/attempt ID` 展示未决工具名、step 与开始时间；工具参数不由 Harness attempt journal 保存。普通 `/resume` 在 Run/Graph lease 下为每个未决 attempt 写入明确的恢复 observation，说明工具结果未记录、外部效果未知并要求 Agent 先核查现场，然后继续同一 Harness run；不自动重放工具，也不把底层 Retry/Completed/Abort 暴露为用户流程。内部 `/recovery` API 保留兼容性，但不是主要用户入口。若 Run 已保存为 `running`、宿主却在注册活动任务前退出，重复 `/resume` 或详情页“继续”可恢复同一 cursor。恢复上下文与 Harness 账本不构成外部副作用 exactly-once。provider-free Graph/NodePort、HTTP 恢复投影、前端普通继续 E2E 及真实 provider Completed 恢复 smoke 已验证；跨存储故障注入仍未验收。多个并行 attempt 会逐项写入同一恢复上下文；Run 终止时未决调用只读保留。真实 `deepseek-chat` 多节点 Op→Agent→Op、HTTP MCP 本机 fixture、Bubblewrap 和 Artifact 验收通过，证据 `.local/rust-multinode-lyk86avx/evidence.json`；A79 Completed 证据为 `.local/rust-recovery-cqifxcyz/evidence.json`。精确累计 `max_provider_requests`、渠道图片输入见 A97；通用 Graph 图片接纳、外部业务 MCP 和 reasoning/富内容 provider 兼容仍未完成。逐节点模型选择通过部署级 alias registry 接入，具体规则见下文。

在 `codex/rust-rig-runtime` 上，`rust/anchor-runtime` 保留 GraphRunner、Run/Checkpoint stores、fanout/join kernel 与 Bubblewrap adapter；AgentNode 生产执行由 `rust/anchor-io-harness-runtime` 的 HostNodes 接管，Rig 只作 Provider transport，不再作为 Agent loop。`anchor-runner-host` 的 stdio `start_bundle` 与独立 Axum 入口调用同一 Runner；format-1 bundle loader 固定读取 manifest/graph 并核对 Plugin 资源摘要。宿主现支持串行多节点 Agent/Op.run、同 Run 内一一配对非嵌套 fanout/join，以及 `Op.call` wait/detach 生命周期。wait `Op.call` 已支持严格的 `input_map`、父已提交文件 `/in/call` 只读交接和 child `result` 文件回传；standalone/framed parent 在 child admission 前保存来源 metadata。普通 Op 路由在成功退出后读取首个非空 stdout 行 `ANCHOR_ROUTE: <target>`，并把允许出口放入 `ANCHOR_ROUTES`；Sandbox 将同一 Rust binary 作为原生 `anchor-route` helper 只读挂载，不依赖 Python helper。`Op.run` 原字符串交给已授权的 `sh -c`，保留变量、case、管道和重定向语义；节点 network 意图由既有 Sandbox 宿主授权约束。Host 通过作者编译入口接受含 `graphs` 的原 JSON 模块定义，不要求用户手工展开。wait child 复用同一 GraphRunner/RunStore/ArtifactPort，暂停后可独立继续并在完成后接回仍等待的父Run；detach admission 后立即独立后台执行，服务启动仅接续 metadata 标明 detach 且 Ready 的 child，未知 Running 不重放。父 stop 取消 wait child、不影响 detach child；child 按 identity 独立 lease，可同时执行多个同 target 调用。Run metadata 和 detail.calls 投影父子关系及 active 状态；Graph CRUD 对所有存在未完成 Run 的 Graph 拒绝。child 来源 metadata 存 parent Run/Graph/digest/call node/invocation/mode/root Run，metadata v1 兼容读取但旧记录不触发 detach 恢复。A86 已补齐本地恢复硬化：可恢复 child 才重入，终态/等待恢复 child 不重复派发，未知 child 事实映射为 `Uncertain`；Artifact 输入/结果半发布和篡改均 fail closed。provider-free 四边界通过，真实 DeepSeek Flash Responses 仅在已观测的 child-write 与 parent-completion 边界通过；仍不支持 `session`、嵌套调用、真实业务 MCP、跨存储故障窗口和外部副作用 exactly-once。

宿主节点执行、文件事实、Agent命令工具分别位于 `node_host.rs`、`artifacts.rs`、`node_tools.rs`。每 invocation 使用独立工作区；同 Run 同节点的新回访从输入祖先中最近已提交快照播种，重开同一 invocation 保留未提交工作，不跨 Run 继承。输入 Artifact 提供派生的只读 Git view，HEAD 固定于该快照，只含同节点祖先；fs2 文件/manifest 仍是唯一权威，Git view 按需生成并验证，忽略 Agent 自带的根 `.git` 配置和 hooks。部署需提供系统 Git。文件快照由 ArtifactPort 独立复制、摘要校验及 fsync/rename 发布为 fs2 commit，保留 fs1 读取。Coordinator 通过类型化冻结上下文传入 Node/Fanout/Join 及精确父 commit；宿主生成控制文件，并沿不可变父关系以 `/in/<node-id>` 只读挂载祖先文件。模型输出不能授予文件访问权；最近祖先优先，同深度同名冲突、跨 Run、摘要不符及损坏谱系拒绝。Agent 的 `anchor_run` 同样经过宿主命令授权与 Bubblewrap；参数/准入拒绝返回明确 not_executed 供模型修正，取消或未知执行错误不伪称成功。Agent/Op 执行前持久化事实；未知 mutating tool effect 会让同一 Run 保持待恢复，普通 `/resume` 把未决工具与结果未知的事实交给 Agent 核查，然后在原 Harness run 上继续，不自动重放。内部恢复决策 API 只保留兼容性，不是主要用户流程。Op.started、没有 Harness cursor 的 Agent.started 及冲突 backend 事实仍 fail closed。宿主尚不支持精确累计 `max_provider_requests` 或 Graph 图片输入；声明式 Agent `reads/writes` 已可进入作者编译和 Host admission，并由 Graph 输入快照传递，仍未提供逐文件写权限限制；没有默认16轮研究上限。

当前 HostNodes 的 Agent 完成通过原生 `final_result(summary, route?)` 提交，额外字段忽略、null route 归一为未提供；io-harness `TaskContract` schema 本地校验并以 Harness 反馈纠错，Anchor 最终仍校验 route；早期 Rig executor 的结构化输出能力作为历史实现保留。Rust Host 通过 `ANCHOR_MODEL_ALIASES` 按 Python 规则选择节点模型：已声明别名映射为同一部署 endpoint 上的实际 model，未知引用 fallback 默认；默认 NAME 为 `default`、WIRE_API 为 `responses`。每 invocation 将请求模型配置指纹冻结到独立 model fact，不包含 API key；恢复时拒绝配置漂移，已完成 invocation 不再依赖当前模型配置。旧 invocation 无事实时按当前配置记录首次绑定，不把 Provider 返回的规范化模型名称误当作原请求配置。Plugin server 直接从 canonical `plugin.json` / `.mcp.json` 读取，HTTP MCP 要求 node `network=true`，stdio MCP 通过 Bubblewrap 在 `/plugins/<id>` 只读挂载内启动；MCP 握手返回的全部工具按 `<plugin>-<server>_<tool>` 暴露，Skill 以 `/plugins/...` 路径挂载。RMCP 2.2 没有 legacy SSE client，声明为 `type: "sse"` 的 Plugin 会明确拒绝，不能伪称为 Streamable HTTP。Rust 真实业务 MCP 和 provider/model 全覆盖仍待验收。

Rust 核心与 Plugin 外部语言环境分离：宿主读取操作员显式选择的 `ANCHOR_RUNNER_LIBRARY_ROOT/tools/<id>/tool.json`，复用现有 `entrypoint/environment/imports` 约定，只读挂载 `/tools/<id>/run`、解释器环境和依赖；Agent `anchor_run`、Op 和 stdio MCP 共用该部署工具集。Graph/Plugin 不能通过定义增加宿主路径授权；Agent 网络声明已传到命令沙箱。显式 MCP 解释器路径保留，避免 PATH 中其他环境替换它。工具环境不自动安装，也未纳入 Run 内容摘要；操作员必须在运行及恢复期间保持稳定。多个工具共用按工具 ID 排序的 PATH/imports，MCP 显式 PYTHONPATH 优先。Agent 与 Op 的节点 network 声明均交由同一 Sandbox 策略处理。

本地输入由操作员 `ANCHOR_RUNNER_LOCAL_INPUTS_ROOT/<immutable graph name>/local-inputs.json` 提供，内容沿用 `{node: {name: absolute_path}}`；Graph bundle 不携带宿主路径授权。Agent 命令、Op、stdio MCP 只获得本节点的只读 `/local-inputs/<name>`。新 Run 将授权（含空集合）冻结为独立 local-inputs fact，child 使用自己的 Graph metadata 和授权；恢复拒绝源配置/路径授权变化，旧 Run 缺少事实时只允许迁移空授权。API 在改写 Run 状态前预检当前及正在等待的非终态 child 配置，已结束/历史/detach child 不构成父 Run 继续的前置条件。删除 Run 同步清理模型与本地输入事实。

A90 已以 release binary、原样 `plugin-research.json` / academic-research Plugin、现有 Python scholarly 工具完成真实 DeepSeek Flash → Crossref 检索 → 原始 JSON 与 research.md Artifact 的接线验收（`.local/rust-plugin-reuse-gx8k_ms9/evidence.json`）。这不包含完整深度研究或外部业务 MCP 验收。Run/Graph 文件 lease 在持有者析构时显式解锁，避免沙箱子进程 fork 到 exec 的窗口中继承的文件描述符延长旧锁；持有期间的排他与进程崩溃释放语义保持不变。

Bubblewrap 启动通道显式清除目标 FD 的 CLOEXEC，包含源 FD 恰好等于目标值的情况。命令根进程用 `waitid(WNOWAIT)` 观察退出，在输出管道收尾/原进程组清理后才回收，防止使用已复用的 PGID；管道读取遵守原 timeout/cancel。命令已成功但后代保持管道时，保留 Completed 并标记输出 incomplete。离开原进程组的 writer 不跨组清理；异常外部回收导致 ECHILD 时保留少量 Child 句柄，避免 drop 向可能复用的 PID 发送信号。这是本次真实挂起触发的局部修复，不新增运行时进程监管体系。

A91 完成协议由 `anchor-io-harness-runtime/completion.rs` 在 Provider 边界无状态适配。原生调用与业务调用同轮返回时，保留全部业务调用及顺序，完成候选仅记为 deferred；后续必须查看结果并重新单独提交。普通文本、多个完成调用及截断完成不能冒充有效交付；非法摘要/路由进入 Harness 纠错。现有 `step_turns.text` 的可选 `_anchor_completion` 由 adapter 保存收到的调用和转换状态，不是新的完成事实或日志；旧已完成记录仍可读。完成后重开无需再次调用模型，混合回合在业务步骤提交后中断并重开不会重放业务或采用旧候选。Cancelled 沿用 Harness 终态规则，最后一轮取消仍受既有 step 边界限制。完成模式流式响应缓冲到完整调用后才发出内部值，尚未提供用户摘要的增量流。该 output-schema adapter 面向 Anchor 的 summary/route 完成契约，未承诺任意结构化输出类型。最终 release 的真实 DeepSeek Flash 原生完成验收见 `.local/rust-plugin-reuse-rk32np54/evidence.json`，Graph/Plugin 未修改。

`anchor-rsi` 是独立只读业务 Plugin，不依赖 Graph Kernel。它从操作员授权根动态冻结源码、部署 Graph、公开 Plugin、Run 机械元数据及历史报告，通过分页索引/按行读取供普通 JSON RSI Graph 使用。Run 投影记录字段存在性及省略清单，避免把脱敏省略误判为源记录缺失；模型 trace、checkpoint、对话与凭证不作为审查材料。公开联网目前覆盖直接依赖注册表和 GitHub release 元数据，包含超限/HTTP 失败记录，不等于全面社区调研或特性适用性验证。报告经独立模型评审及结构门禁后落文件；执行通过、内容验收、提案实施与长期收益分别记录，未切换生产定时任务。标准 18 节点原图已在 Rust Host 受控运行通过，但真实 provider 内容验收仍未完成。

Rust宿主执行入口分为 `api/{graphs,runs,files}` 协议层、`application` 接纳/控制层、`execution` 共享Runner接线。HTTP接纳先保存冻结Run及独立不可变来源元数据（Graph名、digest、bundle来源、创建时间），不再按相同digest猜Graph名。暂停在节点/并行wave边界结算，resume重载原snapshot/input并校验原Plugin资源，未知started副作用不重放。同图手动触发检查活动/暂停/遗留未完成Run，不同Graph可并行。每个state根由一个宿主持OS写入lease，HTTP持有整个服务周期、stdio执行持有执行周期；只读status不争写锁。旧无来源metadata的Run列表显示unknown，HTTP拒绝接管，未完成者阻止新的接纳/Graph修改。此约束不取代未来独立调用的同图多Run并发契约。

Rust host 的 Axum `serve` 入口已提供 Graph CRUD/trigger、Run list/detail/control、Artifact list/read 和 `/timeline` 的 Run 历史投影，并可托管现有 React bundle。React 浏览器 E2E 已从 Rust Host 提供的页面加载应用，在非 loopback + Bearer key 配置下运行 Rust-owned Graph，通过界面查看节点文件，并检查时间线。时间线只显示真实 Run；`schedules`/`scheduled` 为空且 `capabilities.scheduling=false`，计划管理按钮会禁用。Run detail 的 trace 从 io-harness 持久 turn/observation 投影；当前 fixture 测试不是实际模型 trace 的浏览器验收。Graph/Run/Artifact 这条前端垂直切片已通过 provider-free 浏览器验证，但 Session/Pilot、Scheduler、Plugin 管理、relations/channel 与全平台行为仍未全部迁入 Rust。可选 Python 平台适配见下文；Rust 宿主和独立 bundle 继续复用同一个 Runner。

Session 已接入服务：`/sessions` 提供创建、列表、读取、消息和事件读取、状态变更、Run 关联、停止、失败后续答与删除。Pilot 使用 PydanticAI 和 Harness 会话存储，WebUI 可创建及恢复对话，历史会话旁的“…”菜单支持改名与确认删除。`PUT /sessions/<id>` 只更新 title；空闲会话可直接删除，服务端在同一调度锁内拒绝正在回复的会话并执行删除，避免与新 turn 并发。删除保留关联 Graph/Run/产物及框架步骤文件，不代表磁盘记录的彻底擦除。Pilot 还通过显式 PydanticAI 工具查询 Graph、Plugin、Run 和产物，并可在 Scheduler 校验后创建、修改、删除、启动及控制 Graph Run。

Pilot 对话执行走 turn API。客户端为每次提交生成 `request_id`，服务端在一张 SQLite 表里原子接受提交并分配 `turn_id`，随后在后台线程执行；同一个 Session 下同 ID 同内容复用已存在的 turn，所以丢失响应后的重试不会触发第二次模型调用。模型输出经 PydanticAI 的 Vercel AI 事件编码器转成 chunks，按序号追加到同一数据库；`GET /sessions/<id>/turns/<turn>/events` 只是这些记录的只读 SSE 投影，`id` 即游标，`Last-Event-ID`/`?after=` 决定补发起点，重连或重新订阅都不会重新调用模型和工具。执行在服务端进行，关闭页面既不取消任务也不启动新请求，停止必须显式调用。Harness 保存权威模型消息，Anchor 保存提交意图、执行身份、终态与传输游标；恢复尝试使用新的 `turn_id`，不复用原 framework run ID。进程启动时遗留的 `running` turn 会被标记为 `interrupted` 并保留事件，不自动重放。

turn 数据库使用 SQLite WAL，让 SSE 读取已提交事件时不与逐条增量写入争抢数据库排他锁。Session JSON/JSONL、Harness 存储和 turn 数据库没有统一事务；实例内锁只对单进程有效。这是当前边界，不再作为跨存储事务改造的待办。

续聊按框架记录接通：Pilot 的 `StepPersistence` 用原生 `FileStepStore`（`state/pilot-steps/`，每个持久化 run 一份 `run.json`、`events.jsonl`、`tool_effects.jsonl`、`snapshots/*.json`、`media/*`），并打开 `capture_frontier=True`——进程在工具执行中被杀时不会走到「已结算」边界，没有 frontier 快照就没有任何可读现场。新一轮消息先看框架记录：记录比已保存对话更长（进程被杀或用户停止的回合走不到保存）时用它，否则用 conversation store。`continue_run(include_interrupted=True)` 读回的历史末尾可能是未完成的 tool call，而框架拒绝在未处理调用上叠加新 prompt，所以 `pilot._close_unfinished` 只把这种响应标成框架自己的 `state='interrupted'`，由框架合成 `outcome='interrupted'` 的 tool-return：模型看到「调用过、结果未知」，不会重放那次调用。`Scheduler.create_turn`、`pilot_message` 允许中断会话接新消息，旧 `unsafe_to_retry` 门禁已删除。

上述 Session/Pilot 续聊、压缩和对象跳转属于 Python 当前入口的 P2/P7 验收；Rust-native 宿主不读取这些内部文件或 turn ID，迁移到 R8 时按同一用户结果重新验收，并保持各自 Run/Session 事实单写。

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

#### Rust 平台生命周期与官方原生工具首片

`rust/anchor-platform-session` 以独立 Rust SQLite 保存 Session/Turn 元数据、生命周期、关联和 UI delivery events，不保存 Agent 消息。Host 的 `/sessions` 生命周期和 `/sessions/{id}/turns` 幂等提交、查询与 SSE 使用该端口，存储位于 `<state-root>/platform/sessions.sqlite`。普通 Pilot 保持操作员共享可见性；`responses-` 命名空间按可信 API key 摘要隔离，body 不接受 owner。已关联 Run 或被 Run metadata 引用的身份不能通过该 API 删除或重新认领。原生 exact v1/v2/v3 schema 事务升级至 v4，只增加 Goose 关联表；未知或未来 schema 在 DDL 前拒绝，旧 native 事实不变，不导入或修改 Python 会话。只在部署 writer 启动时将残留 running Turn 标 interrupted，不自动请求模型。

默认 legacy Pilot 由 `anchor-io-harness-runtime::pilot` 直接调用固定框架 Session 的 `turn_bounded_observed`，不执行隐藏 Graph；显式 Goose Pilot 见后续章节。Host `PilotService` 负责接纳、控制和 terminal delivery，与 HTTP adapter 分离；`PilotTools` 保留九个只读工具，新增 `graph_create`、`graph_update`、`graph_run`、`run_pause`、`run_resume`、`run_stop`。变更工具校验受信 owner 下的 running Turn；用户请求是普通创建/更新/启动/控制的授权，不增加审批引擎。HTTP 与 Pilot 变更工具共用 `RunApplication::graphs` 的 catalog mutation guard、Graph admission lease、资源冻结及 durable staging。读取文本仍使用 NOFOLLOW 资源端口；内建 FS/exec 均 masked，不能借 Pilot 获得宿主 shell。legacy 模型、step、token 和 wall-time 使用框架契约，默认 12 steps、24000 tokens 和 90 秒；这些默认限额不施加到 Goose Pilot。Native outcome 不完结时不伪造成功。Run 启动返回接纳事实，不代表完成；暂停/停止只请求安全边界收束，应通过 `run_status` 核查。

固定 io-harness 的 ToolMask 拒绝调用但刻意保留完整 tool catalogue，并不省掉禁用工具的 schema tokens。Pilot 另外在 Rig 传输端固定只发送该 Turn 显式 ToolPort 的工具目录，记录也保存这个实际传输目录；该收窄不授予权限，伪造内建调用仍被框架 mask 拒绝。系统描述通过公开 `SystemPrompt::Replace` 去掉不适用的仓库 FS 编辑提示，框架 boundary/ending、上下文、预算和循环不变。Graph AgentNode 的默认传输目录未改变。这使普通创建→更新→启动的短链无需提高原 24000-token 预算；实际成本及未通过尝试以开发台账为准。

RunApplication 在构造时固定宿主 Tokio executor。Pilot 的 io-harness 在临时 current-thread executor 中执行，但派发、恢复、父子 Run 的回调和取消监视使用宿主 executor，不能依附于一次 Pilot Turn 的临时执行器；Pilot 回复或临时执行器退出不取消已接纳 Graph。

Turn 的 `native` 保存 scope 摘要和原生 Session/Run ID：在框架 `Started` 事实产生后、首个 provider 请求前由 observer 写入，写失败取消执行，不向模型计费。`runs` 与 Session `run_ids` 在业务存储同一事务关联。Graph Run 的 immutable metadata 先持久化受信 `pilot {owner,session,turn}` 来源，再保存 Run 和派发；启动时只从保留的 Run metadata 补齐缺失业务关联，不重放 Graph 或模型请求，不追加终态 Turn delivery。若 Run 已接纳而业务关联写失败，工具保留 Run ID 并明确要求核查，不能把它当未执行后重发。Session 删除/重新认领也检查该来源，关联投影暂缺不释放身份。这不是跨 SQLite/RunStore 的原子事务；原生行创建到 observer 关联之间的故障窗仍保留 native facts，业务 Turn 启动后标中断，由新输入核查，不自动重跑。

io-harness 在 `<state-root>/platform/pilot/<owner+session+created_at 摘要>/` 持有原生 Store/Session；业务 Turn 仅保存输入身份及显示投影。新输入通过原生 `branch_from` 接回中断 tail，附带公开 Store 中的未知工具结果事实，不恢复或重放旧工具。纯聊天中断不能通过 Run `check_resumable` 继续，读取历史/新输入不把“可 resume Run”当作前置条件。读取消息使用公开 Store API；`Store::open` 可做框架原生迁移/chmod，并非 SQLite READ_ONLY。每个业务 Turn 有独立 provider recording/call-ID ledger，避免框架跨轮只带 prompt/reply 时复用旧位置化 call ID。SSE cursor 从持久化 UI projection 重放，GET/断线不驱动执行；terminal 之后不能追加 delivery。接纳和 terminal/control-map 清理共用短 gate，防止旧 Turn 清除下一 Turn 的取消句柄。

原生 `/messages` 只投影用户与助手文本，工具详情保留在 Turn SSE 和原生 Store。普通 Host Pilot 入口没有 Graph/channel Session admission、原生提问/删除确认、完整 Responses、旧历史导入或跨 Run/Turn/framework 存储原子协议，相关 Graph/channel Turn 入口明确 501。`graph_delete` 不暴露给 Pilot，不能绕过尚未接入的确认/拒绝。业务 Session 删除级联业务事实，但原生 Harness 目录的 retained/清理契约仍待 F2b2/F2c；重新创建身份使用新的 created_at scope，不继承已删除 Session 历史。当前生产及 Python 兼容平台依然保留原完整 Pilot。

Runtime crate 另提供显式 opt-in 的 `run_interactive_pilot`、`pilot_pending_question` 和 `resume_pilot_with_answer` 组件接口，未改变普通 Pilot 的工具目录。传输端仅在该路径将原生 `ask_question` 映射为 `session_ask`，recording 保存实际 wire 名称；问题、答案 CAS、等待和续答由固定 io-harness 持有。续答核对 scope/Session/head/turn/run/question 与原生 lease，继续原 Run 并更新原生 turn，不新建 Session turn；重复回答不再次驱动模型。文本投影可展示问题和已保存答案。答案写入到 observation、原生完结到 turn 投影以及外部业务关联仍不是原子事务；未知窗口保留原生事实，不能盲目重放。该组件只有确定性 HTTP Provider 验证，尚未接 Host 的 business Turn/owner/HTTP，也没有真实 Provider 或完整产品提问闭环证据。

Graph 应用层增加目标快照与条件删除。`GET /graphs/{graph}/delete-precondition` 返回禁缓存的 precondition JSON 和强 ETag；`DELETE /graphs/{graph}` 可用一个带引号的 `If-Match` 提交该条件。目标名称/路径、定义、冻结资源和文件身份变化或同名重建会拒绝旧条件；特殊文件和 symlink 在这个条件路径中 fail closed。快照校验与原删除清理持有同一 Graph lease，并继续核对当前调用方、活跃/未完成 Run 和待投递 Session。未带条件的 HTTP 删除沿用原有行为；条件不等于用户确认或新增宿主授权。该路径不保护操作者绕过宿主锁直接修改活跃资源的操作，也不是 Pilot 删除审批引擎。

`rust/anchor-wecom-tools` 与 `rust/anchor-docmost-tools` 使用固定 rmcp stdio ServerHandler。前者提供三个应用 API 工具、token 缓存/并发刷新；后者提供 `upload_page_image`，stdio 只读取 `/in/publish/assets` 的常规文件，并拒绝越界、符号链接、特殊文件和超限上传。两者不加载 dotenv、不自动重发修改请求，并通过 `package-plugin <new-directory>` 输出独立的 manifest/原 Skill/native binary。Docmost 保留远程 MCP；WeCom 首片不包含仍为 Python 的长连接网关。原 `plugins/` manifest 和活跃 Library 资源没有被替换。Host/Graph/io-harness/Bubblewrap/真实 stdio 与 loopback 业务服务的组合测试不等于公网投递或生产发布验收；真实模型证据及剩余边界见台账。

`rust/anchor-scholarly` 是独立官方工具 crate，不依赖 Runtime Kernel 或 Python。首片提供 Crossref/arXiv/OpenAlex 的 `sources`、`search`、`search-many` 和原有 JSON/逐项失败语义；生产请求只访问公共 HTTPS，固定已验证 IP、禁代理/隐式跳转/重试，跳转重新校验，并限制响应大小、总超时及来源重试/节流。fixture 通过显式 transport 注入，不提供生产私网开关。`read`、`read-many`、`citations` 的名称和参数保留但明确非零失败，全文/PDF 提取和引用追踪仍待迁移；原 Python 工具、`/tools/scholarly/run`、活跃 Library/Plugin 未替换。新增查询文件限制与更保守地址策略的差异见 crate README；没有真实学术 API、TLS、部署或标准工具闭包验收证据。

`rust/anchor-devtools` 提供 `regression fixture` 开发入口，直接构建既有官方 MCP 并运行 Host 的 `runtime_contract`/`native_plugins` 测试，不需要 Python/venv。场景仍保存原生历史、Run、workspace 和 Artifact，入口仅汇总当次命令/退出码/完整测试数，核对实际 Host binary 摘要与确定性 Provider 证据；不复制 Runner 或恢复语义。live、官方工具部署和生产验收不因该入口存在而完成。

#### Python 平台对 Rust 的接线

`runtime_http.py` 是公共 HTTP 适配器；配置 `ANCHOR_RUNTIME_BACKEND=rust`、`ANCHOR_RUNTIME_URL`，可选 API key 与超时。Graph、Run、Artifact 只由 Rust 写入；Plugin 安装/授权仍由 Python Library 管理。显式 Rust backend 下 Python 将 schedules CRUD、timeline 查询转发给 Rust Host，Rust 使用 `ANCHOR_RUNNER_SCHEDULES_PATH` 指向原 `state/schedules.json` 并负责 ticker；Python 不加载或推进该文件，避免双写。默认 Python backend 下既有 Python Scheduler 仍负责计划管理和 tick。定时触发经 Rust admission，忙碌或不可接纳时不创建 Run；Host 不可用时返回 503，不回退执行 Python。A104 双服务浏览器在 `TZ=UTC` 下验证 Host 停机期间过期 once 计划重启后不补跑（`enabled=false`、Run 数不增、timeline=`missed_downtime`）；DST/non-UTC 未验证。OAuth route 测试只证明 POST 留在 Python handler、没有 Rust proxy，不表示 OAuth 执行授权已迁移。旧 Python Run 按实际存储存在性只读呈现，同名 ID 冲突返回 409；普通 Pilot 已通过公共 Graph/Run/Artifact 端口接入 Rust；Graph/通道会话及附件经 `POST /conversation-runs` 接入；未绑定 Graph 的渠道会话、Webhook/relations 返回 501，不产生执行副作用。A104 证据 `.local/platform-rust-WRanc9/evidence.json` 不含 live model，且显式 Rust backend 仍为 opt-in，生产 backend 默认 Python。

Pilot 的 Session/Turn、原生提问/删除确认、SSE 和 Harness 对话记录继续由既有 Python 宿主管理；Graph Run 和产物由 Rust 唯一写入。工具不读取本地 graph.json、scheduler.running 或 Run 目录，Session 重开后通过公共 Run 查询找回关联事实。`POST /graph-validation` 只编译作者定义、展开子图并解析已安装 Plugin，使用与 create/save 共用的解析路径，加上现有 Host 静态能力检查；不保存 Graph、不创建 Run、不启动模型/MCP/命令。校验通过不承诺 provider、运行期授权或调用目标已就绪。

文字渠道和附件沿用原 Graph 和 Plugin：Python 校验来源、保存 Session/Turn 和 EventLedger、监管网关及投递；Rust 保存会话 Run 接纳、控制、冻结输入和产物。Run ID 来自 Turn UUID，相同请求重试复用已接纳 Run；同 Session 前驱必须收束后才能接下一轮，不同用户可同图并发，普通 trigger 仍互斥。新消息替代旧回复，连续排队中已经过时的消息不再创建 Run。Graph 的 `node_plugins` 是展开节点挂载的公共投影，通道监管不再读取 Python 本地 Graph 副本。渠道文件在 Python 受信入口按原描述读取一次，提取文本和上传使用相同字节；Rust 按 Run 保存 hash/MIME/字节，挂载为 `/in/channel` 只读输入，图片按显式视觉模型能力送入每次模型请求。重试比较来源描述和冻结 manifest，原下载文件删除后仍只读取 Rust 快照。

每个规范 bundle 路径、Session、完整节点 ID 对应一个 io-harness 0.86 原生 Session/Store；框架历史根稳定，工具工作区仍属于当前 Run。每个 invocation 保存精确原生 run/turn 定位，发布中断后按该定位补齐，不猜测最新记录。`anchor_conversation_history` 只读查询当前节点的可信前驱链及真实工具记录；未记录结果明确为 unknown。`/previous` 从最近可用前驱的提交或未完成工作区冻结成只读输入，拒绝符号链接，不将中断文件标为完成产物。停止时未决调用保留，新轮不自动重放；有后继的旧轮及连续 wait 子调用不能 resume/recover，detach 边界保持独立任务语义。

会话 Run 不允许单独删除；删除整个 Graph 在执行收束检查后同时清理对应原生 Session、定位、模型记录和 Run 数据，保留其他 Graph 的会话。仍有 Rust 会话 Run 时，Python Session 删除返回冲突，并在查询后复核本地 Session/Turn 修订，避免竞态丢失历史。已封存且收束的 Plugin 会话祖先不再永久阻止修改 Graph；当前未结束的执行仍受保护。可选 MCP 缺少配置时不发布该工具，声明之外的工具引用继续报错；新目录 Graph 的连接参数只从匹配冻结 Plugin 资源的 catalog 解析。

SSE 保留原 Turn 事件与最终回复，不等于模型 summary 增量。当前 Rust 渠道附件/原生图片已通过隔离真实视觉模型验收。挂载 `wecom` 的 Rust AgentNode 接入既有 `wecom_send_message`；只有受信会话的回复节点可用 `wecom_attach_image`。发送经现有网关私有 Unix socket，沿用 EventLedger 的 ACK/去重/未知结果不自动重放；请求身份来自 InvocationKey 与 io-harness 原生工具 attempt。网关凭证只留在宿主，Supervisor 将轮换的控制端点原子发布到 `<platform-root>/state/channels/wecom/control.json`（0600），Rust 通过显式 `ANCHOR_CHANNEL_CONTROL_DESCRIPTOR` 读取，不跨部署搜索。

回复图片只读取该节点 `/workspace`、`/in`、`/previous` 的普通无符号链接 PNG/JPEG，复用已有图片校验；停止后不准备图片。图片按 Run 保存，普通 Run 详情只投影 `channel_reply`，完成且不活动后经 `/runs/{id}/channel-reply` 单独读取，Python 将其接回内部 `msg_item`，由网关转换为上传和图片回复协议（不直接传给流式消息）；图片加载失败返回结构化错误，不静默降级成正文。整图删除清理正式图片与崩溃临时文件。真实 provider + 本地 ControlServer/EventLedger ACK 夹具已验证连续相同正文的两次有意发送、图片字节、服务重启和事件去重（A98）；主动发送随后通过真实网关获得平台 ACK 并由用户确认收到；公网入站驱动 Rust、上传图片并回复也已通过真实验收，用户确认文字和图片均收到（A98，`.local/rust-wecom-inbound-k0y31ypg/evidence.json`）；先前两次 `stream.msg_item` 路径仅有文字 ACK 而图片不可见，不能作为成功证据。`call.session` wait/detach 已接入；ACK 事实写入既有 Session 事件账本后可在重启、归档或权限变化后只重试结算。ACK 返回与本地事件落盘之间的进程崩溃窗口尚无平台回执查询能力，不能证明该极窄窗口不会造成未知投递；详细状态见开发台账 A99。在线整个平台暂保留 Python backend。Python legacy 通道的已有能力不能作为 Rust 已完成证据。

删除确认保存公共 Graph 定义的规范 JSON 摘要；定义变化、旧字节摘要不匹配或后端查询失败均阻止删除，只有实际 404 视为不存在。Responses 会话继续绑定原 key 的摘要；公共 Session 列表及所有 Session HTTP 读写/SSE 入口也核对同一归属。`responses-` 是内部会话前缀，首次 Responses 引用尚未持久化或只剩孤儿 Session 时不对外开放。普通 Pilot 的共享管理权限保持现有规则，这不构成完整多租户隔离。

Rust trigger 接收 `objective` 和 `trigger` 来源，objective 仅冻结到本次 Run，不改 catalog。计划来源保存 schedule ID 和原 scheduled_at 字符串。列表、详情、时间线从同一持久事实投影，updated 使用 Run 文件 mtime；复制或迁移文件会改变此时间，历史暂停/停止造成的 busy 区间仍是时间推断。Graph create 的初始 definition 与 Plugin 资源先完整暂存再发布。设置 `ANCHOR_RUNNER_LIBRARY_ROOT` 时，创建/保存 Graph 从同一操作员 Library 解析 Plugin 并冻结资源；未设置时沿用 catalog。

Run detail 的 `control_requested` 来自当前进程活动控制 token；UI 展示“正在停止/暂停”，待节点收束后显示持久状态。最终模型回答已提交时，stop 保留这一次完成并阻止下游；继续同一 Run 不重新调用已完成节点。重启后的非活动 Run 不伪造待处理控制请求。

#### 模型交互记录

生产 NodePort 按 invocation 在 `state/io-harness/store/np1-<key digest>.recordings/<attempt>/` 持久化请求和结果。`request.json` 保存 io-harness 请求，`rig-request.json` 保存适配后的 typed 请求及 final_result 定义；`rig-response.json` 去除 raw transport 文档，`recording.json` 使用框架原生 Record，保留完成转换前的实际调用参数。outcome 仅保存状态和错误分类。请求在发送前落盘，续跑追加 attempt；记录不参与恢复决策。删除所属 Run/Graph 时一并清理。

新目录 0700、文件 0600；部署认证 header/API key 不注入这些对象，但 prompt、工具输出和模型正文按原样保存，可能含业务秘密。此处不是 HTTP 原始字节或流片段全量抓包：响应未收到、进程中断或响应后写盘失败时可能只有请求，recording_incomplete 或缺少终态不能当作成功。响应后录制失败不会触发模型重试。回放原生 Record 的完成调用仍需同一个 completion 投影。

#### Goose ACP 候选验证（显式 opt-in，非生产默认）

本节的短时限、模型代理和 fail-closed 对应历史 `goose-acp-spike` 模式；新增 `goose` 接入的实际边界见下一节，两者不共享恢复状态。

Goose-only G0 将共享 Kernel 的 Graph/Sandbox 与旧 Rig 执行器隔离，A116 进一步将 Kernel 默认 feature 改为空。稳定 ToolPort、ToolError 和 text/json 工具内容不依赖 Rig；旧 executor 与类型只在显式 `rig-legacy` 下构建，旧 ioRuntime 显式启用该 feature。标准 Host 的 normal/build/dev 依赖闭包不含 io-harness、rig-agent、rig-core 或 rig-rmcp；保留的旧回归仅由 Host `legacy-regression` 启用，不是产品 fallback。迁移顺序见 [G0–G3](goose-runtime-migration.md#最短实施顺序)，实际验收见 A113/A116。

2026-10-06 的 `codex/goose-acp-spike` 在现有 Host 的 NodeExecutionPort 边界增加候选适配器，复用同一 GraphRunner、Host ToolPort、Bubblewrap、Run 与 Artifact；一次节点只运行 Goose loop，不将完整 Agent 嵌入 io-harness 的模型 transport。默认仍为 io-harness，使用独立 state root 和固定 Goose v1.53.0 binary hash；Graph model 必须与显式 fixture model 匹配，Plugin、媒体、跨轮会话与精确累计预算目前拒绝，不静默降级。实现、重跑命令与局限见 [Goose ACP 验证分支](goose-acp-spike.md)。

候选路径以 HTTP MCP 暴露实际授权工具和结构化 final_result，拒绝 fs/terminal 请求、额外工具和不合法完成，关闭时等待工具 handler 收束并清理进程组。Goose 原生 SQLite 保存会话；Anchor 外层保存 invocation、session、binary identity 与完成事实。未完成 invocation 重启拒绝自动 load/replay，保留现场，不声称已提供未知副作用安全继续。模型请求经本地 proxy 计数并有界缓冲，不代表实时 summary/SSE 或精确累计预算。

Runtime 控制平面需要独立显式 `ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1`；`--share-net` 不是仅允许 loopback 的 OS 网络隔离。业务工具仍服从 Graph 网络意图及宿主授权；此片只在本地 fake Provider/凭据中验证，不对任意 Goose 进程承诺 outbound 隔离。受控 Graph、原生 history、workspace、Artifact、取消、拒绝越权与杀进程后 fail-closed 已有 fixture 证据，真实外部 Provider 和完整产品对齐未验收，不计入生产替代完成。

#### Goose AgentNode 标准构建（生产部署未切换）

标准 Host 默认选择 Goose，AgentNode 通过固定 Goose v1.53.0 的公开 ACP 直接执行模型，仍使用同一 Rust GraphRunner、OpNode、Host ToolPort、Plugin/MCP、Bubblewrap 与 Artifact；Pilot 接同一 ACP 层。`ANCHOR_RUNNER_AGENT_RUNTIME=goose` 可显式指定，但不再必需；标准构建拒绝 io-harness 模式。Agent 需要固定 binary/hash、显式共享控制网络授权和模型配置；无 Agent 配置仍可执行 Op。旧 invocation 在配置、恢复事实和执行入口均不能冒充 Goose，旧 state root 不自动迁移；未配置 Agent 时允许 Op 使用保留旧事实的根，不接管旧 Agent。生产部署、凭据和数据未切换。实现入口为 `rust/anchor-runner-host/src/goose_acp.rs`。

模型接入读取 `ANCHOR_MODEL_URL/API_KEY/NAME/WIRE_API/ALIASES`，支持显式 chat/responses 路径，Goose 自己调用配置的 Provider，不增加 Anchor 模型代理。未知别名拒绝；model binding 摘要包含 endpoint/wire/实际 model，不含密钥，恢复时不可静默改变。HTTPS 和明确的 HTTP loopback 配置可接入。原生路径没有 Anchor 默认 30 秒任务时限，配置的 wall time、用户取消、协议帧和内存上界仍有效。累计预算未启用，显式请求预算、媒体和跨轮 conversation hint 目前拒绝，而不是伪装已支持。

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

标准依赖树出口已由 A116 关闭，旧实现仅显式 `legacy-regression`/`rig-legacy` 保留；生产 backend/数据未切换。G2c 不代表媒体、渠道、压缩组合、Goose-only 发行版或生产替代全部完成。

#### Goose MCP 图片结果（确定性传输切片）

标准 Kernel 工具结果增加 inline `Image {data,mime_type}`，不引入 Provider 或图像解码依赖。MCP adapter 负责用成熟 base64/image crates 验证并映射原生 Image 和 image blob resource；后者只使用已有 blob，URI 从不解引用、也不授予宿主路径/网络权限。Goose bridge 再校验直接 Rust 工具的媒体边界，用原生 MCP Image block 回传，不能把 base64 包进普通 JSON 文本后称为图片支持。

仅支持静态 PNG/JPEG/WebP，要求 canonical base64、声明 MIME 与真实字节相符、完整解码；单张 10MiB、每次结果总计 20MiB/8 张、20 million pixels 和 128MiB 解码内存上界。动画、损坏、GIF/HEIF/HEIC/SVG/未知 image MIME 明确失败，整个 typed 结果不返回部分成功。原始 raw MCP 与显式 legacy mapping 契约不改；渠道附件复用同一 codec 校验，原有数量/上传/冻结权限仍归原层。

含图片结果在小型 metadata/receipt header 后保持 Text/Json/Image 原序；非媒体结果保持原 JSON envelope。Anchor invocation fact 和 `tool_calls` 只保存 MIME/解码字节数/SHA256，避免将大 base64 注入恢复提示；Goose 原生历史及原有 ACP 显示记录保留原图片，不新增 Agent 日志或恢复引擎。带图片的公开 Run trace 另保留 native `contents`。Web 按原序呈现 Text/Image，以 `tool_call_id` 配对交错结果，thinking 单独折叠；只嵌入有界 inline PNG/JPEG/WebP，拒绝 URL/resource/SVG，浏览器解码失败显示不可用，原图查看复用既有 Modal。A122 的 UI 投影浏览器 fixture 检查真实解码/像素与桌面/移动端，不等于实际 Goose 图片到 UI 的完整组合或真实 vision 验收。ACP 单帧 32MiB、通知尾部 64MiB，保留截断标记、取消和 fail-closed；trace 文件读取上界 160MiB。

固定 Goose OpenAI formatter 对 vision-capable model 把工具图片转换为后续独立 user `image_url` 消息，工具文本显示原生 placeholder；非视觉模型会明确省略图片。Anchor 不擅自替换模型，不把可用 MCP 图片接口等同于任意模型都能看图。小型 fixture 用视觉模型名称、真实 Goose/Host/Graph/Plugin/Sandbox 和确定性本地 Provider 检查 bytes/history/receipt/workspace/Artifact 与同 session 恢复；真实视觉 Provider、媒体输入/渠道及完整图片 UI 组合仍待验收。

## 保持的设计原则

复用现有运行链与沙箱边界；图仍是 JSON，节点仍拥有独立工作区，成果仍通过 commit 关联。研究是否结束应由目标、证据和反馈决定，不能用固定 `max_xxx` 充当质量或收敛判断。用户主动停止和显式资源预算是另外的控制需求。

这些原则约束 Plugin 接入，不要求恢复历史上已删除的服务、数据库或图版本发布体系。

## 本机工作周报

`weekly-work-report` 的采集、理解、写作和评审仍是普通业务 Graph；通过评审后的 Docmost 同步节点挂载 `docmost` Plugin。Graph 工作区中的 `local-inputs.json` 由本机操作员按节点 ID 授予具名只读路径，挂载到 `/local-inputs/<name>`；Graph JSON/API 本身无权增加该授权。采集 Op 读取两个 sessions 目录和普通采集脚本，将当次窗口的证据交给后续节点。授权路径随 Run 保存，恢复时授权改变则拒绝继续。图按程序采集→项目理解与选题→写作→读者视角独立评审→门禁分流运行；表达问题退回写作，项目理解与判断问题退回理解。证据缺口通过限定结论处理，不设 blocked 分支。正文围绕项目实质变化，来源单独保留，评审先检查可理解性再核查关键事实。四项评审通过、阻断问题解决且评审对应当前稿件 commit 后才组装 Markdown、来源附录和 SVG；Docmost 发布节点通过附件接口上传 SVG 并插入页面，失败时不更新正文；使用方式见 [每周工作报告](weekly-work-report.md)，真实验收以台账 A21 为准。图片下载保留 attachment，并提供图片 MIME 与隔离 CSP，使 Markdown 图片预览可用且不开放脚本或外部资源执行。
`weekly-work-report` 的采集、理解、写作和评审仍是普通业务 Graph；通过评审后的 Docmost 同步节点挂载 `docmost` Plugin。Graph 工作区中的 `local-inputs.json` 由本机操作员按节点 ID 授予具名只读路径，挂载到 `/local-inputs/<name>`；Graph JSON/API 本身无权增加该授权。采集 Op 读取两个 sessions 目录和普通采集脚本，将当次窗口的证据交给后续节点。授权路径随 Run 保存，恢复时授权改变则拒绝继续。图按程序采集→项目理解与选题→写作→读者视角独立评审→门禁分流运行；表达问题退回写作，项目理解与判断问题退回理解。证据缺口通过限定结论处理，不设 blocked 分支。正文围绕项目实质变化，来源单独保留，评审先检查可理解性再核查关键事实。四项评审通过、阻断问题解决且评审对应当前稿件 commit 后才组装 Markdown、来源附录和 SVG；Docmost 发布节点通过附件接口上传 SVG 并插入页面，失败时不更新正文；使用方式见 [每周工作报告](weekly-work-report.md)，真实验收以台账 A21 为准。图片下载保留 attachment，并提供图片 MIME 与隔离 CSP，使 Markdown 图片预览可用且不开放脚本或外部资源执行。Rust 已用 reject-only Docmost 替身完成真实 provider 的七节点无发布验收；该证据不等于生产页面发布。


## 本机 RSI Graph

仓库提供 [rsi Graph](../examples/graphs/rsi.json) 和 [安装脚本](../scripts/setup_rsi.py)。它复用同一计划存储和 Graph 反馈机制：`collect → audit-context → audit-fanout → 五个专项分支 → audit-join → analyze → review-fanout → 两个独立评审 → review-join → review → gate → publish`。五个领域为 Run、架构代码、Graph、Plugin、依赖/社区；最后一个分支先执行联网 research Op，再执行 dependency-audit。整个流程属于同一个 Run。gate 区分修改综合稿（回 analyze）与重新专项审查（回 audit-context 保存反馈，再 fanout），不设固定模型请求或 Graph 轮数上限。 review 是校验评审 commit 并合并意见的普通 Op，事实/研究与方案/验收/回滚分工，任一未通过不能被另一方覆盖。

collect 动态发现可授权源码与未忽略新文件、全部部署 Graph、已安装 Plugin/工具/Skill/MCP/通道资源和全历史 Run 索引。本周 Run 及按需历史投影包含错误、恢复和 commit/输入关系；不会采集完整聊天/trace/推理。领域索引引导重点读取，源码及 Plugin 证据保留哈希与脱敏标记。Python 字面量和注释脱敏保留可解析语法，目录排除、二进制和授权缺失都有覆盖记录。历史提案从成功 publish 的记录 commit 读取，模型先读简洁 previous-index，再按 ID 回查完整历史。

research 从本次冻结 manifest、可选/开发依赖和包注册表项目链接发现目标，公开 API 主机限定 GitHub/PyPI/npm；记录版本、发布说明与 issue 信号以及限流/失败，未覆盖 Discussions/私有/非 GitHub 社区，不等同于穷尽互联网。Python 清单来自采集解释器，不冒充服务环境。专项输出 findings 与实际读取/未覆盖范围；综合以分支结论为起点，按需追溯证据。gate 检查当前分析评审 commit、join 分支 commit、证据可读取、提案 ID 唯一与历史连续性；这些机械检查不证明报告语义正确。结果写入 Run 的 publish/，包含 audit-manifest.json、报告、提案、来源、评审和门禁。

该 Graph 不写源码、不修改 Graph/Plugin、不重放历史副作用，也不把计划或模型回答当成完成事实。源码和数据根通过 Graph 工作区的 `local-inputs.json` 由操作员只读授权；网络响应有主机白名单、超时和大小限制，失败会进入证据文件。当前实现和真实 provider/长期递归效果的验证状态见 [RSI Graph](rsi.md) 和开发台账。

## 独立 Graph 调用

同 Run 的局部并行与独立调用分别表达，具体见下节及 [组合设计](graph-composition-design.md)。

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

WebUI 的“添加节点 → 并行分支”生成配对控制节点及两个 Agent 分支，画布展示配对关系和多个活动节点。示例见 [parallel-audit.json](../examples/graphs/parallel-audit.json)。已取得真实 DeepSeek 并行 Agent、join、综合文件的运行证据，以及真实后端浏览器和进程退出恢复验证；具体测试与部署边界以台账 A31 为准。

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

Host 安装与 Graph 冻结共用 catalog mutation gate；blocking 安装任务持有 guard，因此 HTTP 取消不提前解锁。独立安装 CLI 还持有 `plugins/.install.lock` 的跨进程独占 lease，Graph 冻结取得同文件的非阻塞共享 lease；安装中返回 conflict，不生成混合资源快照。冻结后的 Graph 仍读取自己的资源和摘要，Library 替换不改变已有 Graph/Run。普通 catalog 展示读取不持有该 lease，非协作的宿主目录修改不受保护。仓库/文件/深度总额度尚未设置，不将 Git 超时当资源配额；当前安装仅适合操作员审查的来源。A128 的 Rust Host 提供 owner-scoped OAuth 状态/撤销路由，并让旧 authorize 路径明确返回未接通；不接受 token body，未实现浏览器 state/PKCE、真实 provider exchange/refresh 或 MCP transport 注入。Python 兼容入口的安装/OAuth 所有权未改，OAuth 外部授权与真实 GitHub 安装未由本片验收。

`anchor-scholarly` 保留独立官方 CLI 归属，增加 HTML/PDF/text 读取、八项有序批量读取及 OpenAlex 引用链，不调用 Python。实际成熟 parser 提取、40 页/24,000 Unicode 字符窗口、损坏 PDF、逐项失败、公共 HTTPS/DNS pinning/redirect 边界已用本地 transport 验证；格式、重复 URL 顺序及共享 I/O deadline 与 legacy 的差异见 crate README。PDF 内部解压和 parser CPU/memory 不具有硬配额，I/O 超时不终止解析任务。活跃 Plugin、`/tools/scholarly/run`、真实公共 API/TLS 和模型内容验收尚未替换或关闭。

#### Rust 文本渠道 transport 与发行 builder

`anchor-wecom-gateway` 是独立入口适配器，只持有长连接及投递事实，不创建 Session/Turn、Run 或 Agent loop。固定 WebSocket subscription、heartbeat、匹配 ACK、有界退避重连与私有 Unix NDJSON control 共用既有 Host sender 契约；入站文本及纯文本 mixed 通过操作员配置的 bearer webhook 交接，文字结果沿原 callback 回复。媒体 callback、mixed 非文本项和 webhook 媒体显式拒绝，不静默丢失附件。平台 Session supervisor、relations、替换取消及附件提取/上传仍未接线。

新 native 私有状态目录为 0700，descriptor/socket/database 为 0600；稳定文件 lease 保证单 owner，NOFOLLOW 拒绝 symlink/special/hardlink。SQLite 在实际发送前绑定 request ID、内容/身份摘要和 wire ID 为 unknown；只有匹配成功 ACK 才 confirmed。未知、超时、拒绝、断连、SIGKILL 与晚到 ACK 不触发自动重发，confirmed 相同请求幂等返回。未知不等于失败且无 exactly-once 承诺。持久事实绑定 bot/endpoint，control token 轮换保留事实；旧 Python events.sqlite 拒绝接管并保留。回调去重后中断不重新执行 webhook；未发送 ready 回复在重启后核查当前入站授权与较新消息身份。

`anchor-distribution` 是操作员 Rust builder，调用同一 FileGraphBundleLoader，不执行输入 binary 或新增 Runner。包只带给定 ELF Host、固定 Goose v1.53.0、精确声明的 Graph/Plugin 资源和显式工具/Web；资源原子私有快照与发布前身份/内容复查，归档顺序、uid/gid、mtime、mode 和 gzip header 确定。逐文件 SHA256、平台、Plugin pins 和环境变量引用记录在 runtime-manifest；它不是签名，也不能从任意 ELF 证明 Host 无 legacy feature。标准 Host 的 normal/build/dev 依赖闭包另行验证。

源码/凭据/状态路径、symlink/hardlink/special、未知 bundle 文件或空目录均拒绝；JSON/TOML/YAML 的已知凭据字段结构化检查，保留未展开的环境引用。该过滤不是任意文本/二进制秘密发现，输入需要操作员审查；ELF 共享库、系统 Git/Bubblewrap 与外部服务不自动打包，工具放入 bin 不自动扩大授权。输出用 NOREPLACE 原子发布，不覆盖并发赢家；发布后 durability 不确定明确返回 uncertain。实际解包 Host/Goose 的小图停止、重启、同 Session 核查续行和只读 Artifact 已验收；这不是无 Python OS、release candidate、真实模型或生产切换验收。
