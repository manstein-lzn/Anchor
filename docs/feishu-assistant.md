# 飞书常驻助手

飞书渠道使用 `anchor-feishu-gateway` 主动建立 WebSocket 长连接，接收企业自建应用的文本、富文本 `post`、图片和文件私聊，以及显式授权群中人 @ 本 Bot 的文本／`post`，交给现有 Host、Session/Turn、常驻 Graph 与 Goose ACP；进度、Markdown 回复和文件发送调用飞书 HTTPS 消息 API。无需公网入站端口、回调域名或内网穿透，但 Host 必须持续运行并能访问飞书。它不是群自定义 Webhook 机器人。

当前支持私聊进度卡、Markdown 回复、文件与图片接收，以及回原私聊的文件和任务通知；显式授权群内的人 @ 当前 Bot 的文本收发已真实验收。可选的官方 CLI 应用工具，以及明确授权 peer Bot 的群内文本回复提及均默认关闭，不能因应用已获得 scope 就自动开启。人类 `post` 可携带图文和文件；独立群图片／文件消息仍忽略，peer Bot 仍只准入文本。2026-11-13 已部署 `post` 准入并通过固定 Goose 的本地多图原生输入回归；本次真实飞书客户端的图文收发仍待验收。传输实现、本地确定性回归、真实模型和真实飞书收发是不同验收层级；当前结果统一见[验收台账](pilot-development-plan.md)。

`graph-tasks` 与渠道通知是不同授权：任务创建、联网和定时执行仍须操作员单独授予；完成结果回原 Session，由原助手处理。普通用户轮的 `session.reply` 自动回复，结果轮不自动外发。结果轮须调用 `feishu_send_message({content})` 或文件工具，再调用 Host `task_result_resolve({action:"delivered"})` 核验原 Gateway confirmed 回执；纯 summary 不再完成结果消费。无需通知时须显式调用 `{action:"silent",reason:"..."}`，只记录有原因的静默；已提交的发送尚无回执、unknown 或文件/介绍部分成功均不能绕过。不能把任务创建成功、结果入队或内部生成回复当作已发送。机制已于 2026-11-14 上线；本次真实 Provider 和公网定时结果投递仍待验，见[上线边界](../experiments/task-result-resolution/rollout-20261114/README.md)。

`feishu_send_message` 只允许正在处理可信任务结果的原助手使用，机器人、应用、`open_id` 与私聊来源均由 Host 选择；模型不能指定另一收件人或机器人。发送前复查当前助手绑定、Session 选择、启用状态、Plugin 授权和成员名单；停用或退役不会被任务完成绕过。工具在用户轮可发现，便于安排任务前核对通知能力，但不能用来重复发送普通回复。正文每条最多 20,480 UTF-8 字节，并受下述序列化卡片上限约束，不自动拆条。失败或受阻结果也可按原请求通知；发送未确认须如实说明，不以换内容或新身份重试。

## 进度、格式与文件

进度直接复用固定 Goose 的 ACP `tool_call` 与同 `toolCallId` 的 `tool_call_update`，以及现有 Host 只读进度投影；不是 Agent 自报进度、模型文本流或另一套执行器。状态卡显示当前步骤、文件查看、工作区命令、历史查询、业务工具及完成／失败状态。Goose 并非每次都会报告中间运行状态；没有工具事件时显示准备状态，不编造百分比。命令、路径、工具参数／结果和内部思考不外发，原生标题可能包含参数，因此不把标题作为渠道展示内容。

每条已准入用户消息使用一张卡片展示进度，按变化合并、最多每秒一次更新，工具观察最多五分钟。执行结束先停止工具订阅并显示「本轮执行已结束，正在发送回复」，不把 Host/Goose 结束事件当成投递回执。取得最终回答后，停止新的进度写入，等待正在进行的有界请求结束，再将本轮已知卡片 ID 独占交给最终回复：用完整 Markdown 回答替换原卡内容，不再保留「工作进度」标题，不另发一份答案，也不撤回消息。最终 PATCH 与原回复使用相同的持久投递 claim、内容摘要和 Host 结算；只有 HTTP 200 且 `code:0` 后才 confirmed，不新建一套回复事实。

