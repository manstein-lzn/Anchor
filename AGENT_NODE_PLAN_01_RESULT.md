# 第一包实施结果：PydanticAI Node 最小适配与控制流验证

状态：实施完成，可审阅。**本包结论不等于迁移决策**——按计划第 8 节，只有主验收 Agent 复核并通过
完成语义、真实沙箱、图集成三项后，此入口才冻结给第二、三包并行使用。

**结论一句话：PydanticAI 能保住 Anchor 的五条执行语义，只用公开 API，而且不复用任何框架的终止特性
——`bash` 是普通函数工具，停止由两处自己做（工具内的完成状态检查 + 公开迭代边界），三条要求同时成立。
这修正了本报第二轮的结论，见 §5.9。**

> **第二轮修订（审阅后）。** 审阅指出四处基础问题，已全部修复并补测试：请求计数在失败路径上错误
> （§5.7）、失败时完整对话记录丢失（§5.7）、沙箱构造与探针在异常处理之外（§5.7）、真实图只验证了
> 直线（§4 的 A12b）。
>
> **第三轮修订。** 第二轮把「`ToolOutput + ModelRetry` 会让 Harness 压缩失明」报为需主验收 Agent
> 决定的首要问题，并称「三个要求不可兼得」。**这个结论是错的**，审阅给出了正确设计：`bash` 保持普通
> 函数工具，停止由工具内的完成状态检查 + 公开迭代边界完成。已按此重写并通过全部验收（§5.9）。
> §5.6 保留原样，作为「为什么不是那条路」的记录。

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

**这条不能只当作措辞差异接受——见 §5.6，它与 Harness 的压缩机制不兼容。** 

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

---

## 5.6 审阅后新增的决定性发现：工具记录与 Harness 压缩不兼容

审阅要求「启用真实 Harness 生命周期观察，确认普通 bash 成功、命令失败和显式提交分别如何被记录」。
**结果是不可接受，而且比审阅预判的更严重：不是记录形态不好看，是第三包要用的压缩机制根本看不到我们的
工具输出。**

### 机制

`pydantic-ai-harness` 的 `compaction` 提供 `ClearToolResults`——**长节点上下文不无限增长所依赖的那个
能力**。它靠 `iter_tool_pairs` 找出可清理的工具结果，而该函数的判据是（`compaction/_shared.py:761`）：

```python
if isinstance(part, ToolReturnPart) and part.tool_call_id in calls:
```

**只认 `ToolReturnPart`。**

而本适配器的普通命令是 `RetryPromptPart`——因为输出工具不返回就必须抛 `ModelRetry`（§5.3）。

**后果**：`ClearToolResults` 能清掉的只有那一条提交（它是输出工具的返回，是 `ToolReturnPart`），
**清不掉任何一条普通命令的输出**——也就是它本来要回收的全部东西。第三包的上下文治理会在接入后才发现
自己什么也没清。

由真实运行的 trace 佐证（§5.7 的测试把这条钉住）：

```
1: response [call(bash: echo one), call(bash: echo two), call(bash: echo three)]
2: request  [RETRY(one), RETRY(two), RETRY(three)]     ← 三条 exit 0 的成功命令，全是 RetryPromptPart
```

### 三个要求不可兼得（均为实测）

| 机制 | ① 模型只看到一个工具 | ② 提交后同响应剩余调用不执行 | ③ 普通观察是 `ToolReturnPart` |
| --- | --- | --- | --- |
| 输出工具 + `ModelRetry`（当前实现） | ✅ | ✅ | ❌ `RetryPromptPart` |
| 函数工具 + `CallDeferred` | ✅ | ❌ 剩余照跑（实测 `after` 执行了） | ✅ |
| 函数工具 + `SkipToolExecution` | ✅ | ❌ 剩余照跑 | ✅ |
| 输出工具 + `ToolFailed` | ✅ | — 整个 run 失败 | — |

① 来自协议（单 bash），②来自计划第 5 节，③来自 Harness 的压缩实现。**公开 API 下没有组合能同时满足。**

### 后续：这条不是死路，是被一条更好的路取代

上面这张表曾让本报得出结论「三个要求不可兼得」。**那个结论错了**——它把「框架不能停止这一批」等同于
「这一批的命令必须执行」。审阅指出正确的区分，设计见 §5.9：**`bash` 保持普通函数工具，纪律放在工具
内部和公开迭代边界上**，于是①②③同时成立。下面三个候选因此都不必采用，保留在此说明为什么当时会走到
这里。

