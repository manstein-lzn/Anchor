# Anchor Session Handover

这份文档用于把 `/home/mansteinl/Anchor` 交给新的 Codex session 继续开发。它记录的是当前工作区事实、已经获得的测试证据、尚未验收的改动和下一步执行顺序。新 session 应先用代码、迁移、测试和运行状态核对本文，不要只依赖历史对话。

本次交接核对时间：`2026-09-08 22:00 CST`；Verifier 收尾与执行策略会话完成下述验收并更新本文。

## W0.1：content_ref 边界类型（2026-09-09，ADR-027）

- `domain/content.py`：唯一的边界类型。`ContentRef` 解析/校验/序列化两类引用
  （`artifact://sha256/<digest>`、`workspace://<id>@<immutable-revision>[/<path>]`）。
- **可变 revision 在解析期即拒绝**：`main`/`HEAD`/`latest`/`refs/...`/分支名一律拒绝，
  因为它们会让重放依赖环境而非记录历史（I2/I9）。
- `runtime/content.py`：按 kind 注入 resolver；未注册 kind → `ContentUnavailable`；
  产物缺失 → 抛错而非返回空。**没有回退到 live workspace 的路径。**
- `ContentRefError` 故意不是 `ValueError`：pydantic 会把 validator 里的 ValueError 包成
  `ValidationError`，掩盖拒绝原因。
- 架构测试已把前缀所有权收紧到 `domain/content.py` / `runtime/content.py`。
- 证据：`tests/test_content_ref.py` 21 passed（往返、可变 revision 拒绝、路径穿越、
  缺失失败关闭、未注册 kind 失败关闭）；全量测试见下。

## 质量门禁（2026-09-09）

- 现状：有分层纪律，但**零自动强制**（无 ruff/mypy/架构测试/依赖契约）。
- 新增四道门：`tests/test_architecture.py`（依赖方向、边界所有权、反模式棘轮）、
  `scripts/quality_gate.py` + `quality-baseline.json`（指标只减不增）、ruff、mypy（棘轮）。
- 基线：ruff `C901: 13`、mypy `238`、最大模块 `api/app.py 708`（单独预算 750）。
- **门禁直接发现并修复的存量缺陷**：
  - `state/operations.py` 导入 `runtime.artifacts/settings`（分层违规）→ 改为注入 `read_artifact`；
  - `runtime/agent_tools.py` 闭包捕获循环变量 `capability`（潜在错配 bug）→ 绑定默认参数；
  - `checkpoints.py`/`protocols.py` 注解缺 `datetime`、`operations.py` 缺 `NodeRun`；
  - 3 处死赋值、14 处未使用导入。
- 负向测试已验证：注入违规文件时架构门禁确实失败。
- 规则与 DoD 见 `QUALITY_GATES.md`。

## 内容面架构决策（2026-09-09，ADR-026）

- 现状：控制面约 3077 行、内容面约 242 行（12.7:1）。工作区从未被建模，原因是
  ADR-012 的 "private workspace" 指**每次工具调用的临时沙箱目录**，同名词掩盖了缺口。
- 决策：新增内容面，但**不引入第二真相来源**。模型是 **Recovery Closure**——控制事件历史
  决定"哪些 revision 属于该 run"，内容存储拥有其字节，两者由不可变 `content_ref` 连接。
- I2 已改写为 Canonical Recovery Closure；I9 拆分为 R0–R3 复现等级（只有 R0/R1 是保证）。
- 已收敛、不再讨论：Git-first（代码）+ CAS-backed（通用树），统一 `WorkspaceRevision` 且
  Anchor 自算 `tree_digest`；graph **声明**契约、run **实例化**工作树、节点级 revision 血缘；
  `merge_policy: require_clean`；验证器只读冻结 revision；并发靠不可变性而非写锁。
- 文档：`WORKSPACE.md`（v2 边界）、`CONTENT_COMMIT_PROTOCOL.md`（提交/对账 + 故障注入矩阵）、
  `WORKSPACE_STORAGE.md`（CoW/配额/可达性 GC）、`docs/deep-research-report.md`（外部调研）。
- 证据声明：调研报告结论与本决策一致，但其引用以内部标记交付、无可点击 URL，暂作方向性证据，
  待补引用附录后方可作为可核查依据。
- 下一步：W0（只读工作区 + `content_ref`）设计细化；调研引用附录；两份工程文档的 benchmark。

## 架构精简（2026-09-08，ADR-021）

- `relational.py` 1599 行/81 方法 → 23 行组合门面 + 6 个按事务边界拆分的 mixin
  （base/graphs/execution/checkpoints/operations/progress），事务核心仍是同一份。
- 删除 387 行 `execution_policy.py`（约 300 行零消费者）与重复的
  `ProgressEvidence`/`DiagnosticRequest` 定义；记录归位 `domain/models.py`。
- `runtime/propagation.py` → `domain/propagation.py`，`state/`/`domain/` 不再依赖
  `runtime/`，分层约束成立。
- 删除重复的 `claim_ready_node` 实现与死代码 `checkpoint_node_result`，提取
  `_locked_running_node` 消除 lease 守卫重复。
- 内核不再认识学术领域：新增 `runtime/behaviors.py`（`NodeBehavior` + 注册表），
  学术策略在 composition root 注册；工具证据裁剪改由 `ToolCapability` 声明。
- 前端共享类型/标签表收敛到 `execution.ts`，去掉 `any[]`；诊断与进展证据同时显示在
  Run Console 与图上执行面板。
- 证据：全量 293 passed + 62 skipped；前端 Vitest 31、Playwright 11/11。

## 滚动清理（2026-09-09，ADR-025）

- 调度器按 `ANCHOR_STORAGE_SWEEP_INTERVAL`（默认 60s）执行滚动清理：超预算时按最老优先
  淘汰终止态 run，直到回到预算内。
- 保护集永不删除：非终止态、持有活跃 lease、等待审批/事件、`outcome_unknown` 未对账。
- 产物回收为 mark-sweep（含 context snapshot JSON 内嵌引用），行删除按 FK 子先父后单事务，
  随后 VACUUM（SQLite 需要，PG 内部回收）。
- `GET /api/retention/preview` 干跑；`POST /api/retention/sweep` 手动执行；
  `GET /api/retention/audit` 记录每次条数与释放字节（不含内容）。
- 无预算时清理是 no-op；`ANCHOR_STORAGE_ENFORCE=false` 可完全关闭，预算退回仅提示。
- 证据：后端 313 passed + 67 skipped；PG 67 passed；前端 Vitest 31、Playwright 15/15；
  迁移 head `0017_retention_audit`。

## 存储预算与历史整理（2026-09-09，ADR-024）

- Run 归档：`archived_at` 可逆标记，仅终止态可归档；默认列表隐藏，按 id 与所有子集合仍可查；
  `POST /api/runs/{id}/archive|unarchive`；列表支持 `status` / `include_archived` / 前端搜索。
