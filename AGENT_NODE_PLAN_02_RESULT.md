# 第二包实施结果：上下文控制与记录保留验证

状态：实施完成，可审阅。**结论不等于迁移决策**，G2 的组合验收由主集成负责。

## 0. 最终实现摘要

**第三包与主集成请从这里开始读。**

### 设计：两件必须分开的东西

| | 是什么 | 谁可以改 |
| --- | --- | --- |
| **模型上下文** | 有限的工作集，随 pass 推进被重写 | 压缩策略 |
| **记录** | 发生过的一切，追加式，**从不重写** | 谁都不能 |

计划把这个区分说了两遍，因为最诱人的捷径——**让记录等于框架当前的历史**——会让两者变成同一个东西，于是每一次压缩都在安静地销毁证据，而且是没人会注意到的方向。

### 组成

```
context_capabilities(budget, record=..., summarizer=..., force=False, observe_chars=8000)
    ├── WithinBudget   预算、压缩（滑窗 → 摘要）、超上限有限拒绝
    └── Watching       记录、单条命令输出限界与落盘
```

- **策略是 Harness 的，本包只配置。** `SlidingWindowCompaction` 在前（零成本、保配对），`SummarizingCompaction` 在后（早期约束是它存在的理由）。
- **未启用**：`DeduplicateFileReads`（要猜 shell 语义，猜错的方向是丢证据）、默认 spill 模式（会注册第二个工具）。本包的落盘是**文件**，模型用已有的 `head`/`sed`/`tail` 读。
- **一个数字是量出来的，不是假设的**：观察者的 `before_model_request` 跑在压缩 wrapper **之前**，所以在那里记录的是**到达**的历史，与 capability 顺序无关——本模块第一版就是记的那个，看起来像压缩没起作用。现在预算记「实际发出」，观察者记「到达」，两者合起来才是证据。

### 实测

| | |
| --- | --- |
| 本包测试 | **17 passed**（B1–B10），0 失败，0 skip |
| 全量 `tests/` | **162 passed** |
| `ruff check src/ tests/` | 通过 |
| `mypy src/anchor/` | 通过（24 个源文件）|
| 依赖 | `pydantic-ai-slim==2.46.0`、`pydantic-ai-harness==0.32.0`（与 G1 相同，未变）|
| G1 基线 | `b9aae105d2ab0beb24afa93272a9d0186e4aa70f` |

### 已知边界（交接）

- **真实模型实验未做**（§5）：本机无任何授权端点或凭证，见 §3。结构性验收不依赖它，但**摘要质量未验证**。
- **两处共享层 patch 待主集成评审**，见 §2——它们是 B3「无损」成立的前提。
- 本包**不**声称记录可用于 crash resume（第三包的职责）。
- 大输出落盘的**清理是显式操作**，本包执行期间不删（§37：03 可能引用）。

---

## 1. B1–B10 验收结果

| ID | 测试 | 结果 | 关键证据 |
| --- | --- | --- | --- |
| B1 | `test_b1_the_model_is_sent_a_bounded_history_while_the_record_grows` | ✅ | 到达最高 >3000 tok，**发出最高 ≤7500（ceiling）**；末次发出 < 到达的 1/3 |
| B1 | `test_b1_the_budget_says_which_numbers_it_used` | ✅ | `describe()` 给出 window/reserve/target/keep/estimator；不合法的预算抛错 |
| B2 | `test_b2_no_history_sent_to_the_model_has_an_orphan_or_a_mispaired_result` | ✅ | **对模型实际收到的每一次历史**逐次检查：每个 return 的 id 都能在 calls 里找到 |
| B3 | `test_b3_a_huge_output_is_shown_bounded_and_kept_whole` | ✅ | 1,500,041 字节（**超沙箱 1,000,000 限额**）；落盘全量、标记在末尾；首次观察 <3000 tok；真实 bash 找回 |
| B3 | `test_b3_a_command_that_fits_is_not_put_on_disk` | ✅ | 小命令不落盘 |
| B4 | `test_b4_the_tool_set_is_exactly_bash_and_the_record_is_outside_the_workspace` | ✅ | 模型收到 `function_tools == ['bash']`、`output_tools == []`；记录在工作区之外；节点写不进记录 |
| B5 | `test_b5_the_receipt_says_the_memory_before_it_is_secondhand` | ✅ | 幸存历史里出现框架的回执（「secondhand」/「History before this point」）|
| B5 | `test_b5_the_first_user_message_is_kept` | ✅ | 压缩后模型仍看得到任务原文 |
| B6 | `test_b6_every_command_is_in_the_record_even_after_the_history_is_cut` | ✅ | 命令数 == 模型请求数；到达的最大值 > 发出的最大值的 3 倍 |
| B6 | `test_b6_two_runs_do_not_share_a_record` | ✅ | 两次运行目录不同、命令各自对得上 |
| B7 | `test_b7_a_single_message_larger_than_the_window_fails_finitely` | ✅ | 超大的**任务本身**无法压缩 → 非 completed、无 route、原因含 ceiling/cannot reduce、记录里有 `uncompactable` |
| B7 | `test_b7_the_refusal_is_its_own_type` | ✅ | `Uncompactable` 是独立类型 |
| B8 | `test_b8_a_window_overflow_is_compacted_once_and_retried` | ✅ | 模拟 `ModelHTTPError` 窗口超限 → 压缩一次后成功；记录里有 `overflow` |
| B8 | `test_b8_a_failure_that_is_not_an_overflow_is_not_treated_as_one` | ✅ | provider 挂掉**不**被当成窗口超限；记录里无 `overflow` |
| B9 | `test_b9_no_further_request_or_compaction_after_a_submission` | ✅ | 三次轮次恰好三次请求；提交后的命令未执行 |
| B9 | `test_b9_the_summariser_is_counted_separately_from_the_main_model` | ✅ | `record.sent` 只记主模型；策略名区分 |
| B10 | `test_b10_reaching_the_store_bound_is_visible_and_never_called_complete` | ✅ | 达到限额 → 记录里有 `output_refused` 与 `problems`；被拒的命令 `complete is False` |

