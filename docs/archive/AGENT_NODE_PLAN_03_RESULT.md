# 第三包实施结果：步骤持久化与崩溃边界验证

> **历史归档（2026-09-25）**：保留当时的讨论、计划与验证证据，不代表当前实现或待办。
> 文中的“下一步”“待实施”“必须”及旧阅读顺序仅适用于当时阶段，不作为后续开发指令。
> 当前入口见 [项目 README](../../README.md)，唯一升级方向见 [Plugin 设计](../plugins.md)。

状态：实施完成，可审阅。**结论不等于迁移决策**；G2 的组合验收由主集成负责。

## 0. 最终实现摘要

**第三包问的是**：Harness 的 `StepPersistence` 能记录什么、重启后能可靠判定什么、以及 Anchor 最少还
需要多少恢复代码 ✓。**不是 exactly-once** ✓，不做任意 shell 的副作用推断 ✓，不做自动对账 ✓。

### 一句话结论

**框架在 settle 之后给的东西足够可靠；在 settle 之前，它诚实地什么都不知道，而本包把这份「不知道」
变成了一等结果。** 五个 kill 窗口里，**两个是安全问题（必须报 uncertain）、一个是可用恢复（可继续）、
两个是边界本身不存在或够不到** ✓——这正是计划允许并期待的那类结论 ✓。

### 四个判定

| 判定 | 含义 | 何时给出 |
| --- | --- | --- |
| `replayable` | 模型要过东西，但**没有任何工具调用开始过** ✓ | 账本里有 `model_request_completed`、没有 `tool_call_started` |
| `uncertain` | 有调用 `started` 而无终态 ✓——**可能发生了，不重放** ✓ | 计划里那张表的前两行、以及任何 failed ✓ |
| `continuable` | 全部终态 + 存在 `complete` 快照 ✓ | 可以从那里继续，不重做 ✓ |
| `invalid` | 引用不成立 ✓——**明确失败，不偷偷从头开始** ✓ | 截断/篡改/版本不符/store 里没有 ✓ |

### 参考格式与 store

```
anchor1.<base64url(JSON)>     {version, node, run, store, budget{requests_used, requests_allowed}}
```

- `node` 是**逻辑**节点执行 ID，`run` 是框架每次执行的 ID ✓——重试因此不会被记成上一次 ✓（§25 ✓）。
- 校验失败**按名字拒绝**，不做修补 ✓：被强行修好的 token 会指向调用方没打算恢复的那次运行 ✓。
- store 是 `FileStepStore`，落在**控制目录**里、节点工作区之外 ✓（§17 ✓——能改自己副账本的节点可以让
  账本说什么都没发生 ✓）。

### 实测（全部真实 kill，不是 sleep 猜的）

```
本包测试 16 passed   ·   全量 tests/ 212 passed，无 skip   ·   ruff / mypy 通过（26 个源文件）
```

单命令跑全部窗口：

```bash
.venv/bin/python scripts/recovery_windows.py                     # C1–C9
.venv/bin/python scripts/recovery_windows.py --json evidence.json
.venv/bin/python -m pytest tests/test_node_recovery.py -q
```

---

## 1. C1–C10 逐项结果

注入方式：真实 `bubblewrap` ✓、真实 CLI ✓、确定性模型 ✓（**无 provider 调用、无凭证、无外部副作用** ✓）。
每次试验是**真进程**在窗口处被 `SIGKILL` ✓（`README` 的「不能用重新实例化对象代替」✓）。

**同步屏障**：节点到达窗口 → 写一个字节到管道 → 阻塞 ✓；父进程 `select` 那根管道，**读到字节才 kill** ✓
（§42：不靠 sleep 猜时刻 ✓）。`exit_code == -9` 与 barrier 文本一起进证据 ✓——**这两条同时成立才说明
杀对了地方** ✓。

