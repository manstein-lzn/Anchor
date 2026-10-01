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

MCP 工具集默认使用 PydanticAI 的 `defer_loading()`。Agent 初始只看到自动注入的 `search_tools` 和其他核心工具；模型需要某项能力时先搜索，框架再揭示匹配工具的定义。支持原生工具搜索的 provider 会使用 provider 的延迟标记，其他 provider 使用框架的本地关键词回退。MCP 连接和沙箱边界不因此改变，工具调用仍会写入原生运行记录。少量必须始终可见的宿主控制工具不通过 MCP 延迟。

### CodeMode

CodeMode 是 AgentNode 的内部执行优化，不是 Graph 配置项。安装 `codemode` 可选依赖后，普通 AgentNode 自动使用 Harness 的 `tools='all'` 模式；模型可以在一次 `run_code` 中批量调用已发现的 Plugin/MCP 工具并整理结果，用户不需要填写工具名、Monty 限制或额外 JSON。没有安装 Monty 时自动回退到普通工具调用。

PydanticAI/Harness 会自动保留 Tool Search、完成控制和其他框架工具的原生路径；Anchor 的 Bash 工作区工具也保持原生。CodeMode 不获得宿主文件、环境变量、时钟或远程 sandbox 访问，已有 NodeSandbox 和 Graph 权限边界继续生效。CodeMode 的嵌套调用元数据保留在原生 trace 中。

Plugin 开发者不需要为每个工具编写适配层。工具仍按普通 PydanticAI/MCP 工具注册；结构化返回 schema 会让模型在代码中获得更准确的类型提示，但缺少 schema 也不会阻止工具运行。

stdio 工具发现和调用已在真实 Bubblewrap MCP server 测试中通过，UI 安装与只读 Skill/资源场景有浏览器端到端覆盖。仓库包含 Docmost 示例 Plugin，使用服务端环境变量 `DOCMOST_API_KEY` 作为 Bearer token；配置解析已覆盖，需提供有效 key 才能验证真实服务握手。OAuth 外部服务授权和真实社区 MCP provider 尚未端到端验证，因此不声明所有 MCP server 兼容；hooks、commands、agents 仍不执行。

## 企业微信

仓库中的 [`plugins/wecom`](../plugins/wecom/) 现在包含两种入口：`server.py` 通过 stdio MCP 提供 `wecom_send_text`、`wecom_send_markdown` 和 `wecom_get_user`；`ws_gateway.py` 使用官方 `wecom-aibot-python-sdk` 建立智能机器人 WebSocket 长连接，规范化消息并向 Anchor 通道端点转发。`channel.json` 是服务级通道声明；当 Graph 节点挂载此 Plugin 时，Anchor 的 `ChannelSupervisor` 自动启动并监管网关进程，不在每个 Graph Run 内重复连接。图片、文件和混合消息会先下载到通道状态目录，再以只读附件挂载给 Graph。`bridge.py` 保留为自建应用旧版 HTTP 回调桥，负责签名校验、XML 解密和 Graph Webhook 转换。

凭证与连接配置统一写在根目录 `.env`，显式 shell 环境变量优先。智能机器人长连接负责平台收发，Anchor 按允许名单把私聊接到指定普通 Graph；Graph 节点使用既有 Plugin 机制查询或操作业务系统。每个用户独立维护原生历史，新消息可以取消旧 Run 并接续。默认 Graph 不挂自建应用 MCP，因此基础聊天仅需 Bot ID/Secret，无需 Corp ID/Agent ID/Secret。安装、自检和接入步骤见 [企业微信助手接入](wecom-assistant.md)。真实 provider + 本地 Graph/Plugin 已验证，真实企业微信公网私聊待用户凭证后验证。