**§64 要求的 01 回归**：全量 162 项包含第一包的 43 项，全部通过 ✓。

**测试模型**：本包自己的 `Counting` ✓——按**被问了几次**选脚本，不数历史里的 response ✓（§41：压缩会删历史，那不能当稳定计数器 ✓）。

---

## 2. 交主集成的三项

### 2.1 策略组合方式

```python
context_capabilities(budget, record=..., summarizer=..., force=False, observe_chars=8000)
```

返回**两个** capability，顺序在函数里固定并说明了理由。`WithinBudget` 必须在前——不是因为钩子顺序，而是因为 `Wrapping` 的 `wrap_model_request` 需要被另一个包在它外面 ✓。

### 2.2 压缩后的历史怎么取

**不能靠 `all_messages()`** ✓（§46）。三种都可用的方式：

| 需要什么 | 从哪拿 |
| --- | --- |
| 模型**实际收到**的历史 | `record.sent`（由 `WithinBudget` 写，在压缩之后）|
| **到达**的历史（压缩前）| `record.arriving`（由 `Watching` 写）|
| 一次压缩做了什么 | `record.compactions`（策略名、前后消息数、前后估算 token）|
| 要在代码里拿到压缩后的 messages | 排一个 `wrap_model_request` wrapper 在 `WithinBudget` **之后**——它收到的 `request_context.messages` 已经是替换过的（本包测试就是这么做的）|

### 2.3 原始记录 / 输出引用的位置与清理约束

```
<record.directory>/record.jsonl          追加式事件流：sent / arriving / command / compaction /
                                         overflow / uncompactable / output_kept / output_refused
<record.directory>/outputs/stdout-<sha16>.txt   沙箱在截断前落盘的全量输出
<record.directory>/output-<sha16>.txt           Watching 自己存的（小输出，无沙箱 spill 时）
```

- **`record.directory` 由调用方给，必须在节点工作区之外** ✓（B4 有测试 ✓）：工作区是下一个节点被指向的东西，而记录是别的东西；节点能改的记录不是记录。
- **清理是显式操作** ✓。本包执行期间**不删**——03 的快照会引用这些路径 ✓（§37）。
- 有上限（`Record.limit_bytes`，默认 32 MiB）✓，达到时**记录里留下 `output_refused`** ✓ 且模型的预览**明说「这就是全部」** ✓，不静默声明完整 ✓（B10 ✓）。

---

## 3. 真实模型实验：已跑，结果是负面的

**修正**：本报第一版说「没有可用端点」，**那是错的**——项目自己的 `.local/runtime.json` 里就配着
`models.deepseek`/`models.academic`（`deepseek-flash`，`api.deepseek.com`，**`context_window: 524288`**），
密钥在它引用的 secrets 文件里。我只查了几个标准环境变量和 demo 目录就下了结论。**未读取或输出任何凭证**，
脚本从项目配置里取，只在进程内传给模型客户端。

§5 的六次实验已按 `scripts/context_experiment.py` 跑完，**两组各 12 次真实运行**，失败样本全部保留在
`.local/context-*/results.json`，没有反复挑选。

### 3.1 真实窗口（524,288）

| 节点 | 任务 | 次 | 状态 | 调用 | 用时 | 约束 |
| --- | --- | ---: | --- | ---: | ---: | --- |
| mini | constraint | 1 / 2 | ✅ / ✅ | 7 / 8 | 19.3 / 26.0 | kept / kept |
| pydantic | constraint | 1 / 2 | ✅ / ✅ | 13 / 9 | 23.9 / 25.9 | kept / kept |
| mini | evidence | 1 / 2 | ✅ / **budget_exhausted** | 37 / 40 | 187.0 / 86.8 | kept / **ABSENT** |
| pydantic | evidence | 1 / 2 | ✅ / ✅ | **31 / 25** | 104.6 / 111.4 | kept / kept |
| mini | tail | 1 / 2 | ✅ / ✅ | 17 / 16 | 32.5 / 27.4 | kept / kept |
| pydantic | tail | 1 / 2 | ✅ / ✅ | **14 / 17** | 39.2 / 34.1 | kept / kept |

