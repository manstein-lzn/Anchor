# Rust-native Runtime 重构计划

本文是 `codex/rust-rig-runtime` 实验分支的开发计划，描述如何把 Anchor 逐步收敛到一个由平台宿主和独立 Graph 宿主共同使用的 Rust Runtime Kernel。它不是当前生产 Python 实现的完成声明，也不替代 [开发台账](pilot-development-plan.md)；每个阶段只有在取得对应证据后，才更新台账中的验收状态。

## 目标和不做的事

最终目标是让下面两种交付形态调用同一个 Runner 和 Runtime Kernel：

```text
完整 Anchor 平台 = API/WebUI/Session/Scheduler/Channel + Shared Rust Runner/Kernel
独立执行包     = Graph + 明确 Plugin 资源 + Shared Rust Runner/Kernel
```

迁移过程中以 Rust-native Graph、Run、Plugin、Sandbox 和恢复事实为目标真相。Python 宿主可以暂时作为 legacy 适配层，但不再要求 Rust 复制 Python Run/Harness 的全部格式；同一 Run 不允许由 Python 与 Rust 双重写入。

本计划不包含：一次性重写 WebUI；把平台 API 塞进 Kernel；绕过 Sandbox 执行宿主命令；自造无必要的记忆系统；为了迁移而改变 fanout/join 的并发语义；把 Harness 全部功能机械复制到 Rust。可以重新设计 Rust-native Session、持久化、HTTP/CLI 和 bundle，只要它们不建立第二个 Runner 或模糊事实所有权。

## Rust-native 架构决策（2026-10-02）

用户明确不要求完全兼容旧 Python 平台或 PydanticAI/Harness。io-harness 负责 Rust AgentNode 的 loop、上下文、SQLite checkpoint、compaction 和工具效果恢复；Rig 只作为 Provider transport adapter，不运行第二个 Agent loop。Anchor 负责 Graph 调度、Run facts、Sandbox、Plugin 授权、Artifact 和外层恢复。Rust 不自行复制通用 Harness 能力。

Rust 平台宿主和独立 Graph 包共享同一 `anchor-runtime` Runner。Python 代码只保留为 legacy 入口、历史读取或迁移工具；它不参与 Rust-owned Run 的可写状态。HTTP/API、CLI、Session、Scheduler 和 bundle 可以使用成熟 Rust crates，按 Rust-native 契约逐个实现。迁移出口从“兼容旧 Python”改为“保持 Anchor 产品形态，并让 Rust standalone 与 Rust platform host 产生一致事实”；Python 兼容性成为可选适配能力。

产品形态出口只要求 Graph、AgentNode、OpNode、Plugin、Run、Session、执行观察/控制/恢复、产物和独立 Graph 包这些用户能力继续成立。Python 的 `run.json`、Harness 事件格式、旧 RPC、旧 API 字段和内部调用顺序不属于 Rust-native 兼容约束；Rust 可以重新设计存储、事件、Session、API 和 bundle manifest，但必须为用户可见的语义变化提供明确验收。

## 不变量

1. **唯一执行语义**：Node Runtime、Graph Runner 和恢复规则只有一个权威实现；平台与独立宿主只提供适配器。
2. **端口隔离**：模型、工具、Sandbox、事件、持久化和时钟通过小端口进入 Kernel；密钥、live client、宿主权限和平台连接不能进入 checkpoint。
3. **边界持久化**：模型请求前、模型响应后、工具批次结算后和终态都可以保存；恢复按已保存事实继续，不凭猜测重放未知副作用。
4. **局部并行**：Graph Run 仍然是单一协调单元；只有配对的 `fanout → branches → join` 区域允许并行，其他节点按既有串行规则推进。
5. **分层验收**：接口存在、provider-free 测试、Anchor 接入、真实 provider、真实 Sandbox、平台和独立分发分别记录，不能互相代替。
6. **边界可回退**：每个 Rust-native 垂直切片都能独立运行和验证；旧 Python 路径保留为 legacy，不把两套执行语义合并到同一 Run，也不修改历史事实格式伪造迁移完成。

## 阶段和出口条件

| 阶段 | 交付 | 出口条件 |
| --- | --- | --- |
| R0 事实与契约 | 本计划、架构边界、运行身份和错误/恢复术语 | 契约能映射现有 Node/Run/Sandbox；未决权限和持久化变化单独记录 |
| R1 AgentNode Kernel | `NodeRequest`、`NodeOutcome`、`NodeExecutionPort`、Anchor `ToolPort`、io-harness `TaskContract`/SQLite checkpoint、结构化 route；Rig Provider adapter | provider-free 工具循环、取消、非法 route、模型/工具边界恢复通过；真实单节点 smoke 通过 |
| R2 持久化边界 | `CheckpointStore` port、原子文件适配器、版本与身份校验、边界保存约定 | 进行中：端口、原子文件保存、pending model/tool 失败后续行、缺失/删除和路径拒绝已通过；未知外部副作用、并发写入语义和宿主集成仍待做 |
| R3 Provider 与流式 | provider 配置适配、stream 事件、超时、取消传播、请求/响应观测 | 进行中：OpenAI-compatible chat/responses、模型/工具超时、pending 状态保留、provider-free 流事件/取消、真实 provider 流式 smoke、可组合请求观测，以及中断后 checkpoint 重载并换 provider 续行的确定性测试已通过；真实进程/provider 断线中断仍待做 |
| R4 Sandbox Adapter | Rust Sandbox port；Bubblewrap、只读输入、网络权限、命令超时和取消适配 | 部分完成：独立 Bubblewrap host adapter 已实现；本机真实 bwrap 只读挂载 smoke 和 fake-helper 策略/超时/取消测试通过。真实网络隔离、host 路径 TOCTOU、真实 bwrap 超时/取消和 Python/平台接线未验收 |
| R5 串行 Graph Runner | 展开后 Graph 快照、Run 状态、串行路由、不可变提交输入、恢复与停止 | 进行中：首个 Kernel Runner vertical slice、十种 provider-free Python `run()` 场景（含明确失败语义差异、未启动 SCC 闭包及嵌套模块回边重入）、format 1→2 Run 迁移、本机跨进程 lease 退出释放、结果顺序/cursor 输入事实校验、edge decision 晚于其 source result 的 freshness 校验、四个 test-port 跨进程 Runner 故障窗口，以及 FileRunStore 提交成功但调用反馈失败后的重载恢复测试均通过；生产 host/provider 崩溃恢复、更广语义覆盖仍待做。fanout/join Kernel 能力见 R6；当前串行/并行宿主见 R8/A60–A61，完整平台接线待做 |
| R6 fanout/join | 复用现有配对契约；分支活动身份、乱序收束、失败/停止/崩溃恢复 | 部分完成：Rust 已验证显式一一配对、非嵌套线性拓扑、Run format 3 activation，以及单 Coordinator 的 provider-free 局部并行、join 控制事实、node ceiling/module activation 约束和并行 wave 三个进程崩溃窗口。A61 已接真实宿主文件谱系及五领域 RSI provider 并行运行，并验证分支失败阻断 join、进程中断保留完成分支且不重放未知分支；平台控制与 Agent kill/restart 仍待做。不引入嵌套或隐式并行 |
| R7 Op.call 与 Plugin/MCP | wait/detach Graph call 通过既有 admission 语义复用同一 Runner；Plugin manifest、MCP stdio/HTTP、凭证与 Sandbox 绑定 | A82 已接入生产 Rust Host 的后台生命周期、child 独立控制/active 与父子关系投影，并通过 provider-free API 测试。Run 固定自身 Graph snapshot；PUT 与 trigger admission 共用短 lease，已有 Run 不受编辑影响；DELETE 仍拒绝存在任意未完成 Run 的 Graph。Plugin resources 漂移仍 fail closed。R7 仍待 Plugin child 执行、文件/result/session 语义、真实 Provider/MCP/Sandbox 与进程故障窗口验收。Python 平台兼容不再是出口条件。 |
| R8 Rust-native 宿主与独立包 | Rust API/CLI/Session 宿主和 `anchor-graph` 使用同一 Runner；Graph + Plugin 资源闭包与兼容清单 | Rust standalone 与 Rust platform host 对同一 Graph 产生一致 Run/提交/恢复事实；bundle 不含密钥、不扩大授权；旧 Python 只做可选 legacy 适配 |
| R9 产品迁移与 Python 收缩 | RSI、周报、企业微信助手逐个切换到 Rust-native host；Python 路径降为 legacy/迁移工具 | 每个目标 Graph 通过真实 provider、Sandbox、恢复和平台验收；达到条件后再移除不再使用的 Python Kernel 依赖 |

