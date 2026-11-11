# 飞书常驻助手

飞书渠道使用 `anchor-feishu-gateway` 主动建立 WebSocket 长连接，接收企业自建应用的文本、图片和文件私聊，交给现有 Host、Session/Turn、常驻 Graph 与 Goose ACP；进度、Markdown 回复和文件发送调用飞书 HTTPS 消息 API。无需公网入站端口、回调域名或内网穿透，但 Host 必须持续运行并能访问飞书。它不是群自定义 Webhook 机器人。

当前支持私聊进度卡、Markdown 回复、文件与图片接收，以及回原私聊的文件和任务通知；不接收群聊，不支持任意收件人发送，不申请通讯录权限。图片可作为文件发送，不是内联图片回复。传输实现、本地确定性回归、真实模型和真实飞书收发是不同验收层级；当前结果统一见[验收台账](pilot-development-plan.md)。

`graph-tasks` 与渠道通知是不同授权：任务创建、联网和定时执行仍须操作员单独授予；完成结果回原 Session，由原助手处理。普通用户轮的 `session.reply` 自动回复，结果轮的 `session.reply` 只记录已处理，不自动外发。结果轮须显式调用 Host 工具 `feishu_send_message({content})`，取得 confirmed 后才可称已通知。不能把任务创建成功、结果入队或内部生成回复当作已发送。

`feishu_send_message` 只允许正在处理可信任务结果的原助手使用，机器人、应用、`open_id` 与私聊来源均由 Host 选择；模型不能指定另一收件人或机器人。发送前复查当前助手绑定、Session 选择、启用状态、Plugin 授权和成员名单；停用或退役不会被任务完成绕过。工具在用户轮可发现，便于安排任务前核对通知能力，但不能用来重复发送普通回复。正文每条最多 20,480 UTF-8 字节，并受下述序列化卡片上限约束，不自动拆条。失败或受阻结果也可按原请求通知；发送未确认须如实说明，不以换内容或新身份重试。

## 进度、格式与文件

进度直接复用固定 Goose 的 ACP `tool_call` 与同 `toolCallId` 的 `tool_call_update`，以及现有 Host 只读进度投影；不是 Agent 自报进度、模型文本流或另一套执行器。状态卡显示当前步骤、文件查看、工作区命令、历史查询、业务工具及完成／失败状态。Goose 并非每次都会报告中间运行状态；没有工具事件时显示准备状态，不编造百分比。命令、路径、工具参数／结果和内部思考不外发，原生标题可能包含参数，因此不把标题作为渠道展示内容。

每条已准入用户消息使用一张卡片展示进度，按变化合并、最多每秒一次更新，工具观察最多五分钟。执行结束先停止工具订阅并显示「本轮执行已结束，正在发送回复」，不把 Host/Goose 结束事件当成投递回执。取得最终回答后，停止新的进度写入，等待正在进行的有界请求结束，再将本轮已知卡片 ID 独占交给最终回复：用完整 Markdown 回答替换原卡内容，不再保留「工作进度」标题，不另发一份答案，也不撤回消息。最终 PATCH 与原回复使用相同的持久投递 claim、内容摘要和 Host 结算；只有 HTTP 200 且 `code:0` 后才 confirmed，不新建一套回复事实。

交接最多等待当前一次两秒上限的进度请求。没有已知卡片、创建结果未知，或之前进度写入失败时，保守使用原回复 API 单独发送答案，避免迟到的旧状态覆盖答案；已有进度卡尽力标明结束。最终 PATCH 只有收到 HTTP 200／4xx 且非零业务码的明确失败响应，才允许在同一 claim 下改走单独回复。超时、断线、5xx、异常响应或关闭期间取消均为 unknown，不自动另发或重试；卡片一旦交给最终投递，进度、终态提示及关闭处理都不能再覆写它。文件与介绍继续分别投递，不被原卡终态替换。

订阅断开或达到工具更新上限只停止工具观察，不放弃已知且没有不确定写入的卡片。新消息替代、回调失败、监听关闭或五分钟到期会结束观察，对未交给最终投递的卡片尝试标明相应状态。进度映射是内存中的尽力展示，不持久保存；进程退出、平台失联或观察结束可能留下旧卡，重启后也可能新建卡片，不扫描、撤回或改写历史卡片。最终投递 confirmed 只表示飞书接受答案，不代表请求中的文件、业务操作或整项工作成功。

最终回复、任务通知和文件介绍使用显式 `schema:"2.0"` 的交互卡片，正文为 `body.elements` 中的 `markdown`。支持标题、列表、引用、强调、链接、行内／围栏代码和表格，不承诺与网页渲染完全相同；飞书客户端须为 7.20 或以上。每个 Markdown 组件最多四张表，表格超过五行时由客户端分页。序列化后的完整请求保守限制为 30,000 字节，转义较多时可能早于正文上限触发。主动提及标签会被转为普通文本，避免正文触发额外成员通知。正文及原有投递身份仍取原始回复，不另建一套富文本事实。