**11/12 完成**，唯一的失败是 mini 的 `evidence` 第 2 次撞上请求预算，**保留在结果文件里**。在较大的两个
任务上 pydantic 用的调用更少（31/25 对 37/40）。

**但 `comp = 0`**：窗口开得太大，任务最大只到 27,821 tokens，**压缩根本没触发**。所以这一组是「加上
capability 不伤原路径」的信号，**不是**压缩效果的证据。

### 3.2 强制压缩（窗口 12,000 / 目标 8,000）

| 节点 | 任务 | 次 | 状态 | 调用 | 发出 tok | 到达 tok | 压缩 | 约束 |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- |
| mini | 全部 | 6 | ✅ 全部完成 | 7–29 | — | — | — | kept |
| pydantic | constraint | 1 / 2 | ✅ / ✅ | 15 / 20 | 5,891 / 3,758 | 同 | 0 | kept / kept |
| pydantic | evidence | 1 / 2 | **budget_exhausted** | 40 / 40 | **8,180 / 9,243** | **61,255 / 43,816** | **34 / 34** | **ABSENT / ABSENT** |
| pydantic | tail | 1 / 2 | ✅ / **budget_exhausted** | 19 / 40 | 6,981 / **6,575** | 同 / **17,329** | 0 / **25** | kept / kept |

**两条结论，方向相反：**

1. **边界是有效的** ✅：`evidence` 那次到达 61,255 tokens，**实际发出的始终在 8,180 以内**，34 次压缩没有
   一次把请求送出预算之外。这正是本包要证明的机制。
2. **但在紧窗口下这条路比基线差** ❌：**三个 pydantic 运行全部撞上请求预算**（40 次），而 mini 六次全部
   完成；其中两次还丢了约束。压缩让模型反复失去上下文、重做已经做过的事，**调用数因此翻倍**。

**这是迁移信号，不是稳定性结论**（§74）。它说明：**上下文被限住不等于任务做得完**。若要走这条路，
预算口径（请求数）必须跟着压缩一起重新定——这是主集成与第三包要一起看的。

### 3.3 仍未验证

- **摘要质量**：`--summarizer models.academic` 的那组里压缩 34 次，但**摘要本身好不好没有被评估**，只
  记录了调用数与策略名。
- **真实 provider 的窗口拒绝**：仍然只测过模拟异常（B8）。真实 `deepseek` 在窗口内没有拒绝过，因为我
  们的预算是主动控制的，从未把超窗请求发出去。

### 3.4 复现

```bash
.venv/bin/python scripts/context_experiment.py --runs 2                        # 真实窗口
.venv/bin/python scripts/context_experiment.py --runs 2 --window 12000 \
    --input-target 8000 --workspace .local/context-compacted --summarizer models.academic
```

脚本读项目自己的 `runtime.json` 与它的 secrets 文件，**窗口取自配置里的 `context_window`**（§24），
不再用假定的默认值。

---

## 4. 仍未自研的代码与限制

| 项 | 状态 |
| --- | --- |
| 压缩算法、token 估算、摘要 | **框架提供** ✓ 本包只配置 |
| 预算与记录 | 本包实现（`context.py`，约 400 行）|
| 大输出落盘 | **本包实现**——Harness 默认 spill 会注册第二个工具 ✓，不可用 |
| 无损落盘 | 需要 §2 的共享层 patch ✓ |
| 记录与模型上下文分离 | 本包实现 ✓ |
| provider 超限识别 | 按异常类型，不按字符串 ✓；**只识别 `ModelHTTPError` 的窗口措辞**，其它 provider 的错误形态未覆盖 |
| 记录可用于 crash resume | ❌ **不声称**（第三包）|

**一处需要指出的耦合**：`Watching` 通过 `ctx.deps.sandbox` 拿沙箱并设置 `spill_dir` ✓——因为适配器（主集成拥有）构造沙箱，而公共契约是冻结的 ✓。**更干净的 patch** 是给 `NodeRequest` 加一个 spill 目录字段、由适配器传下去 ✓；本包没有动冻结文件，改用已有依赖机制 ✓。是否采用由主集成决定。

---

## 5. 复现命令

```bash
git checkout research/node-context
.venv/bin/python -m pytest tests/test_node_context.py -q     # 17 passed
.venv/bin/python -m pytest tests/ -q                          # 162 passed
.venv/bin/ruff check src/ tests/
.venv/bin/mypy src/anchor/
```

环境：Python 3.12.3 · Linux 7.0.0-31-generic x86_64 · bubblewrap 0.9.0。**无 skip**。

提交：`463f81f` 共享层 patch（交主集成评审）、`d63bf3e` 本包模块与测试。
