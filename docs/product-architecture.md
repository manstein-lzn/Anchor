# 产品与系统架构

本文记录产品方向、职责边界和执行不变量。它不作为实现清单；现状见 [当前架构](architecture.md)，完成状态与证据见 [开发台账](pilot-development-plan.md)。

## 产品方向

Anchor 是本地优先、可观察、可恢复的 Agent 工作系统。用户通过 Graph 表达任务拆分、检查、返工和交付，通过 Plugin 积累可复用能力，通过 Run 检查执行证据，通过 Session 持续协作。

核心对象是 Graph、AgentNode、OpNode、Plugin、Run 和 Session。研究、周报、企业助手与递归改进由普通 Graph/Plugin 表达，不在通用 Runtime 里创建专属业务阶段。

后端、共享 Runtime 和官方工具使用 Rust；WebUI 保持 React/TypeScript。AgentNode 与 Pilot 统一使用固定 Goose，通过 ACP 交互、MCP 暴露授权工具。Goose 拥有 Agent loop、Provider、原生历史与 compaction；Anchor 拥有产品事实与宿主权限，不建设第二套通用 Agent 框架。预算控制暂不作为交付门槛，优先完成任务。

长期方向是多用户工作中枢：用户可以直接提出任务，由 Copilot 选择或临时构建 Graph，保留来源和访问范围，并把结果返回原用户。系统 Pilot Graph、临时 Graph 的可见性/保留期限、主动工作策略和自动发布授权尚未冻结；不能把方向当作已经授权在线自改或跨用户读取。

## 不可动摇的设计原则

- Graph 是用户资产，定义与一次 Run 的记录分开；编辑 Graph 不静默改变已开始的 Run。
- Agent 负责理解、取舍和综合；代码负责身份、schema、权限、路由、持久化和机械门禁。
- 节点通过不可变产物和只读输入交接，反馈回访保留可追溯的谱系。
- Run 有唯一协调者；只有 Graph 显式声明的合法区域可以并发。
- 恢复先保存事实、核查现场，再继续；未知结果不伪装成可安全重放。
- UI、摘要和模型上下文都是事实投影，不能成为运行状态或权限的权威。
- 官方和独立交付共用一个 Kernel、Runner、Node 契约和 Run 格式。

## 架构指导：新能力放在哪里

依赖从交互边缘指向稳定执行契约。核心执行不反向依赖 HTTP、聊天平台、研究领域或 Web 页面。

| 变化内容 | 归属 | 事实所有者 |
| --- | --- | --- |
| HTTP、CLI、企业微信请求、身份和协议 | 入口适配器 | 可信来源/请求 |
| Run 接纳/控制、Session 协调、入口共用用例 | Host 应用服务 | 产品执行身份 |
| Graph 编译、路由、反馈、并行调度 | Kernel / Runner | Graph IR / Run cursor |
| Agent/Op 执行、结构化结果、ACP/MCP、通用 host operation 端口 | Node Runtime；具体 host 能力由宿主适配 | invocation 与工具观察 |
| 文件、网络、进程、凭证和隔离 | Sandbox / 宿主授权 | 获准能力与冻结输入 |
| Skill、资源、业务工具和外部系统 | Plugin / Library | 可复用能力资产 |
| 研究、周报、审查目标与内容验收 | 普通 Graph / Plugin | 业务证据与结论 |
| 画布、时间线、会话与文件展示 | WebUI | 对既有事实的投影 |

采用 Ports and Adapters 隔离外部入口；遵循 Clean Architecture 依赖方向；用限界上下文明确 Graph/Run、Session/Turn、Library/Plugin、Node Runtime/Sandbox 的事实和不变量。只采用有用的边界，不引入完整 DDD 仪式、微服务、事件溯源、CQRS 或通用依赖注入框架。

新设计依次确认：已授权用户结果或已观察失败；现有框架/契约能否复用；唯一事实所有者；失败、取消和恢复语义；可执行验收。小型可逆选择直接推进。改变用户结果、持久化迁移、权限、并发或恢复语义的重要决定需明确记录；文档用于后续开发，不是额外审批平台。

## 对象与所有权

