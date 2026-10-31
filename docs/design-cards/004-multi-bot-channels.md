# 设计卡 004：一张常驻 Graph 一个机器人

> **状态：已批准（2026-10-24）——D6A / D7A / D8A / D9A，按 §10 顺序施工。**
>
> 流程：设计卡 → 用户批准 → 按卡施工 → 偏离设计时停下改卡再问 → 对照 §8 验收清单逐条取证。
>
> 来源：用户 2026-10-24 的讨论。用户已同意的方向：
> - 以后可以随便建一张常驻 Graph，挂上企业微信 Plugin，在节点的 Plugin 详情里填好凭据，就能上线一个新机器人；
> - 用 Graph 名作为绑定标识；
> - 删除用环境变量配置企微的部署方式；
> - 只保留常驻助手这一种形态；
> - 把"消息触发"对话框并入 Plugin 详情；
> - 顺带修复"模型拒绝一次，助手就停了"。
>
> 本卡完成多机器人及上述附带改动。2026-10-28 用户授权补齐普通任务使用已有机器人主动通知的契约，见 R1；向 Session 注入任务或结果仍属于 §9 的后续方向。

## 1. 现状（读码结论，行号以 2e19d22 为准）

现在整个 Host 只能有一个企微入口。写死的地方有五层：

| 层 | 写死的地方 | 位置 |
| --- | --- | --- |
| 配置 | 只有一份 `platform/channels/wecom.json`；适配器里只有一个 `current`，也只有一个 `supervisor` | `channel_supervisor/management.rs:93-115, 294, 737` |
| 网关 | 状态目录是 `channels/<平台名>`，descriptor、回调密钥都按平台名存 | `channel_supervisor.rs:81-88, 198` |
| 路由与鉴权 | 回调路由固定为 `/channels/wecom/{events,progress,settlements}`；回调密钥只能解析出 owner，`current_settings` 只能拿到唯一那份配置 | `api.rs:1121-1123, 1238`；`api/wecom.rs:150-182` |
| 凭据 | 机器人凭据按 (owner, plugin, `channel_wecom`) 存，两个机器人会共用同一组 BotID/Secret | `credential_store.rs:83-89, 695-737` |
| 节点工具 | `wecom_send_message` 的 descriptor 固定读 `channels/wecom/control.json`，收件人名单取进程级环境变量 | `channel_tools.rs:248-287` |

另外还有一条环境变量部署路径（`ANCHOR_WECOM_GRAPH` 等），以及"每条消息一个 Run"的旧形态（`api/wecom.rs:388-439`），都只能表达一个机器人。

**不用改的部分：**
- 会话本来就按 `(owner, source, account, …)` 区分，`account` 由机器人凭据算出，所以两个机器人的会话天然是隔开的。
- 网关程序本身支持多开：URL 全部从环境变量读，状态目录有文件锁保护，没有固定端口。
- 所以 Session 数据库和网关程序都不用改结构。

## 2. 目标

1. 一个 Host 可以同时运行多个企微机器人。每个机器人对应一张常驻 Graph，有自己的凭据、成员名单、网关进程和投递账本，彼此互不影响。
2. 上线一个新机器人只需四步：建一张常驻 Graph → 在 Agent 节点上挂企业微信 Plugin → 在节点的 Plugin 详情里填凭据和成员名单 → 打开开关。
3. 删掉写死的单例代码、环境变量部署路径和旧形态，Host 里只剩一条渠道代码路径。
4. 模型或服务商拒绝本轮请求时，用户收到一条说明，助手继续等下一条消息。
5. 现在线上的机器人迁移后照常工作：会话、助手、投递账本都保留，已发出的消息不重发。

## 3. 定义

