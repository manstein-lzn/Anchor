# Model call recording and replay

**状态：已立项（ADR-044）。步骤 1 已完成并验证；步骤 2–3 未做；步骤 4 另行决定。**

本文回答一个问题：上下文管理是 Anchor 产品的核心，但今天还无法开发它。
本文说明为什么，以及第一步该做什么。

---

## 1. 为什么第一步不是上下文管理

上下文管理是**策略**：哪些内容进上下文、怎么压缩、怎么召回、每个 agent 用哪一套。

策略只能靠**对比**判断好坏。对比需要**变量受控**。而今天：

```
一个完整 campaign = 31 分钟 / ¥2.21 / 完全非确定
```

模型自己决定检索什么、读哪篇、综合什么、评什么。于是：

> 你改了上下文策略 → 结果变好了 → **你分不清是策略起作用，还是模型这次发挥好。**

**没有受控对比，你测不出自己的改进。** 这才是当前真正的障碍——不是平台不稳，
不是 workspace 没接上，不是缺哪个功能。

而且这件事你已经声明过了：`PRODUCT_VISION.md` 的 **I9** 要求
**R0（事件史取证重放）** 与 **R1（打桩模型结果的确定性模拟）**，并明确
"Only R0 and R1 are guarantees"。**它们至今未实现。**

---

## 2. 现状核实

| 能力 | 状态 | 证据 |
|---|---|---|
| 工具结果的幂等重放 | ✅ 已有 | `agent_tools.py`：重试的 claim 重放已持久化的结果，不重新执行 |
| 模型调用的**录制** | 🔴 无 | 全仓无任何 `record`/`replay` 模型调用的代码 |
| 模型调用的**回放** | 🔴 无 | 同上 |
| `model.usage` 事件 | 🟡 只有计数 | `worker.py:305`：node/attempt/model/tokens/cost/response_id，**无 prompt、无响应正文** |
| 单独运行一个节点 | 🔴 无 | `client.run_nodes` 是只读 |

**所以：工具层已经可重放，模型层完全不可观测。** 而上下文管理要动的正是模型层。

---

## 3. 接缝：包装 `Model`，不是包装 gateway

这是一个容易选错的地方，且选错会让整个设计失效。

### 为什么不能包 gateway

`ModelGateway` 看起来是自然的接缝——它已经是 `Protocol`，且 `ModelResponse` 是冻结
dataclass。但看 `generate_with_tools`：

```python
agent = PydanticAgent(self._model, ...)
...
return await self._run_agent(agent, prompt=prompt, system_prompt=system_prompt)
```

**PydanticAI 在 `agent.run()` 内部跑完了整个工具循环。** 所以一次
`generate_with_tools()` 调用 = **一整个 agent 轮次**（内含 N 次模型调用 + N 次工具执行），
不是一次模型调用。

在 gateway 层录制，你会得到"最终答案"这一个点，而**丢失工具循环内部那个不断增长的
上下文**——恰好是上下文管理最需要观察的东西。

### 应该包 `Model`

```python
from pydantic_ai.models.wrapper import WrapperModel
```

`WrapperModel` 是 PydanticAI 官方的包装接缝，**并且 `durable_exec` 自己就是这么做的**
（`pydantic_ai/durable_exec/dbos/_model.py` → `class DBOSModel(WrapperModel)`，
以及 `TemporalModel`、`PrefectModel`）。

包在 `Model` 层意味着：

- 看见**每一次**真实模型调用，**包括工具循环内部的每一次**
- 拿到的是真正的 messages 序列——就是上下文管理的输入
- 走的是库自己支持的扩展点，不是 hack

---

## 4. 设计

### 4.1 录制

```python
class RecordingModel(WrapperModel):
    """把每次模型调用写成不可变、内容寻址的投影。"""
```

每次调用产出一个工件：

```json
{
  "model": "deepseek-flash",
  "provider": "deepseek",
  "messages": [ ... ],            // 真实的 messages 序列
  "settings": { "max_tokens": 32768 },
  "tool_names": [ ... ],          // 只记名字与描述，callable 不可序列化
  "response": { "parts": [ ... ], "usage": { ... } },
  "digest": "sha256:…"            // 请求的规范化哈希
}
```

