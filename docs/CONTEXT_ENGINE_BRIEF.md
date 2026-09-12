# Agent Graph Context Engine —— 架构讨论简报

**给外部讨论 Agent 的问题陈述。**

这份文件**不提出方案**。它描述一件事：我想要什么、我已经确知什么、以及我卡在哪。
你的价值在于帮我把问题定义准，而不是给我一个看起来合理的架构。

**如果你读完只想说"应该做一个分层记忆系统"——那说明这份文件没写清楚，请直接指出，我会重写。**

---

## 1. 你的任务

帮我把下面这个问题的**结构**搞清楚：

> **一张拓扑固定的 agent graph，在【单次 run】内连续工作几十小时、累积信息达到几千万 token。
> 我要的是它在这种尺度下仍然保持【连续性】——任务能一直正确地往下走，而不是越跑越偏。
> 我需要知道：每个 agent 的上下文和整张图的上下文该怎样管理，才能让它在理论上无限的时间轴上正常工作。**

具体想要你回答的六件事（§10 有完整清单），其中最核心的四件：

1. **我的问题定义准不准？** 我担心"上下文管理""连续性""漂移"这些词太含糊，让讨论漂到一个不解决问题的层面
2. **它是不是一个有解的问题？** 或者它依赖某个我还没意识到的前提（比如"任务的必需事实必须有界"）
3. **如果要求"用理论和单元测试验证，而不是跑一个几十小时的任务"，可验证的命题是什么？**
4. **我这个"无限时间轴"的框架本身对不对？** 也许真正的问题不是时间轴，而是别的什么

**不要**给我一个分层架构图。**要**指出我的定义哪里不准、我的前提哪里可能是错的。

---

## 2. 背景一：Anchor 是什么（当前项目）

一个 **agent graph 运行时**。与本次讨论相关的只有几个要点。

**一个图 = 节点 + 边。** 节点类型包括 `agent`（调用模型的节点）、`tool`、`verifier`（判定）、
`parallel`、`join`、`loop`、`approval`、`artifact` 等。

**一次 run = 把一张【已发布且不可变】的图，在一个具体输入上执行一遍。**

```
① 声明式输入
   一个节点只看到它的【入边声明】给它的东西。
   没有隐式全局共享——节点不能读工作区、不能读别的节点的中间状态。

② 累积在工件里，不在上下文里
   一次节点执行 = 组装一个 prompt + 调用模型 + 结束。
   下一次执行（哪怕是同一个节点的下一次）是【全新的 prompt】。
   累积的产物（证据、代码、结论）作为不可变工件存在外部。

③ 事件日志是唯一真相
   发生过什么 = 事件序列。任何摘要、记忆、投影都是【派生物】，不是真相。
   重试不重放历史。

④ 确定性验证 + 完成门
   节点的完成可以由一个确定性验证器判定（JMESPath 表达式），而不是由模型声明。
```

**换句话说：Anchor 目前的形状是"每个节点每次全新投影"。** 累积发生在工件里，不发生在上下文里。

---

## 3. 背景二：这个问题的**上一次尝试**（原始 Anchor）

**这一节必须在前一节之后立刻读。**

有一个**同名但不同**的早期项目，逐字节归档在本仓库的 `contextengine/`（附 `PROVENANCE.md`）。
它是 `anchor-pi`：一个 Pi 扩展（约 90 KB JavaScript）+ 一个纯标准库 Python 状态核心
（643 行），带 41 个测试。

**它是针对本文件 §1 那个问题的上一次尝试，不是一段引言。**

### 3.1 它对自己的定义（原文，我不转述）

> **一句话定义**
>
> Anchor 是一个 Pi Extension。它把 compact 从"压缩聊天记录"升级为"提交认知检查点"，
> 使 Agent 在多次压缩、进程重启和长时间工作后，仍能准确恢复任务目标、当前指令、
> 有效事实、约束、失败经验和下一步。
>
> Anchor 不替换 Pi，也不试图成为通用 Agent runtime。

