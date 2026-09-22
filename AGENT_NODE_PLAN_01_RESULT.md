# 第一包实施结果：PydanticAI Node 最小适配与控制流验证

状态：实施完成，可审阅。**本包结论不等于迁移决策**——按计划第 8 节，只有主验收 Agent 复核并通过
完成语义、真实沙箱、图集成三项后，此入口才冻结给第二、三包并行使用。

**结论一句话：PydanticAI 能保住 Anchor 的五条执行语义，且只用公开 API；代价是普通命令的观察以
「重试提示」的形式到达模型，以及一个被实测否定的写法（`ToolFailed`）。**

---

## 1. 基线、分支与改动

| | |
| --- | --- |
| 基线 SHA | `52c94bfbef956ebf9724b30d1e7c9f646866d709`（`52c94bf`，工作区干净） |
| 实施分支 | `research/node-controlflow`（未合并、未推送主分支） |
| 本包 commit | `943acab` 适配器与共享接线；`1ccfe0d` 验收矩阵 |
| 改动文件 | `src/anchor/node/__init__.py`（新）、`src/anchor/node/pydantic_adapter.py`（新）、`src/anchor/runtime/execenv.py`（新）、`src/anchor/simple/agent.py`（改，净 -9 行）、`tests/test_node_controlflow.py`（新）、`tests/test_sandbox_live.py`（改导入）、`requirements/node-verification.txt`（新） |
| 依赖 | `pydantic-ai-slim==2.46.0`、`pydantic-ai-harness==0.32.0`（后者本包未使用，仅为锁定与后续包准备）。**未进 `pyproject.toml`**；`requirements/node-verification.txt` 是精确版本清单 |
| 研究原件 | `AGENT_HARNESS_RESEARCH.md`、`AGENT_HARNESS_RESEARCH_REPORT.md`、`AGENT_NODE_PLAN_01.md` 保留未跟踪，未改动 |

**一处必须先说的前提核实。** 计划与报告写的是「PydanticAI 2.46.0」。执行前核实：PyPI 上
`pydantic-ai 2.46.0`（2026-09-19）与 `pydantic-ai-harness 0.32.0` 均存在；harness 要求
`pydantic-ai-slim>=2.44.0`，而环境里装的是 `2.43.0`（由 `pydantic-evals` 拉入，而 **Anchor 从未声明或
使用 `pydantic-evals`**）。升级后 `pydantic-evals 2.43.0` 与 `slim 2.46.0` 冲突，但该包无任何依赖方，
实测无影响：完整后端测试在升级前后均通过。

安装的是 **`pydantic-ai-slim`** 而非元包 `pydantic-ai`——元包会拉入 anthropic/google/logfire/mcp/web
等 extras，本包一个都不需要。

---

## 2. 对外契约

`src/anchor/node/__init__.py`，普通 dataclass，**不含任何 PydanticAI / mini / LiteLLM 类型**。

### `NodeRequest`

| 字段 | 语义 |
| --- | --- |
| `execution_id` | 区分一次执行。**明确不是 resume token**——注释里写死，避免被当成一个 |
| `task` | 目标，节点被告知的任务 |
| `instructions` | 这一处对角色要求的追加；**公共规则由 runner 提供，调用方给不了** |
| `workspace` | 调用方已准备的节点工作区 |
| `inputs` | `(宿主路径, 沙箱内可见路径)` 对，只读。**已解析**——Node 不遍历图 |
| `routes` | 允许选择的目标 ID。Node 校验，**Graph 保留最终调度权** |
| `network` / `timeout_seconds` / `max_requests` | 明确限制。**请求计数仅限本次执行**，不宣称跨恢复累计 |
| `trace` | 记录落点。**在工作区之外** |

### `NodeOutcome`

`status`（`completed` / `budget_exhausted` / `failed`）、`submission`、`route`、`model_requests`、
`trace_ref`、`reason`、`files`。

`__post_init__` 强制两条不变式，越界即 `ValueError`：

- **非 `completed` 不得带 `route`**——一个没做完的节点不能是可调度的，否则图会基于没有发生的
  工作前进。
