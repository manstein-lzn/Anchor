# ADR-062 迁移验收（M5）：执行器已切换，mini 已删除

基线：分支 `migrate/agent-node`，M5 提交 `bbefd46` 之后（本文件与删除本身同一提交）。
环境同 G2：Python 3.12.3 · bubblewrap 0.9.0 · `pydantic-ai-slim==2.46.0` ·
`pydantic-ai-harness==0.32.0`。**无付费 provider 调用、无凭证读取**；所有模型调用都是
`FunctionModel` 或写死的命令脚本。

## 1. 状态

迁移的五个步骤全部落地：M0 冻结契约（ADR-062）、M1 拆分 node runtime、M2 op runtime、M3 切换调度器
并把 `already_submitted` 变成调度器自己的读、M4 兼容边界（不转换历史运行）、M5 端到端验收并删除
mini。**生产图不再调用 mini-swe-agent，`simple/agent.py` 已从树中删除。**

## 2. §12 验收矩阵逐项

§12 的九项要求，以及每一项跑出来的是什么：

| §12 要求 | 覆盖 | 结果 |
| --- | --- | --- |
| Graph schema/reads/writes/route 校验 | `tests/test_graph_*.py`、`tests/test_examples.py` | 通过 |
| Agent 单节点、Op 单节点、Agent→op→Agent | `tests/test_node_controlflow.py`（真沙箱、真 console script）、`tests/test_ops.py`、`tests/test_node_integration.py` | 通过 |
| 上下文压缩后恢复 | `tests/test_node_context.py`；故障矩阵 B1、B2×3 | 通过 |
| B1–B8 故障矩阵 | `scripts/recovery_windows.py`，26 窗口 | 全通过（§3） |
| 预算跨主模型/摘要/重试恢复 | `tests/test_node_context.py`；A5、B5、C8 | 通过 |
| 完成事实拒绝反例 | `tests/test_node_controlflow.py`（假提交、非零退出带 marker、超时带 marker）；A4、C7 | 通过 |
| 大输出恢复读回和只读挂载 | `tests/test_sandbox_live.py`；B4、B4-partial | 通过 |
| Graph resume 跳过已完成 Node | `tests/test_simple_run.py`；B8、A6 | 通过（B8 图闭环；A6 见 §4） |
| mini 路径删除后的旧测试替换为新 Node API 测试 | 见 §5 | 通过 |

全量：**238 passed**（`.venv/bin/python -m pytest tests/ -q`，约 130s）· `ruff check src/ tests/
scripts/` 通过 · `mypy src/anchor/`（30 files）通过。

## 3. B1–B8 / C1–C9：26 个窗口全部通过

`PYDANTIC_AI_NO_BANNER=1 .venv/bin/python scripts/recovery_windows.py` —— 26 个 `=== … ===`，
无 `BAD`、无 `FAIL`。摘要：

- **C1–C5**：`replayable` / `uncertain` / `uncertain` / `continuable` / `uncertain`，与计划一致；
  C5 的"旧 complete snapshot 不覆盖后来的副作用"是对的，counter 停在 2 而不是 3。
- **C9**：宿主被 SIGKILL 后沙箱内进程没有继续（0 个残留进程），状态 `uncertain`。
- **B1**：真压缩（`SummarizingCompaction`）之后 kill，续跑后输入有界（最大 3343 token）、约束在
  17/17 次请求里都在、步骤各记录一次。
- **B2×3、B3×3**：压缩前后与三种副作用间隙，选中历史自洽；三个间隙全部 `uncertain`。
- **B4、B4-partial**：恢复进程按只读挂载读回 300074 字节尾部；超出存储时"被裁"是明说的。
- **B5**：主模型 10 + 摘要 6 = 16，等于持久化的 `requests_allowed=16`；耗尽后再问一次是
  `budget_exhausted` 且**零请求**。
- **B6**：追加记录只增不减（20430 → 86320 字节），各类原始事实都在，trace 不代替它。
- **B8**：恢复的图只问 `finish`，`write`/`count` 因为 pass 已记录而**根本没被提名**，结束
  `finished`，无重复。
- **A1**：第二个进程只拿到引用，1 次请求后 `completed`，counter 停在 1。
- **A3**：`uncertain` → 零请求仍然 `uncertain`；`continuable` → 继续；`finished` → 零请求交回结果。
- **A4**：11 类错引用/坏文件，全部带理由拒绝，且**没有跑任何命令**。
- **A5**：反复 kill 直到额度用尽，预算文件与模型自己数的次数一致，之后零请求。
- **A6**：`blocked`，见 §4。
- **C6–C8**：同一引用两次评估不重复确认副作用；乱码/伪造/不存在的引用分别拒绝；重启不退还已花额度。

## 4. 两条明确的、不改的结论

**A6 `blocked` 不是回归，是设计边界。** §9 要求"节点已提交但图还没记下来"能被接住。节点侧做到了：
store 里的 run 判定为 `finished`，交回引用得到 `wrote it` 且 0 请求。图侧接不住——一个提交了的
节点不再发起模型请求，所以最后一次 agent 侧钩子在提交**之前**，下一次属于**下一个节点**；
`run.py` 里 `agent.run(...)` 返回到 `_record(...)` 之间没有钩子。**但是 M3 的
`_settled_already` 正是为这个缝隙写的**：调度器在跑节点前读 `control/<node>` 的完成事实，读到就
直接记录这次 pass。B8 证明它在真图里生效。A6 停在 `blocked` 是因为它的钩子位置由框架决定，
而不是因为缝不存在。

**旧 runs 只读，不转换。** M4 的边界：历史 `runs/` 继续由 view 展示，没有代码把它读成
`RecoveryRef`，也没有代码把旧 trace 迁移成新事件格式。

## 5. 删除清单（本提交）

| 删除 | 原因 |
| --- | --- |
| `src/anchor/simple/agent.py` | mini 的 `TracingAgent`/`SandboxEnvironment`/`OpEnvironment`/`build_agent`/`scripted_model`/`system_prompt`；第二个完成解析器 `_check_finished` 随之消失 |
| `tests/test_agent_resume.py` | 只测 mini 的 resume 语义，无对应物 |
| `requirements/node-verification.txt` | 目录清空；pydantic-ai 进 `pyproject.toml` |
| `pyproject.toml` 的 `mini-swe-agent>=2.4,<3` | 换成 `pydantic-ai-slim==2.46.0` + `pydantic-ai-harness==0.32.0` |
| `run._messages` / `node_bridge.read_trace_messages` | M4 已删（无人调用，且 `resume` 不再接收参数） |

保留：`tests/test_node_context.py` 里引用历史 run 布局的那处断言（只读历史，不是活路径），以及
`node/op_runtime.py`、`node/agent_runtime.py` 里提到 `OpEnvironment`/mini 的注释——它们现在是在说
"这些东西从哪来、为什么这么写"，不是在说"当前实现是它们"。

## 6. 仍然不承诺

不提供 exactly-once、断电耐久性、真实 provider 窗口错误覆盖，也不做任意外部副作用的自动对账。
与 `AGENT_NODE_VALIDATION_RESULT.md` 一致，迁移不改这些边界。
