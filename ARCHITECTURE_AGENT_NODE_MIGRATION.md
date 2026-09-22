# Anchor 新 Agent Node 架构与一次性迁移设计

状态：提案，作为外部 Agent 的实施总说明。目标是一次完成架构切换，不保留 mini-swe-agent 作为运行时后备路径。

## 1. 结论先行

Anchor 的运行时分成两层：

- **Graph Runtime**：只负责 Graph IR、依赖、路由、NodeRun 状态、提交记录、恢复调度和 op 执行。
- **Agent Node Runtime**：只负责一次 Agent Node 的模型循环、上下文、bash 工具、输出记录、步骤持久化和恢复判断。

两层通过稳定的 `NodeRequest / NodeOutcome / RecoveryRef` API 交互。Graph 不读取 Harness 消息、摘要、工具调用或检查点；Node 不读取 Graph 调度器内部状态。

PydanticAI Harness 是 Agent Node 的内部实现基础。Anchor 自己保留产品行为：bash 单工具、完成协议、上下文记录、恢复安全判定、Graph 提交接缝和操作证据。Harness 负责模型循环、工具调用生命周期、上下文 capability 和步骤持久化。

`agent` 与 `op` 是两种 Node：

- **Agent Node**：PydanticAI Harness + 一个 bash 工具 + Anchor 完成协议。
- **Op Node**：真实 sandbox 中执行一个命令；退出码和显式 route 是结果；不创建模型、Harness、上下文或恢复快照。

二者共享 NodeRun、workspace、输入挂载、Git 提交、输出记录、事件和 Graph 边语义。

## 2. 不变量

这些不变量优先于具体类名和目录结构：

1. 每个 NodeRun 有稳定的 `graph_run_id`、`node_id`、`attempt_id` 和 `operation_id`。
2. 控制目录、步骤 store、预算、追加记录和输出存储永远位于 node workspace 之外，并以只读方式挂载给 Agent。
3. Agent 能看到的工具只有 `bash`；输出引用通过 bash 读取，不注册第二个 Harness 工具。
4. 任务、instructions、工具 schema、模型响应、工具参数、tool_call_id、工具结果、摘要和预算事件追加保存，压缩只改变后续模型看到的 projection。
5. 完成是事实，不是模型文字：只有 `read_completion()` 接受的 `anchor-done` 或合法 `anchor-route` 才能产生 `CompletionFact`。
6. 未知副作用永不自动重放。`uncertain` 是正常终态，Graph 必须把它暴露给上层处理。
7. 已有完成事实的 NodeRun 恢复时零模型请求、零工具执行、零重复提交。
8. 请求预算是逻辑执行总预算，主模型、摘要、窗口重试共用一个持久化计数器；请求发出前先收费一次，耗尽后零请求。
9. Op 不依赖 PydanticAI、Harness 或 Agent 内部类型。
10. Graph 只能消费 NodeOutcome 和不透明 RecoveryRef，不能根据文件猜测 Node 是否完成。

## 3. 目录与模块

建议最终布局：

```text
src/anchor/
  graph/
    ir.py                 # Graph/Node/Edge/AgentDef/OpDef 的产品 schema
    validate.py           # reads/writes、路由、模块展开、版本 hash
    scheduler.py          # Ready/Running/Waiting/Completed/Uncertain/Skipped
    recovery.py           # Graph 级恢复与 NodeOutcome 消费
  node/
    contract.py           # NodeRequest/NodeOutcome/RecoveryRef/NodeStatus
    agent_runtime.py      # PydanticAI Agent + Harness capabilities
    op_runtime.py         # 单命令执行
    completion.py         # 唯一完成协议解析
    context.py            # budget、record、compaction、output reference
    persistence.py        # StepPersistence/FileStepStore 接线
    recovery.py           # Node 级 assess/continue/finished/invalid
    adapter.py             # Graph-facing run_node()
  runtime/
    sandbox.py            # bubblewrap、双流采集、挂载、清理
    execenv.py            # NodeSandbox 与输入/输出接线
```

