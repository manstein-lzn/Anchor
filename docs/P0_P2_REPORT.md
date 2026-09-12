# P0–P2 执行汇报

**日期**：2026-09-12　**范围**：`docs/DEVELOPMENT_PLAN.md` 的 P0.1 至 P3
**提交范围**：`cedd62f..e4252cc`（15 个提交，80 个文件，+8,517 / −180）

---

## 1. 一句话

计划里的 **10 个切片全部推进完毕**（9 个实现，1 个按自身退出判据作决定），
过程中**发现并修掉了 5 处"静默回落"类既有缺陷**——系统看起来在工作，而实际行为与运维的认知不同。

---

## 2. 逐切片

### P0.1 持久服务与重启恢复　`cf4015c`

| | |
|---|---|
| **做了什么** | `infra/systemd/` 从 5 个单元增至 7 个（**API 和 receiver 根本没有单元**）；每个单元显式声明全部路径；`StartLimitIntervalSec/Burst` 让崩溃循环变成可见的 `failed` 而不是永久 `activating`；`runtime/preflight.py` 在每个服务进入主循环前检查环境并以退出码 2 + JSON 原因失败 |
| **验收证据** | `scripts/validate_service_recovery.py` PASSED：停 worker mid-node → lease `state=stale, recoverable=True`（**不是被静默回收**）→ 显式恢复 → run 闭合并 `completed`，11 条事件无缺口，无重复操作 |
| **修掉的既有缺陷** | `supervisor_service` **静默回落到开发者本地 SQLite**（`ANCHOR_DATABASE_URL` 未设时），运维看着一个空库以为一切正常 |

### P0.2 并行分支失败传播　`c8b242a`

| | |
|---|---|
| **做了什么** | `fail_node_and_propagate` 增加扇出（形状照抄 `stop_run`，它早就为操作员停止做了这件事）：非终态节点 → `cancelled` + `error_code=run_failed`；释放该 run 全部 lease；`run.failed` 记录 `abandoned_nodes`。新增 `FencedAttempt` 异常，与 `ConcurrencyConflict` 分开。`dispatch_pending` 逐条隔离 |
| **验收证据** | `scripts/validate_failure_fanout.py` PASSED：真实分支失败 → sibling `cancelled(run_failed)` → 无遗留 lease → supervisor 重启后状态不变 |
| **修掉的既有缺陷** | **失败的 run 留下 76 个永久 `pending` 节点**，无终态、无原因——从外部看与"还在等 worker"无法区分。以及 `dispatch_pending` 一条坏消息中止整批 |

### P0.3 生产边界与失败关闭　`51b2570`

| | |
|---|---|
| **做了什么** | URL 形状的 `ANCHOR_ARTIFACT_ROOT` 启动即拒绝（`artifact_backend_unsupported`）；`LocalArtifactStore` 在**构造函数**里拒绝，所以任何调用方都继承；preflight 问那个 store 而不是重新推导规则（**规则写两遍会漂移**）。`docs/limits.md` 加生产边界表 |
| **验收证据** | 13 项测试，含一个**真实 worker 进程退出 2 并输出结构化原因** |
| **修掉的既有缺陷** | `ANCHOR_ARTIFACT_ROOT=s3://bucket/artifacts` **被静默接受，在本地建了一个叫 `s3:/bucket/artifacts` 的目录**。运维以为写了 S3，而问题会在一小时后以"证据不见了"的形式浮出来 |

### P1.1 整轮录制/回放　`0f2a623`

| | |
|---|---|
| **做了什么** | 回放键从 `(node_run_id, attempt, seq)` 改为**进程声明 `replay_of` → 第 N 次调用取父 run 的第 N 条录制**。序号是免费的（录制本就按事件顺序加载）；**录制的 `node_id` 是校验的而非假设的**，不一致即定位失败 |
| **验收证据** | `scripts/validate_replay_run.py` PASSED：录制一个 run，对**死端口**回放，同状态/同节点路径/同执行事件，**唯一差异是 `model.call` → `model.call_replayed`**（必需，回放不得覆盖它读的录制） |
| **意义** | 它证明了那个此前**所有实验都默认成立却从未验证**的前提：给定模型答案，引擎路径是确定的 |

### P1.2 稳定前缀与工作集遥测　`fed4dd4`

