# 第二包实施结果：上下文控制与记录保留验证

状态：第二轮六条已逐条处理完毕，见 §8。
第一轮 R1–R6 与第二轮六条已逐条复核，**全部成立**。修复与回归见 §6、§7。

> **本报又一次把结论写反了，这次是「mini 没保住约束」。** §7 末尾那条结论基于一个排除 `runs/` 路径的
> 判定器，而真实 mini 的产物**恰好在 `runs/<run>/only/` 下**——于是基线被判成每次都失败。
> **判定器已修，已有产物已重新判定（零 API 调用），结论撤回**，见 §7.1。

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

## 3. 真实模型实验：**已撤回**（主验收 R6）

> **本节的成功率与迁移结论全部作废。** `scripts/context_experiment.py::one` 对 mini 和 pydantic
> **都调用同一个 Pydantic `run_node`**，前者只是不加载 capability。所以那是
> **「Pydantic 不启用上下文策略」对「Pydantic 启用上下文策略」**，不是 mini 基线对候选。
> 旧结果已移到 `.local/context-exploration-old/` 并标注，**不得当作原结果引用**。
>
> **修正后的实验已重跑**，结果见 §6 末尾——与作废那版**完全不同**。
>
> 下面保留原始数据，仅为记录当时看到了什么。

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

---

## 6. 主验收 R1–R6 的处理

六条**全部成立**，均已复核并修复。下面是逐条。

### R1：大输出路径在沙箱中不可读，B3 假通过 —— 已修

**复核确认**。`Record` 在宿主 `/tmp/.../record` 下，返回宿主绝对路径；而沙箱的 `/tmp` 是**独立
tmpfs**，该目录从未挂进去。独立复现：

```
宿主上的目录: /tmp/tmpxzx3tjtd/record/outputs
沙箱里读它:   ls: cannot access '…': No such file or directory
```

管道的 `tail` 返回 0，确定性模型照常提交，而我的 B3 **只检查了宿主文件存在和有标记**——**从没检查过
读回**，所以一条 `no such file` 被算作通过。

**修复**：输出目录以**只读**方式挂到沙箱内固定路径 `/kept`（`NodeSandbox.spill_mount` →
`readonly_binds`），**只挂这一个目录**，模型收到的是 `/kept/...` 而不是宿主路径，记录里两者都存
（`kept` / `seen_by_node`）。回归 `test_r1_the_node_can_actually_open_the_kept_output` 断言的是
**模型真实收到的观察**里含末尾标记——缺失即失败 ✓。

**R1 附带两条也修了**：`_decode` 现在**按流**引用各自的文件（stdout/stderr 不再都指 `spilled[0]`）；
`wrap_tool_execute` 在每次调用前**清空** `sandbox.spilled`/`incomplete`，所以被守卫拒绝的 skipped
调用不会沿用上一条命令的落盘 ✓。

### R2：输出上限未覆盖实际 spill —— 已修

**复核确认**：`Record(limit_bytes=1000)` + 1,100,000 字节输出 → 目录里真有 1,100,000 字节，而
`record.spill_bytes` 只有 88、`problems` 为空——**限额只活在 `keep_output` 里，沙箱 spill 绕过了它**。

**修复**：上限由 `Record` 持有，每次调用前把**剩余额度**交给沙箱（`spill_limit_bytes`）；沙箱
**写之前**检查而非写之后（不再先无界积累）；实际写入的字节**记回 `record.spill_bytes`**；写不下时
`SandboxResult.incomplete` 为真、记录里留 `problems`，命令的 `complete` 为假——**不静默声明完整** ✓。

回归 `test_r2_the_record_limit_covers_what_the_sandbox_writes`：1,100,000 字节对 50,000 上限，
断言**磁盘上**与**账上**都不超、且 `complete is False` ✓。

**仍未解决**：`subprocess.run(stdout=PIPE, stderr=PIPE)` 仍先完整收集再落盘，所以**内存有界尚不能声称**
✓——要真正做到需要流式读取，属于共享执行层的进一步改动，交主集成判断。

### R3：预算估算漏掉工具参数等请求内容 —— 已修