不要建立通用 plugin bus、第二套 scheduler、第二套 completion parser 或多个 Store 抽象。现有模块可重命名，但每个职责只保留一个事实来源。

## 4. Graph IR

保持现有 JSON 兼容形状，逐步将定义归一为内部 `NodeDefinition`：

```json
{
  "agents": {
    "writer": {
      "model": "models.academic",
      "instructions": "...",
      "network": false,
      "reads": ["plan.md"],
      "writes": ["draft.md"]
    }
  },
  "ops": {
    "check": {
      "run": "grep -q '^## References' /in/write/draft.md",
      "reads": ["draft.md"],
      "writes": ["check.txt"]
    }
  },
  "nodes": [
    {"id": "write", "agent": "writer"},
    {"id": "check", "op": "check"}
  ],
  "edges": [{"from": "write", "to": "check"}]
}
```

内部定义统一为：

```python
@dataclass(frozen=True)
class NodeDefinition:
    kind: Literal["agent", "op"]
    reads: tuple[str, ...]
    writes: tuple[str, ...]
    agent: AgentConfig | None = None
    op: OpConfig | None = None
```

保留外部 `agents`/`ops` 两张表，避免无必要破坏现有文件；内部统一处理。未来若增加 `wait`、`approval`、`verifier`，新增 `kind` 和专用 runtime，不把它们伪装成 bash Agent。

Graph 校验必须拒绝：未知定义、重复 node id、无效读写声明、不可达节点、非法 route、循环模块展开、缺失输入生产者、op 使用 Agent 专属字段。加载时计算不可变 Graph Version hash。

## 5. Node API

Graph 只依赖以下 API：

```python
@dataclass(frozen=True)
class NodeRequest:
    graph_run_id: str
    node_id: str
    attempt_id: str
    task: str
    workspace: Path
    inputs: tuple[InputMount, ...]
    model: str | None = None
    instructions: str = ""
    network: bool = False
    routes: tuple[str, ...] = ()
    max_requests: int | None = None
    recovery: str | None = None
    control_dir: Path | None = None

@dataclass(frozen=True)
class NodeOutcome:
    status: Literal["completed", "routed", "skipped", "uncertain", "failed"]
    submission: str = ""
    route: str | None = None
    reason: str = ""
    model_requests: int = 0
    files: tuple[str, ...] = ()
    recovery: str | None = None
    evidence_ref: str | None = None

async def run_agent_node(request: NodeRequest, *, model, capabilities=()) -> NodeOutcome
async def run_op_node(request: NodeRequest, *, command: tuple[str, ...]) -> NodeOutcome
```

`run_agent_node` 是唯一 Harness 入口。`run_op_node` 永远不接收 model/capabilities。两者都返回 NodeOutcome；异常必须映射为 `failed` 或 `uncertain`，不能让 Graph 通过异常类型猜状态。

## 6. Agent Node 内部执行

一次 Agent Node 的初始化顺序固定：

1. 校验 request、workspace、control_dir、输入挂载和预算。
2. 读取 recovery ref；若 `invalid`/`uncertain`/`finished`，立即返回且零模型请求。
3. 创建控制目录外的 `Record`、`FileStepStore` 和 `NodeSandbox`。
4. 构造 capabilities，顺序固定为：收费器 → 原始记录 → 摘要压缩 → StepPersistence → 测试屏障。生产代码不依赖屏障。
5. 创建只有 `bash` 的 PydanticAI Agent。
6. 通过 `agent.iter` 执行。模型只可调用 bash。
7. 每次请求由统一 `charge_request()` 先写入预算，再交给主模型或摘要模型。
8. bash 执行经过 NodeSandbox；输出预览逐流有界，完整输出按共享存储额度落盘，引用路径在 sandbox 内稳定且只读。
9. `read_completion()` 只接受成功退出、首个非空行的合法 done/route；接受后同步写 `CompletionFact`。
10. 正常工具终态和快照由 StepPersistence 记录；Node 通过事实文件判断 finished。
11. 返回 NodeOutcome，并生成新的 RecoveryRef。

