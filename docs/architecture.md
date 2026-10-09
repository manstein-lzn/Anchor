# 当前架构

Anchor 的维护实现是 Rust Host + 共享 Runtime Kernel + Goose ACP，WebUI 为 React/TypeScript。官方工具、发行 builder 和开发检查位于同一个 Cargo workspace。产品目标见 [产品与系统架构](product-architecture.md)，具体证据和未验收范围见 [开发台账](pilot-development-plan.md)。

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
| [anchor-runtime](../rust/anchor-runtime) | Graph 编译、IR、Runner、RunStore 和节点执行端口；不依赖 HTTP 或具体业务 |
| [anchor-runner-host](../rust/anchor-runner-host) | API/应用接纳、共享 Runner、Goose 接入、Artifact、渠道协调、调度和控制 |
| [anchor-graph-host](../rust/anchor-graph-host) | bundle 读取、资源绑定、独立 Graph Call 和恢复关联 |
| [anchor-platform-session](../rust/anchor-platform-session) | Session/Turn、幂等请求、问题/回答、事件游标、投递事实及助手 current/历史 binding 与 retire 事实 |
| [anchor-library](../rust/anchor-library) | Plugin/tool catalog、受审资源、凭据引用和 OAuth transaction |
| [anchor-sandbox-bwrap](../rust/anchor-sandbox-bwrap) | 文件挂载、命令、网络与取消边界 |
| [anchor-mcp-host](../rust/anchor-mcp-host) | MCP 连接、工具与媒体内容映射 |
| [anchor-scholarly](../rust/anchor-scholarly) | 官方学术搜索、论文读取与 MCP |
| [anchor-docmost-tools](../rust/anchor-docmost-tools)、[anchor-wecom-tools](../rust/anchor-wecom-tools) | 原生业务工具 |
| [anchor-wecom-gateway](../rust/anchor-wecom-gateway) | 企业微信 WebSocket、ACK、去重与私有控制 socket |
| [anchor-rsi](../rust/anchor-rsi) | RSI/周报采集、证据、评审门禁与安全产物装配；复用 Host 的路由与 Runner |
| [anchor-distribution](../rust/anchor-distribution) | 受审闭包的 source-free 归档，不执行输入程序 |
| [anchor-devtools](../rust/anchor-devtools) | 确定性回归、候选构建、部署预检与切换盘点 |
| [apps/web](../apps/web) | Graph 画布、Run 时间线、节点对话、Session 和产物投影 |

Goose 拥有 Agent loop、Provider、原生会话与 compaction；Anchor 拥有 Graph/Run、宿主授权、Artifact 和平台 Session/Turn。工具记录、API 与 UI 都围绕相同身份，UI 缓存不能成为第二份运行事实。

## Graph 与节点

作者 JSON 定义 `agents`、`ops`、`graphs`、`nodes`、`edges` 和 `layout`。`GraphSnapshot::from_authoring` 编译为执行 IR，保留作者定义供 Web 编辑；内联模块在执行前展开。bundle loader 绑定清单内资源并校验摘要。资源安装与 Graph 保存都经过 canonical Library 校验。

AgentNode 引用角色与节点级 `plugins`，角色只复用模型、指令和权限。Op 定义在 `run`、`call`、`fanout`、`join`、`host` 中恰好选择一种；Op 不继承 Plugin。`Op.host` 通过 `NodeExecutionPort` 交付通用 operation JSON；Kernel 不依赖 Session 存储或 WeCom 协议，具体能力由 Host 实现和授权，手动/standalone 入口默认没有 Session host operation 授权。反馈回访从同 Run 最近已提交产物延续，新 invocation 保留自己的执行现场；普通跨 Run 不自动继承。

同一 Run 由唯一协调者维护。配对 fanout/join 允许独立串行分支并发，所有成功后收束；分支失败取消同伴。嵌套、分支交叉和区域内 Graph Call 等不支持的组合在接纳前拒绝。独立 Graph Call 使用同一 Runner，`wait` 与 `detach` 的父子身份和生命周期分别持久化。

## 数据布局

Host 分别配置 bundle、catalog、state、workspace 和 Library 根。release 目录只读；可写根放在 release 外。Host 在监听前验证配置并持有 state-root `deployment-writer` 锁。开发脚本使用独立数据根，旧本地记录不会被重新接管。