- **渠道 Plugin**：带有 `channel.json` 声明的 Plugin。它和普通 Plugin 用同一个 Library、同一套安装流程。本卡只支持 `platform: "wecom"`，没有为其他平台做通用抽象。
- **机器人 Graph**：同时满足以下三条的 Graph：
  - 是常驻助手形态（`session.wait_input → Agent → session.reply` 循环，现有 `AssistantPlan` 就是在判断这个）；
  - 它的 Agent 节点挂了一个渠道 Plugin；
  - 已经保存了绑定。

  回复节点不再手填，固定取这个 Agent 节点。
- **绑定**：机器人 Graph 的渠道配置，**用 Graph 名作标识**。内容包括：
  - owner、plugin；
  - 成员名单 `users`、可主动推送的成员 `send_users`；
  - 启用状态、授权摘要、回调密钥、机器人账号。

  机器人凭据（BotID/Secret）也属于这份绑定。

**规则：**

- **R1 网关归机器人 Graph，发送权归节点挂载。** 只有机器人 Graph 启动网关、保存机器人凭据。普通 Graph 的 Agent 节点显式挂载渠道 Plugin 后可调用 `wecom_send_message(bot_graph, userid, content)`，必须指定已有机器人的 Graph；每次发送由 Host 核对同 owner、同 Plugin、当前有效启用及 `send_users`。挂载授权的是该 owner 下使用此 Plugin 的机器人发送能力，不允许跨 owner 或使用未挂载的渠道。常驻助手固定使用自己的机器人，保留原 `userid/content` 参数；只有助手工作节点提供 `wecom_attach_image`。不按唯一机器人猜目标，不创建新网关，不注入 Session/Turn。
- **R2 一张常驻 Graph 最多挂一个渠道 Plugin。** 保存 Graph 时校验。
- **R3 机器人凭据按 Graph 存放。** `channel.json` 的 `required_environment`（BotID/Secret）存在绑定名下。Plugin 的 MCP 凭据（自建应用的 CorpID/Secret）仍按 Plugin 共用，和现在一样。
- **R4 一个机器人账号最多被一个已启用的绑定使用，整个 Host 范围都算。** 企微只允许一个机器人保持一条长连接，新连接会把旧的踢掉。所以启用时如果发现账号已被别的绑定占用，就拒绝，并说明是被哪张 Graph 占用了。

## 4. 方案

### 4.1 存储

| 内容 | 现在的位置 | 改成 |
| --- | --- | --- |
| 绑定 | `platform/channels/wecom.json`（format 1） | `platform/channels/graphs/<graph>.json`（format 2）。字段：owner、plugin、users、send_users、enabled、authorization_digest、callback_key、account。去掉 `graph`（文件名就是）和 `reply_node`（从 Graph 推导） |
| 机器人凭据 | `credentials/<owner>/<plugin>/channel_wecom/<FIELD>.json` | `credentials/<owner>/<plugin>/channel_wecom/<graph>/<FIELD>.json`。文件格式、权限、锁和授权摘要的算法都不变 |
| 网关状态（投递账本、profile、锁、control） | `channels/wecom/` | `channels/graphs/<digest>/`（见 S5 施工记录） |

旧的单例文件 `platform/channels/wecom.json` 如果还在，Host 启动时**直接拒绝，并在报错里写明原因**。Host 不保留任何兼容旧格式的读取路径，这和 C6 的惯例一致。线上数据由部署脚本离线迁移，见 §6。

### 4.2 Host

- **管理适配器**：`current` 改成按 Graph 名索引的 map；每个已启用的绑定配一个现有的 `ChannelSupervisor` 实例，一个实例管一个网关进程。`configuration.lock` 保留，它的作用是保证同一时间只有一个 Host 进程，跟有几个绑定无关。
- **路由**：
  - 网关回调改为 `/channels/{graph}/events`、`/channels/{graph}/progress/{event_id}`，要求请求带的回调密钥必须属于这个 Graph 的绑定。
  - `/channels/wecom/settlements` 这条路由：网关的结算其实发往 events 路由，施工时先核实这条路由已经没人调用，再删除。
  - 管理接口：
    - `GET /channels` 列出所有绑定及其状态；
    - `GET` / `PUT /graphs/{graph}/channel`；
    - `POST /graphs/{graph}/channel/enabled`；
    - `GET /graphs/{graph}/channel/credentials`；
    - `PUT` / `DELETE /graphs/{graph}/channel/credentials/{field}`。

    原来的 `/channels/{platform}/configuration|enabled` 删除。