模型文字不能完成 Node。模型没有工具调用时触发 Harness 重试；预算耗尽返回 `failed`/`uncertain`，不得伪造 completed。

## 7. Context 与记录

Context 不是 canonical history。它是模型请求的有限 projection。

```text
canonical record (append-only, immutable)
  ├── initial_context
  ├── request_sent / request_refused
  ├── model_response
  ├── tool_call / tool_result
  ├── compaction_started / compaction_finished
  ├── summary_request / summary_result / summary_failed
  ├── completion_fact
  ├── output_kept / output_refused
  └── recovery / uncertain

model context projection
  └── 当前请求实际发送的有限 messages
```

摘要策略顺序固定为：摘要优先，滑窗兜底。摘要每次调用参与同一 request budget；摘要失败要追加记录并进入滑窗或明确失败，不能静默丢旧上下文。每个请求记录 `policy_version`、`messages_hash`、估算 token、实际 request 序号和来源消息范围。

## 8. 恢复状态机

Node 级判定：

- `replayable`：模型请求已完成，但没有任何工具调用开始。
- `uncertain`：工具 started 无终态、工具 failed、快照未覆盖已完成 effect、预算/记录损坏或无法确认副作用。
- `continuable`：所有 effect 有终态，complete snapshot 覆盖所有 tool_call_id，可恢复历史继续。
- `finished`：存在结构化 CompletionFact，恢复只读事实，零模型/工具调用。
- `invalid`：引用、node/run/store/version/预算不匹配或记录损坏。

Graph 级 NodeRun 状态：`pending → ready → running → completed/routed/skipped/uncertain/failed`。`uncertain` 不自动重试；由 API/人工/策略产生新 attempt。旧 attempt 永不覆盖。

恢复引用必须绑定：graph_run_id、node_id、attempt_id、control_dir、graph_version、budget。只解码 token 不等于合法恢复；必须检查 store 中的实际 run 和事实。

## 9. Graph 调度与恢复

Graph Scheduler 在运行节点前执行：

1. 读取 pinned Graph Version 和 NodeRun 状态。
2. 若节点已 `completed/routed/skipped`，记录 skip，不再调用 Node。
3. 若节点有 RecoveryRef，调用 Node recovery；`finished` 直接将结果写入 Graph canonical state；`continuable` 重新执行同一 Node API；`uncertain` 暂停该 NodeRun。
4. NodeOutcome 写入 Graph event，再决定 outgoing edges；边决策使用 immutable output evidence。
5. `op` 节点成功由 exit code/route 决定；失败进入 failed，不由 Agent 恢复逻辑接管。
6. 所有 incoming edges 决定且至少一条 selected 后，节点 ready；无 selected 的节点为 skipped。

`already_submitted` 接缝必须成为正式 scheduler API，而不是测试参数：scheduler 在启动 Node 前读取 Node completion fact，并将 NodeOutcome 作为已完成结果写入 Graph。Graph resume 必须同时读取 Graph pass record 和 Node control record，二者冲突时 fail closed。

## 10. Op Node

Op 定义：

```python
@dataclass(frozen=True)
class OpConfig:
    run: str
    reads: tuple[str, ...]
    writes: tuple[str, ...]
    routes: tuple[str, ...] = ()
    timeout_seconds: float = 600
    network: bool = False
```

执行流程：准备只读 inputs → 创建 NodeSandbox → 执行一次命令 → 收集 stdout/stderr → 解析退出码和 route → 写 NodeRun record → Git commit。Op 不创建模型上下文、StepPersistence、摘要或 recovery ref；如果 op 的命令发生未知外部副作用，状态为 `uncertain`，禁止自动重跑。

Op 的 bash 命令仍受 bubblewrap 边界约束。命令输出和退出码进入 canonical evidence；stdout 不是成功协议，成功由退出码和合法 route 决定。

## 11. 事件与持久化

控制目录建议：

```text
control/<graph_run_id>/<node_id>/<attempt_id>/
  record.jsonl
  budget.json
  completion.json
  steps/
  outputs/
  recovery.json
  trace.jsonl
```

