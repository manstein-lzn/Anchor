# Rust Runtime 迁移边界与当前收口状态

本文记录 Rust Runtime 从实验切片进入产品对齐后的当前边界。历史冻结决定仍保留在开发台账中，但不再作为当前路线解释。

## 当前结论（2026-10-06）

Rust 已经可以作为 Anchor 的共享 Runtime Kernel 和独立二进制 Runtime 使用。Graph、Run、Artifact、Sandbox、Plugin/MCP、io-harness AgentNode、Provider transport、fanout/join、Graph call wait/detach、附件/图片输入、恢复和基础平台 HTTP 接线均已实现，并有相应的 Rust、Python、Web 或真实 provider 证据。

这还不等于 Python Anchor 平台已经被替换。当前生产入口仍由 Python `serve.py`、Scheduler、Library、Session/Turn、SSE 和企业微信网关协调；显式 Rust backend 可以把 Graph/Run/Artifact 等执行职责委派给 Rust Host。实现归属和未完成边界以 [当前架构](architecture.md) 为准，逐项验收以 [开发计划与验收台账](pilot-development-plan.md) 为准。

## 已达到的交付边界

- 可从不含 Python Runtime 源码的发行目录启动 Rust Host，执行 Graph、生成 Artifact，并在进程重启后继续同一 Run。
- 原 Graph/Plugin 可以通过 bundle 进入 Rust admission；Plugin 外部工具仍可使用 Python、Node 等既有生态。
- io-harness 是 Rust AgentNode 的执行、上下文、压缩和单节点恢复基础；Rig 只提供模型传输适配。
- Rust Host 已覆盖 Graph CRUD、Run 触发/查询/控制、Artifact、时间线、文件附件、图片输入、Plugin 绑定和部分 Session/Graph call 接线。
- 企业微信入站文字、附件、图片和图文回复已有真实公网验收；这证明 Rust Run 可以被真实入口驱动，但在线整个平台仍保持 Python backend。

不同 Linux 发行版兼容性按用户决定暂不作为本阶段范围。Provider 网络延迟占主导的场景也不预先宣称 Rust 更快；当前可证明的价值是二进制交付、依赖收敛、可恢复和可复现的运行事实。

## 仍然阻止“完全替代 Python 平台”的事项

1. **业务 Graph 的完整验收**：深度学术研究、标准 18 节点 RSI 和周报仍缺完整 Rust 原图闭环证据。已有 admission、局部工具或受控 Graph 证据不能替代完整 provider、产物、恢复和业务结果验收。
2. **平台职责迁移**：Scheduler、Library/Plugin 管理、Session/Turn、SSE 摘要流、企业微信协调和 EventLedger 仍由 Python 持有。Rust backend 接线不等于这些事实所有权已经迁移。
3. **产品调用体验**：`call.session` 的基础 wait/detach 已接入，但摘要增量、嵌套/并行 Graph call、OAuth 执行授权和完整前台/后台产品闭环仍未完成。
4. **生产切换**：还需要在保留现有数据和凭证的前提下完成单一生产 backend 切换、旧 Run/Session 兼容、Scheduler/网关接管、重启恢复和回滚验证。
5. **原 Graph 无修改执行契约**：Rust bundle 要求显式 Plugin 绑定；Responses 工具名必须符合 provider 合法字符集；HTTP MCP 还受节点网络授权约束。需要把这些规则收敛为正式作者/部署契约，避免每个 Graph 在隔离副本中手工适配。

## 推荐收口顺序

先完成标准 RSI 和周报的无外部发布完整验收，再补齐 Plugin 管理、Session/Turn/SSE 和 Scheduler 的 Rust 宿主职责，最后做一次生产 backend 切换。每一步都要分别记录组件通过、组合通过、真实 provider 通过和生产切换通过，不能用其中一个状态代替另一个。

## 历史决定

2026-10-04 曾冻结完整 R8/R9 平台迁移，只保留二进制 Runtime 交付评估；2026-10-05 用户将授权扩大为保持原 Graph/Plugin 和产品体验的 Rust Runtime 替代。此前的冻结记录、实验限制和证据仍在 `docs/pilot-development-plan.md` 的历史条目中，作为决策背景保留，不再作为当前完成状态。
