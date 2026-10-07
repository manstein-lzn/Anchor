# Goose-only Runtime 迁移决定

2026-10-06 用户确认：停止 io-harness/Rig 与 Goose 的框架选型比较，Anchor Rust 服务端以 Goose + ACP 为唯一目标 Agent 后端。预算控制暂不进入开发或迁移门槛，优先让 Agent 完成任务。本文件固定边界与取舍；实际完成状态只写入 [开发台账](pilot-development-plan.md)。

## 目标与非目标

- AgentNode 与 Pilot 共用同一 Goose ACP 接入，不维护两套 Agent loop；Goose 承担 Provider、原生会话与 compaction，Anchor 不重新实现这些能力。
- Anchor Rust Kernel 继续拥有 Graph/Run、OpNode、Artifact、Sandbox、Plugin 绑定与宿主权限。ACP 是节点交互边界，MCP 是授权工具边界；不把 Goose session 或 recipe 变成 Anchor 的新 Graph/Session 模型。
- Goose-only 标准服务端及官方工具闭包不依赖 io-harness、Rig、Python/venv。保留 React/TypeScript WebUI；Git、Bubblewrap、固定 Goose binary 和选用的外部工具仍需显式部署。
- 迁移期间旧执行器只为已有验证和历史隔离暂存，退出条件是新 AgentNode/Pilot 产品切片通过，而不是等整个渠道/Library/生产搬迁全部完成。最终移除旧 Agent 装配及依赖，不发布永久多后端选择器，也不静默回退。
- 不重写已有 Graph/Plugin 格式、已可用的 Rust 官方工具或 Runner。不新增通用审批、恢复、记忆、计划、框架抽象平台或第二个执行内核。

## 收敛后的判断空间

选型已经结束；只保留两个不能用“尽快迁移”跳过的执行正确性门槛：

1. **恢复能继续任务**：保存原生工作记录和 Anchor invocation；结果未知时由 Agent 先查询 workspace/产物/外部现场，再继续，不默认重放。不要求 exactly-once 或旧框架格式兼容。
2. **工具不能越权**：工具目录、文件、网络、凭据和用户来源仍由宿主机械校验。提示词、cwd、ACP permission 或测试用共享网络都不能代替 Sandbox。

提问、压缩、媒体、反馈、并行、会话和 UI 是需要迁移的已有产品能力，不再逐项作为重新选框架的理由。使用固定版本公开能力和薄适配交付；若某项只能靠重建 Agent loop 或大规模长期 fork 解决，才暂停并说明具体缺口，而不是开展另一轮无边界比较。

## 预算取舍

本轮不实现累计请求/token/金额预算，不为精确 usage 建模型代理或独立计量系统，也不因完成预算对齐推迟迁移。已有预算字段或配置不自动删改；Goose 接入时必须明确告知未启用，不能静默忽略后宣称已强制执行，也不能因无预算设置拒绝普通任务。

取消、协议帧/内存上界、工具进程收束和明确配置的操作超时是安全与稳定性边界，不随预算取舍撤销。spike 的固定短时限不是长任务产品契约；后续接入不能用任意轮数或请求次数代替任务完成判断。

## 单节点薄适配契约

- Goose 原生 Session 是 Agent 历史与上下文的所有者；Anchor 只保存 invocation、binary/model binding、结构化完成及最近业务工具的现场观察，不另建 Agent 日志或恢复引擎。工具开始前保存名称与参数，结果缺失只表示未知，不能推导为未执行。
- 恢复复用公开 ACP `session/load` 与同一 invocation。原生历史和最近工具观察都提供给 Agent；存在旧业务观察时，必须取得本次工具结果的回执才可完成。Agent 负责核查 workspace/产物/外部现场，不默认重放，也不承诺 exactly-once。
- `final_result` 保持用户的 `summary/route` 语义。MCP 边界增加一次性 `observed_receipt`，机械拒绝未观察业务结果或猜测回执的完成；字段在保存 canonical 输出前移除。业务调用在完成之后仍拒绝。
- 工具关闭先停止接纳并发请求，再等待实际 handler 收束；不能因为短暂等待超时就释放 invocation，允许同一宿主启动重叠恢复。无法收束时仍保留运行所有权，宿主故障后的结果按未知事实核查。
- 模型 URL/wire/model 使用显式配置，恢复校验不含密钥的绑定摘要；不得为一次 Provider 失败静默换协议、换模型或退回旧框架。Usage 通知可用于观察，但不包装成精确累计费用或请求账本。