### R8 compatibility spike（历史切片，已被 Rust-native 方向取代）

第一步作为兼容性实验验证 Rust Runner 的真实子进程边界，不切换生产 Graph：

- Python `Scheduler` 继续唯一负责 admission、同 Graph busy 判断和 worker 生命周期；只调用一个受控 Runner adapter，不新增队列或调度器。
- Rust `FileRunStore` 是该实验 Run 的唯一可写事实源。平台传入已准入的稳定 `run_id`、服务端取得的展开 Graph snapshot、snapshot digest 和 JSON input。相同 ID、digest、input 的重试为恢复；身份或内容不一致必须拒绝。不能同时把 Rust Run 和 Python `run.json` 当作同一运行的两个可写权威。
- 跨进程协议与 Graph-call RPC 分开命名、独立版本化：4 字节 big-endian 长度前缀 + UTF-8 JSON，单帧最多 1 MiB，严格拒绝未知字段，带 `request_id`。首期仅 `start_or_resume` 与 `status`；请求不可选择命令、宿主绝对路径、环境变量、凭证或沙箱策略。
- 首个 Graph 仅允许一个 `Op.run` 节点，无 Plugin、Agent/provider、Graph call、fanout/join、Session 或文件/result handoff；不满足即 admission 拒绝。命令只能通过 host-owned Sandbox port 执行，策略来自部署配置。执行前 durable 记录 invocation 已启动/结果不确定；重启后无法证明命令终态时 fail closed，禁止重放。
- 该实验的 Rust status 暂不投影为现有 Python `runs()`、节点工作区和 Artifact 浏览事实。接入生产 Scheduler 前，必须单独完成只读状态投影、stop/pause 语义、Run/workspace/artifact 映射、平台重启和取消验收，并证明 Python 不会写出竞争状态。

这条 compatibility spike 仍作为协议和 Bubblewrap 的测试证据保留，但不再是生产迁移目标；它不代表 Python Runtime 替代，也不代表 R7 完成。

### R8 Rust-native 宿主契约（新的实施出口）

- `anchor-runtime` 是平台宿主和独立包唯一的 Graph Runner。平台 API、CLI、Session 和计划触发都调用 Rust 应用用例，不通过 Python Scheduler 转发。
- Rust-owned Run 使用 Rust-native `GraphRunRecord`、`AgentCheckpoint`、Artifact 和事件记录。旧 Python `run.json` 只读或由迁移工具转换；不对同一 Run 做双写，也不把 Python 的字段兼容当作成功条件。
- Rust API/CLI 负责 admission、控制、状态查询和事件流；HTTP 框架、CLI 参数解析和本地存储可以使用成熟 Rust crates，但所有入口必须依赖窄的应用端口，不能进入 Kernel 内部。
- HTTP 路由必须满足当前 React 核心画布/Run Inspector 的产品投影契约（Graph CRUD、trigger、Run list/detail/control、timeline/schedules、文件/产物）；具体字段清单见 [Rust-native WebUI API 出口](rust-frontend-api-contract.md)。React 页面在 Rust host 切换时保持不变，Pilot/Session、Plugin 管理 API 分批迁移。
- io-harness 是唯一 Agent loop，负责单节点上下文、compaction、SQLite checkpoint 与工具效果恢复；Rig 只提供 Provider transport。Anchor 持有 Graph Run、权限、Sandbox、Plugin、Artifact 与跨节点状态，不另建 Harness。
- 独立 bundle 首先支持一个真实 Graph 的资源闭包：展开后的 Graph、显式 Plugin/工具资源、Runtime 兼容版本和无密钥 manifest。凭证、模型配置、Sandbox 根目录和运行数据由部署环境提供。
- Rust-native host 的首个生产出口是一个包含 AgentNode、Op.run、Plugin/MCP、fanout/join 和恢复的真实 Graph；provider-free 只证明接口，真实 provider、Sandbox、重启和 artifact 需要分别验收。

## 当前进度和下一步

2026-10-03 决策更新：用户要求开发阶段直接以 io-harness 满足 AgentRuntime 需求。io-harness 是唯一 Agent loop 与上下文/恢复 owner；Rig 只作 Provider transport。该方向已从隔离 spike 推进到生产 HostNodes Agent 分发，Anchor GraphRunner/Sandbox/Artifact/Plugin 仍为外层事实 owner。能力范围为文本、图片、流式和简单 JSON 工具结果；复杂结构化输出不作为选型阻断，但 Anchor 的 `{summary, route?}` completion 仍由 Harness schema 本地校验。

