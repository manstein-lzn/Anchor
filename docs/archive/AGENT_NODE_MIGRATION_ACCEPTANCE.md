# Agent Node 一次性迁移验收

> **历史归档（2026-09-25）**：保留当时的讨论、计划与验证证据，不代表当前实现或待办。
> 文中的“下一步”“待实施”“必须”及旧阅读顺序仅适用于当时阶段，不作为后续开发指令。
> 当前入口见 [项目 README](../../README.md)，唯一升级方向见 [Plugin 设计](../plugins.md)。

结论：**M0–M5 结构性迁移通过，可进入后续 ADR/生产部署评估。**

独立核对：

- 当前分支：`migrate/agent-node`
- 全量测试：235 passed，无 skip（删除 mini resume 专属测试后的当前集合）
- `ruff check src/ tests/ scripts/`：通过
- `mypy src/anchor/`：通过，29 个源文件
- 生产源码不再导入 mini-swe-agent；`mini-swe-agent` 已从 `pyproject.toml` 删除
- PydanticAI slim/harness 已进入运行时依赖并固定版本
- B1–B8、C1–C9 故障矩阵在删除 mini 后重新执行，全部通过

## 已通过范围

- Agent Node 统一经 PydanticAI Harness 执行，保留 bash 单工具和 Anchor 完成协议。
- Op Node 使用独立 sandbox runtime，不依赖 PydanticAI/Harness。
- Graph scheduler 通过 Node API 获取结果，不读取 Harness 内部消息。
- Graph resume 同时检查 Graph pass 记录和 Node completion fact；已完成节点零模型请求、零工具执行。
- 上下文压缩、原始追加记录、输出引用、共享预算、崩溃判定和恢复均有组合证据。
- Agent→op→Agent 图恢复 B8 通过，已完成节点不会重复执行。
- 历史 runs 保持只读，不伪造 RecoveryRef，不做隐式迁移。

## 已知边界

- A6 的特定 kill 窗口因 Harness 没有可插入钩子而保持 `blocked`；这不是 B8 图恢复失败。正式接缝是 scheduler 的 `_settled_already`，已由 B8 验证。
- 不提供 exactly-once、断电耐久性、任意外部副作用自动对账或真实 provider 窗口错误覆盖。
- 摘要质量、引用真实性、多文件事务性仍未验证。
- `pydantic_adapter.py` 保留为兼容 re-export；生产入口是 `anchor.node.adapter.run_agent_node`。
- 共享 sandbox/context 修改应在发布前由主集成做一次 ownership/清理审阅。

文档修订：ADR-062 依赖矩阵中的 mini 删除里程碑已同步为 M5。
