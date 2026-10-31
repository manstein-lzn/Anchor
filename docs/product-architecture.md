# 产品与系统架构

本文记录产品方向、职责边界和执行不变量。它不作为实现清单；现状见 [当前架构](architecture.md)，验收范围与证据入口见 [验收摘要](pilot-development-plan.md)。

## 产品方向

Anchor 是本地优先、可观察、可恢复的 Agent 工作系统。用户通过 Graph 表达任务拆分、检查、返工和交付，通过 Plugin 积累可复用能力，通过 Run 检查执行证据，通过 Session 持续协作。

核心对象是 Graph、AgentNode、OpNode、Plugin、Run 和 Session。依赖声明属于插件，平台提供共享存储与每插件版本一份视图，见 [依赖标准](plugin-dependencies.md)。研究、周报、企业助手与递归改进由普通 Graph/Plugin 表达，业务阶段归业务所有者。

Pilot 的管家目标是：Anchor 独立部署、配置模型后，用户通过 Pilot 初始化基础环境，管理 Graph、Plugin 和依赖内容的完整生命周期，包括安装、检查、更新/回退和删除，并通过普通沙箱命令开发、自测和交付。优先验收 Python，概念不限语言；共享资源保持唯一事实来源，开发方法由 Pilot 决定。

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
| 解释器、依赖包与工具链（插件依赖） | Host 统一解析基础环境与正式来源；插件持有运行依赖，Session 持私有开发现场及临时候选/旧视图引用，Library 持共享存储与只读视图 | Host 管理配置、归属与授权；Plugin 依赖声明；Session 项目文件与实际开发现场；共享存储中的（包、版本） |
| 研究、周报、审查目标与内容验收 | 普通 Graph / Plugin | 业务证据与结论 |
| 画布、时间线、会话与文件展示 | WebUI | 对既有事实的投影 |