2026-10-03 A66 provider-free spike 已完成并收纳到 `experiments/io-harness`（独立 Cargo 包，不加入主 workspace）。固定 `io-harness=0.86.0`，5 项测试通过：30 次 scripted JSON 工具调用实际触发 semantic compaction；SQLite Store 关闭/重开后恢复且不重复已完成 step；`ToolRecovery::Indeterminate` 在未知副作用后进入 `AwaitingRecovery`，显式 `RecoveryDecision` 后才继续；ReadOnly 工具完成；完整 1×1 PNG media fixture 和 streaming delta 拼接通过。另以显式环境变量启动 live binary，对当前 OpenAI-compatible DeepSeek endpoint 完成一次真实文本请求（`Finished { steps: 1 }`，证据 `.local/io-harness-spike/live-text-evidence.json`）；首次低 step cap 的失败保留在推进记录中。该实验明确把 io-harness 的 Agent loop/context/checkpoint/recovery 与 Anchor Graph/Run/Artifact/Sandbox/Plugin 事实分开；它没有接 Rig 或生产 Graph。下一出口是实现窄的 Rig `Provider` adapter 与 Anchor Tool port 映射，先做真实模型单 AgentNode，再做 checkpoint/未知副作用恢复和 MCP/沙箱边界验收；任何无法无损表达的 response identity、复杂结构化结果或多媒体必须 fail closed。

2026-10-03 A67 窄转换适配层已完成，仍在独立实验目录。`experiments/io-harness-adapter` 固定 `io-harness=0.86.0` 与 `rig-core=0.43.0`，提供 `to_rig_request`/`from_rig_response` 和 `RigProviderAdapter`：保留文本、简单 JSON 工具 schema/调用/位置化结果、JPEG/PNG/GIF/WebP；按 message/call position 生成确定性 Rig call id；Rig response identity 不伪造回 io，reasoning、assistant image 和其他富内容直接 fail closed。5 个 provider-free 测试及 fmt/check/test/clippy/diff 通过，其中官方 `MockCompletionModel::stream` fixture 验证了两个 text delta 和最终响应一致。io-harness 仍是 loop owner，adapter 已把 Rig `StreamEvent::Text` 转发到 io token sink；Rig provider error 在缺少无损分类时保守 non-retryable，避免未知请求重复。它不执行 Anchor 工具或持久化，未加入主 workspace、未接 Anchor ToolPort/GraphRunner/真实外部 provider；下一片是 Anchor ToolPort→io Tool adapter，再做新 `IoHarnessNodeExecutor` 旁路现有 `NodeExecutor` 的单 AgentNode 垂直切片，不能把 io Provider 塞进 Rig loop。

2026-10-03 A68 Anchor ToolPort→io-harness Tool 垂直 spike 已收纳到 `experiments/io-harness-node`，固定依赖主仓库 `anchor-runtime-rig`、`io-harness-adapter` 和公开版本。`AnchorToolAdapter` 只转发 Anchor definition/call，单个 JSON result 保持 JSON 文本，多结果序列化为数组，富图片结果 fail closed；默认 `Mutating + Indeterminate`，显式 fixture 才可声明 `ReadOnly + Replayable`。Rig `MockCompletionModel` 经 RigProviderAdapter 驱动 io-harness 唯一 loop，fake Anchor tool 调用一次，下一模型请求读取结果，Run Finished。3 项 provider-free 测试、fmt/check/test/clippy 通过。该 crate 不进入主 workspace，不改 HostNodes/GraphRunner；下一步是把它接到一个可恢复的单 AgentNode backend，并绑定真实 Sandbox/Tool policy。

2026-10-03 A69 在同一独立 spike 增加 `IoHarnessNodeBackend`，以固定 SQLite Store path 提供 `start`/`resume`。resume 强制调用方传入同一冻结 `TaskContract`、Toolbox 与 io run id，不读取当前 Graph 定义；io-harness 自己拥有内部 checkpoint/compaction/recovery，Anchor Graph/Run/Artifact 事实仍在外层。provider-free 测试先让首轮达到 StepCap，关闭 Store 后重开并 `resume_with`，Anchor fake Tool 调用保持一次且最终 Finished；共4项测试、fmt/check/test/clippy通过。未接生产 HostNodes/GraphRunner，不证明 crash window、Sandbox/MCP 或真实 provider 恢复。

2026-10-03 A70 完成 `NodeRequest` 执行边界实验并收纳到 `experiments/io-harness-node-exec`。`IoHarnessNodeExecution` 将冻结的 Anchor `NodeRequest` 转成 io-harness `TaskContract`，用 `AnchorToolAdapter` 注册 ToolPort，并通过 `IoHarnessNodeBackend` 的固定 SQLite Store 提供 `start`/`resume`。只有 `RunOutcome::Finished` 且最后一个无 tool call 的 assistant turn 严格满足 `{summary, route?}` 时才映射 `NodeOutcome::Completed`；StepCap、启动前取消、provider error、recovery pause 等结果返回带 io run id 的 incomplete error。4 项 provider-free 测试、fmt/check/test/clippy 全通过，其中运行中取消由 io-harness `Observer` 在安全 step 边界收束为 `RunOutcome::Cancelled`。该 crate 仍不进入主 workspace，尚未接 `NodeExecutionPort`/`HostNodes`、进程 crash window、Sandbox/MCP 或真实外部 provider；实验性的 `NodeError` 转换在主线接入前需要保留不完整/恢复状态。

2026-10-03 A70 真实 provider 对照 smoke 通过简单文本边界：`deepseek-chat` + chat wire 完成同一 NodeRequest/Anchor ToolPort 任务，2 次 provider request、1 次工具调用、`route=done`，证据 `.local/io-node-exec-live/evidence-chat.json`。同 endpoint 的 `.env` `deepseek-flash` 由于返回 Rig `Reasoning` content 被转换层明确拒绝，说明当前范围仍是 text/simple JSON，reasoning 模型需要后续单独设计无损协议；没有为了通过 smoke 而扁平化或丢弃该内容。

2026-10-03 A73 将 `NodeHostResolver::tools` 改为命名的异步 `ToolResolution` future。`IoHarnessNodePort` 在专用 current-thread runtime 中等待 host-owned 工具解析，使未来生产 Plugin/MCP bind 可以沿既有异步边界接入，不需要同步 resolver 嵌套 runtime；Graph `NodeExecutionPort` 的外部 `Future + Send` 契约不变。17 项 runtime 测试及 workspace check/clippy 通过，尚未接生产 HostNodes。

