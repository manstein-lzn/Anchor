# Anchor Continuity Architecture Review & Validation Brief

## 0. Purpose

你现在拿到这份文档，是为了让一个独立 Coding/Research Agent 基于当前 Anchor 文档，对 Anchor 的长期认知连续性架构做一次**聚焦、批判性的分析**。

这不是让 Agent 重新设计 Anchor，也不是让 Agent 立即实现功能。

你的首要任务是：

> 判断 Anchor 当前的架构是否真的能够解决 Long-Horizon Agent 在多轮 context compaction、进程重启和长期任务推进中的“认知漂移 / 降智 / 失忆”问题；如果不能，指出最小缺口，并给出可以通过实验验证的修改建议。

---

# 1. 当前问题定义

Anchor 针对的问题不是普通短任务能力。

短任务中，当前 Agent 的执行能力已经足够好。

真正的问题出现在：

```text
复杂、长期、方法不完全明确的任务
        ↓
context 持续增长
        ↓
context window 饱和
        ↓
多轮 compaction
        ↓
历史信息不断被压缩
        ↓
关键决策 / 失败原因 / 用户修正 / 长期约束丢失或变形
        ↓
Agent 开始重复工作、重新尝试已失败路径、违反旧约束
        ↓
出现“降智” / “认知漂移”
```

因此，核心问题不是：

> “怎样让 Agent 记住更多？”

而是：

> **怎样让长期任务中的 Agent，在上下文被反复压缩之后，仍然保持足以正确继续行动的认知状态。**

---

# 2. 当前 Anchor 的核心架构

当前 Anchor 已经形成了一个比较明确的认知状态机：

```text
Pi execution
    |
    v
Pi compaction preparation
    |
    v
Episode
    |
    v
Update Agent
(previous Checkpoint + Episode)
    |
    v
Candidate Cognition
+
Transition Certificate
    |
    v
Deterministic Validation
    |
    v
Immutable Checkpoint
    |
    v
Deterministic Context Projection
    |
    v
Next Agent Invocation
```

核心公式：

```text
S(t+1) = Update(S(t), Episode(t))

C(t) = Project(S(t)) + Pi Active Window
```

其中：

- `S` = Anchor authoritative cognition；
- `Episode` = Pi 已经选定、但尚未被当前 Checkpoint 吸收的工作；
- `Update` = 专门的、无工具的认知更新任务；
- `Project` = 从最新 Checkpoint 确定性地产生模型 Context；
- `C` = 模型看到的工作上下文；
- Context 永远不是 authoritative State。

---

# 3. 当前 Anchor 已经具备的关键设计

以下内容来自当前 Anchor 文档，不应重新发明：

## 3.1 Task

一个 Pi session 最多拥有一个 Anchor Task。

Task 保存用户确认的：

```text
goal
acceptance criteria
constraints
non-goals
risks
verification
confirmed execution plan
```

其中 Contract 是稳定约束。

Update 不得静默改变用户确认的 Contract。

---

## 3.2 Checkpoint

Checkpoint 是当前 authoritative cognition。

它包含：

```text
task identity
checkpoint version
parent version + content hash
source frontier
current cognition
transition certificate
provenance
```

最新合法 Checkpoint 是当前真相。

旧 Checkpoint 不进入普通 Context。

---

## 3.3 Episode

Episode 是上一个 Checkpoint 尚未吸收的 Pi 工作片段。

它严格依赖 Pi 的 compaction preparation：

```text
preparation.messagesToSummarize
+
preparation.turnPrefixMessages
```

Anchor 不重新读取完整 transcript/branch 来决定边界。

Tool call 与 tool result 必须作为不可拆分 replay unit。

---

## 3.4 Cognition

当前 Anchor cognition v3 使用：

```text
Situation
Experience
Intent
Knowledge Index
```

核心原则不是保存完整历史，而是保存：

> **如果被遗忘，会导致未来做错决策、违反约束、重复失败或丢失必要下一步的信息。**

也就是：

> Minimal sufficient cognition.

---

## 3.5 Attention levels

信息不是简单的 save/delete，而有三层：

```text
Active Cognition
    直接影响当前决策
    始终进入 projection

Dormant Knowledge
    当前不需要详细关注
    但未来可能有价值
    用 exact recovery reference 表示

Event Archive
    默认不参与当前 cognition
    仍可用于恢复、审计和调试
```

