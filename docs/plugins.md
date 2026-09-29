# Plugin

Anchor Plugin 以安装后的目录为唯一资产。安装器可接收社区 Plugin 来源并放入共享库；运行时只认 bundle 根目录 `plugin.json`，不记录来源格式或 `codex` 类型字段。来源格式只影响安装时怎样定位清单，其他文件和目录按来源原样保留。

```text
library/plugins/<id>/
├── plugin.json              # 安装器放在 bundle 根目录的入口清单
├── skills/<skill>/SKILL.md
├── commands/                 # 可选
├── agents/                   # 可选
├── hooks/                    # 可选
├── .mcp.json                 # 可选
└── 其他来源资源
```

Graph 的 AgentNode 仍只保存 Plugin 目录 ID。Anchor Library 校验根清单和声明的 Skill 路径，把已选择的 Plugin 原目录只读挂载到 `/plugins/<id>`，并将清单、MCP 配置和资源摘要记录在 Run。Agent 按需读取 Skill 与其他资源。Plugin 文件由安装流程或操作者维护；运行期间不要原地修改，恢复时资源摘要变化会拒绝静默继续。

当前兼容范围是根清单解析、Skill/资源读取、只读挂载和 MCP；Codex `hooks`、`commands`、`agents` 暂不支持。可通过 `POST /plugins/install` 安装 GitHub 仓库子目录，安装时将来源清单放到 bundle 根目录 `plugin.json`。MCP 通过 PydanticAI `MCPToolset` 接入 AgentNode：stdio server 在节点现有 Bubblewrap 边界中运行；HTTP/SSE 要求节点允许网络，可用环境变量配置 headers 或 OAuth。OAuth 可从 Plugin 面板显式授权，token 保存在 Anchor state 中。

stdio 工具发现和调用已在真实 Bubblewrap MCP server 测试中通过，UI 安装与只读 Skill/资源场景有浏览器端到端覆盖。仓库包含 Docmost 示例 Plugin，使用服务端环境变量 `DOCMOST_API_KEY` 作为 Bearer token；配置解析已覆盖，需提供有效 key 才能验证真实服务握手。OAuth 外部服务授权和真实社区 MCP provider 尚未端到端验证，因此不声明所有 MCP server 兼容；hooks、commands、agents 仍不执行。

## 企业微信

仓库中的 [`plugins/wecom`](../plugins/wecom/) 是企业微信自建应用的最小接入 Plugin。`server.py` 通过 stdio MCP 提供 `wecom_send_text`、`wecom_send_markdown` 和 `wecom_get_user`；凭证只从 `WECOM_CORP_ID`、`WECOM_AGENT_ID`、`WECOM_SECRET` 读取。`bridge.py` 是单独运行的 HTTP 回调桥，校验企业微信签名、解密消息，并向 `ANCHOR_WEBHOOK_URL` 转发为 Graph 输入；它不由 Agent 或 Anchor MCP 自动启动。

使用前配置企业微信应用的接收消息 URL、Token、EncodingAESKey，并在运行 MCP/回调桥的环境设置 `WECOM_TOKEN`、`WECOM_ENCODING_AES_KEY`、`ANCHOR_WEBHOOK_URL` 和 `ANCHOR_API_KEY`。当前测试覆盖本地 API 夹具、MCP JSON-RPC、AES 回调解密和 Anchor Webhook 转换；尚未使用真实企业微信凭证完成端到端验证。