> **它自己陈述的核心问题**
>
> 长期任务中的 transcript 同时混合了目标、临时推理、工具输出、错误、修正和决策。
> 随着历史增长，Agent **必须反复从事件中猜测"现在什么是真的"**。
> 普通 compact 主要解决长度问题，无法保证多次摘要后仍保留用户修正、失败原因和当前计划，
> 因此容易产生认知漂移。

> **它的定位**
>
> Anchor 是 Agent Runtime 与模型 Context 之间的 **authoritative cognitive state layer**。
> 明确**不是** Agent Memory，也**不是** Universal Agent Runtime。

### 3.2 它的运作方式（这是理解它的关键，而我之前的版本漏掉了）

```
Pi 会话运行，transcript 不断增长
        │
        │  Pi 到达 compact 边界（边界由 Pi 计算，不是 Anchor 决定）
        ▼
Anchor 的 Update —— 一个【无工具的专用认知任务】
        │
        │  产出：新的认知 + transition certificate
        ▼
校验：coverage / demotion / 引用不可变 / schema / version / provenance
        │
        │  通过才提交
        ▼
新的不可变 Checkpoint（存于 SQLite，是唯一权威）
```

**而每次模型调用的上下文，是一个固定公式（原文）：**

```
Pi fixed system / developer / tool prefix
+ latest authoritative Anchor Checkpoint
+ Pi compaction-aware active window
```

**关键性质**（原文）：*"Projection 是对权威 State 的确定性读取，不调用 LLM，
不扫描完整 EventLog，也不自行裁剪 Pi 的窗口。"*

**另一条关键性质**（原文）：*"Anchor does not resummarize or trim the active window on
ordinary model calls."* —— 投影是在 **compact 边界**重建的，**不是每次调用重组**。

### 3.3 它的四个核心对象（原文）

```
Task        一个 session 的长期目标身份：goal / acceptance criteria / constraints /
            non-goals / risks / verification / confirmed execution plan。
            一个 session 最多一个 Task，不因模型调用、compact 或 tree navigation 改变身份。

Checkpoint  权威的当前认知 = 【State revision + source frontier】
            State revision 含：current understanding / current directive / confirmed facts /
            active hypotheses / unresolved conflicts / decisions / failed paths 及原因 /
            blockers / open questions / next action / evidence references /
            directive history / parent version / provenance
            source frontier 表示这份认知已经吸收了哪段 Pi 历史
            （session identity + firstKeptEntryId + 被吸收输入的确定性 hash）

Episode     上一个 Checkpoint 尚未吸收、本次需要归约的工作片段。
            严格来自 Pi 已经算好的 preparation.messagesToSummarize + turnPrefixMessages。
            Anchor 不读取完整 branch 重新决定边界。

Projection  上面那个公式。确定性读取，不调 LLM，不扫描全量，不自行裁剪。
```

**它的一条核心不变量（原文）：**

> **Checkpoint 没有 source frontier 就不能取代 transcript 中的任何内容。**

**它的十条核心不变量**（原文，值得逐条读）：

```
1.  State 回答"现在什么是真的"；transcript 只回答"发生过什么"。
2.  Context 是 State 的投影，不是新的事实来源。
3.  Checkpoint 必须声明准确的 source frontier。
4.  frontier 之前的信息由 Checkpoint 承担，之后的信息由 Pi active window 承担。
5.  模型输出只是候选；通过 schema、identity、version、transition 和 provenance
    检查后才能提交。
6.  Task goal、current directive、accepted next action 和 directive history 相互独立。
7.  用户最新的非省略指令具有立即执行意义；"继续"优先解析为 accepted next action，
    再以此前 directive 为支持上下文，不能回退为原始 goal。
8.  Update 失败不得破坏旧 Checkpoint 或 Pi history。
9.  compact 记录缺失不得使已提交 Checkpoint 无法恢复。
10. Normal Pi 模式没有 Anchor prompt、model tools、State 文件或 Context 投影。
```

