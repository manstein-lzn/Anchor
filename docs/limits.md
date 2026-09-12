# Execution limit inventory

所有限制的位置、默认值、作用范围与失败路径。判断顺序为「代码/迁移 > 测试 > 本文」。
本文件是清单，不是能力声明。

## 分类

| 类别 | 含义 | 能否终止任务 |
| --- | --- | --- |
| `transport` | 单次请求/连接的物理保护 | 否，只让该请求失败 |
| `resource_capacity` | 连接池、并发、载荷、上下文窗口 | 否 |
| `task_behavior` | 节点/工具执行行为边界 | 默认否；仅物理超时保护单次调用 |
| `operator_policy` | 用户显式启用的费用/时长应急策略 | 是，但必须可见、可追溯、默认关闭 |
| `convergence` | **判定工作已完成**（不是预算耗尽） | 是，但只在边际价值消失时触发 |
| `quality_gate` | 交付物的确定性门槛 | 是，退回重做，不静默通过 |

`convergence` 与 `quality_gate` 是这套系统里"智慧的方式"：它们不猜某个场景需要多少资源，
而是判断**继续做还有没有价值**、**产出是否达到既定标准**。见下面的「为什么不用 max_xxx」。

## 收敛判据（不是预算）

| 判据 | 位置 | 阈值 | 触发时 |
| --- | --- | --- | --- |
| 研究饱和 | `runtime/academic_rounds.py` `MIN_MARGINAL_GAIN` / `SATURATION_WINDOW` | 连续 3 轮新增 < 语料 5% | 结束采集，进入写作 |
| 零新增 | 同上 | 某轮新增 0 条 | 同上（这是收敛，不是"预算用尽"） |
| 模型自评饱和 | 采集器输出的 `saturation` | 模型判断 | 同上 |
| 同一机械缺陷不改 | `runtime/academic.py` `MECHANICAL_REPEAT_LIMIT` | 连续 3 次修订同一缺陷 | `blocked`，交人工 |
| 评审只报 minor | `runtime/academic.py` `evaluate_review` | 0 个 major 且确定性检查通过 | **批准**（accept with minor revisions，ADR-043） |

## 交付物质量门（可判定，判不了就不存在）

| 门 | 位置 | 阈值 | 失败路径 |
| --- | --- | --- | --- |
| 必需章节 + 锚点顺序 | `runtime/academic.py` `structure_errors` | 见 ADR-041 | 退回作者 |
| 禁止 catch-all 章节 / 主题节数量 | 同上 | ≥2 个主题节 | 退回作者 |
| 正文禁止证据级别标签、过程语言、内容哈希 | 同上 `craft_errors` | 0 次出现 | 退回作者 |
| 段落长度 | 同上 `MAX_PARAGRAPH_CHARS` | 1200 字符 | 退回作者 |
| 方法论章节长度 | 同上 `MAX_METHODS_CHARS` | 2500 字符 | 退回作者 |
| 摘要长度 | 同上 `MAX_ABSTRACT_CHARS` | 1800 字符 | 退回作者 |
| 结果型数字须有全文级来源 | 同上 `unsupported_number_claims` | 每个数字至少 1 篇读过全文 | 退回作者 |
| 引用必须可验证 | `runtime/evidence.py` `verify_source` | id 在检索结果中 + 读取为同一篇 | 退回作者 |
| 来源数量 / 全文阅读数 | 同上 | `minimum_sources` / `minimum_reads` | 退回采集 |
| 工作区执行（`workspace.exec`） | `runtime/sandbox.py` | 仅 bubblewrap；缺失即拒绝 | 工具失败 |

## 清单

