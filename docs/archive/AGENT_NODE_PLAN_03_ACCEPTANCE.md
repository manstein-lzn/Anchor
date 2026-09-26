# 第三包主验收

> **历史归档（2026-09-25）**：保留当时的讨论、计划与验证证据，不代表当前实现或待办。
> 文中的“下一步”“待实施”“必须”及旧阅读顺序仅适用于当时阶段，不作为后续开发指令。
> 当前入口见 [项目 README](../../README.md)，唯一升级方向见 [Plugin 设计](../plugins.md)。

结论：**结构性验收通过，可进入 G2 组合验收；不等同于生产级恢复或 exactly-once 保证。**

验收依据：`AGENT_NODE_PLAN_03_RESULT.md`、提交 `919b70b`，以及本地复验。

## 本地复验

- `tests/test_node_recovery.py`：16 passed
- 全量 `tests/`：212 passed，无 skip
- `ruff check src/ tests/`：通过
- `mypy src/anchor/`：通过

## 已通过范围

- 使用公开 `StepPersistence`、`FileStepStore`、快照与继续 API，没有复制执行器或依赖私有 API。
- 真实 `SIGKILL` 和同步屏障覆盖 C1–C9；C10 完成第一包回归。
- `replayable`、`uncertain`、`continuable`、`invalid` 四类判定已实现，并对损坏、篡改、错引用和重复恢复做显式拒绝。
- 工具副作用完成但终态未写入时判定为 `uncertain`，不会自动重放；已 settle 且存在 complete 快照时可继续。
- 恢复引用包含 node/run/store/budget 信息；预算在控制目录持久化并采用原子写入。
- 控制账本和快照位于工作区之外；不允许节点通过修改自身工作区伪造恢复证据。

## 必须保留的边界

1. C1 在公开 API 上无法精确停在“模型响应已持久化、工具尚未开始”的瞬间，实际只能保守判为 `uncertain`。
2. C5 的“终态已写、快照尚未写”窗口在当前框架边界不存在；这是实测结果，不是 exactly-once 证明。
3. C9 只验证了 `bubblewrap --die-with-parent` 配置；更换沙箱配置需重新验证。
4. 不提供 exactly-once、副作用自动对账、生产级 crash recovery 或真实 provider/费用结论。
5. 摘要预算与第二包的上下文压缩尚未组合验证；压缩后的历史、快照恢复和大输出引用必须在 G2 端到端复验。

因此，第三包完成了故障边界测量、恢复证据格式和保守判定逻辑，满足进入组合验收的条件；生产切换仍需 G2 验证和明确的副作用幂等策略。