并由一个 `model.call` 事件引用它：

```
model.call   node_id, attempt, sequence, request_digest, response_ref, usage
```

**为什么这是投影而不是状态（重要）：**

`PRODUCT_VISION.md` I2 明确写：*"caches, memory, vector indexes and summaries are
rebuildable or discardable projections and must never independently determine
recovery."*

**录制的模型调用属于这一类。** 它是一份证据，用来观察和对比；它**绝不参与恢复**。
删掉它，恢复语义不变。这条必须写进 ADR，否则录制迟早会被当成"模型说了什么"的权威。

### 4.2 回放

```python
class ReplayModel(WrapperModel):
    """按位置消费录制过的响应；序列不一致时显式失败。"""
```

**键必须是结构位置 `(node_id, attempt, sequence)`，不能是 prompt 哈希。**

理由：你的目的是**改变 prompt**。如果按 prompt 哈希匹配，一改策略就必然 miss，
回放彻底无意义。

**分歧必须显式，不能静默继续。** 当回放序列与录制不一致时（调用次数变了、
顺序变了），回放**停止并定位到具体位置**：

```
ReplayDivergence: node=gather attempt=1 expected call #7, got #9
  recorded request digest sha256:a1b2…, actual sha256:c3d4…
```

静默继续会让"回放成功"变成一个假信号——而假信号比没有信号更糟。

### 4.3 三种模式

| 模式 | 行为 | 用途 |
|---|---|---|
| `off` | 不录制、不回放（**默认**） | 生产；零开销 |
| `record` | 调用真实模型，录制 | 建立基准、复现一次具体运行 |
| `replay` | 提供录制响应，分歧即失败 | 确定性复现、回归测试 |

---

## 5. 诚实说清：回放能做什么、不能做什么

这是本文最需要你判断的一节。

### 能做

1. **精确复现一次运行** —— 排查"为什么那次写出了审计文档"时有确定答案
2. **检视模型实际看到了什么** —— 今天你无法回答"它到底收到了什么上下文"，
   只能重建。这是上下文工作**最缺的诊断能力**。
3. **回归测试 harness** —— 改了非上下文的部分，确认模型行为未被扰动
4. **把一次运行变成可归档的证据** —— 与工件、事件一起构成完整档案

### 不能做

**回放不能用来 A/B 一个会改变 prompt 的上下文策略。**

策略改了 → prompt 变了 → 模型必须被**真实重新调用**，改变才有意义。回放提供的是
同一个旧响应，测不出任何东西。

这一点必须写清楚，否则会有人（包括我）误以为"有回放就能做上下文实验了"。

### 所以另一半是什么

**低成本迭代需要的是"节点级测试台"，而不是回放引擎。**

```
今天：改策略 → 跑完整 campaign → 31 分钟 / ¥2.21 → 结果无法归因
目标：改策略 → 跑一个节点（冻结输入快照）→ 分钟级 → 可对比
```

这需要两件事：

1. **录制的输入快照** —— 由本方案的录制提供
2. **单独运行一个节点的能力** —— **今天没有**（`run_nodes` 是只读）

**所以本方案是节点级测试台的前置**，但它本身不等于测试台。这一点我建议在计划里
明确，不要让"录制+回放"假装解决了迭代成本问题。

---

## 6. 体积与保留

用上一次真实 campaign 的用量估算：

```
gross input tokens  2,011,654
output tokens         214,368
原始文本估算          8.9 MB
去重后上限（前缀命中 75%） 2.9 MB
```

对照当前工件库 **212 MB** —— **一次 campaign 的完整录制约占 1.4%。**

- **内容寻址天然去重**：工具循环每次重发的绝大部分是重复前缀（实测缓存命中 75%），
  录制同样受益
- **可淘汰**：作为投影，它是保留策略**第一个**该淘汰的对象
- 复用现有存储预算机制，不新增一套

---

## 7. 风险与禁止事项

