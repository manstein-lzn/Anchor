# Rust Runtime 迁移边界与当前收口状态

本文记录 Rust Runtime 从实验切片进入产品对齐后的当前边界。历史冻结决定仍保留在开发台账中，但不再作为当前路线解释。

2026-10-06 确认的新目标是完整 Rust 服务端和官方工具/集成，保留现有 WebUI；见 [交付决定](rust-platform-target.md) 与 [开发顺序](rust-platform-development-plan.md)。该目标不代表本文记录的平台缺口已经完成。

## 当前结论（2026-10-06）

Rust 已经可以作为 Anchor 的共享 Runtime Kernel 和独立二进制 Runtime 使用。Graph、Run、Artifact、Sandbox、Plugin/MCP、io-harness AgentNode、Provider transport、fanout/join、Graph call wait/detach、附件/图片输入、恢复和基础平台 HTTP 接线均已实现，并有相应的 Rust、Python、Web 或真实 provider 证据。2026-10-06 又完成了标准 18 节点 RSI 的原图受控闭环，以及标准周报原图的真实 provider 无外部发布闭环；这两项证据分别证明 Rust 执行契约和安全发布边界，不能替代真实 RSI 内容质量或 Docmost 生产发布验收。

这还不等于 Python Anchor 平台已经被替换。生产 backend 默认仍是 Python；`ANCHOR_RUNTIME_BACKEND=rust` 是显式 opt-in。该模式下计划 CRUD、tick 与 timeline 投影已委派给 Rust Host 并复用既有 `state/schedules.json`，而 Python legacy backend 仍由 Python Scheduler 管理；Library 安装/授权、Session/Turn、SSE 和企业微信网关仍由 Python 协调。实现归属和未完成边界以 [当前架构](architecture.md) 为准，逐项验收以 [开发计划与验收台账](pilot-development-plan.md) 为准。

## 已达到的交付边界

- 可从不含 Python Runtime 源码的发行目录启动 Rust Host，执行 Graph、生成 Artifact，并在进程重启后继续同一 Run。
- 原 Graph/Plugin 可以通过 bundle 进入 Rust admission；Plugin 外部工具仍可使用 Python、Node 等既有生态。
- 标准 Rust Host/Kernel 默认 Goose，通过 ACP/MCP 接入，不含 io-harness/Rig 正常构建依赖；后两者仅保留在显式 legacy 开发回归 feature。Goose AgentNode、Pilot 续聊与普通 Pilot 原生提问/删除确认已有 A114–A117 证据；MCP 图片结果已取得真实 Goose/确定性 Provider 的传输与恢复证据，不冒称真实视觉模型通过。压缩、完整媒体输入/渠道/图片 UI 和 Graph/channel Session 组合仍需验收。
- Rust Host 已覆盖 Graph CRUD、Run 触发/查询/控制、Artifact、时间线、文件附件、图片输入、Plugin 绑定和部分 Session/Graph call 接线。
- 企业微信入站文字、附件、图片和图文回复已有真实公网验收；这证明 Rust Run 可以被真实入口驱动，但在线整个平台仍保持 Python backend。
- 标准 `examples/graphs/weekly-work-report.json` 已在隔离 reject-only Docmost Plugin 下通过真实 provider 执行，原 Graph 字节、七个 Artifact 和安全拒绝事实均有证据；未触发 Docmost 生产写入。
- 标准 `examples/graphs/rsi.json` 已在受控本地 Provider 下通过完整 18 节点 fanout/join、反馈、评审、gate 和 publish；Rust Git 兼容投影已绑定 native Artifact 身份。两次隔离真实 RSI 尝试均未形成业务内容通过证据：第一次 143 次 DeepSeek Flash 请求后因 Responses body decode error 终止；第二次运行 900 秒、155 次请求后停止，Run 仍记录为 running，六个上游节点和六项 Artifact 已提交但 audit fanout 未收束。第二次执行进程与运行期间重建的 release binary inode 不同，构建身份无法确认。两个 Run 均未发布或发送业务消息，真实内容验收未通过（A105）。
- 显式 Rust backend 下的计划 CRUD/tick/timeline 投影由 Host 接管，沿用既有计划文件；双服务浏览器证据见 A104。验收在 `TZ=UTC` 环境进行，证明 Host 停机期间过期的 once 计划重启后 `enabled=false`、Run 数不增加并投影为 `missed_downtime`；没有覆盖 DST 或非 UTC 时区。OAuth route 测试只证明 POST 留在 Python handler、没有 Rust proxy。上述局部接线不是生产 backend 切换，也不表示平台其他 Scheduler/Session/OAuth 职责迁移。

