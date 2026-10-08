# 接入 Anchor 企业微信助手

当前链路是原生 `anchor-wecom-gateway` → Rust Host 的 Session/Turn → 普通助手 Graph → Goose ACP 与授权 MCP 工具 → 网关投递。业务消息不经过 Pilot；Gateway 负责平台传输与投递账本，Host 负责用户隔离、Run、取消和会话事实。运行环境为 Linux，客户端可在其他电脑或手机上使用。

## 机器人和成员身份

在企业微信智能机器人管理入口选择 API 模式、长连接，取得 Bot ID 和 Secret。默认平台端点为 `wss://openws.work.weixin.qq.com`；群 Webhook 地址不能替代这组凭证。无需向公网开放本机 HTTP 回调，网关与 Host 通过本机端点交互。

使用实际私聊事件中的 `body.from.userid` 配置成员名单。它不是显示姓名、手机号或个人微信号，也不能未经核对当成自建应用通讯录账号。接入不会读取或代答本人已有的个人私聊。

## 部署配置

通过服务管理器提供配置环境；不要把凭证写入 Graph、Plugin 包或聊天。若使用部署的 EnvironmentFile，将其权限限制为服务用户可读；二进制不自动加载仓库 `.env`。

```dotenv
WECOM_BOT_ID=后台提供的机器人ID
WECOM_BOT_SECRET=后台提供的机器人Secret
ANCHOR_WECOM_USERS=已核实的成员userid
ANCHOR_WECOM_SEND_USERS=
ANCHOR_WECOM_GRAPH=wecom-assistant
ANCHOR_WECOM_REPLY_NODE=assistant
ANCHOR_API_KEYS=["至少32字节的随机密钥"]
ANCHOR_API_KEY=与服务白名单匹配的密钥
```

多位成员用英文逗号分隔；空名单拒绝所有成员，`*` 表示明确允许全部平台成员。主动发送名单为空时继承入口名单。`ANCHOR_API_KEYS` 也保护管理 API。修改启动配置后按部署流程重启，保持一个 Bot 只有一个网关。

按 [Rust 部署指南](rust-production-deployment.md) 安装 Host、固定版本 Goose、`anchor-wecom-gateway` 和 `anchor-wecom-tools`。通过现有 Library/Graph API 安装 [助手示例](../examples/graphs/wecom-assistant.json) 及原生 WeCom Plugin，确认回复节点挂载 `wecom`。独立应用 API 包不包含通道声明，不能替代完整机器人部署。

Host 使用 `ANCHOR_RUNNER_BUNDLE_ROOT`、`ANCHOR_RUNNER_CATALOG_ROOT`、`ANCHOR_RUNNER_LIBRARY_ROOT`、`ANCHOR_RUNNER_STATE_ROOT` 和 `ANCHOR_RUNNER_WORKSPACE_ROOT` 区分资源、持久事实与工作区。部署配置完成后启动 `anchor-runner-host serve`；监听地址由 `ANCHOR_RUNNER_LISTEN` 配置。不要同时启动另一个共享状态根的服务。

Host 的 ChannelSupervisor 监管 Plugin 声明的原生 Gateway，设置本机回调地址、私有控制 socket 和 descriptor。正常部署不手动启动第二个网关。Gateway 的独立配置和协议见 [原生 Gateway README](../rust/anchor-wecom-gateway/README.md)。

## 对话与投递

每条输入对应独立 Run，同一通道会话串行交接，不同用户隔离。新消息可取消旧 Run，待旧执行退出后接续原生 Goose 会话；上轮工作文件通过授权的只读 `/previous` 提供。已经发生的外部操作不会回滚，未知工具结果需要先核查现场，不能自动重放。

Goose 负责 Agent loop、Provider、原生历史和 compaction；Anchor 保存 Session/Turn、Graph/Run、产物和权限事实。原生会话文件与 Host 状态应按部署备份契约一起保存，不能把单个 JSONL 当作完整会话。

网关持久保存投递事实，相关平台 ACK 才算确认；超时、断线或重启后的未知结果不会自动重发。Host settlement 重试只同步账本，不再次发送平台消息。Graph 完成、调用已接纳、平台确认与收件人已读是不同状态。

## 业务工具与附件边界

显式挂载 Plugin 才能查询或操作业务系统。需要联网的 MCP 要求节点 `network=true`，仍受宿主授权；运行中不得原地修改已冻结资源。Bot ID/Secret 与自建应用的 Corp ID/Agent ID/Secret 是两套凭证。

- `wecom_send_message(userid, content)` 通过受限宿主控制通道主动发送 Markdown。仅在用户明确要求或已经授权的定时任务中调用，目标须在允许名单内；普通回复不重复调用发送工具。
- `wecom_attach_image(path)` 是受限回复节点工具，选择工作区或授权只读输入中的真实 PNG/JPEG。登记图片不等于平台成功投递；当前原生 Gateway 的公网富媒体回传尚未验收。
- 自建应用 MCP 由 `anchor-wecom-tools` 提供 `wecom_send_text`、`wecom_send_markdown` 和 `wecom_get_user`，另行配置 `WECOM_CORP_ID`、`WECOM_AGENT_ID`、`WECOM_SECRET`。未配置的可选 MCP 不影响基础机器人聊天。

Rust Host 支持可信入口提交规范化 Base64 附件，按 Run 冻结 SHA-256、大小与 MIME，并以 `/in/channel` 只读挂载。入口拒绝宿主路径、非法文件名、伪造类型和越界数据；Goose/provider 是否能理解图片需单独验证。

这份 Host 契约不等于官方媒体回调已接入。当前 Gateway 拒绝官方 image/file 及含媒体的 mixed 消息，尚未实现短时 URL 下载和 AES 解密；不声明旧文本提取、Office/PDF/OCR 或图片分析能力已在原生公网路径通过。业务需要这些能力时按独立切片验收，不通过模型提示词绕过边界。

成员名单控制助手入口；业务 MCP 使用操作员配置的凭证。对话隔离不能代替下游系统的逐用户数据授权。审批、借款与报销等 API 仍需按实际业务模板接入。

## Docmost 和验收

可在助手节点显式挂载 `docmost` 并开启授权网络，以服务环境中的 `DOCMOST_API_KEY` 访问知识库；原生附件上传工具见 [Docmost README](../rust/anchor-docmost-tools/README.md)。搜索、读取和页面变更均以当轮实际工具及权限为准，不声称本机配置已经部署成功。

常规运行验证使用 [确定性 Goose fixtures](runtime-contract-tests.md)，经过实际 Host、共享 Runner、Goose ACP 与授权 MCP，替换模型传输。Gateway 的本地传输测试为：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-wecom-gateway --all-targets --locked
```

真实账号的验收应分别检查基础收发、跨轮事实、双用户隔离、补充消息取消、进程重启以及允许名单。历史部署证据保留在开发台账；本轮不把旧实现或本地 fixtures 写成真实原生 Provider、WeCom、Docmost 或媒体业务已通过。