2026-10-03 A74 修正 NodePort 的 durable marker 细节：同一 `InvocationKey` 已有 `.started` 时允许幂等重入；facts/io-store 目录创建、原子 rename 和 marker 删除均同步父目录。provider-free 测试验证 io-harness `Escalated` resume 仍返回 recovery-required，不会在外部结果未知时自动重发；Step/Time/Cost budget outcome 的 provider 请求数从 SQLite 累计调用表读取。GraphRunner 对 `CompletionFact::Uncertain` 仍没有 recovery decision API，故 crash-window 和生产 HostNodes 接线仍未验收。runtime 共18项测试。

2026-10-03 A75 阻断 io-harness 自带文件、shell、exec、内建扩展工具绕过 Anchor Bubblewrap/Plugin 授权。冻结的 NodeRequest TaskContract 用 io-harness 原生 `ToolMask` 屏蔽固定版本公开的内建工具名，只留 Anchor ToolPort 能力；新增 contract regression 后 canonical runtime 19 项 provider-free 测试通过。未接生产 HostNodes。

2026-10-03 A76 将 Graph Agent 的 `wall_time_limit_seconds` 映射到 io-harness `TaskContract::with_time_budget`，start/resume 使用相同冻结限制；io-harness SQLite 按 run elapsed 计量，包含停机时间。`TimeBudgetExceeded` 进入 Anchor `BudgetExhausted`，保留原 Graph cursor 和 io run id；累计 `max_provider_requests` 仍 fail-closed，不做轮数换算。新增 contract 映射测试后 canonical runtime 20 项 provider-free 测试通过，未接生产 HostNodes/provider。

2026-10-03 A77 完成 runner-host owned resolver 前置集成。io-harness 的异步 `NodeHostResolver` 现可解析宿主冻结的 PluginBinding，并返回完全 owned 的 Anchor ToolPort；`anchor_run` 通过 Bubblewrap、只读输入从既有 Artifact snapshots 派生。provider-free GraphRunner 单节点测试串起 fake Plugin、真实本机 Bubblewrap 命令、io-harness loop 与 Anchor Artifact freeze。canonical runtime 21 项测试通过；Rust workspace 189 项测试、check、Clippy `-D warnings`、fmt 与 diff 检查通过。此接线尚未启用生产 `HostNodes`：io-harness 路径 live MCP/真实 provider 未验收，Graph `Uncertain` recovery 仍无显式决策入口，`max_provider_requests` 也不支持；更不能在同一持久状态中无身份保护地切换 Rig 与 io-harness，否则不同 completion/checkpoint namespace 可能导致旧 invocation 被误当作未开始。后续以显式 Graph Uncertain recovery decision、逐节点模型 alias 解析、累计 provider-request budget、图片输入与外部业务 MCP 为推进项；A78 已完成生产 HostNodes 切换，不能再把生产切换列为下一步。

2026-10-03 A72 将 A67–A71 收敛到 workspace crate `rust/anchor-io-harness-runtime`，分层提供 `adapter`、`node`、`node_exec`、`node_port`，初始17项 provider-free 测试通过。原四个 `experiments/io-harness-*` 包改为薄兼容 wrapper，避免实验实现与未来宿主接线产生两份语义；A70 live binary 改用 workspace crate。`cargo check --workspace --all-targets` 与 Clippy `-D warnings` 通过。该步骤只完成代码归属和依赖收敛，不代表生产 `HostNodes`/GraphRunner 已切换。

2026-10-03 A71 完成 `NodeExecutionPort` 前置接线实验，收纳到 `experiments/io-harness-node-port`。host-owned `NodeHostResolver` 提供 workspace 与 Anchor `ToolPort`；`IoHarnessNodePort` 复用 A70 执行器，并通过 `spawn_blocking` + current-thread Tokio runtime 隔离 io-harness 0.86 `Store` 的非 `Send` future，使 Graph coordinator 仍满足 `Future + Send`。完成只在严格 A70 结果校验后写 Anchor completion fact；`io-harness` 已产生的 Step/Time/Cost outcome 返回 `BudgetExhausted`，取消返回 `Cancelled`，均不写完成事实；该切片不把 Graph 累计 provider-request budget 伪装成 io step budget，A70 NodeRequest 到 TaskContract 的 wall/cost budget 注入仍待明确；recovery pause、provider/store error 保留 `.started` 与 io run id 并 fail-closed 返回不确定错误；已知非法完成/工具注册错误才写 terminal failed fact。3 项 provider-free 测试及 fmt/check/test/clippy 通过。仍未接生产 `HostNodes`/GraphRunner、Sandbox/MCP、真实 provider 或 crash-window 验收。

2026-10-03 A70 收尾验证：io-harness NodeRequest 实验4项测试、运行中取消、fmt/check/clippy通过；Rust workspace check、全量 test 和 clippy 在授权环境通过。首次受限环境的本地 HTTP MCP socket 权限失败经授权环境复跑通过，不改变测试边界。A70 仍是独立实验，生产 NodeExecutionPort/HostNodes 接入留待下一片。

2026-10-02 用户补充硬约束：上下文管理是长任务与长期系统的核心，不希望Anchor自行实现复杂Harness。当前优先事项调整为A64框架能力评估，详见 [Rig/Harness核查](rig-harness-capability-audit.md)。Rig0.43已有高层Agent、memory policies、cassette与ECS，但不能把它们认定为PydanticAI/Harness长任务上下文与持久恢复的现成等价替代；“少量接线即可完整替代”目前未通过选型门槛。下文Op.call等保留为迁移待办顺序，不能以尚未满足的Harness假设继续默认扩建。已验证RustGraph/Artifact/Sandbox成果保留，生产路径和依赖不因评估自动切换。

当前推进已超过 A61：`HostNodes` 使用 io-harness Agent loop，Graph/Run/Artifact/Sandbox 与恢复路径由 Rust Host 持有；Rust HTTP 已把同一 React bundle 接到 Graph/Run/Artifact/Timeline 首片，非 loopback Bearer 流程和文件查看有浏览器验收（A80）。A81 已接生产 `GraphCallPort` 的有限生命周期语义：wait child Op.run可执行并持久查询；detach只有 durable admission，没有后台 dispatcher，不称为完整 detach。`Op.call` 文件/result/session、child Agent/Plugin执行、完整并发控制、Session/Pilot、Scheduler、Plugin 管理、relations/channel、跨平台沙箱/分发及生产 Graph 切换仍待推进。RSI 的执行链路可运行，但报告内容验收与长期收益尚不能视为完成。