因此“忘记当前注意力”不等于“永久删除”。

---

## 3.6 Explicit forgetting

每个之前 active 的 cognition item 都必须有明确 disposition：

```text
carry
revise
resolve
supersede
demote
archive
```

不能静默消失。

例如：

```text
D17:
do not modify public API

后来用户明确允许 breaking change

D17 -> superseded
D42 -> current directive
```

---

## 3.7 Transition Certificate

每次 Update 必须给之前 active cognition item 一个 disposition。

Validator 用它保证：

- 没有 active item 静默消失；
- carry/revise/supersede 等关系完整；
- 新事实有合法来源；
- 失败不被错误表示成成功；
- demote 的对象有可恢复引用；
- parent / Task / frontier / provenance 正确。

注意：

Transition Certificate 是 validation/audit material，不进入普通 Context。

---

## 3.8 Context projection

普通 Active 模型请求接收：

```text
Pi fixed system / developer / tool prefix
+
immutable Task Contract core
+
latest Active Cognition
+
compact Knowledge Index
+
Pi compaction-aware active window
```

Projection：

- 不扫描完整 transcript；
- 不重新调用 LLM；
- 不拼接旧 Checkpoints；
- 不包含完整 persistence object；
- 不包含 transition history；
- 不包含所有旧 Artifact；
- 不把历史 summary chain 重新塞给模型。

---

## 3.9 Persistence

Checkpoint 是 immutable revision。

具备：

- parent hash；
- content hash；
- source frontier；
- version；
- provenance；
- stale-version CAS；
- content-bound receipt；
- compact crash-window replay；
- fail-closed mismatch recovery。

Anchor 与 Pi 的 compaction append 不宣称跨文件原子事务。

---

## 3.10 Recall

Recall 是一个明确、有限的能力：

```text
reference
  ↓
exact locator
  ↓
content/hash verification
  ↓
bounded recovery
  ↓
current work window
  ↓
later Update may promote it again
```

Recall 不会因为“被看过一次”就永久重新进入 Active cognition。

---

# 4. 一个非常重要的架构判断

不要把 Anchor 重新做成：

```text
Agent Memory
```

也不要重新做成：

```text
Universal Agent Runtime
```

目前更准确的定义是：

> **Anchor 是 Agent Runtime 和模型 Context 之间的 authoritative cognitive state layer。**

职责分工：

```text
Pi
→ 发生了什么
→ tools
→ transcript
→ active window
→ compaction boundary
→ execution

Anchor
→ 当前任务什么是真的
→ 哪些认知必须继续保持
→ 哪些信息可以降低注意力
→ compaction 后如何恢复 cognition
→ 哪些状态变化经过验证

LLM
→ 基于当前 cognition 选择下一步行动
```

---

# 5. 现在真正需要验证的，不是“有没有 memory”

当前 Anchor 的实现已经跨过了“设计概念是否存在”的阶段。

下一阶段核心应该是：

> **证明 minimal sufficient cognition 真的能支撑长期 continuation。**

重点不是继续扩展 schema。

重点是做实验。

---

# 6. 第一核心实验：Fresh-Agent Takeover

这是目前最重要的验证。

设计：

```text
Agent A
    |
    | long-running task
    | multiple compactions
    | user correction
    | failures
    | decisions
    |
Checkpoint Cn
    |
    v
Fresh Agent B
```

Agent B 不得到完整历史，只得到：

```text
Task Contract
+
Projected Cognition
+
Knowledge Index
+
Pi recent active window
```

然后问：

> “继续执行这个任务，你下一步应该做什么？”

比较：

```text
Agent A with full historical context
vs
Agent B with Anchor projection
```

重点不是文字相似度。

重点是：

> **二者是否能选择一致且正确的下一步。**

---

# 7. 建议定义 Continuation Quality

不能只测：

```text
state similarity
```

应该测：

```text
Continuation Quality
```

至少覆盖：

### Goal continuity

Agent 是否仍知道：

- 长期目标；
- acceptance criteria；
- non-goals。

### Constraint continuity

Agent 是否仍遵守：

- hard constraints；
- user corrections；
- explicit prohibitions。

### Decision continuity

Agent 是否仍理解：

- 关键决策；
- 为什么这么决定；
- 什么情况下可以重新考虑。

### Failure continuity

Agent 是否避免：

- 重复失败路径；
- 没有新证据的 retry；
- 已被否定的 hypothesis。