- 存储预算：`storage_budgets` 表（migration `0016_storage_budgets`）保存全局与每图预算，
  `GET/PUT /api/storage/budget` 运行时可调，无需重启。`GET /api/storage` 只读报告真实占用
  （数据库文件 + 产物目录），产物按图归因并区分 exclusive/shared（内容寻址跨图共享）。
- 预算只是监控目标，**绝不终止正在运行的节点**，也**不会自动删除**；自动淘汰、产物 GC 与
  保留窗口留待后续，且必须策略化、可审计。
- Web 新增“存储”视图：实时调整全局/每图预算并查看占用。
- 证据：后端 307 passed + 66 skipped；PG 66 passed；前端 Vitest 31、Playwright 14/14；
  迁移 head `0016_storage_budgets`。

## MVP 收尾：对账与运行控制（2026-09-09，ADR-023）

- 副作用工具 `http.post`：`side_effect` 声明 + `owner_agent` + 已完成审批前置才执行；
  网关默认拒绝，agent tool loop 永远拿不到。传输失败（超时/断流）→ `outcome_unknown`
  （`transport_unknown`），确定的 HTTP 错误 → `failed`。私有/回环地址默认拒绝，需
  `allow_private_network` 显式开启。
- 对账闭环：`POST /api/operations/{id}/reconcile`（succeeded/failed + 证据引用）→ 账本
  确定性完成或失败该节点；节点在未知期间保持 running 与 lease，工具 lease 不可走普通
  恢复路径；不新增 attempt。Run Console 工具账本提供“对账并解决节点”。
- Run 暂停/恢复：`POST /api/runs/{id}/pause|resume`。暂停后不再领取新节点，已运行节点
  完成且下游保持 ready；恢复后继续领取。取消仍为终止操作。
- 触发器管理 UI：按已发布版本注册/启停 manual、cron、interval、内部事件、webhook；
  webhook 仅保存 secret 引用。
- 证据：后端 300 passed + 64 skipped（含 `http.post` 成功/失败/未知/私网拒绝、审批门、
  对账 API、暂停恢复）；PG 64 passed；前端 Vitest 31、Playwright 12/12（新增触发器管理）。

## 画布布局与连线（2026-09-09，ADR-022）

- 自动分层布局改用 `@dagrejs/dagre`（MIT，3.1.1）：无保存坐标的图按拓扑分层，
  按拓扑缓存，轮询/改名不会移动画布；用户拖动的 `layout.positions` 仍然优先。
  执行面板按自己的节点尺寸（240×146）单独布局。
- 连线：前向边用贝塞尔并按同源扇出变化曲率；回边（target 在 source 左侧）拆成两段
  正交路径从图下方绕行；标签只在 hover/选中显示，未选中的已决边用虚线，选择状态靠
  颜色/线型而非常驻文字；`interactionWidth` 26 便于点选。
- 修复 React Flow #015：编排画布此前传受控 `nodes` 却没有 `onNodesChange`，且每次渲染
  重建节点对象丢掉 `measured`，拖动即报“node is not initialized”并丢连线。现改用
  `useNodesState` + `onNodesChange`，同步文档时保留 `measured`，拖动期间跳过同步；
  `edgeTypes` 提升为模块常量（React Flow 要求身份稳定）。
- 回归：Playwright 新增“拖动后连线不丢”断言；Vitest 31、Playwright 11/11。
- 真实深度调研 E2E（DeepSeek `deepseek-v4.1-flash-expires-on-0910`，academic-research v3）：
  Run `39d5db63-a35b-42d3-9510-058fcc3a3c5b` COMPLETED，2 轮修订（review a0=revise →
  a1=pass），158 事件，耗时约 25 分钟；报告 29,502 字，导出
  `.local/artifacts/reports/<run_id>/report.md`（48,080 字节）。

## 执行策略落地（2026-09-08，ADR-019）

- `execution_policy.py` 新增限制分类（transport / resource_capacity / operator_policy /
task_behavior）、观察状态映射、`ProgressEvidence`、`DiagnosticRequest`、故障分类。
- 默认解除隐藏上限：`run_timeout_seconds` 与 `max_rounds` 不再有隐含默认值；
  `ANCHOR_EXPIRE_RUN_BUDGETS` 默认 false；`output_retries` 默认 0。
- 业务循环、节点 attempt（含故障重试）、请求重试三者计数独立，互不消耗。
- `watchdog.py` 升级为持久化观察器：写 `progress_evidence`，完整循环重复且无已验证
  进展时登记去重 `diagnostic_requests`，不终止 Run。
- 故障恢复计划持久化（migration `0014_recovery_schedule`）：失败 attempt 记录
  `last_error_class`，新 attempt 记录 `next_attempt_at`；claim 在到期前拒绝；尊重
  Retry-After；worker 不再进程内 sleep，重启不丢失退避。
- 新迁移 `0013_progress_evidence` / `0014_recovery_schedule`；新 API
  `/api/runs/{id}/progress`、`/api/runs/{id}/diagnostics`、`.../diagnostics/{id}/supersede`；
  Run Console 新增“诊断”和“进展证据”区域。
- 全量后端 293 passed + 62 skipped；PG 参数组 62 passed；前端 Vitest 31 passed、
  Vite build 通过、Playwright 11/11 通过（需 `ANCHOR_BROWSER_CHANNEL=chrome`，本机
  已装的 Playwright 浏览器版本与 1.58 期望的 headless shell 不一致）。
- 隔离库真实进程 E2E 双路径通过（见下文“真实 Verifier 隔离进程 E2E”）。

## 架构瘦身（2026-09-06 深夜，用户指令：做高级架构师，瘦身优先）