| 对象 | 内容与生命周期 |
| --- | --- |
| Plugin | 独立清单、Skill、资源和工具入口；多个节点可以引用同一资产 |
| AgentNode | Graph 内角色引用、任务、Plugin 与权限；模型在节点 workspace 执行 |
| OpNode | Graph 内确定性命令、独立调用、配对并行控制或获授权 host operation |
| Graph | 可编辑 JSON、节点、边、接口、目标与 layout |
| Run | 冻结定义、启动输入/资源授权、cursor、节点工作区绑定、逐 invocation Artifact、路由和工具轨迹 |
| Session | 长期对话、可信来源、Turn、幂等、问题/回答和关联 Run；常驻助手 current/历史 binding 与 retire 事实 |
| Turn | 一次幂等输入或继续请求，保留事件游标、取消与投递身份 |
| Pilot | 管理和协作入口；系统级 Graph 与保护策略为后续需求 |

共享 `agents` 只是角色配置复用，不是独立 Agent 注册库。Plugin 直接挂在 AgentNode，不通过角色继承、默认合并或复制；OpNode 不隐式取得 Plugin 能力。

## Graph、工作区与产物

作者模型与 Runtime IR 分层：用户继续使用 Graph JSON，编译层负责模块展开、拓扑/接口/读写校验和资源绑定。IR 的限制必须在接纳前明确反馈。未知展示字段不能误当执行权限；支持边界由 [作者契约](rust-graph-authoring-contract.md) 和测试固定。

Graph 默认目标和单次 Run 输入分别保存。手动、计划、Webhook、Responses 与渠道共用接纳，Run 保留触发来源；一次输入不能修改 Graph 默认值。忙碌、停机错过与控制请求的状态必须可解释，计划不另建第二套调度器。

每个节点拥有 workspace。完成后提交不可变 Artifact，输入关联上游节点与具体提交。反馈回访从同 Run 同节点最近已提交产物延续；已开始 invocation 保留现场；跨 Run 不自动继承。只读 Git 输入视图服务审查和 commit 绑定，不是第二套可写历史。

当前常驻助手显式复用同 Run、同节点的稳定可写 workspace，不共用全 Graph 可写目录；逐轮 invocation 与 fs2 Artifact 仍不可变，可写现场数量与历史快照数量是不同指标。新实例的首个 Agent 可从可信、获授权的 previous 提交或中断快照初始化文件一次，而不是每轮复制 `/previous`。普通一次性 Graph 保持既有默认。中断先保存现场 checkpoint，再提交 `Yielded`/`Interruption` 控制证据并沿普通 route 接续，未完成现场不能冒充成功文件或回复。

AgentNode 必须通过原生授权完成工具提交 `summary` 和可选 `route`。普通回答或看似正确的 JSON 不代表完成；缺少必需信息、非法路由或未完成现场核查，应反馈给同一 Agent 修正。机械门禁不能被模型判断替代。

### 组合、调用与并行

内联 Graph 模块属于同一个 Run；独立 `Op.call` 产生父子关联 Run。`wait` 等结果，`detach` 保留独立生命周期；目标只读取显式映射输入和选定产物。定义关系由 Graph 派生，调用关系保存为实际执行事实，不增加订阅平台。

`call.session` 交接到操作员选择并绑定用户的会话。用户新消息优先，系统调用保留来源身份；等待、让位、后台接续和恢复必须复用同一 Runner/Session 协调。投递使用已授权 Plugin 网关，不另建消息执行引擎。

配对 fanout/join 允许同一 Run 中独立串行分支并发，全部成功后收束，失败取消同伴。首期禁止嵌套、分支交叉及未支持的区域内调用；整个区域可参与反馈循环。禁止将模型内部的并行想法作为调度许可。

## Session、对话与恢复

平台 Session/Turn 保存用户输入、运行关联、问题/回答与 SSE 投递；Goose 保存原生模型历史。刷新或重开页面按稳定 Session/Turn 和事件游标读取，不再次提交旧请求。原生会话绑定 Graph/节点/用户、模型与 endpoint，不能静默切换或借用另一用户历史。

必要提问由原生 elicitation 接入，同一 Turn 等待有效回答。重复相同回答幂等，冲突回答拒绝；保存回答不等于破坏性工具已经完成。删除 Graph 的确认绑定准确目标和资源前置条件，目标变化、取消或重启不能复用旧确认。