交接最多等待当前一次两秒上限的进度请求。没有已知卡片、创建结果未知，或之前进度写入失败时，保守使用原回复 API 单独发送答案，避免迟到的旧状态覆盖答案；已有进度卡尽力标明结束。最终 PATCH 只有收到 HTTP 200／4xx 且非零业务码的明确失败响应，才允许在同一 claim 下改走单独回复。超时、断线、5xx、异常响应或关闭期间取消均为 unknown，不自动另发或重试；卡片一旦交给最终投递，进度、终态提示及关闭处理都不能再覆写它。文件与介绍继续分别投递，不被原卡终态替换。

订阅断开或达到工具更新上限只停止工具观察，不放弃已知且没有不确定写入的卡片。新消息替代、回调失败、监听关闭或五分钟到期会结束观察，对未交给最终投递的卡片尝试标明相应状态。进度映射是内存中的尽力展示，不持久保存；进程退出、平台失联或观察结束可能留下旧卡，重启后也可能新建卡片，不扫描、撤回或改写历史卡片。最终投递 confirmed 只表示飞书接受答案，不代表请求中的文件、业务操作或整项工作成功。

最终回复、任务通知和文件介绍使用显式 `schema:"2.0"` 的交互卡片，正文为 `body.elements` 中的 `markdown`。支持标题、列表、引用、强调、链接、行内／围栏代码和表格，不承诺与网页渲染完全相同；飞书客户端须为 7.20 或以上。每个 Markdown 组件最多四张表，表格超过五行时由客户端分页。序列化后的完整请求保守限制为 30,000 字节，转义较多时可能早于正文上限触发。主动提及标签会被转为普通文本，避免正文触发额外成员通知。正文及原有投递身份仍取原始回复，不另建一套富文本事实。

`post` 接收支持直接正文及 locale 包装（优先 `zh_cn`、`en_us`、`ja_jp`，否则取首个可用语言），提取标题、文本／Markdown、多个图片资源和 `files` 附件，不把不同语言版本拼成重复内容。图片可无文字；去掉本 Bot 提及后仍有图片的群 `post` 也可准入，完全空白且无附件的消息不准入。群 `post` 仍须真实 @ 本 Bot，并同时满足当前群、接收成员和回复成员授权；不是放开独立群图片／文件或 peer Bot 媒体。

附件接收在长连接事件可靠记录并写出 ACK 后异步下载。下载仅使用原消息 ID、该事件的资源 key 和飞书 API 授权请求，不跟随重定向或事件内任意地址；凭据不进入模型。原有附件机制校验、冻结并只读挂载本轮 `/in/channel`，复用原 Session/Turn 和中断输入隔离。单轮最多 16 个附件，其中 PNG/JPEG/WebP 图片最多 8 张、每张 10 MiB、图片合计 20 MiB；单文件最多 20 MiB，其余总量校验沿用 Host。文件名是非路径元数据，不加载文件夹、音视频或任意下载地址。结算原回执不重新准入内容，Gateway 去除附件字节后仍可结算，包括原本只有图片、正文为空的 `post`；这不授权新输入或新发送。