### Intent continuity

Agent 是否仍知道：

- 当前 user directive；
- accepted next action；
- current blocker；
- next concrete step。

### Evidence continuity

Agent 是否知道：

- 哪些是 confirmed fact；
- 哪些只是 hypothesis；
- 证据在哪里。

---

# 8. 第二核心实验：Cognitive Drift

人为构造容易漂移的任务。

例如：

```text
Goal
    implement migration

Constraint
    backward compatibility required

Decision
    do not modify public API

Failure
    approach X fails because condition Y

Correction
    user later changes one constraint

Long irrelevant context
    many pages of noise

Compactions
    10–20 times
```

然后观察 fresh Agent 是否会出现：

```text
Goal drift
Constraint drift
Decision drift
Failure-memory drift
Directive drift
```

尤其关注：

```text
“这个路径以前已经失败了”
```

是否还能约束未来行为。

---

# 9. 第三核心实验：Repeated Failure Prevention

这是最容易被用户真实感知的“降智”。

例如：

```text
Path A
    ↓
failed
    ↓
reason = missing condition X

later:
Agent considers Path A again
```

正确行为：

```text
do not retry until evidence X exists
```

错误行为：

```text
Maybe try Path A again
```

这类 test 对长期连续性比测试 summary quality 更有价值。

---

# 10. 第四核心实验：Directive Supersession

例如：

```text
t1:
do not use API v1

t2:
...
compact × many

t3:
user:
API v1 is now allowed
```

正确 cognition：

```text
old directive
    ↓
superseded

new directive
    ↓
active
```

fresh Agent 应理解：

> 不是“API v1 从来都不允许”，而是“此前不允许，现在用户已经明确修改”。

---

# 11. 第五核心实验：Over-forgetting

Anchor 强调 minimal sufficient cognition。

因此风险不只有“记得太少”。

另一个风险是：

> **为了让 state 很小，把实际上仍然影响未来行为的信息错误 demote/archive。**

因此必须有 adversarial cases：

```text
关键约束非常早出现
↓
之后出现大量无关信息
↓
Update × N
↓
fresh Agent takeover
```

检查：

> 如果信息被 demote，fresh Agent 还能否知道在哪里精确恢复？

如果不知道：

> over-forgetting。

---

# 12. 第六核心实验：50–100 Update Churn

在任务复杂度大致稳定的情况下：

```text
Update 1
Update 2
...
Update 100
```

记录：

```text
projection size
cognition size
checkpoint size
takeover quality
recall quality
```

期望：

```text
Checkpoint count ↑↑↑

Projected Context
──────────────────────→ plateau

Cognition
──────────────────────→ plateau

Continuation Quality
──────────────────────→ stable
```

不能用：

```text
maxFacts
slice(0, N)
固定 token quota
```

强行制造 plateau。

正确的 plateau 必须来自：

> semantic forgetting + demotion + exact recovery + minimal sufficient cognition。

---

# 13. 当前最值得警惕的架构风险

## Risk 1 — State 重新变成第二个 Transcript

随着字段增加：

```text
facts
decisions
hypotheses
milestones
artifacts
references
history
evaluations
...
```

容易变成“完整历史账本”。

判断标准不是字段多少。

而是：

> 这个 cognition item 是否还会改变未来行为？

---

## Risk 2 — Belief State 成为第二套 State

不要建立：

```text
Canonical State
+
Agent Belief State
```

两个并列权威。

Agent 报告“我现在相信什么”只能用于：

- takeover evaluation；
- drift diagnostics；
- 分歧调查。

它不能替代 authoritative Checkpoint。

---

## Risk 3 — 每个事件都启动 LLM Capture

不建议：

```text
每次 tool call
→ LLM capture
```

这会造成：

- 成本；
- latency；
- race；
- state churn；
- 多套状态竞争。

当前设计更合理：

```text
Pi decides compaction boundary
→ one Episode
→ one serial Update
```

只有真实数据证明这种模式不足，才考虑主动 capture。

---

## Risk 4 — Arbitrary State Budget

不要为了稳定 state size 引入：

```text
maxFacts = 100
maxTokens = X
```

状态大小应该由：

> 当前任务复杂度 + 最小充分认知

决定。

---

## Risk 5 — Recall 重新把历史污染回来

Recall 必须保持：

```text
explicit
targeted
bounded
recoverable
```

不能变成：

