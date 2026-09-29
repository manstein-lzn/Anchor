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

`bridge.py` 是被动入口，不由 MCP 启动。配置企业微信“接收消息”回调后运行它；它会校验签名、解密 XML，并把消息转换为 Anchor Graph Webhook 输入。

所需环境变量：

```text
WECOM_CORP_ID
WECOM_AGENT_ID
WECOM_SECRET
WECOM_TOKEN
WECOM_ENCODING_AES_KEY
ANCHOR_WEBHOOK_URL=https://anchor.example/v1/webhooks/graphs/wecom-assistant
ANCHOR_API_KEY
```

回调服务还支持 `WECOM_LISTEN_HOST`（默认 `127.0.0.1`）和 `WECOM_LISTEN_PORT`（默认 `8090`）。公网部署时应在前面配置 HTTPS 反向代理。