### 3.4 它已经实现的机制（它的 ROADMAP 原文，不是我推测的）

```
· serial Update at Pi's compact boundary with stale-version rejection
· SQLite reduced to Task and immutable Checkpoint truth
· anchor.cognition.v3  Situation / Experience / Intent / Knowledge Index
· stable cognition item IDs and provenance
· anchor.transition.v1  coverage and demotion validation
· exact checkpoint:<version>:item:<id> recall through immutable Checkpoints
· bounded Contract-plus-cognition Context projection
· request-local schema-constrained Bootstrap and Update submission
· exactly-one-call, closed-schema, malformed-submission, persistence-boundary tests
```

**把它对照"任何方案都必须满足的约束"（本文件 §8），它逐条回应了：**

| §8 的约束 | 原始 Anchor 的机制 |
| --- | --- |
| 调用前有界 | `bounded Contract-plus-cognition Context projection` |
| 结构性而非政策性 | 不可变 Checkpoint 是唯一权威，投影由它确定性导出 |
| 不静默丢弃 | `anchor.transition.v1` coverage + demotion validation（见 §3.5） |
| 前缀稳定 | 投影只在 compact 边界重建，不在每次调用重组 |
| 派生物不是真相 | 不变量 1、2：State 是权威，Context 是投影 |
| 无界部分可寻址 | `exact checkpoint:<version>:item:<id> recall` |

### 3.5 "不静默丢弃"在它那里是怎么变成可判定的

这是它的机制里最值得你审视的一处，因为它把一条模糊要求变成了**一张证书**：

```
transition_certificate: {
  schema: "anchor.transition.v1",
  dispositions: [                    ← previous 里的【每一项】都必须出现
    { item_id, disposition: "carry",   reason, sources },
    { item_id, disposition: "revise",  reason, sources, replacement_id },
    { item_id, disposition: "archive", reason, sources },
    { item_id, disposition: "demote",  reason, sources, reference },
  ]
}

强制拒绝（来自 test/cognition-v3.test.js 的断言）：
  · 漏掉 previous 里的任何一项        → /omits|coverage/
  · 处置一个不存在的项                → /unknown previous item/
  · demote 而不给 reference           → /reference/
  · reference 指向 transcript（可变） → /immutable Checkpoint/
```

**含义：某一项"自己消失了"在结构上不可能发生——必须显式处置它，而降级必须留下可取回的引用。
而降级留下的引用必须指向不可变的东西。**

### 3.6 它的验证状态：比“三个实验只跑了一个”更精确

**它的 ROADMAP 里有一份自评的阶段评估（原文）：**

```
✅ Phase 2（结构化 Bootstrap/Update 提交）已实现，确定性测试覆盖：
   closed schemas / mandatory tool choice / exactly-one-call validation /
   rejected submission 不写 State

✅ Phase 1（历史规模的真实 provider 溢出验证）已完成受控探针：
   两次历史规模 overflow compact（tokensBefore = 188,918 与 191,902），真实 provider
   → Bootstrap Checkpoint 0 与 Update Checkpoint 1 均已提交
   → Checkpoint 1 的 parent hash / frontier / event ID / task ID / version /
     content hash 全部匹配
   ⚠️ 但原话说：“该探针使用【合成 Episode 证据】，所以验证的是 Anchor/Pi/provider
      的边界，而不是某个具体业务任务的语义质量。”

⬜ 下一阶段 “minimal sufficient cognition”——要跑：
   correction / failure / conflict / recall / restart /
   fresh-Agent takeover / 【50-to-100-Update plateau acceptance】
```

**其中的 “50-to-100-Update plateau acceptance” 就是本文件 §4.2 的那个问题，而且它有名字。**

**所以准确的说法是：它的边界验证跑过了（真实 provider、历史规模），
而它的语义验收（包括那个 plateau 实验）从未跑。**

它另有两项“Remaining host acceptance”未做：把溢出探针留作回归记录；
测量 projection 与 Update 的延迟 p50/p95。