| ID | 窗口 | 计数器 | 判定 | 结果 |
| --- | --- | --- | --- | --- |
| **C1** | 模型响应后、工具循环前 | 0 → 0 | `uncertain` | ⚠️ **部分**——见 §2.1 |
| **C2** | `tool_call_started` 已持久化、命令未执行 | 0 → 0 | `uncertain`（1 个 started） | ✅ 与计划一致 |
| **C3** | 副作用完成、终态记录未写 | **0 → 1** | `uncertain` | ✅ 与计划一致 |
| **C4** | 已 settle 的循环之后、run 结束之前 | **0 → 1** | `continuable`（complete 快照） | ✅ 与计划一致 |
| **C5** | 终态记录之后、快照之前 | 0 → 1 | **窗口不存在** | ⚠️ **测得不存在**——见 §2.2 |
| **C6** | 同一恢复引用重复调用 | — | `no-repeat` | ✅ 历史不被覆盖、不重复确认 |
| **C7** | 检查点损坏 / 错引用 | — | `explicit` | ✅ 四种坏引用四种有解释的拒绝 |
| **C8** | 多次中断后的预算 | — | `carried`（6/8） | ✅ 重启不清零 |
| **C9** | shell 运行时宿主被杀 | 0 → 0 | `uncertain` | ✅ 附带答案见 §2.3 |
| **C10** | 01 回归 | — | ✅ | 即时提交、串行调用、真实图、mini 路径全过（含在 212 项里）|

### 关键证据（C3 与 C4）

```
=== C3 ===
  killed=True exit=-9 barrier='side effect done, terminal record not written'
  counter: 0 -> 1
  effect ...28409dd7 bash = completed
  effect ...126f52d7 bash = started          ← 第二个调用开始了、没有终态
  snapshot: snapshot step 1 (complete)
  verdict: uncertain — 不重放

=== C4 ===
  killed=True exit=-9 barrier='after the settled cycle, before the run ends'
  counter: 0 -> 1
  effect ...5cbef425 bash = completed
  snapshot: snapshot step 1 (complete)
  verdict: continuable
```

---

## 2. 三处必须如实说明的地方

### 2.1 C1 的边界在公开 API 上够不到

计划要 C1 = 「模型调用已持久化，工具 started 之前」✓——**这个状态在公开钩子上无法停住** ✓。实测：

```
after_model_request      tool_call_started=False  model_request_completed=False   ← C1 落在这里
before_tool_execute      tool_call_started=True   ...                             ← C2 落在这里
```

`tool_call_started` **写在** `before_tool_execute` **之前** ✓，而 `model_request_completed` **写在**
`after_model_request` **之后** ✓——所以「模型调用已持久化、工具还没开始」这个瞬间在两者之间，**没有任何
公开钩子在那里** ✓。

**结果**：C1 最近的落点只能报 `uncertain` ✓（**保守方向，不是错误方向** ✓），而不是计划期望的 `replayable` ✓。
按 §67，这是**如实交回**而不是伪造检查点 ✓。

### 2.2 C5 的窗口不存在（测出来的，不是推出来的）

C5 = 「效果的完成记录之后、完整结果/快照之前」✓。我在可达的钩子上探测：**有 `tool_call_completed` 的
同一瞬间，`complete` 快照已经在** ✓——

```
=== C5 ===
  killed=None barrier='C5@before the second tool call'
  events: ..., tool_call_started, tool_call_completed, model_request_started, ...
  snapshot: snapshot step 1 (complete)      ← 与终态记录同时存在
```

所以这个窗口**不存在** ✓（框架在同一个 boundary 里先写终态再存快照 ✓），不是我没找到 ✓。

### 2.3 C9：宿主死了，沙箱里的 shell 也停了

计划要求分开「主进程死了」和「命令没有继续」✓。实测：宿主在命令运行中被 `SIGKILL` 之后 ✓，
**`survived.log` 从未出现** ✓，且**零残留进程** ✓——

```
note: the shell did NOT continue after its host was killed;
      0 process(es) mentioning this window still running
```

在这个配置下（`bwrap --die-with-parent` ✓）两者等价 ✓，**但这是观察到的，不是假定的** ✓。而账本仍然只能
报 `uncertain` ✓——**账本分不出这两件事** ✓，这正是这个包存在的理由 ✓。

---

## 3. 交 G2 的五项