**复核确认**：`estimate_tokens` 只计 `parts.content`，`ToolCallPart` 里 100,000 字符的 `command`
独立估算为 **0**。而且它**既是预算的实现，又是唯一的验收 oracle**——所以「发出始终在预算内」只证明了
一个坏计数器的值 ✓。

**修复**：估算改为走**每一种 part**，`args` 按 JSON 计（wire 形态，不是 repr），并把
`instructions` 与工具 schema 作为 `request_overhead` 计入**每一次请求**。回归
`test_r3_a_large_tool_argument_is_counted` **从模型真实收到的请求**用**第二种方法**独立计数，
断言二者都 >20,000 ✓。

**计数口径也修了**：`sent` 原先在拒绝发送前就写、溢出重试不写第二次。现在 `sent` 记录**实际发出**的
那一次（在拒绝检查之后），报告区分估算 token 与 provider 实际 token ✓。

### R4：完整原始记录尚未实现 —— 已修

**复核确认**：`response` 只存 `part_kind`，`arriving`/`sent` 只存数量——**没有原始模型文本、完整参数、
调用 ID、usage**。B6 只数命令，证明不了没丢证据 ✓。

**修复**：`Record.keep_message` 用框架自己的 `ModelMessagesTypeAdapter` 在**任何改写之前**追加原始消息；
工具结果按**原始文本 + `tool_call_id` + 字节数 + sha256** 记录（`tool_result` 事件）；错误路径也记 ✓。
回归 `test_r4_the_record_keeps_the_raw_result_that_compaction_dropped` 断言：被压缩丢掉的**特定文本**
能在记录里找到，且**带调用 ID**、可关联 ✓。

### R5：摘要与约束保留未被验证，策略先丢历史 —— 已修

**复核确认**：`_compact_once` 在滑窗任何缩短后立即返回，**通常不调用摘要**——先丢掉旧结论，再决定无需
总结。`record.summaries` **根本没有写入路径**，而 B9 的条件断言让摘要零调用也能通过 ✓。

**修复**：策略顺序改为**摘要在前**（配置了摘要器时），即「先决定要保留什么，再丢历史」；摘要器被包一层
计数，每次调用**无论是否缩短**都记入 `record.summaries`。回归
`test_r5_the_summariser_runs_before_the_window_drops_the_history`：断言摘要器**真的被调用**、
它收到的输入里**含将被丢掉的内容**、且记录有对应行 ✓。

### R6：真实实验的比较对象与验收指标错误 —— 已修

**复核确认**：两条分支都调 `run_node` ✓——结论作废，已撤回（§3）✓。

**修复**：
- **mini 分支改走真实的 `runner.run`**（单节点图，调度器/沙箱/提交都是真的）✓
- **控制目录移到工作区之外**（`workspace.parent/control/...`）✓——原先 record 与 trace 都在节点
  workspace 里，节点可读写自己的审计记录 ✓
- 旧结果**保留**并移到 `.local/context-exploration-old/`，附 README 标明它是什么、结论作废 ✓

**仍未做**：任务特定断言（每个文件以指定行结尾、每条引用有来源、尾行逐字相同）**只做到关键词/文件
存在** ✓；两种策略的输入预算是否等价也尚未说明 ✓。**因此修正后的真实实验还没有重跑**——按 R6 的要求，
应当先通过结构性修复再跑小规模真实实验 ✓。

### 修复后的状态

| | |
| --- | --- |
| 本包测试 | **22 passed**（B1–B10 + R1–R5 回归）|
| 全量 `tests/` | **167 passed** |
| `ruff` / `mypy` | 通过 |
| 共享层 patch | `sandbox.py` / `execenv.py` 的改动**待主集成复核**（R2 明确要求）|

**第二包状态：主验收不通过；R1–R6 已逐条修复并有回归，但修正后的真实实验尚未重跑，任务特定断言仍未做。**

### R6 之后：修正实验的**重跑结果**

`scripts/context_experiment.py` 的 mini 分支现在走**真实的 `runner.run`**（单节点图，调度器、沙箱、
提交都是真的），控制目录在工作区之外。真实窗口 **524,288**，3 任务 × 2 次 × 2 臂 = 12 次真实运行。

