# Execution limit inventory

阶段 1 交付物：所有限制的位置、默认值、作用范围与失败路径。判断顺序为
「代码/迁移 > 测试 > 本文」。本文件是清单，不是能力声明。

## 分类

| 类别 | 含义 | 能否终止任务 |
| --- | --- | --- |
| `transport` | 单次请求/连接的物理保护 | 否，只让该请求失败 |
| `resource_capacity` | 连接池、并发、载荷、上下文窗口 | 否 |
| `operator_policy` | 用户显式启用的费用/时长应急策略 | 是，但必须可见、可追溯、默认关闭 |
| `task_behavior` | 节点/工具执行行为边界 | 默认否；仅物理超时保护单次调用 |

## 清单

| 限制 | 位置 | 默认 | 类别 | 作用范围 | 失败路径 |
| --- | --- | --- | --- | --- | --- |
| `AgentCapability.timeout_seconds` | `runtime/capabilities.py` | 600s | task_behavior | 单次 Agent 调用 | 超时 → 记录 attempt 失败；可重试或失败关闭 |
| `AgentCapability.output_retries` | 同上 | 0 | task_behavior | 仅 JSON 序列化修复 | 超限 → `agent_output_invalid` |
| `AgentCapability.max_retries` | 同上 | 0 | operator_policy | 瞬态故障恢复 | 超限 → 节点失败，等待操作员 |
| `AgentCapability.max_tool_calls` | 同上 | 0（无限） | operator_policy | 单 Agent 工具调用总数 | 超限 → 工具拒绝消息，不杀 Run |
| `AgentCapability.tool_call_limits` | 同上 | 空 | operator_policy | 单工具调用上限 | 同上 |
| `AgentCapability.max_parallel_tools` | 同上 | 4 | resource_capacity | 工具并发 | 排队 |
| graph `run_timeout_seconds` | `domain/graph.py` 校验；`worker.py`/`control_worker.py`/`state/execution.py` 读取 | **无默认** | operator_policy | 整个 Run | 仅当显式配置且 `ANCHOR_EXPIRE_RUN_BUDGETS=true` 时失败关闭 |
| graph `max_rounds` | `domain/graph.py` 校验；`runtime/academic.py` 读取 | **无默认** | operator_policy | 学术修订轮数 | 仅当显式配置时 `blocked`；否则记录机械问题并继续 |
| `ANCHOR_EXPIRE_RUN_BUDGETS` | `runtime/settings.py` | `false` | operator_policy | 监督进程预算清扫 | 关闭时不扫描 |
| `ANCHOR_LEASE_STALE_AFTER` | 同上 | 30s | transport | lease 心跳评估 | 只报告 stale，不偷取 lease |
| `ANCHOR_SUPERVISOR_INTERVAL` | 同上 | 10s | resource_capacity | 观察频率 | — |
| model gateway HTTP timeout | `runtime/model_gateway.py` | connect 30 / read 900 / write 60 / pool 30 | transport | 单次 HTTP | 该请求失败，进入故障分类 |
| OpenAI SDK `max_retries` | 同上 | 0 | — | 禁用 SDK 隐式重试 | 重试归 durable node 层 |
| `ToolGateway.OUTPUT_LIMIT` | `runtime/tool_gateway.py` | 1 MB | resource_capacity | 工具 stdout | 截断并标记 |
| `ToolGateway.DEFAULT_TIMEOUT_SECONDS` | 同上 | 30s | transport | 单次工具执行 | 超时 → `timeout` 失败 |
| `ToolCapability.model_excerpt_chars` / `retry_excerpt_chars` | `runtime/capabilities.py` | None | resource_capacity | 送回模型的证据大小 | 截断，durable artifact 不变 |
| model context window | 供应商 | 依模型 | resource_capacity | 单次请求 | 供应商报错，进入故障分类 |
| API `PageSize` / `Offset` | `api/app.py` | 1..200 / ≥0 | resource_capacity | 分页 | 422 |

## 不再是隐藏默认

- `run_timeout_seconds` 与 `max_rounds`：缺省即无界。图元数据里出现即显式运营策略。
- 学术示例 `examples/graphs/academic-research.json` 已移除这两个字段。
- `output_retries` 默认从 1 改为 0。
- 故障恢复计数、业务循环计数、请求重试计数三者独立（ADR-019 / ADR-020）。

## 保留的基础设施限制

请求超时、连接池、并发、来源限流、输出大小、模型上下文窗口。它们处理资源与故障，
不等价于「研究失败」，也不得被删除以「解除限制」。

## 显式运营策略

`run_timeout_seconds`、`max_rounds`、`max_retries`、`max_tool_calls` 都属于
operator_policy：必须由用户在图或 capability 中显式声明，可见、可审计，默认关闭。
无法区分框架默认与用户策略时，请求操作员决定，不静默取消。