A62已通过Rust HTTP Run生命周期首期验收：接纳/Graph身份先落盘，按Graph管理活动运行，暂停后以同一Run和冻结快照续跑，真实进程重启、Plugin漂移、单写入宿主与未知副作用边界验证通过。证据 `.local/rust-lifecycle-bwnpt18x/evidence.json`。后续保持复用application/execution，不复制执行入口；A80已完成核心 Graph/Run/Artifact/Timeline 的 React slice，Session、计划和其他平台能力仍待做。

R0 已由现有架构约束和本计划冻结；R1 已完成实验纵向切片，代码位于 `rust/anchor-runtime`，真实 provider smoke 已通过。R2 的 checkpoint 端口、文件适配器和 pending model/tool 失败续行已通过；R3 已加入可选模型/工具超时、独立 `StreamingCompletionPort` 和 `ObservedCompletionPort`。`execute_with_store_and_policy` 让宿主在同一次执行中组合持久化和超时策略，旧的 `execute_with_store` 委托给默认策略入口。确定性测试覆盖 unary timeout、流式取消后序列化/重载 pending model step 并用新 provider 续行，且终态会清除 pending step；这不等价于真实进程退出或 provider 断线恢复。真实 provider 流式 smoke 已通过。R4 已新增独立 `rust/anchor-sandbox-bwrap` host adapter；命令/网络授权与 host path roots 由 adapter policy 所有，Kernel 保持无子进程能力。本机真实 Bubblewrap 已通过只读输入挂载和 `workspace_readonly` 负向 smoke；fake helper 覆盖输出配额、设置失败、超时和取消。网络隔离、真实 bwrap 超时/取消、TOCTOU 和 Python/平台接线仍待验收。R5 首个串行 Graph Runner slice 已在 `rust/anchor-runtime/src/graph/` 落地：快照与能力 admission、持久 Run 状态/cursor、RunStore 原子文件+OS advisory lease、节点/产物 port、按边路由与回边、ceiling/module activation、budget/pause/stop/failure、固定 commit 输入、完成事实 crash-window reconciliation。provider-free oracle 当前覆盖十种 Python `run()` 场景，包含未启动循环 SCC 安全闭包与二层嵌套模块经根图回边重入；闭包仅在未运行组件的外部 ingress 已否决/来源已证明 inactive 时写 false 边，selected/未决 ingress 与历史已运行组件保持保守。RunStore 校验每节点结果 sequence 顺序、cursor 输入 commit 与 selected 入边精确一致，以及引用实际 source result 的 edge decision 序号晚于该结果。Run format 1→2 迁移、本机跨进程 lease 退出释放、四个进程故障窗口及 post-commit feedback Err 后必须重载 Run 的回归均有定向证据；workspace 74 项 runtime、8 项 Bubblewrap 测试，Clippy/fmt/diff 通过。仍是 fake Node/Artifact ports，oracle 覆盖有限；完整 Runner 崩溃恢复、真实宿主 adapter 或 R8 接线仍未完成，因此 R5 继续进行，不宣称 Rust Runtime 替代 Python。

2026-10-02：补充 R5 RunStore post-commit feedback 故障回归。FileRunStore 包装器先成功原子保存包含节点结果的 Run，再向 Runner 返回 Err；直接使用旧内存快照重试会得到 `RunConflict`，测试随后从 durable store 重载并用新 Runner 恢复为 Completed，确认 dispatch、结果与 artifact freeze 仅一次。随后补上 edge decision freshness 不变量：引用实际 source result 的决定序号必须严格晚于该结果；新增合法 stopped self-loop 与 stale decision 的 FileRunStore save/load 拒绝测试。Rust workspace 63 runtime + 8 Sandbox tests、Clippy `-D warnings`、fmt、10 场景 Python oracle 重生成及 Rust 差分、Python `test_simple_run.py` + `test_node_controlflow.py` 70 项、Ruff、diff 检查通过。Oracle 只在固定场景比较 Python `RunState.executed` 与 Rust dispatch 顺序，且未逐项比较 decision/result sequence 或 module activation；这些限制不被表述为普遍语义等价。以上均为 test-only provider-free 故障注入，不模拟真实目录 sync/磁盘错误，也不构成 host/provider/platform 恢复验收。

2026-10-02：R6 Rust 最小区域调度接入。fanout 与 join 均由 Graph Coordinator 生成确定性 manifest，作为普通 `RunResult` 经 `ArtifactPort` 冻结；join 不要求 `NodeExecutionPort` 的 `op_run` capability，后续 AgentNode 消费 join commit。分支 cursor 在 dispatch 前逐个持久化；不同分支通过唯一 Coordinator 的 `FuturesUnordered` 并发执行，分支内部沿静态路径串行；完成按到达顺序逐个 freeze、记录 result/edge、更新 branch progress 并保存。全部分支完成后才收束 join；Failed/Uncertain 不放行 join，已启动 future 先 drain，Cancellation 仅 fail-closed，不宣称同伴 exactly-cancel。provider-free 测试覆盖并发峰值 2、乱序完成、join manifest 两支结构化事实、失败不调度 join、BudgetStopped/Cancelled reload 不重放已完成分支，以及 format3 `FileRunStore` activation/cursor roundtrip。并行分支已复用普通 node ceiling/module activation；并行 wave 三个进程崩溃窗口也通过 durable test ports 验证。真实 host/provider、生产 workspace 接线仍未完成；不得据此宣称完整 R6 或平台验收。

## 每阶段记录

每个阶段必须同时更新：

- `docs/pilot-development-plan.md` 的验收矩阵和推进记录；
- 本计划的阶段状态与证据位置；
- Rust 单元/集成测试以及 `cargo fmt`、Clippy、`git diff --check`；
- 真实 provider、Sandbox、平台或独立宿主证据（如果该阶段要求）。

阶段完成只表示该阶段出口条件满足，不表示整个 Anchor 已经完成 Rust 重构。

### R4 Sandbox 契约归属

Rust 请求表达执行意图和可验证的边界字段；真实授权与主机资源始终由宿主适配器持有。`workspace_readonly`、`tool_dirs`、环境变量、输出预览上限和 spill 配额现已进入请求契约。`SandboxResult` 另区分宿主 spill 文件、沙箱可见路径和无法完整保留的 `incomplete` 输出。环境变量值及宿主 spill 目录在 Debug 输出中脱敏。