1. **接受现状并承担第三包的成本**：自己在压缩前把 `RetryPromptPart` 归一化为 `ToolReturnPart`。这是
   把框架的形状差异补在 Anchor 里，与本包「减少自研」的目标相抵。
2. **放弃 ②，改用函数工具 + `CallDeferred`**：记录形态正确、压缩可用，代价是**同响应内提交之后的命令
   仍会执行**（`anchor-done; rm -rf x` 会把 `rm` 跑掉）。这是协议语义的实质放宽，不能默默做。
3. **把提交做成第二个模型可见工具**：记录与停止都正确，代价是**破坏单 bash**。与协议冲突，但最少自研。

本包**不替主验收 Agent 选择**，按计划第 7 节停在此处的具体失败证据上。

---

## 5.7 第二轮修复

四处，全部有回归测试。

### 请求计数：已修

原实现在失败与预算退出路径上把 `wiring.commands`（**命令数**）填进 `model_requests`。审阅的复现：

| 实际行为 | 修复前 | 修复后 |
| --- | ---: | ---: |
| 1 次模型请求，只回复文本 | 0 | **1** |
| 1 次模型请求，包含 3 条命令 | 3 | **1** |

两个方向都错，而第三包的累计预算正要建在这个数上。修法：在模型外面套一层 `WrapperModel` 子类
（`_CountingModel`）在 `request()` 处计数——**在发生的地方数**，于是成功、失败、预算三条路径报的是
同一个东西。之所以不能事后从框架读：`usage` 挂在 result 上，而这两个路径恰恰没有 result。

测试：`test_the_request_count_is_the_model_request_count_and_not_the_command_count`。

### 失败时的完整对话记录：已修

原实现只在成功返回后取 `messages`，异常路径的 trace 只有一行退出记录——**失败后诊断所需的证据实际上
不存在**。修法：改用 `agent.iter()`，因为 `AgentRun.all_messages()` 在任何时刻可读，**包括异常处理器
里**；两条路径都写完整对话。

测试：`test_the_conversation_is_in_the_record_when_the_pass_does_not_finish`。

### 沙箱构造与探针在异常之外：已修

`NodeSandbox` 构造与 `require_working()` 原本在 `try` 之前，沙箱起不来会直接抛出，而契约承诺的是返回
一个结果。已移入 `try`，现在返回明确的 `failed` 与原因。

测试：`test_a_sandbox_that_cannot_start_is_a_failed_result_and_not_an_exception`。

### 真实图只验证了直线：已补

新增 A12b：一张带分叉的图（`decide → left | right`），**走哪条臂由适配器返回的 route 决定**，通过真实
`anchor-route` 选择。断言 `executed == ["decide", "right"]`、`skipped == ["left"]`、真实 op 产物内容、
以及记录里的 `route`。

测试：`test_a12b_the_route_the_adapter_returns_drives_a_real_branch`。

### 修复过程中暴露的第五处：重试预算 ≠ 请求预算

一个响应里如果有**多于剩余预算**的命令，框架会先以 `UnexpectedModelBehavior: Exceeded maximum output
retries` 失败，而不是返回 `budget_exhausted`。也就是说 A10 的「预算是自己的状态」在「一响应多命令」这个
形状下不成立。**本包未修**——它取决于 §5.6 的选择：若保留 `ModelRetry` 路线，`max_retries` 与
`request_limit` 的关系需要重新定义并单独验证。

---

## 5.8 第二轮后的计数与状态

| | 第一轮 | 第二轮 |
| --- | ---: | ---: |
| 本包测试 | 32 | **38** |
| 全量 `tests/` | 134 | **140** |
| 失败 / skip | 0 / 0 | **0 / 0** |
| `ruff` / `mypy` | 干净 | **干净** |

**仍然没有 skip**：本机 bubblewrap 可用，全部真实执行。

**本包的验收状态：控制流已证明可实现；入口在 §5.6 的问题解决前不宜冻结。** 按审阅意见，记录、计数与
Harness 兼容性三项已分别有结论，其中兼容性一项是**否定的**。

---