- 第一刀：删除 `src/anchor/state/sqlite.py`（1542 行原型）。`RelationalStateStore` 是唯一实现，同时服务 PG 与 SQLite。6 个测试文件迁移到 `tests/conftest.py` 的共享 migrated-SQLite fixture；`anchor.state` 不再导出 `SQLiteStateStore`；`sqlalchemy`/`alembic` 移入核心依赖（不再是 storage extra），`pydantic-settings` 新增为核心依赖。全量 `173 passed + 55 skipped`，与下刀前一致，零回归。
- 第二刀：31 处散弹 `os.environ.get` 收敛为 `src/anchor/runtime/settings.py` 的 `AnchorSettings(BaseSettings)`，类型校验前置，历史错误信息原样保留；7 个 service + API 全部改用；新增 3 个 settings 测试。dev API 已重启验证，readiness 与 capabilities 正常。
- 冻结中：eval 相关 PR 必须先出 `pydantic-evals` 适配 spike（仍有效）。
- 新环境克隆验证：git clone 到 /tmp 后建 venv、装 `.[dev,storage,api]`、migrate 到 head 0011、抽测通过；缺的只有 `.local`（runtime profile 按 README 从 `examples/runtime.codex.json` 重建，文档已有）。
- Goal 01a071a8 已按用户授权标为 complete（Codex goals 库）。
- 深度调研 loop v4/v5：v3 空转 6 轮暴露数据流缺陷（review 拿不到 questions），v4 加 plan→review 直连边修复，v5 首轮 pass terminal（verdict=pass round=1，25 事件单调）。结论：循环收敛靠信息流完备 + 标准与能力对齐，不是靠多跑几轮。
- 深度调研 v2 实战（reviewer 独立角色，真实课题）：3 agent 完成，verifier 以具体理由 rejected（人工审批集成未覆盖、部分来源未核实），Run 按设计失败关闭，证据链完整。这是门在正常工作的证明，不是故障。
- 深度调研图实战：dev 上发布 `deep-research` v1（Web 可见 scount→analyst→critic→verify→gate→report），Bundle 导出后在隔离库导入（hash 一致），四进程 + 真模型跑通：3 agent + verifier passed + 人工 approve + report terminal，27 事件单调。dev 库因有旧 ready 节点未起共享 worker。
- PG 对等重验（relational 大改后）：一次性 PG17 上 relational+api+admission+approval 共 156 passed，容器已删。dev API 已重启，waits 端点 live，四服务 active。
- Tool 节点执行器落地（ADR-018）：control 认领 + 网关调用 + owner_agent + snapshot 参数；拒绝/失败 loud；顺手修了 control/tool 认领与恢复集合混淆（tool lease 永不可走 lease 恢复，supervisor 保持 unknown）。真实 Loop E2E（gpt-5.6-luna 三进程）：迭代 + 退出 + 复活 + terminal，26 事件单调。全量 230 绿。
- Loop 执行器落地（ADR-017，migration 0011）：control pass-through + 按 attempt 重入 + 决议按源 attempt 作用域 + skipped 复活 + 决议新旧消歧；PG 162 过（含迁移往返）。全量待终验。
- 经验晋升闭环 v1 落地（ADR-016）：propose → review → promoted 进 prompt（domain 打标），write-back 明确推迟；语义 judge 基线落地（离线 coverage tripwire + LLM judge 接口位）。全量 223 绿。
- Bundle 文件包落地（ADR-015）：`GraphBundle` + 导出/导入端点（hash 验签、trigger 重绑）+ Web 导出/导入；全量 218 绿，前端 e2e 8/8。
- Tool-use 接线落地（ADR-014）：模型定调什么、网关定能否跑；`AgentToolLoop` + `generate_with_tools` + 确定性 operation id + 拒绝即消息；TestModel 离线真循环证明 + worker 无 tools/loop 时零行为变化。全量 216 绿。
- Approval/Wait durable 落地（ADR-013）：`waiting_approval`/`waiting_event` 阻塞 terminal、无 worker 可领、decide/resume 与 lease 路径共享 `_propagate_completion` 尾巴；`GET /api/waits` + approve/reject/resume + Run Console 等待区。store 6 + API 2 用例，全量 212 绿，前端 e2e 8/8。
- 沙箱 v1 已落地（政策自研 + 隔离组装，ADR-012）：`ToolGateway`（注册/作用域/副作用拒绝/凭证拒绝）+ `BubblewrapBackend`（本机验证：无网络、根只读、工作区可写）+ ledger 全链（register/start/finish，重放返回持久化结果不重执行）+ `tests/test_tool_gateway.py` 11 用例。全量 204 绿。worker 接线（agent tool-use loop）与审批门待后续。
- 子图组合 v1 已落地（发布时物化，ADR-011）：`NodeType.SUBGRAPH` + pin + `expand_subgraphs`（命名空间、入口/终点重连、mapping 改写、provenance 元数据）+ `publish_draft` 解析（仅已发布版本可引用、环拒绝）+ `tests/test_subgraph.py` 8 用例（含 store 层实际跑通 expanded parent 到 terminal，证明执行面零改动）+ Web 子图节点类型/图标/版本输入。全量 193 绿。独立子 run（需 wait 原语）与 Bundle 格式待后续。
- 防漂移机械门 v1 已落地：`src/anchor/runtime/integrity.py` 纯只读检查器（snapshot hash 重算、generation 稠密、pin hash、decision 结构、证据可读、verification 绑定）+ `tests/test_integrity.py` 10 用例（含 SQL 注入 corruption）+ 挂进 Agent `_resolver`（`IntegrityError` 即留 lease 待监督，等同 transport failure 语义）。全量 185 绿。语义 judge 层、control/verifier 路径接入门待后续。
- `pydantic-evals` spike 已有结论（脚本 `/tmp/spike_evals.py`，2.40.0）：4/4 通过。 deterministic JMESPath 与 strict-JSON verdict 均可表达为 task + Evaluator，report 给出 per-case assertion/score/label，OTel 钩子具备。**采纳引入**，但边界严格：evals 只做离线评分（`evals` extra），在线 gate 零新增依赖；strict-JSON 解析规则抽取为纯函数 `parse_model_verdict` 由 worker 与 eval 任务共享。已落地：`src/anchor/runtime/eval_verifiers.py` + `tests/test_verifier_evals.py`（7 case 矩阵）+ ADR-010，全量 175 绿。经验晋升评审以后复用同一原语。
- `pydantic-graph` spike 已有结论（脚本 `/tmp/spike_pygraph.py`，2.40.0）：分支奇偶性通过（Anchor edge 0 selected / edge 1 rejected 与 graph path route→left→join 一致），但**否决引入**：无 persistence/snapshot 模块（单进程内存执行，verifier/approval/human 跨进程挂起恢复无钩子）、无 skipped-cascade/join-wait 语义（未走分支永不执行，join 无法等待“全部入边决议含未选中”）、不产出审计记录（edge index/evaluator/evidence 仍需手写层）、源码明确 `TODO: Support adding subgraphs`（P1 子图递归无望）。`propagation.py`（224 行纯函数、全测试、产证据）保留为内核，不再评估替换。Loop/子图执行后续优先考虑 Temporal/Prefect 类 durable 执行器，而非内存图库。

## 一分钟接手摘要

```text
Project: /home/mansteinl/Anchor
Goal: 01a071a8-98b8-7591-8035-a9924a336424
Goal state: paused（用户为切换 session 主动暂停，不是 blocked 或 complete）
Web: http://127.0.0.1:5173
API: http://127.0.0.1:8090
Source migration head: 0017_retention_audit
Local development DB revision: 0017_retention_audit
Model profile: rightcode / gpt-5.6-luna / Responses API
Current P0: Verifier 与执行策略已验收；下一步按优先级推进 Approval/UI、pause/resume 或 Loop 监督
First test: .venv/bin/pytest -q tests/test_relational_store.py -m 'not postgres' -x
```

当前已经真实走通：

```text
Graph Version
-> Task/Run admission
-> transactional outbox
-> durable receiver/inbox
-> typed worker claim/lease
-> Agent model call 或 deterministic control execution
-> content-addressed artifact
-> immutable context snapshot
-> persisted edge decisions
-> conditional/multi-predecessor propagation
-> downstream NodeRun
-> terminal Run/Task
-> Web Run Console
-> persistent progress evidence + diagnostics
```

独立 Verifier 已完成 SQLite/PostgreSQL/全量/真实进程双路径 E2E 验收；执行策略（ADR-019）已完成限制分类、默认解除隐藏上限、独立计数与持久化进展观察。Agent/control 基础闭环与 Verifier 均已验收，不要重新实现。