- **非 `completed` 必须给 `reason`**——不能有说不清为什么停下的状态。

### 调用示例

```python
from anchor.node import NodeRequest
from anchor.node.pydantic_adapter import run_node

outcome = await run_node(
    NodeRequest(execution_id="exec-1", task="…", workspace=Path("/runs/r1/draft"),
                instructions="你是这篇综述的作者。", inputs=(("/runs/r1/gather", "/in/gather"),),
                routes=("review", "done"), max_requests=60,
                trace=Path("/runs/r1/draft.trace.jsonl")),
    model=my_model,          # 调用方决定用哪个 provider，或用一个确定性替身
)
```

模型是**参数**而不是在内部构造——哪个 provider、甚至有没有 provider，是调用方的事；自己构造模型的
runner 是一个无法在没有 provider 时测试的 runner。

### 公开 / 私有框架 API

**适配器只使用公开 API**：`Agent`、`RunContext`、`ToolOutput`、`UsageLimits`、
`ModelMessagesTypeAdapter`、`ModelRetry`、`UsageLimitExceeded`。

探索阶段读过私有属性（`agent._output_schema`），但**最终实现和测试都不依赖它们**：A1 的
「模型只看到一个工具」断言在 `FunctionModel` 的 `info` 参数上——**那正是模型实际收到的请求**，
比内部属性更强，也不会随实现变动而失效。

唯一使用私有名字的地方是 `tests/test_node_controlflow.py` 里的 `runner._agent_for`，即计划第 3 节
允许的「现有执行工厂接缝」。

---

## 3. 环境与可复现命令

```
Python 3.12.3 · Linux 7.0.0-31-generic x86_64 · bubblewrap 0.9.0
```

```bash
# 隔离安装精确候选版本（不进默认依赖）
.venv/bin/pip install 'pydantic-ai-slim==2.46.0' 'pydantic-ai-harness==0.32.0'

# 本包验收
.venv/bin/python -m pytest tests/test_node_controlflow.py -q

# 默认路径回归（A13）
.venv/bin/python -m pytest tests/ -q

# 静态检查（覆盖新增模块）
.venv/bin/ruff check src/ tests/
.venv/bin/mypy src/anchor/
```

**实测结果**

| | 通过 | 失败 | skip |
| --- | ---: | ---: | ---: |
| `tests/test_node_controlflow.py` | **32** | 0 | 0 |
| `tests/` 全量 | **134** | 0 | 0 |

（全量 134 = 本包新增 32 + 既有 102。升级 `pydantic-ai-slim` 前后既有 102 个均通过。）

`ruff` 与 `mypy src/anchor/`（23 个源文件）均干净。

**没有 skip**：本机的 bubblewrap 可用，因此 A1–A13 全部真实执行。仅在「本机无外网出口时无法区分
network 开关」这一种情况下 A9 会 skip；本机有出口，故未 skip。

---

## 4. 验收矩阵结果