## 5.9 第三轮：按审阅给的设计重写，三条要求同时成立

### 设计

**`bash` 保持普通函数工具。** 终止不靠框架特性，靠两处自己做的检查：

1. **普通命令正常返回**——`ToolReturnPart`，这是记录与上下文机制都认的形态；
2. **识别提交后把完成结果存在节点状态里**——命令本身不需要 run 在那里结束；
3. **同响应后续调用即使被框架派发，也在进入沙箱前检查完成状态并跳过**——计划 A4 的要求是「必须观察
   实际副作用」，**框架处理了后续调用，不等于后续命令必须实际执行**；
4. **在工具处理结束后的公开迭代边界直接返回 Node 结果**，不再请求模型。

四小块，无私有 API，三条要求同时成立：单工具、提交处停住效果、记录可被 Harness 读取。

### 四条声明逐一实测

| # | 声明 | 测试 | 结果 |
| --- | --- | --- | --- |
| 1 | 普通命令正常返回 `ToolReturnPart` | `test_what_an_ordinary_command_is_recorded_as` | ✅ 真实运行里 `['user-prompt','tool-call','tool-return']`，**无 `retry-prompt`** |
| 2 | 识别提交后保存完成结果 | `test_a1_one_bash_tool_writes_a_file_and_submits` | ✅ `submission` 来自 CLI，`route` 正确 |
| 3 | 同响应后续工具被派发但不进沙箱 | `test_a4b_a_later_call_is_dispatched_and_does_not_reach_the_sandbox` | ✅ 记录里有 `after` 的 call，**沙箱没跑它**，结果记为 skipped |
| 4 | 工具处理后的迭代边界直接返回 | `test_a3_the_model_is_never_asked_again_after_a_submission` | ✅ 模型在提交后被调用即抛错；实跑 `requests == 1` |
| — | Harness 压缩**能**看到普通观察 | `test_the_harness_compaction_can_see_an_ordinary_observation` | ✅ 普通命令是 Harness 认的配对 |

**§5.6 的问题因此消失**：`ClearToolResults` 现在能读到并清理每一条普通命令的输出。

### 我在这一轮走错的两处，记录在此

**① 「不可兼得」是错的。** 我把「框架不能停止这一批」当成了「这一批必须执行」。审阅指出的区分是对的：
`end_strategy='early'` 只是**框架**停止批次的机制；工具**自己**检查完成状态是另一个机制，而后者不
影响记录形态。我用前者的失败推出后者不可能，是推理错误。

**② `end_strategy` 必须回到默认值。** 在 `'early'` 之下，**函数工具只在「所有输出工具都失败」时才运
行**——而没有输出工具时，一件都不跑。这个错误的表现是：冒烟测试立刻耗尽 60 次请求、产物不存在。上表
说明 `'early'` 是**被替换掉的那个设计**需要的设置；换成普通工具后它是错的。

### 过程中新发现的一处记录缺口（已修）

**pass 提交时，最后一批工具的结果进不了框架的 history。** `agent.iter` 在节点**进入时**yield，而在
「工具处理结束后」break 意味着 break 的那个节点还没运行——而正是它的运行会把上一批的结果写进
`all_messages()`。表现：一条提交的 pass，记录里**只有 tool-call，没有任何 tool-return**，包括那条
skipped 和提交自身的观察。

这不影响上下文压缩（那一批已经是这次 pass 的末尾，压缩不会再看到它），但**记录会缺掉一次 pass 里最
重要的部分**。修法：适配器把自己派发过的每条命令（含 skipped）连完整输出写入记录的 `exit` 行，不依赖
框架的 history 停在哪。`test_a4b` 因此读的是记录里自己的账。

### 第三轮后的状态

| | 第一轮 | 第二轮 | 第三轮 |
| --- | ---: | ---: | ---: |
| 本包测试 | 32 | 38 | **39** |
| 全量 `tests/` | 134 | 140 | **141** |
| 失败 / skip | 0 / 0 | 0 / 0 | **0 / 0** |
| `ruff` / `mypy` | 干净 | 干净 | **干净** |

**本包验收状态：四条控制流声明全部实测成立，Harness 兼容性问题已消除，入口可以交主验收 Agent 复核。**
§5.8 里「重试预算 ≠ 请求预算」那处观察随之作废：`ModelRetry` 已不用于普通观察。
