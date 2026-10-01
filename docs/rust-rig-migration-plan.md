# Rust/Rig Runtime 重构计划

本文是 `codex/rust-rig-runtime` 实验分支的开发计划，描述如何把 Anchor 逐步收敛到一个由平台宿主和独立 Graph 宿主共同使用的 Rust Runtime Kernel。它不是当前生产 Python 实现的完成声明，也不替代 [开发台账](pilot-development-plan.md)；每个阶段只有在取得对应证据后，才更新台账中的验收状态。

## 目标和不做的事

最终目标是让下面两种交付形态调用同一个 Runner 和 Runtime Kernel：

```text
完整 Anchor 平台 = API/WebUI/Session/Scheduler/Channel + Shared Rust Runner/Kernel
独立执行包     = Graph + 明确 Plugin 资源 + Shared Rust Runner/Kernel
```

迁移过程中保留现有 Graph、Run、Session/Turn、Plugin、Sandbox 和恢复事实，不建立第二套 Graph 语义。Python 宿主可以暂时作为适配层，但不能复制一套长期独立的 Rust/Python 执行规则。

本计划不包含：一次性重写 WebUI；把平台 API 塞进 Kernel；绕过 Sandbox 执行宿主命令；自造记忆、日志或调度系统；为了迁移而改变 fanout/join 的并发语义；在真实 provider 和恢复验收前移除 PydanticAI/Harness。

## 不变量

1. **唯一执行语义**：Node Runtime、Graph Runner 和恢复规则只有一个权威实现；平台与独立宿主只提供适配器。
2. **端口隔离**：模型、工具、Sandbox、事件、持久化和时钟通过小端口进入 Kernel；密钥、live client、宿主权限和平台连接不能进入 checkpoint。
3. **边界持久化**：模型请求前、模型响应后、工具批次结算后和终态都可以保存；恢复按已保存事实继续，不凭猜测重放未知副作用。
4. **局部并行**：Graph Run 仍然是单一协调单元；只有配对的 `fanout → branches → join` 区域允许并行，其他节点按既有串行规则推进。
5. **分层验收**：接口存在、provider-free 测试、Anchor 接入、真实 provider、真实 Sandbox、平台和独立分发分别记录，不能互相代替。
6. **可回退迁移**：每个垂直切片都能继续由 Python 路径运行；切换由宿主适配器控制，不修改历史事实格式而伪造迁移完成。

## 阶段和出口条件

| 阶段 | 交付 | 出口条件 |
| --- | --- | --- |
| R0 事实与契约 | 本计划、架构边界、运行身份和错误/恢复术语 | 契约能映射现有 Node/Run/Sandbox；未决权限和持久化变化单独记录 |
| R1 AgentNode Kernel | `NodeRequest`、`NodeOutcome`、`CompletionPort`、`ToolPort`、Rig `AgentRun` checkpoint、结构化 route | provider-free 工具循环、取消、非法 route、模型/工具边界恢复通过；真实单节点 smoke 通过 |
| R2 持久化边界 | `CheckpointStore` port、原子文件适配器、版本与身份校验、边界保存约定 | 进行中：端口、原子文件保存、pending model/tool 失败后续行、缺失/删除和路径拒绝已通过；未知外部副作用、并发写入语义和宿主集成仍待做 |
| R3 Provider 与流式 | provider 配置适配、stream 事件、超时、取消传播、请求/响应观测 | 进行中：OpenAI-compatible chat/responses、模型/工具超时、pending 状态保留和 provider-free 流事件/取消已通过；真实 provider 流式、请求观测和真实中断证据仍待做 |
| R4 Sandbox Adapter | Rust Sandbox port；Bubblewrap、只读输入、网络权限、命令超时和取消适配 | 真实命令在权限边界内运行；越权路径、网络和取消负向测试通过 |
| R5 串行 Graph Runner | Graph 快照、Run 身份、普通节点路由、提交/恢复/停止 | 与现有 Python Graph 的代表性 Graph 结果和失败语义对照通过；平台与独立 CLI 共用 Runner |
| R6 fanout/join | 复用现有配对契约；分支活动身份、乱序收束、失败/停止/崩溃恢复 | 现有 A31 代表性 Graph 在 Rust Runner 上通过；不引入嵌套或隐式并行 |
| R7 Plugin/MCP | Plugin manifest、工具目录、MCP stdio/HTTP、凭证和 Sandbox 绑定 | 明确 Plugin 闭包可独立启动；工具恢复、权限拒绝和资源变更检查通过 |
| R8 宿主适配与独立包 | Python 平台调用 Rust Runner；`anchor-graph` 使用同一 Runner；最小 bundle/兼容清单 | 同一 Graph 在平台和独立宿主产生一致 Run/提交/恢复事实；bundle 不含密钥、不扩大授权 |
| R9 迁移与收缩 | RSI、周报、企业微信助手逐个切换；旧 Python 执行路径只保留兼容用途 | 每个 Graph 通过真实 provider、Sandbox、恢复和平台验收；达到条件后再移除 Python Kernel 依赖 |

## 当前进度和下一步

R0 已由现有架构约束和本计划冻结；R1 已完成实验纵向切片，代码位于 `rust/anchor-runtime`，真实 provider smoke 已通过。R2 的 checkpoint 端口、文件适配器和 pending model/tool 失败续行已通过；R3 已加入可选模型/工具超时和独立 `StreamingCompletionPort`，流式端口在 provider-free 测试中能发出增量事件、在取消时不提交半成品响应。下一步是真实 provider 流式、请求观测和真实中断证据；R3 之前不宣称 Rust Runtime 已替代 Python，R5 之前不实现第二套 Rust Graph 调度语义。

## 每阶段记录

每个阶段必须同时更新：

- `docs/pilot-development-plan.md` 的验收矩阵和推进记录；
- 本计划的阶段状态与证据位置；
- Rust 单元/集成测试以及 `cargo fmt`、Clippy、`git diff --check`；
- 真实 provider、Sandbox、平台或独立宿主证据（如果该阶段要求）。

阶段完成只表示该阶段出口条件满足，不表示整个 Anchor 已经完成 Rust 重构。