## 最短实施顺序

| 阶段 | 交付 | 出口 |
| --- | --- | --- |
| G0 Kernel 去框架耦合 | Graph/Sandbox 与旧 Rig 执行器分离；旧路径暂时条件编译，不复制 Runner | 无 Rig 的 Kernel 构建与 normal dependency tree；旧公开接口定向回归 |
| G1 Goose AgentNode 产品切片 | 从 A112 接入真实模型配置与 Plugin；结构化完成、原生历史、权限及未知结果核查后继续；复用 Goose compaction | 真实 Goose + 确定性 Provider 的小 Graph 验证结果、逐节点历史、workspace、Artifact、取消/杀进程与越权反例；再做少量真实 Provider 验收 |
| G2 Pilot 同一执行层并删除旧依赖 | Pilot 工具经授权 MCP 接入同一 ACP 层；续聊、提问/回答、必要删除确认、SSE/重连、Session 身份；移走 resolver 等泄漏的 io-harness 类型 | AgentNode/Pilot 共用 Goose；标准 Host/build/test 依赖树无 io-harness/rig-agent/rig-core；旧记录不冒充 Goose 可恢复记录 |
| G3 全 Rust 平台交付 | 复用现有 F3/F4 组件收口 Library/授权、渠道、官方工具和部署；WebUI 保持；候选版、旧数据策略与切换/回滚 | 无 Python 隔离发行环境通过现有用户流程；生产切换仍须独立授权 |

G1/G2 内部先交付一条可运行纵向链，再补原有产品边界；不能把未支持功能隐藏或删除。预算控制、比较报告和大型研究图不进入这条路径。Goose-only 构建和旧依赖移除在 G2 验收，不等待 G3 的所有外部服务与生产迁移。

G2 按可验收切片推进：G2a 是 Pilot 短会话、现有授权工具、同一原生 Session 续聊、取消与 SSE 重连；G2b 是默认 Goose 的标准 Host/Kernel、无旧框架的 build/test 依赖闭包、稳定工具契约、原生 MCP adapter、Goose trace 与 Rust 一键 fixture 回归；G2c 接公开 native elicitation 的提问/回答、必要删除确认及 UI/中断验收。删除确认复用精确 Graph 删除 precondition，拒绝、过期或目标变化均不能删除；Goose 进程内等待不冒称跨进程原样恢复。G2c 的普通 Pilot 切片已取得确定性回归、定向真实浏览器和真实 Provider 短会话证据，见 A117；Graph/channel Session、媒体、跨轮/渠道和 compaction 组合继续按产品边界验收，不重新选框架。实际状态与证据见 A115–A117，不把任一切片当 G2 全部完成。

## 并行边界与验收

G2d 先交付 MCP 图片结果的确定性传输切片：稳定工具契约保留 inline Image，Goose bridge 返回真实 MCP 图片而非 JSON base64；静态 PNG/JPEG/WebP 的 MIME/字节/大小/解码和权限边界由宿主校验。同一会话恢复仍由 Agent 核查现场并取得新工具回执，不重复副作用。fixture 选固定版本认可的视觉模型名称，但模型传输仍为确定性本地 Provider；没有真实 vision 配置/端到端证据时，只关闭传输切片，不关闭完整媒体、渠道/输入/图片 UI 或生产出口。默认配置不会被改成另一个模型来伪造视觉验收。

