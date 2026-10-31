# 当前架构

Anchor 的维护实现是 Rust Host + 共享 Runtime Kernel + Goose ACP，WebUI 为 React/TypeScript。官方工具、发行 builder 和开发检查位于同一个 Cargo workspace。产品目标见 [产品与系统架构](product-architecture.md)，当前验收结论和未关闭范围见 [验收摘要](pilot-development-plan.md)。

## 执行链与所有权

```text
WebUI / CLI / Webhook / 通道
        ↓
Rust Host 应用用例：接纳、鉴权、Session、控制、Library、Scheduler
        ↓
共享 GraphRunner：节点、路由、反馈、fanout/join、Run 状态
        ↓
NodeExecutionPort
  ├─ AgentNode → Goose ACP → 授权 MCP → Sandbox / Plugin
  └─ OpNode    → Bubblewrap 命令 / Graph Call / 通用 host operation
        ↓
Artifact、节点历史、工作区、Run / Session / Turn 事实
```

| 模块 | 职责与事实 |
| --- | --- |
| [anchor-kernel](../rust/anchor-kernel) | 独立提交/裁决/任务生命周期内核；成本从全部裁决历史派生，等待中只接受生命周期控制，Code 凭观察可停止但不能释放人的等待。K0.1 与 Graph 中断事实见[工作记录](constitution-refactor-log.md#k01内核修订与中断事实)；所有 Host Run 的唯一生命周期权威，存储与调度边界见本节下文 |
| [anchor-runtime](../rust/anchor-runtime) | Graph 编译、IR、纯校验器、KernelScheduler 和节点执行端口；不依赖 HTTP 或具体业务 |
| [anchor-runner-host](../rust/anchor-runner-host) | API/应用接纳、共享 Kernel 调度、Goose 接入、Artifact、渠道协调、调度和控制 |
| [anchor-graph-host](../rust/anchor-graph-host) | bundle 读取、资源绑定、独立 Graph Call 和恢复关联 |
| [anchor-platform-session](../rust/anchor-platform-session) | Session/Turn、幂等请求、问题/回答、事件游标、投递事实及助手 current/历史 binding 与 retire 事实 |
| [anchor-library](../rust/anchor-library) | Plugin/tool catalog、受审资源、凭据引用和 OAuth transaction |
| [anchor-sandbox-bwrap](../rust/anchor-sandbox-bwrap) | 文件挂载、命令、网络与取消边界 |
| [anchor-mcp-host](../rust/anchor-mcp-host) | MCP 连接、工具与媒体内容映射 |
| [anchor-scholarly](../rust/anchor-scholarly) | 官方学术搜索、论文读取与 MCP |
| [anchor-docmost-tools](../rust/anchor-docmost-tools)、[anchor-wecom-tools](../rust/anchor-wecom-tools)、[anchor-git-tools](../rust/anchor-git-tools) | 原生业务工具；Git Plugin 调用本机 Git，与节点工具共用工作区 |
| [anchor-wecom-gateway](../rust/anchor-wecom-gateway) | 企业微信 WebSocket、ACK、去重与私有控制 socket |
| [anchor-rsi](../rust/anchor-rsi) | RSI/周报采集、证据、评审门禁与安全产物装配；复用 Host 的路由与 Runner |
| [anchor-distribution](../rust/anchor-distribution) | 受审闭包的 source-free 归档，不执行输入程序 |
| [anchor-devtools](../rust/anchor-devtools) | 确定性回归、候选构建、部署预检与切换盘点 |
| [apps/web](../apps/web) | Graph 画布、Run 时间线、节点对话、Session 和产物投影 |

GraphValidator 已支持并行分支内模块作用域：同一并行激活内共享模块首次从外部进入时计一次，后续激活重入递增，模块上限按首次终结事实结算。入口、成员 passes 与嵌套计数重置由纯校验逻辑决定；读取 frontier 不消费激活，也不增加持久化字段。该能力已通过内核用例与 5,000 个模块随机样本；Host 的 KernelScheduler 已接通多槽位并发，单个结果提交后即可补位，慢分支不形成波次屏障。串行 Op.call 复用共享调用准入、输入暂存与结果导出，父子 Run 都走 Kernel；宿主提交 Bound 后等待子 Run，观察结算后提交 resume 及 Completed/Failed/Uncertain。并行分支中的 Call 由共用的 Host 准入 `reject_snapshot` 具名拒绝，不扩展校验器，见 [M2 产品准入边界](../experiments/constitution-host-preflight/m2/parallel-call-boundary.md)。

所有新 Run（HTTP、渠道、助手、子调用与独立宿主）都通过 `application/run_access` 接入共享 `KernelScheduler<S: Store>`，不再按存储分派。`POST /trigger` 不接受 `execution` 选择字段。调度器读取 Kernel 相位与 GraphValidator 开放空位，经 NodeExecutionPort / ArtifactPort 并发执行并提交。Run 只存于 `<state>/kernel-runs` 的 Kernel FileStore（资源 ID `run/{run_id}`），由单个 Host writer 持有文件锁；旧 `<state>/runs/*.json` 只检查存在性并以 `legacy_run_record` 具名拒绝，不读取内容。GraphRunRecord format 15；旧格式 1–14 不兼容，开发状态清理见 M3 交回。Graph 记录不再持有 error/recovery/recovery_submissions/updated；失败原因来自 Kernel 结算，恢复观察/人工决定来自已接受 history，更新时间归 Host RunObservations。字段归属见 [M4 审计](../experiments/constitution-host-preflight/m4/field-disposition.md)。

宿主Rust文件租约统一使用 `anchor-file-lease::FileLease`，保留各调用者的共享/独占、阻塞/非阻塞策略，Drop先显式解锁再关闭文件，提前返回与panic同样释放；避免fork继承的描述符延长父进程锁持有。

Run 的 stop/resume/abandon 是 Human Request；内核接受 stop/abandon 后才取消执行，在跑结果可被 TaskWaiting / TaskSettled 拒绝并留在历史。节点失败由 Code Settle，成功 Work 由 Agent Write，成本只从宿主观察经 submit 参数盖章，StaleVersion 重试不重复收费。Uncertain / WaitingRecovery 以 Code + Observation stop 保存未知效果；后台自动恢复不释放等待、不自动重放，人的显式 resume 允许 Agent 核查后继续。重启读取相位、开放空位和端口 completion_fact，以同 invocation 恢复；没有第二份恢复游标。执行错误保留 Kernel 事实；孤立执行显示为 Executing、active=false。唯一 worker 表在 KernelRuns 中，派发前同步登记，退出或未启动任务被丢弃时释放。Host 的只读 RunView 包含 Phase、Graph 进度、frontier、worker 和 updated 观察，动作由共享相位派生，Session/身份条件进一步收窄；pending 未知效果不隐藏普通 resume，不再保留旧状态映射。pause 是 Executing 且有 worker 才可用的宿主调度意图：持久保存后通过事件交给派发边界，当前节点结果与成本先提交，再以 Human stop、reason=paused 等待；不取消当前节点。重启先恢复端口已有完成事实，再消费意图；最后节点已结算时保留结算。意图消费、控制接受和清理共用宿主锁，不新增 Kernel 状态或执行标志轮询。空闲 wait_input 使用每 worker 的 Notify 接收同一 pause 事件并返回 Suspended，再由调度边界提交 stop；模型或工具工作不被该事件取消。更新时间来自独立的`<state>/kernel-run-observations/{run_id}.json`：创建及接受提交后由宿主记录，不读取Kernel文件布局或mtime，不用于控制、权限或恢复；观察故障不撤销接受事实或阻断控制，无观察时对外 updated 为 null。

Kernel Run/Graph 删除经 run_access 门面，在宿主锁内复查相位、worker 与原有 Session/引用约束，再调用 FileStore::remove；存储通过 rename 到 `.deleting-<encode(id)>`、同步父目录、递归删除、再次同步完成删除，读取幂等清理中断现场。Store trait 和 MemoryStore 不增加删除，独立资源锁文件保留。Kernel Probe 接口的测量点投影到 `<state>/kernel-probe/points.jsonl`，启动从保留的 Graph history 重建；投影故障只诊断，不影响裁决和控制，也不提供新的对外 API。

唯一执行路径支持 Work、fanout/join、并行模块、串行 Op.call、宿主操作、常驻助手与 Run/Graph 删除。助手绑定、活动节点和 HTTP trace 的 invocation 从 GraphValidator frontier 只读派生，不另存游标。Yielded 冻结 Interruption 产物后由 Code Write 提交，预算耗尽由 Code + Observation stop 等待人恢复，Interrupted 保留原因，Aborted 对应人放弃，Cancelled/Suspended 排空退出。HTTP 准入预算仍用 `u64::MAX`；有限预算与端口结果由 fixture 验证，W4 未裁定。旧执行器已删除，30,000 例旧侧事实与轨迹的冻结对照见 [快照基线](../experiments/constitution-host-preflight/m3/snapshots/README.md)。

Kernel 的未知效果 attempts 与决定从已接受 Kernel history 投影；原生 Goose 使用现有 invocation fact 的 `tool_observation`，未完成且 result 为 null 时形成待核查事实。attempt ID 取 invocation、工具、参数和原生 tool call 身份的 SHA256 前 63 bit（最小 1）；step=0 表示没有旧 Harness step 序号，旧观察无时间时显示 unknown。人的显式 resume 与认证 Session-call 共用 `inspect_interrupted_attempts`：先发现原生 pending，经唯一 `observe_recovery_attempts` 复用 Runtime 的 `observation_stop` 提交 Code + Observation stop，再逐项将“先核查现场，不盲目重放”的提示持久追加到原观察的 `operator_decision`，最后提交 Human release，让 Agent 在同一原生 Session 中核查后继续。提示不代表未知工具已经成功，不伪造既有工具结果或模型 NodeCompletion；后台自动恢复不调用该释放 helper。

人工 Completed/Abort 决定 API 经 `record_recovery_decision` 保留，供人已知结果时直接裁定；先持久保存人工说明，再 Human resume 或 abandon。`prepare_resume` 在 worker registry 锁外执行，保留原验证与错误优先序；相同决定幂等、不同决定冲突，多 pending 逐项保存决定后才释放，Abort 保留原观察并结算放弃。

Goose 拥有 Agent loop、Provider、原生会话与 compaction；Anchor 拥有 Graph/Run、宿主授权、Artifact 和平台 Session/Turn。工具记录、API 与 UI 都围绕相同身份，UI 缓存不能成为第二份运行事实。

AgentNode 默认按 Run、Graph 身份和 node 建立稳定原生 scope，同一节点后续 invocation 通过 ACP `session/load` 延续；不同 Run/node 默认隔离。受信任的消息 Session 绑定才能授权跨 Run 延续，Workspace 共享不隐含共享对话。同 invocation 恢复保留冻结绑定与未知效果核查，新 invocation 读取当前模型设置。Run 内原生 scope 在释放 Workspace 和执行事实前清理，使用中的 scope 受同一原生租约保护。旧的 invocation 独立会话不自动拼接或迁移；上下文连续性与 Provider 缓存命中分别验证。

渠道的逻辑账号由配置入口统一生成，兼容别名由 SessionStore 按 owner/source 持久登记；Session 接纳、消息去重与恢复使用同一解析结果。配置表示改变不另建逻辑会话，已有重复历史不自动合并。Host 在渠道配置加载后恢复助手，只有当前部署匹配且未被规范身份会话取代的绑定具备恢复资格；交接的新来源必须仍为 current。详细兼容和暂停边界见[渠道契约](wecom-assistant.md#渠道身份与历史兼容)。

Web 管理与 Pilot 管理调用相同 Host 用例。插件密钥归 owner-bound 凭据存储，安装/卸载沿 Library 预检与发布；模型配置归 `ModelConfiguration`，运行环境归 `RuntimeConfiguration`，渠道由原 `ChannelManagementPort`/supervisor 托管，计划仍归 Scheduler。设置页只管理模型与环境；图编排的「消息触发」配置渠道绑定与启停，运行看板的「消息会话」管理对话实例，凭据复用插件库的同一组件与存储。Web 不保存第二份配置，也不保留顶层渠道页。

HTTP、Session、Pilot 的插件基础目录读取归 `InstalledPluginCatalog`，清单有效性、依赖就绪与上下文授权、连接验证分别表达；目录列表保持轻量，不生成执行绑定。Graph 的发现、bundle 读取与校验复用 Graph 应用用例，适配器保留身份、格式和私有诊断脱敏。Web 的插件设置、鉴权下载和行菜单复用完整交互，草稿、持久策略和历史绑定保持不同归属。具体边界见 [共同事实与实现收敛](shared-facts-convergence.md)。

Host 托管的模型配置将公开字段、用于恢复的旧绑定和按槽/服务索引的密钥分别表示。执行直接读取保存的槽配置，旧托管字段仅用于未保存 default 时的兼容默认与原显式别名；模型服务字段和凭据不读取环境变量，Web 与执行共用唯一私有存储。新 Turn/invocation 通过 Goose 原生接口选择最新模型并延续历史，当前执行及同 invocation 恢复保留冻结绑定。运行环境初始化和插件安装的进度只描述管理操作，不成为 Agent loop 或通用恢复引擎。接口见 [Web 管理契约](rust-frontend-api-contract.md#web-管理入口)。

模型公开字段统一使用 [ModelFields](../rust/anchor-runner-host/src/model_fields.rs)，设置保存与 Goose 执行均通过 `ModelRegistry::from_fields` 校验 URL、凭据、模型名、协议、别名与正数限制；类型化字段不再转回环境变量字符串表解析。上下文窗口和输出上限为 0 时不能保存，保存失败不改写已有配置。

同一事实所有者内部按职责分模块，入口负责组合：

Host 的[依赖方向围栏](../rust/anchor-runner-host/tests/layer_fence.rs)固定组合根（`main.rs`、`bin/`、`execution.rs`）、入口适配（`api`、`pilot_tools`、`node_tools` 模块及 `channel_tools.rs`）、用例（`application` 模块）与其余 Host 服务的角色。服务引用用例或入口、用例引用入口的现有次数由[逐文件上限](../rust/anchor-runner-host/layer-fence.txt)限制，新增引用失败，下降时提示降低上限。它与 Run 围栏共用生产源码扫描器，排除测试、注释和字面量；统计显式 `crate::` 路径（含分组导入、原始标识符与宏输入），不做名称解析、相对路径解析或宏展开。Run 的运行时豁免不改变这里的层次归属；现有反向依赖尚未清零，阶段状态见[收敛任务书](host-convergence-task.md)。

Plugin id 与名称的字符规则统一归 [anchor-runtime::identifier](../rust/anchor-runtime/src/identifier.rs)：名称非空、非 `.`/`..`，仅含 ASCII 字母、数字和 `_.-`；Plugin id 另要求首字节为字母或数字。Library 安装/回收、Graph 编译/catalog 与 Host 入口共用此规则，凭据和配置入口同样先校验 Plugin id。Host 的不跟随符号链接的持久建目录与 invocation 文件名哈希归 [host_fs](../rust/anchor-runner-host/src/host_fs.rs)，各存储保留原前缀和路径；组合根的 `create_durable_directory` 仍保留跟随符号链接的原语义。

Run 身份元数据存储、触发来源、Session 调用数据结构与 Graph 路径身份归顶层 [run_metadata](../rust/anchor-runner-host/src/run_metadata.rs)，不依赖用例层。它使用独立的 `MetadataError`，由 `application` 中唯一的转换实现映射到 `ApplicationError`；持久字段、序列化、路径与保存/读取语义保持原样。接纳、查询、Session 授权和执行协调仍归各自用例或组合根。

| 实现入口 | 内部边界 |
| --- | --- |
| [Graph Host](../rust/anchor-graph-host/src/lib.rs) | bundle、Graph/Plugin catalog、资源校验、MCP 声明、授权摘要和 Graph Call；公共契约仍由 crate 根导出 |
| [RunApplication](../rust/anchor-runner-host/src/application.rs) | 接纳、查询、删除、控制、恢复和执行登记；共用同一个 active registry、Kernel 存储与宿主锁顺序 |
| [Node Host](../rust/anchor-runner-host/src/node_host.rs) | 输入、挂载选择、工具准备、现场事实和节点执行；部署注入 Pilot 身份，执行授权不依赖 HTTP 入口 |
| [Web App](../apps/web/src/App.tsx) | 根组件负责导航；编辑 hook 拥有草稿与撤销，工作台 hook 拥有可丢弃的 Host 投影，Run 详情按页面订阅；Pilot 草稿 hook 只拥有未发送文本和附件 |

Web 工作台的颜色、字体、控件、面板标题和默认栏宽由 [共享样式](../apps/web/src/style.css) 统一定义。[Pilot 页面样式](../apps/web/src/Pilot.css) 独占会话页面布局与断点；插件面板复用同一列表、搜索与详情组件，窄屏使用抽屉，不通过插件局部样式改写聊天网格。Pilot 的 [阅读布局](../apps/web/src/PilotConversationLayout.tsx) 负责两侧拖动、键盘调宽与重置，正文和输入框共用居中宽度；浏览器只保存用户阅读偏好，窗口与插件栏变化限制实际显示宽度，不覆写偏好或产生 Host 写请求，窄屏自动适配。

Graph/Run 的面板显隐由各页的 [Workspace](../apps/web/src/Workspace.tsx) 持有；收起和最大化位于面板标题，展开入口位于当前页操作栏，不设顶部全局开关。选择节点会打开详情；返回保留页面不会重新展开用户已收起的面板。宽度沿用共享阅读偏好，运行详情不生成不存在的左侧面板控件。

Graph Call 的 `wait` 子执行在 Runner 返回成功、错误或合作取消后都注销登记。取消转发任务由该登记持有，退出时在 active 锁外中止并等待回收；正常结束不写入停止请求。Web 只在 Graph/Run 消费页面轮询工作台数据，其他页面拥有各自请求；首次直达直接读取，返回消费页面立即刷新，离开页面后丢弃迟到响应。

Web 的图编排与 Pilot 仅在首次访问后保留页面 DOM 和本地阅读状态；隐藏时通过 React Activity 清理 effects、请求和流订阅，返回后重新读取 Host 事实，不以页面保留替代授权或有效性检查。相同工作台投影保持对象身份，未变的 Markdown 不重复解析；Pilot 同一会话返回不先清空消息，已删除会话仍在列表校验后清理。运行过程只保留可重建的文本、工具投影和已消费 SSE 游标，返回从该游标接续，终态不重复回放，不改变服务端执行。图布局使用至多 30 项的 LRU，缓存命中在首 render 即可读取；拓扑、标签或保存位置变化仍失效，画布保留视口，仅几何或可见尺寸变化时自动适配。ELK 和非首屏页面按需加载，不删除 StrictMode，不修改轮询周期或超时；浏览器刷新仍需重新核验和构建当前页面。

Run 的生命周期唯一来源为 Kernel Phase；Host 静止规则组合相位与 worker 观察，Session 交接继续保留当前轮占有约束。

[RunProjection](../rust/anchor-runner-host/src/application/run_access/projection.rs) 在查询时组合既有持久状态、执行登记和控制前置条件，供 HTTP/Pilot 列表、详情、时间线及子调用共用；Web 只展示该操作投影，最终控制仍核验现场。运行画布从该 Run 的冻结 snapshot 投影只读拓扑，包含展开后的模块，不读取当前 Graph 或编辑草稿，也不暴露执行脚本、输入和 host 参数。投影不新增持久格式或恢复游标。

查询投影只在读取 worker 登记时短暂持有 `KernelRuns.active`，释放后再读取 Session、渠道与部署资格；历史助手先核对 current binding，已被交接或退役的 Run 不再触发当前部署的资源检查。当前实例的资格检查与实际控制校验保留，不以查询优化放宽恢复权限。

Host 的 [Run 访问边界](run-access-facade.md) 对业务内部类型引用实行零上限，新增能力先扩展门面。`application/run_access` 接管相位、只读视图、规则与 HTTP/Pilot/渠道兼容序列化，Graph/会话/助手只消费安全视图或按意图操作；准入、控制、恢复决定、放弃、Session 调用及子 Run 释放的执行与保存协议归门面，生产 `store()`、原始枚举与执行准备/派发仅在门面内部可用。Run 状态只由 Kernel 提交改变；Session 投递、绑定与授权事实仍由各自所有者保存。默认执行、恢复与删除均调用同一内核路径，逐规则与 Session 单写入者审计见 [M3 规则表](../experiments/constitution-host-preflight/m3/rules-session-audit.md)。

## Graph 与节点

作者 JSON 定义 `agents`、`ops`、`graphs`、`nodes`、`edges` 和 `layout`。`GraphSnapshot::from_authoring` 编译为执行 IR，保留作者定义供 Web 编辑；内联模块在执行前展开。bundle loader 绑定清单内资源并校验摘要。资源安装与 Graph 保存都经过 canonical Library 校验。

AgentNode 引用角色与节点级 `plugins`，角色只复用模型、指令和权限。Op 定义在 `run`、`call`、`fanout`、`join`、`host` 中恰好选择一种；Op 不继承 Plugin。`Op.host` 通过 `NodeExecutionPort` 交付通用 operation JSON；Kernel 不依赖 Session 存储或 WeCom 协议，具体能力由 Host 实现和授权，手动/standalone 入口默认没有 Session host operation 授权。反馈回访从同 Run 最近已提交产物延续，新 invocation 保留自己的执行现场；普通跨 Run 不自动继承。

同一 Run 由唯一协调者维护。配对 fanout/join 允许独立串行分支并发，所有成功后收束；分支失败取消同伴。嵌套、分支交叉和区域内 Graph Call 等不支持的组合在接纳前拒绝。独立 Graph Call 使用同一 Runner，`wait` 与 `detach` 的父子身份和生命周期分别持久化。

## 数据布局

Host 分别配置 bundle、catalog、state、workspace 和 Library 根。release 目录只读；可写根放在 release 外。Host 在监听前验证配置并持有 state-root `deployment-writer` 锁。开发脚本使用独立数据根，旧本地记录不会被重新接管。

`ANCHOR_BWRAP`、`ANCHOR_RUNNER_LIBRARY_ROOT`、`ANCHOR_RUNNER_CATALOG_ROOT`、`ANCHOR_RUNNER_STATE_ROOT` 由 `main.rs` 在进程入口各读取一次，形成 `HostConfiguration` 交给组合根。`RunApplication` 在已有 state/catalog 根之外只保存 Library 覆盖与 bwrap；Pilot 和工具装配接收这些已解析参数，执行准备接收入口提供的 reader，启动清理接收惰性的工作区枚举回调。Library 的显式根优先于调用方已解析的 catalog 根；工具环境仍只从显式 Library 根加载，保留原回退语义。[配置围栏](../rust/anchor-runner-host/tests/configuration_fence.rs)限制四个变量名的生产字面量归属；其他环境读取尚未迁移，清单见 [H3 交回](../experiments/host-convergence/h3/environment-inventory.md)。

```text
<release>/anchor-runtime/
├── bin/                   Host、固定 Goose 与显式工具
├── bundle/                独立 Graph 包的 Graph、manifest、Plugin ID 引用
├── plugins/network/       完整平台的原生联网引导 Plugin
├── web/                   编译后的 WebUI
└── runtime-manifest.json  文件 hash、模式与 ELF 依赖身份

<mutable roots>/
├── catalog/               可编辑 Graph bundle 与资源闭包
├── library/               Plugin、工具、授权引用
├── state/                 RunStore、Session/Turn、事件、计划、渠道与锁
└── workspaces/            稳定 Workspace 资源与 Goose 执行目录
```

Artifact 位于 `<state>/artifacts`：format 3 清单保存显式交付路径、内容摘要、大小、可执行位与空目录，字节按内容去重，引用使用 `fs2-` 身份。仅支持新格式；旧 Artifact/Workspace 不导入，本次以新数据根切换。发布完成前生成轻量只读视图，下游通过 `.anchor-artifact.json` 绑定固定版本，不再生成 Git 仓库。历史消费者通过共享 reader 持只读租约核验引用、描述、过期状态和相关文件摘要。单节点沙箱使用 `/workspace`、只读 `/in/<node>`、`/plugins/<id>`、`/tools/<id>`；显式授权的本地输入按 Run 冻结。宿主工具目录由 Library `tool.json` 注册；可选 `plugin` 字段把登记限定到对应 Plugin 的 MCP，不向普通命令和其他 Plugin 暴露。共享 Python 与 Rust 的配置由 Host 统一解析：完整平台可通过设置页或 Pilot 初始化，显式部署字段仍由部署者管理。Pilot 在自己的 Session 工作区处理 Plugin 候选，不因此获得任意宿主路径或环境写权限。第三方工具的运行语言不改变核心职责。

沙箱的 `/dev` 是 bwrap 建的最小 devtmpfs，因此默认没有任何加速器设备节点。需要 GPU 的第三方工具由操作员在同一个 `tool.json` 里登记 `devices`：每一项必须是指向 `/dev` 下真实字符/块设备节点的绝对路径，Host 在装载工具时校验并把它变成该沙箱策略的精确设备授权，节点执行时按同一绝对路径 `--dev-bind`。设备既不随只读挂载根、workspace 或模型参数隐含获得，也不能登记 `/dev` 本身、目录、普通文件或 `/dev` 之外的路径；未登记的请求在创建 namespace 之前就被拒绝。设备授权是部署字段，Graph/Plugin 文本无法扩大它。

部署者可通过 `ANCHOR_NODE_COMMAND_TIMEOUT_SECONDS` 配置 AgentNode 单条 `anchor_run` 命令上限（默认 30 秒），并通过 `ANCHOR_MODEL_MAX_TOKENS` 将单次模型回复的输出预算传给 Goose；后者默认不设置，沿用 Provider 默认。节点整体墙钟控制继续生效。

插件运行依赖声明属于 `plugin.json` 的 `dependencies.python`，正式、候选及兼容旧项目的只读视图引用 Library 的共享包；新项目使用私有开发 venv。Host 使用获授权的来源与固定 uv 准备正式依赖，安装经原生确认和 Library 发布锁。包字节比较、视图 launcher 与跨解释器边界见 [依赖标准](plugin-dependencies.md#当前实现映射)；Graph bundle 只引用 Plugin ID，旧 `environment` 声明具名拒绝。

Run format 13 将准入授权与执行观察分开：`plugin_bindings.authorization_digest` 保存无凭据值的 Host 权限投影；节点结果的 `plugin_digests` 和 `plugin_views` 保存本次实际准备的代码与依赖声明摘要。代码、Skill 路径和依赖变化从后续调用读取；联网、可写路径、宿主命令、凭据来源、远程端点或 OAuth 授权声明变化不能沿用旧 Run 权限。format 12 记录只迁移格式，不填造新观察或权限；旧权限为空时，Host 与 Runner 共用完整旧 ID、代码摘要、资源与 MCP 名称核验，全部通过后由持 Run lease 的 Runner 一次性保存升级。

Host 的 `plugin_library` 统一拥有 Catalog/Library 构造、可取消发布租约、定义与摘要重验和 Graph 挂载声明；列表用例建在其共享读取之上。普通观察不新增锁，共享读取保持非阻塞，发布与开发 scope 保持既有生命周期和锁顺序。`plugin_mounts::resolve` 继续拥有 Session 开关合并规则。

`plugin_library::tools` 提供共用准备与连接/披露挂接步骤，OAuth 复用 `McpToolConfig::authorize_oauth`；Pilot 保持逐项软失败，Node 保持批量硬失败、OAuth 前保留配置、连接前记录观察。

AgentNode 和 Pilot 的正式工具准备在同一短 Library lease 内读取一份 `PluginDefinition`，由它生成绑定、MCP 配置、挂载、依赖视图及只读披露目录；释放后才做 OAuth/MCP 网络启动。目录再与实际工具装配的 Plugin/server/remote-tool 归属关联，不解析工具名前缀猜所属资产。提示词只提供固定的发现入口，不平铺全部 Plugin/Skill 路径，不另存提示词事实。此锁保证准备期间的读取一致性，不是历史代码快照；释放之后的更新仍可替换当前代码或回收旧视图，不能据此承诺旧进程可复现。Plugin 的 `mutable` 授权独立于 Python 依赖：无 Python 包的插件也可使用声明的数据目录；`plugin.json`、`.mcp.json` 与外置 MCP 声明文件必须属于不可变代码。升级逐项替换代码、保留数据；多条目更新中途失败如实报告。依赖规则见 [依赖标准](plugin-dependencies.md)，验证范围见 [验收摘要](pilot-development-plan.md)。

Library 的 `PluginMetadata` 是列表使用的实时目录投影，与携带资源内容摘要和执行绑定的 `PluginDefinition` 分型；两者共用清单、资源路径和配置校验，只有完整定义读取代码字节并计算身份。Web 插件库与会话列表复用目录投影，不创建另一个持久目录或授权事实；目录读取放在阻塞工作线程，列表显示按批次渲染，搜索覆盖全部结果。

渠道状态与部署资格观察同样读取实时 `PluginMetadata`，不为 Graph 列表或 Run 操作投影重新读取整份插件正文。渠道授权摘要及 owner-bound 凭据校验由完整定义与元数据入口共用，声明、账号或资源路径变化仍立即影响资格；配置、启用和启动继续读取完整定义，不缓存或跳过执行校验。

AgentNode 的 Goose 接入区分工具解析已经返回的结果与外层等待被取消/超时：解析错误保留原错误类型并沿既有 Run 错误路径结算，不再次轮询已经完成的 future；只有外层中断才通知仍未结束的解析过程，并等待它回收所拥有的 MCP 子进程。

Library 审查、候选准备与导入共用代码暂存规则：选中 root 或 `.codex-plugin` 清单后只读一次，解析和暂存使用同一份原始字节；枚举在读取类型之前排除 `mutable`，不复制源数据，暂存只提供空数据位置。升级的 durability 同步只遍历代码与必要父目录，不进入数据子树。嵌套数据声明保留完整缺失尾路径，符号链接或非法父路径不能扩大写权限。

引用发布与卸载共用 Library lease。Run 在读取 Plugin 至保存持久引用之间持有此锁，执行前释放；Run 准入持 Graph lease 时不等待 catalog guard。Session 挂载与安装/卸载按 catalog guard → Library lease 排序，并在锁内复核版本和引用。卸载沿用用户批准的前置条件，不在确认后刷新；安装失败的视图清理也在锁内核查，已引用或状态不确定的内容保留。
所有节点默认按 `(Run, node)` 绑定稳定 Workspace，后续 invocation 复用同一目录；新 Run 默认独立。资源现场位于 `<workspace-root>/.resources/<resource>/files`，Kernel 保存资源及绑定授权，磁盘绑定是投影。跨 Run 通过 Host 授权绑定同一资源，渠道交接复用同一用例。执行租约覆盖写入者退出和选择性交付发布；未发布候选保留原现场且不能被其他 invocation 接管。失败、中断和取消保留修改，不生成全目录 checkpoint 或 `/previous` 副本。

`final_result.files` 必填，`[]` 表示纯文字，目录递归选择；节点 `writes` 是必交子集。Op.run 按 `writes` 交付，Call 只交付显式导出文件。Host 只读取选集，未选中的依赖、缓存、项目 Git 和链接不参与归档。内容对象与可写现场不共用可变字节，发布后再由 Kernel 接受结果引用；同 invocation 重试复用已发布交付。Artifact 字节可独立保留或过期，历史清单不永久保留字节；当前执行/恢复输入及显式保留交付受保护，来源 Run 删除不直接删除共享对象。

## Goose 与恢复

Goose 固定为 v1.53.0；Anchor 在上游源码应用[已审查的本地兼容补丁](../scripts/patches/README.md)，再用 lean ACP-only 入口构建静态 x86_64 musl 二进制（recipe 见 [构建脚本](../scripts/build-goose-acp.sh)）。现有固定发行产物的 SHA256 为 `17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640`；源码启动使用本机编译产物，由启动脚本自动提供实际摘要。它与官方完整 CLI 共用 `goose::acp::server::run`。补丁在 Goose 原生 Thinking 的可选元数据中保存完整 reasoning item，并在后续 Responses 请求按原序单次回传，包括 opaque/encrypted 字段；同时忽略空 text delta 保活，非空畸形帧仍报错。Anchor Bridge 和产品事实不复制或重写 reasoning，旧历史已丢失的数据也不重建。

AgentNode 与 Pilot 共用 ACP/MCP 接入。当前执行及同 invocation 恢复严格绑定二进制、模型/endpoint 和原生 Session，不静默换模型或创建替代历史。新 invocation 可选择最新模型；跨 Goose 二进制版本的 Graph 原生历史接续，仅允许[已审查的旧固定摘要到当前摘要](../rust/anchor-runner-host/src/goose_acp/conversation/revision.rs)单向升级，核对前驱与 scope 事实后仍加载同一 Session，不授权逆向回滚或未审查二进制。Pilot 的新 Turn 通过公开 `session/load` 接续保留的 Session。跨不同 Provider、模型或账号的 opaque reasoning 兼容尚未验收。固定 Goose 将请求、网络、拒答及终止的压缩错误保存为原生结构化 Error，并通过 ACP 错误结束；成功的压缩恢复继续原路径。Host 仅依据经协议校验的错误代码与原因区分模型请求失败和运行中断，Pilot 分别结算为 `failed` 与 `interrupted`，用户取消仍为 `stopped`；原始服务正文不进入 Web 错误信息。普通助手正文不参与分类，Turn `completed` 仍不替代业务成功验收。实际 Host/Goose 错误场景及真实 Provider 验收边界见[验证记录](../.local/closeout-20261018/README.md)。

固定版 Goose 的 `enabledExtensions` 在新建时追加到默认扩展，加载时保留已存扩展，不能单独用作授权集合。共享 ACP 接入在新建/加载后、首次模型提示前，通过官方 `session/extensions/list`、`remove`、`add` 接口校准为 Anchor MCP 与部署显式允许的原生扩展；默认只保留提供边界指令的 `tom`，不启用 `developer` 的独立文件和 shell 工具。撤销旧扩展保留原生 Session 与历史，不直接修改其数据库；校准失败拒绝本次执行。

Pilot 与 AgentNode 共用从当前授权和 Library 派生的 Plugin/工具目录，通过有界摘要和固定发现入口按需读取。完整分页、读取、调用和兼容开关见 [Plugin 披露契约](plugins.md#发现与读取)。目录是可重建投影，Goose 原生系统提示和 compaction 承载上下文；MCP 仍按准备流程连接，按需连接尚未实施。

AgentNode 通过授权完成工具提交 `summary` 和可选 `route`；普通回答不能完成节点。Goose 为同一次模型响应的全部工具生成可信批次身份，Host 接受最近授权非完成批次中任一实际返回的回执，不依赖工具完成顺序；发现与只读工具也产生回执。完成工具必须独立调用，旧批次、其他 invocation 与猜测回执均不接受。恢复后需重新核查现场，不能拿旧回执直接完成；诊断要求复制实际返回值，可通过授权只读工具核查，不应重复业务操作。

完成校验通过后冻结候选：相同提交幂等，冲突提交和后续业务调用被拒绝，不撤销候选。仅 AgentNode 启用固定 Goose 的外部完成工具边界：成功后先保存完整原生请求/结果，再结束本轮，不再请求模型。Host 仍须取得正常结束、完成清理并核验取消，之后才保存完成事实并交给 Runner 提交产物；内存候选不等于持久完成。用户取消、断连或原生历史保存失败不转成成功。该边界由 [固定补丁](../scripts/patches/README.md) 实现，Pilot 保持原生文本结束行为。工具效果发生但结果缺失时保存观察，由 Agent 在原生会话中核查 workspace、Artifact 和业务状态；工具不会自动重放，外部副作用不承诺 exactly-once。

Run 的暂停/停止请求与节点已收束是不同事实。恢复保留 Graph 进度、invocation、原生 Session 和中断记录；重启不会自动执行未结算业务或确认。旧执行器记录保留旧身份，可识别和拒绝冒充 Goose 游标；清理源码不迁移旧数据。

Host 对外 trace 投影 ACP 消息、工具观察和媒体，支持增量读取与重开节点。该投影用于观察，不代替 Goose 原生历史。真实模型、vision 与长会话效果必须分别验收。

Host 的 trace 证据与 `write_durable` 文件通过同目录临时文件写入、文件 fsync、原子替换及父目录 fsync 发布；已打开的读取保持完整旧版本，重新打开只见完整已发布版本，不因并发写读到空文件或半份 JSON。该保证针对单文件，不将 invocation fact 与 trace 证据包装成跨文件事务；字段格式、发布顺序和恢复校验不变。[调用点与回归](runtime-contract-tests.md#trace-证据与宿主持久文件的原子发布)。

## Session、渠道与入口

Pilot Session 保存业务关联、Turn、问题/回答和 UI 事件；Goose 保存模型历史。Turn 提交按 `request_id` 幂等，SSE 按事件游标回放。原生 MCP elicitation 经过 ACP；必要删除确认绑定确切 Graph 与资源前置条件，重启中断未完成的问题，不重放旧确认。

Pilot 与渠道附件复用 Host 的字节校验、限额、hash 和不可变保存原语，各自保留入口身份与归属。Pilot 原件位于已登记的 Session 资源 scope，Turn 只保存输入身份和文件引用；输入只读挂载 `/in/pilot`，图片通过原生 ACP image 内容交给 Goose。接续/恢复继承确切来源引用；新用户请求不自动获得历史附件。`publish_file` 固定工作区文件的交付版本，原生工具图片复用同一发布路径；输出事件驱动实时界面与历史，鉴权下载核查 Session/Turn/原件身份，删除沿既有 Session 清理。API 与限额见 [API 契约](rust-frontend-api-contract.md#sessionlibrary-与渠道)。

`GET /sessions/{id}/messages` 按既有 Turn 和 UI 事件顺序投影用户输入、回复、提问与回答；正文保留 `role`/`text`，提问与回答分别附加 `question`/`question_answer` 的归属及保存状态。回答显示字段标题和实际填写的值，拒绝与取消保持明确的用户决定，中断提问没有虚构回答。Web 将这些记录直接显示在对应的对话位置；运行中的问题仍由当前 Turn 显示一个交互表单，完成后从消息投影读取，不另设历史提问区、不重复保存消息或重放回答。

完整平台 Pilot 使用内置管理指令，不需要业务 Graph，空 catalog 可以启动和就绪。独立 Graph 宿主及显式配置的旧助手 Graph 仍保留相应准入检查；只有配置了助手 Graph 时，才检查其指令 Plugin 的自修改保护。安装和卸载的原生确认、摘要与引用前置条件不因没有业务 Graph 而改变。

Pilot 的 Graph validate/create/update 工具共用作者格式说明与最小合法示例，空 catalog 不依赖已有 Graph 猜测格式。校验直接复用 Kernel 的 `GraphSnapshot::from_authoring`；输入格式错误返回具体位置和原因，Host 能力及私有 Plugin catalog 的错误保持安全泛化。说明和示例不定义另一套编译规则，也不授予执行权限。

Pilot 的任务联网由 `network` Plugin 提供启用入口。Host 在网页与 Responses API 新建 Pilot 会话时默认保存主动挂载 `network`；已有会话不补默认，关闭设置在续聊和重启后保留，渠道和业务 Graph 会话不增加此默认。插件的 `network:true` 或远程 MCP 声明产生联网需求，Host 从当前挂载集合推导依赖启用；Session 的 `network_allowed` 是用户授权上限，依赖不能覆盖明确禁止，带 Session 的 AgentNode 仍受节点网络权限上限约束。授权与挂载共同冻结到 Turn 快照，stdio 沙箱、HTTP MCP 与 OAuth 使用同一结果；离线工具保留无网运行。Provider 的控制通道连接独立。具体状态、旧快照兼容、独立 Graph 边界和候选包契约见 [任务联网能力](network-plugin.md)。

Pilot 的普通 Turn 与 Responses 共用接纳、Session 工作区及现有文件/命令工具。Host 的 `RuntimeConfiguration` 统一解析基础环境，Session 持有私有开发现场，Library 持有候选引用和正式 Plugin；显式部署字段仍由部署者管理。准备、命令、测试、正式交付及清理的具体行为见 [Pilot 开发契约](pilot-development.md)。

Pilot 接纳保存网络和工具授权快照，执行启动时读取一次 `RuntimeSnapshot` 并冻结基础解释器和依赖准备器，包括“尚未配置”的状态；当前执行不会因其他请求更新配置而获得新能力。运行时探测和 MCP 启动不持全局接纳 gate。对话内联网授权、挂载、环境准备或凭据配置保存事实后请求幂等 continuation，当前 Turn 收束后通过同一 `PilotService::admit` 接纳下一 Turn，Web 跟随新 Turn。停止、删除、归属变化或未收束执行不能自动接续；重新接纳核查已保存请求，不重放准备或业务工具。Graph 后续节点从 Host 当前配置及当前 Library 装配，保持既有热替换语义。

网页挂载在 Pilot gate → catalog guard → Library lease 顺序内复核 Session、保存挂载并登记接续。API 保留 `plugin`/`plugins`，增加 `effect`（`next_turn`/`after_current_turn`/`unchanged`，活动源附 `source_turn`）及既有 continuation 投影。仅当前登记执行且仍为最新 Running Pilot Turn 的真实变化请求 `continuation:<source>`；多次切换合并为一次，当前 Turn 保持旧冻结快照，空闲切换不调用模型。仅正常结算后自动接纳；失败、中断、停止、归档或新用户 Turn 取代来源都不唤醒旧任务。自动接续沿原生历史只处理最新用户请求中未完成且已授权的部分，能力变化不构成新的业务任务，也不要求重做已完成结果。Web 跟随宿主来源与后继，重开时处理列表/详情之间已接纳的交接，等待接续仍可停止或发送新消息。

网页网络许可按钮只保存 `network_allowed`，影响后续 Turn；该 API 不返回挂载 `effect`，也不登记自动接续。对话内工具发起的联网授权仍按其既有 continuation 路径处理。

停止与结算/接纳共用 Pilot gate：执行已返回成功但尚未结算时，取消仍结算为 Stopped；来源已经 Completed 时保留该历史，通过幂等 `pilot.continuation.cancelled` 事件取消该来源的后续推进，即使尚无 requested 事件也适用。取消在重启后仍有效；OAuth 回调在交换 token 前和持 gate 发布授权前都核查该事实，迟到回调不能交换/保存 token 或恢复任务。该事件是 Session 控制事实，不是新审批流程或 Agent 恢复日志。

允许联网的沙箱通过独立网络 namespace 与 `anchor-sandbox-network`/slirp4netns 出网，禁用 host-loopback；失败不会退回共享宿主网络。relay 和网络进程在取消、父进程退出及启动失败时收束。Host 的管理 API 没有登录、仅允许 loopback 监听，非 loopback 直接拒绝启动；模型控制通道与任务授权分开。

`credential_request` 只发起 owner/Session/Turn、已安装 Plugin、server 和声明字段绑定的专用输入。Web 的 password 控件直接提交 Host，值不进入聊天、工具结果或 Goose 历史；Host 私有 600/700 文件保存凭据，下一 Turn 向该 owner 的正式 MCP 配置注入。取消、删除、过期、版本或授权变化拒绝旧输入，返回 `no-store`/`no-referrer`。`oauth_authorize`/`oauth_status` 复用 Library OAuth transaction 和原 token 流程；Pilot 来源另持关联事实，停止或删除后迟到 callback 不交换 token，也不降级成无来源旧流程。

Pilot 接纳在 Session gate 内保存 Turn、冻结挂载并登记取消；环境探测、OAuth、MCP 启动与 Goose 执行在 gate 外进行。结束前显式关闭持有的 MCP 端口。Goose Bridge 先等待实际工具收束，再通过 Axum graceful shutdown 关闭 HTTP 连接、调用固定 rmcp 的公开 Session 关闭接口并等待 handler 释放引用；abort 仅用于失败兜底，关闭失败保留不确定结果。节点启动取消先通知资源所有者再等待清理，不从外层直接丢掉正在启动 MCP 的 future。

Pilot 清理保存 format 1 的不可变归属绑定 `state/platform/pilot-resources/<scope-hash>.json`，包含 owner、Session、创建时间、workspace 与 Goose 根。接纳和删除都在 Session gate 内重新核对当前行，防旧请求删除或接入同 ID 重建的 Session；删除持 Goose 原生 execution lease，仅清理对应 workspace、原生历史、候选准备与 mount 快照，成功后再删 Session 行，部分失败保留归属以供重试，已安装 Plugin 不随 Session 删除。mount 快照保持旧路径编码，按真实 Turn 集合与 format 1/2 JSON 中精确的 Session/Turn 身份逐文件回收，路径及读取不跟随符号链接；其他 Session、未知文件和归属不符的快照保留，只有空目录可移除。Graph 会话另用 `state/conversation-resources/<scope-hint>.json` 保留 Graph/Session/node 的共享 scope 归属；幸存 Run 或 metadata-only handoff 保留引用，最后引用释放时在原生 lease 内清理。Run lineage 墓碑仍只解释缺失前驱，不作为 Goose 恢复游标。

普通助手由 Graph 执行。可信入口绑定来源、Session、Graph 与结果节点；同 Session 串行，私聊按成员隔离，群聊按群共享。渠道只接纳常驻助手形状，每条消息一个 Run 的旧形态与 `/conversation-runs` 已移除；逐节点会话 scope 不跨会话、Graph 或节点复用。

### 常驻助手实例

已接入的常驻路径是普通三节点循环：`wait_input (Op.host session.wait_input) → assistant (Agent) → reply (Op.host session.reply) → wait_input`。示例 [wecom-persistent-assistant.json](../examples/graphs/wecom-persistent-assistant.json) 须显式选择。Host 当前只接纳这一输入驱动、无有限轮次上限的助手形状，不建设 WeCom 专用 Runner。

- 首条可信消息惰性创建该 Session 的 current assistant Run，后续输入复用它；Session 保存 Turn、幂等、历史 binding 和 retire 事实，RunStore/Runner 拥有冻结 Graph、cursor 与执行状态，Host 工作区适配层保存 Run 的稳定节点 workspace 绑定。
- `wait_input` 从持久输入事实领取 Turn，按等待 invocation 幂等绑定，重试不取下一条；将本轮文本、可信身份与附件引用提交给 Agent。后续消息不覆写 `Run.input`，等待期间不调用模型。
- WeCom dispatch 拒绝或等待失败时，Session 事务只将仍 Running、尚无 Run 关联且未被新输入取代的入站 Turn 结算为 Failed；已领取 Turn 的执行与投递沿原有事实收束。助手已停止返回具名 `409 assistant_stopped`，网关提示在运行看板的消息会话中恢复后再发消息，不把拒绝留下为永久 Running 或自动重放。
- 新输入打断旧轮而非停止整个 Run。Host 取消并等待 Goose 与工具执行者真正退出，保留未提交现场和持久 `Yielded` 事实；Runner 冻结 `Interruption` 控制 Artifact 后沿普通 route 到 `reply`，旧回复记 `suppressed`，再回到等待节点。它不把旧 Agent/Turn 标成成功，不自动重放未知业务效果。
- `reply` 读取本 Turn 绑定的确切 Agent commit；出站图片按 Turn 登记，不取长期 Run 的“最新回复”，不串到下一轮。Turn 结算不等待 Run `Completed`，平台发送 claim、ACK 与 unknown 仍复用既有账本。

Session SQLite 仅支持 v9：新文件以 IMMEDIATE 事务创建并在取得写锁后复核，已存在的库须与当前 schema（含账户别名表）完全一致。旧版本、未知身份或被改动的 schema 在写入和启用 WAL 前报错退出，不自动升级或修复，原数据库保持不变。渠道会话键为 `(owner, source, account, conversation_kind, conversation_id, participant_id)`：私聊 participant 为真实 sender，群聊为空；每条输入仍保存真实 sender。助手继续按 Run 保留历史 binding、每 Session 一个 current 及 `(run, wait invocation key) → Turn` 领取事实；普通 `associate_channel_run` 不因此获得一 Run 多 Turn 授权。

Run 详情和 Web 节点投影将 `RunResult.interruption` 显示为“已中断”，不标作成功提交或普通执行失败。新版接管前驱已收束的同一工作区资源，不把中断控制 Artifact 当作业务文件提交。

运行看板按事实分段投影。普通 Run 仍画自身 `started`..`updated`（仍在执行时延伸到当前时刻）的一段；常驻实例的 Run 跨多个 Turn，因此 `/timeline` 另发 `activity`：`turns JOIN turn_runs` 的执行窗口（`created_at`/`updated_at`，合并相邻窗口，按本页过滤；只有 Turn 仍在跑且宿主仍持有该 Run 时才标记未结束），空闲等待不画成执行。常驻 Run 即使没有窗口也带空 `activity`，前端只在 `started` 处画标记，绝不把它拉到当前时刻；`started` 早于本页但本页有窗口的常驻 Run 仍会收录，Session 库不可读时降级为不带窗口。该投影只读、不带 prompt 或投递内容，不新增 schema，也不改变 `/runs` 列表字段。

常驻媒体授权来自可信 Turn 及固定 invocation binding：当前附件只读挂载 `/in/channel`；在本轮输入之前、最近一次 confirmed 投递之后的中断输入中，冻结最多最近 8 条快照，分别只读挂载 `/in/channel-pending/<turn>`。这是有界补充范围，不是全历史授权；后续已越过 confirmed 边界的轮次不再自动挂载这些附件。

首次 wait binding 查询最近 9 条以检测截断，实际只授权最近 8 条并保存 `pending_truncated`；当 `output.interrupted_messages_truncated=true`，Agent 必须如实说明更早中断输入未自动纳入/未完整阅读，不能声称信息全保留。该标记无需额外数据库 schema 迁移，旧 binding facts 缺省为 `false`。

停止/恢复保留 current binding；停止请求须等到执行者退出和真实 `Stopped`，本进程内的新消息或连接恢复不自动恢复已停止实例。Host 重启时空闲在输入等待的实例在原 Run 上继续，其余按 [助手恢复契约](wecom-assistant.md#停止恢复与退役) 自动交接，均不重放未知效果。`GET /channel-sessions/{session}/assistant` 读取 current 实例；`POST /channel-sessions/{session}/assistant/retire` 接收 `{ "run_id": "…" }`，仅在 Run 已真实 `Stopped` 或终态且非 active、Session 无 Running Turn 和 pending/sending/unknown delivery 时允许退役。retire 保留历史但解除 current，禁止该 Run resume/重新绑定；下一条可信消息才惰性创建新实例。Graph 结构更新不热改旧 Run 快照，合法新版经退役后的新实例接纳；仅助手工作节点的插件选择及对应 Agent 的 network 可经操作员保存，在下一 Turn 应用。仍有 current 助手时，结构变化或 Agent 自行修改其在线权限均拒绝保存。

Graph cascade 删除先保护 active/未收束执行、共享原生历史与未结算投递，再退役目标助手；清理只按 owned Run/Turn/invocation binding 删除输入、回复、yield 和 Run 自有工作区，并释放 Artifact 引用。它不是全 workspace-root 扫除或生产旧数据迁移，部分删除重试保留可核查的所有权事实。

会话 Run 也可单独删除，但按血缘设前置条件：不能是 current assistant binding（须先 retire），不能有未结算渠道投递，且每个更新的 Run 都必须已经不可能再执行（终态，或已有自己的后继因而无法 resume/recover）或已完成会读取它的首轮 invocation（绑定共享现场与 Goose 原生前驱身份都发生在那一次执行里）。删除哪个环节不限顺序：删除前先落不可变墓碑事实 `state/run-deletions/<run>.json`（Run、Graph bundle 身份、Session、回复节点，以及被删 Run 自己当时的上一个 Run），lineage 遍历遇到缺失前驱时用同一会话的墓碑继续往后走，幸存历史因此仍是一条链、只有一个头，没有墓碑的缺失前驱仍然 fail-closed。若每个幸存环节都已是某个已删除环节的前驱，则该 Session 没有活动链，下一条被接纳的轮次重新开链。墓碑不随 Run 删除移除，也不参与 current 或恢复判定；单 Run 删除不隐式 retire 当前助手实例。共享的 Goose/legacy scope 在不再被幸存 Run 或交接引用时按独立归属事实清理，删除全部 Run 后同 Session 的下一次接纳建立新链。

### 常驻助手的每轮插件权限

Graph 是插件选择的唯一可编辑来源。Host 在 `wait_input` 领取消息后、提交输入产物前，按 Graph lease → Library publication lease 顺序冻结本轮声明、Session 开关、联网结果与无密钥的插件授权摘要；`state/assistant-plugin-turns/` 保存不可变执行事实。操作员保存 Graph 的授权凭据位于 `state/graph-plugin-authorizations/`，按 bundle 身份与 Graph digest 绑定，Agent 保存不生成它。损坏、缺失授权、结构不兼容或实际插件授权变化均明确拒绝，不回退到旧权限。

Kernel 的 `NodeExecutionPort::invocation_plugin_authority` 默认关闭，普通 Graph 继续校验 Run 准入基线；Host 仅为可信常驻助手启用，由同一 Turn 快照约束挂载、MCP、沙箱和网络。当前 Turn 重试不重读 Graph；启动交接只沿该 Turn 的持久化 `runs` 关联验证前驱并继承权限，新 Turn 直接读取当前配置，不沿 `conversation.previous_run` 跨历史轮次追溯。启动清理仅保留已验证 current 助手中已认领、未被新消息替代的 Turn，Pilot 与孤立执行继续按原规则中断。插件卸载保护仍可执行的 Turn 引用，Run/Graph 删除清理各自拥有的快照与授权事实。

正常插件切换保留 Run、Goose 原生会话和稳定 workspace。重启需要新 Run 接续时，Host 核对同一会话的交接链，并仅对插件/联网差异登记精确 workspace 来源与目标；连续重启产生的未准备实例不能成为空现场来源，普通跨 Graph 复制仍被拒绝。显式退役后可以接纳节点改名的合法新版；每个交接 binding 先核对所属实例的元数据，再判断版本兼容，不要求旧 wait/reply 名称等于新版本。结构或指令变化不继承旧实例的未提交现场，仍可按既有会话规则从已提交产物初始化；连续未准备实例的回溯也止于该版本边界，身份事实不一致仍报错。

### 渠道适配与监听

`call.session` 继承的是绑定会话与显式输入映射，持有稳定来源身份。未知发送效果由 Agent 核查，ACK 与效果事实分别保存。一张常驻助手 Graph 对应一个机器人（[设计卡 004](design-cards/004-multi-bot-channels.md)）：每个启用的绑定由 Host 监管一个原生 WeCom Gateway，状态目录 `channels/graphs/<Graph 摘要>/` 与回调路由 `/channels/<graph>/…` 按 Graph 分开，回调密钥只能访问本 Graph 的路由。Gateway 接受私聊与群内 @ 回调，群按 chatid 共用进度与最新输入判断，允许名单和回执仍核对真实 actor。私聊 image/file/voice 及群 mixed 中的媒体由网关下载短时 URL、做 AES-256-CBC 解密，再交给 Host 冻结 SHA-256/大小/MIME 并只读挂载 `/in/channel`；临时 URL 与 AES key 不进入 Host 或 ledger。出站图片使用 Host 校验的 `channel-replies` 项，由网关三阶段上传换 `media_id` 后按原回调回复，发送前 durable claim、未确认结果不重发。不新增主动群发；任意文件出站、video 与真实公网投递/文档理解仍按具体场景验收。

Gateway 常驻连接与 Graph Run 的执行状态分别投影。Supervisor 及其子进程 IO 固定归 Host 启动时的执行器所有，Pilot Turn 的临时执行器退出不会取消监听；进程退出沿既有监督循环重启，网络断线由 Gateway 重连。`GET /graphs`、渠道 API 和 Pilot 使用同一状态观察：优先读取鉴权的私有 NDJSON `status`，进程已启动而控制接口尚未发布时显示 `connecting`，退出后的重试显示 `reconnecting`；守护任务已结束或已发布的控制接口失联显示 `unavailable`，不会冒充在线或返回控制凭据。已认证、认证中、停止仍分别投影。`GET /graphs` 只为有效启用的机器人返回 `listener`。`active_nodes` 来自仍有宿主执行者的 Running Run 游标及正在执行的并行分支，不把遗留游标当成当前工作。

Web 编辑画布的监听入口和工作流列表在已认证且空闲时使用 1px 虚线，并标注“常驻监听”；实际执行节点及活动工作流使用 3px 实线，空闲后恢复。重连或状态不可用显示对应文字与普通细实线。运行详情继续展示所选 Run 的历史事实；网关连接本身不创建 Run，惰性创建的常驻 Run 在 `wait_input` 时可以仍为 Running，但等待节点不计入 `active_nodes`，也不触发模型调用。

Pilot 的 `channel_configure`/`channel_enable` 经原生确认管理 owner-bound 的按 Graph 绑定，专用凭据输入复用上述机制；Web 在节点 Plugin 详情中管理同一绑定。同一配置供 Supervisor、WeCom 入站和 Graph listener 使用，凭据只来自凭据库，不读部署环境。`enabled` 是保存的启用意图；守护任务缺失、已结束、资源可用但监听失联或账号身份变化时，`restart_required=true`。重复启用修复失效的监听并返回 `changed=true`，健康、启动中和重连中保持原连接；同一绑定的修复不轮换 callback key。一个账号同时只能服务一张启用的 Graph；机器人保存为启用期间，不能改成员和凭据、删除 Graph 或卸下渠道 Plugin。损坏的绑定文件只让它自己的 Graph 停用，不阻止 Host 启动。空 users 默认拒绝所有用户，users 非空时 send_users 必须显式声明。停用立即撤销 callback key，从停用转为启用时轮换；回调授权仅限事件、单条进度查询和结算，不授予管理 API、任务联网或 Plugin 挂载。渠道控制能力已接入，真实企微收发仍须独立验收。

手动触发、计划、Webhook 与 Responses 子集共用接纳。Pilot 的 schedule 创建、列出、修改和删除复用同一 ScheduleStore 与 Dispatcher，按 owner 校验 Graph 和触发规则，不另建调度器。手动和定时 Run 使用相同的安装身份到凭据作用域转换。普通任务显式挂载渠道 Plugin 后可用 `wecom_send_message(bot_graph, userid, content)` 选择已有机器人：Host 每次核对同 owner、同 Plugin、当前有效启用和接收人授权，复用原网关与原生调用身份；常驻助手仍固定自己的机器人，图片归原 Turn。不创建第二个网关、不新增计划/Run 持久字段，不向 Session 注入任务。具体授权见 [渠道契约](wecom-assistant.md#业务工具与附件边界)。Responses 只实现文档中的子集；健康 `/health` 与就绪 `/ready` 不发送模型请求。Graph/工具授权由 Host 强制执行，Plugin 说明和提示词不授予权限。

WebUI 不需要登录；JSON 请求与 Pilot SSE 各自读取本机 Host，取消即中止请求，迟到结果不保存。

工作台按页面请求投影：运行看板并发读取 `/graphs` 与 `/timeline`，复用时间线中的本页 Run，不再额外读取 `/runs`；图编排与运行详情继续读取完整运行列表。各投影返回即可显示，时间线不可用时保留原有运行列表读取与错误反馈。定时刷新在上一轮请求尚未结束时跳过本次 tick，不取消并重发首屏请求；请求失败取消同批其他读取，释放在途标记后下一轮仍可重试。手动刷新、导航及删除继续使旧投影失效，旧请求迟到结束不能释放新请求的在途标记。轮询周期与请求超时保持原值。

平台安装启动器从自身位置解析只读 release、固定 Goose/relay 与 `data/` 私有根，清除调用进程配置，不读取环境文件。systemd 调用同一启动器；预检 `--launcher` 复用其命令配置，并通过安装器同一 validator 检查 format-2 平台。开发脚本和 Vite 也不加载 `.env`；源码启动先探测系统工具与 namespace，按 lock 准备前端依赖，增量构建 debug Host/relay，并更新缺少或变化的 Web 资产；Goose 通过既有固定版本/补丁配方构建到 `.local/bin/goose`（含固定工具链的 musl target 准备），输入未变且本机校验和相符则复用缓存，否则重新准备。源码启动自动把实际产物摘要传给 Host，不以跨机器固定摘要作为启动条件。默认平台源码启动按 [上手清单](../examples/starter.json) 首次安装官方 Plugin 及本地 Rust 工具，经现有 Library 安装器发布，再经 Host Graph API 导入示例；首次导入标记归开发启动器，用户 Graph 和 Plugin 事实仍归 catalog/Library，已有同名内容与后续修改/删除不覆盖。不会自动运行 Graph 或启用渠道。显式 entry bundle、自定义 Host 和外部服务不导入；完整平台发行清单不受影响。`setup` 准备插件与依赖，Graph 在 `start/restart` 接入 Host 后导入；`status`/`stop` 不构建。默认不自动使用旧 release 产物，显式 Host/relay 路径需已可执行；Host 仍独立核对启动器提供的摘要，既有发行校验和同 invocation 的执行器身份绑定保持原契约。旧文件保留原样；显式进程覆盖仍是开发/独立 Graph 宿主的部署接口，不是模型、Plugin 或渠道的产品配置源。

模型设置由 WebUI 经 Host 管理接口写入同一个私有 `state/managed/model-config.json`，执行只读该事实来源。`ANCHOR_MODEL_URL/API_KEY/NAME/WIRE_API/ALIASES` 已退出平台配置，旧安装 `config/model.env` 不再加载；不自动迁移或覆盖已有凭据。Goose 路径、网络和运行预算仍为部署控制。固定 `default/backup × flash/pro`，两槽凭据独立。Pilot 和未指定模型的节点默认使用 `models.default.flash`；其余三种引用由 Graph 明确选择，不自动降级。每个新 Turn/invocation 直接读取最新槽设置，当前执行持有独立快照；已有原生 Session 加载后通过 `session/set_config_option` 选定本轮模型。Chat/Responses 由固定 Goose 原生 Provider 按显式协议生成和解析，Host Bridge 只转发到本轮冻结的服务。

旧托管 `current/history` 保留兼容默认和显式别名，已保存槽缺字段或密钥时失败，不退回旧配置。更换服务保留槽的公共历史与独立旧服务凭据，供同 invocation 恢复核对原绑定；历史凭据仍可通过现有接口管理。命名槽绑定包含槽身份，避免与旧执行或另一账户混用。存储读取兼容 format 1/2，写入 format 3（增加 `providers.history`），复用原文件锁与原子发布；读取不迁移，回滚旧版本前需使用兼容备份。管理接口见 [API 契约](rust-frontend-api-contract.md#web-管理入口)。

## 发行与验收边界

source-free builder 校验固定 Goose、目标 ELF、Graph 闭包、资源摘要与非密钥内容。它支持编译 Web 资产，拒绝脚本/source map、符号/硬链接和未声明资源；打包不会安装依赖或转换用户状态。使用命令见 [发行 builder](../rust/anchor-distribution/README.md)。

完整平台发行使用 format 2、kind `platform`，没有占位业务 Graph，包含 Host、固定 Goose、relay、网络 sidecar、Web、原生 network Plugin、uv/rustup-init 和 launcher；独立 Graph 格式仍为 format 1。安装器验证摘要和闭包，从私有快照解包，原子发布且拒绝覆盖现有安装。模型输入作为有限字段的数据解析，不执行 shell；launcher 清空继承环境并使用独立 HOME 和数据根，不借开发机解释器或缓存。当前平台要求 Linux x86_64、Bubblewrap、slirp4netns、git/sh/cc、CA 及对应动态库；Python、Rust 和包在安装后按需准备。脚本入口见 [平台交付](../rust/anchor-distribution/README.md#fresh-platform-installation)，验收与部署状态分别以台账为准。

常规 [小型 Graph 回归](runtime-contract-tests.md) 执行实际 Host、Runner、Goose、授权 MCP 与 Sandbox，只替换模型传输。检查最终结果、逐节点历史、workspace、Artifact、恢复与幂等；[候选回归](rust-production-candidate.md) 使用 release Host/Web/Goose 的隔离发行包。

常驻循环、实际 WeCom transport 和外部业务分别验收，具体范围与证据见 [企微助手验收边界](wecom-assistant.md#docmost-和验收) 与 [验收摘要](pilot-development-plan.md)。

候选代码和本地证据不等于生产已切换。外部业务、目标发行版、升级回滚与配置兼容的未关闭范围统一见 [验收摘要](pilot-development-plan.md#保留的验收边界)。早期实现和设计快照见 [历史归档](archive/README.md)。