| 限制 | 位置 | 默认 | 类别 | 作用范围 | 失败路径 |
| --- | --- | --- | --- | --- | --- |
| `AgentCapability.timeout_seconds` | `runtime/capabilities.py` | 600s | task_behavior | 单次 Agent 调用 | 超时 → 记录 attempt 失败；可重试或失败关闭 |
| `AgentCapability.output_retries` | 同上 | 0 | task_behavior | 仅 JSON 序列化修复 | 超限 → `agent_output_invalid` |
| `AgentCapability.max_retries` | 同上 | 0 | operator_policy | 瞬态故障恢复 | 超限 → 节点失败，等待操作员 |
| `AgentCapability.max_tool_calls` | 同上 | 0（无限） | operator_policy | 单 Agent 工具调用总数 | 超限 → 工具拒绝消息，不杀 Run |
| `AgentCapability.tool_call_limits` | 同上 | 空 | operator_policy | 单工具调用上限 | 同上 |
| `AgentCapability.max_parallel_tools` | 同上 | 4 | resource_capacity | 一次模型轮次内可并发发出的工具调用数 | 排队；同时决定模型轮次数（见下） |
| `ModelProfile.max_tokens` | 同上 | None | task_behavior | 单次模型调用的输出预算 | 无预算时用供应商默认值；思考型模型的 reasoning 与答案**共用**该预算，太小会截断 JSON |
| `ModelProfile.stream` | 同上 | false | transport | 响应方式 | 非流式长请求可能被网关 504 截断 |
| graph `run_timeout_seconds` | `domain/graph.py` 校验 | **无默认** | operator_policy | 整个 Run | 仅当显式配置且 `ANCHOR_EXPIRE_RUN_BUDGETS=true` 时失败关闭 |
| graph `max_rounds` | `domain/graph.py` 校验；`runtime/academic.py` 读取 | **无默认** | operator_policy | 学术修订轮数 | 仅当显式配置时 `blocked` |
| `ANCHOR_EXPIRE_RUN_BUDGETS` | `runtime/settings.py` | `false` | operator_policy | 监督进程预算清扫 | 关闭时不扫描 |
| `HEARTBEAT_FAILURE_LIMIT` | `runtime/worker.py` | 3 | task_behavior | 连续心跳失败次数 | 达上限 → **显式失败节点并释放 lease**（旧行为是静默取消并泄漏 lease） |
| `RETRY_CONTEXT_CHAR_BUDGET` | `runtime/agent_tools.py` | 12,000 | resource_capacity | 重试时重放的历史证据上限 | 截断；该块每次模型调用都会重发，故必须小 |
| `PRIOR_EVIDENCE_LIMIT` | 同上 | 30 | resource_capacity | 重放的操作条数 | 只在**真正重试**时生效：前一次尝试已完成则不重放 |
| `BATCH_READ_LIMIT` | `runtime/research_tools.py` | 8 | resource_capacity | 单次 `scholarly.read_many` 的文档数 | 超出被拒 |
| `ToolCapability.model_excerpt_chars` × `excerpt_list_limit` | `runtime/capabilities.py` | 依工具 | resource_capacity | **送回模型的证据大小（两者相乘）** | 截断，durable artifact 不变 |
| arXiv 来源限速 | `runtime/research_tools.py` `pace_arxiv` | 3s/请求 | transport | 对 arxiv.org 的请求 | 排队等待；这是外部硬约束，读 N 篇至少 3N 秒 |
| Crossref 来源限速 | 同上 `pace_crossref` | 1s/请求 | transport | 对 api.crossref.org 的请求 | 排队等待 |
| `ANCHOR_STORAGE_GLOBAL_BYTES` | `runtime/settings.py` | `None` | operator_policy | 整个安装的存储监控目标 | 仅报告/提示；不终止节点、不自动删除 |
| `ANCHOR_STORAGE_PER_GRAPH_BYTES` | 同上 | `None` | operator_policy | 每个图的存储监控目标 | 同上；运行时可经 `PUT /api/storage/budget` 调整 |
| `ANCHOR_STORAGE_ENFORCE` | 同上 | `true` | operator_policy | 滚动清理开关 | 关闭后预算仅提示；无预算时始终不删 |
| `ANCHOR_STORAGE_SWEEP_INTERVAL` | 同上 | 60s | operator_policy | 清理扫描间隔 | 由 scheduler 服务执行 |
| `ANCHOR_STORAGE_SWEEP_BATCH` / `_MAX_ROUNDS` | 同上 | 25 / 40 | operator_policy | 每轮淘汰条数与最大轮数 | 限制单次清理工作量 |
| 失败扇出 | `state/checkpoints.py` | 总是执行 | task_behavior | 节点失败导致 run 失败时 | 该 run 全部非终态节点 → `cancelled` + `error_code=run_failed`；剩余 lease 释放；`run.failed` 记录 `abandoned_nodes`。重复投递幂等 |
| `FencedAttempt` | `state/errors.py` | — | task_behavior | lease 在执行中被释放 | 抛给 worker，worker 安静停止；**不**再试图失败一个已有终态的节点 |
| dispatch 逐条隔离 | `runtime/dispatch.py` | 总是执行 | resource_capacity | 一条消息不可投递 | 该条留在 pending 重试，**不再中止整批**；记录 `run.dispatch_failed`（幂等键固定，重复重试只记一次） |
| `ANCHOR_CONTENT_CACHE_ROOT` | `runtime/content_cache.py` | 空（关闭） | resource_capacity | 抓取研究内容 | 未设置即不缓存。设置了则是四选一结果：`hit`/`miss`/`expired`/`corrupt`；后三者都回落到真实抓取。损坏由**每次读取重算 sha256** 检出 |
| `ANCHOR_CONTENT_CACHE_TTL_SECONDS` | 同上 | 未设（不过期） | resource_capacity | 缓存条目老化 | 不设适用于不可变已发表论文；设了则过期等价于 miss |
| 服务启动 preflight | `runtime/preflight.py` | 总是执行 | task_behavior | 每个服务进入主循环之前 | 退出码 2 + stderr 上一行 JSON：`database_url_missing` / `database_unreachable` / `schema_not_migrated` / `runtime_config_missing` / `runtime_config_invalid` / `runtime_config_empty` / `artifact_root_unwritable`。**没有静默回落** |
| `systemd StartLimitIntervalSec` / `Burst` | `infra/systemd/*.service` | 60s / 5 | task_behavior | 崩溃循环 | 超过后 systemd 标记 `failed`，不再无限重试成 `activating` |
| `ANCHOR_MODEL_RECORDING` | `runtime/settings.py` | `off` | operator_policy | 模型调用的录制/回放（ADR-044） | `off` 不安装 wrapper，零开销；`record` 写投影；`replay` 按位置提供录制答案并拒绝越界 |
| `MAX_TRACKED_ATTEMPTS` | `runtime/model_recording.py` | 4096 | resource_capacity | 常驻进程的调用计数器上限 | 淘汰最旧；一个早已结束的 attempt 被淘汰不会被误解 |
| `MAX_TRACKED_REFS` | 同上 | 4096 | resource_capacity | 录制引用检查日志上限 | 同上，保留最近条目 |
| `MAX_PLANNED_RUNS` | `runtime/model_replay.py` | 64 | resource_capacity | 回放计划缓存的 run 数 | 淘汰最旧 |
| `ANCHOR_LEASE_STALE_AFTER` | 同上 | 30s | transport | lease 心跳评估 | 只报告 stale，不偷取 lease |
| `ANCHOR_SUPERVISOR_INTERVAL` | 同上 | 10s | resource_capacity | 观察频率 | — |
| model gateway HTTP timeout | `runtime/model_gateway.py` | connect 30 / read 900 / write 60 / pool 30 | transport | 单次 HTTP | 该请求失败，进入故障分类 |
| OpenAI SDK `max_retries` | 同上 | 0 | — | 禁用 SDK 隐式重试 | 重试归 durable node 层 |
| `ToolGateway.OUTPUT_LIMIT` | `runtime/tool_gateway.py` | 1 MB | resource_capacity | 工具 stdout | 截断并标记 |
| `ToolGateway.DEFAULT_TIMEOUT_SECONDS` | 同上 | 30s | transport | 单次工具执行 | 只读工具超时 → `timeout` 失败 |
| `http.post` 传输超时/断流 | 同上 | 调用方传入（默认 30s） | transport | 单次副作用请求 | 请求已发出 → `outcome_unknown`，需人工对账；绝不自动重试 |
| `ToolCapability.allow_private_network` | `runtime/capabilities.py` | false | operator_policy | 副作用 HTTP 目标 | 默认拒绝回环/私有/保留地址 |
| model context window | 供应商 | 依模型 | resource_capacity | 单次请求 | 供应商报错，进入故障分类 |
| API `PageSize` / `Offset` | `api/app.py` | 1..200 / ≥0 | resource_capacity | 分页 | 422 |