Graph、Plugin 各有一份权威资产，其余位置只保存引用。依赖声明跟随插件，私有开发现场跟随 Session，不新增命名环境资产；取舍原因见 [依赖标准的背景](plugin-dependencies.md#背景与动机)。

采用 Ports and Adapters 隔离外部入口；遵循 Clean Architecture 依赖方向；用限界上下文明确 Graph/Run、Session/Turn、Library/Plugin、Node Runtime/Sandbox 的事实和不变量。只采用有用的边界，不引入完整 DDD 仪式、微服务、事件溯源、CQRS 或通用依赖注入框架。

新设计依次确认：已授权用户结果或已观察失败；现有框架/契约能否复用；唯一事实所有者；失败、取消和恢复语义；可执行验收。小型可逆选择直接推进。改变用户结果、持久化迁移、权限、并发或恢复语义的重要决定需明确记录；文档用于后续开发，不是额外审批平台。

任务联网通过 `network` Plugin 主动或依赖启用，Host/Sandbox 执行授权。Pilot 默认启用任务联网以完成初始化与开发，用户可以明确关闭；默认不扩大业务节点或渠道权限。行为契约见 [任务联网](network-plugin.md)。

## 对象与所有权

| 对象 | 内容与生命周期 |
| --- | --- |
| Plugin | 独立清单、Skill、资源和工具入口；多个节点可以引用同一资产 |
| 插件依赖 | 插件声明自己的包依赖；平台共享存储按（包、版本）只存一份字节，每个插件版本一份视图；每次节点调用解析当前 Plugin 并记录实际观察到的依赖视图 |
| AgentNode | Graph 内角色引用、任务、Plugin 与权限；模型在节点 workspace 执行 |
| OpNode | Graph 内确定性命令、独立调用、配对并行控制或获授权 host operation |
| Graph | 可编辑 JSON、节点、边、接口、目标与 layout |
| Run | 冻结定义、启动输入/资源授权、cursor、节点工作区绑定、逐 invocation Artifact、路由和工具轨迹 |
| Session | 长期对话、可信来源、Turn、幂等、问题/回答和关联 Run；常驻助手 current/历史 binding 与 retire 事实 |
| Turn | 一次幂等输入或继续请求，保留事件游标、取消与投递身份 |
| Pilot | 管理和协作入口；完整平台不依赖业务 Graph，可初始化基础环境、开发并交付 Plugin，以及管理 Graph、联网/挂载、凭据、渠道与计划；各业务的真实验收范围见台账，自升级交接策略仍未冻结 |

Pilot 开发环境是 Session 每项目一份私有可写现场，引用共享只读解释器。项目文件作为重建基线，试装允许偏离；默认准备保留现场，显式重建或生命周期结束才释放。候选和正式 Plugin 按正式声明独立准备、测试和交付。具体行为见 [Pilot 开发](pilot-development.md)。

共享 `agents` 只是角色配置复用，不是独立 Agent 注册库。Plugin 直接挂在 AgentNode，不通过角色继承、默认合并或复制；OpNode 不隐式取得 Plugin 能力。

## Graph、工作区与产物

作者模型与 Runtime IR 分层：用户继续使用 Graph JSON，编译层负责模块展开、拓扑/接口/读写校验和资源绑定。IR 的限制必须在接纳前明确反馈。未知展示字段不能误当执行权限；支持边界由 [作者契约](rust-graph-authoring-contract.md) 和测试固定。

Graph 默认目标和单次 Run 输入分别保存。手动、计划、Webhook、Responses 与渠道共用接纳，Run 保留触发来源；一次输入不能修改 Graph 默认值。忙碌、停机错过与控制请求的状态必须可解释，计划不另建第二套调度器。

每个节点的 invocation 继续使用同一 `(Run, node)` 可写 Workspace；新 Run 默认获得独立现场，跨 Run 复用需要 Host 显式授权绑定资源。完成时 Agent 选择文件或目录形成不可变 Artifact，`writes` 是必交子集；Op 与控制节点也使用同一套显式交付契约。下游读取固定 Artifact 版本，不改变工作区字节。

Artifact 只保存被选择的文件内容，按内容去重；不复制完整项目、依赖、缓存或 `.git`，也不为失败和中断建立全目录 checkpoint。普通失败、中断和取消保留现场供后续 invocation 核查与继续；Run 删除释放它拥有的现场和交付引用，独立保留的 Artifact 或共享 Workspace 按自身引用回收。助手与其他 Graph 使用同一工作区和交付机制。

AgentNode 必须通过原生授权完成工具提交 `summary` 和可选 `route`。普通回答或看似正确的 JSON 不代表完成；缺少必需信息、非法路由或未完成现场核查，应反馈给同一 Agent 修正。机械门禁不能被模型判断替代。

### 组合、调用与并行

内联 Graph 模块属于同一个 Run；独立 `Op.call` 产生父子关联 Run。`wait` 等结果，`detach` 保留独立生命周期；目标只读取显式映射输入和选定产物。定义关系由 Graph 派生，调用关系保存为实际执行事实，不增加订阅平台。

`call.session` 交接到操作员选择并绑定用户的会话。用户新消息优先，系统调用保留来源身份；等待、让位、后台接续和恢复必须复用同一 Runner/Session 协调。投递使用已授权 Plugin 网关，不另建消息执行引擎。

配对 fanout/join 允许同一 Run 中独立串行分支并发，全部成功后收束，失败取消同伴。首期禁止嵌套、分支交叉及未支持的区域内调用；整个区域可参与反馈循环。禁止将模型内部的并行想法作为调度许可。

## Session、对话与恢复

平台 Session/Turn 保存用户输入、运行关联、问题/回答与 SSE 投递；Goose 保存原生模型历史。刷新或重开页面按稳定 Session/Turn 和事件游标读取，不再次提交旧请求。原生会话绑定 Graph/节点/用户、模型与 endpoint，不能静默切换或借用另一用户历史。

必要提问由原生 elicitation 接入，同一 Turn 等待有效回答。重复相同回答幂等，冲突回答拒绝；保存回答不等于破坏性工具已经完成。删除 Graph 的确认绑定准确目标和资源前置条件，目标变化、取消或重启不能复用旧确认。

提问、用户填写的回答及确认或拒绝直接放在当时的对话记录中，按原有顺序可回看。正在等待回答时显示必要的表单，完成后成为普通对话消息；不另设“历史提问”汇总入口。

普通“继续”保留 Run cursor、invocation 和原生 Session。工具可能已生效但结果缺失时，Host 保存中断与观察事实，Agent 核查 workspace、Artifact 和可用外部状态后决定继续、补偿或询问用户。底层 fail-closed 可以保留，但内部恢复字段不包装成用户审批流程。

停止请求被接受与节点停止是两个时点。重启后恢复已保存事实，不自动执行未结算任务或重放确认。任意外部副作用不承诺 exactly-once，部署回滚也不自动撤销业务效果。

Session 与 Run 分别拥有状态。Session 只引用真实 Run，不复制另一份运行状态。删除前必须处理关联 Run 和原生历史范围；单条持续会话 Run 删除受共享 scope 约束，不能破坏仍在使用的历史。

常驻实例的 stop/resume 与 explicit retire 分开：stop/resume 保留当前 Run、Session 与 Workspace 绑定；retire 要求 Run 已真实 Stopped/终态且非 active，Session 的 Running Turn 及 pending/sending/unknown delivery 均阻断退役。退役保留对话历史、禁止旧 Run resume 或重新绑定，下一条可信消息惰性创建新实例；Graph 结构更新不热改旧 Run；操作员保存常驻助手的插件选择和节点联网权限后，各 Session 在下一 Turn 冻结并应用，当前 Turn 和恢复保持原权限，工作文件通过已授权 Workspace 资源绑定继续使用。Graph cascade 删除先完成保护与退役，再按 owned bindings 清理输入、回复、yield 与执行数据，并释放 Workspace/Artifact 引用。

## Plugin 与能力积累

Plugin 包含 Skill、资源、工具和可选渠道说明，工具语言由作者决定。Host 为 Pilot 提供通用开发、候选准备/测试和正式安装接口，统一管理基础环境与授权；Session 保存临时开发现场，Library 保存正式能力及共享依赖。显式部署配置保持外部归属，Plugin 声明不能扩大宿主权限。

Pilot 与 AgentNode 共用从 Library 真身及授权事实派生的发现、读取和调用契约。有界能力摘要随 Goose 系统提示保留，完整说明、Skill、资源和 schema 按需读取；摘要、搜索和管理查询不赋予执行权限。当前 Turn 保持冻结授权，能力变化的生效与接续由 Host 决定，停止或新用户请求优先；变化不授权重做已完成任务。具体行为见 [Plugin](plugins.md#发现与读取) 和 [Session 契约](architecture.md#session渠道与入口)。

Plugin 说明不是权限策略。文件路径、网络、工具、凭据和控制动作由 Host/Sandbox 强制校验。Graph/Plugin 包不能包含密钥或扩大宿主授权；模型输出不构成授权。知识库自动编译与通用 Plugin 市场暂缓；受授权的插件依赖安装已进入管家目标（见 [插件依赖标准](plugin-dependencies.md)）。

Library 拥有已安装能力、来源摘要和 owner-bound OAuth 事实，Host 拥有基础环境配置、私有凭据和输入请求。API 与 Pilot 通过稳定用例管理，不直接修改其他模块的私有 SQL 表、锁或字典。Plugin 代码和依赖视图由 Library 管理；Run 不冻结 Plugin 版本，节点调用使用当前 Library 版本，并将实际视图记入 Run 观察。开发引用保护只限临时资源，不变成 Run 历史环境绑定。Pilot 对工作区的 Rustup/Cargo 和 Python 使用权来自 Host 授权，不能由 Plugin 声明扩大。

## 渠道与多用户工作

普通企业助手路径为平台事件 → 可信 Session/Turn → 指定普通 Graph → 指定回复节点 → 原来源投递。用户消息不能覆盖服务端选择的 Graph、回复节点、Plugin、路径或权限。

同一 Session 串行，不同用户可以并发同图。当前 opt-in 常驻助手用普通 `wait_input (Op.host session.wait_input) → assistant (Agent) → reply (Op.host session.reply) → wait_input` 循环表达：首条消息惰性创建 Session 的 current assistant Run，后续 Turn 通过等待节点的固定产物进入同 Run，不覆写启动输入。Kernel 只支持通用 host port，不依赖 Session 或 WeCom；具体身份、等待和投递属于 Host，手动/standalone 默认无此授权。渠道不再接纳每条消息一个 Run 的旧形态。

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

Pilot 的消息允许文字、文件与图片组合，回复允许展示图片和下载交付文件。Host 保存不可变原件，Session/Turn 保存归属与引用；网页上传和企微媒体接收复用公共附件处理，分别保留入口身份与授权。执行只读取获准输入，图片通过原生 ACP 内容交给 Goose；交付文件固定发布时的版本，不随之后工作区编辑变化。界面按 Host 的真实文件引用显示和下载，文档解析由获授权工具承担。

Health 与 readiness 分开。Responses 仅提供冻结的子集，Webhook、计划和手动输入共用 Run 接纳。浏览器刷新不能再次执行模型或外部副作用；API 缺失不能用空数组或假状态掩盖。具体路径见 [Web API 契约](rust-frontend-api-contract.md) 和 [使用指南](usage.md)。

长期可见计划、上下文用量、跨会话附件整理、编辑后分支、导出和更深的 Copilot 联动保留为产品需求，按明确切片决定，不自动进入本轮开发。

## 验收与明确不做

运行时回归默认使用小型 Graph 和确定性本地 Provider，实际经过 Host、Kernel、Goose ACP、授权 MCP 与 Sandbox，检查最终结果、节点历史和 workspace。低成本回归与少量真实模型兼容检查、大型业务内容验收分别进行，不能彼此冒充。

不自造模型循环、通用记忆/计划平台、第二套 Runner、通用审批平台、未知效果人工处置平台、跨存储原子事务、外部 exactly-once 保证或云工作流集群。不把隐藏提示词当控制 API，不为未观察到的问题预建平台。

当前验收结论与未关闭事项见 [验收摘要](pilot-development-plan.md)，未冻结的产品需求保留在本文。早期方向与迁移证据见 [历史归档](archive/README.md)，旧依赖和内部格式不约束当前核心。
