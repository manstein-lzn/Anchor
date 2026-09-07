# Anchor Session Handover

这份文档用于把 `/home/mansteinl/Anchor` 交给新的 Codex session 继续开发。它记录的是当前工作区事实、已经获得的测试证据、尚未验收的改动和下一步执行顺序。新 session 应先用代码、迁移、测试和运行状态核对本文，不要只依赖历史对话。

本次交接核对时间：`2026-09-06 21:45 CST`；Verifier 收尾会话于 `2026-09-06 22:30 CST` 左右完成下述验收并更新本文；架构瘦身会话（同日深夜）完成第一刀、第二刀并更新本文。

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
Source migration head: 0010_verification_records
Local development DB revision: 0009_edge_decisions
Model profile: rightcode / gpt-5.6-luna / Responses API
Current P0: 完成独立 Verifier 的关系型、PostgreSQL、全量和真实进程 E2E 验收
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
```

独立 Verifier 垂直切片的主体代码、迁移、API、Web UI 和专项测试已经写入，但最新 relational contract 修复后的测试被 session 暂停打断，尚未完成 PostgreSQL、全量回归和真实 `verifier_service` 进程 E2E。因此不要把 Verifier 或整个 Goal 标记为完成，也不要重新实现已经验收的 Agent/control 基础闭环。

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

## 当前 P0：独立 Verifier

Verifier 垂直切片正在收尾。主体实现已写入以下文件：

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
src/anchor/state/relational.py
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

## Verifier 收尾前必须审查的契约

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

本次核对结果：

```text
Source Alembic head: 0010_verification_records
/home/mansteinl/Anchor/.local/api.sqlite: 0010_verification_records（收尾会话已迁移）
```

`RelationalStateStore.check_schema()` 新代码已经要求 `0010_verification_records`。本地 API 当前仍返回 HTTP 200，但响应仍是旧进程加载的格式：

```json
{
  "status":"ready",
  "execution_connected":true,
  "worker_connected":false,
  "control_worker_connected":false
}
```

收尾会话已迁移开发库并重启 API，readiness 现为：

```json
{"status":"ready","execution_connected":true,"worker_connected":false,"control_worker_connected":false,"verifier_worker_connected":false}
```

`verifier_worker_connected:false` 是 worker 未启动时的预期值，不要为把它变 true 而启动 worker 消费旧 Run。

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

本次核对时间：`2026-09-06 21:45 CST`。

```text
anchor-api-dev.service                   active / loaded
anchor-receiver-dev.service              active / loaded
anchor-web-dev.service                   active / loaded
anchor-supervisor.service                active / loaded
anchor-worker.service                    inactive / user unit not found
anchor-control-worker.service            inactive / loaded
anchor-verifier-worker.service           inactive / disabled（收尾会话已安装 unit 文件，保持禁用停止）
anchor-scheduler.service                 inactive / user unit not found
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

当前 `/home/mansteinl/Anchor/.local/runtime.json` profile：

```text
provider: rightcode
model: gpt-5.6-luna
base_url: https://api.a6api.com/v1
wire_api: responses
secret_file: /home/mansteinl/.codex/auth.json
secret_ref: OPENAI_API_KEY
agent_ref: agents.researcher
verifier_ref: verifiers.evidence
verifier version: v1
verifier adapter: model
verifier model_ref: models.codex.local
```

绝不打印、复制、转存或提交 `/home/mansteinl/.codex/auth.json` 的内容。不要在诊断命令中输出环境变量或请求 Authorization header。Secret 只能由运行时 `SecretProvider` 读取。

`gpt-5.6-sol` 此前不稳定；真实 E2E 继续使用已经验证可用的 `gpt-5.6-luna`。模型错误不得被测试桩或普通文本改写为成功。

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
src/anchor/state/relational.py
src/anchor/state/schema.py
migrations/versions/0007_webhook_secret.py
migrations/versions/0008_context_snapshots.py
migrations/versions/0009_edge_decisions.py
migrations/versions/0010_verification_records.py
```

Runtime：

```text
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
src/anchor/runtime/propagation.py
src/anchor/runtime/resolution.py
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

## Verifier 收尾执行顺序

严格按以下顺序推进并记录真实输出：