- **鉴权**：每个回调密钥只能访问它自己那份绑定的三条回调路由。
- **身份**：
  - 事件对应的 `account`、Graph 和成员名单，一律由 Host 按绑定填写，事件自己带的不算。
  - 按 inbound 查 Run 的接口（`channel_run_for_inbound`，进度路由会用到）改成同时按账号限定，避免两个机器人的消息 ID 撞上时串到别人的 Run。
  - inbound ID 的算法保持不变，不迁移已有数据。
- **启动与恢复**：
  - 启动时恢复每个已启用的绑定。
  - 助手恢复资格改为按 `saved.graph` 找绑定，再核对账号一致。
- **Graph 保存**：
  - 机器人 Graph 的绑定处于启用状态时，拒绝从 Agent 节点上卸下渠道 Plugin，提示"先停用机器人"。
  - 已停用的绑定，卸下后在状态里显示"Graph 未挂载渠道插件"。
  - 结构修改仍沿用现有的"先退役再保存"规则。
- **Graph 删除**：见 D6。
- **节点工具**：
  - 按 R1 区分任务主动发送和助手图文回复；两者复用同一 Host 工具和现有网关控制通道。
  - 发送前读取应用服务的有效绑定投影，核对 owner、Plugin、启用状态和 `send_users`；不再由工具直接读取绑定文件或缓存名单。目标 Graph 保存在 Goose 原生调用参数中，原调用身份、幂等与未知效果处理沿用原链路。
- **网关环境变量**：
  - 继续透传调优参数：`ANCHOR_WECOM_WS_URL`、各项超时、`DEBUG`、`PROGRESS`。
  - 删除那些会覆盖绑定内容的变量：`ANCHOR_WECOM_{GRAPH,REPLY_NODE,USERS,SEND_USERS}`，以及进程级的 `WECOM_BOT_ID/SECRET`。

### 4.3 删除的部分

**环境变量部署路径：**
- `ChannelHostConfiguration` 的 deployment 字段，以及 external 模式；
- `WecomSettings` 和它的 `from_env`；
- 旧入口 `ChannelSupervisor::start`（用 `api_keys[0]` 当回调密钥）；
- 以 `anchor-channel-api-` 前缀判断的特殊鉴权分支。

**每条消息一个 Run 的旧形态：** `receive_event` 中不走常驻助手的那个分支、`/conversation-runs` 路由、`ConversationAdmission`、`admit_conversation`、`RunMetadata.channel`、`associate_channel_run`、`pending_channel_messages`，以及 `examples/graphs/wecom-assistant.json`。完整清单以施工前的读码表为准。

**和常驻助手共用、必须保留的部分：**
- `previous_run` 交接链：退役后新开实例要靠它，`call.session` 也在用；
- 显式授权的稳定 Workspace 资源交接；
- Goose 会话的前后接续；
- 删除墓碑；
- `stop_previous_run`；
- 投递结算。

只覆盖旧形态的测试会删掉或改写：`api/tests/conversations.rs`、`tests/goose_conversation.rs`，以及 `api/tests/wecom.rs` 里用单节点 fixture 的那 9 个测试。

- 校验类的测试改用常驻助手的 fixture，保留下来。
- 涉及会话链的测试（替换、删除墓碑），改成用"退役 → 新实例"的交接来表达。
- `goose_session_calls.rs` 里的前台 helper 改用常驻助手。

### 4.4 模型拒绝不再停掉助手