| # | 风险 | 禁止方式 |
|---|---|---|
| 1 | 分歧被静默吞掉，"回放成功"成假信号 | 构造上失败关闭，不支持"跳过" |
| 2 | 录制被当成权威状态，参与恢复 | ADR 明确：投影，永不参与 I2 闭包 |
| 3 | secret 泄进录制 | 包装层只见 messages，永不接触 api_key；落盘前做一次失败关闭扫描 |
| 4 | 并行工具调用的顺序不稳定 | 记录**派发顺序**；attempt 内的 sequence 即匹配依据 |
| 5 | 流式响应 | 记录组装后的结果；回放返回非流式，行为一致 |
| 6 | 生产环境开销 | 默认 `off`；开启需显式设置 |
| 7 | 录制被当成评估框架 | 明确非目标，见下 |

---

## 8. 非目标

- **不是 R2/R3**（同版本重跑 / 当前模型重跑）。I9 已明确它们
  *"recorded and diffed, never claimed identical"*，是记录与对比，不是保证
- **不是评估框架**。`pydantic-evals` 已是可选依赖，`eval_semantics.py` /
  `eval_verifiers.py` 已存在，那是另一条线
- **不解决"什么算好"**。学术图的经验是：质量标准写不成 predicate，
  最终是"确定性检查 AND 评审判断"的混合体。录制不改变这一点
- **不改动恢复语义**。租赁、检查点、事件流一律不动

---

## 9. 验收标准

```text
1. 一次 campaign 可回放，节点输出与终态完全一致
2. 分歧的回放以定位到具体调用的错误失败，绝不静默成功
3. 任意节点的实际 prompt 可以按文本读回
4. 录制一次 campaign 对工件库的增长 <5%
5. 录制关闭时无可测量的开销
6. 录制中不含任何 secret（失败关闭扫描通过）
```

第 3 条是给上下文工作用的——**它是本方案最直接的价值**。

### 当前状态

| # | 标准 | 结果 |
|---|---|---|
| **1** | 一次 campaign 可回放，节点输出与终态一致 | 🟡 单节点端到端已验证（真实 worker + 真实 lease + 真实模型）；整轮回放未跑 |
| **2** | 分歧以定位到具体调用的错误失败，绝不静默成功 | ✅ **真实模型验证**：越界调用报 `node=plan attempt=0 sequence=1` |
| **3** | 任意节点的实际 prompt 可以按文本读回 | ✅ **真实 `deepseek-flash` 调用验证**：instructions + user prompt 完整读回 |
| **4** | 录制一次 campaign 对工件库的增长 <5% | 🟡 估算约 1.4%；保留策略未接 |
| **5** | 录制关闭时无可测量的开销 | ✅ 默认 `off`，不安装任何 wrapper |
| **6** | 录制中不含 secret | ✅ 拒绝落盘，且**不让 run 失败**（I2） |

---

## 10. 实施顺序

```text
步骤 1  RecordingModel + 工件格式 + model.call 事件          ✅ 完成
步骤 2  ReplayModel + 显式分歧                              ⬜ 未做
步骤 3  电闸与配置（off / record / replay）+ 保留策略接入      🟡 配置已有；保留策略未接
步骤 4  （独立评估）节点级测试台                              ⬜ 未做
```

### 步骤 1 的落点

| 文件 | 内容 |
|---|---|
| `runtime/model_recording.py` | `RecordingModel(WrapperModel)`、`ModelRecorder`、`CallContext` + `bind_call`/`unbind_call`、`read_recording`、`text_of` |
| `runtime/model_gateway.py` | 录制开启时包 `Model`；把解析出的 secret 作为 forbidden 交给 recorder |
| `runtime/worker.py` | 在节点执行的 `try/finally` 上绑定/解绑调用上下文 |
| `runtime/worker_service.py`、`verifier_service.py` | 构造 recorder 并传给 gateway 工厂 |
| `runtime/settings.py` | `ANCHOR_MODEL_RECORDING`，默认 `off` |
| `tests/test_model_recording.py` | 13 个测试，含真实 worker 端到端与真实模型验证 |

### 步骤 1 验证到的三件事