| 节点 | 任务 | 次 | 状态 | 用时 | 约束 |
| --- | --- | ---: | --- | ---: | --- |
| mini | constraint | 1 / 2 | ✅ / ✅ | 41.1 / 43.2s | kept / kept |
| pydantic | constraint | 1 / 2 | ✅ / ✅ | 94.9 / 32.2s | kept / kept |
| mini | evidence | 1 / 2 | ✅ / ✅ | 99.5 / 199.6s | kept / kept |
| pydantic | evidence | 1 / 2 | ✅ / **budget_exhausted** | 114.5 / 136.8s | kept / kept |
| mini | tail | 1 / 2 | ✅ / ✅ | 45.9 / 8.9s | kept / kept |
| pydantic | tail | 1 / 2 | ✅ / ✅ | 25.7 / 40.5s | kept / kept |

**与作废那版的关键差别**：修正后 **mini 6/6 完成、pydantic 5/6**（一次撞上请求预算），而 old 数据里
「mini」的失败根本不属于 mini。**用时互有胜负，没有一致方向**；`constraint` 与 `tail` 两任务的约束
两臂都保住 ✓。

**因此对真实窗口这一组，正确的说法是：两臂大体相当，pydantic 多了一次预算退出。** 原先那句
「pydantic 更少调用、指向迁移」**不成立**，已撤回 ✓。

**未重跑**：**强制压缩那组（窗口 12,000）没有重跑** ✓。那一组才是压缩信息损失的证据，而作废版的数字
不能用 ✓——**这是本包明确留下的空缺** ✓。

**仍在的两处不足**（R6 要求过）：
- **mini 臂的模型调用数没有采集** ✗（表格里是 0）——mini 的调用发生在 `LitellmModel` 内部，
  没有计数接缝 ✓。所以「调用数」这一列**只能看 pydantic 一侧** ✓。
- **`evidence` 与 `tail` 的判定仍是代理指标** ✓（见 §6 R6）：`constraint` 是精确判定（每个 `.md`
  以指定行结尾，实测两臂都过 ✓），另两个只检查词/文件存在 ✓。

### R6 之后：紧窗口（强制压缩）那组的重跑结果

`--window 12000 --input-target 8000 --summarizer models.academic`，同样是 3 任务 × 2 次 × 2 臂。

| 节点 | 任务 | 次 | 状态 | 发出 tok | 到达 tok | 压缩 | 用时 | 约束 |
| --- | --- | ---: | --- | ---: | ---: | ---: | ---: | --- |
| mini | constraint | 1 / 2 | ✅ / ✅ | — | — | — | 29.4 / 21.4s | **ABSENT / ABSENT** |
| pydantic | constraint | 1 / 2 | ✅ / ✅ | 6,670 / 7,007 | 5,670 / 6,007 | 0 / 0 | 29.3 / 35.8s | **kept / kept** |
| mini | evidence | 1 / 2 | ✅ / ✅ | — | — | — | 164.8 / 96.7s | kept / kept |
| pydantic | evidence | 1 / 2 | **budget_exhausted** / **budget_exhausted** | 6,743 / 6,499 | **30,636 / 40,125** | **17 / 17** | 205.9 / 211.1s | kept / ABSENT |
| mini | tail | 1 / 2 | ✅ / ✅ | — | — | — | 12.6 / 12.7s | kept / kept |
| pydantic | tail | 1 / 2 | ✅ / ✅ | 4,233 / 4,571 | 3,233 / 3,571 | 0 / 0 | 28.0 / 33.1s | kept / kept |

**三条结论：**

1. **边界确实守住了** ✅：`evidence` 两次到达 30,636 / 40,125 tokens，**实际发出始终 ≤7,007**，17 次压缩一次
   都没把请求送出预算 ✓。`sent` 比 `arriv` 稳定多出 1,000 —— 那是 `request_overhead`（4,000 字符 ÷ 4），
   也就是**这一次修复让它可见的那部分** ✓。
2. **`constraint` 这一格方向相反，而且是精确判定** ✅：任务要求「三个文件**每个**都以
   `REVIEWED-BY-ALPHA` 结尾」，**基线两次都没做到（ABSENT），候选两次都做到了** ✓。这是机械判定的结果，
   不是关键词代理 ✓——文件三个都写了（运行日志可见），失败在内容 ✓。