普通“继续”保留 Run cursor、invocation 和原生 Session。工具可能已生效但结果缺失时，Host 保存中断与观察事实，Agent 核查 workspace、Artifact 和可用外部状态后决定继续、补偿或询问用户。底层 fail-closed 可以保留，但内部恢复字段不包装成用户审批流程。

停止请求被接受与节点停止是两个时点。重启后恢复已保存事实，不自动执行未结算任务或重放确认。任意外部副作用不承诺 exactly-once，部署回滚也不自动撤销业务效果。

Session 与 Run 分别拥有状态。Session 只引用真实 Run，不复制另一份运行状态。删除前必须处理关联 Run 和原生历史范围；单条持续会话 Run 删除受共享 scope 约束，不能破坏仍在使用的历史。

常驻实例的 stop/resume 与 explicit retire 分开：stop/resume 保留 current binding 和冻结快照；retire 要求 Run 已真实 Stopped/终态且非 active，Session 的 Running Turn 及 pending/sending/unknown delivery 均阻断退役。退役保留历史、禁止旧 Run resume 或重新绑定，下一条可信消息惰性创建新实例；画布更新不热改旧 Run。Graph cascade 删除先完成保护与退役，再按 owned bindings 清理输入、回复、yield、checkpoint 与执行数据，不把 opt-in 升级变成全历史 workspace 清除。

## Plugin 与能力积累

Plugin 包含渐进披露的 Skill、资源、工具和可选渠道说明。短目录提供名称、描述和入口；完整内容保留在唯一 Plugin 目录，按需只读读取。工具语言由作者决定，官方工具使用 Rust；第三方工具环境与外部依赖由部署者准备并授权。

Plugin 说明不是权限策略。文件路径、网络、工具、凭据和控制动作由 Host/Sandbox 强制校验。Graph/Plugin 包不能包含密钥或扩大宿主授权；模型输出不构成授权。知识库自动编译、通用 Plugin 市场和环境自动安装暂缓。

Library 拥有已安装能力、来源摘要和 owner-bound 授权事实。API 与 Pilot 通过稳定用例管理，不直接修改其他模块的私有 SQL 表、锁或字典。运行中使用的资源必须可追溯，更新不能静默替换已经冻结的依赖。

## 渠道与多用户工作

普通企业助手路径为平台事件 → 可信 Session/Turn → 指定普通 Graph → 指定回复节点 → 原来源投递。用户消息不能覆盖服务端选择的 Graph、回复节点、Plugin、路径或权限。

同一 Session 串行，不同用户可以并发同图。当前 opt-in 常驻助手用普通 `wait_input (Op.host session.wait_input) → assistant (Agent) → reply (Op.host session.reply) → wait_input` 循环表达：首条消息惰性创建 Session 的 current assistant Run，后续 Turn 通过等待节点的固定产物进入同 Run，不覆写启动输入。Kernel 只支持通用 host port，不依赖 Session 或 WeCom；具体身份、等待和投递属于 Host，手动/standalone 默认无此授权。一次性助手仍保留跨 Run 的只读 `/previous` 交接，不原地替换已有 `wecom-assistant`。

新消息打断旧轮，必须等 Goose 与工具执行者真正退出并保存中断事实后再接续；不能用停止整个 Run 后静默复活替代。旧回复 `suppressed` 不算成功，新轮复用绑定的原生历史与本节点现场，持续补充的输入事实应保留，超出当轮有界范围须明确提示。未知业务效果先由 Agent 核查，进程重启仍需显式 resume，不能自动重放。

附件在可信入口冻结字节、hash 和 MIME，Graph 只读使用获准输入。常驻助手当前 Turn 使用 `/in/channel`；仅额外授权当前输入之前、最近 confirmed 投递之后最多最近 8 条中断输入的固定快照，使用 `/in/channel-pending/<turn>`，不全历史挂载。出站媒体按 Turn 隔离并复用 ACK 账本，同 Run 不构成复用上一轮图片的许可。图片输入、媒体工具结果、摘要增量与平台回传是不同能力，需分别验收。企业微信欢迎语、语音、卡片、审批和更广渠道产品需求按明确业务接入决定，不能因 SDK 提供接口就宣称已支持。