## 为什么不用 `max_xxx` 限制模型

早期版本用隐藏的 `max_rounds` / `run_timeout_seconds` / 工具预算来"防止跑飞"。它们猜错了
两件事：模型在某个具体场景需要多少资源无法预测，而这些上限会杀掉正在正常工作的任务。

现在这套系统用的是三类**可判定**的机制：

1. **收敛判据**：研究在边际价值消失时停（连续 3 轮新增 < 5%），修订在**没有进展**时停
   （同一缺陷 3 次不改），而不是在预设数量处停。真实同行评审也是这个形状——期刊不会
   因为"改了 5 轮"拒稿，但会因为"你改的还是同一个问题"要求编辑介入。
2. **质量门**：交付物必须通过确定性检查才能发布；判不了的规则不存在，所以每条规则都
   会被 CI 判定，而不是写在文档里。
3. **观察与建议**：supervisor 只评估 lease 并给出建议，**绝不抢占活跃 lease、绝不自动
   重试外部副作用、绝不代替 worker 完成节点**。不确定即不确定，请求人决定。

## 显式运营策略

`run_timeout_seconds`、`max_rounds`、`max_retries`、`max_tool_calls` 都属于
operator_policy：必须由用户在图或 capability 中显式声明，可见、可审计，默认关闭。
无法区分框架默认与用户策略时，请求操作员决定，不静默取消。