| | |
|---|---|
| **做了什么** | 每次模型调用记录三个哈希（prefix / declared / working-set），`/api/runs/{id}/usage` 报告哪一段动了、每段稳不稳、gross 如何拆成"新增 vs 重发"。`PromptFactory` 改为返回 `ResolvedPrompt` dataclass（**已经是 4 个位置字段，第 5 个按位置读就是等着发生的静默 bug**） |
| **验收证据** | 真实 run：三个哈希全部记录，`prefix_stable=True`；7 项测试 |
| **为什么值得做** | 实测：缓存命中价格是未命中的 **1/50**，而账单 62% 是 output。**吓人的 gross 计数不是账单**，决定价格的是前缀稳不稳——而此前没有任何东西记录"哪一段动了" |
| **测试抓到的错** | 我第一版用 `max(delta, 0)` 算"新增"，**prompt 收窄时对不上总数**。而收窄是真实情况（memory 被裁剪、快照被修正） |

### P1.3 跨 Run content cache　`13647f6`

| | |
|---|---|
| **做了什么** | 文件型缓存，**独立于数据库与工件库**（所以"是投影、可丢弃"是结构事实而非承诺）。每次读取返回 `hit`/`miss`/`expired`/`corrupt`；**每次重算 sha256** 而非信任元数据；键含 graph version 与 task scope；覆盖已存在条目必须说明理由 |
| **验收证据** | 18 项测试：冷/热/损坏（正文与元数据两种）/未知格式/过期/不同版本/不同 scope/不同 URL/覆盖被拒后允许/损坏后恢复/丢弃/远程根被拒/非正 TTL 被拒 |
| **默认** | **关闭**。一个默认打开的缓存是没人决定要的缓存 |

### P2.1 provider-free 端到端 CI　`27de422`

| | |
|---|---|
| **做了什么** | `scripts/ci_e2e.py` 自建临时环境（库/工件根/token/profile/临时端口的 API），驱动完整链路：MCP 授权 → 准入 → 执行（control+agent+verifier）→ 人工门 → 批准 → MCP 观察 → 完成。agent 步骤由**签入的录制**供给 |
| **验收证据** | PASSED，五个节点全部 `completed`，事件里有 `model.call_replayed` 且**没有 `model.call`**——所以"没够到 provider"是**事实而非意图**。夹具前后哈希一致 |
| **我犯的错** | 加了 GitHub Actions workflow 而未问你，它在你的仓库上持续报错 —— **已删除**（`e3cecec`）。而且按写的那样根本跑不过（套件需要 PostgreSQL 服务端） |

### P2.2 operator policy　`fe7bf1d`

| | |
|---|---|
| **做了什么** | `max_rounds` 现在**在回边即将开设超出上限的修订时执行**：该边 `selected=False`，决策记录 `reason=revision_ceiling`（**自己的理由，不是 `condition_false`**——读者不该去猜循环是因为被限制了还是因为条件为假） |
| **验收证据** | 8 项测试，含"上限属于 pin 住的版本"（改图不影响运行中的 run） |
| **修掉的既有缺陷** | `max_rounds` **被校验但从不执行**。而 `domain/ir.py` 的映射表说 `run_timeout_seconds` 是"唯一被识别的键"，与校验器**互相矛盾**——一个看着像策略、实际什么都不做的键，比没有更糟 |

### P2.3 文档与协议稳定化　`bbe8f3f`

| | |
|---|---|
| **做了什么** | `docs/INVARIANTS.md` 把 I1–I9 各绑定一个具名测试，**并由测试解析该表逐个查找**——改测试名会让它失败。`docs/PROTOCOL_VERSIONS.md` 说明每个版本化表面的语义与版本变更承诺；测试**双向校验**（每个记录的表面有标记，每个标记被记录）。状态词统一为 已实现 / 未实现 / 已弃用，并拒绝被替换的词 |
| **验收证据** | `test_invariants.py` 第一次运行**就抓到了我自己的两处错误**（测试名写在了错误的文件里）。`test_protocol_versions.py` 立刻找出三个存在但无文档的标记 |

### P3 认知 schema 决定　`e4252cc`