| # | 交接内容 |
| --- | --- |
| 1 | **快照何时取得**：每个工具调用全部返回的 `CallToolsNode` 之后（终态记录与快照在同一个 boundary，见 §2.2）；`after_run`；以及 run **失败**时保存当时的历史（`complete` 或 `interrupted`）|
| 2 | **历史如何恢复**：`continue_run(store, run_id=...)` 返回最新 **complete** 快照的 messages ✓；`interrupted` 默认跳过（它可能重放待定调用 ✓）；没有时 `LookupError` → 本包转成 `InvalidReference` ✓ |
| 3 | **ledger 与快照的非原子窗口**：**不存在**（§2.2 实测）✓——终态记录与快照同时可见 ✓。真正非原子的是「命令的副作用」与「它的终态记录」之间 ✓——那就是 C3 ✓ |
| 4 | **预算持久化口径**：Anchor 自己存 `{requests_used, requests_allowed}` 于控制目录 ✓，**框架不恢复 retry counter** ✓（其文档明说）✓。写入用 rename ✓，损坏的文件**不**当成满额 ✓。**摘要预算未纳入本包** ✓（§38 说留给 G2 与 02 合并验证）✓ |
| 5 | **输出引用保留要求**：本包依赖 02 的输出引用仍可读（重启后大输出要能用 bash 取回）✓；两者的清理约束由 G2 一起定 ✓ |

**合并时的已知风险**：02 的压缩会改写模型历史，而 `StepPersistence` 的快照保存的是**某个时刻的 messages**
✓——所以「压缩后重启，模型收到的是压缩后的有限上下文，而不是恢复成未压缩的全量历史」这条**必须由 G2 实测** ✓，
不能由两包各自的验收推断 ✓。

---

## 4. 用了哪些 API，还剩多少自研

**公开 API**（无私有 API ✓，§21）：

| 用途 | API |
| --- | --- |
| 步骤事件 / 效果账本 | `StepPersistence`、`store.list_events`、`store.list_unresolved_tool_effects` |
| 快照与继续 | `store.latest_snapshot`、`continue_run` |
| store | `FileStepStore`（原子写入 ✓） |
| 挂载点 | 01 冻结的 `run_node(..., capabilities=...)` ✓ |

**本包自研**（`src/anchor/node/recovery.py`，约 380 行）：

- 恢复引用的格式、编码与校验 ✓（§19 要求引用归 Node 所有 ✓）
- 四判定的**决策逻辑** ✓——框架给证据，不替调用方决定重放不重放 ✓（其文档明说「副作用去重是编排器的
  责任」✓）
- 预算的持久化 ✓（框架不提供 ✓）
- 故障注入的屏障与 kill ✓

**没有复制执行器，也没有第二套完成解析** ✓（§21 ✓）。

---

## 5. 未做到与不声称的

- **不声称 exactly-once** ✓。本包决定的是「什么是可知的」✓；要让重复命令无害，得由命令自己无害 ✓。
- **不声称生产级恢复** ✓。这是原型 + 故障验证 ✓。
- **`replayable` 在真实崩溃里可能够不到**（§2.1）✓——C1 的落点只能保守报 `uncertain` ✓。
- **C9 只测了 `bwrap --die-with-parent` 这一种配置** ✓——换沙箱或去掉该选项，结论要重测 ✓。
- **真实 provider 与真实费用**不在本包内 ✓（§38 ✓）。
- 摘要质量、压缩策略不是本包的事 ✓（03 不检验摘要内容 ✓）。

---

## 6. G2 的 A 门槛推翻的结论（2026-09-22 修正）

> **本节由 G2 补验写入。** 03 原文的两条结论被反例推翻，保留原文以便对照，并注明适用 commit。
> 修正依据：`AGENT_NODE_PLAN_G2.md` 的 A2；证据在 `AGENT_NODE_G2_RESULT.md`。

### 6.1 「C5 窗口不存在」是错的 —— 窗口存在，而且可达

**原文（§2.2）说**：在有 `tool_call_completed` 的同一瞬间 `complete` 快照已经在，所以那个窗口不存在 ✓。
**那是错的** ✗，错在**探测点取晚了** ✓：我探的是**第二轮**工具调用的 `before_tool_execute` ✓，那时第一轮的
`after_node_run` 早就把快照写完了 ✓。

**固定版本源码**（`pydantic_ai_harness/step_persistence/_capability.py`，0.32.0）的写入位置：