### 3.7 它**设计**的三个独立实验（在另一份文档里）

| # | 实验 | 状态 |
| --- | --- | --- |
| ① | **Fresh-Agent Takeover** —— 给定 Checkpoint，fresh agent 能否选出**同样且正确**的下一步 | 跑过一个变体，见 §3.8 |
| ② | **Cognitive Drift** —— 构造易漂移任务（见下），观察 fresh agent 是否漂移 | **从未跑** |
| ③ | **Repeated Failure Prevention** —— "这条路以前失败过"是否还能约束未来行为 | **从未跑** |

**它自己写着（原文）：**

> *"证明 minimal sufficient cognition 真的能支撑长期 continuation。
> **重点不是继续扩展 schema。重点是做实验。**"*

**实验②的设计原文（注意它不需要几十小时）：**

```
人为构造容易漂移的任务：
  Goal:        implement migration
  Constraint:  backward compatibility required
  Decision:    do not modify public API
  Failure:     approach X fails because condition Y
  Correction:  user later changes one constraint
  Long irrelevant context:  many pages of noise
  Compactions: 10–20 次

然后观察 fresh Agent 是否出现：
  Goal drift / Constraint drift / Decision drift / Failure-memory drift / Directive drift
```

它为这个实验定义了判据 `Continuation Quality`，六项：Goal / Constraint / Decision /
Failure / Intent / Evidence 连续性。

### 3.8 已经跑过的那一个：我之前的说法不准

我跑了实验①的一个变体（同一真实 run 的同一真实切点）：

```
条件（同一个全新 agent）：
  A 原始材料      141,033 字符
  S 结构化摘要      5,045 字符
  B 认知状态        4,762 字符     （S 与 B 由同一模型从同一材料生成）

判据一：七问考卷（目标/已确立/冲突/受阻/下一步…），盲评，每条件 3 次
  A 17.7   S 7.0   B 20.3
  → 同样尺寸下，认知状态比摘要强约 3 倍

判据二：让它真的写出论文，按交付物要求评判
  → 三个条件【都满足全部 5 项要求】
```

**我说过"认知状态没有独立行为收益"。那个说法把两件不同的事混在了一起：**

```
· 判据一【支持】原始 Anchor：状态比同尺寸摘要承载任务信息强 3 倍
· 判据二限制的是【判据】，不是【状态】：考卷答得好不预测活干得好
```

**而且实验①问的是"能否答出任务状态"，原始 Anchor 要的是"能否选出同样的下一步"。
这两者不是同一件事——所以即使按字面，实验①也没有被完整执行。**

### 3.9 它**没有**解决的

```
· 它是为 Pi【会话】建的（一个会话，一个 Anchor），不是为【图】建的。
  "Update is a lifecycle node" 是它自己的原则，但把它映射到一张图从未做过。
· 它的代码已归档，未集成进当前项目。
· 它的边界验证跑过了，但语义验收（含 50-to-100-Update plateau）从未跑。
```

**它自己的 §14 “Non-goals and host limits” 明确列出了它对宿主的依赖（原文）：**

```
The core does not include:
  · proactive per-turn or model-chosen compact triggers   ← 它从不自己决定压缩边界
  · Anchor-private context, time, tool-count, or checkpoint budgets
  · any scheduler, Agent pools, vector memory, or a knowledge graph

Pi currently validates model selection and provider authentication before firing the
Extension compact hook. Even exact receipt replay through ctx.compact() therefore
requires Pi's model/auth precondition.
```

**以下是我的观察，不是它说的（我之前把自己写的话误记成了它的）**：

> **它把两件事委派给了宿主——压缩边界，和 active window 本身**
> （见 §3.2 的投影公式：投影的三部分里，首尾两部分都是 Pi 提供的）。
> **而图里没有那个宿主。**那么在一张图里，这两部分由谁提供、由谁界定，
> 是它从未回答过的问题。

---

## 4. 我想要什么

### 4.1 场景

