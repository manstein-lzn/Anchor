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

`bridge.py` 是旧版自建应用 HTTP 回调入口，不由 MCP 启动。智能机器人应运行 `ws_gateway.py`：它使用官方 `wecom-aibot-python-sdk` 建立 WebSocket 长连接，持久化事件去重，并将规范化事件 POST 到 `ANCHOR_CHANNEL_WEBHOOK_URL`。回调返回 `{"text":"..."}` 或 `{"reply":"..."}` 时，适配器会通过企业微信流式回复发送结果。

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

WebSocket 还需要：

```text
WECOM_BOT_ID
WECOM_BOT_SECRET
WECOM_CHANNEL_STATE=.local/wecom-channel
ANCHOR_CHANNEL_WEBHOOK_URL=https://anchor.example/v1/channel/events
```

三个独立入口启动时会从当前工作目录读取 `.env`。回调服务还支持 `WECOM_LISTEN_HOST`（默认 `127.0.0.1`）和 `WECOM_LISTEN_PORT`（默认 `8090`）。公网部署时应在前面配置 HTTPS 反向代理。
