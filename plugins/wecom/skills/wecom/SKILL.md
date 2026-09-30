---
name: wecom
description: 企业微信消息联动
---

# 企业微信

这个 Plugin 的 MCP 工具会以当前企业微信自建应用身份调用 API：

- `wecom_send_text`：向成员、部门或标签发送文本。
- `wecom_send_markdown`：向成员、部门或标签发送 Markdown。
- `wecom_get_user`：按成员 userid 查询成员资料。

发送消息时至少提供 `touser`、`toparty`、`totag` 之一。不要向工具参数或聊天内容写入 `corpsecret`。

`bridge.py` 是旧版自建应用 HTTP 回调入口，不由 MCP 启动。智能机器人由 Anchor 服务根据本 Plugin 的 `channel.json` 自动监管 `ws_gateway.py`；它使用官方 `wecom-aibot-python-sdk` 建立 WebSocket 长连接，持久化事件去重，并将规范化事件 POST 到 Anchor 的 `/v1/channels/wecom/events`。图片、文件和混合消息先下载到 `state/channels/wecom/events/`，Graph 通过只读 `/in/channel` 读取。回调返回 `{"text":"..."}` 或 `{"reply":"..."}` 时，适配器会通过企业微信流式回复发送结果。手动运行网关仍可用于单独调试，但不要和 Anchor 自动监管同时启动两个相同 Bot 连接。

凭证统一配置在仓库根目录 `.env`（shell 已设置的同名环境变量优先）。MCP 应用 API 和旧版回调桥按需使用：

```text
WECOM_CORP_ID
WECOM_AGENT_ID
WECOM_SECRET
WECOM_TOKEN
WECOM_ENCODING_AES_KEY
ANCHOR_WEBHOOK_URL=https://anchor.example/v1/webhooks/graphs/wecom-assistant
ANCHOR_API_KEY
```

WebSocket 自动监管还需要：

```text
WECOM_BOT_ID
WECOM_BOT_SECRET
WECOM_CHANNEL_STATE=由 Anchor 自动设置为 <root>/state/channels/wecom
ANCHOR_CHANNEL_WEBHOOK_URL=由 Anchor 按监听端口自动设置
```

Anchor 服务启动时从根目录 `.env` 读取凭证，并把必要的通道连接配置注入受监管进程。公网部署时应在前面配置 HTTPS 反向代理；企业微信 SDK 长连接本身由网关主动出站建立。

`ANCHOR_API_KEY` 必须与 Anchor 服务端 `ANCHOR_API_KEYS` JSON 数组中的一把密钥相同，至少 32 字节。当前通道直接运行 `.env` 指定的普通助手 Graph，业务消息不经过 Pilot。配置 `ANCHOR_WECOM_GRAPH`、`ANCHOR_WECOM_REPLY_NODE` 和 `ANCHOR_WECOM_USERS`（企业成员 userid 允许名单）；每位用户独立维护历史，同一用户的新消息会取消旧 Run，读取原生工作记录和未完成文件后接续。挂载此 Plugin 会启用 WebSocket 通道；若还要使用发送/成员查询 MCP 工具，再配置 `WECOM_CORP_ID`、`WECOM_AGENT_ID`、`WECOM_SECRET`，三者缺失时仅跳过 MCP，不影响智能机器人通道。联网工具需要 Agent `network=true`。业务审批 API 尚未提供。完整接入步骤见仓库 `docs/wecom-assistant.md`。
