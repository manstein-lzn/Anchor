# 企业微信 SDK 能力使用核查

> 历史调研：结论限于当时检查的版本和边界，当前实现与验收见开发台账。

核查日期：2026-09-30。结论：基础对话、主动 Markdown 发送、真实附件处理、持续正文和图文回复已接入；语音转写输入、欢迎语、卡片和反馈事件仍未接入。新增四项经本地 SDK 协议与隔离真实 provider 验证，真实企业微信平台逐项验收尚未完成。这些 SDK 接口使用机器人 Bot ID/Secret，不以补齐自建应用凭证为前提。接口存在不代表当前企业的权限、目标成员范围、频率限制已经实测。

## 核查依据

- 安装版本：`wecom-aibot-python-sdk==1.0.2`（`aibot/client.py`、`message_handler.py`、`types.py`、`ws.py`）。
- 上游 [README/API 矩阵](https://github.com/WecomTeam/wecom-aibot-python-sdk/blob/6bcb59a9a636c566f4c6ea5268b228e3def1611a/README.md)、[Node/Python 对照](https://github.com/WecomTeam/wecom-aibot-python-sdk/blob/6bcb59a9a636c566f4c6ea5268b228e3def1611a/COMPARISON.md)、[消息示例](https://github.com/WecomTeam/wecom-aibot-python-sdk/blob/6bcb59a9a636c566f4c6ea5268b228e3def1611a/examples/basic.py)。固定到本次取得的上游 commit，避免把后续版本能力混入当前结论。
- Anchor 网关（历史路径 `../plugins/wecom/ws_gateway.py`）、Graph 通道入口（历史路径 `../src/anchor/channel/assistant.py`）、通道监管（历史路径 `../src/anchor/channel/supervisor.py`）、自建应用 MCP（历史路径 `../plugins/wecom/server.py`）。
- 现有 `tests/test_wecom_plugin.py`、`tests/test_channel_gateway.py` 及台账中真实文本私聊证据。本轮未向真实成员发测试消息、未调用审批业务 API、未升级 SDK。

## 能力矩阵

“已接”描述代码路径；真实平台验收另列，不能以单元测试或静态工具清单替代。

| SDK 能力 / 公开入口 | Anchor 使用状态 | 实际边界 / 验收 |
| --- | --- | --- |
| `connect` / `disconnect`、Bot 认证 | 已接 | 服务监管单一连接，真实文本私聊已通过 |
| 心跳、指数退避重连、连接状态 | 已使用 SDK；额外补正常关闭后的重连 | 配置无限重连，用 `is_connected` 和生命周期接口；本地 SDK 断线重连测试已有，非全部公网故障验收 |
| 同一 req_id 串行回复、ACK 等待 | 间接使用 SDK | `await reply_stream` 等待回执；Anchor 另存事件和待投递回复；不代表远端用户已读 |
| `message.text` | 已接（通过通用 `message` 订阅） | 提取 `text.content`，真实普通 Graph 对话、跨轮历史已有证据 |
| `message.image` / `message.file`、`download_file` | 已接 | SDK 下载解密后落盘；有界文本/PDF/DOCX/XLSX 提取、原生 BinaryContent 图片输入；真实 provider 图片/TXT/DOCX 已验证，公网媒体下载待验收 |
| `message.mixed` | 已接 | 提取文本/附件，接受纯文本 mixed；逐项错误及截断显式标记 |
| `message.voice` | 未接 | 上游示例读取 `voice.content`（平台转写文字）；Anchor 未提取，且后端消息类型白名单拒绝 voice。无需先自建语音识别才能消费该转写字段 |
| `reply_stream` 文本 / Markdown | 已接持续正文 | 原生 ModelRequestNode.stream 观察结构化 summary，经 TurnStore/SSE、约 0.5 秒节流更新；不公开思考/工具参数，终态以校验结果为准 |
| `reply_stream(msg_item=...)` 图文输出 | 已接 | 指定回复节点 `wecom_attach_image` 选择 PNG/JPEG，终态随正文带 base64+MD5；真实模型工具和本地 SDK 帧已验证，平台显示待验收 |
| `send_message(chatid, body)` | 已接 Markdown | `wecom_send_message` → 本机鉴权 socket → 唯一网关；允许名单、工具调用去重、ACK、未知结果不重发；本地 SDK 协议通过，目标范围待平台实测；模板卡片未接 |
| `event.enter_chat` + `reply_welcome` | 未接 | 无事件订阅；SDK 文档要求事件后 5 秒内回复，不能先等一个长 Graph 执行完成 |
| `reply_template_card` | 未接 | 无结构化卡片输出，现有最终回复只有文本 |
| `reply_stream_with_card` | 未接 | 未接文本与卡片组合回复 |
| `event.template_card_event` + `update_template_card` | 未接 | 无按钮事件到可信 Session/Graph 的映射；SDK 文档要求使用对应事件 req_id，在 5 秒内更新 |
| `feedback` 参数 + `event.feedback_event` | 未接 | 未设置反馈关联，也不采集用户反馈；采集反馈不等于实现 RSI |
| 通用 `event` 回调 | 未接 | 只订阅 `message`，规范化只接受 `aibot_msg_callback`，业务事件不会进入 Graph |
| `connected` / `authenticated` / `disconnected` / `reconnecting` / `error` | 部分使用 | 当前只监听 authenticated、reconnecting、error；没有完整的通道状态查询/页面展示 |
| 自定义 Logger | 未使用扩展点 | 当前使用 SDK 默认日志及 Anchor 的少量 JSON 错误记录，未统一为通道日志策略 |
| `reply` 通用回复、`api`、`run` 便捷启动 | 已有间接使用或无需单独接入 | reply_stream 内部调用 reply；download_file 内部调用 api。Anchor 自己管理 asyncio 生命周期，不需要为 API 覆盖率改用 run() |

## 对当前助手的影响

1. Bot 凭证已能建立长连接。自建应用 MCP 的 `wecom_send_text`、`wecom_send_markdown`、`wecom_get_user` 是另一条 API 路径；当前缺少自建应用三项凭证使该 MCP 未加载，不代表 SDK 主动推送缺凭证。
2. 当前节点 `network=false` 不影响宿主网关收发机器人消息。节点内调用自建应用网络工具则需要相应网络权限；不能简单把整个 Agent 开网当成补齐 SDK 接线。
3. 原始文件落盘不等于模型获得视觉输入；回复文本也不等于能发送任意文件附件。本次 SDK 矩阵没有独立的任意文件上传/发送或视频能力，不将通用 dict 参数视为平台支持证明。
4. 通讯录、审批、个人账号聊天接管不在此机器人 SDK 矩阵内；审批卡片样式不等于企业微信原生审批接口。
5. 用户已排除群场景，不把群聊接入作为缺口或近期任务。

## 本轮验证与剩余范围

用户选择的四项已实现。隔离真实 provider 证据：`.local/wecom-capability-proof/20260930T133215/evidence.json`，覆盖普通 Graph 的附件理解、原图回复、提前收到正文和跨轮续聊。`tests/test_channel_delivery.py` 还覆盖真实 Python SDK 的主动发送/ACK/图文协议、越权目标、断线重放、取消与附件接纳顺序。没有向真实企业成员实发，本地 WebSocket 平台为显式夹具。

仍待：四项企业微信公网验收；按需接语音转写、欢迎语、卡片及反馈；通道状态展示；独立审批业务 API。任意文件上传/发送在较新 Node SDK 的接口不应算作固定 Python 1.0.2 已支持的能力。