```text
<release>/anchor-runtime/
├── bin/                   Host、固定 Goose 与显式工具
├── bundle/                Graph、manifest、只读 Plugin 资源
├── web/                   编译后的 WebUI
└── runtime-manifest.json  文件 hash、模式与 ELF 依赖身份

<mutable roots>/
├── catalog/               可编辑 Graph bundle 与资源闭包
├── library/               Plugin、工具、授权引用
├── state/                 RunStore、Session/Turn、事件、计划、渠道与锁
└── workspaces/            节点 workspace、fs2 Artifact、只读输入与 Goose 会话
```

fs2 Artifact 的文件及谱系是节点产物权威；只读 Git 输入视图兼容需要 commit 绑定的业务检查。单节点沙箱使用 `/workspace`、只读 `/in/<node>`、`/plugins/<id>`、`/tools/<id>`；显式授权的本地输入按 Run 冻结。开发环境路径由部署者注册，第三方工具的运行语言不改变核心职责。

常驻助手由 Host 显式绑定同 Run、同节点的稳定 workspace（`<workspace-root>/<run>/nodes/<node-hash>`）；Agent、Op、freeze 与清理共用该绑定。可写现场不随 invocation 新建，但每轮 invocation、完成事实与 fs2 Artifact 仍独立且不可变。工作区 owner/初始化事实与 Run 的唯一执行者共同保护接续，不能仅凭目录存在或 preparation lock 认定可接管。中断现场另存只读 checkpoint，不能冒充成功产物。新实例仅在首个 Agent invocation 从可信、获准的 previous 提交或中断快照初始化文件一次，不每轮复制 `/previous`；普通一次性 Graph 的工作区默认行为不变。

## Goose 与恢复

固定 Goose v1.53.0 x86_64 musl 二进制 SHA256 为 `bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340`。AgentNode 与 Pilot 共用 ACP/MCP 接入。Host 绑定二进制、模型/endpoint、原生 Session 和 invocation，恢复时不静默更换模型或新建替代历史。

AgentNode 通过授权完成工具提交 `summary` 和可选 `route`；普通回答不能完成节点。Host 校验路由、核查回执与必要产物后提交 Run 事实。业务工具效果发生但返回结果缺失时，保存观察事实；继续原生会话后，Agent 先核查 workspace、Artifact 和可用业务状态，再决定下一步。工具不会自动重放，外部副作用不承诺 exactly-once。

Run 的暂停/停止请求与节点已收束是不同事实。恢复保留 Graph cursor、invocation、原生 Session 和中断记录；重启不会自动执行未结算业务或确认。旧执行器记录保留旧身份，可识别和拒绝冒充 Goose 游标；清理源码不迁移旧数据。

Host 对外 trace 投影 ACP 消息、工具观察和媒体，支持增量读取与重开节点。该投影用于观察，不代替 Goose 原生历史。真实模型、vision 与长会话效果必须分别验收。

## Session、渠道与入口

Pilot Session 保存业务关联、Turn、问题/回答和 UI 事件；Goose 保存模型历史。Turn 提交按 `request_id` 幂等，SSE 按事件游标回放。原生 MCP elicitation 经过 ACP；必要删除确认绑定确切 Graph 与资源前置条件，重启中断未完成的问题，不重放旧确认。

普通助手由 Graph 执行。可信入口绑定来源、Session、Graph 与结果节点；同 Session 串行，不同用户独立。既有一次性助手仍逐条创建 Run，结算旧 Run 后接续原生历史，并在新工作区只读挂载 `/previous`；逐节点会话 scope 不跨用户、Graph 或节点复用。

### 常驻助手实例

已接入的常驻路径是普通三节点循环：`wait_input (Op.host session.wait_input) → assistant (Agent) → reply (Op.host session.reply) → wait_input`。示例 [wecom-persistent-assistant.json](../examples/graphs/wecom-persistent-assistant.json) 须显式选择，不原地替换已有 `wecom-assistant`。Host 当前只接纳这一输入驱动、无有限轮次上限的助手形状，不建设 WeCom 专用 Runner。