## Goal 与产品目标

Goal ID：

```text
01a071a8-98b8-7591-8035-a9924a336424
```

Goal objective：

```text
完成 /home/mansteinl/Anchor 的可运行多 Agent Graph MVP：实现从 Graph Version、
Task/Run admission、durable queue、worker claim、模型调用、artifact/context 持久化、
节点完成与下游传播，到终端 Run 完成的真实闭环；提供可长期运行的 worker 服务和
Web Run Console；保持 SQLite/PostgreSQL 语义一致、事件可审计、外部副作用可追溯且
未知结果不盲目重试；在此基础上完成基础 Agent/Model/Tool capability 配置、
手动/定时/事件触发的可扩展边界，并通过全量自动化测试和本地运行验收。
```

Goal 当前是 `paused`。这是用户为了整理和新开 session 主动暂停，不是技术阻塞。新 session 获得用户继续指令后应恢复推进；除非通过完整完成审计，否则不得把 Goal 标记为 `complete`。

产品北极星只有一个：构建用户友好的 Web 多 Agent Graph 编排和运维平台。用户能够在 Web 中定义、校验、发布不可变 Graph Version，并把它固化为手动、时间、内部事件或 webhook 触发的长期工作流；平台必须保证长时间运行、证据可追溯、明确恢复和失败关闭，而不是只做一个短命的聊天或流程演示。

## 不可破坏的架构约束

- 不以 `max_steps`、`max_tokens`、`max_iterations` 等用户无法事先合理预测的预算控制健康 Agent 的正常工作。
- 监督重点是 stale heartbeat、worker 断开、deadlock、重复循环、无进展和未知外部结果。正常运行可以持续很久；断开和无进展必须可观测、可处置。
- 外部副作用必须先写 operation ledger，再允许执行。
- Tool `outcome_unknown` 必须 fail-closed 并进入 reconciliation，禁止盲目重试。
- Agent、deterministic control 和 Verifier lease 只能在操作员明确确认原进程已中断后恢复。不得偷取 active lease。
- Tool lease 不得走普通 recovery；未知外部结果必须使用专用 reconciliation 协议。
- 模型、Agent、Verifier 和 Tool capability 必须供应商中立，Graph 只引用 capability，不绑定某家 API wire format。
- 密钥只允许在运行时 `SecretProvider` 边界解析。禁止把密钥写入 Graph、事件、artifact、浏览器 storage、日志或提交记录。
- 模型自然语言声称“完成”不能替代 canonical NodeRun/Run 状态、持久化证据或确定性验证结果。
- SQLite 和 PostgreSQL 必须维持相同的状态机、事件顺序、事务原子性和恢复语义。
- 核心状态转移使用 append-only、单调 sequence、idempotency key 和不可变证据支持审计。
- Anchor 拥有 canonical Graph IR、状态机、恢复语义、context/memory policy、质量门和产品 Web；通用设施优先组装成熟、流行且许可友好的开源组件。
- 不要为了“灵活”增加没有真实消费者的参数、模块或导出；产品表面应尽量少暴露需要用户猜测的运行参数。

## 已完成并验收的基础闭环

以下能力此前已经通过自动化测试或隔离进程 E2E，不要重新搭脚手架：

- Graph draft、revision、validation、immutable publication 和 content hash。
- Graph IR 节点类型：`agent/tool/router/parallel/join/verifier/approval/wait_for_event/human_task/artifact/loop`，以及 duplicate、未知端点、entry、unreachable、cycle、exit path 和 webhook secret 校验。
- manual、interval、cron、internal-event 和 webhook trigger/admission 边界。
- Task/Run admission、transactional outbox、durable receiver/inbox、幂等接纳和固定 Graph Version。
- Agent typed claim、lease、heartbeat 和显式人工恢复。
- PostgreSQL 使用 `SKIP LOCKED` 并发领取。
- `gpt-5.6-luna` Responses API 真实模型调用。
- content-addressed artifact 和读取时 SHA-256 完整性检查。
- immutable context snapshots、context generation、恢复时复用 canonical snapshot。
- JMESPath 条件求值，发布时检查语法、运行时严格 boolean，不使用 Python `eval`。
- durable `EdgeDecision`，包含 evaluator/version、context hash 和 evidence ref。
- `skipped` 分支语义、级联跳过和 selected-only input mapping。
- Router、Parallel、Join、Artifact 专用 deterministic control worker。
- 已知执行失败原子写入 `node.failed`、`run.failed` 和 `task.failed`。
- Tool operation ledger 和 `outcome_unknown`/reconciliation 状态机基础。
- Web Graph Builder 和来自持久化 API 的真实 Run Console，不显示 synthetic progress。
- Agent/control 独立进程 E2E，以及 Agent 进程中断、stale assessment、人工恢复证据。

## 独立 Verifier（已验收）

Verifier 垂直切片已验收。实现文件：

```text
src/anchor/domain/models.py
src/anchor/runtime/capabilities.py
src/anchor/runtime/config.py
src/anchor/runtime/sinks.py
src/anchor/runtime/verifier.py
src/anchor/runtime/verifier_service.py
src/anchor/runtime/supervisor.py
src/anchor/state/protocols.py
src/anchor/state/schema.py
src/anchor/state/execution.py
src/anchor/api/app.py
migrations/versions/0010_verification_records.py
infra/systemd/anchor-verifier-worker.service
apps/web/src/RunConsole.tsx
```

### 领域记录

`VerificationVerdict`：

```text
passed
rejected
error
```

`VerificationRecord` 已包含：

```text
verification_id
claim_id
run_id
node_run_id
node_id
verifier_ref
verifier_version
adapter
adapter_version
verdict
reason
evidence_ref
verified_artifact_hashes
verified_context_hash
model_ref
model_provider
model_name
model_response_id
decided_at
```

### 已实现协议

```text
Verifier typed claim
-> rebuild selected-input context
-> resolve selected predecessor artifacts
-> verify artifact SHA-256 integrity
-> deterministic JMESPath 或 model JSON adapter
-> strict passed/rejected/error verdict
-> content-addressed evidence artifact
-> atomic VerificationRecord + ContextSnapshot transition
-> passed: node.completed + downstream propagation
-> rejected/error: node/run/task failed
```

关键语义：

- `claim_ready_verifier_node()` 只领取 `verifier` 节点。
- Agent worker 和 control worker 不能领取 Verifier。
- 通用 `complete_node_and_propagate()` 不能在没有 `VerificationRecord` 时完成 Verifier。
- 旧的 `checkpoint_node_result()` 也不能绕过 Verifier completion gate。
- 确定性 adapter 使用 JMESPath，表达式结果必须是显式 boolean。
- 模型 adapter 只接受严格 JSON：

  ```json
  {"verdict":"passed","reason":"specific evidence-based reason"}
  ```

  或：

  ```json
  {"verdict":"rejected","reason":"specific evidence-based reason"}
  ```