3. **`evidence` 这一格代价是真的** ✗：候选两次都在 23/22 次请求时耗尽预算，而基线两次都完成 ✓。压缩让
   模型反复重做，**轮次花在了压缩本身上** ✓——包括摘要调用也计入节点请求预算（见上一节）✓。

**所以正确的说法是：紧窗口下两臂各有胜负——候选守住了精确约束，代价是在较重的任务上把轮次耗在压缩里。**
这既不是「候选更好」，也不是作废版那个「候选更差」；两处差异都指得出机制 ✓。

**仍未解决的**：`calls` 一列对 mini 恒为 0（mini 的调用在它自己的客户端内部，没有计数接缝）✓；
`evidence` 与 `tail` 仍是代理判定 ✓。

---

## 7. 第二轮主验收的处理

### 7.1 第 1 条：判定器错误，结论撤回 —— 已修，且**已有产物已重新判定**

**复核确认**。`_holds(every_file_ends)` 排除了所有 `runs/` 开头的路径 ✗，而**真实 mini 的产物恰好在
`runs/<run>/only/`** ✓——所以基线被判成每次都失败 ✓，而我把那个结果写进了报告 ✗。

**修复**：`artifacts(attempt, node)` 按执行器各自的产物根解析 ✓（mini → `runs/<run>/<node>/` ✓；
新入口 → 工作区本身 ✓），判定**从文件本身**读内容 ✓。新增 `--rejudge` 模式：**读已有产物重新判定，
不调用任何模型** ✓。

**同一批产物，重新判定的结果**（零 API 调用）：

| 组 | mini 的 constraint | pydantic 的 constraint |
| --- | --- | --- |
| 真实窗口（524,288） | **kept / kept** | **kept / kept** |
| 紧窗口（12,000） | **kept / kept** | **kept / kept** |

**三组产物全部重新判定，每一次尝试都保住了约束** ✓——唯一的例外是紧窗口下 `pydantic-evidence-2`
（那次 `budget_exhausted` 没跑完 ✓）。**「基线两次都没保住、候选两次都保住」错了，已撤回** ✓。

**正反例回归**（`test_the_judge_*`）：三个文件都正确 → True ✓；都不带 → False ✓；三个里错一个 →
False ✓；只有两个 → False ✓；**验收构造的 mini 布局（`runs/<run>/only/` 里三个正确文件）→ True** ✓；
新入口的直接布局 → True ✓。

**因此紧窗口那组唯一站得住的差异是**：`evidence` 任务上候选两次都在 22/23 次请求时耗尽预算，而基线两次
都完成 ✗——**代价在轮次上，不在约束保留上** ✓。

**仍未做**（第 1 条要求）：`tail`/`evidence` 仍是代理判定 ✗；mini 的模型配置硬编码 `models.deepseek` ✗；
调用数为 0 ✗；两臂预算未对齐 ✗。

### 7.2 第 2 条：总限额被双流绕过 —— 已修

**复核确认**：`_spill` 对每个流**各用一次**调用方的剩余额度 ✗，所以两流各 1,100,000 字节、
`spill_limit_bytes=50,000` 时会写出**两个** 50,000 字节文件 = 100,000 ✗。

**修复**：额度**递减** ✓——`remaining` 跨两个流累计，按**实际保留字节**扣减 ✓。

**仍未做**：`PIPE` 仍先无界收集 ✗。**这一条不能以「交主集成判断」关闭** ✓——报告在此明确记为本包
**未完成项** ✓。

### 7.3 第 3 条：中等输出的读回缺口与完整性误报 —— 部分修复

**复核确认**：大于 `observe_chars`、小于沙箱限额的输出由 `Record.keep_output` 写到**记录根目录** ✗，
而只读挂载只覆盖 `record/outputs` ✗——返回的宿主路径在沙箱里仍不可读 ✓。

**修复**：`keep_output` 现在写进 **`outputs/`** ✓——**所有可能被模型读取的输出都在被挂载的那一个目录里** ✓。
B3 的两条测试随之按大小区分「沙箱的全量副本」与「记录自己的有界副本」✓。