```
一张拓扑固定的图（节点与边不再改变）
在【单次 run】内连续工作 几十小时
期间累积的信息达到 几千万 token 量级
```

**"拓扑固定"是我明确设定的前提。** 我不关心"如何设计一张能自我改写的图"，
我关心的是：**给定一张固定的图，它能不能无限地跑下去。**

### 4.2 我要的性质：**连续性**

我用的词是**连续性**，而它和"不漂移"不是同一件事：

```
连续性    = 任务在任意长的时间轴上仍然正确地往下走        ← 我要的是这个（目的）
不漂移    = 决策不因"跑得久"而系统性偏离                  ← 这是手段之一
```

**连续性具体指什么**（借用原始 Anchor 的六项判据，我认为它们比我自己的说法准）：

```
Goal continuity       仍知道长期目标、验收标准、明确的不做什么
Constraint continuity 仍遵守硬约束、用户修正、明确禁止
Decision continuity   仍理解关键决策、为什么这么决定、什么情况下可以重新考虑
Failure continuity    避免重复失败路径、没有新证据的 retry、已被否定的假设
Intent continuity     仍知道当前指令、已接受的下一步、当前阻塞、下一个具体动作
Evidence continuity   知道哪些是已确认事实、哪些只是假设、证据在哪里
```

**它不是这些：**

```
不是：模型输出完全可复现            —— 那是确定性，我不要这个
不是：不花太多钱                    —— 成本是次生问题
不是：产出质量更高                  —— 见 §5.3，实测说这不由材料形状决定
```

**反面例子（实测）**：一个学术调研 run 跑到后期——评审违反自己声明的标准、
同一个缺陷被"修"了 18 次、引用编号漂移、报告与证据链脱节。
**这些不是随机波动，是连续性断了。**

### 4.3 验证方式的要求

**我不想通过跑一个几十小时的任务来验证。**

理由：那样的验证一次只能给一个数据点，非常昂贵，而且我怀疑它根本无法区分
"设计对"与"这次运气好"。

**我想要的是**：把连续性变成一个**可以推理、并且可以用单元测试验证的命题**——
测的是**单步转移的性质**，而结论覆盖任意步数。

**如果这不可能，那本身就是一个重要答案**，请明确告诉我。

---

## 5. 我已经确知的事实（全部实测，可复算）

### 5.1 硬墙的确切位置

```
上下文窗口 = 1,048,576 tokens（模型文档写的 1M）

实测：
  1,000,000 字符 →   125,032 tokens → 成功
  6,000,000 字符 →   750,032 tokens → 成功
  8,500,000 字符 → 1,095,300 tokens → HTTP 400

错误原文：
  "This model's maximum context length is 1048576 tokens.
   However, you requested 1095300 tokens
   (1062532 in the messages, 32768 in the completion)."
```

```
· 超限是 HTTP 400，不是 5xx —— 即"这个请求无效"，不是"稍后重试"
· 补全预留【也计入】：可用输入 = 窗口 − max_tokens（这里 32768）
· 错误消息【精确且可解析】——它告诉你超了多少
```

### 5.2 缓存的经济结构（我们的 provider）

```
缓存命中    $0.003 / M tokens
缓存未命中  $0.15  / M tokens      → 比值 50
输出        $0.6   / M tokens

一次真实 campaign 的账单构成：输出 61.7% / 缓存未命中 36.0% / 缓存命中 2.2%
```

**含义**：改变 prompt 的【前缀】会让整段前缀变成未命中，按 50 倍计价。

### 5.3 关于"上下文该放什么"的三项对照实验

**实验①②见 §3.8**（它们就是原始 Anchor 的第一个实验）。第三项：

```
实验③ 取回机制：状态固定，只换取回方式

  条件  机制                    gross      billed+输出   子调用次数
  ────  ────────────────────  ─────────  ───────────  ──────────
  A     材料进 prompt              100,945      100,305        0
  B     分页取回（累积）             306,970       74,650        0
  C     子调用取回（不累积）       1,616,556      337,324       18
```