**现在的流程：**
1. 固定版本 Goose 的补丁会把服务商错误包装成 `-32603` 加 `data.reason = "provider_error"`。认证失败和额度耗尽各有单独的类别。
2. 传输层已经能识别这些类别（`transport.rs:466-472`），Pilot 也会据此把 Turn 记为 `failed`。
3. 但 AgentNode 从来不读这个识别结果，统一当成 `Uncertain → Interrupted` 处理（`goose_acp.rs:918-925, 257-259`）。
4. 常驻助手因此进入"等人处理"，用户看到的是"助手已停止"。

**改法**（只改 runner-host，Kernel 和 anchor-runtime 都不动）：

1. 在 `goose_acp.rs` 里，`provider_failure()` 识别出类别时，产生一个带类型的服务商失败，并把类别记进事实。普通 Graph 的结局保持现状（Interrupted，可以恢复），见 D9。
2. 在 `node_host/assistant.rs` 里，常驻助手的工作节点遇到带类型的服务商失败时：
   - Host 先写一条带原因的 Yield 事实（`YieldFact` 增加原因字段，旧事实缺这个字段时按"被新消息替换"处理）；
   - 然后返回 `Yielded`，路由到回复节点。这条路复用现有"被新消息打断"的那条路。
3. 回复节点碰到这个原因时，产出一条由 Host 署名的回复，内容大致是"模型服务拒绝了这次请求（类别）。请调整内容后重试。"，然后回到等待节点。
   - Turn 照常记为 completed，因为投递要求 Turn 必须是 completed。
   - "这一轮失败了"体现在回复内容和 interruption 记录里。

没有任何类别的错误（真正未知的情况）维持现状：助手停下来等人处理。我们不把"不知道发生了什么"当成"已经失败"。

**施工记录（S1，2026-10-24）：**
- 实现与上面一致。回复节点的输出带 `host_reply: "provider_rejected"` 和类别，不带图片；回复文案只显示类别，不显示服务商原文。
- 审查后补充：
  - 恢复执行同一 invocation 时，先清掉上一次的类别；
  - 被取消的那次尝试不记类别；
  - 类别 `provider_error` 同时覆盖临时故障（限流、5xx）和内容拦截，所以文案写成"可能是临时故障，也可能是内容被拦截"。
- **已知限制（实测）：** Goose 会把被拒绝的那条用户消息留在原生历史里，下一轮会再次发给模型。所以如果是**内容被拦截**，之后每一轮都可能继续被拒，直到这个会话的助手被退役、重新开始。临时故障不受影响。本卡不改 Goose 历史；如果以后要处理，单独开卡（例如在渠道里提供"重置对话"）。

## 5. Web 与 Pilot

**Web：**
- 删除图编排工具栏上的「消息触发」按钮和 `MessageTrigger` 对话框。
- 图编排里 Agent 节点的 Plugin 列表，打开详情时带上 Graph 名。`LibraryPluginDetails` 拿到 Graph 上下文，并且这个 Plugin 带渠道声明时，多显示一栏「此 Graph 的机器人」：
  - BotID/Secret：沿用 `PluginCredentials` 组件，换成按 Graph 的接口；
  - 成员名单和可主动推送的成员；
  - 启用开关；
  - 网关的实际状态：已停止 / 连接中 / 已认证 / 失败及原因，以及账号指纹。
- 草稿里挂了 Plugin 但还没保存时，显示"先保存 Graph"，因为绑定读的是已保存的版本。
- 从 Plugin 库打开详情时（没有 Graph 上下文），只列出"哪些 Graph 正在用它做机器人"，不在这里编辑机器人凭据。
- 「消息会话」现在已经显示 Graph 列，这一列就代表是哪个机器人。只改文案，不改结构。

**Pilot：**
- `channel_status{graph?}`：不带 Graph 时列出所有绑定。
- `channel_configure{graph, users, send_users}`。
- `channel_enable{graph, enabled}`。
- `credential_request` 申请机器人凭据时要带上 graph。
- 原生确认流程不变。

**插件说明：** `plugins/wecom/skills` 里"唯一的机器人连接""一个平台只绑定一张 Graph"这类说法要改掉。

## 6. 迁移与部署