| 数据/能力 | 所有者和适配要求 |
| --- | --- |
| argv、工作区只读相对路径、网络意图、超时、输出预览/保留配额 | Kernel/节点调用方提出；适配器必须在结果中如实反映执行、超时、取消和输出完整性。`network=enabled` 只是请求，不是授权 |
| 实际 workspace、只读输入源、tool_dirs、spill host directory、env 值、取消句柄 | 宿主拥有并绑定；Graph/模型不得自行挑选宿主绝对路径、注入任意凭证或扩大授权。host spill 目录必须由宿主预创建在受控 Run 存储内；adapter 校验前不创建请求路径 |
| 路径安全、符号链接与挂载、网络 namespace、子进程终止、输出与磁盘限额 | 真实 adapter 的强制职责。独立 bwrap adapter canonicalize host paths 并校验 host policy roots，拒绝 `workspace_readonly` symlink 和保留挂载目标覆盖；当前不宣称可抵御并发宿主文件系统改写（TOCTOU） |
| `spilled_host_paths`、`visible_spill_paths`、`incomplete` | 适配器产生的执行事实；host path 仅供宿主后续读取/清理，不传给模型或普通日志；模型可见的只能是 sandbox 内路径。丢失超出配额的数据时必须设置 `incomplete=true` |

Bubblewrap adapter 位于独立 host crate `rust/anchor-sandbox-bwrap`，不把进程执行权加入 Kernel。`BubblewrapPolicy` 显式配置命令 allowlist、workspace roots，以及使用到的只读输入/tool/spill roots 和 sandbox destination roots；spill 目录必须先由宿主创建，adapter 只 canonicalize 和授权校验，不创建请求路径；网络另需 host policy 开关。环境值通过清理后的子进程环境传递，不放进 bwrap argv。双流并发读取并按 preview 与共享 spill quota 限制；reader 或 spill 落盘不完整会设置 `incomplete`。Bubblewrap `--info-fd` 启动握手未成功时返回错误，不把 setup 失败记作 `Completed`；超时/取消会 kill 并 wait。

本机真实 bwrap smoke 已验证只读输入绑定和 `workspace_readonly` 写保护；fake-helper 覆盖 host policy 拒绝、路径 symlink、输出界限、setup 失败、超时与取消。这不等价于网络 namespace 负向证明，也未覆盖真实 bwrap 的超时/取消。A35/R4 尚未完成；不得接入 Python Sandbox/平台或宣称替代现有运行时。不能实现的字段必须拒绝请求，不能静默降级。

### R5 串行 Graph Runner 契约（冻结）

- Rust Runner 输入是与 Python `graph.json` 同形的**展开后快照**。Runner 不重新解释 YAML/模块引用，也不在恢复时读取 Graph 工作区的当前版本；模块展开与独立 Graph bundle 编译另属宿主/打包工作。
- Runner 拥有 Graph Run 状态：稳定 Run 身份、快照摘要、状态、cursor、节点 pass/invocation、边决定、提交历史和 stop/pause 原因。节点 `AgentCheckpoint` 仍只保存一次 AgentNode 的 Rig 协议状态，二者不能互相替代。
- 每次调用节点执行 port 前先持久化 cursor；节点工作通过节点执行 port，产物通过 artifact/workspace port 冻结为不可变 commit。下游输入必须引用上游精确 commit，不能挂载会继续变化的工作目录。
- 恢复核对 Run 状态与节点完成事实。完成事实已经持久化但 Run 尚未提交时补记结果而不重跑；事实缺失/冲突或外部副作用结果不明时 fail closed，不猜测重放。commit、节点事实与 Run 状态的多存储窗口必须有明确错误结果。
- 串行调度复用 Python 的入口、全部入边决定/至少一条新选中边、多出口唯一 route、回边、回合 ceiling 和作用域计数语义。多出口缺 route 属节点契约错误；`max_steps` 是累计 provider-request 预算，不可未经证据直接映射成 Rig `max_turns`。
- R5 明确只承接串行协调；遇到 fanout/join 在 admission 时拒绝，R6 才启用配对区域。Runner 不静默忽略插件或 `Op.call`：节点工作由声明能力的执行 port 承接，暂未提供的 Node kind/capability 必须在执行前拒绝。`Op.call` 的 wait/detach 语义在 R7 由同一 Runner 与专用 call port 实现，不建立第二个调度器。
- pause 只在节点边界停下；stop 取消当前节点并保留 cursor/checkpoint 事实。R5 做 Kernel 与 Python oracle 的 provider-free 对照，不接 Python 平台或独立 CLI；宿主接线归 R8。

R5 首片已覆盖展开快照 admission、入口/路由/未选分支、循环回边与回合上限、module scope 重入、不可变 commit 输入、预算停止续行、完成/失败事实的 RunStore crash window 对账、Uncertain fail-closed 和 pause/stop 边界；另以 Python `graph.to_dict()` 生成的 `one-search.snapshot.json` 验证真实序列化形状能被 Rust admission 读取。provider-free Python oracle 直接调用 Python `run()`，覆盖十种场景并逐项比较可映射的状态、输入、执行次序、passes、ceased、cursor、skip 与边选择。需准确理解其执行次序证据：生成器单独导出 Python `RunState.executed`，但 Rust 差分测试以 NodeExecutionPort dispatch 次序作为对应代理；十个固定场景一致，不证明所有失败/中断时两种事实普遍等价。边选择 oracle 比较路由真假，不比较 decision/result 序号新鲜度；module activation 计数也不在该 oracle 断言内。失败场景明确固定一项有意差异：Python 会在单出口失败后记录 selected edge；Rust 保留已启动计数、清除确定失败 cursor、不结算边或运行下游。`Uncertain` 保留 cursor。pause-before-dispatch确认没有创建pass、边或cursor。diamond convergence 要求未选源节点传播 false 边，使所有入边可决后仍可执行有选中输入的 merge；Rust 还覆盖多级跳过、未运行循环防过度传播、较新 false 输入撤销旧 selected 输出、模块 activation ceiling 的本轮边事实。GraphRunRecord format 2 记录在 Runner 持 lease 后迁移旧 format 1，旧 cursor 的启动计数只补记一次。跨进程子进程测试覆盖 FileRunStore lease 在进程退出后释放，以及 cursor-only 后、durable Completed fact 后、durable Failed fact 后、durable Artifact commit 后四种 Runner 崩溃点；父进程以新 Runner 与 test ports 恢复并核对是否重放。该证据限于 test ports 和本机进程，仍不是生产 host/provider 或平台端到端恢复验收。FileRunStore load/save均执行跨字段完整性校验，引用实际结果的 edge decision 必须严格晚于 source result sequence；`result_sequence=0` 的 synthetic false decisions 不适用，未新增 source latest invocation 约束。该 freshness invariant 有 FileRunStore save/load 损坏记录回归，Run format 不变。旧 format 1先按明确迁移规则转换再校验。阶段出口仍要求覆盖更多图/失败组合、精确 Run 状态对照和必要的持久化故障路径；平台/CLI 接线留 R8。不能证明等价的语义先返回明确不支持/不确定状态，不能以默认值改写行为。