**⚠️ 不要从这个实验里读“谁的机制更省”——A 与 B 的排序在两个口径下是反的：**

```
gross  下 A 便宜（100,945 vs 306,970）
billed 下 B 便宜（ 74,650 vs 100,305）

差别全在【前缀缓存】。而缓存冷热的影响极大：同一个条件（材料完全相同）
在不同运行里的 billed 可以是 68,569（冷）与 177（热）—— 相差 390 倍。
所以成本比较必须先确认缓存状态一致，否则测的是缓存而不是设计。
```

**两个口径下都成立、所以站得住的只有一条：**

```
C 显著更贵（gross 下 16 倍，billed 下 3.4 倍），原因是它【没有选择能力】：
要回答任何关于账本的问题，子调用就得装载整本账本；
代理只能枚举着把账本分块问完（18 次子调用）。

→ 取回的【接口形状】决定对象读多少。没有“先选择”的接口会退化成“读完一切”。
```

```
· 同样尺寸下组织方式影响很大（考卷 7.0 vs 20.3）—— 支持原始 Anchor
· 但材料的形状不改变【交付物】—— 两者的区别见 §3.8
· 成本的比较【被前缀缓存支配】，不是稳定的设计信号
```

### 5.4 漂移的实测

见 §4.2 最后一段。**我认为根因不是"上下文太长"，而是**：那个循环里
"判断自己是否在正轨上"的东西，**是模型对自己输出的判断**，而判断本身在环里自我强化。

**这是我的判断，不是测量结果。请质疑它。**

---

## 6. 现有机制的缺口（只陈述代码现状）

| 机制 | 现状 |
| --- | --- |
| 上下文窗口 | **worker 根本不知道它存在。** 没有任何地方声明窗口大小，也没有任何地方计算请求尺寸 |
| 超窗错误 | 被归类为 `model_rejected`（"模型拒绝了请求"）。**运维看到这个会去查请求格式，而真问题是输入需要拆分** |
| 工具循环的增长 | **按【调用次数】有界，不按【字节】有界。** 上限是 `sum(每个工具的调用上限 × 每次结果上限)`——**这个和没人计算，也没人和窗口比较** |
| 声明的输入 | 图可以声明任意大的输入（实测一个 writer 节点的 prompt 是 403,340 字符） |
| 累积的产物 | 在工件里，内容寻址，**不在上下文里**——这一条是好的 |
| "frontier"（账目边界） | 设计文档里有这个概念，但**是纪律，没有任何东西强制它** |
| **原始 Anchor 的那套机制** | **已实现、已测试、已归档——但没有集成，也没有跑完验证实验** |

---

## 7. 问题的准确陈述（这是我最想要你帮我的部分）

我认为"上下文管理"这个词把**两个轴**混在了一起，而两个轴都需要分开看。

### 轴一：哪一层的上下文？

```
(a) 单个 agent 的上下文
    一次节点执行里那个模型看到的东西

(b) 整张图的上下文
    跨节点、跨迭代累积起来的东西，以及它如何被投影回各个节点
```

**这一轴是我最不确定的。** 在原始 Anchor 里，"一个会话一个 Anchor"，
所以 (a) 和 (b) 是同一件事。**在图里它们不是。** 请特别注意这一条。

### 轴二：多长时间尺度？

```
① 单次模型调用之内
   prompt = 声明输入 + 工具循环已累积的对话          两者都可以无界

② 一次节点执行之内 —— 与①相同，因为一次执行就是一个模型循环

③ 图的一轮循环（跨节点）
   每次节点执行是全新的，累积在工件里                "看起来"有界？

④ 无限 —— 理论上不限轮数
```

### 我的三个问题

> **1. 轴一和轴二里，哪一个组合才是"无限时间轴"的真正问题？还是说它们需要不同的处理？**
>
> **2. 轴二③真的已经有界了吗？还是我只是把无界的东西从上下文搬到了"声明的输入"里？**
>    （现在的 writer 节点 prompt 是 403K 字符，而它在一张会循环的图上）
>
> **3. 如果要"用单元测试验证单步转移"，那个"步"应该定义在哪个轴上？**