线上现状：只有一个绑定，挂在 `wecom-persistent-assistant` 上，属于常驻形态。

迁移用离线脚本完成，和 `kernel-deploy-20261023` 一样：做一份独立构建，写切换脚本。

1. **前置检查**：
   - 运行中的 Turn 为 0；
   - 待投递、企微收件处理中、未送达结算都为 0；
   - 不存在任何带 `metadata.channel` 的 Run（旧形态的 Run）。

   任何一项不满足就中止，**什么都不改**。
2. 停 Host。从这一步开始，任何一步失败都要回滚到原样，并用旧版本重新启动，不能让服务停在半路上（吸取上次部署事故的教训）。
3. 迁移文件：
   - `platform/channels/wecom.json` → `platform/channels/graphs/wecom-persistent-assistant.json`（format 2）；
   - 机器人凭据 → 绑定名下的新位置；
   - `channels/wecom/` 下的 `delivery.sqlite`、profile 等数据 → `channels/graphs/<digest>/`。运行时文件 `control.json` 和 `control.sock` 不迁移，也不参与 hash 比对。
4. 核对迁移后的文件和迁移前逐字节一致。只有绑定文件是重新编码的，按字段逐一核对。
5. 用新版本启动，等健康检查和就绪检查通过、网关认证成功，再跑冒烟测试。
6. 会话数据库不改：账号和 Graph 都没变，现有会话和当前助手继续有效。

这次部署不调用真实模型，也不发送业务消息。真实对话等用户发下一条消息时验证。

## 7. 需要你拍的决定

- **D6｜删除一张机器人正在启用的 Graph**
  - **A（建议）拒绝，提示先停用机器人。** 停用后删除，绑定、机器人凭据和网关状态目录一起删掉。现有"先退役助手、投递已结算"的删除前提照常适用。
  - B 删除时自动停用。省一步操作，但删除这件事会顺带产生停机的副作用。
- **D7｜把一个机器人账号换到另一张 Graph**
  - 会话数据库里，一个会话固定属于一张 Graph（`verify_channel_session`）。如果直接换，这个账号下的老会话都会冲突。
  - **A（建议）启用时检测：这个账号已有属于别的 Graph 的会话时，拒绝启用，并提示去「消息会话」清理这些会话。** 不改会话数据库的结构。
  - B 自动退役并删除这些会话。更省事，但会悄悄丢掉对话历史。
- **D8｜catalog 里那张旧的 `wecom-assistant` Graph**
  - **A（建议）部署时删掉。** 它是旧形态的示例，在新规则下已经不能当机器人用。
  - B 留着当普通 Graph。
- **D9｜服务商拒绝的修复范围**
  - **A（建议）只改常驻助手。** 普通 Graph 维持 Interrupted，可以恢复；临时性的服务商错误（限流、5xx）也被归到 `provider_error` 这一类里，直接判成 Failed 会让它们失去恢复的机会。
  - B 所有 AgentNode 都按 Failed 结算，和 Pilot 一致。

## 8. 验收清单