1. 修复并通过最新 relational SQLite contract。
2. 检查 `tests/test_relational_store.py` 中按 `run_id` 选择 rejection dispatch 的修改没有误改其他测试。
3. 运行 Verifier、API、admission、capability、config、supervisor、SQLite state 相关 Python 测试。
4. 启动一次性 PostgreSQL 17 容器，不复用任何未知数据库。
5. 在迁移到 `0010` 的一次性 PostgreSQL 上运行 `tests/test_relational_store.py` 和 `tests/test_api.py`，确认 PostgreSQL 参数组实际执行而非全部 skipped。
6. 删除一次性 PostgreSQL 容器并确认测试端口释放。
7. 运行完整非 PostgreSQL Python suite。
8. 运行 `pip check`、`compileall` 和 `git diff --check`。
9. 运行前端 unit、build 和 Playwright。
10. 使用新建 `/tmp` 隔离库运行真实独立 `worker_service` + `verifier_service` 进程 E2E。
11. 核验 artifact/context/verification/event/downstream/terminal state 全部来自 canonical 持久化结果，并终止所有隔离进程。
12. 更新 `DECISIONS.md`、`STATUS.md`、`API.md`、`WEB.md`、`README.md` 和本文。
13. 迁移 `.local/api.sqlite` 从 `0009` 到 `0010`。
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

- Approval、HumanTask 和 WaitForEvent 的 durable wait/resume。
- 完整 ToolGateway、真实工具执行、schema validation、permission/approval 和 MCP。
- pause/resume/cancel。
- Loop executor、progress signal、deadlock/repeated-loop/non-progress detection。
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
join 和 context snapshots 已完成，不要重新实现。当前 P0 是收尾独立 Verifier：
主体代码、0010 migration、typed claim、deterministic/model adapter、evidence record、
completion gate、API 和 Run Console 已写入，但最新 relational rejection contract
修复后的重跑被暂停。

第一条测试运行：
.venv/bin/pytest -q tests/test_relational_store.py -m 'not postgres' -x

通过后继续 PostgreSQL contract、全量 Python、前端 unit/build/Playwright 和全新
/tmp 隔离库中的真实 verifier_service 进程 E2E。只有这些证据通过后，才迁移
.local/api.sqlite 从 0009 到 0010、重启 API、安装但不启用 Verifier user unit，
并更新 DECISIONS/STATUS/API/WEB/README/HANDOVER。

保持核心语义：正常 Agent 不依赖 max_* 预算；Tool outcome_unknown 不盲目重试；
外部副作用先写 ledger；lease 只能人工确认恢复；Verifier 只有结构化、持久化且
绑定 artifact/context hash 的 passed verdict 才能完成并传播。遇到改变状态机、
持久化协议或向后兼容契约的关键架构决策时再停下来询问用户。
```

## 完成审计（两级门，ADR-017 时代修订）

MVP complete 只要求可交付的单机产品闭环；生产门按真实部署触发，不 blocking 交付。

### MVP 门（全部具备即可 complete）

- 全部目标节点类型具有真实、持久化、可恢复的执行或等待语义。
- 手动、定时、内部事件和 webhook 均能固定不可变 Graph Version 并完成真实闭环。
- 干净隔离库上的模型、工具、验证、等待/恢复和失败路径 E2E 成功。
- worker 长期运行、heartbeat、进程中断、rolling restart 和显式恢复可证明。
- 健康 Agent 不被用户难以预测的 `max_*` 预算干预。
- Tool unknown outcome 绝不自动重试，并具有可用 reconciliation 流程。
- SQLite/PostgreSQL 协议、迁移、并发领取、事务回滚和重开持久化验收一致。
- context、memory、artifact、operation、verification、edge decision 和事件证据可追溯。
- Web Builder/Run Console 的状态全部来自持久化 API，支持用户完成编排、发布、触发、观察和人工处置。
- worker 长期运行指单机常驻进程的 heartbeat、中断与显式恢复（多主机 rolling restart 移入生产门）。

### 生产门（按真实部署触发，不 blocking MVP complete）

- retention/GC 与 backup/restore 策略。
- production auth、权限边界、tenant isolation、OpenTelemetry 和部署运维。
- 多主机、rolling restart、长时间 soak、多租户验收。
- Prefect/Temporal 等 durable 执行 muscle（现有语义被证明不足时）。
- MCP/A2A、向量检索、对象存储（出现真实消费者时）。

在此之前，Goal 应保持 active/paused，而不是 complete。MVP 门证据（以本轮为准）：230 后端绿 + PG 162 绿 + 前端 19/e2e 8/8；隔离库真实 E2E（verifier 双路径、loop 迭代退出、tool 网关bwrap）；迁移 head 0011；初始提交 `ef94329`。