2026-10-02：R7 Rust Kernel `Op.call` 与 Plugin manifest pinning provider-free 切片。唯一 `GraphRunner` 通过宿主 `GraphCallPort` 以父 Run、Graph digest、node、invocation 和 call-spec digest 组成稳定身份接纳/查询 child；`wait` 将父 Run 保存为 `WaitingCall` 并保留 cursor，重载后继续轮询同一 identity；`detach` 只提交 child Run 引用，child 已完成时也不向父节点泄露业务结果。port 契约要求按 identity 原子幂等接纳，未知结果 fail-closed。Plugin resolver 在 dispatch 前解析无密钥 `PluginBinding`，包含 manifest digest/resources/MCP server ids，并将映射持久到 Run；恢复时摘要变化会终止 Run，不 dispatch 新节点。Run format 3→4→5 显式迁移。Admission 已验证 Python `Op.call` 的 graph/mode/input/input_map/files/result/session 结构并将完整 spec 传入 GraphCallPort；实际 input mapping、文件 bundle/result materialization、session 语义仍由尚不存在的 host adapter 实现。MCP stdio/HTTP、凭证/Sandbox adapter、Python 平台接线及独立包均未实现。provider-free 测试覆盖 wait reload 同 identity、detach admission-only（含 child 已完成）、无 `op_run` capability、Plugin manifest drift 拒绝及 format 3→5 缺字段迁移；Rust workspace 78 runtime + 8 Bubblewrap tests、Clippy `-D warnings`、fmt 与 diff 检查通过。该切片不是完整 R7，也不证明可替代 Python Graph-call/Plugin 执行。
2026-10-02：继续推进 R7 宿主切片。新增隔离 crate `rust/anchor-graph-host`，由 host 提供 Graph catalog 和共享 Run/Artifact/Node ports；基于 `FileRunStore` 以 call identity 派生 child Run ID，先核 parent durable Run cursor 和冻结 call spec，再由同一 `GraphRunner` 执行 wait 子图，重载后复用已完成 child；detach 只持久接纳。输入映射、files、result、session和含嵌套call的Graph明确拒绝。新增 `FilePluginCatalog`：校验 plugin id、拒绝 symlink/路径逃逸/包内 credentials，提取 bundle资源/MCP server names并计算稳定 digest，绑定到 Runner Plugin resolver；catalog 五个文件系统测试通过。新增 `rust/anchor-mcp-host`，基于固定 `rig-rmcp=0.43.0` / `rmcp=2.2.0` 实现 manifest-bound tool inventory、Rig DynamicTool 转换、Streamable HTTP 与 secret-redacted host config；stdio launch fail-closed，等待宿主 Sandbox adapter 提供已隔离 RMCP service 后再绑定。4个 fake RMCP/in-process policy tests通过，无真实 MCP server/provider smoke。新增 Python GraphCalls 本机 length-prefixed JSON v1 facade groundwork：绑定 frozen parent Graph/call identity，status/cancel 限定到已接纳调用；无 Rust transport/client 或 serve/platform装配，输入映射/文件/session受限。Rust workspace runtime 79 + bwrap 8，MCP host 4、graph host 8 provider-free tests；各 crate Clippy/fmt通过；Python Graph-call与RPC相关70 tests、Ruff通过。以上仍非完整 R7；真实 Sandbox/Provider/MCP、运行平台调用 Rust Runner、Python bundle/artifact互操作与恢复验收待做。

2026-10-02：R8 Rust host Plugin/MCP binding 首片完成。`anchor-runner-host` 将 format-1 bundle 的 secret-free `PluginBinding` 传入同一 `GraphRunner`，通过 `ANCHOR_RUST_MCP_SERVERS` 读取部署者提供的 HTTP endpoint、凭证环境变量和 allowed_tools；`anchor-mcp-host` 建立 Streamable HTTP inventory，`ToolPort` 只暴露 manifest server 与部署配置的交集。缺配置、凭证、工具或非 sandbox stdio 明确 fail-closed。仅 provider-free/配置负向测试通过，真实业务 MCP/provider、平台切换和 checkpoint 重启仍待验收。

2026-10-02：R7/R8 HTTP MCP 补验收：本地 RMCP 服务经 loopback TCP 验证 transport、Bearer、inventory 白名单及 runner CombinedPluginTools。修复节点 network=false 绕过和 structuredContent/媒体结果丢失，复用 Rig 公开 mcp_result_output；连接前拒绝工具名冲突并对重复 server 去重。MCP host 5 tests、runner host 13 tests 通过。此为 provider-free 本地协议服务验收，外部业务 MCP + 模型 + 完整 Graph/重启恢复尚未验收。

2026-10-02：A60 完成首条 Rust-native 多节点文件流程：Op生成→Agent通过anchor_run读取只读上游、调用本地HTTP MCP、写报告→Op验证产物，真实配置模型7次请求后Completed。宿主提供按invocation隔离的workspace与fs1固定文件快照，同一GraphRunner服务stdio/HTTP入口；未知节点副作用不重放。Rig结构化输出接线修复真实模型prose终态，业务工具不重做。证据 `.local/rust-multinode-rov5xhss/evidence.json`，复跑见host README。本机MCP是fixture，未声称外部业务插件或完整平台迁移完成。R8当前host从单节点扩展到串行多节点，fanout/join与Op.call仍未开放；后续重点是并行文件传递/控制恢复/业务图迁移。

2026-10-02：A61 把局部并行接入真实宿主文件层：fs2父commit谱系、控制节点文件、最近祖先只读挂载及故障守卫均有真实子进程测试；五领域RSI真实运行166次模型请求完成，使用独立Rust证据MCP访问项目/历史/公共依赖元数据。该技术链路与报告质量分开验收：纠错Run另53次请求后仍有事实误报，模型review通过不作owner通过；独立意见见 `.local/rust-rsi-dwge0rsx/owner-review.md`。一项真实Graph名称响应缺陷经工程师复现、修复及HTTP回归，未自动实施其他建议。R6已取得真实host/provider并行证据；R7仍缺Op.call宿主/stdio沙箱接线等，R8完整前端/控制/Session/计划未完成，R9生产图与定时任务未切换。自动可靠RSI内容、广泛社区/特性调研与长期改进仍待验证。

## Plugin生态与跨操作系统分发出口（A63）