| 步骤 | 钩子 | 写入 |
| --- | --- | --- |
| 1 | `after_tool_execute`（第 622 行） | `_finish_tool_effect(...)` → `store.record_tool_effect(...)` + `tool_call_completed` 事件 |
| 2 | `after_node_run`（第 661 行） | `_save_snapshot(...)` → `store.save_snapshot(...)` |

**两个不同的钩子、两次独立写入** ✓——**不是**一个事务 ✓。真实 kill 落在两者之间：

```
C5  killed=True exit=-9 barrier='terminal effect record written, snapshot not yet'
    counter: 0 -> 2
    effect ...464146c4 bash = completed
    effect ...b43b4738 bash = completed
    snapshot: snapshot step 1 (complete)      ← 旧的 complete 快照存在
    verdict: uncertain — 1 settled tool call(s) are not covered by the newest complete snapshot
```

**怎么够到这个窗口**：把屏障 capability 注册在 `StepPersistence` **之前** ✓——钩子按注册顺序跑 ✓，
所以它的 `after_tool_execute` 落在框架的**之后** ✓。注册在之后则落在之前 ✓（两种都实测过 ✓）。

### 6.2 由此暴露的本模块缺陷（已修）

原文的 `assess` **只要有 complete 快照就返回 `continuable`** ✗——而那个快照可能是**上一轮**的 ✓，
从它继续会**重跑已经发生过的命令** ✗✓——正是计划 §36 点名的「旧快照掩盖之后的副作用」✓。

**修法**：快照必须**覆盖每一个终态 effect** ✓——逐个 `tool_call_id` 在快照历史里找得到 ✓。
上面那次 kill 因此报 `uncertain` ✓ 而不是 `continuable` ✓。

### 6.3 「C4 已证明可继续」需要收窄

原文 §1 的 C4 是 `continuable` ✓，但当时只检查了「存在 complete 快照」✗，**没有检查它覆盖了已完成的
effect** ✗。修好覆盖检查后 C4 **仍然**是 `continuable` ✓（那一轮的快照确实覆盖了它的 effect ✓），
但**结论的依据换了** ✓：现在是「快照含每个终态调用的结果」✓，不是「快照存在」✓。

### 6.4 仍然成立的部分

C2（`started` 无终态 → `uncertain` ✓）、C3（副作用已发生但无终态 → `uncertain` ✓）、C7（坏引用有解释地
拒绝 ✓）、C8（预算不因重启清零 ✓）、C9（沙箱不随宿主存活 ✓）**没有**被推翻 ✓，A 门槛下会再验一遍 ✓。

### 6.5 「C1 的边界够不到」同样是错的 —— 也是 capability 顺序造成的

**原文（§2.1）说**：`model_request_completed` 写在 `after_model_request` 之后，所以「模型调用已持久化、
工具还没开始」这个瞬间没有公开钩子 ✓。**那也是错的** ✗——测量的是**钩子顺序**，不是框架的能力 ✓。

**实测的规律**（本 build）：

| 钩子方向 | 顺序 |
| --- | --- |
| `before_*` | 按**注册顺序** |
| `after_*` | 按**逆序** |

所以把屏障 capability 注册在 `StepPersistence` **之前** ✓，它的 `after_model_request` 就跑在框架写
`model_request_completed` **之后** ✓✓——C1 于是给出 **`replayable`** ✓，正是计划期望的 K1 ✓：

```
C1  killed=True exit=-9  barrier='after_model_request, before the tool cycle'
    counter: 0 -> 0
    verdict: replayable — 没有工具调用开始过，请求可以发出一次
```

**两处「够不到」和一处「不存在」全部是同一个原因** ✗：03 当时把屏障注册在 `StepPersistence` **之后** ✓，
于是所有 `after_*` 钩子都落在框架写入**之前** ✓。这不是框架的边界问题，是我的接线问题 ✓。

### 6.6 修正后的窗口表

| 窗口 | 需要的注册位置 | 结果 |
| --- | --- | --- |
| C1 | 屏障在**前** | `replayable` ✓（K1 成立）|
| C2 | 屏障在**后** | `uncertain` ✓ |
| C3 | 无关 | `uncertain` ✓ |
| C4 | 屏障在**前** | `continuable` ✓ |
| C5 | 屏障在**前** | `uncertain` ✓（旧快照不覆盖）|
