---
name: wecom
description: 企业微信机器人私聊、主动通知、附件分析和图文回复；可选自建应用 API
---

# 企业微信

以本轮实际注册的工具为准。挂载此 Plugin 后，Anchor 服务维护唯一的机器人 WebSocket 连接；普通助手 Graph 处理消息，不经过 Pilot。凭证统一由根目录 `.env` 配置，不要在参数、文件或回复中复制密钥。

## 机器人长连接工具

只需 `WECOM_BOT_ID`、`WECOM_BOT_SECRET` 及 Anchor 通道配置，不需要自建应用凭证，也不要求节点 `network=true`。工具通过宿主受限通道委托现有网关；节点拿不到机器人密钥。

- `wecom_send_message(userid, content)`：主动发送 Markdown 给明确指定的企业成员。仅在用户明确要求通知、转发或已授权的定时任务中调用；普通私聊直接在最终 `summary` 答复，不重复调用发送工具。目标必须是可信上下文中的真实 userid，不猜姓名对应账号，不支持群或 `@all`。接收人受 `ANCHOR_WECOM_SEND_USERS` 限制，未设置/留空时继承 `ANCHOR_WECOM_USERS`。返回 `accepted` 表示平台确认接收，不等于已读。超时或结果不确定时如实说明，不自动重发。
- `wecom_attach_image(path)`：仅指定回复节点提供。把本节点 `/workspace` 或已挂载只读输入（如 `/in/channel`、`/previous`）中的真实 PNG/JPEG 附到最终答复；不会立即发送。不得使用宿主路径、符号链接或 `.git`。每张最多 10 MiB、整轮最多 10 张且 base64 合计不超过 14 MiB；重复图片去重。正文写入 `summary`，图片由网关在终态一并发送。不是任意文件发送工具。

主动发送工具目前仅在 Anchor HTTP 服务启动的 Graph 中注入；离线 CLI 没有网关控制入口。同一平台的通道目前只绑定一个 Graph，可在这个 Graph 内使用多节点、手动或定时触发。

## 附件与回复

图片、文件和混合消息下载解密后保存在 `state/channels/wecom/events/`；本轮 `input.channel.attachments` 给出只读 `/in/channel` 路径。原件始终保留。

`input.attachment_content` 提供 UTF-8 文本、PDF 文本层、DOCX 正文和 XLSX 单元格的有界提取结果；扫描 PDF 没有 OCR，Office 图片/嵌入对象不算已读取。遇到 `Not read` 或截断提示，明确指出范围，不能声称读完。PNG/JPEG/WebP 经实际解码验证后通过模型原生图片输入传入；是否理解取决于当前模型，不能把文件存在当成视觉成功。

面向用户的正文放在结构化 `summary` 中；生成期间仅该正文持续更新，不输出内部思考、工具参数或其他节点内容。执行工具时可能只有处理提示；最终结果可以修正先前预览。超出平台 20480 UTF-8 字节的文字明确截断，全文仍在 Anchor Run 记录。

每位用户独立维护历史；新消息取消旧 Run 并接续。被打断前已发生的外部操作不会撤销，下一轮先核查不确定状态。若图片请求失败且未获得模型响应，下轮不自动重发该图片，原生记录不变；需要时请用户重发。

## 可选自建应用 MCP

另行配置 `WECOM_CORP_ID`、`WECOM_AGENT_ID`、`WECOM_SECRET` 后才注册以下工具；缺失时跳过这组 MCP，不影响上述机器人能力：

- `wecom_send_text` / `wecom_send_markdown`：以自建应用身份给成员、部门或标签发送消息。
- `wecom_get_user`：按 userid 查询成员资料。

应用发送需提供 `touser`、`toparty`、`totag` 至少一项；网络工具需节点 `network=true`。应用 API 工具与智能机器人通道分别使用原生二进制。审批、借款、报销 API、个人账号私聊接管、语音、欢迎语及卡片事件尚未接入。

操作员配置：`ANCHOR_WECOM_GRAPH`、`ANCHOR_WECOM_REPLY_NODE`、`ANCHOR_WECOM_USERS`、可选 `ANCHOR_WECOM_SEND_USERS`；内部 `ANCHOR_API_KEY` 对应服务白名单 `ANCHOR_API_KEYS` 的至少 32 字节密钥。网关状态目录、回调地址和控制 socket 由 Anchor 设置；不要同时手动启动第二个 Bot 网关。完整接入和验收见仓库 `docs/wecom-assistant.md`。