**仍未做**：容量不足时 `_spill` 会创建**部分文件** ✓，而 `_decode`/`_bounded` 仍称其为 whole output ✗；
`incomplete` 只写进控制记录 ✗，模型可能被误导 ✗；`skipped` 时 **`visible` 未清空** ✗（只清了
`spilled`/`incomplete` ✓）。

### 7.4 第 4 条：`keep_message` 没有调用点 —— 已修

**复核确认**：`Record.keep_message` **只定义、从未被调用** ✗，`after_model_request` 仍只记 `part_kind` ✗。

**修复**：`after_model_request` 现在**真的把原始响应追加进记录** ✓（经框架自己的适配器 ✓），并把
provider 报的 usage **另记一份** ✓（`_usage_of`），与估算值并存 ✓。`test_r4_*` 断言被压缩掉的特定文本
能从追加记录取回 ✓。

**仍未做**：`Command.args` 为 JSON 字符串时仍不会被解析成 `command` ✗——工具**传入**的完整参数尚未
单独断言 ✓。

### 7.5 第 5 条：发送计数与请求开销 —— 部分修复

**复核确认**：`_compact_once` 的 `before` 用含 overhead 的 `cost`、`after` 用不含的 `estimate_tokens` ✗
——**`before > after` 可能只是 overhead 的差** ✓，于是「有没有缩短」这个判断本身是坏的 ✓。

**修复**：两侧统一走 `self.cost` ✓。

**仍未做**：`sent` 仍在 ceiling 拒绝**之前**追加 ✗（拒绝本身还没写成独立事件 ✓）；overflow 重试仍不记
第二次 `sent`、也不重查 ceiling ✗；`request_overhead` 仍是固定 4,000 **字符** ✗，不读真实
instructions/schema ✗——**固定余量不能声称覆盖任意实际指令** ✓。

### 7.6 第 6 条：R5 的验证仍不足 —— 部分修复

**复核确认**：摘要器确实被调用了 ✓，但新测试只验证「输入里有旧输出」✗，没有验证**摘要进入后续实际模型
输入** ✗，也没有验证**跨多次压缩保留中途约束** ✗；`summary` 记录的是估算前后尺寸 ✗，不是**摘要响应的
真实 usage** ✗；失败路径未覆盖 ✗。

**仍未做**：以上四项**全部未做** ✓。报告已分清主模型与摘要模型的计数 ✓（`record.sent` 与
`record.summaries` ✓），并记录了**摘要调用计入节点请求预算**这一发现 ✓。

### 7.7 本轮状态

| | |
| --- | --- |
| 本包测试 | **25 passed**（B1–B10 + R1–R5 + 判定器正反例）|
| 全量 `tests/` | **170 passed** |
| `ruff` / `mypy` | 通过 |
| 真实实验 | **未新增任何付费样本** ✓；三组已有产物已用修好的判定器重新判定 ✓ |

**第二包状态：仍不通过。** 第 1、2、4 条已修并回归；第 3、5、6 条**部分修复**，未完成项已在上面逐条
列出，不以「交主集成判断」结案。

---

## 8. 第二轮六条：处理完毕

### 8.1 第 1 条：判定器 —— 修好，三组已有产物已重新判定（零付费样本）

规则按**任务实际要求**重写 ✓：`artifacts()` 按各执行器自己的产物根解析 ✓；`tail` **逐字比对**清单末行 ✓；
`evidence` 按「每个引用块后跟来源」判定 ✓，并接受 `>` 与围栏两种引用写法 ✓、把连续 `>` 行算作同一个块 ✓。

**这个规则我改错了三次，每次都会造出一个不存在的「发现」**，所以它现在有**正反例测试**
（`test_the_experiment_judges_rules_against_their_own_examples` ✓，每个规则各一个满足与不满足的例子 ✓）：

| 三次错法 | 后果 |
| --- | --- |
| 排除 `runs/` 路径 | mini 全部被判失败 ✗——**已写进报告的错误结论** |
| 数「含 URL 的行」 | 48 个正确引用被报成缺来源 ✗ |
| 每个 `>` 行算一个引用 | 11 个多行引用被报成缺 22 个来源 ✗ |