`session.reply` 的 Markdown 卡片不会上传或显示 `/workspace`、`/in` 本地图片。已授权 CLI 的 Bot 如需交付图文，先用 `feishu_cli.files` 显式暂存图片，以 cwd 相对路径通过 `im images create --data '{"image_type":"message"}' --file ./chart.png` 上传，再把返回的 `image_key` 写入 `im +messages-reply --message-id <context.platform_message_id> --markdown '说明 ![图](img_...)'`；只有明确需要新消息时才用 `im +messages-send --chat-id <已核对的 chat_id>`。固定版 `--markdown` 生成 `post`，不会上传本地图片路径；`--image ./chart.png` 是独立图片发送，不能与 `--markdown` 合用。每次需要本地文件的调用都须重新暂存，准确工具参数见 [Feishu Skill](../plugins/feishu/skills/feishu/SKILL.md#mixed-text-and-image-output)。没有 CLI 的 Bot 在原私聊用 `feishu_send_file` 先发图片文件及文字介绍，不承诺内联图文或群文件发送。普通最终回答仍走 `session.reply`，已发送的图文／介绍不再重复；上传成功不等于消息已投递，未知结果不得自动重发。

`feishu_send_file({path, content, context_files?})` 只回本轮可信来源的原私聊，模型没有机器人或收件人参数。用户轮可读取当前 `/workspace` 或已授权的 `/in` 文件，不跟随符号链接或越出挂载；发送本身不发布 Artifact，也不额外保存通知快照，需要保存生成物时另由 `final_result.files` 选择。任务结果轮只能发送精确选中的不可变 `/in/tasks` 或 `/in/notifications` 文件，保留生产者原文件名，并复用既有 Artifact 保留与通知记录；最多三个同样被选中的 `context_files` 仅保留供后续讨论，不额外外发。

外发文件须非空且不超过 20 MiB。Gateway 校验文件名、字节上限与摘要，使用 IM multipart 上传后发送文件消息，不把入站资源 key 或网盘 token 当作可外发文件。文件与 Markdown 介绍有分别确认的回执；上传成功不等于发送成功，文件已确认而介绍失败须报告部分成功，不能更换身份重发文件。任务文件已确认时，即使介绍失败仍保留通知及选中文件；普通回复仍由 `session.reply` 完成。

上传失败发生在消息发送前，错误会提示核对应用身份的 `im:resource:upload`（或 `im:resource`）、发布版本与文件约束；它不等同于所有上传错误都由权限引起，也不向模型透传平台原始错误正文。若确实未授予上传权限，接收／回复名单填 `*`、更换后缀或只添加用户身份权限均无效。由操作员开通应用身份权限并发布后，再明确请求重试原文件；Anchor 不自动扩大权限或补发旧轮附件。

## 配置与首次单聊

使用独立示例 [feishu-persistent-assistant](../examples/graphs/feishu-persistent-assistant.json)。其结构仍为 `session.wait_input → Agent → session.reply → wait_input`，不新建 Agent loop 或飞书专用 Runner。首条可信消息才创建常驻实例；后续私聊复用原 Session、Run、节点工作区与 Goose 原生历史。不同发送者、应用和渠道互相隔离。

已经保存其他平台机器人绑定的 Graph 不直接改绑飞书；其投递账本固定关联原平台与账号。保留原 Graph、会话和历史，另建飞书 Graph。

1. 在飞书开发者后台创建企业自建应用并添加机器人能力，取得 App ID / App Secret。
2. 在「应用身份」下开通 `im:message.p2p_msg:readonly` 与 `im:message:send_as_bot`，用于私聊事件和消息发送；进度与最终回答的卡片更新复用后者，不强制另加更新权限。文件／图片下载还需要 `im:message:readonly`（官方也允许 `im:message` 或 `im:message.history:readonly`），文件上传需要 `im:resource:upload`（或 `im:resource`）。这些额外权限由操作员明确开通并发布版本，不由 Anchor 自动申请；仅能收到私聊事件不代表能下载资源。不需要群消息或通讯录权限。
3. 在 Anchor 保存这张 Graph，打开 Agent 节点所挂 `feishu` Plugin 的「此 Graph 的机器人」。接收和回复名单均填写此应用下获准成员的 `open_id`；空名单不接纳成员，`*` 只在操作员明确填写后允许全部成员。不自动授权全部成员。默认只接收私聊，群功能需按下一节另行授权。
4. 在同一入口通过专用输入控件填写 `FEISHU_APP_ID`、`FEISHU_APP_SECRET`，然后启用。凭据只由 Host 凭据库保存，不放进 Graph、Plugin、聊天或模型历史。
5. 观察监听状态成为 `authenticated`，再到飞书后台「事件与回调 → 事件配置」保存「使用长连接接收事件」，添加 `im.message.receive_v1`。飞书要求先有在线长连接才能保存订阅方式；此状态只证明网关已连接，不代表订阅、权限或应用发布已完成。
6. 创建并发布应用版本，确认测试成员在可用范围内，再从飞书客户端给机器人发送一条文本私聊。最终核对飞书客户端确实显示回复，以及 Anchor 原 Session 的 Turn、Run 和投递事实。

「接收消息」事件可凭任一接收权限订阅，但平台仍按具体权限决定推送范围；只开通群聊 @ 权限不会推送私聊。`im:message:readonly` 不代替私聊接收事件权限，「用户身份」下的 `im:message` 也不代替机器人的「应用身份」发送权限。长连接和订阅正常但没有入站时，先核对这两项应用身份权限是否已随版本发布生效，不要反复更换凭据或放开名单。

名单变化或凭据轮换先停用，再修改并重新启用。监听异常时复用现有「恢复连接」，不手动启动共享同一应用/状态目录的第二个 Gateway。应用多连接采用集群分发，不广播；同时启动开发和正式客户端可能让事件落到另一端。成员 `open_id` 是此应用范围内的身份，不用姓名、手机号或其他应用下的 ID 代替。

源码首次启动会准备此 Plugin 和 Graph；更新源码不等于升级已安装资产。`feishu` 属于系统 Plugin，已有安装缺少此项时由操作员按[系统 Plugin 维护](development.md#系统-plugin-维护)使用 `install-system`，更新已安装项使用 `upgrade-system`，不走普通 Web/Pilot 安装或覆盖路径。候选须包含 Gateway、固定版本的 `bin/lark-cli` 及其 MIT 许可并核对摘要，示例 Graph 另经 Graph API/网页导入。安装、更新或导入不自动部署 Host、扩大权限、配置成员、填写凭据、启用连接或发送消息。完整平台发行包预装此系统 Plugin，但不附带该业务示例 Graph。

升级已有 Graph 时不要用基础示例覆盖用户已保存的 Plugin、Graph Tasks 或联网授权。若旧指令写死了不支持通知，按既有停止、退役和保存流程仅更新相应指令；保留原 Session、历史、工作区和计划，下一条可信来信才创建新版实例。安装新工具和回读权限不代替一次新的真实定时通知验收。

## 获准群内的人 @ Bot

此切片复用既有共享群 Session、常驻 Graph、进度卡与原消息回复，不新增任务引擎。同 Bot 的同群成员共享历史，不同群、私聊及不同 Bot 相互隔离，真实发送者仍逐 Turn 保存。群与私聊不共享项目经理的会话上下文。Bot 来源须另按下一节授权。

1. 飞书应用新增应用身份 `im:message.group_at_msg:readonly`，保留私聊及 `im:message:send_as_bot`，发布版本并把 Bot 加入目标群。需要 Bot 协作时改用包含 Bot 提及的权限，见下一节；仅获批该权限不代替 Anchor 的 peer 授权。
2. 在停用的 Bot 配置中关闭「只接收私聊」，填写明确的群 `chat_id`（`oc_...`），接收与回复成员名单仍须同时授权发言人。群清单不接受 `*`；空清单不接纳任何群。只接收私聊时群清单须为空。保存后重新启用。
3. 在获准群中选择真实 Bot 提及并发送文本或 `post`（可含图片、文件），核对原群的进度/最终回复与 confirmed 回执，再验证另一获准成员接续。普通文本 `@名称`、提及别的 Bot、未授权群/成员均不入站；独立图片／文件消息仍忽略，Bot 消息没有独立 peer 授权也不入站。

若已有 Graph 的角色明确写了只收私聊或不支持群，应按已有停止/退役/保存流程更新对应指令后再开群；修改渠道设置不会替换冻结角色，也不应拿新示例覆盖用户现有 Graph。群共享历史会向获准群公开回复，启用前同时确认成员范围；沿用 `users:["*"]` 与 `send_users:["*"]` 就是允许群内所有成员，而不是仅群主。

Gateway 按当前应用查询自身 `bot.open_id` 核对 mentions；查询失败仅让该群事件等待平台重试，不停止私聊。重启后的待执行消息与已准备回复仍须复核当前群授权。新增 `groups`、`peer_bots`、`cli_enabled` 的默认值均不增加旧绑定的序列化字段。群附件只随获准的人类 `post` 入站，专用文件／通知工具仍限原私聊；群管理、主动群消息、云文档与 Base 经下述独立 CLI 授权访问，不因为能群聊就自动可用。

## 通用飞书应用工具

Anchor 随系统 Plugin 分发固定的官方 `larksuite/cli` 1.0.100，模型使用一个 `feishu_cli` 工具按需读取官方 Skills、命令帮助、schema，再调用文档、Wiki、Base、群管理等业务 API。不逐接口维护 Anchor 工具目录，也不把应用 Secret 或 token 交给模型。升级上游版本仍需审核、校验和部署，不能承诺尚未支持的未来 API 自动可用。

- 停用 Bot 后在原配置开启「飞书应用工具」（`cli_enabled:true`），再启用；只对挂载该 Plugin 的本 Bot 活动 Turn 提供工具。`argv:["context"]` 返回可信渠道、平台消息 ID 和获准 peer IDs，`argv:["--help"]`、`["skills","list"]`、`["im","+messages-reply","--help"]` 按需发现能力。
- 此授权允许使用该 Bot **全部已发布的应用身份业务权限与资源 ACL**，不受私聊回复名单限制；因此只向可信成员开放。它不继承用户登录，也不让模型切换应用。用户身份专属接口仍不可用，云文档/Wiki/Base 仍须共享实际资源。
- `argv:["capabilities"]`（Gateway 适配器只读别名，底层查询本应用的 `app_versions`，不附加其他参数）读取应用版本的 scope；只看已发布版本及 tenant 权限。需要 `application:application.app_version:readonly`，或已经获准的较宽 self-manage 权限。读取失败表示权限清单未知，不表示业务 API 没权限；本租户初次只读检查即缺少该项。
- CLI 独立隔离运行，临时 HOME/工作区、无 shell、个人配置、宿主目录或 `/proc`；禁用登录、配置、升级、事件监听及身份覆盖。使用既有网络隔离 helper 出网，不回退宿主网络；这不是完整内网目的地址防火墙。
- 显式复制已挂载 `/workspace` 或 `/in` 文件，输入/输出各最多八个、合计 20 MiB；输出只回工作区路径。单次最多 120 秒，stdout 48 KiB、stderr 8 KiB，用分页或 `--jq` 控制大小。
- `accepted` 只证明适配器完成，业务成功须看 `exit_code` 和 stdout。超时、输出截断或断线可能已有外部效果，保留 unknown，不能自动重放写入或换参数绕过回执。普通用户最终回答仍由 `session.reply` 发送，不重复用 CLI 发送。

## 群内 Bot 协作

双方应用发布应用身份 `im:message.group_at_msg.include_bot:readonly` 并订阅 `im.message.receive_v1`；双方绑定同一明确群，分别在 `peer_bots` 填当前接收应用范围内、经过核对的对方 Bot `ou_...`，不是 `cli_...` App ID，也不能复制对方应用查询自身得到的 open ID。可用当前应用查询群 Bot 或读取真实提及消息核对；`context.peer_bots` 返回本应用中已获准的 ID。成员名单不自动授权 peer，peer 名单也不授权人或其他群。

经理使用官方 `im +messages-reply`（先读 `--help`，不是不存在的 `schema im.message.reply`），回复 `context` 给出的实际平台消息，并在文本 `content.text` 中使用结构化 `<at user_id="ou_PEER">名称</at>`。新建无关联顶层 Bot 消息不会成为可接纳的委派。接收端须有同 owner、同群的已接收人类根消息；可从另一获准 Manager 收到该根，不要求人同时 @ 所有 Bot。

同一人类根消息下，每个有向 peer 对最多接纳一个不同消息；预算从现有入站事实计算，重启或退役不补充，删除整个 Session 也会删除其事实。平台重放先复用原幂等。Bot 输入标为 `source:"bot"` 并携带可信 `bot_origin`，不伪造用户或任务完成；不发进度卡，正常最终回复不自动提及 peer。需要回报时经理显式 reply/@，不自动回复回执或再委派成循环。

该入口沿用新渠道消息可替代当前 Manager Turn 的语义，没有新增 FIFO 或通信平台；Codex 执行会话独立管理。CLI 返回 message_id 只证明发送，必须核对对方真实事件、准入及回复才能声称协作成功。群协作与云文档的真实租户验收状态见[统一台账](pilot-development-plan.md)。

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
