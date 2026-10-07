# Rust 平台交付目标与迁移边界

2026-10-06，用户确认将此前的 Rust Runtime Kernel 替代扩大为完整 Rust 服务端与官方工具/集成；保留现有 React/TypeScript WebUI。本文是目标决定，不是已实现声明。

同日后续决定：AgentNode 与 Pilot 统一迁移到 Goose + ACP，最终移除 io-harness/Rig；暂不实现预算控制、不将其作为迁移门槛。框架选型结束，实施与最短出口见 [Goose-only Runtime 迁移决定](goose-runtime-migration.md)。

## 目标

- 标准生产部署的 Anchor HTTP/CLI、Graph/Run、Session/Turn/Pilot、Library/授权、Scheduler、渠道协调及官方可执行工具均使用 Rust，不要求 Python 解释器或 venv。
- WebUI 保持原有实现和产品体验，由 Rust 服务提供；不因后端迁移重写前端。
- Graph 作者语言、Plugin 资源形态、工具名称及参数/结果语义保持；官方工具启动入口可以替换为 Rust binary。
- Docmost、企业微信和远程 MCP 是外部服务，不要求重写这些服务。第三方 Plugin 可继续声明其他语言依赖，但不属于标准 Rust 原生部署的必需闭包。
- Git、Bubblewrap、选用的外部服务和第三方工具依赖仍需显式部署；Rust 原生不等于无外部依赖。
- 标准 Agent 执行后端仅 Goose；发行闭包明确包含兼容 Goose binary，Host/Kernel 的普通依赖与构建验收须不含 io-harness/rig-agent/rig-core。旧路径只在迁移期间保留，不作为永久可选后端。

## 事实与依赖方向

Graph/Run/Artifact 继续由现有唯一 Rust Kernel 持有。平台 Session/Turn 只持有生命周期、请求身份、关联和事件投影，不复制 Run 状态，也不存另一套 Agent 消息/恢复记录。Pilot 和 AgentNode 共用固定 Goose 的 ACP 接入，原生会话和上下文由 Goose 持有；授权工具经 MCP 进入现有宿主边界。Library 持有安装、资源身份和宿主授权；渠道适配器通过小端口提交可信事件、接收回复及记录投递事实。

各上下文可在同一 Rust 服务内组合，不引入微服务、通用事件溯源或第二个 Runner。共享 HTTP/CLI 装配由主轨负责，外部适配依赖稳定的小契约。

## 迁移规则

- 新 Rust 平台 Session 首片使用独立的宿主 SQLite 业务事实存储，状态和活动事件在同一事务提交；不是 Harness 日志或 Agent 恢复引擎。
- 新存储不读取、改写或隐式接管 Python Session/Turn 数据。旧数据导入、未完成执行处理及生产切换须独立验收，不能让两套实现同时写同一事实。
- Session 所有者来自受信入口，不来自请求 body。普通 Pilot 保持既有操作员共享可见性，不能借迁移默改为每 key 一份；`responses-` 私有命名空间按受信 API key 隔离，不能把旧私有 Session 变成公开。无鉴权 loopback 是单一 local 所有者。
  开启鉴权后，原 loopback 的 local 私有记录保持隐藏，不随普通共享列表发布，也不自动被新 key 认领。
- 官方 Rust Plugin 包先输出到独立目录并验收；不在原 Library 的符号链接源上直接替换 manifest 或活跃资源。原生产 Plugin 不因开发期间的编译产物缺失而被破坏。
- 每个切片区分组件、组合、真实 Provider 与外部服务验收。小型测试 Graph 替代日常研究执行，但不替代最终业务内容验收。
- 生产 backend、数据迁移、真实发送/发布及回滚操作继续单独授权。

## 完成条件

在不安装 Python 的隔离发行环境中，使用既有 WebUI 和 Rust CLI 完成聊天、Graph、Plugin、计划、渠道、文件、重启恢复及权限反例；固定 release binary 与版本证据，完成旧数据策略和生产切换/回滚演练。只发布 Rust binary 或只通过一组测试不满足完整替代。