**三组重新判定后的全部结果**（真实窗口 2 组 + 紧窗口 1 组，共 30 次尝试）：

- **`constraint` 任务：两臂各 6/6 全部 kept** ✓——**没有差异** ✓
- **`tail` 任务：两臂全部 kept** ✓
- **`evidence` 任务：只有 2 次 ABSENT，都是同一个候选运行 `pydantic-evidence-2`** ✓（其中一次是
  `budget_exhausted` 没跑完 ✓；另一次有文件但未通过规则 ✓——**我没有判定它是真失败还是第三种引用写法** ✗，
  如实标为未裁定 ✓）

**因此修正后的结论：在两种窗口下，两臂在约束保留上没有可测量的差异；唯一稳定的差异是候选在较重的
`evidence` 任务上更常耗尽请求预算。** ✓

### 8.2 第 2 条：内存与双流限额 —— 已修

- **内存有界**：沙箱改成**输出写文件、只读前 N 字节** ✓，不再 `PIPE` 全收 ✓。**这条测试当场抓到一个残留
  泄漏**：`_keep` 算 sha256 时 `handle.read()` 把整个文件读进内存 ✗——300 MB 的输出让进程涨了 **281 MB** ✗。
  改成分块哈希后回落 ✓。测试量的是**进程峰值内存增量**，不是代码里写了什么 ✓。
- **双流共用一份递减额度** ✓，按实际写入计 ✓。
- **不完整要说出来**：`_decode` 现在分三种情形措辞 ✓——没裁 ✓／余下在某处可读 ✓／**余下被裁且没能完整
  保留，这就是全部** ✓；`_bounded` 同样 ✓；`skipped` 也清 `visible` ✓。测试断言的是**模型收到的那段文本**
  里没有「the whole output is at」✓。

### 8.3 第 3 条：发送计数 —— 已修

`sent` 移到 **ceiling 检查之后** ✓（被拒的请求从没发出去，记录里不该有 ✓）；拒绝成为**独立事件** `refused` ✓；
**overflow 重试记为 `attempt=2` 的第二次 `sent`** ✓，并**再查一遍 ceiling** ✓。

### 8.4 第 4 条：请求开销读真实值 —— 已修

开销**从请求本身量** ✓：`model_request_parameters.instruction_parts`（真实指令）+ 工具 schema ✓，不再用固定
4,000 ✓。测试是**大指令反例** ✓：两段长度差很多的指令必须得出不同的开销 ✓。

### 8.5 第 5 条：摘要验证 —— 已修

- **摘要进入后续真实输入** ✓（在后续请求里找摘要内容 ✓）
- **中途约束跨多次压缩保留** ✓（第一次说出的约束，断言在**后续**请求里仍在 ✓——只测第一条用户消息是
  不够的，它被 `preserve_first_user_message` 保着 ✓）
- **失败路径留痕** ✓（摘要器抛错时记录里有 `model_error` ✓）

**仍未做**：`summary` 的 usage 字段已加到记录 ✓，但断言只检查字段存在 ✓，未验证与 provider 实际计数一致 ✗。

### 8.6 第 6 条：其余缺口

- **mini 的模型配置** 来自 CLI ✓（`model_spec["ref"]` ✓），不再硬编码 ✓
- **mini 的调用数** 从**它自己的 trace** 读 ✓（`role == "assistant"` 的行数 ✓），不再是 0 ✓
- **两臂预算**：请求上限相同（各 40）✓；mini 侧没有窗口 ✓——**这个不对称是固有的，不是等价对照** ✗，
  已在报告写明 ✓

### 8.7 最终状态

| | |
| --- | --- |
| 本包测试 | **35 passed**（B1–B10 + R1–R5 + 判定器规则正反例）|
| 全量 `tests/` | **180 passed** |
| `ruff` / `mypy` | 通过 |
| 真实实验 | 本轮**未新增任何付费样本** ✓；三组已有产物全部重新判定 ✓ |
| 共享层 patch | `sandbox.py` / `execenv.py` **待主集成复核** ✓ |
