# Goose + ACP 初步适配调研

日期：2026-10-06。此目录保存调研工作材料，不代表架构变更或产品验收完成。

## 核心问题

Goose 的 Rust Agent Runtime 与 Agent Client Protocol（ACP）能否更适合 Anchor？区分复用 Goose 运行时、通过 ACP 调用 Goose，以及仅提供外部 ACP 接口，不预设替换现有 io-harness + Rig。

## 独立研究范围

1. 主 Agent：核对 Anchor 当前实现、产品不变量和生产缺口；确认 Goose 官方项目定位与可核查版本；综合判断收益、代价及最小验证方案。
2. ACP 子任务：核对协议与 Goose 的服务端/客户端方向、工具和权限、文件系统和 MCP、取消与会话恢复、稳定与可选能力。输出 `findings_acp.md`。
3. Runtime 子任务：核对 Goose 的 Rust 可复用边界、Provider、工具、会话持久化与恢复、compaction、安全和分发、许可证与发布依赖。输出 `findings_runtime.md`。

## 证据与方法

- 每个子任务最多 3–5 次搜索，只使用官方文档、协议仓库、Goose 源码和发布记录。
- 明确区分协议提供、Goose 已实现、Anchor 尚未接入和未经实际执行验证的能力。
- 不做真实模型调用，不运行大 Graph，不修改现有业务代码或生产配置。
- 各子任务仅编辑自己的 findings 文件，保留来源链接、源码路径与不确定项。

## 综合输出

先在对话中提供初步结论、不同接入方式的适用性、关键风险和低成本验证建议。当前不冻结架构、不宣称迁移完成；用户评估后再决定是否做受限 spike 或正式方案。