| ID | 测试 | 结果 | 关键证据 |
| --- | --- | --- | --- |
| A1 | `test_a1_one_bash_tool_writes_a_file_and_submits` | ✅ | 产物由真实 bwrap 写出（`out.txt` = `from the sandbox\n`）；`submission` = `wrote out.txt` 来自 CLI；`model_requests == 2` |
| A1 | `test_a1_the_model_is_offered_exactly_one_tool` | ✅ | 模型实际收到 `function_tools == []`、`output_tools == ['bash']` |
| A2 | `test_a2_plain_text_does_not_submit_and_the_node_can_still_finish` | ✅ | 先说「我完成了」不结束；随后提交成功 |
| A2 | `test_a2_saying_it_is_done_repeatedly_has_a_finite_end` | ✅ | 50 次纯文本 → 非 `completed`、无 `route`、有 `reason` |
| A3 | `test_a3_the_model_is_never_asked_again_after_a_submission` | ✅ | **模型在提交后被调用即主动抛错**；运行成功即证明未被再调用；`model_requests == 2` |
| A4 | `test_a4_commands_after_a_completion_do_not_run` | ✅ | 同响应三连 `before`/`done`/`after`：**`before.txt` 在、`after.txt` 不在**；`model_requests == 1` |
| A4 | `test_a4_the_order_commands_ran_in_is_the_order_they_were_emitted` | ✅ | 各命令追加自身名字，文件内容 `a\nb\nc\n` |
| A5 | `test_a5_with_several_ways_out_done_is_refused_and_route_finishes` | ✅ | 真实 `anchor-route`；`route == "right"`；`submission` = reason |
| A5 | `test_a5_with_one_way_out_done_finishes_and_names_no_route` | ✅ | 单出口 `anchor-done` 完成且 `route is None` |
| A6 | `test_a6_an_unknown_target_is_refused_and_can_be_corrected` | ✅ | 真实 `anchor-route --to nowhere` 被拒；下一轮合法目标成功 |
| A7 | `test_a7_a_marker_that_is_not_the_first_line_is_not_a_completion` | ✅ | 输出中间的标记不提交 |
| A7 | `test_a7_a_marker_on_a_command_that_failed_is_refused` | ✅ | 非零退出携带 `done` 标记 → 拒绝 |
| A7 | `test_a7_a_route_on_a_command_that_failed_is_refused` | ✅ | 非零退出携带 route 标记 → 拒绝（**mini 路径在此接受**，见 §5） |
| A7 | `test_a7_read_completion_is_exact` | ✅ | 7 组边界：精确首行 / 超时 / 127 / 中间出现 / 前缀 / 空 |
| A7 | `test_a7_done_is_refused_where_the_node_has_to_choose` | ✅ | 多出口时 `done` 被拒且错误里列出可选目标 |
| A8 | `test_a8_a_failing_command_is_visible_and_is_not_run_again` | ✅ | **计数文件 = `run\n`（一次）**，证明框架没有重跑副作用；trace 里有退出码与 stderr |
| A8 | `test_a8_a_timed_out_command_is_refused_as_a_completion` | ✅ | 超时命令携带的标记被拒；后续提交成功 |
| A9 | `test_a9_its_own_workspace_is_writable_and_what_it_was_given_is_not` | ✅ | 自己可写；`/in/upstream/theirs.md` 写入失败且**字节不变** |
| A9 | `test_a9_the_history_is_readable_and_not_rewritable` | ✅ | 能 `git log` 读到 HEAD；写 `.git/config` 被拒 |
| A9 | `test_a9_network_is_off_and_that_is_the_isolation_the_sandbox_provides` | ✅ | 用沙箱自身探针比对 network 开关，不依赖公网连通性 |
| A10 | `test_a10_running_out_of_requests_is_its_own_status_and_never_a_route` | ✅ | `budget_exhausted`、无 route、无 submission、产物恰为预算内 3 步 |
| A10 | `test_a10_the_budget_does_not_buy_one_more_request` | ✅ | 预算 4 且模型记录被越界调用次数 = **0** |
| A11 | `test_a11_a_second_execution_does_not_see_the_first_one_s_completion` | ✅ | 第一次 route=`right`；第二次 routes 变成 `("only",)`，结果为 `(completed, None)`；工作区按设计保留 |
| A12 | `test_a12_a_real_graph_runs_agent_op_agent_through_the_new_entry_point` | ✅ | 见下 |
| A13 | `test_a13_the_default_path_runs_with_pydantic_ai_unimportable` | ✅ | 子进程用 meta_path 屏蔽 `pydantic_ai`，仍能 `runner.run` 跑完 op 图 |
| A13 | `test_a13_nothing_on_the_default_path_imports_the_adapter` | ✅ | 默认路径五个模块均不出现 `pydantic_ai` 或 `anchor.node` |

### A12 的关键证据

图 `first(agent) → middle(op) → last(agent)`，通过 `runner` 的真实调度器运行：

- **输入 commit 与只读挂载是真的**：`last` 从 `/in/middle/size.txt` 读到内容并写入
  `upstream-size.txt`，值 `.strip() == "10"`（与 `wc -c` 对 `the draft\n` 的结果一致）。