Host 首次绑定查询最近 9 条、只授权最近 8 条并保存 `pending_truncated`；`output.interrupted_messages_truncated=true` 时 Agent 必须说明更早中断输入未自动纳入/未完整阅读，不能声称信息全保留。这不扩大历史权限，无数据库 schema 迁移，旧 facts 默认 `false`。

主动发送只允许获授权目标，来源身份与原生调用身份不能由模型伪造。发送 ACK、业务效果和 UI 展示分别记录；断线后的未知结果由 Agent 核查，不盲目重发。公网服务、真实 vision 和实际成员授权单独验收。

## RSI 与研究应用

RSI 是普通 Graph：从获授权 Run、源码、Graph/Plugin 与公开生态采集证据，五领域并行审查，经综合、独立评审和确定性门禁形成提案。证据目录随项目演进，不固定几百篇论文或依赖仓库名单；动态覆盖与未知必须可追溯。

优先改善已有 Graph/Plugin。只有证据表明通用执行缺口，才修改核心。候选改动在隔离 workspace 验证，使用来源明确及未参与修改的保留案例比较。发现需求、生成候选、回归通过、获准发布和长期改善是不同状态。

私有用户记录保持原访问范围。模型概括不构成脱敏或公开授权；跨用户积累能力不能直接复制聊天和业务原文。自动发布类别、私有记录保留期限和实际容量尚未冻结。

研究目标、证据质量与最终交付属于研究 Graph/Plugin。是否新增独立“研究合同”、怎样呈现目标变更和证据验收属于待讨论需求，不预建通用 Runtime 阶段。完成来自目标、证据与反馈；固定轮数、请求数或 `max_xxx` 不能替代研究收敛判断。

## 独立交付与生产

最小执行闭包由 Graph、显式 Plugin/工具资源、同一 Runtime 和部署者配置组成。完整平台可组合 WebUI、Session/Pilot、Scheduler 和渠道；精简部署复用同一 Runner，不维护另一套语义。

发行包记录资源与可执行身份，禁止密钥、未声明资源和可变状态；凭据、host-path grants 与用户数据属于部署环境。包构建、小图通过、真实业务验收和生产数据/配置切换分别记录。旧记录可原地只读盘点，不能冒充新原生会话或双写；生产切换和迁移需独立明确授权。

默认是本机单服务进程。Bearer 白名单不等于完整多租户授权；公开部署必须有对应网络与身份边界。任意外部系统权限与业务审批由对应 Plugin/产品契约决定。

## API 与用户界面

WebUI 与 Pilot 使用同一资源 API，稳定 ID、状态、错误和游标是公共契约。Graph 编辑保留 layout；Run 视图展示路径、轮次、控制请求、节点对话/工具、文件与 Artifact。聊天中的 `#anchor/...` 引用进入已有对象页面并能返回会话，不增加对象视图系统。

Health 与 readiness 分开。Responses 仅提供冻结的子集，Webhook、计划和手动输入共用 Run 接纳。浏览器刷新不能再次执行模型或外部副作用；API 缺失不能用空数组或假状态掩盖。具体路径见 [Web API 契约](rust-frontend-api-contract.md) 和 [使用指南](usage.md)。

长期可见计划、上下文用量、附件管理、编辑后分支、导出和更深的 Copilot 联动保留为产品需求，按明确切片决定，不自动进入本轮开发。

## 验收与明确不做

运行时回归默认使用小型 Graph 和确定性本地 Provider，实际经过 Host、Kernel、Goose ACP、授权 MCP 与 Sandbox，检查最终结果、节点历史和 workspace。低成本回归与少量真实模型兼容检查、大型业务内容验收分别进行，不能彼此冒充。

不自造模型循环、通用记忆/计划平台、第二套 Runner、通用审批平台、未知效果人工处置平台、跨存储原子事务、外部 exactly-once 保证或云工作流集群。不把隐藏提示词当控制 API，不为未观察到的问题预建平台。

后续范围和验收只维护在 [开发台账](pilot-development-plan.md)。早期方向与迁移证据见 [历史归档](archive/README.md)，旧依赖和内部格式不约束当前核心。