所有文件写入采用临时文件 + rename；追加记录使用单写者顺序。跨文件不宣称事务。每条事件至少带：`graph_run_id`、`node_id`、`attempt_id`、`event_id`、时间、来源、payload hash。

Graph canonical event 与 Node record 是两层事实：Node record 证明 Node 内部发生了什么，Graph event 证明调度器接受了什么。恢复时必须同时读取，不能用一个替代另一个。

## 12. 一次性迁移步骤

### M0：冻结契约

- 将本文写入 ADR，冻结 Node API、状态、事件名、预算口径、CompletionFact 和 Graph resume 接缝。
- 更新 README/OPEN/DECISIONS，删除“mini 是运行时实现”的描述。
- 固定 `pydantic-ai-slim`、`pydantic-ai-harness` 版本和兼容矩阵。

### M1：实现新 Node Runtime

- 把当前 `pydantic_adapter.py` 收敛为 `agent_runtime.py`/`adapter.py`。
- 抽出唯一 completion parser、charge_request、CompletionFact、Node recovery。
- 保留现有确定性测试，删除重复 parser 和只服务 mini 的分支。

### M2：实现 Op Runtime

- 从现有 graph op 执行抽出 `run_op_node`。
- 确保 op 不导入 pydantic_ai/harness。
- 为 op 添加超时、输出、route、unknown side effect 证据。

### M3：替换 Graph Scheduler

- Graph 所有 agent 节点统一调用 `run_agent_node`。
- Graph 所有 op 节点统一调用 `run_op_node`。
- `resume` 同时检查 Graph pass 与 Node CompletionFact；已完成节点零执行。
- 删除 mini agent 默认路径和分叉完成协议。

### M4：迁移数据和兼容边界

这是一次性代码迁移，不要求历史运行自动转换。旧 runs 只读展示；不能把旧 trace 假装成新 RecoveryRef。新运行全部使用新控制目录和新事件格式。

### M5：端到端验收

必须通过：

- Graph schema/reads/writes/route 校验
- Agent 单节点、Op 单节点、Agent→op→Agent
- 上下文压缩后恢复
- B1–B8 故障矩阵
- 预算跨主模型/摘要/重试恢复
- 完成事实拒绝反例
- 大输出恢复读回和只读挂载
- Graph resume 跳过已完成 Node
- mini 路径删除后的旧测试替换为新 Node API 测试

## 13. 外部 Agent 分工

不要让多个 Agent 同时修改共享适配器和 scheduler。按以下包分工，主集成按顺序合并：

1. **Contract/ADR Agent**：只改 ADR、README、公共 contract、事件 schema；先合并。
2. **Agent Runtime Agent**：只改 `src/anchor/node/*`、上下文/恢复测试；依赖冻结 contract。
3. **Op Runtime Agent**：只改 op runtime、sandbox 接线和 op 测试；不得导入 Harness。
4. **Graph Integration Agent**：只改 scheduler、resume、NodeOutcome 接缝和图闭环测试；依赖 1–3。
5. **Acceptance Agent**：不改实现，独立运行全量测试、B1–B8、静态检查和真实 graph smoke test。

每个 Agent 交付：一个或多个 coherent commit、变更文件、测试命令、已知限制、未解决失败。不得自行合并或宣称整体验收通过。

## 14. 完成标准

迁移完成必须同时满足：

- 生产 Graph 不再调用 mini-swe-agent。
- Agent 与 Op 通过统一 Node API 执行，Graph 不依赖 Harness 类型。
- B1–B8 全通过；预算账目主模型+摘要+重试严格一致。
- 恢复不会重复已确认副作用，失败或未知状态显式 `uncertain`。
- 图 resume 能读取 Node CompletionFact，已完成节点零请求零命令。
- 全量测试、静态检查、真实 bubblewrap graph smoke test 通过。
- README、OPEN、DECISIONS、ADR、迁移说明与实际代码一致。

仍需明确写入交付文档：不提供 exactly-once、断电耐久性、真实 provider 窗口错误覆盖和任意外部副作用自动对账。