```text
Agent remembers something
→ dump 5000 tokens
→ context grows again
```

---

# 14. 建议暂时不要做的东西

除非实验明确证明需要，不要增加：

```text
vector memory
knowledge graph
scheduler
agent pool
multi-agent governance
reviewer authority
workspace sandbox
automatic per-turn projection LLM
asynchronous Update
parallel state writers
private Anchor token budgets
```

目前 Anchor 的产品边界已经明确排除这些能力。

---

# 15. Learning-oriented instrumentation 应该放在哪里？

这是前面讨论中一个重要方向，但目前不应该成为 Anchor 核心 runtime。

合理演进路线：

```text
Stage 1
Cognitive Continuity
    ↓
Stage 2
Continuity Evaluation
    ↓
Stage 3
Attribution / Instrumentation
    ↓
Stage 4
Harness Optimization
    ↓
Stage 5
Harness RSI
```

也就是说：

> 先证明 Agent 能长期保持认知连续性，再利用这些稳定的 Checkpoint / Episode / Projection 数据建立 learning substrate。

---

# 16. 一个建议的未来 Learning Substrate

未来可以把 Anchor execution 表示为：

```text
Task
  ↓
Checkpoint C0
  ↓
Episode
  ↓
Update
  ↓
Checkpoint C1
  ↓
Projection
  ↓
Agent behavior
  ↓
Outcome
```

从中提取：

```text
State
Decision
Action
Observation
Milestone
Outcome
Attribution
```

但这属于下一阶段。

现在不要因为“RSI 很有趣”而提前扩大核心 runtime。

---

# 17. 你需要输出的分析

请基于当前 Anchor 文档，输出一份 architecture review。

不要泛泛介绍。

必须明确回答：

## A. Anchor 是否已经解决了核心问题？

即：

> 多次 context compaction 后，Agent 是否仍然能够保持正确的长期任务认知？

分成：

```text
已经解决
部分解决
尚未证明
明显缺失
```

---

## B. Anchor 当前最大的技术风险是什么？

最多列 5 个。

每个风险说明：

```text
risk
why it matters
evidence from current architecture
how to test
minimal fix
```

---

## C. Fresh-Agent Takeover 应该怎样设计？

给出：

```text
test scenario
inputs
agent instructions
ground truth
scoring
failure taxonomy
```

重点是行为，不是文本相似度。

---

## D. 怎样定义 Cognitive Drift？

给出一个可操作的 taxonomy：

```text
goal drift
constraint drift
directive drift
decision drift
failure drift
evidence drift
next-action drift
```

并建议每一类怎么测试。

---

## E. Anchor 的 State Schema 是否已经接近“最小充分认知”？

不要因为字段少就说是。

请从：

```text
future behavioral value
recoverability
state growth
fresh-Agent takeover
```

四个角度审查。

---

## F. 是否应该修改架构？

只有在有明确证据的时候提出修改。

优先级：

```text
P0 = 核心 correctness
P1 = continuity quality
P2 = evaluation
P3 = performance
P4 = future learning substrate
```

不要为了“完整”而提出大量新 feature。

---

# 18. 推荐最终输出格式

请严格使用：

```text
# Executive Verdict

# 1. What Anchor Already Gets Right

# 2. What Is Actually Proven

# 3. What Is Not Yet Proven

# 4. Failure Modes / Risks

# 5. Fresh-Agent Takeover Design

# 6. Cognitive Drift Evaluation

# 7. Minimal-Sufficient-Cognition Review

# 8. Required Changes (P0/P1/P2)

# 9. Tests Before More Architecture

# 10. Final Recommendation
```

最后必须给出一个明确判断：

```text
KEEP
KEEP + MODIFY
REDESIGN
```

以及：

> 如果选择 KEEP + MODIFY，只允许提出最小必要修改，不要重新设计 Anchor。

---

# 19. 最后一个重要约束

不要假设：

- “LLM summary 很好”；
- “State schema 看起来合理”；
- “Checkpoint 能恢复所以 continuity 已解决”；
- “recall 存在所以 memory 已解决”。

所有结论都必须尽可能绑定：

```text
architecture invariant
implementation status
observable behavior
acceptance test
```

核心目标不是让文档更漂亮，而是回答：

> **Anchor 是否真的能让一个长期 Agent 在经历多次 compaction 后，仍然像“同一个 Agent”一样继续正确工作。**