| 编号 | 用例 | 断言 |
| --- | --- | --- |
| T1 | 两张机器人 Graph，用两个不同的模拟机器人账号同时启用 | 两个网关进程，状态目录各自独立（`channels/graphs/<digest>/`）；各自认证成功 |
| T2 | 同一个用户分别给两个机器人发消息 | 落在两个不同的会话里，进入各自的 Graph；A 的成员名单不影响 B |
| T3 | 用 A 的回调密钥调用 B 的回调路由 | 拒绝；B 的会话里没有新增任何内容 |
| T4 | 第二张 Graph 填了相同的 BotID 并尝试启用 | 拒绝（R4），提示占用它的 Graph 名；第一个机器人不受影响 |
| T5 | 两个机器人收到 event_id 相同的消息 | 各自的进度只指向自己的 Run |
| T6 | 修改 A 的凭据 | B 的凭据和运行状态都不变；读取接口拿不到任何凭据明文 |
| T7 | 普通 Graph 挂企业微信 Plugin 运行 | 不启动网关；发送必须显式指定同 owner、同 Plugin 的有效机器人并通过其接收人名单；没有图片回复工具。未挂载的节点不获得机器人发送工具 |
| T8 | 常驻 Graph 的 Agent 节点挂两个渠道 Plugin | 保存被拒绝（R2） |
| T9 | 绑定处于启用状态时，从节点上卸下渠道 Plugin，或者删除这张 Graph | 拒绝（D6=A）；停用之后可以执行，绑定、凭据和网关目录被清掉 |
| T10 | 账号已有属于其他 Graph 的会话时启用 | 拒绝（D7=A），提示要清理的会话 |
| T11 | `wecom_send_message` 在机器人 A 上调用 | 走 A 的网关，收件人受 A 的 `send_users` 限制 |
| T12 | Host 启动时状态目录里还有旧的 `wecom.json` | 拒绝启动，报错里写明原因 |
| T13 | 迁移脚本在一份线上状态副本上演练 | 迁移后启动正常；会话和当前助手可以恢复；账本逐字节一致；已结算的消息不重发；前置检查不满足时什么都不改 |
| T14 | 常驻助手遇到 `-32603` 加 `reason=provider_error`（确定性模型传输模拟） | 用户收到说明性回复；Run 回到等待状态；下一条消息正常处理；重启后从 Yield 事实恢复，不和 Goose 事实冲突 |
| T15 | 常驻助手遇到没有 reason 的 `-32603` | 维持现状：停下来，显示"助手已停止" |
| T16 | 普通 Graph 遇到服务商拒绝 | 维持现状（D9=A） |
| T17 | 旧形态清理完成后 | `/conversation-runs` 返回 404；没有 `AssistantPlan` 的 Graph 不能绑定成机器人；`call.session` 和删除墓碑相关的测试全部通过 |
| T18 | Web | 节点上的 Plugin 详情能完成"填凭据 → 启用 → 显示已认证"整个流程；「消息触发」入口已不存在；从 Plugin 库打开详情能看到在用它的 Graph 列表 |
| T19 | Kernel 冻结 | `git diff 9a5fa57 -- rust/anchor-kernel` 为空 |

## 9. 明确不做（后续方向）

- 卡片交互、按钮回调、人工等待的界面展示，以及 `feedback_event` 写入 hub；
- 后台 Run 结果回到对话、计划和 Webhook 投递到会话；
- Pilot 和渠道助手合并成同一个"助手"；
- 飞书和其他平台：本卡不做通用协议抽象；
- 修改成员名单不用重启网关（现在的规则是先停用再修改，本卡保留）；
- 一个机器人跨多个 owner 共用。

## 10. 施工顺序

每一步都要能独立通过测试。

1. **S1 服务商拒绝**（§4.4）：范围小、独立，可以先交付。
2. **S2 删除旧形态**（§4.3 的后半部分）：先做减法，后面的改动面更小。
3. **S3 一张 Graph 一个绑定**：存储、适配器、路由、鉴权、凭据、身份、节点工具，同时删除环境变量部署路径。
4. **S4 Web 和 Pilot**。
5. **S5 文档、迁移脚本**（含演练）、**部署**。部署需要你确认后再执行。

**施工记录（S3，2026-10-24）：**
- 按 §4 实现。没有用到的设计细节：`/channels/wecom/settlements` 路由已经没有调用者，直接删除；网关结算仍走 events 路由，并核对入站记录的 Graph 与路由一致。
- 回调密钥和 Graph 一一对应：一把密钥只能访问自己 Graph 的 `/channels/<graph>/…`，旧的 `anchor-channel-api-` 形式不再接受（T3）。
- 进度查询和入站去重按规范账号限定范围（T5），会话数据库结构不变。
- 进程环境不再提供渠道凭据；执行快照只包含 MCP 目标，网关凭据只交给它自己的 Supervisor。
- S3 当时仅向机器人 Graph 的助手节点注入渠道工具；2026-10-28 的 R1 修订补上普通任务显式选择已有机器人发送的授权关系（T7），助手仍固定使用自己的机器人（T11）。
- 管理 API 和 Pilot 工具已按 §5 换成按 Graph 的形式。
- 已覆盖：T1、T3、T4、T5、T7、T8、T9、T10、T11、T12、T19 的定向测试，以及固定 Goose 的渠道、常驻助手、插件轮次集成测试。T2、T6 由适配器测试覆盖（两个 Graph 各自的绑定、凭据目标互不影响），尚未用两个真实网关进程端到端验证。