- 普通自然语言不能解释为通过；非 JSON 或 schema 错误产生 `error` verdict 并失败。
- 模型传输异常不会伪装成 rejection/error。Node 保持 `running`，lease 保留，等待 supervisor 检测和人工处置。
- `passed` verdict 与 content-addressed evidence、完整的已验证 artifact hashes 和 context hash 绑定。
- rejection/error 在同一事务中保存 `VerificationRecord` 与 `ContextSnapshot`，然后失败 Run/Task，不打开任何下游边。
- Verifier stale lease 可恢复，但仍要求操作员明确确认原进程已经中断。
- API 已增加：

  ```http
  GET /api/runs/{run_id}/verifications
  ```

- 新 readiness 代码增加：

  ```json
  {"verifier_worker_connected":false}
  ```

- Run Console 已增加“验证证据”区域。
- Graph Builder 的 Verifier 引用已连接 capability datalist。
- `.local/runtime.json` 已加入 `verifiers.evidence` model adapter 配置。

### 已获得的 Verifier 测试证据

以下结果已在此前 session 中通过：

```text
tests/test_verifier.py                                      7 passed
Verifier/capability/config/supervisor/admission 组合       52 passed
tests/test_api.py SQLite 参数组                            passed
前端 Vitest                                                19 passed
前端 Vite build                                            passed
compileall                                                 passed
隔离 SQLite alembic upgrade -> 0010                        passed
RelationalStateStore.check_schema on 0010                  passed
```

`tests/test_verifier.py` 覆盖：

- deterministic passed；
- deterministic rejected；
- model passed；
- model rejected；
- model 非 JSON 转为 `error`；
- model transport failure 保留 running Node 与 lease；
- Agent/control 不能误领 Verifier；
- completion gate；
- Verifier 人工 recovery。

这些专项结果不能替代尚未完成的关系型、PostgreSQL、全量和进程 E2E 验收。

## 被暂停的 relational contract

最后新增了关系型 store 的 Verifier contract，同一参数化测试用于验证 SQLite 和 PostgreSQL：

- typed claim；
- 无 verdict 无法完成；
- completion transaction 回滚；
- 人工 recovery；
- passed record/context 持久化；
- rejected record/context 与 Run failure；
- store reopen 后证据仍存在；
- PostgreSQL parity。

第一次运行暴露的是测试选择错误，不是已确认的 store bug：测试取了第一个旧 pending dispatch，而不是第二个 rejection Run 对应的 dispatch。测试已修正为按 `run_id` 选择：

```python
rejected_dispatch = next(
    item for item in store.pending_dispatches()
    if item.run_id == rejected_receipt.run_id
)
```

修正后的测试已在新 session 通过：`25 passed, 25 deselected`（SQLite 组执行，PostgreSQL 组按预期 skip）。

```bash
cd /home/mansteinl/Anchor
.venv/bin/pytest -q \
  tests/test_relational_store.py \
  -m 'not postgres' \
  -x
```

## Verifier 契约（已逐项验收）

- `VerificationRecord` identity 必须匹配 claim、run、node_run、node 和 `verifier_ref`。
- `verified_context_hash` 必须等于同一事务即将持久化的 `ContextSnapshot.input_hash`。
- artifact hash 必须来自经过 `ArtifactStore.get_text()` 完整性验证的 artifact ref。
- `verified_artifact_hashes` 必须排序、去重，并且每项都是 64 位十六进制 SHA-256。
- passed Verifier 的 NodeRun `output_ref` 必须是该次验证的 evidence artifact。
- rejected/error 不得打开任何下游边。
- model transport exception 不得写入虚假的 error/rejected verdict。
- outgoing routing condition 失败时，已经持久化的 Verifier verdict 与 routing failure 都必须保留并可审计。
- SQLite 和 PostgreSQL 的事件顺序、context generation、回滚与重开 store 后结果必须一致。
- `verification.decided` 事件必须在 `node.completed` 或失败事件之前。
- 普通 Agent/control checkpoint 不能绕过 gate。
- `resolve_node_context()` 会读取 NodeRun artifact；确保未选择分支不会进入 Verifier target。
- 模型 prompt 可以截断展示 artifact 正文，但 record 必须绑定完整 artifact SHA-256；最终文档要明确这个语义。

尚未要求在本 P0 中解决、但需要记录的长期问题：Graph Version 当前固定 `verifier_ref`，capability 配置保存在运行时；record 已保存 verifier version 和模型快照，但后续仍需正式确定 capability snapshot/version publication policy。

## 迁移和开发数据库状态

本次核对结果（2026-09-08）：

```text
Source Alembic head: 0017_retention_audit
/home/mansteinl/Anchor/.local/api.sqlite: 0017_retention_audit（已迁移）
```

`RelationalStateStore.check_schema()` 接受 `0012_lease_history`、`0013_progress_evidence`、`0014_recovery_schedule`、`0015_run_archive`、`0016_storage_budgets` 与 `0017_retention_audit`。开发库已迁移并重启 API，readiness 当前为：

```json
{"status":"ready","execution_connected":true,"worker_connected":false,"control_worker_connected":true,"verifier_worker_connected":false}
```

`worker_connected` / `verifier_worker_connected` 为 false 是共享 worker 未启动时的预期值，不要为把它变 true 而启动 worker 消费旧 Run。

正确顺序：

1. 完成 Verifier SQLite、PostgreSQL、全量和隔离进程 E2E 验收。
2. 再迁移开发库：

   ```bash
   ANCHOR_DATABASE_URL=sqlite:////home/mansteinl/Anchor/.local/api.sqlite \
     .venv/bin/alembic upgrade head
   ```

3. 重启 API。
4. 检查 readiness 包含 `verifier_worker_connected`。
5. 不要为了让该值变成 `true` 而启动 worker 消费旧 Run。

## 当前服务状态

本次核对时间：`2026-09-08 22:00 CST`。

```text
anchor-api-dev.service                   active / loaded
anchor-receiver-dev.service              active / loaded
anchor-web-dev.service                   active / loaded
anchor-supervisor-dev.service            active / loaded（已重启加载新 watchdog 代码）
anchor-control-worker-dev.service        active / loaded
anchor-worker.service                    inactive / user unit not found
anchor-verifier-worker.service           inactive / disabled（unit 已安装，保持禁用停止）
anchor-scheduler-dev.service             active / loaded
```

源码中已有：

```text
infra/systemd/anchor-verifier-worker.service
```

收尾会话已安装用户级 unit 到：

```text
/home/mansteinl/.config/systemd/user/anchor-verifier-worker.service
```

保持 `disabled` + `inactive`，不要 enable/start。

完成所有测试后，可以只安装 Verifier unit，但保持禁用、停止：

```bash
install -m 0644 \
  /home/mansteinl/Anchor/infra/systemd/anchor-verifier-worker.service \
  /home/mansteinl/.config/systemd/user/anchor-verifier-worker.service
systemctl --user daemon-reload
systemctl --user is-enabled anchor-verifier-worker.service || true
systemctl --user is-active anchor-verifier-worker.service || true
```

不要运行整个 `scripts/install_user_services.sh`。该脚本会 `enable --now` 多个 worker 和 scheduler，可能消费开发库中的旧 Run。