文件接收在长连接事件可靠记录并写出 ACK 后异步下载。下载仅使用原消息 ID、该事件的资源 key 和飞书 API 授权请求，不跟随重定向或事件内任意地址；凭据不进入模型。原有附件机制校验、冻结并只读挂载本轮 `/in/channel`，复用原 Session/Turn 和中断输入隔离。文件最多 20 MiB，PNG/JPEG/WebP 图片最多 10 MiB；文件名是非路径元数据，不接受文件夹、音视频或任意下载地址。

`feishu_send_file({path, content, context_files?})` 只回本轮可信来源的原私聊，模型没有机器人或收件人参数。用户轮可读取当前 `/workspace` 或已授权的 `/in` 文件，不跟随符号链接或越出挂载；发送本身不发布 Artifact，也不额外保存通知快照，需要保存生成物时另由 `final_result.files` 选择。任务结果轮只能发送精确选中的不可变 `/in/tasks` 或 `/in/notifications` 文件，保留生产者原文件名，并复用既有 Artifact 保留与通知记录；最多三个同样被选中的 `context_files` 仅保留供后续讨论，不额外外发。

外发文件须非空且不超过 20 MiB。Gateway 校验文件名、字节上限与摘要，使用 IM multipart 上传后发送文件消息，不把入站资源 key 或网盘 token 当作可外发文件。文件与 Markdown 介绍有分别确认的回执；上传成功不等于发送成功，文件已确认而介绍失败须报告部分成功，不能更换身份重发文件。任务文件已确认时，即使介绍失败仍保留通知及选中文件；普通回复仍由 `session.reply` 完成。

上传失败发生在消息发送前，错误会提示核对应用身份的 `im:resource:upload`（或 `im:resource`）、发布版本与文件约束；它不等同于所有上传错误都由权限引起，也不向模型透传平台原始错误正文。若确实未授予上传权限，接收／回复名单填 `*`、更换后缀或只添加用户身份权限均无效。由操作员开通应用身份权限并发布后，再明确请求重试原文件；Anchor 不自动扩大权限或补发旧轮附件。

## 配置与首次单聊

使用独立示例 [feishu-persistent-assistant](../examples/graphs/feishu-persistent-assistant.json)。其结构仍为 `session.wait_input → Agent → session.reply → wait_input`，不新建 Agent loop 或飞书专用 Runner。首条可信消息才创建常驻实例；后续私聊复用原 Session、Run、节点工作区与 Goose 原生历史。不同发送者、应用和渠道互相隔离。

已经保存其他平台机器人绑定的 Graph 不直接改绑飞书；其投递账本固定关联原平台与账号。保留原 Graph、会话和历史，另建飞书 Graph。

1. 在飞书开发者后台创建企业自建应用并添加机器人能力，取得 App ID / App Secret。
2. 在「应用身份」下开通 `im:message.p2p_msg:readonly` 与 `im:message:send_as_bot`，用于私聊事件和消息发送；进度与最终回答的卡片更新复用后者，不强制另加更新权限。文件／图片下载还需要 `im:message:readonly`（官方也允许 `im:message` 或 `im:message.history:readonly`），文件上传需要 `im:resource:upload`（或 `im:resource`）。这些额外权限由操作员明确开通并发布版本，不由 Anchor 自动申请；仅能收到私聊事件不代表能下载资源。不需要群消息或通讯录权限。
3. 在 Anchor 保存这张 Graph，打开 Agent 节点所挂 `feishu` Plugin 的「此 Graph 的机器人」。接收和回复名单均填写此应用下获准成员的 `open_id`；空名单不接纳成员，`*` 只在操作员明确填写后允许全部成员。不自动授权全部成员。只接收私聊固定开启。
4. 在同一入口通过专用输入控件填写 `FEISHU_APP_ID`、`FEISHU_APP_SECRET`，然后启用。凭据只由 Host 凭据库保存，不放进 Graph、Plugin、聊天或模型历史。
5. 观察监听状态成为 `authenticated`，再到飞书后台「事件与回调 → 事件配置」保存「使用长连接接收事件」，添加 `im.message.receive_v1`。飞书要求先有在线长连接才能保存订阅方式；此状态只证明网关已连接，不代表订阅、权限或应用发布已完成。
6. 创建并发布应用版本，确认测试成员在可用范围内，再从飞书客户端给机器人发送一条文本私聊。最终核对飞书客户端确实显示回复，以及 Anchor 原 Session 的 Turn、Run 和投递事实。

「接收消息」事件可凭任一接收权限订阅，但平台仍按具体权限决定推送范围；只开通群聊 @ 权限不会推送私聊。`im:message:readonly` 不代替私聊接收事件权限，「用户身份」下的 `im:message` 也不代替机器人的「应用身份」发送权限。长连接和订阅正常但没有入站时，先核对这两项应用身份权限是否已随版本发布生效，不要反复更换凭据或放开名单。