---

## 8. 我认为任何方案都必须满足的约束

**以下是从 §5 的事实推出的，不是我的设计。若你认为其中某条是错的，那比方案本身更有价值。**

```
1. 上下文必须在【调用之前】就有界，而不是在调用失败之后
   （因为超窗是硬错误，不是渐变）

2. 有界必须是【结构性】的，不是【政策性】的
   一个"到了 90% 就压缩"的规则是在两次检查之间会失效的东西

3. 不得【静默丢失】
   危险不是遗忘，而是"遗忘了却不知道自己遗忘了"——
   一个不知道自己缺什么的 agent 会自信地继续

4. 前缀必须稳定
   改变 prompt 前缀会让整段前缀按 50 倍计价（§5.2）

5. 任何派生物都不是真相
   投影出错时必须能从真相重新导出
```

**我明确不知道的：**

- 第 3 条（不静默丢失）和第 1 条（调用前有界）是不是**可以同时满足**
- "有界"的界应该由**谁**决定——图？引擎？模型？
- 有界化的代价是不是一定表现为**行为退化**（而那就等于连续性断了，只是换了个名字）

---

## 9. 明确不在本次讨论范围内的

```
RSI / 自动改进                —— 我明确说先不着急
成本优化本身                  —— 成本是次生问题
"如何设计一张能自我改写的图"    —— 我假定拓扑固定
向量记忆 / RAG 的通用做法       —— 我调研过，没有一种解决这个问题
```

**关于"认知 schema 扩展"，有一处我必须说明清楚**（因为我之前说错过）：

```
· 原始 Anchor 自己写着："重点不是继续扩展 schema。重点是做实验。"
· 因此"停止扩展 schema"不是对原始 Anchor 的否定，而是执行它自己的建议
· 而“做实验”只做了 1/3（指 §3.7 那三个语义实验；它的边界验证见 §3.6）
· 所以：不要认为这个方向已经被证据否决了。它的主张【从未被验证过】。
```

---

## 10. 我希望你输出的

```
① 我的问题定义哪里不准？
   （特别是 §7 的两个轴——它们是不是真的两个轴，还是我把一件事拆成了两件）

② 这个问题的【结构】是什么？它是某个已知问题的一个实例吗？叫什么？

③ 有没有一个【可判定】的性质，它等价于"连续性"？如果没有，为什么没有？

④ "用单元测试验证单步转移"这个想法，有没有可能成立？
   如果成立，"一步"是什么？如果不成立，替代的验证方式是什么？

⑤ 【重要】原始 Anchor（§3）的机制是否已经足够？
   如果不够，缺的是什么？如果够，为什么它的实验从未跑完？
   特别注意 §3.9 的最后一条：它假定宿主提供 compact 边界，而图里没有那个宿主。

⑥ 你认为我漏掉了什么？特别是：我可能把一个真实约束误当成不重要的东西。
```

**最后一条方法论要求**：如果你要提出一个方案，请**同时说明它在什么条件下会失效**。
一个没有失效条件的方案，我没法用它做决策。

---

## 附：必读的原始材料

```
contextengine/docs/PRINCIPLES.md                       ← 八条原则，最快的入口
contextengine/docs/PRODUCT_SPEC.md                     ← 四个核心对象 + 十条不变量（§3.3 的来源）
contextengine/docs/ARCHITECTURE.md                     ← 充分统计量、注意力层级、证书、投影
contextengine/docs/ANCHOR_CONTINUITY_REVIEW_BRIEF.md   ← 与本文最相关：三个实验的设计
contextengine/docs/ANCHOR_UPDATE_PROTOCOL_V2.md        ← 语义操作 + 确定性物化 + 证书 + 事务边界
contextengine/src/update.js                            ← Update 机制（40 KB，最大的一块）
contextengine/test/cognition-v3.test.js                ← §3.5 那张证书的不变量测试
```