- 首条可信消息惰性创建该 Session 的 current assistant Run，后续输入复用它；Session 保存 Turn、幂等、历史 binding 和 retire 事实，RunStore/Runner 拥有冻结 Graph、cursor 与执行状态，Host 工作区适配层保存 Run 的稳定节点 workspace 绑定。
- `wait_input` 从持久输入事实领取 Turn，按等待 invocation 幂等绑定，重试不取下一条；将本轮文本、可信身份与附件引用提交给 Agent。后续消息不覆写 `Run.input`，等待期间不调用模型。
- 新输入打断旧轮而非停止整个 Run。Host 取消并等待 Goose 与工具执行者真正退出，保存未提交现场 checkpoint 和持久 `Yielded` 事实；Runner 冻结 `Interruption` 控制 Artifact 后沿普通 route 到 `reply`，旧回复记 `suppressed`，再回到等待节点。它不把旧 Agent/Turn 标成成功，不自动重放未知业务效果。
- `reply` 读取本 Turn 绑定的确切 Agent commit；出站图片按 Turn 登记，不取长期 Run 的“最新回复”，不串到下一轮。Turn 结算不等待 Run `Completed`，平台发送 claim、ACK 与 unknown 仍复用既有账本。

Session SQLite 正式迁移至 v7：移除入站 Run 的全局唯一约束，为助手增加按 Run 保留的历史 binding、每 Session 一个 current 的部分唯一约束和 `(run, wait invocation key) → Turn` 领取事实。旧 `associate_channel_run` 仍拒绝一个 Run 关联多 Turn；只有显式助手 binding 和领取契约授权复用。迁移保留旧入站、Turn、投递和关联事实，生产升级前仍须备份，未对现有生产数据库执行迁移。

Run 详情和 Web 节点投影将 `RunResult.interruption` 显示为“已中断”，不标作成功提交或普通执行失败。新版初始化遇前驱的中断控制 Artifact 时读取其已收束的工作现场快照，不把控制 Artifact 当作业务文件提交。

运行看板按事实分段投影。普通 Run 仍画自身 `started`..`updated`（仍在执行时延伸到当前时刻）的一段；常驻实例的 Run 跨多个 Turn，因此 `/timeline` 另发 `activity`：`turns JOIN turn_runs` 的执行窗口（`created_at`/`updated_at`，合并相邻窗口，按本页过滤；只有 Turn 仍在跑且宿主仍持有该 Run 时才标记未结束），空闲等待不画成执行。常驻 Run 即使没有窗口也带空 `activity`，前端只在 `started` 处画标记，绝不把它拉到当前时刻；`started` 早于本页但本页有窗口的常驻 Run 仍会收录，Session 库不可读时降级为不带窗口。该投影只读、不带 prompt 或投递内容，不新增 schema，也不改变 `/runs` 列表字段。

常驻媒体授权来自可信 Turn 及固定 invocation binding：当前附件只读挂载 `/in/channel`；在本轮输入之前、最近一次 confirmed 投递之后的中断输入中，冻结最多最近 8 条快照，分别只读挂载 `/in/channel-pending/<turn>`。这是有界补充范围，不是全历史授权；后续已越过 confirmed 边界的轮次不再自动挂载这些附件。

首次 wait binding 查询最近 9 条以检测截断，实际只授权最近 8 条并保存 `pending_truncated`；当 `output.interrupted_messages_truncated=true`，Agent 必须如实说明更早中断输入未自动纳入/未完整阅读，不能声称信息全保留。该标记无需额外数据库 schema 迁移，旧 binding facts 缺省为 `false`。

停止/恢复保留 current binding；停止请求须等到执行者退出和真实 `Stopped`，重启仍需显式 resume，不因新消息或连接恢复自动执行未知效果。`GET /channel-sessions/{session}/assistant` 读取 current 实例；`POST /channel-sessions/{session}/assistant/retire` 接收 `{ "run_id": "…" }`，仅在 Run 已真实 `Stopped` 或终态且非 active、Session 无 Running Turn 和 pending/sending/unknown delivery 时允许退役。retire 保留历史但解除 current，禁止该 Run resume/重新绑定；下一条可信消息才惰性创建新实例。Graph 更新不热改旧 Run 快照或权限，合法新版经退役后的新实例接纳。

Graph cascade 删除先保护 active/未收束执行、共享原生历史与未结算投递，再退役目标助手；清理只按 owned Run/Turn/invocation binding 删除输入、回复、yield、checkpoint、工作区和 Artifact。它不是全 workspace-root 扫除或生产旧数据迁移，部分删除重试保留可核查的所有权事实。