**施工记录（S4，2026-10-24）：**
- Web 按 §5 实现：「消息触发」按钮和对话框已删除；节点 Plugin 详情只在该节点挂载了渠道 Plugin 时显示「此 Graph 的机器人」，否则和插件库一样只列出在用它的 Graph。凭据沿用 `PluginCredentials`，改为传入按 Graph 的接口。
- Pilot 的工具在 S3 已完成，界面没有另外的改动。
- T18 由组件测试和 `message-management` e2e（1440/390 两种宽度）覆盖：保存成员 → 凭据 → 启用 → 常驻监听。全量 e2e 在本机有 32 个与本改动无关的既有失败（真实后端和时序用例），改动后的失败是其子集。

**施工记录（S5，2026-10-25）：**
- 迁移脚本和演练见 `.local/channel-deploy-20261024/`（[README](../../.local/channel-deploy-20261024/README.md)）。T13 在线上状态副本上演练通过：前置检查不满足时什么都不改；迁移后回滚逐字节还原；新版 Host 在未迁移的状态上拒绝启动；迁移后机器人账号、成员和凭据不变，唯一的消息会话保留当前助手并正常恢复，账本字节不变，D8 删除可执行。演练用桩网关，不连真实企业微信。
- 演练发现：网关状态目录按 Graph 名命名时，`control.sock` 的路径会随名字变长，超过 Unix socket 的 107 字节上限，网关会一直重启。网关状态目录改为 `channels/graphs/<sha256(graph) 前 16 位>/`，路径仍然超长时 Host 直接报错（e806a22）。
- 2026-10-25 按用户确认用 `deploy.py` 部署 e806a22：迁移通过核对，网关重新认证，账号和成员不变，会话恢复，其他配置摘要不变，D8 已删除旧示例，冒烟 Run 通过。回滚路径未触发。真实企微对话待用户验收。

**独立审查（S3–S5，2026-10-25）后的修正：**
- 「启用」原来同时表示「保存的意图」和「实际在运行」。凭据改成别的账号后，机器人实际不运行，删除 Graph 的检查就会放行，等删到最后一步才被拒绝，这时历史已经清掉了。现在删除 Graph、卸下渠道 Plugin、修改凭据都按保存的启用意图判断。
- 删除时原来用 bundle 目录名当 Graph 名去查机器人，部署入口那张 Graph 的目录名和 Graph 名不同，会漏掉检查；现在传入真正的 Graph 名。
- 机器人启用时不能修改凭据（HTTP、Pilot 申请和提交都走同一条规则）。重启时或重新启用时如果账号变了，要重新过 R4/D7 检查，不通过就保持停用。
- 一个绑定文件损坏，只让它自己的 Graph 停用，不阻止 Host 和其他机器人启动；不是 `<graph>.json` 的文件不再当作绑定。暂存文件移到单独的目录，避免和以 `.tmp` 开头的 Graph 名冲突。
- 删除 Graph 时清掉该 Graph 在所有 Plugin 下的机器人凭据，没有绑定时也清。
- 「主动发送名单留空继承接收名单」的说法改正：接收名单不为空时必须显式填写。
- 已知未改：运维 API key 也能调用 `/channels/<graph>/events`（S3 之前就是这样）。

Kernel 的改动为零。anchor-runtime 预计也不用改；如果施工中发现需要改，先停下来更新本卡。每一步完成后做一次独立审查，再按清单取证。