- **op 不调用模型**：桥接层只接管 agent 路径，op 由**真实的 `_agent_for`** 处理；工厂看到的调用
  列表里没有 `middle`。op 的 `submission` 是它真实的 stdout（`counted`）。
- **Git freeze 是运行时的**：三个节点各有 commit，`git log` 里都有 `start`。
- **没有 stub 掉任何东西**：Git freeze、输入视图、沙箱、op 全部是运行时的实现。

顺带：这张图的第一版被**加载期接口检查**拒绝（`node 'middle' reads draft.md, and nothing it can be
handed writes it`），因为角色没声明 `writes`。这是检查在正常工作，不是障碍——记在这里是因为它说明了
两条路径共用同一套图语义。

---

## 5. 与默认 mini 路径的行为差异

### 5.1 退出码检查：新适配器更严，mini 有一个真实缺口

实测（`.local/probe_mini.py`，直接调用 `SandboxEnvironment._check_finished`）：

| 情形 | mini 路径 | 新适配器 |
| --- | --- | --- |
| `ANCHOR_ROUTE: left` + 退出码 0 | 接受为完成 | 接受 |
| **`ANCHOR_ROUTE: left` + 退出码 7** | **接受为完成** ✗ | **拒绝** ✅ |
| `COMPLETE_TASK_AND_SUBMIT_FINAL_OUTPUT` + 退出码 7 | 拒绝 | 拒绝 |

也就是说：**mini 的多出口分支未统一检查退出码**——一个先打印 route 标记、随后失败的命令，在 mini
路径上仍然会让节点路由出去。计划第 5 节要求在行为差异中明确记录，不得顺手修改旧路径；本节即为该记录。
新适配器按协议拒绝。

### 5.2 终止机制：`end_strategy='early'` 是承重的那一个

PydanticAI v2 的默认是 `'graceful'`，在它之下**输出成功之后同一响应里剩余的调用仍会执行**。A4 的
行为只靠 `'early'` 成立，这也是「提交命令是响应里最后一件事」从希望变成事实的原因。

### 5.3 普通观察的形态：这是本包最需要被审阅的代价

输出工具**返回即结束**，因此非完成的命令不能返回。框架从输出工具留下的唯一出口是 `ModelRetry`，
于是：

> **一条普通的 `ls` 会以「重试提示」的形式到达模型**，带着框架为此附加的纠正性措辞，而不是一条普通
> 的工具结果。

**`ToolFailed` 读起来完全正确、但在这里不能用。** 它的文档说它产生「模型看得见的失败结果，且不像
`ModelRetry` 那样附加重试/纠正指令」——正是普通命令输出应有的样子。实测：

| 实验 | 结果 |
| --- | --- |
| 输出工具 + 正常返回 | ✅ 完成，且同响应后续调用未执行 |
| **输出工具 + `ToolFailed`** | ❌ **异常逃逸，整个 run 失败** |
| 函数工具 + `ToolFailed` | ❌ 被计为重试，耗尽后 `UnexpectedModelBehavior` |
| 输出工具 + `ModelRetry`（默认预算 1） | ❌ `Exceeded maximum output retries (1)` |
| 输出工具 + `ModelRetry`（预算 10） | ✅ 循环继续，`: DONE` 上结束 |
| 函数工具 + `CallDeferred` | ❌ 需要输出类型包含 `DeferredToolRequests`（未继续验证） |

因此 **重试预算就是请求预算**：`max_requests` 同时传给 `UsageLimits(request_limit=…)` 与
`ToolOutput(max_retries=…)`，而不是留框架默认的 1（默认值会让节点在第二条命令上停住）。

**这条是否可接受，需要主验收 Agent 判断。** 它不影响完成语义、路由、权限与预算，影响的是模型看到的
观察的语气。

### 5.4 记录

| | mini 路径 | 新路径 |
| --- | --- | --- |
| 记录位置 | 节点目录旁 `<node>.trace.jsonl` | 由 `NodeRequest.trace` 指定，**同样在工作区之外** |
| 内容 | 逐条消息，边发生边写并 flush | 结束时整体写出，经 `ModelMessagesTypeAdapter`，末行带 `exit` 与命令序列 |
| 流式可观察性 | ✅ 边跑边写（现有前端轮询依赖它） | ❌ **本包不流式**——这是与现有观察能力的真实差异，第二包若要接事件流需补 |