不同 Linux 发行版兼容性按用户决定暂不作为本阶段范围。Provider 网络延迟占主导的场景也不预先宣称 Rust 更快；当前可证明的价值是二进制交付、依赖收敛、可恢复和可复现的运行事实。

## 仍然阻止“完全替代 Python 平台”的事项

1. **业务 Graph 的完整验收**：标准周报已经完成无外部发布的真实 provider 闭环；标准 RSI 仅完成受控 Rust 原图闭环，真实内容验收仍缺；深度学术研究仍缺完整 Rust 原图闭环。已有 admission、局部工具或受控 Graph 证据不能替代真实 provider、产物、恢复和业务结果验收。
2. **平台职责迁移**：Scheduler 仅在显式 Rust backend 下把计划 CRUD/tick/timeline 子路径交给 Rust Host；Python legacy backend 仍由 Python 管理，生产 backend 默认仍为 Python。直接 Rust Host 已有独立普通 Pilot 的 Session/Turn/SSE、Graph 变更与 Run 控制切片，但完整 Graph/渠道会话、摘要流、企业微信协调、EventLedger 及 Library/Plugin 安装与授权仍待原生接管。Rust backend 下的 Plugin catalog、详情、文件只读路由也是读取接线，不迁移 Library 写入所有权。上述局部接线均不等于 Scheduler 全面迁移或生产切换。
3. **产品调用体验**：`call.session` 的基础 wait/detach 已接入，但摘要增量、嵌套/并行 Graph call、OAuth 执行授权和完整前台/后台产品闭环仍未完成。
4. **生产切换**：还需要在保留现有数据和凭证的前提下完成单一生产 backend 切换、旧 Run/Session 兼容、Scheduler/网关接管、重启恢复和回滚验证。
5. **原 Graph 无修改执行契约**：Rust bundle 要求显式 Plugin 绑定；Responses 工具名必须符合 provider 合法字符集；HTTP MCP 还受节点网络授权约束。需要把这些规则收敛为正式作者/部署契约，避免每个 Graph 在隔离副本中手工适配。

## 推荐收口顺序

当前按 [全 Rust 平台计划](rust-platform-development-plan.md) 推进：普通 Goose Pilot 提问/删除确认的 F2b2 接线已按 A117 完成定向验收，下一步补完整 Graph/渠道会话，并在共享契约稳定后并行补齐 Library/授权、官方工具和渠道。该结果不扩展为复杂 form、所有媒体/压缩/Responses 组合或完整浏览器验收；学术检索 CLI 不代表全文/引用链或官方工具闭包。Scheduler 的计划 CRUD/tick/timeline Rust backend 子路径已经有双服务验收，仍需覆盖时区边界并与其他平台职责集成。标准 RSI 和深度研究的真实业务验收保留为候选版出口，不在日常回归中反复执行；之后再评估并验证生产 backend 切换。每一步都要分别记录组件通过、组合通过、真实 provider 通过和生产切换通过，不能用其中一个状态代替另一个。

## 历史决定

2026-10-04 曾冻结完整 R8/R9 平台迁移，只保留二进制 Runtime 交付评估；2026-10-05 用户将授权扩大为保持原 Graph/Plugin 和产品体验的 Rust Runtime 替代。此前的冻结记录、实验限制和证据仍在 `docs/pilot-development-plan.md` 的历史条目中，作为决策背景保留，不再作为当前完成状态。