1. **真实模型**：`deepseek-flash` 一次实调用，录制 1 条、`text_of` 完整读回 instructions + user prompt（验收 3）。
2. **instructions 必须单独记**：PydanticAI 把 `instructions` 放在 `ModelRequestParameters.instruction_parts`，不进入 `messages`；只记 messages 会静默丢掉 agent 最大的那块文本。
3. **digest 必须剔除易变字段**：消息 part 带墙上时钟 `timestamp`，直接哈希会让每次调用都不同、digest 失去意义。

### 步骤 1–2 未做的事（诚实记录）

- **保留策略未接入**：录制作为投影应当被优先淘汰，但现在没有接入 `retention`。
- **`model.call` 事件参与恢复吗？** 不参与，但也没有任何机制**阻止**它被读成权威
  ——那是一条纪律，不是一条门禁。若要变成门禁，需要一个类似 `test_architecture.py`
  的断言。

### 一条必须知道的限制：回放复现的是「已有 run 的尝试」，不是「重跑图」

回放键包含 `node_run_id`，而**新 run 会重新生成它**。所以：

| 场景 | 回放能否服务 |
|---|---|
| 恢复 / 重试 / 取证重执行**同一个 run 的同一个节点尝试** | ✅ 能 |
| **新准入**一个同一张图的 run | ❌ 不能——位置对不上，直接报分歧 |

**这是刻意选择。** 另一种做法是按 `(node_id, 第几次出现, 调用序号)` 匹配以跨 run 生效，
但那会在控制流发生变化时**静默地把一次调用配到错误的录制答案上**——正是本机制要消灭的
失效模式。宁可报错，不要默默配对。

两个后果：

1. **「重跑同一张图得到同样结果」不靠回放，靠图本身的确定性。** 回放不提供这个保证。
2. **这与 ADR-044 的边界一致**：回放不能 A/B 一个会改变 prompt 的上下文策略。
   面向"换策略再看效果"的工具是**节点级测试台**（步骤 4）。

该行为已由测试固定（`test_a_different_run_does_not_match_an_old_recording`），不是隐式行为。

### 长驻进程的记账是有上限的

worker 是长驻进程，一次 campaign 约一千个节点尝试。若按 attempt 记账且永不释放，
内存会线性增长到几十 MB——对"承诺跑几周"的服务是真实缺陷。现在：

| 结构 | 上限 | 验证 |
|---|---|---|
| `ModelRecorder._sequences` | 4096 attempt | 10 万次节点执行后恒定约 1 MB（修复前约 17 MB 且持续增长） |
| `ModelRecorder.written` | 4096 ref | 同上，保留最近条目 |
| `ReplayPlan._by_run` / `_loaded` | 64 run | 淘汰最旧 |
| `ReplayModel._positions` | 4096 attempt | 同上 |

淘汰是安全的：一个早已停止发起调用的 attempt，其计数器被丢掉不会被误解。

## 11. 需要你决定的三件事

1. **默认开还是关？**
   我建议 `off`（生产）／`record`（开发）。理由是投影不该有生产开销。

2. **录制放哪？**
   我建议复用工件库（内容寻址、去重、保留策略全都现成）。
   备选是独立的 trace 存储——好处是与业务工件隔离，代价是多一套存储要运维。

3. **步骤 4（节点级测试台）现在做，还是先做 1–3 看效果？**
   我倾向先做 1–3，因为步骤 4 的设计依赖步骤 1 的工件格式定型。
   但如果你认为迭代成本是眼下的痛点，可以合并评估。

---

## 附：ADR-044（已采纳）

本方案的核心决定已写入 `DECISIONS.md` 的 **ADR-044：模型调用作为投影被录制，回放按位置匹配并显式报告分歧**。要点：

1. **录制是投影，不是状态**（I2）。它是证据，可淘汰，**永不参与恢复**。
2. **回放按结构位置匹配**（node, attempt, sequence），不按 prompt 哈希——
   因为目的正是改变 prompt。按哈希匹配会让回放对任何策略变更必然失效。
3. **分歧显式失败**，定位到具体调用。静默继续会让"回放成功"变成假信号。

包装层在 PydanticAI 的 `Model`，而非 Anchor 的 `ModelGateway`，理由见上文第 3 节。