## 模型与 Secret 配置

当前 `/home/mansteinl/Anchor/.local/runtime.json` profile（2026-09-08 起）：

```text
models.codex.local  provider=openai_compatible  model=gpt-6-astra
                    base_url=https://apihub.cwise.dev/v1  secret_ref=OPENAI_API_KEY
models.deepseek     provider=deepseek
                    model=deepseek-v4.1-flash-expires-on-0910
                    base_url=https://api.deepseek.com/v1  wire_api=responses
                    secret_ref=DEEPSEEK_API_KEY
secret_file: /home/mansteinl/Anchor/.local/anchor-secrets.json
academic agents: model_ref=models.deepseek
```

最终验收按用户指令改用 Pi 的 DeepSeek 模型（调用方式在
`/home/mansteinl/.pi/agent/models.json`，provider `DeepSeek`，OpenAI Responses API），
不再使用 A6API。桥接脚本：`scripts/import_pi_secrets.py`（只写 secret ref，不打印 key）；
模板：`examples/runtime.deepseek.json`。已实测：`gateway.generate` 返回 `OK`，
`generate_with_tools` 能完成一次真实工具调用。

绝不打印、复制、转存或提交任何 key（`.codex/auth.json`、`.pi/agent/models.json`、
`.local/anchor-secrets.json`）。不要在诊断命令中输出环境变量或请求 Authorization
header。Secret 只能由运行时 `SecretProvider` 读取。模型错误不得被测试桩或普通文本改写为成功。

## 代码地图

Domain 和 admission：

```text
src/anchor/domain/graph.py
src/anchor/domain/models.py
src/anchor/domain/admission.py
src/anchor/domain/operations.py
```

State、migration 和协议：

```text
src/anchor/state/protocols.py
src/anchor/state/base.py        # 事务/锁/事件/心跳核心
src/anchor/state/graphs.py      # draft/version/trigger/admission/outbox/inbox
src/anchor/state/execution.py   # node/lease/claim/verification/edge decision
src/anchor/state/checkpoints.py # completion/failure/retry tail + waits
src/anchor/state/operations.py  # tool operation ledger
src/anchor/state/progress.py    # progress evidence + diagnostics
src/anchor/state/relational.py  # 组合门面（对外 API）
src/anchor/state/schema.py
migrations/versions/0007_webhook_secret.py
migrations/versions/0008_context_snapshots.py
migrations/versions/0009_edge_decisions.py
migrations/versions/0010_verification_records.py
migrations/versions/0011_decision_attempts.py
migrations/versions/0012_lease_history.py
migrations/versions/0013_progress_evidence.py
migrations/versions/0014_recovery_schedule.py
migrations/versions/0015_run_archive.py
migrations/versions/0016_storage_budgets.py
migrations/versions/0017_retention_audit.py
```

Runtime：

```text
src/anchor/runtime/behaviors.py     # NodeBehavior 接口 + 引用注册表
src/anchor/runtime/capabilities.py
src/anchor/runtime/config.py
src/anchor/runtime/secrets.py
src/anchor/runtime/model_gateway.py
src/anchor/runtime/worker.py
src/anchor/runtime/worker_loop.py
src/anchor/runtime/worker_service.py
src/anchor/runtime/control_worker.py
src/anchor/runtime/control_service.py
src/anchor/runtime/verifier.py
src/anchor/runtime/verifier_service.py
src/anchor/runtime/receiver.py
src/anchor/runtime/dispatch.py
src/anchor/runtime/scheduler.py
src/anchor/runtime/scheduler_service.py
src/anchor/runtime/supervisor.py
src/anchor/runtime/supervisor_service.py
src/anchor/runtime/artifacts.py
src/anchor/runtime/sinks.py
src/anchor/runtime/context.py
src/anchor/runtime/memory.py
src/anchor/runtime/resolution.py
src/anchor/domain/propagation.py    # 纯领域传播逻辑（已从 runtime 归位）
```

API 和 Web：

```text
src/anchor/api/app.py
apps/web/src/App.tsx
apps/web/src/RunConsole.tsx
apps/web/src/graph.ts
apps/web/src/style.css
```

重要设计记录：

```text
ARCHITECTURE.md
DECISIONS.md
RUN_ADMISSION.md
API.md
WEB.md
STATUS.md
README.md
```

实现事实的判断顺序：当前代码和 migration > 自动化测试与真实 E2E > canonical 数据库/API > 本文 > 其他文档。Verifier 收尾前，部分文档仍把 head 写成 `0009` 或把 Verifier 写成未实现；不要用旧文档覆盖新代码。

## 新 Session 首轮动作

先建立事实基线：

```bash
cd /home/mansteinl/Anchor
sed -n '1,760p' HANDOVER.md
git status --short --branch
.venv/bin/alembic heads
ANCHOR_DATABASE_URL=sqlite:////home/mansteinl/Anchor/.local/api.sqlite \
  .venv/bin/alembic current
systemctl --user is-active \
  anchor-api-dev.service \
  anchor-receiver-dev.service \
  anchor-web-dev.service \
  anchor-supervisor.service \
  anchor-worker.service \
  anchor-control-worker.service \
  anchor-verifier-worker.service \
  anchor-scheduler.service
```

然后立即运行被中断的测试：

```bash
.venv/bin/pytest -q \
  tests/test_relational_store.py \
  -m 'not postgres' \
  -x
```

不要先迁移 `.local/api.sqlite`，不要启动常驻 Agent/control/Verifier worker 或 scheduler，也不要用开发库做真实执行验收。

## Verifier 验收执行顺序（已执行）

严格按以下顺序推进并记录真实输出：

1. 修复并通过最新 relational SQLite contract。
2. 检查 `tests/test_relational_store.py` 中按 `run_id` 选择 rejection dispatch 的修改没有误改其他测试。
3. 运行 Verifier、API、admission、capability、config、supervisor、SQLite state 相关 Python 测试。
4. 启动一次性 PostgreSQL 17 容器，不复用任何未知数据库。
5. 在迁移到 head 的一次性 PostgreSQL 上运行 `tests/test_relational_store.py` 和 `tests/test_api.py`，确认 PostgreSQL 参数组实际执行而非全部 skipped。
6. 删除一次性 PostgreSQL 容器并确认测试端口释放。
7. 运行完整非 PostgreSQL Python suite。
8. 运行 `pip check`、`compileall` 和 `git diff --check`。
9. 运行前端 unit、build 和 Playwright。
10. 使用新建 `/tmp` 隔离库运行真实独立 `worker_service` + `verifier_service` + `control_service` 进程 E2E。
11. 核验 artifact/context/verification/event/downstream/terminal state 全部来自 canonical 持久化结果，并终止所有隔离进程。
12. 更新 `DECISIONS.md`、`STATUS.md`、`API.md`、`WEB.md`、`README.md` 和本文。
13. 迁移 `.local/api.sqlite` 到当前 head。
14. 重启并核验 API/Web，readiness 应包含 Verifier 字段。
15. 安装但不要 enable/start Verifier user systemd unit。

### 推荐回归命令

相关 Python 测试通过后运行：