## 不再是隐藏默认

- `run_timeout_seconds` 与 `max_rounds`：缺省即无界。图元数据里出现即显式运营策略。
- 学术示例 `examples/graphs/academic-research.json` 不含这两个字段。
- `output_retries` 默认从 1 改为 0。
- 故障恢复计数、业务循环计数、请求重试计数三者独立（ADR-019 / ADR-020）。

## 保留的基础设施限制

请求超时、连接池、并发、来源限流、输出大小、模型上下文窗口、输出 token 预算。它们处理
资源与故障，不等价于「研究失败」，也不得被删除以「解除限制」。

## 成本相关

一次模型调用的花费由**轮次 × 上下文**决定，不是由读了多少字节决定：工具循环每次调用都会
重发整段对话。

- `ModelProfile.max_tokens` 给输出一个显式预算（思考型模型的 reasoning 与答案共用它）。
- `BATCH_READ_LIMIT` 与批量读工具用于压低**轮次数**。
- `model_excerpt_chars` × `excerpt_list_limit` 用于压低**每次调用新增的上下文**。
- `GET /api/runs/{id}/usage` 分别报告 gross / cached / billed input；只看 gross 会高估
  账单数倍，因为工具循环重发的绝大部分是供应商前缀缓存命中的内容。

## 生产边界（P0.3）

以下是**未实现**的能力。每一项都是显式拒绝或明确记录，不留静默回落。

| 能力 | 现状 | 行为 |
| --- | --- | --- |
| **远程 artifact 后端**（S3/MinIO/GCS…） | 未实现 | `ANCHOR_ARTIFACT_ROOT` 若是 URL 形状（`s3://…`、`s3:/…`、`minio:…`）→ **启动即拒绝**，退出码 2，错误码 `artifact_backend_unsupported`。生产可替换任何实现 `ArtifactStore` 协议的后端，**但本地 store 不会去模仿它** |
| **身份与授权** | 单一共享 bearer token（`ANCHOR_API_TOKEN`） | 无按用户身份、无角色、无认证的 actor。审计里的 `actor` 是调用方自报的字符串。边界是：**无 token 或错 token → 401/403**；`/health/live` 故意不鉴权（探针需要密钥就无法在密钥有问题时报告任何事） |
| **PostgreSQL 路径** | 已实现，按标记验证 | `tests/test_relational_store.py` 参数化 sqlite/postgresql，无 PG 时跳过而非静默通过 |
| **人工门（approval）** | 已实现且三面一致 | CLI `waits`/`approve`/`reject`、Run Console、API 路由三处都覆盖，且共用 `anchor.client` 一条路径 |
| **未知 `ANCHOR_MODEL_RECORDING`** | 拒绝 | 服务启动时转换枚举，未知值即失败，不落回 `off` / `record` |

**判据**：一个只在文档里写着的限制不是边界。上表每一行都有测试（`tests/test_production_boundaries.py`），
且拒绝发生在**启动时**而不是使用时——因为一个启动后就静默放错工件位置的服务，问题会在一小时后以
「证据不见了」的形式浮出来。