名单变化或凭据轮换先停用，再修改并重新启用。监听异常时复用现有「恢复连接」，不手动启动共享同一应用/状态目录的第二个 Gateway。应用多连接采用集群分发，不广播；同时启动开发和正式客户端可能让事件落到另一端。成员 `open_id` 是此应用范围内的身份，不用姓名、手机号或其他应用下的 ID 代替。

源码首次启动会准备此 Plugin 和 Graph；已有安装不会因更新源码而覆盖或补回用户资产。已有安装须经原有 Library 安装入口安装含 Gateway 可执行文件的 `feishu` Plugin，再经 Graph API/网页导入示例。安装或导入不自动配置成员、填写凭据、启用连接或发送消息。完整平台发行包仍按其 inventory 交付，不把源码示例当作已随包附带。

升级已有 Graph 时不要用基础示例覆盖用户已保存的 Plugin、Graph Tasks 或联网授权。若旧指令写死了不支持通知，按既有停止、退役和保存流程仅更新相应指令；保留原 Session、历史、工作区和计划，下一条可信来信才创建新版实例。安装新工具和回读权限不代替一次新的真实定时通知验收。

## 身份、事实与恢复

渠道管理 API、凭据表单、Supervisor、Graph listener 与 Session 控制均复用现有入口。Host 从已保存 Graph 的渠道声明选择 `wecom` 或 `feishu`，不接受回调事件替 Graph 选择平台。每个 Graph 只绑定一个渠道；回调凭据只能访问自己的 Graph。停用、删除、账号唯一性与停止/恢复/退役仍遵守[现有渠道契约](wecom-assistant.md)。

绑定继续使用 format 2；WeCom 省略 `platform` 的表示不变，飞书显式保存 `platform:"feishu"`。应用账户使用 `feishu:SHA256(trim(App ID))`，凭据目标为 `channel_feishu@<graph>`。旧企微账号、Session、入站哈希、投递 kind/key 和网关账本均不迁移、不改写。

飞书入站以 `message_id` 去重，不以可能改变的传输帧身份创建新 Turn。事件只在可靠记录后及时确认，慢模型处理与长连接收帧分离。二进制协议的分片、心跳和网络重连归 Gateway；Host 保存可信 `open_id`、`chat_id` 和来源，执行授权与会话隔离不由模型决定。

最终发送或更新前保存 durable claim。消息创建／回复只有飞书 HTTP 成功、业务成功且返回实际消息 ID 后才确认投递；原卡终态须已有创建时返回的消息 ID，且最终 PATCH 为 HTTP 200、`code:0`，不能因进度创建成功而提前确认答案。超时、断线或进程退出后的未知投递不自动重发。卡片序列化等本地校验失败发生在发送 claim 前，已有 Host 回执结算为 `suppressed`，不冒充已发送或未知发送，也不留下无限等待的投递。已有发送 claim 不能再按本地拒收处理。Gateway 重试 Host 结算不再次发送或更新平台消息。停止的助手不会因来信或重连自动恢复；重启继续核查原事实，不承诺外部 exactly-once。

任务通知复用原有逻辑投递身份与同一 Gateway SQLite 账本，不新增通知表、调度器或恢复队列。身份绑定原 Session、任务结果及机器人，不随中断后的新 Turn 或原生调用改变；同内容 confirmed 重入只读回旧事实，同身份更改正文或私聊身份报冲突，unknown 不再次发送。实际消息由飞书创建消息 API 按原 `open_id` 投递；这不是对群聊或其他成员的发送授权。先前已 handled 但未投递的结果不会因升级被重放。

渠道共用的私有目录/租约、事件摘要、投递账本、回调、状态控制、进度订阅读取器和外发文件校验位于 `anchor-channel-transport`，从 WeCom 原实现迁出，两个平台不互相依赖。平台登录、帧协议、卡片展示和消息 API 留在各自 Gateway；Host 复用同一个文件发送用例，不复制权限、路径或任务文件规则。不会让通用内核依赖飞书。

## 官方参考

- [使用长连接接收事件](https://open.feishu.cn/document/ukTMukTMukTM/uYDNxYjL2QTM24iN0EjN/event-subscription-configure-/request-url-configuration-case)
- [机器人配置与权限](https://open.feishu.cn/document/uAjLw4CM/uMzNwEjLzcDMx4yM3ATM/develop-an-echo-bot/faq)
- [接收消息与去重要求](https://open.feishu.cn/document/server-docs/im-v1/message/events/receive)
- [回复消息](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message/reply)
- [发送消息](https://open.feishu.cn/document/server-docs/im-v1/message/create)
- [卡片 2.0 Markdown](https://open.feishu.cn/document/uAjLw4CM/ukzMukzMukzM/feishu-cards/card-json-v2-components/content-components/rich-text)
- [更新卡片消息](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message/patch)
- [下载消息资源](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/message-resource/get)
- [上传文件](https://open.feishu.cn/document/uAjLw4CM/ukTMukTMukTM/reference/im-v1/file/create)