会话 Run 也可单独删除，但按血缘设前置条件：不能是 current assistant binding（须先 retire），不能有未结算渠道投递，且每个更新的 Run 都必须已经不可能再执行（终态，或已有自己的后继因而无法 resume/recover）或已完成会读取它的首轮 invocation（继承现场、`/previous` 挂载与 Goose 原生前驱身份都发生在那一次执行里）。删除哪个环节不限顺序：删除前先落不可变墓碑事实 `state/run-deletions/<run>.json`（Run、Graph bundle 身份、Session、回复节点，以及被删 Run 自己当时的上一个 Run），lineage 遍历遇到缺失前驱时用同一会话的墓碑继续往后走，幸存历史因此仍是一条链、只有一个头，没有墓碑的缺失前驱仍然 fail-closed。若每个幸存环节都已是某个已删除环节的前驱，则该 Session 没有活动链，下一条被接纳的轮次重新开链。墓碑不随 Run 删除移除，也不参与 current 或恢复判定；单 Run 删除不隐式 retire 当前助手实例，也不清理 Session 共享的 legacy scope。

### 渠道适配与监听

`call.session` 继承的是绑定用户的会话与显式输入映射，持有稳定来源身份。未知发送效果由 Agent 核查，ACK 与效果事实分别保存。Host 按 Graph 的 `channel.json` 自动监管原生 WeCom Gateway，私有控制路径由 state root 派生。Gateway 自己承担平台媒体边界：入站 image/file/voice 回调在本进程内下载短时 URL 并做 AES-256-CBC 解密，只把解密后的规范化附件交给 Host 既有契约（Host 冻结 SHA-256/大小/MIME 并只读挂载 `/in/channel`），临时 URL 与 AES key 不进入 Host 或 ledger；出站图片使用 Host 已校验的 `channel-replies` 项，由 Gateway 三阶段上传换 `media_id` 后单独发出 image 消息，发送前 durable claim、未确认结果不重发。出站任意文件类型平台未证实、不支持 video 回调，入站真实公网投递与文档理解仍需独立验收。

Gateway 常驻连接与 Graph Run 的执行状态分别投影。`GET /graphs` 只为实际受监管且被配置为 WeCom 事件目标的 Graph 返回 `listener`，通过鉴权的私有 NDJSON `status` 请求读取连接状态；已认证、连接中、认证中、重连、停止和不可用有独立状态，读取超时或异常不会冒充在线，也不返回控制凭据。`active_nodes` 来自仍有宿主执行者的 Running Run 游标及正在执行的并行分支，不把遗留游标当成当前工作。

Web 编辑画布的监听入口和工作流列表在已认证且空闲时使用 1px 虚线，并标注“常驻监听”；实际执行节点及活动工作流使用 3px 实线，空闲后恢复。重连或状态不可用显示对应文字与普通细实线。运行详情继续展示所选 Run 的历史事实；网关连接本身不创建 Run，惰性创建的常驻 Run 在 `wait_input` 时可以仍为 Running，但等待节点不计入 `active_nodes`，也不触发模型调用。

手动触发、计划、Webhook 与 Responses 子集共用接纳。Responses 只实现文档中的子集；健康 `/health` 与就绪 `/ready` 不发送模型请求。API-key 和 Graph/工具授权由 Host 强制执行，Plugin 说明和提示词不授予权限。

## 发行与验收边界

source-free builder 校验固定 Goose、目标 ELF、Graph 闭包、资源摘要与非密钥内容。它支持编译 Web 资产，拒绝脚本/source map、符号/硬链接和未声明资源；打包不会安装依赖或转换用户状态。使用命令见 [发行 builder](../rust/anchor-distribution/README.md)。

常规 [小型 Graph 回归](runtime-contract-tests.md) 执行实际 Host、Runner、Goose、授权 MCP 与 Sandbox，只替换模型传输。检查最终结果、逐节点历史、workspace、Artifact、恢复与幂等；[候选回归](rust-production-candidate.md) 使用 release Host/Web/Goose 的隔离发行包。

常驻循环的真实 provider 隔离两轮已于 2026-10-08 跑通，覆盖同 Run、稳定 workspace、旧 fs2 不变、回到等待与显式停止；证据与进一步验证方式见 [企微助手验收边界](wecom-assistant.md#docmost-和验收)。这不是实际 WeCom transport 验收，更多确定性边界仍由主轨验证；该常驻切片尚未部署，实际企微仍待用户私聊验收。

候选代码和本地证据不等于生产已切换。真实 Docmost、WeCom、公共学术服务、目标发行版 systemd/动态库、旧数据/配置迁移与回滚单独验收；具体状态以台账为准。早期实现和设计快照见 [历史归档](archive/README.md)。