| | |
|---|---|
| **判据** | 计划 P3 自己的第 5 条：*"如果没有独立行为收益，停止扩展认知 schema。"* |
| **结论** | **没有独立行为收益，所以执行该条。** 实验在计划成形之前已跑完（`experiments/cognition_takeover/`） |
| **证据** | ① 考卷：同尺寸下 schema 决定一切（S 7.0 vs B 20.3），状态用 3.4% 体积达到或超过原始材料 ② **行为测试：三个条件都满足全部 5 项交付要求**，包括考卷只拿 7/31 的那个 ③ 取回：**不累积不够，必须先选择** |
| **为什么形状不重要** | Anchor 的节点**本来就是"每次全新投影"**：累积在工件里不在上下文里，重试不重放历史。**认知层想做的事，引擎已经在做，只是声明粒度更粗** |
| **写明什么会推翻它** | 多次交接（N≥10）中状态不退化而摘要链退化；一个受控 selector 证明"先选择再读取"降低错误决策；细节无法按需取回的任务。**不写这个，结论就不可证伪** |
| **P4** | 按计划 §7 排除（"RSI 暂不进入当前开发批次"） |

---

## 3. 发现的既有缺陷（这是本轮最有价值的部分）

**五处"静默回落"**——系统看起来在工作，而行为与运维认知不同：

| # | 缺陷 | 表现 |
|---|---|---|
| 1 | supervisor 静默回落本地 SQLite | 运维看着空库以为服务正常 |
| 2 | 失败 run 留 76 个永久 pending 节点 | 与"还在等 worker"无法区分 |
| 3 | `s3://` 工件根静默建本地目录 | 以为写了 S3 |
| 4 | `max_rounds` 被校验但从执行 | 设了上限，无报错，相信有上限 |
| 5 | `dispatch_pending` 一条坏消息中止整批 | 一个坏行挡住它后面所有 run |

**两处"测试在守护缺陷"**：`test_relational_store` 与 `test_worker_service_context` 断言了"节点留在 pending"——**正是要修的行为**。已改写。

**一处我的错误归因**：`ModelAPIError: Connection error.` 交替出现，我差点判定"provider 不稳定"。真相是 **`httpx.AsyncClient` 被跨 event loop 使用**——而 `scripts/node_harness.py` 的 docstring 里**早就写明了这个坑**，我写了它又违反它。

---

## 4. 未完成与已知限制（诚实清单）

```
P2.1 的 workspace 部分未接入无 provider CI
  —— workspace validation / lineage / parallel merge 仍由各自脚本覆盖，会起真实 agent 节点
  —— 需要为 agents.coder 类也录夹具，是同一机制的扩展

P0.3 未实现：远程工件后端、身份与角色、按用户的审计归因

P1.3 未实现：远程缓存后端；缓存未被 workspace 物化使用
     且没有度量过缓存是否真的回本

P2.2 max_rounds 限制的是每个节点的修订次数，不是 run 的总量

P2.3 HTTP API 无版本前缀，依据是"唯一消费者在本仓库内"——这个依据在某天会出现第二个消费者时失效，
     而没有任何东西探测那一天

P3 的结论建立在这两点上：实验只有【一次】交接（而架构原始主张是关于【累积】的），
   且从未实现一个受控 selector
```

---

## 5. 仓库状态

```
15 个提交 · 80 个文件 · +8,517 / −180
59 个测试文件 · 524 个测试函数 · 56 个 ADR
ruff C901   13（基线 13，不变）
mypy         97（基线 97，不变）
工作区干净 · 全部已推送
```

**新增的能力性文件**：`runtime/preflight.py`、`runtime/content_cache.py`、`infra/systemd/anchor-api.service`、
`infra/systemd/anchor-receiver.service`、`scripts/ci_e2e.py`、`scripts/validate_{service_recovery,failure_fanout,replay_run}.py`

**新增的文档**：`docs/{COGNITION_DECISION,INVARIANTS,PROTOCOL_VERSIONS}.md`

---

## 6. 一个方法论上的观察

本轮 9 个切片里有 **6 处**是"某个东西被声明了但没人执行"：

```
max_rounds 被校验      → 但运行时没人读
ir.py 说"只有 1 个键"  → 但校验器接受 2 个
s3:// 被接受          → 但会变成本地目录
版本标记存在          → 但无文档
AGENT_SURFACE 的能力   → 但 agent 无法观察/恢复 lease
"CI 不依赖 provider"   → 但 CI 根本不存在
```

**这是一个模式，不是一个巧合。** 它指向的检查是：每当引入一个"声明"（一个配置键、一个标记、一个
文档承诺），就要问"谁读它"，并让那个问题有一个可机械判定的答案。

本轮在这件事上建立的三样东西——`test_invariants.py`、`test_protocol_versions.py`、preflight 的
"问那个 store 而不是重新推导规则"——都是同一个答案的不同形式。