G2e 接既有 Graph/channel 会话契约和原生发送身份：可信 bundle/Session/完整节点绑定同一 Goose Session，每个 Run 仍拥有自己的 workspace/Artifact；循环和跳过节点不丢原生历史，前驱现场只读，未知副作用先核查而非重发。渠道请求身份取固定 Goose 的 MCP 原生 Session/tool-call 元数据并在业务调用前落盘，不从模型参数或 Harness attempt 编造。A119 关闭确定性 Graph/渠道发送与冻结图片输入传输切片，并取得真实 Chat 两轮/重启证据；不等于真实渠道网关、vision、摘要增量或生产替代通过。

A121 已交付 compaction + Plugin + 取消/重启的七个确定性组合和真实文本短会话压缩/重启证据。固定 Goose 的普通循环支持 prompt 入口阈值检查和同 prompt 超窗后的原生恢复；Anchor 不启实验循环或另写上下文引擎。此切片不证明多次/超大上下文、摘要 Provider 错误或提问/媒体组合全部通过，Pilot 手动 `/compact` 也尚未直接暴露。

A122 交付活跃 Run 原生 ACP trace、文本浏览器刷新验收和媒体 UI 投影，以及 `call.session` wait/detach/ACK/重启与后台 yield 后前台完成再接续的确定性组合。原生会话恢复只增加可信已结算后继候选，不重写 Run/逻辑前驱或放宽普通旧 Run 恢复；旧 receipt 仍需现场重读后替换。其历史入口为十一套 57 场景，零真实模型；A124 当前入口扩为十二套 59 场景并增加 Rust 文本 gateway ACK 与 Library 安装场景。图片 UI 浏览器 fixture 与 Goose 媒体传输是分别验收，尚不是完整真实视觉/渠道组合。固定 Goose ACP 没有完成工具参数的逐 token 增量，普通 assistant text 不冒充 `final_result.summary`；该缺口保持开放，不为它建立模型代理或自有 Agent loop。

G3 继续按四个不重叠边界并行：Library 安装与 OAuth、官方学术工具、渠道 transport、发行 builder；主轨独占共享 Host/Cargo/资源冻结和组合验收。A123 已交付安装与读取的确定性切片；原生文本 transport 和 Rust builder 分别提供私有 control/ACK 事实与 source-free Graph 包，仍不能替代完整平台 supervisor 或生产发行出口。组件接口稳定后即可整合，不等待重复报告，不由渠道 worker另写 Session/Run 调度。

后续先让已有原生组件形成完整用户路径：官方 scholarly Plugin/工具部署；Library OAuth 授权/刷新；平台渠道 Session supervisor、relations、附件与 `call.session` 投递/让位/续行组合。与此同时以固定 release 在无 Python 隔离环境验证发行闭包，并补真实 vision、媒体/压缩/提问组合和摘要增量的公开接入边界；固定 Goose 不能提供的增量需明确产品策略，不造模型代理或自有 Agent loop。最后执行候选版代表业务验收，明确旧数据只读/导入、未完成 Run 收束、单写 backend 和切换/回滚。真实外部发送、旧数据迁移与生产切换需单独明确授权和验收，不能用确定性用例代替。

主轨独占 ACP/MCP 的共享契约、Host 装配、恢复和最终验收；Kernel 去耦、已有官方工具与稳定平台模块可在独立 worktree 并行，一个路径只保留一个编辑者。Pilot 和 AgentNode 先共用稳定接入，避免两个 worker 各写一个运行时。

常规回归用实际 Host、共享 Runner、实际 Goose 和确定性本地 Provider，只替换模型传输，不搜索数百论文或调用生产业务系统。检查完成结果、原生历史与工具配对、workspace、Artifact 和失败事实。Goose 原生文件是工作记录来源，ACP 显示事件不是另一套恢复日志；模型/Provider、提问/重启、压缩/长会话等真实组合分别如实验收。

开发分支的标准构建可默认 Goose，但生产部署的 backend/配置仍不切换；新状态根不接管未完成旧 Run，不迁移凭据、不实际发送/发布。旧框架仅作为显式开发回归 feature 保留，标准服务端不自动 fallback。编译闭包去依赖、新产品切片通过、旧源码彻底删除、全 Rust 发行及生产切换是不同状态，不能提前合并为“迁移完成”。