- Web保留React/TypeScript，编译后静态文件可由Rust宿主提供；部署核心不要求安装Node.js。A80已验收 Graph/Run/Artifact/Timeline 核心浏览器路径；其他 Rust 产品能力接入后仍需相应浏览器验收。
- Plugin是语言无关的目录资产：清单、Skill、资源、MCP工具。Rust迁移的是加载/授权/生命周期，不要求社区Plugin重写成Rust。核心无Python依赖与第三方Plugin的Python/Node依赖分别验收、分别声明。
- 社区来源安装需保留Codex来源清单定位及现有Skill/资源/MCP可用范围，不能把安装后根plugin.json解析误报为完整生态兼容。hooks/commands/agents尚不支持；Rust安装、资源挂载/发现、stdio/HTTP和凭证接线用真实案例逐项验证。
- 当前仅Linux/Bubblewrap有运行证据，且有Unix进程、信号及目录持久化假设。Kernel、模型/MCP协议与平台适配分开；Windows/macOS原生沙箱、进程/服务生命周期、文件语义、构建和安装均待实现或验收。浏览器跨系统访问、通过Linux VM/WSL运行、原生本机执行是三个不同支持等级。
- 分发验收矩阵须列OS/CPU架构、沙箱backend、外部命令和Plugin依赖；Graph含sh/jq/Linux命令时不得宣称天然跨平台。尚未选择Windows/macOS生产沙箱backend，不以不隔离运行作为默认回退。

### Op.call 宿主接入顺序（A62后的工作）

A82 已把 wait/detach 生命周期接到生产 Rust Host：detach 持久接纳后独立运行，启动时只接续明确标记且仍为 Ready 的 detach child；未知 Running 不重放。wait child 有独立控制与父等待关系，不同调用身份可并发运行同一目标 Graph，Run list/detail 投影父子来源。Run 接纳时保存 Graph snapshot；PUT 与触发读取/持久化快照共用短 admission lease，定义编辑影响后续 Run，不改既有 Run 的恢复语义。2026-10-04 收敛 Graph 删除：创建、编辑、删除经短 catalog mutation gate 串行；当前有效 Graph 定义或未结束 Run 的冻结 snapshot 有 `op.call` 指向目标时拒绝删除。已结束的历史 Run 不算活动调用方；目标自身仍有未结束 Run 时拒绝删除。删除只清理目标 Graph 和它自己的 Run/文件，不级联删除或改写调用方及其子 Run。R7 仍未覆盖 Plugin child 执行、文件/result/session 语义、真实 Provider/MCP/Sandbox 与跨存储故障窗口。

当前宿主只放行 `op.call` 的 `graph`、`mode`、可选对象 `input` 三字段；`input_map`、`files`、`result`、`session`、未知配置及嵌套调用均拒绝。PluginBinding固定进child record，当前target catalog Plugin漂移 fail closed。child Agent/Plugin执行、Plugin资源生命周期验证、文件/result传递和真实进程恢复仍待后续切片。

文件转交沿已冻结call.files/result语义：从父调用可见的已提交输入中复制选定文件至有来源摘要的只读转交快照；子产物复制到父call invocation工作区，再以父自身fs2提交。维持fs2同Run祖先校验，不能直接给模型foreign commit授权或放宽到整个父工作区。先验wait/detach生命期，再验真实父Op→子Agent/MCP→选定返回文件→父验证，最后补接纳/转交/完成进程中断窗口。Session及递归保护另按R7要求推进。

A62的同Graph互斥用于手动/API触发；未来独立调用仍遵守已冻结的“多来源调用同一目标产生独立并发Run”，不得把手动互斥策略推广为所有子调用互斥。每个Run始终只有一个写入执行者。


2026-10-03 A78 生产切换：`HostNodes` 的 AgentNode 已调用 `IoHarnessNodePort`，Op.run 保持 Bubblewrap；Rig 仅是 Provider transport。`CompletionFact::Resumable` 允许带 durable Harness run id 的同一 invocation 经 GraphRunner 进入 `resume`，没有 Harness cursor 的 `.started` 与 Rig/io completion 冲突继续 fail closed。新增 GraphRunner + HostNodes NodePort 重启回归：mutating `anchor_run` 已执行后模拟 provider 中断；重启后 Harness 要求显式 recovery decision，Artifact workspace 中副作用计数仍为一次。此回归未覆盖 Application/API 状态映射：当前 PreparedExecution 将未处理 recovery error 记为终态 Failed，Graph recovery decision endpoint 尚未实现。工具保留名/重复名在 started marker 前验证。真实 DeepSeek Chat 多节点 Op→Agent→Op、loopback HTTP MCP fixture、Bubblewrap 和最终文件校验通过，6 次请求，证据 `.local/rust-multinode-lyk86avx/evidence.json`。该证据不代表外部业务 MCP；Graph recovery decision API、`max_provider_requests`、图片输入、逐节点模型 alias registry 和 reasoning 富内容兼容仍未完成。Host 目前使用部署级单模型配置。最终 workspace 196 项测试、check、Clippy `-D warnings`、fmt、build 与 diff 检查通过；真实 provider smoke 证据 `.local/rust-multinode-lyk86avx/evidence.json`。

2026-10-03 A79 同一 Graph Run 恢复入口：GraphRun format 7 保留 `WaitingRecovery` 与精确 invocation 绑定的 pending io-harness attempt。底层仍 fail-closed，普通 `/resume` 会把未决工具、step 与结果未知的事实写入同一 Harness 上下文，让 Agent 核查现场后续做；不会自动重放工具，也不要求用户选择 Retry/Completed/Abort。内部 `/recovery` 路由仅为兼容入口。重启后同一 Graph digest、invocation、Harness attempt 与下游 Artifact 的真实 provider Completed smoke 已通过，证据 `.local/rust-recovery-cqifxcyz/evidence.json`；跨存储故障注入和外部副作用 exactly-once 不承诺。

2026-10-03 A80 Rust Host → React 浏览器垂直切片通过。Host 可托管 React bundle；静态页面/资源不要求 API key，Graph/Run API 仍由 Bearer key 保护，使非 loopback 部署可先加载登录界面。Playwright 从 Rust Host 页面输入 key、运行无 Plugin Graph、在节点面板查看 committed Artifact，并从 Timeline 打开 Run。Host API 的 `/timeline` 投影真实持久 Run，明确不支持计划；无持久结束时间时前端不造时长，非活动的 `running` Run 显示等待接续。验证：runner-host 56 项测试、workspace Clippy/fmt/diff、Web 33 项单测/build、恢复与 Rust Host Playwright 4 项。该 slice provider-free；不代表真实模型 trace UI、计划/Session/Plugin 管理或全平台替代。