```bash
cd /home/mansteinl/Anchor
.venv/bin/pytest -q
.venv/bin/pip check
.venv/bin/python -m compileall -q src tests
git diff --check

cd /home/mansteinl/Anchor/apps/web
npm test -- --run
npm run build
npm run test:e2e
```

Verifier 收尾会话取得的新证据（2026-09-06 深夜）：relational SQLite contract 25 passed；一次性 PostgreSQL 17（`postgres:17`，`127.0.0.1:55433`，已删除）上 `test_relational_store + test_api` 110 passed，relational 单文件 50 passed（SQLite 25 + PG 25，双组都执行）；全量非 PG 套件 170 passed + 55 skipped；`pip check`、`compileall`、`git diff --check` 通过；前端 Vitest 19 passed、Vite build 通过；Playwright 曾出现 1 failed（`real API lifecycle` 在发布处收不到“已发布 v1”），根因是 Verifier P0 新增发布时能力校验后，一次性 e2e API 的 runtime 缺少 `verifiers.evidence`，已在 `scripts/web_test_api.py` 补 model-adapter 测试配置，之后 Playwright 8/8 通过。真实隔离进程 E2E（`/tmp/anchor-verifier-e2e-dDpUDd`，已保留供核查后可删）：`Agent produce -> Verifier verify -> Artifact report`，Run `0738fd2d-a929-4fb3-b145-f033b7238018` COMPLETED，produce artifact 正文 `OK`，VerificationRecord 为 `passed` 且 evidence 与 verify 节点 output 一致，`verified_context_hash` 与 gen2 snapshot 一致，run stream 16 事件单调且 `verification.decided` 先于 `node.completed`；rejection Run `495e58bd-339a-4b79-a3ac-1a5bef7cbe0b` 按预期 `failed`（verify rejected，report 保持 pending，无下游打开）。隔离 receiver/agent/verifier/control 进程已全部终止并确认无残留；期间误杀的瞬时 `anchor-receiver-dev.service` 已按原方式重建，readiness `execution_connected:true` 已恢复。

执行策略会话取得的新证据（2026-09-08）：`tests/test_execution_policy.py` 14 passed；`tests/test_watchdog.py` 5 passed；全量 293 passed + 62 skipped；一次性 PostgreSQL 17（`postgres:17`，`127.0.0.1:55433`，已删除）上 `test_relational_store + test_api` PostgreSQL 参数组 62 passed；`pip check`、`compileall`、`git diff --check` 通过；前端 Vitest 28 passed、Vite build 通过、Playwright 11/11 通过（需 `ANCHOR_BROWSER_CHANNEL=chrome`）；隔离库真实进程 E2E 见上文双路径证据；`.local/api.sqlite` 已迁移至 `0014_recovery_schedule`，API 已重启，readiness 正常；一次性 PG 容器已删除，55433 端口已释放。

### PostgreSQL 验收要求

先检查本机容器运行时和已有容器，不要复用未知数据库。创建一次性 PostgreSQL 17 容器、选择独立端口、迁移到 head，并设置：

```text
ANCHOR_TEST_POSTGRES_URL=<disposable-postgres-url>
```

至少运行：

```bash
ANCHOR_TEST_POSTGRES_URL=<disposable-postgres-url> \
  .venv/bin/pytest -q \
  tests/test_relational_store.py \
  tests/test_api.py
```

验收输出必须显示 SQLite 和 PostgreSQL 参数组都执行。测试完成后删除一次性容器并确认没有残留监听端口或测试数据库进程。

## 真实 Verifier 隔离进程 E2E

已验收（2026-09-08）。可复现脚本：`scripts/verifier_e2e_setup.sh` 建立全新
`/tmp/anchor-verifier-e2e-XXXXXX` 隔离库、两个 Graph（pass 与 reject）并 dispatch；
随后分别用相同 `ANCHOR_ARTIFACT_ROOT` 启动 agent / verifier / control 三个真实进程。

本轮证据（`/tmp/anchor-verifier-e2e-IvrgOI`）：

```text
PASS   run feffb3bf-6a17-4f71-8562-7fdc16f4c037  COMPLETED
       produce artifact://sha256/565339bc…  (content "OK", integrity read OK)
       verify  passed, evidence == verify.output_ref
       verified_context_hash == gen snapshot input_hash
       verified_artifact_hashes == produce digest
       events: verification.decided(#9) -> node.completed(#10) -> node.ready(#12)
       report/Run/Task 均 completed；claim 分别为 agent/verifier/control
REJECT run ddd9c564-f589-4312-b390-71aa847e4c40  FAILED
       verify failed(verification_rejected)，VerificationRecord 已持久化
       report 保持 pending，无下游 ready
       events: verification.decided -> node.failed -> run.failed
reopen store 后两个 run 的 VerificationRecord 均完整保留
```

三个隔离进程已全部终止并确认无残留；共享 dev control worker 保持运行。

### 历史验收要点（仍适用）

必须使用全新 `/tmp` 目录：

```bash
verifier_e2e_dir="$(mktemp -d /tmp/anchor-verifier-e2e-XXXXXX)"
```

不要使用 `/home/mansteinl/Anchor/.local/api.sqlite`。推荐 Graph：

```text
Agent produce
-> Verifier verify
-> Artifact report
```

真实 capability：

```text
produce: agents.researcher
verify:  verifiers.evidence
```

Agent 输出应当明确、结构化并可验证。Verifier 必须从真实模型返回中解析严格 JSON verdict；禁止把自然语言响应手工改写为 `passed`。

在隔离数据库迁移、Graph 发布、Run admission 和 receiver acceptance 后，分别启动：

```bash
ANCHOR_DATABASE_URL=sqlite:///<isolated-db> \
ANCHOR_RUNTIME_CONFIG=/home/mansteinl/Anchor/.local/runtime.json \
ANCHOR_ARTIFACT_ROOT=<isolated-artifacts> \
ANCHOR_WORKER_ID=anchor-verifier-e2e-agent \
  .venv/bin/python -m anchor.runtime.worker_service
```

```bash
ANCHOR_DATABASE_URL=sqlite:///<isolated-db> \
ANCHOR_RUNTIME_CONFIG=/home/mansteinl/Anchor/.local/runtime.json \
ANCHOR_ARTIFACT_ROOT=<isolated-artifacts> \
ANCHOR_VERIFIER_WORKER_ID=anchor-verifier-e2e-verifier \
  .venv/bin/python -m anchor.runtime.verifier_service
```

E2E 必须证明：

- Agent output 是 `artifact://sha256/<digest>`，且读取时完整性校验通过。
- Verifier 通过 typed claim 领取，Agent/control worker 没有误领。
- `VerificationRecord` 持久化且 identity 与 claim/run/node 一致。
- `verified_context_hash` 与持久化的 `ContextSnapshot.input_hash` 一致。
- `verified_artifact_hashes` 与 selected 前驱的 output refs 一致。
- evidence artifact 是 content-addressed，并且 passed NodeRun 的 `output_ref` 指向它。
- `verification.decided` 事件先于 `node.completed`。
- 只有 persisted `passed` 才产生 downstream ready。
- Artifact 节点、Run 和 Task 最终正确终止。
- worker 进程结束后没有残留后台进程。

