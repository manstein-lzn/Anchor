# 接入 Anchor 企业微信助手

当前执行链路是企业微信智能机器人 WebSocket → Anchor Session/Turn → 普通助手 Graph → AgentNode/业务 Plugin → 企业微信回复。业务消息不调用 Pilot。Anchor 和网关运行在 Linux 即可，企业微信客户端可以在另一台 Windows 电脑或手机上；无需把客户端装在 Anchor 机器上，也不需要企业微信向本机开放公网 HTTP 回调。

## 1. 准备机器人和成员身份

在企业微信的智能机器人创建/管理入口选择 **API 模式、长连接**，取得机器人 **Bot ID 和 Secret**。不同企业版本的入口位置可能不同，以后台实际显示为准。本实现使用 [WeCom Python SDK](https://github.com/WecomTeam/wecom-aibot-python-sdk) 的默认 `wss://openws.work.weixin.qq.com`。普通群 Webhook 机器人地址不能替代这组凭证。

机器人收到私聊时，SDK 回调的 `body.from.userid` 会携带发送者的平台标识，Anchor 已将它保存到通道 Session 的 `channel.sender_id`；无需先找管理员才能取得已经与机器人聊过天的用户标识。这个值可能是不直观的字符串，不是显示姓名、手机号或个人微信号，也不能未经核对就当成自建应用通讯录接口使用的账号。在机器人管理中按实际提供的可见范围、添加/分享入口让指定同事打开私聊；对方发一条消息后即可取得其标识。当前尚未提供按姓名搜索已接触成员的联系人界面/工具。接入不会自动读取或代答你本人已有的私聊。

## 2. 根目录 .env

在 `/root/Anchor/.env` 中设置以下值。保留已有模型配置，不要将凭证发到聊天或提交到 Git。

```dotenv
WECOM_BOT_ID=后台提供的机器人ID
WECOM_BOT_SECRET=后台提供的机器人Secret
ANCHOR_WECOM_USERS=你的企业成员userid
# 可选：允许主动通知的成员；留空或不设置时继承上面的名单
ANCHOR_WECOM_SEND_USERS=
ANCHOR_WECOM_GRAPH=wecom-assistant
ANCHOR_WECOM_REPLY_NODE=assistant
# 自动监管时由 Anchor 设置；手动调试网关时才需要自己设置
# WECOM_CHANNEL_STATE=.local/demo/state/channels/wecom
# ANCHOR_CHANNEL_WEBHOOK_URL=http://127.0.0.1:8077/v1/channels/wecom/events
ANCHOR_API_KEYS=["同一个至少32字节的随机密钥"]
ANCHOR_API_KEY=同一个至少32字节的随机密钥
```

本工作区已生成并写入匹配的两项 Anchor API key，已安装 Graph；你只需填机器人 ID、Secret 和成员 userid。不要用上述示例字符串替换已生成密钥。多位成员用英文逗号分隔；空白拒绝所有成员，`*` 表示明确允许所有平台成员。建议先仅开放自己的 userid，再逐步加入同事。

`ANCHOR_API_KEYS` 同时保护 Anchor 管理 API，Web 管理端也需要使用其中一把密钥。所有这些配置在进程启动时读取，修改后需重启 Anchor 服务和网关。一个机器人只运行一个网关进程。

## 3. 安装、检查和启动

以下命令均从仓库根目录执行：

```bash
cd /root/Anchor
./.venv/bin/python -m pip install -e '.[channels,mcp]'
./.venv/bin/python plugins/wecom/setup.py --root .local/demo
./.venv/bin/python plugins/wecom/setup.py --root .local/demo --check
```

安装脚本保留已存在的 Graph 定义。`--check` 只检查本地配置，不显示凭证、不连接企业微信。当前服务的数据根目录是 `.local/demo`；如换目录，安装和服务启动必须使用同一个 root。

先通过你已有的进程管理方式停止旧 Anchor 服务，再在终端启动更新后的服务，避免两个进程占用 8077 或共享数据目录：

```bash
./.venv/bin/anchor-serve --root .local/demo --config examples/runtime.env.json --host 127.0.0.1 --port 8077
```

如果 `wecom` Plugin 已复制到 `<root>/library/plugins/wecom`，并且助手 Graph 的节点挂载了 `"plugins": ["wecom"]`，Anchor 会在服务启动后自动运行一个网关进程，并在服务退出时停止。不要再手动启动第二个相同 Bot 的网关；同一 Bot 只允许一个连接。网关主动向企业微信建立出站 WSS 连接，Anchor 和企业微信客户端可以部署在不同机器上。手动运行 `plugins/wecom/ws_gateway.py` 仅用于调试，需自行设置回调地址和状态目录。

## 4. 私聊验收

1. 从自己的企业微信打开机器人私聊，发送“你好，我的代号是 A17，请记住”。应看到处理中提示和助手回复。
2. 再发送“我的代号是什么”，应回答 A17。
3. 发起稍长任务后马上补充一句新要求：旧 Run 会取消，保存记录后新 Run 接续；旧答案不应作为业务回复迟到发出。旧回复流可显示“已根据你的补充继续处理”。
4. 加入第二位测试同事的 userid 并重启服务，各自发送不同代号，检查回答没有混入对方历史。
5. 服务重启后再次询问自己的代号，确认能接上原会话。

管理端可以查看 `wecom-assistant` 的 Runs。每轮 Run 位于 `.local/demo/workspaces/wecom-assistant/runs/channel-<turn-id>/`，Session 保存在既有会话存储中，模型步骤保存在每个 Run 的 `control/<node>/` 下。不要只移动 JSONL；框架的消息快照等配套文件也需要保留。

## 对话和工作语义

每条新输入对应独立 Run，同一用户同一会话串行交接，不同用户可并发执行同一 Graph。新消息取消当前工作，待旧执行退出后读取原生对话记录和新增输入；连续补充的消息不会因为模型尚未启动而丢失。被取消的请求重投不重新运行。已发生的外部操作不会回滚，工具结果不完整时下一轮应先核查业务状态。

每个节点按自己的会话身份加载最近可读的原生快照；上轮未执行该节点或初始化失败时，继续向前找有效记录。原始记录落盘保留；长对话进入模型时使用现有 Harness 滑动窗口压缩，因此不承诺无限历史逐字都放入每次模型请求。`/previous` 是上轮该节点产物或取消时保存的未完成文件的只读快照。工作区、Session、Run 和框架记录各自独立，不是外部业务的事务或 exactly-once 保证。

网关默认订阅 HTTP SSE，心跳保持连接，模型生成指定回复节点的 `summary` 时，约每 0.5 秒将最新正文更新到同一个企业微信回复流，结束时发送校验后的完整结果及图片。执行工具或模型思考期间不显示内部内容，可能仍只有处理提示；不保证每个 token 单独显示。旧 JSON 调用仍有 120 秒等待上限，断开 SSE 或超时不取消已接收的 Run。

平台重投同一事件不会再启动 Graph；已保存的回复可重试投递。账本按首次接收顺序阻止旧事件重新覆盖新回复，网关重启后同样有效。升级前缺少会话标识的历史待投递记录采取保守抑制，必要时补充一句新消息继续。平台回复窗口、主动发送范围和限频仍以真实企业账号验收为准；任意长任务没有自动完成通知，需任务明确调用主动发送工具。

## 挂载业务 Plugin

新安装的默认 Graph 已挂载 `wecom`，可以使用机器人通道工具；不会凭空执行报销、借款或查询企业数据。编辑 `.local/demo/workspaces/wecom-assistant/graph.json`，给 `assistant` 节点挂载 `library/plugins/` 中已登记的 Plugin；要启用自动 WebSocket 网关，至少挂载 `wecom`。需要联网的业务工具将该 Agent 的 `network` 设为 `true`；凭证继续放根目录 `.env`，通过 Plugin manifest 明确传入。运行中的 Graph 禁止修改。

图片、文件和混合消息保存在 `.local/demo/state/channels/wecom/events/<事件ID的哈希>/`，原件通过 `/in/channel` 只读挂载。UTF-8 文本/Markdown/CSV/JSON、PDF 文本层、DOCX 正文和 XLSX 单元格自动提取到 `input.attachment_content`；PNG/JPEG/WebP 经解码检查后通过框架原生 `BinaryContent` 传给模型。扫描 PDF 暂无 OCR，Office 图片/页眉页脚/嵌入对象不在自动提取范围；不支持、损坏或截断的文件会标注，不能当成完整读取。

每条消息最多 16 个附件、单个 20 MiB、合计 50 MiB；模型图片最多 8 张、单张 10 MiB/2000 万像素、合计 20 MiB；提取文本每文件 12000 字符、整条 48000 字符，PDF 最多 100 页。限制只约束本轮自动处理，不删除原件。

### 主动发送与图文回复

- `wecom_send_message(userid, content)` 由服务注入已挂载 `wecom` 的节点，通过本机控制 socket 委托唯一网关发送 Markdown。目标受 `.env` 的 `ANCHOR_WECOM_SEND_USERS`（默认继承入口名单）限制；用户需明确授权发送，不猜姓名对应 userid。不需要自建应用凭证或节点开网，平台 ACK 不等于已读。超时记为未确认，不自动重发。
- `wecom_attach_image(path)` 只向回复节点开放，从本节点工作区或只读输入选择真实 PNG/JPEG，终态与正文一起发送。最多 10 张、单张 10 MiB、base64 总量 14 MiB；拒绝符号链接、`.git` 和未授权宿主路径。主动通知首期仅 Markdown，暂不支持发送任意文件。
- 这些运行时工具在 Anchor 服务启动的 Graph 中可用，CLI 离线执行没有网关入口。同一平台当前只允许一个 Graph 声明通道，可在该 Graph 中使用多节点和手动/定时运行。

四项能力的人工验收：发送一张图片并询问内容；发送带已知代号的 TXT/DOCX/PDF 并要求提取；要求写一段较长答复观察正文逐步更新；要求把原图附回。主动发送可先说“请通过主动发送工具给我本人发送一条测试通知”，助手可从可信通道输入取得你的 userid；给同事发消息需先确认真实 userid、允许名单及具体内容。

已有 Plugin 如果是复制安装，`setup.py` 不会覆盖它；需将仓库 `plugins/wecom` 的新版本同步到数据根目录对应 Plugin，保留自行修改的配置。已有 Graph 也不会自动改写，确认回复节点挂载 `wecom` 即可。

同一个 `wecom` Plugin 还提供自建应用身份的文本/Markdown 发送和成员查询 MCP 工具。它需要另外的 `WECOM_CORP_ID`、`WECOM_AGENT_ID`、`WECOM_SECRET`，与智能机器人的 Bot ID/Secret 是两套凭证；基础私聊只需要 Bot ID/Secret，缺少应用凭证时 Anchor 会跳过这些 MCP 工具。企业微信审批查询、报销/借款提交、定时业务 Graph 需要按你实际的审批模板另行接入，当前尚未提供。

成员允许名单控制谁可以进入助手；已挂载 Plugin 使用运营者配置的凭证。对话隔离不等于业务系统已提供逐用户数据权限，正式向同事开放业务操作前，需要在相应 Plugin/API 内实现用户权限和授权校验。

## 验证边界

已用真实 provider 通过 HTTP → 普通 Graph → 沙箱中的本地 stdio MCP、双用户并发、事件去重和实际服务进程重启后的历史/文件读取，证据：`.local/wecom-graph-proof/20260930T150216/evidence.json`。消息打断、连续补充和故障回退有本地自动回归；实际 SDK 的认证、回复和断线重连通过本地 WebSocket 服务验证（测试仅对回环 ws 连接移除 SDK 强制 TLS 参数）。此前基础链路全量 359 项通过；本轮四项能力合入后全量 412 项通过。2026-09-30 用户发起的首条真实企业微信私聊已验收：普通 Graph 完成、176 字符答复与 Graph 输出一致、平台确认投递，约 5.80 秒，证据 `.local/wecom-graph-proof/20260930-live-private-chat/evidence.json`。这证明当前账号的基础收发链路；同次检查后续共三轮正常完成并确认投递，原生历史依次包含 1/2/3 条用户输入，确认跨轮加载。指定事实记忆问答、连续消息打断、多用户、异常回复窗口和财务审批仍需分别实测。


四项新增能力的隔离真实模型验收：`.local/wecom-capability-proof/20260930T133215/evidence.json`。当前模型接收有效图片，普通 Graph 读取图片/TXT/DOCX，正确输出红色、ALPHA-47、128.50 CNY，调用图片工具附回字节一致的原图；HTTP SSE 在 Run 完成前收到多次正文，第二轮保留上述事实。实际 Python SDK 对本地 WebSocket 验证主动发送帧/ACK、持续正文和最终 `msg_item`。这些证据不等于四项能力已在企业微信公网逐项验收，本轮没有向真实成员发送测试消息。


### 本机 Docmost 能力

当前本机 `wecom-assistant` 的 assistant 节点已同时挂载 `wecom` 和 `docmost`，角色 `network=true` 以访问 Docmost HTTP MCP。凭证仍使用根目录 `.env` 中既有的 `DOCMOST_API_KEY`。下一条消息开始即可请求搜索知识库、读取周报，或明确指定页面进行创建/更新；实际权限由这枚 Docmost API key 决定。该改动是本机已安装 Graph 的配置，不强制所有新安装的企业微信助手都依赖 Docmost。真实工具发现和周报读取已验证，具体用户对话可直接从企业微信发起。
