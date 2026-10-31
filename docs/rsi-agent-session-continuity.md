# RSI 节点会话连续性

2026-10-31：原生会话修复已接通 Host 调用链，固定 Goose 的反馈图验收通过。全新真实 Run `rust-18e35d456fbfe745` 已 settled/done、Gate PASS，8 个发布文件通过 HTTP 下载和摘要核验，成功发布历史可经共享 reader 读回。当前契约见 [架构](architecture.md)，业务流程及内容验收边界见 [RSI](rsi.md)。证据见[本轮验证](../.local/rust/clean-20261031-final/evidence/verification.json)、[发布核验](../.local/rust/clean-20261031-final/evidence/publication-check.json)、[确定性回归](../.local/rust/rsi-session-continuity-20261031-feedback/commands.json)。

本轮 20 次 Agent invocation 中，12 次后续调用均恢复原生历史；8 个节点 scope 隔离，analyze 和两个评审各执行 5 次，始终复用各自 Session、执行目录和 Workspace。analyze 第二次调用恢复 483 条历史，首个 Provider 请求的 247,633 input tokens 中有 207,488 cache_read_tokens（约 84%）；这是本次观察，不是后续命中保证。详见[原生会话观察](../.local/rust/clean-20261031-final/evidence/agent-observations.json)和 [Workspace 核对](../.local/rust/clean-20261031-final/evidence/workspace-observations.json)。

首次真实 Run `rust-18e35b25b759d8bf` 曾因 Gate 错读宿主描述、research 不接受非空输出目录而失败；对应业务适配已修复，失败记录保留，不计为通过。见[首次真实失败](../.local/rust/clean-20261031/evidence/rsi-final-failed.json)与[冻结 Gate 输入回放](../.local/rust/clean-20261031/evidence/rsi-descriptor-gate-z1ixu1i3/verification.json)。

同一个 Run 内，同一个 AgentNode 的后续 invocation 使用稳定的 Goose 原生会话；作用域包含 Run、Graph 和 node。不同节点、不同 Run 默认隔离。跨 Run 延续继续通过已核验的消息 Session 绑定授权，Workspace 共享本身不授予对话历史。

新 invocation 追加当前输入，同 invocation 恢复则加载原任务并核查未知效果。Goose 负责历史、Agent loop 和 compaction；Anchor 不拼接 Kernel 历史来模拟会话，也不要求 Agent 每轮自行查询自己的完整记录。每次 invocation 的交付、成本和执行事实仍独立保留。

Run 内 scope 与既有显式跨 Run scope 共用原生执行租约。删除 Run 时先取得原生 scope 清理权，再释放 Workspace 和执行事实；使用中的历史不能被清除。新 invocation 选择当前获授权模型，恢复既有 invocation 保留冻结绑定。

旧 Run 中各自独立创建的原生会话不自动合并；本次使用新数据根验收，不提供旧历史迁移。

## 问题证据

2026-10-30 的隔离 Run `rust-18e34d016dbb565b` 暴露了旧实现缺口：没有消息 Session 绑定时，Goose 目录按 invocation 隔离，反馈轮使用 `session/new`。analyze 的 11 次、fact-review 的 10 次、proposal-review 的 11 次调用均为 `continuation=false`，无 restored history。相同 `session_id` 字符串来自不同数据库，不能证明续接。

该 Run 在第十次 Gate 后仍要求修订，最终没有成功发布。文件交付仍在；缺失的是默认原生会话续接。该缺口在 `dd2fda4` 及 Kernel 迁移前已存在，不能归因于本次 Workspace 改造。评审退回也涉及真实稿件问题，尚无证据把全部耗时归因于会话连续性。

- [旧 Run 历史](../.local/rust/rsi-verification-20261030/state/kernel-runs/run%2Frust-18e34d016dbb565b/history.jsonl)
- [旧 analyze 第二次调用证据](../.local/rust/rsi-verification-20261030/state/goose-acp/508212aca2cc6d95b1afbcc5d76768555ce37611715bc6fbecb831a5ec1f47af.evidence.json)
- [实际反馈图回归](../rust/anchor-runner-host/tests/goose_acp.rs)

## 验收边界

确定性回归必须实际经过 Host、GraphRunner、Goose ACP 和授权 MCP，验证反馈轮加载前序对话、保留 Workspace、隔离其他 Run/node、保持旧消息前缀，以及中断和删除边界。不能只凭 scope 解析单测宣称接通。

原生会话连续不保证 Provider 缓存必然命中。模型、工具定义、服务及 Goose compaction 均可能改变请求；缓存数据须独立测量。完整 RSI 成功还须有 Gate PASS、publish Artifact 与内容抽查证据，不以固定轮数强制发布。
