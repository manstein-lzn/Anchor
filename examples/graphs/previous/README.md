# 历史图示例

本目录保存 `9a7497f` 之前的设计记录，**不兼容当前运行时，也不是待办或升级方向**。相关架构与验证材料见 [历史归档](../../../docs/archive/README.md)。

这些图使用旧节点类型（`artifact`、`loop`、`verifier`、`human_task`）、`agent_ref`、`input_mapping`、`progress_signal` 或带发布版本的 bundle。当前运行时不读取这些结构，勿直接导入。

本目录中的研究说明提到旧 worker 服务、数据库和命令，其操作步骤仅作历史记录。当前使用方式见 [使用指南](../../../docs/usage.md)，当前研究示例是 [deep-academic-research.json](../deep-academic-research.json)。

可运行的图保留在上一级 `examples/graphs/`，由 `tests/test_examples.py` 检查。下一阶段只按 [Plugin 设计](../../../docs/plugins.md) 推进，不从旧示例恢复其他功能范围。
