# Rust Host 与 WebUI API 契约

React/TypeScript WebUI 调用 Rust Host 的资源 API。Host 可直接托管编译 Web bundle；应用服务统一调用共享 Kernel 调度器，持有执行事实，前端负责投影与控制请求。当前证据与待验收项见 [开发台账](pilot-development-plan.md)。

## Graph、Run 与 Artifact

| API | 契约 |
| --- | --- |
| `GET /graphs`、`GET /graphs/{graph}` | Graph 身份、定义与活动 Run，保留作者 layout；联网兼容字段按节点策略投影 |
| `POST /graphs` | `{name,definition?}` 创建普通 Graph |
| `PUT /graphs/{graph}` | `{definition:{...}}` 编译并保存，通过 Library 绑定资源；常驻助手插件/联网调整下一 Turn 生效，存在 current 助手时结构修改返回 409 |
| `POST /graph-validation` | `{definition:{...}}` 校验而不保存/执行；无效输入与语义诊断区分 |
| `GET /graphs/{graph}/delete-precondition` | 返回 `{graph,precondition}` 与 ETag，`no-store`；交互确认在打开时捕获版本 |
| `DELETE /graphs/{graph}` | Web/Pilot 确认使用捕获的版本；HTTP 发送 `If-Match: "{precondition}"`，资源变化或重建返回 409。省略头保留显式无条件兼容语义；活动/可继续 Run、定义或未结算调用引用仍阻止删除，只清本 Graph 数据 |
| `POST /trigger` | `{graph,input?,workspace_bindings?}` 接纳为 `{run,graph}`，busy 冲突；所有新 Run 走 Kernel，携带已移除的 `execution` 字段返回400；绑定形如 `{target_node:{run,node}}`，只接受来源 Run/节点身份，不接受路径 |
| `GET /runs`、`GET /runs/{run}` | 状态、路径/轮次、trace、节点、调用、Plugin 与触发来源 |
| `POST /runs/{run}/pause\|stop\|resume` | pause 等当前节点提交后在下一派发边界等待人；stop 立即提交停止并取消 worker；显式 resume 保存未知效果的核查提示后，让 Agent 在同一原生 Session 中核查后继续；返回实际 operation，保留 snapshot/input/身份 |
| `GET /runs/{run}/files/{node}`、`.../{path}` | 不可变 Artifact 文件、预览/下载；拒绝 host-path 越权 |
| `GET /api/artifacts/{id}/files`、`.../{path}` | 固定交付清单与预览/下载，来源 Run 删除后显式保留的交付仍可读；过期字节返回 410 |
| `POST /api/artifacts/{id}/retention` | `{retained: boolean}` 显式保留/释放，经 Kernel 记录；已过期内容不能重新保留 |
| `POST /workspaces/bind` | `{source_run,source_node,target_run,target_node}` 显式复用获授权资源，拒绝活动目标和任意宿主路径 |
| `GET /runs/{id}/workspaces/{node}`、`DELETE /workspaces/{resource}` | 读取 Kernel 绑定；无引用、无执行者且归属明确后释放资源 |
| `GET /graph-relations`、`GET /channel-sessions` | 真实定义、调用与可信来源投影 |
| `GET /timeline`、`/schedules` | Run 与计划事实；时间戳未知如实披露，不编造历史 |

`GET/PUT /graphs/{graph}` 的 `plugin_activation: {mode,graph_digest}` 表示配置生效规则：`next_turn` 为常驻助手，`new_run` 为普通 Graph，不表示外部插件已可用。`GET /channel-sessions/{session}/assistant` 另投影 `plugin_update: {state,message?}`，状态为 `current/pending/restart_required/unavailable`；未知或读取失败不能显示为已生效。Web 在保存后提示下一条消息生效，活动 Graph 打开已有运行，渠道 Graph 打开助手管理，避免重复 trigger。