还应增加 rejection 或 error 路径的进程证据：VerificationRecord/context 被保存，Run/Task 失败，后继节点不会 ready。模型传输中断则应保持 running lease，不得持久化伪 verdict。

## Git 和工作区注意事项

仓库目前没有初始 commit：

```text
## No commits yet on master
```

`.gitignore`、源码、测试、迁移、前端、infra 和文档几乎全部显示为 `??`。这是当前项目状态，不表示文件可删除。普通 `git diff` 也不会显示未跟踪文件内容，不能据此认为工作区没有变化。

必须遵守：

- 保留全部未跟踪文件。
- 禁止 `git reset --hard`、`git checkout --` 和任何清理未跟踪文件的命令。
- 不要移动或删除开发库、artifact 或 runtime profile。
- 修改前先读当前文件；如果发现非预期变化，默认它来自用户或前一 session，必须与之兼容。
- 不要提交 secret、`.local` runtime 数据或测试数据库。

## Verifier 完成后的优先级

```text
P1 Approval/HumanTask/Wait durable states + authenticated API/Web
P1 ToolGateway + schema/capability/permission boundary + MCP
P1 pause/resume/cancel
P2 Loop executor + progress/deadlock/non-progress supervision
P2 A2A
P2 PostgreSQL memory/artifact projection
P2 long-term context compaction + memory conflict policy + vector retrieval
P2 retention/GC
P2 OpenTelemetry
P2 production auth + tenant isolation
P2 object storage
P2 backup/restore + multi-host/rolling restart/long-running acceptance
```

下一阶段仍应按垂直切片推进：领域状态与协议、SQLite/PostgreSQL 原子实现、runtime executor、API/Web、恢复行为、自动化测试、真实进程 E2E、运维与文档一起完成。不要只在前端增加节点外观，也不要只定义没有执行语义的 Graph 类型。

## 整个 Goal 尚未完成的能力

- MCP 协议适配器（ToolGateway 执行、审批门、对账流程已落地）。
- Loop executor 的进度信号与死锁检测已具备持久化观察与诊断；自动修复、死锁自动处置仍未实现。
- A2A。
- 长期 context compaction、memory conflict policy 和 vector retrieval。
- PostgreSQL memory/artifact projection。
- retention/GC。
- sibling cancellation 和完整 supervisor/recovery policy。
- OpenTelemetry。
- production auth、multi-user/tenant isolation。
- object storage。
- backup/restore。
- 多主机、rolling restart 和长时间 soak test。

已从本清单移除（2026-09-09）：Approval/HumanTask/Wait durable（ADR-013）、pause/resume/cancel
（ADR-023）、真实副作用工具与 reconciliation（ADR-023）、Web 触发器管理（ADR-023）。

Goal 不能仅因为 Agent/control E2E、Verifier 单元测试、一次模型成功返回、全套 service 显示 active 或当前某一阶段测试全绿而标记完成。

## 交给新 Agent 的提示词

```text
继续 Goal 01a071a8-98b8-7591-8035-a9924a336424，项目位于
/home/mansteinl/Anchor。Goal 当前因 session 交接被用户主动 paused，不是 complete。

先完整阅读 HANDOVER.md，并以当前代码、迁移、测试和运行状态核对文档。
保留所有未跟踪文件，禁止 git reset --hard、git checkout -- 和清理未跟踪文件。
不要打印任何 token/key。不要启动 Agent/control/Verifier worker 或 scheduler 消费
.local/api.sqlite；真实执行必须使用新建 /tmp 隔离数据库。

Agent/control durable execution、conditional propagation、edge decisions、skipped、
join、context snapshots、独立 Verifier（0010）与执行策略（0013，ADR-019）已完成并
验收，不要重新实现。

第一条测试运行：
.venv/bin/pytest -q tests/test_relational_store.py -m 'not postgres' -x

下一步优先级（ADR-019 后）：Approval/HumanTask/Wait 的完整 Web 操作面、pause/resume/
cancel、ToolGateway/MCP 与审批门、Loop 诊断的自动修复能力（需显式 capability 授权）。

保持核心语义：正常 Agent 不依赖 max_* 预算；默认无隐藏轮数/时长上限；业务循环、
节点 attempt 与请求重试计数独立；无进展只是未知，重复循环只触发诊断；Tool
outcome_unknown 不盲目重试；外部副作用先写 ledger；lease 只能人工确认恢复；
Verifier 只有结构化、持久化且绑定 artifact/context hash 的 passed verdict 才能完成
并传播。遇到改变状态机、持久化协议或向后兼容契约的关键架构决策时再停下来询问用户。
```

## 完成审计（两级门，ADR-017 时代修订）

MVP complete 只要求可交付的单机产品闭环；生产门按真实部署触发，不 blocking 交付。

### MVP 门（全部具备即可 complete）

- 全部目标节点类型具有真实、持久化、可恢复的执行或等待语义。
- 手动、定时、内部事件和 webhook 均能固定不可变 Graph Version 并完成真实闭环。
- 干净隔离库上的模型、工具、验证、等待/恢复和失败路径 E2E 成功。
- worker 长期运行、heartbeat、进程中断、rolling restart 和显式恢复可证明。
- 健康 Agent 不被用户难以预测的 `max_*` 预算干预。
- Tool unknown outcome 绝不自动重试，并具有可用 reconciliation 流程。**已完成**：`http.post`
  副作用工具 + 审批前置门产生 `outcome_unknown`；`POST /api/operations/{id}/reconcile`
  录入外部证据后确定性完成/失败节点，不新增 attempt；Run Console 提供对账操作。
- SQLite/PostgreSQL 协议、迁移、并发领取、事务回滚和重开持久化验收一致。
- context、memory、artifact、operation、verification、edge decision 和事件证据可追溯。
- Web Builder/Run Console 的状态全部来自持久化 API，支持用户完成编排、发布、触发、观察和人工处置。**已完成**：触发器管理面板（manual/cron/interval/事件/webhook，secret 仅引用）、
  Run 暂停/恢复/停止、审批/事件恢复、工具对账。
- worker 长期运行指单机常驻进程的 heartbeat、中断与显式恢复（多主机 rolling restart 移入生产门）。

### 生产门（按真实部署触发，不 blocking MVP complete）

- retention/GC 与 backup/restore 策略。
- production auth、权限边界、tenant isolation、OpenTelemetry 和部署运维。
- 多主机、rolling restart、长时间 soak、多租户验收。
- Prefect/Temporal 等 durable 执行 muscle（现有语义被证明不足时）。
- MCP/A2A、向量检索、对象存储（出现真实消费者时）。

在此之前，Goal 应保持 active/paused，而不是 complete。MVP 门证据（以本轮为准）：313 后端绿 + 67 skipped，PG 参数组 67 绿，前端 Vitest 31、Playwright 15/15；隔离库真实进程 E2E 双路径（Verifier pass/reject）；迁移 head `0017_retention_audit`。