### 5.5 其他

- **恢复**：新路径**不实现** `resume`，被调用时明确抛错而不是假装支持。mini 路径的 trace 恢复不变。
- **未统一抽取的部分**：完成协议在两处各写了一遍。计划第 5 节明确要求不要改动旧路径，因此这是有意
  的重复，不是遗漏。

---

## 6. 共享抽取：`runtime/execenv.py`

`_console_script`、`_tool_binds` 与工作探针原本在 `simple/agent.py`，而该模块**在模块级 import
mini-swe-agent**。第二个 runner 要复用同一套沙箱接线，就必然继承那个 import——而第二个 runner 存在的
意义正是不继承它。

抽出为 `anchor/runtime/execenv.py`，提供 `NodeSandbox`：一处决定节点的挂载、工具目录、环境变量与
「沙箱能用」的证明，两条路径共用。`simple/agent.py` 改为使用它，**行为不变——既有 102 个测试是回归
证据**。

顺带一处必要的放宽：`workspace_readonly=(".git",)` 只绑定**真实存在**的路径。绑定一个不存在的路径会让
整个沙箱起不来，而调用方可能交来一个还没建 git 的工作区（真实流程里 `run.py` 会先 `_init_history`，
但 runner 不该因此拒绝一个干净目录）。

---

## 7. 后续包应复用的入口与夹具

**入口**

- `anchor.node.NodeRequest` / `NodeOutcome`——请求与结果。第二、三包扩展契约时由主验收 Agent 统一收敛。
- `anchor.node.pydantic_adapter.run_node(request, *, model)`——异步入口。
- `anchor.node.pydantic_adapter.build_agent(model, *, instructions, max_retries)`——只造 Agent，便于
  第二包在 `agent.iter()` 上挂事件流。
- `anchor.node.pydantic_adapter.read_completion(Executed, routes)`——完成协议，纯函数，可单独测。
- `anchor.runtime.execenv.NodeSandbox`——沙箱接线。**任何新 runner 都应经由它**，否则又多一处「按约定
  一致」。

**测试夹具**

- `model_from(*turns, on_extra=…)`——确定性模型，一次请求一项；`on_extra` 用于证明「没有再问」。
- `request(workspace, **overrides)`——最小的合法请求。
- `tests/test_sandbox_live.py` 的 skip 夹具——bubblewrap 不可用时 skip 而非假通过。

**尚未实现的边界**（明确不在本包）

自动压缩、ToolOutputLimits / 大输出落盘、StepPersistence、崩溃恢复、WAITING / 人工输入、前端事件流、
生产配置切换、通用插件平台。`pydantic-ai-harness` 已安装并锁定，但**本包一行未用**。

**已记录、留给后续包的问题**

1. 普通观察以重试提示形式到达模型（§5.3）——是否可接受。
2. 记录不流式（§5.4）——现有观察能力会下降，若要接前端需补。
3. 完成协议在两处各写一遍（§5.5）——是否值得抽成共享模块，需要主验收 Agent 决定，因为它会动旧路径。
4. mini 路径的退出码缺口（§5.1）——本包未修，按计划记录。

---

## 8. 未验证事项

- **真实 provider 下的行为**：全部模型调用都是 `FunctionModel`，未做任何真实请求。真实模型面对「以重试
  提示出现的普通输出」会如何反应，本包无法回答。
- **上下文压缩下的完成协议**：本包不实现压缩，因此「压缩是否会破坏工具调用配对」未触及。
- **`CallDeferred` 路线**：只验到「需要输出类型包含 `DeferredToolRequests`」即停止，未继续。
- **并发与取消**：未测。计划第 5 节明确「主动取消与完整进程树治理不在本包完成声明内」。
- **真实图上的多轮循环与路由分支**：A12 只跑了一条 `Agent→op→Agent` 直线；计划第 6 节允许在图路由分支上
  另补小图，本包未补。