Graph 联网策略由节点的 `plugins` 引用承载：`{id:"network",network:"auto"|"enabled"|"denied",required?,default?}`。`network` 字段只允许用于 `network` 引用；AgentNode 可同时声明普通 Plugin，`Op.run` 只接受联网引用，其他 Op 不接受 Plugin 挂载。编辑只改变本节点引用，保留已有 `required/default`、其他插件和共享角色/命令。联网引用不参与普通默认/必需挂载计算，保留字段不能覆盖三态策略。三态的授权及旧格式读取边界归 [Plugin 联网契约](plugins.md#pilot-任务联网)与 [Graph 作者契约](rust-graph-authoring-contract.md)，`graph_tasks.network` 仍只约束新建子任务，不控制当前助手。

创建、保存和任务 Graph 的作者输入统一归一化为节点策略；新输入无旧联网字段、也无联网引用时按 `auto` 解释，无插件节点不为此增加空引用。单图 GET 只转换返回草稿，将旧角色/命令的联网 true 保留为 `enabled`，false 或缺省保留为 `denied`；无插件旧节点也保留明确的 denied 引用，避免后续选择插件意外开放网络。读取不改磁盘，`plugin_activation.graph_digest` 仍对应原保存定义，既有 Run snapshot/digest 和 Session 的已禁止状态不改写。

`GET /graphs` 保留完整有效目录，已成功收束的一次性任务项可附 `completed_task:true`；省略或 false 均留在主列表。该字段由应用服务按任务归属、现有计划、真实 Run、结果处理及共享引用派生，规则见[Graph 任务](architecture.md#按需-graph-任务与结果输入)，不能由前端按名称或时间自行推断。Web 默认将标记项收进“已完成的一次性任务”，保留展开、搜索、定义和运行历史入口；主列表计数不包含收纳项。再次安排或执行时投影撤销标记，不写归档状态。单图读取和运行历史不因收纳消失，计划及持久文件不因此清理。

文件列表来自交付清单，返回 `artifact`、`artifact_availability`、`files: [{path,size}]`，不枚举可写工作区。节点详情同时投影 `artifact_error`；available/expired/missing/corrupt 区分字节可用、过期、缺失和损坏，一份历史产物不可用不会使整个 Run 详情失败。Web 用固定 Artifact URL 预览和下载，保留过期版本的清单与明确提示。

`workspace_bindings` 由获授权的 Host 调用者在接纳新 Run 时显式提供；键是目标 Graph 节点，值引用既有 `{run,node}`。Host 校验目标节点、来源与排他执行状态，并在 Run 启动前保存绑定事实；Graph/模型不能传绝对路径或任意 Workspace ID。

Run 详情的 state.error 保留字符串：failed 时与 Kernel 结算投影 reason 一致，其他相位为空；时间线继续显示原始失败原因。state.recovery 保留数组，由已接受 Kernel history 的当前 invocation 观察扣除人工决定后投影；记录中不再存这些展示字段。

Run 详情的 `traces` 按节点与 invocation 展示本轮通知，不重复插入每轮恢复的 Session 累计历史；活动 invocation 保留当前轮工具进度。完整会话读取仍使用原有完整历史投影。详情的磁盘读取、JSON 投影和最终序列化在 blocking pool 执行，避免占住渠道和健康检查的异步线程；evidence 身份、Session 归属和文件安全校验保持。Web 明确区分加载、失败和未选择运行，失败可重试，同一 Run 刷新失败保留已加载内容；列表更新不取消在途详情，切换 Run 不接受旧请求晚到结果。

HTTP DTO 是产品投影，不直接输出执行器私有记录。未知结束时间、exit status 或来源必须保持未知；下游失败不能改变上游已提交事实。更新时间来自 Host 对已接受提交的独立观察，不读取文件 mtime。

Run 列表、详情、时间线、子调用及 Pilot Run 查询共用 `projection: {phase,waiting_for?,outcome?,reason,active,superseded?,actions:{pause,stop,resume,abandon},updated}`。phase 为 executing / waiting / settled；waiting 才含 human / child / effect，settled 才含 done / failed / escalated / unknown / aborted。`superseded` 只在同一会话已有更新的 Run 接替它时出现且为 true，此时 Run 不会再执行。旧顶层 status、state.status 和 control_requested、投影 terminal/quiescent/needs_attention 均退出对外 Run 契约；Session Turn、投递、监听与节点结果的状态仍属各自事实。

基础动作只由 Phase 和 worker 派生：Executing 且有 worker 可 pause，执行中可 stop，未结算可 abandon，无 worker 的 Executing 或 Waiting(Human) 可 resume。Session/身份/当前助手/被取代会话条件在边界收窄；写入仍执行现有授权、scope、lease 和控制守卫。pending 未知效果不隐藏普通 resume；人的显式 resume 授权 Agent 先核查现场，未结算退役实例仍可 abandon。Web 直接使用服务端 actions；缺少投影时显示不可用。

所有 Run 的持久权威为 Kernel FileStore。updated 来自 Host 的独立观察文件，无观察为 null，不参与控制。stop 接受后进入 waiting/human 并取消 worker，排空前不能 resume；无 worker 的 Executing 可接续。HTTP pause 与 Pilot run_pause 返回 operation=pause：持久宿主意图交给调度边界，当前节点结果与成本先提交，再以 Human stop、reason=paused 等待；不取消当前节点，最终节点已结算时保留结算。重启先核对端口已有完成事实，不重放未知效果。

Session Call 拒绝通用 pause/resume；未绑定的排队调用不在启动时自动执行。暂停或预算保留中的 conversation 仍占有当前轮，须显式 stop 或结算后才能交接。普通显式 resume 与认证 Session-call 共用核查接续：先登记原生 pending 观察，逐项持久化“先核查，不盲目重放”的提示，再 Human release，沿同一原生 Session 继续；后台自动恢复不执行该释放。人工 Completed/Abort 决定 API 仍可供已知结果的直接裁定，保留持久化先于控制、幂等、冲突和 Abort 约束；不把核查提示当成工具成功或模型完成。已放弃后的历史观察仍保留，但不阻止新轮次。Run/Graph 删除继续受 Session、调用引用、worker 和相位约束。并行分支 Op.call 属于 Host 准入拒绝的组合。

旧 `<state>/runs/*.json` 与 format 1–14 GraphRunRecord 不读取、不兼容，返回具名错误；当前为 format 15，不提供旧数据转换工具。相位与未知效果契约以本节和 [Run 门面](run-access-facade.md) 为准；默认预算的用户决定、各真实 Provider 场景与部署验收分别见[统一台账](pilot-development-plan.md)，不以旧迁移阶段的状态概括当前全部能力。

Run 详情另返回 `definition: {entry,nodes,edges,ops}`，仅来自该 Run 的冻结 snapshot，包含已展开的 inline 模块。它是只读展示拓扑，不含作者 layout、脚本、指令、输入及 host 参数，不能当作完整可重运行的作者定义；当前 Graph 的修改、损坏、移除或未保存草稿均不改变它。Run 文件与 Pilot 附件统一通过鉴权 Blob 下载并显示失败反馈。

## Session、Library 与渠道

Session API 包括创建/重命名/删除、消息、Turn、SSE、停止、问题与回答。Turn 按 `request_id` 去重，SSE 支持游标回放和重连；同一请求不得再次执行。问题/回答归 Session/Turn，模型历史归 Goose。

`POST /sessions`、`POST /v1/responses`、`POST /schedules` 可带单个 `Idempotency-Key`（1–128 个可见 ASCII 字节）；不带时保留原创建行为。同一既有 owner、同一操作种类、同一 key 重试复用已提交对象，输入内容变化返回 409；不同 key 即使内容相同也独立创建。owner 的认证及归属规则不变，不跨 owner 查询或复用。重放返回对象当前事实，不保证 HTTP 响应字节与首次完全一致。Session 及 Responses 的请求身份复用 Session 存储的原子事务与既有事件，保留期为所属 Session 生命周期；删除 Session 不承诺继续保留该 key。

带 key 的 Pilot 创建使用私有 Session 身份；显式提供 `id` 时须沿用 `pilot-private-` 前缀，无 key 的显式 ID 契约不变。Web 新建和首次发送共用创建操作，浏览器按当前凭据摘要保存未确认 key；创建失败或响应没有非空字符串 Session ID 时保留重试身份，成功才清除，不存储凭据原文。持久存储不可用时在当前页面内保留 key，迟到响应不能清除较新创建的 key。Responses 的请求身份包含输入、固定模型、stream 和 previous_response_id；首轮 Session 与每轮 Turn 都稳定复用，已有在跑或终态 Turn 不再次接纳模型执行，SSE 重试回放已有事件。不同续接关系不能复用同一 key。

Pilot `schedule_create` 接受可选 `request_id`，与 HTTP key 使用同一 owner 范围及创建用例。Scheduler 的创建 key 与原始 graph/rule/input 同原子快照持久化；重试先查已提交事实，不因原 once 时间已过或 Graph 后来移除而重新创建。编辑计划后返回当前计划但不重写原创建内容；计划删除后原 key 返回 409，不复活。旧 `schedules.json` 数组继续可读写；首次有 key 的创建写为 `{version:1,schedules:[...],creation_requests:[...]}`，未知版本不加载，请求记录不进入 API 投影。

计划统一按 Host 本地时间使用 once、interval、daily、weekly、monthly 规则，不引入 cron、独立时区或节假日历。`next_at` 是最早尚未接纳的时点；忙碌、Host 离线或接纳失败不再跳过该时点。每轮 tick 每个计划最多接纳一次，从原计划时点推算下一次，而非从实际启动时间漂移。每个时点使用稳定请求身份接纳普通 Run；Run 已持久化但计划游标保存失败时，重试复用该 Run，不重复执行。有限次数按已接纳 Run 递减，不按 tick、失败尝试或完成结果递减。

计划当前游标仍引用的 Run 回执不能通过 Run 删除、Graph 级联删除或自动回收移除，停用计划不解除保护；游标推进或取消计划后解除。重放已接纳 Run 只返回既有事实，不启动或恢复其 worker，也不因 Graph 后来修改、移除或授权撤回而重跑。仅有元数据而没有 Run 的中断接纳不是成功回执，继续接纳须保持相同快照并通过当前授权；显式移除计划及回执后的再次执行不属于去重保证。

挂载 `graph-tasks` 的 Agent 通过受限宿主工具复用同一 ScheduleStore 和准入；不是 Pilot 全局计划权限。作用域由原生 invocation 固定为 owner、来源 Graph、来源节点及 Session；无 Session 时使用来源 Run。模型不能自报作用域或借计划 ID 查询、修改其他作用域的计划。单规则可设置 `remaining` 次数；多个一次性时点以 `group` 原子创建和删除，最多 128 个，不接受混合重复规则或重复时点。创建请求的去重和删除后不复活规则同样适用，部分成员被删除后不补建整组。

委派计划在现有文件中保存可选 `remaining`、`group` 与私有授权快照；API、时间线和工具投影不返回该授权快照。工具列出的计划另带派生 `pending`，表示已启用且最早时点已到。后续派发仍复核原授权和当前授权，不能借旧计划扩大能力。作用域工具的暂停/启用保留待接纳时点；HTTP/Pilot 操作员重新启用仍只安排未来时点。待接纳时点的 Graph、规则和输入不可改写，应取消后另建；暂停、删除计划不删除已接纳 Run。

`GET /timeline` 把已到但尚未接纳的有效时点投影为 `pending` 或 `pending_busy`，未来有效时点为 `planned`；有限计划不投影超过剩余次数的时点，停用计划不投影未来执行。旧历史中的 `missed_busy`、`missed_downtime` 仍可表示此前未执行且已越过游标的时点，不能用于描述仍会重试的当前时点。已接纳时点关联真实 Run 及其相位。

普通定时 Run 与网页手动触发使用同一安装身份和凭据作用域转换。需要机器人通知时，计划仍可传普通 Graph 的 `input`，由已挂载渠道 Plugin 的执行节点按[发送授权契约](wecom-assistant.md#业务工具与附件边界)显式选择机器人；普通计划不自动把结果注入 Session。`graph-tasks` 的受限委派使用原始可信来源关联结果，由来源 Agent 决定后续处理与发送，不让 Scheduler 直接代发消息。

Pilot `POST /sessions/{session}/turns` 接受 `{request_id,message?,attachments?}`，文字与附件至少有一项；附件项为 `{name,data_base64,media_type?}`。同一 `request_id` 必须保持原文字与附件内容，`resume:true` 不能同时提交新文字或附件。输入限制为 16 个文件、单个 20 MiB、合计 50 MiB；其中图片只接受 PNG/JPEG/WebP，最多 8 张、单张 10 MiB、图片合计 20 MiB。只有该上传路由放宽至 72 MiB 请求体。

Turn 的 `attachments` 列出输入；每条消息投影均附加所属 `turn`，有附件时另带 `attachments`，普通文字与重复输入也可准确归属。继承输入保留原上传 Turn 的 URL，不重复生成用户上传消息。文件引用包含 `{id,name,size,sha256,media_type,url,download_url}`，不含宿主路径或密钥。`GET /sessions/{session}/turns/{turn}/attachments/{id}` 核验既有 Session 归属后读取原件；只有校验后的支持图片可内联显示，其他文件强制下载，`?download=1` 对图片也强制下载。预览和下载均沿用 Bearer 鉴权。

Pilot 用 `publish_file` 发布工作区文件，SSE 发出 `{type:"attachment",attachment:引用}`，历史从同一事件投影输出附件。发布保存当时的不可变版本，后续编辑工作区不改变已交付文件；原生工具返回的有效图片也进入同一输出路径。自动接续和恢复引用原获准输入，不扩大到全会话附件；删除 Session 沿既有归属清理原件。Web 支持选择、拖拽、粘贴图片及附件草稿，使用鉴权请求获得 Blob 后预览或下载。

Pilot 在可开始对话、没有消息且没有待处理执行或问题的空页面显示任务入口，示例只填入并保存草稿；未选择会话时，首次发送沿现有接纳接口创建 Session/Turn。进入对话后保留同一个输入框；插件面板默认收起，选中会话后预读真实挂载状态，窗口尺寸变化保留用户的开关选择。

Pilot 凭据请求按 Session 统一读取，待配置表单保持可操作；已完成、取消和中断的请求默认折叠在所属 Turn 的最后一条历史消息后，不追加为最新回复的待办。无可定位消息的旧请求保留在折叠记录中，不猜测归属。历史完成不代表当前凭据仍可用，展开后仍通过共享配置入口读取当前状态。运行或仍有待配置请求时轮询，已结算的空闲历史停止轮询；切换 Session 立即清除旧表单并取消旧读取。SSE 展示无游标的历史读取错误且保留重连游标；父级回调变化不重置订阅，终态后的问题快照不恢复待回答状态。

Plugin API 提供 catalog、安装/移除与 owner-bound 授权事务。OAuth、模型与远程 MCP 的真实兼容性需要独立验收；本地工具测试不等于外部授权流程已经通过。

`GET /plugins` 返回当前目录信息，不返回代码 `digest` 或使用说明；`GET /plugins/{id}` 按需提供完整详情、`digest` 和使用说明。HTTP、Pilot 与 `GET /sessions/{id}/plugins` 共用基础目录读取与健康观察：`definitionValid` 表示清单/声明路径结构有效，`dependenciesReady: bool|null` 表示当前依赖视图就绪；`contextAuthorized`、`connectionVerified` 没有实际证据时为 null。基础 `available` 由前两项决定，Session 可叠加上下文否决，不代表执行授权或连接成功。清单损坏项保留不可用行，Session 中缺失插件的已开启引用也保留以便撤销。启用仍校验完整定义和依赖；关闭不要求插件健康，保留归属、归档、required 和发布并发约束。列表不读取全部代码、生成 digest 或执行绑定；完整定义只在详情、挂载启用和执行时读取。

当前 Library、详情与 Session 行同时返回生命周期投影：`kind:"system"|"extension"`、`canUninstall:boolean`、`canReplace:boolean`、`updateSource:"anchor-release"|"extension"`。系统保留身份为 `network`、`graph-tasks`、`feishu`、`wecom`，完整系统身份还须通过发行资源校验；损坏目录仍显示 system 且两个写入能力均为 false，同时保持不可用。Web 只在对应能力明确为 true 时显示更新或卸载，未知能力不提供写入入口，不能以 ID、名称或 `available` 反推；系统详情说明随 Anchor 升级，普通 API、Pilot 与 CLI 发布仍在事实所有者处阻止替换或卸载。历史 Run 只显示保存的元数据，不用当前目录补写生命周期事实。

| 会话插件 API | 契约 |
| --- | --- |
| `GET /sessions/{id}/plugins` | 返回已安装、声明或已开启但缺失的插件行，保留 `on`、`explicitOn`、`required` 与健康投影 |
| `POST /sessions/{id}/plugins/{plugin}` | `{on:boolean}` 修改插件挂载，返回确认的行/列表及生效反馈；联网改用专门策略 API，其他插件可能按既有规则安排当前回复后的接续 |
| `POST /sessions/{id}/network` | 仅提交 `{mode}`，值为 `auto` / `enabled` / `denied`，原子更新既有 Session 挂载与联网上限，返回完整 `{plugins:[...]}`；不触发接续，归档会话返回 409，渠道/Graph 会话改由 Graph 节点管理 |

联网行通过 `networkProvider:true` 标识，返回保存的 `networkMode`、当前 `networkAllowed` 与依赖来源 `enabledBy:[id]`；`requiresNetwork` 表示 Plugin 的需求，不是授权。`on` 可表示主动或依赖启用，`explicitOn` 只表示主动挂载，均不能替代三态策略或有效访问。Web 不向联网行提供普通挂载开关；Graph 与 Session 复用同一三态控件，但前者只改草稿，后者只采用 Host 确认的结果。策略缺失时显示未知并禁用修改；已禁止联网而产生的不可用不锁死策略控件，开启仍由 Host 重新校验。写入失败保留最后确认模式与错误，网络策略变更只影响下一 Turn，进行中和重试的 Turn 使用首次冻结授权。

旧 Web 的 `{allowed:boolean}` 请求暂读入相同 Session 事实，安装中的客户端切换后删除兼容入口；新 Web 不再提交此格式或同时提交 mode/allowed。`auto` 的依赖启用不写另一份策略，`denied` 对任务工具整体生效，依赖不能恢复网络；模型与渠道控制连接不属于任务工具联网。权限语义与 Plugin 失效处理见 [Plugin](plugins.md#pilot-任务联网)。

Graph、Pilot、Library、历史 Run 共用插件设置的控件、排序、筛选、加载、错误及反馈；页面只提供上下文与操作适配。Web/Responses 新 Pilot 共用创建用例设置默认 `enabled`，通用 Session 创建及既有会话不改变；渠道和业务 Graph 不增加此默认。新增角色与首次并行区域通过共享角色构造消费 `GET /settings/model` 的 `execution.default_reference`，不写旧角色 `network:false`，也不在 Web 维护另一套模型默认值；设置读取失败不猜默认引用，迟到响应不覆盖更新后的草稿，用户显式选择和旧别名保留原义。

可信渠道入口绑定会话、真实发言者、Graph、回复节点与本地冻结附件。`ChannelIdentity.conversation_kind` 可为 `single/group`，缺省 single 且序列化省略；群按 conversation 共享 Session，sender 始终表示本条消息的真实成员。WeCom 对应字段仍为 `metadata.chat_type`。同 Session 串行，私聊/不同群隔离；中断输入保留 actor，回执核对 actor、kind 与 conversation。普通 Graph 和 Pilot 管理入口分别接纳，不把助手业务交给管理 Agent。

同一渠道管理入口支持 `wecom` 和 `feishu`，状态的 `platform` 来自已保存的 Graph/绑定；未配置的有效机器人 Graph 也返回平台。成员配置仍为 `{users,send_users,private_only}`，飞书当前要求 `private_only:true`，两个名单分别授权私聊接收和回复，成员为应用范围内 `open_id`。凭据字段由声明投影，飞书为 `FEISHU_APP_ID` / `FEISHU_APP_SECRET`，不在 Web 硬编码另一套凭据协议。`POST /channels/{graph}/events` 共用入站和结算逻辑，来源必须匹配绑定；飞书接受 single 的 text/image/file，文本禁止附件，媒体准入须有经过现有附件校验的字节，结算不要求再次携带媒体，metadata 的 request_id 须等于 event_id。`GET /channels/{graph}/progress/{event_id}` 复用只读 Goose 工具活动投影，不能成为投递或恢复事实。Graph listener 同样返回实际平台。旧企微回执和身份格式保持不变，飞书独立使用 `feishu_reply` / `channel-feishu-reply:`，Markdown 只改变传输展示，见[渠道契约](feishu-assistant.md)。

Webhook 与 Responses 共用接纳和身份校验。Responses 只实现冻结子集，不能宣称兼容完整 API；SSE 代理需关闭缓冲。`/health` 是 liveness，`/ready` 是配置 Graph readiness，不检查模型网络。

## Web 管理入口

管理操作不要求 Pilot Session 或模型调用。HTTP 与 Pilot 复用 Host 应用用例及配置事实；运行环境、渠道中的部署字段保持只读；模型服务字段与凭据统一由 Host 私有管理存储维护，环境变量不再提供模型配置或回退值，两个模型槽可独立编辑。设置、凭据与插件预检响应使用 `no-store`，密钥只通过专用表单提交，不进入聊天、日志投影或浏览器存储。

Web 异步操作反馈绑定发起时的上下文：删除旧 Run 的迟到响应仍刷新列表，但不能清空后来选中的 Run/节点。环境状态读取与初始化/取消共用版本保护，旧 GET 不覆盖已接纳的操作；后台轮询不重叠，慢响应仍可推进当前状态，关闭页面只取消本地观察。

`POST /sessions` 未指定 ID 时创建 `pilot-private-` 会话，与 `responses-` 会话一样归属本安装的身份：新安装是 `local`，从 API key 登录时代升级来的安装沿用当时记录的旧身份。因此新 Web 会话与插件详情共享同一 owner 的凭据。旧显式普通 ID 仍属共享 `local`，不自动迁移或跨 owner 复用密钥；旧会话表单明确显示该作用域，完成的配置请求只代表历史操作。

| API | 契约 |
| --- | --- |
| `GET /settings/model` | 返回下一轮默认模型 `execution`、固定 `providers.default/backup` 及兼容旧配置；每槽有 `fields: {endpoint, wire_api, flash, pro}`、`api_key_present`、`configured`、`source`（`legacy/host/empty`），不返回密钥；`configured` 只表示字段齐备，连通性须单独测试 |
| `PUT /settings/model/providers/{slot}` | 仅接受 `default/backup`；保存上述 fields 与 `api_key: {action: keep/set/clear, value?}`，可保存未填全的配置，返回完整状态；下一轮按引用读取最新值，正在执行的轮次保持原快照 |
| `POST /settings/model/providers/{slot}/test` | 接受同一草稿并显式指定 `tier: flash/pro`，发送小请求；不保存、不选择默认模型、不自动尝试另一槽 |
| `PUT /settings/model`、`POST /settings/model/test` | 保留旧执行配置管理/测试兼容接口；已保存的新槽不随旧配置编辑而改变 |
| `PUT/DELETE /settings/model/credentials/{provider}` | 管理保留的服务凭据；旧配置按地址与协议标识，命名槽历史另绑定 `slot`。两槽当前配置通过各槽 PUT 管理 |
| `GET /settings/runtime`、`POST /settings/runtime/prepare`、`POST /settings/runtime/cancel` | 读取 Python/Rust 状态与初始化操作；准备显式携带 `allow_network`，复用既有准备器、部署限制和取消收束 |
| `GET /plugins/{plugin}/credentials`、`PUT/DELETE /plugins/{plugin}/credentials/{target}/{field}` | owner-bound 字段状态：缺失、已保存、声明失配、部署管理；修改核验当前授权摘要，不返回原值 |
| `POST /plugins/install-preflight`、`POST /plugins/{plugin}/uninstall-preflight` | 候选或卸载影响预检；安装接收 GitHub tree URL 或受限普通文件列表，不接受宿主任意路径 |
| `GET /plugins/preflights`、`GET/DELETE /plugins/preflights/{token}`、`POST /plugins/preflights/{token}/confirm` | owner-bound 预检、操作观察与精确确认；确认复用 Library 发布锁和应用前置条件，同一操作不重复执行 |
| `GET /channels`、`GET/PUT /graphs/{graph}/channel`、`POST /graphs/{graph}/channel/enabled` | 按 Graph 管理机器人，复用渠道适配器与 Host supervisor；启停携带 `{enabled,expected_binding}`，保存的启用意图与实际连接分别投影；重复启用修复失效监听，实际重启时 `changed=true` |
| `GET /channel-sessions/{session}`、`.../assistant`、`POST .../assistant/retire`、`DELETE /channel-sessions/{session}` | 校验渠道 owner，复用 current binding、静止与未结算投递限制；退役不会立刻创建新实例 |
| `POST /schedules`、`PUT/DELETE /schedules/{schedule}` | 操作员新建、编辑、暂停/启用与删除共享计划；更新保留 ID/创建时间，未来输入修改保留下次时点，改规则或重新启用只安排未来执行；已到的时点不接受内容改写 |
| `POST /schedules/{schedule}/assign` | `{session,instruction,input?}` 把尚未到期、未在准入中的普通计划交给原机器人的既有私聊助手；保留 ID、Graph、规则、创建时间、下一时点、次数及启停状态，只原子更新输入和来源。instruction 非空且至多 8192 字节；核验旧输入的机器人/收件人与 Session 一致、当前发送权限、节点任务授权及目标确切版本。不启动助手、不创建用户 Turn、不迁移已接纳 Run。相同请求幂等，已委派计划不能重绑或借重试更新授权；`assignment` 投影返回来源说明而不返回授权快照 |

渠道 `listener` 在渠道 API、Pilot 和 `GET /graphs` 中使用同一观察：`connecting/authenticating/reconnecting` 表示正在连接，`authenticated` 表示已认证，停用为 `stopped`，失管或已发布的控制接口失联为 `unavailable`。`restart_required` 覆盖启用后守护任务缺失、已结束、资源可用但监听失联及账号变化；前端不能统一解释为账号变化。资源不齐时先检查 `available` 和凭据字段状态。正常、启动或重连中的重复启用返回 `changed=false`，不打断连接；同一绑定的修复不改成员、凭据和 callback key。

渠道助手的 `plugin_update` 是 Graph 插件配置与已冻结 Turn 策略的比较，不是连接健康检查。读取使用同一 Graph 锁文件的共享租约，并发读取互不排斥；保存、准入和授权写入仍要求独占，冲突保持 fail-closed。真正的读取失败或写锁占用仍返回 `state: unavailable` 及原因，不改动会话、Run 或 Plugin 权限。

`execution` 投影下一轮的默认选择：`default_reference/source/configured/fields/api_key_present/error`；其中 `fields` 是选定模型的公开字段，顶层旧 `fields/configured` 仅保留兼容含义。Pilot 和未指定模型的节点使用 `models.default.flash`，Graph 可显式选择 `models.default.pro`、`models.backup.flash/pro`；不自动降级。default 尚未保存时兼容旧托管默认配置和预填，已保存但不完整时明确失败；四个设置引用以外的原显式模型别名仅保留已托管的映射。模型历史凭据的 `deployment` 兼容字段固定为 `false`，可由网页轮换或清除；`deployment_fields` 仅可包含执行预算与传输控制，不含服务地址、模型名或密钥。旧 invocation 即使用过同名别名，恢复时仍核对其原始绑定。

新 Pilot Turn 和新节点 invocation 读取最新设置，包含同一 Run 的后续节点或常驻助手下一轮；进行中的执行保持独立快照，无需重启 Host。加载已有 Goose Session 后通过原生模型配置接口选定本轮模型，保留历史。恢复同一个节点 invocation 仍核对原模型/服务绑定，不能改用新服务；槽的公共历史和分服务凭据复用同一私有存储与锁，历史凭据投影增加 `slot`，可经现有凭据接口轮换或清除。两槽即使地址相同也使用独立凭据；换地址/协议的 `keep` 不携带原密钥。初始化状态可跨页面刷新读取，Host 重启后的未完成初始化显示中断，重试复用准备器检查现场。

模型测试保留 20 秒总时限。收到成功 HTTP 响应后读取正文超时返回固定诊断 `model provider response timed out`，网页说明已经连接、尚未得到完整模型结果；发出请求阶段超时仍为 `model connection test timed out`。HTTP 200 或保活消息不能单独作为模型测试通过的依据，也不证明服务整体不可用。

Runtime 状态的 `python_prepare_available` 表示当前配置满足 Python 准备条件：未被部署禁用、无准备字段冲突，且有完整外置解释器与准备器或有效的发行包 bootstrap。它与 `python_configured`（已有解释器）、`python_bootstrap_available`（发行包工具）分别判断，不保证后续下载成功。Web 按该字段开放按钮；旧 Host 缺字段时保守回退到 bootstrap 状态。GET 通过有界本地探测返回 `python_version`（完整实测版本）与 `python_version_error`；失败时版本为 null，不使用路径或默认安装版本推测。刷新会重新检测；prepare/cancel 响应不执行探测，Web 保留最近观察直至下一次 GET。部署管理的 Python 展示实测版本，不允许网页改版本，准备请求传 `version: null` 以复用原解释器。

插件预检依次投影 `preparing/ready/running/succeeded/failed`，确认携带具体替换与数据删除选项，重复确认观察同一操作。文件导入最多 2,048 项、解码后 32 MiB，请求上限 48 MiB；拒绝绝对路径、路径穿越、重复项及链接/特殊文件。预检和操作句柄驻留当前 Host，重启后客户端核对实际 Library 状态再决定是否重新预检，不把句柄丢失当作安装失败。独立保存凭据只完成同 owner、同连接身份且仍有效的待处理请求；若保存成功但通知同步失败，响应保留成功事实并附带提示，不让用户误认为密钥未保存。MCP 旧表单提交仍由后端比较创建时配置版本，配置变化后需重新申请，不能依赖通知成功来阻止旧请求覆盖。

插件默认连接配置为 owner-bound Host 事实：`GET/PUT /plugins/{plugin}/configuration`，显式协议测试为 `POST /plugins/{plugin}/configuration/test`。读取返回 `authorization_digest`、`revision`、逻辑字段和 MCP targets；只有普通字段可带 `value`。PUT 发送 `{authorization_digest, revision, fields: {field_id: {action: "set", value: "..."} | {action: "clear"}}}`，一次提交原子更新共享消费者。冲突返回 409；清除不回退旧环境/default，更换 URL 不携带未重新提交的旧凭据。测试请求带摘要与 revision，结果提供各 target 的 `verified/failed/unsupported`、工具数及 `current`；不执行业务工具，不把 stdio 初始化解释为上游认证。所有配置响应和错误使用私有 no-store 策略。旧 credentials API 与 Pilot 请求继续复用配置用例，显式映射的逻辑字段不再重复展示。

新建工作流与运行输入使用正式表单；运行输入草稿保存在当前浏览器。Graph/Run 的 Pilot 协助入口只带入对象引用到未发送草稿，不自动执行任务。插件、渠道、设置页面按各自需要读取状态，不恢复全工作台后台轮询。

## 权限、观察与验收

工具范围、路径/网络、原生 Session scope 与精确删除前置条件由 Host 强制执行。提示词、Plugin 说明、前端隐藏按钮和模型输出都不是授权。

trace 展示 ACP 的原生文本/工具内容，按 `tool_call_id` 配对；媒体有独立可接受类型/解码边界。投影用于查看，恢复仍使用持久 Run/工具事实与 Goose 原生 Session。普通 assistant text 不冒充 canonical 完成摘要。

浏览器验收使用实际 Rust Host、固定 Goose 与 Node 本地 Provider，检查 Graph CRUD、真实执行、暂停/停止/继续、历史、文件、Session 和必要提问。UI fixture 只证明展示；未配置而跳过、实际端到端与真实业务验收须分别记录。运行方法见 [小图回归](runtime-contract-tests.md)。
