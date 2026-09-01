# Anchor 核心产品方案

状态：生命周期产品定义；认知架构以 `docs/ARCHITECTURE.md` v0.5 为准

版本：0.5 draft

日期：2026-08-25

本文保留产品边界、用户生命周期和 Pi 集成要求。关于最小充分认知、可逆遗忘、
Active/Dormant/Archive 三层注意力、Transition Certificate、Artifact/Recall 和
有界投影的规范定义，以 `docs/ARCHITECTURE.md` 为唯一权威；本文较早的字段列表
和实施顺序不覆盖该架构。

## 1. 一句话定义

Anchor 是一个 Pi Extension。它把 compact 从“压缩聊天记录”升级为“提交认知
检查点”，使 Agent 在多次压缩、进程重启和长时间工作后，仍能准确恢复任务
目标、当前指令、有效事实、约束、失败经验和下一步。

Anchor 不替换 Pi，也不试图成为通用 Agent runtime。

## 2. 核心问题

长期任务中的 transcript 同时混合了目标、临时推理、工具输出、错误、修正和
决策。随着历史增长，Agent 必须反复从事件中猜测“现在什么是真的”。普通
compact 主要解决长度问题，无法保证多次摘要后仍保留用户修正、失败原因和
当前计划，因此容易产生认知漂移。

Anchor 将两类信息分开：

- Pi transcript 记录发生过什么；
- Anchor Checkpoint 记录当前什么是真的，以及它已经吸收了哪段历史。

完整 transcript 继续由 Pi 保留，用于 UI、审计和调试，但不作为长期任务的
主要记忆。

## 3. 产品成功标准

在一个经历多次 compact 和进程重启的真实任务中，新的 Agent invocation 应能
仅凭最新 Checkpoint 和 Pi 保留的近期窗口，正确回答并继续：

- 长期目标与明确非目标是什么；
- 用户当前要求是什么；
- 哪些事实已经确认，证据在哪里；
- 哪些只是推测或仍有冲突；
- 做过哪些关键决定；
- 哪些路径失败了，为什么不应重试；
- 当前 blocker 和下一步是什么。

总任务历史继续增长时，正常模型请求的 Anchor 输入不应随完整历史线性增长。

## 4. 产品边界

### 4.1 Anchor 负责

- Anchor 模式的启动与 Planning；
- Task 和 Checkpoint 的持久化；
- compact 时运行专门的 Update Agent；
- 验证候选 Checkpoint 并提交新版本；
- 将 Checkpoint 投影到 Pi 当前上下文；
- resume 时恢复同一个 Task；
- 保留 State 的版本和来源。

### 4.2 Pi 继续负责

- model transport、streaming 和 provider 兼容；
- 工具执行与用户配置的工具权限；
- interruption、retry 和 overflow recovery；
- transcript、session tree 和 TUI；
- context-window threshold、近期窗口和 replay 合法性。

### 4.3 核心版本不负责

- Contract 驱动的项目文件沙箱；
- 多进程工作区 writer lease；
- scheduler、agent pool 或角色系统；
- vector memory、知识图谱或通用项目管理；
- workspace 全量 hash 和并发文件治理；
- high-assurance reviewer authority；
- 自动完成审批或发布治理；
- Anchor 私有的 token、时间、工具次数或 checkpoint 预算。

这些能力只有在真实使用证明核心机制不足时才单独设计。Anchor 不宣称提供尚未
实现的安全或治理保证。

## 5. 四个核心对象

### 5.1 Task

Task 是一个 Pi session 中长期目标的稳定身份，包含用户确认的：

- goal；
- acceptance criteria；
- constraints；
- non-goals；
- risks、verification 和 confirmed execution plan。

一个 Pi session 最多创建一个 Anchor Task。Task 不因模型调用、compact 或
session tree navigation 而改变身份。

### 5.2 Checkpoint

Checkpoint 是权威的当前认知，由两部分组成：

```text
Checkpoint = State revision + source frontier
```

State revision 至少包含：

- current understanding；
- current directive；
- confirmed facts；
- active hypotheses 和 unresolved conflicts；
- decisions；
- failed paths 及原因；
- blockers；
- open questions；
- next action / next plan；
- evidence references；
- directive history references；
- parent version 和 provenance。

source frontier 表示这份认知已经吸收的 Pi 历史边界。对于 compact，它由 Pi 的
compaction preparation 确定，至少记录：

- Pi session identity；
- `firstKeptEntryId`；
- 被吸收输入的确定性 hash。

Checkpoint 没有 source frontier 就不能取代 transcript 中的任何内容。

### 5.3 Episode

Episode 是上一个 Checkpoint 尚未吸收、且本次需要归约的工作片段。compact 时
它严格来自 Pi 已经计算好的：

```text
preparation.messagesToSummarize
+ preparation.turnPrefixMessages
```

Anchor 不读取完整 branch 重新决定边界，也不把 Pi 决定保留的近期 suffix 重复
塞给 Update Agent。tool call 和对应 result 必须作为不可分割 replay unit。

### 5.4 Projection

每次 Active 模型请求的上下文遵循同一个公式：

```text
Pi fixed system / developer / tool prefix
+ latest authoritative Anchor Checkpoint
+ Pi compaction-aware active window
```

active window 中最新用户消息保持其自然位置。Checkpoint 保存长期认知，近期
窗口保存尚未沉淀的局部工作与对话连续性。

Projection 是对权威 State 的确定性读取，不调用 LLM，不扫描完整 EventLog，也
不自行裁剪 Pi 的窗口。

## 6. 核心不变量

1. State 回答“现在什么是真的”；transcript 只回答“发生过什么”。
2. Context 是 State 的投影，不是新的事实来源。
3. Checkpoint 必须声明准确的 source frontier。
4. frontier 之前的信息由 Checkpoint 承担，之后的信息由 Pi active window 承担。
5. 模型输出只是候选；通过 schema、identity、version、transition 和 provenance
   检查后才能提交。
6. Task goal、current directive、accepted next action 和 directive history 相互独立。
7. 用户最新的非省略指令具有立即执行意义；“继续”优先解析为 accepted next
   action，再以此前 directive 为支持上下文，不能回退为原始 goal。
8. Update 失败不得破坏旧 Checkpoint 或 Pi history。
9. compact 记录缺失不得使已提交 Checkpoint 无法恢复。
10. Normal Pi 模式没有 Anchor prompt、model tools、State 文件或 Context 投影；
    首次 Pi compact 可按需触发一次无感 Bootstrap，失败时回退原生 compact。

## 7. 用户模式

用户只需要理解三个模式：

```text
Normal -> Planning -> Active
```

- `Normal`：原生 Pi，Anchor 零侵入；
- `Planning`：只读澄清目标，尚无权威 Task；
- `Active`：Task 和 Checkpoint 已创建，正常执行和 Update。

Proposal review 是一次交互，不是持久模式。Update 是事务，不是用户模式。
Blocked 是健康状态，completed 是 Task 属性，不扩张主状态机。

## 8. 完整生命周期

### 8.1 新 Session

新的空白交互 session 直接以 Normal Pi 启动，不显示 Anchor 选择，也不创建
Anchor runtime 数据。若用户未通过 `--anchor` 或 `/anchor start` 显式进入
Planning，首次 Pi compact 时才按需执行 Bootstrap Update，创建 provisional
Task 和 Checkpoint 0；短任务不会产生 Anchor State。

非交互模式默认 Normal Pi；用户必须通过明确 flag 或命令启用 Anchor。

### 8.2 随时激活

用户可通过 `--anchor` 或 `/anchor start` 进入 Planning。Anchor 不用自然语言
猜测是否应该激活。

中途激活时，现有 active window 只是 Planning 材料，不自动成为权威 State。
Agent 应通过只读检查区分用户陈述、工作区事实和仍未验证的假设。

### 8.3 Grill 式 Planning

Planning 的目标是产生可靠的 Checkpoint 0，而不是立即执行任务。Extension 在
工具边界仅开放只读工具和 Planning 工具；这项限制只保护 Planning，不延伸为
Active 模式的项目沙箱。

`anchor_ask` 每次只提出一个会改变目标、验收、约束、风险或执行路径的问题。
能从项目中查到的事实由 Agent 查找，不反问用户。问题可提供推荐选项和取舍，
同时允许自由回答。

当 fresh Agent 无需猜测关键决策即可开始工作时，Agent 生成 proposal。proposal
至少包含 Task 字段和 Checkpoint 0 的认知字段。

若 Planning 本身触发 compact，Anchor 使用相同的 Episode 边界生成结构化
Planning summary，并将它作为 Pi compaction summary 保存。它只是尚待用户确认
的共同理解，不创建 Task 或 SQLite Checkpoint，也不能绕过 proposal review。

### 8.4 Proposal 确认

`anchor_propose` 只保存候选并结束 proposing turn。proposal 在 assistant turn
完整落入 Pi session 后才被 seal，避免 tool result 使自己的来源立即 stale。

Review 提供三个动作：

```text
Accept and create Anchor
Revise through discussion
Cancel Anchor Planning
```

proposal 绑定 producing entry 和内容 hash。如果它之后出现新的用户 directive，
旧 proposal 不能直接接受。这里不使用 workspace 全量 hash。

### 8.5 初始化

用户接受后，Anchor 在默认 session State 中原子创建 Task 和 Checkpoint 0。
proposal identity 是幂等键，重复接受必须得到同一 Task。

未进入 Planning 的普通 session 不会阻塞等待初始化。首次 compact 使用同一
Episode 调用 Bootstrap Agent，输出 provisional Contract 和 Checkpoint 0。它只
能保留 Episode 明确支持的目标、约束、事实和下一步；未知内容必须保留为未知
或 open question。Bootstrap 失败时不写入有效 Anchor State，Pi 继续原生 compact。

默认 State locator 由 Pi session identity 确定：

```text
<Pi agent dir>/anchor/sessions/<session-id>/anchor.db
```

因此默认模式不需要 branch binding 作为 Task 真相，也不存在“数据库已创建但
binding 尚未写入”的双写事务。Pi 中的小型 Anchor entries 只服务 UI、来源和
审计。

用户显式提供 State path 时才使用自定义位置。正常 Anchor 使用不写项目目录。

### 8.6 Active 工作

确认后恢复 Pi 原有 Active tools。Anchor 不根据 Task Contract 重新解释 shell、
路径或第三方工具权限；这些权限继续来自 Pi 和用户环境。

每次请求确定性投影最新 Checkpoint，并保留 Pi active window。新的用户 directive
先以近期消息形式生效，在下一次 Update 时折叠进 Checkpoint。Update 不得静默
修改用户确认的 goal、constraints 或 non-goals；发生实质范围变化时，Agent 必须
向用户说明并获得明确确认。

### 8.7 Update 与 Compact

manual、threshold 和 overflow compact 使用同一条路径：

```text
Pi prepares compaction boundary
        |
Anchor reads previous Checkpoint + Episode
        |
request-local schema-constrained submission produces complete candidate
        |
validate schema / identity / parent version / frontier / provenance
        |
CAS commit Checkpoint
        |
return Pi CompactionResult carrying Checkpoint receipt
        |
Pi appends compaction entry and keeps its recent suffix
```

Update Agent 通过唯一的请求内 `anchor_submit_update` function call 提交完整的当前
State，而不是聊天摘要、自由 delta 或自由文本 JSON。该 function 只作为 model
transport 的类型化返回通道：不注册为 Pi session tool、不执行、不产生 tool result
或 workspace 副作用。支持 strict tool schema 的 provider 直接约束采样；其他受支持
provider 仍强制选择这个唯一 function，并由 Anchor 对 arguments 做同一套确定性
验证。语义证据只能
来自旧 Checkpoint 和本次 Episode；确定性控制信封可以携带 immutable Task
Contract 与 target frontier，但二者不是 Episode directive 或新增证据。不得再次
发送完整 branch。

Pi compaction summary 只需提供人类可读的 Checkpoint 标识，结构化 `details`
记录 Checkpoint version、source hash 和 `firstKeptEntryId`。下一次模型请求仍以
Anchor State 为权威，不把该 marker 当作第二份认知。

### 8.8 两个持久化系统之间的恢复

SQLite Checkpoint 与 Pi compaction entry 无法组成真正的跨文件原子事务，因此
产品不宣称二者原子。可靠性来自可识别、可重放的 frontier：

- Checkpoint 提交前失败：旧 Checkpoint 和 Pi history 保持不变；
- Checkpoint 已提交但 Pi entry 尚未写入：resume 识别未交付 receipt，使用已提交
  Checkpoint 重新返回相同 CompactionResult，不再次调用 Update Agent；
- Pi entry 已写入：其 details 与 Checkpoint receipt 对齐后正常继续；
- receipt 或 source hash 不匹配：不猜测、不覆盖，停止 Anchor 投影并报告恢复错误。

恢复未完成 compact 必须发生在下一次 Active 模型请求前，避免同时发送新
Checkpoint 和它已经吸收的完整旧历史。

### 8.9 Resume、Tree 与 Fork

Resume 通过 Pi session identity 读取相同 State，不重新询问模式，也不自动调用
模型。Active session 显示 Task title 和 next action，等待用户输入。

Anchor State 是 session 级外部事实，不随 transcript tree navigation 回滚。正如
导航历史不会撤销已经写入工作区的文件，它也不会撤销已提交 Checkpoint。

创建新的 Pi session 或 fork 到新的 session identity 时，不自动复制可写 Anchor
State；新 session 默认从 Normal 启动，首次 compact 才按需 Bootstrap。跨 session 共享同一个 Task 不属于
核心版本。

### 8.10 结束

退出 Pi 时不强制调用模型或 Update。已提交 Checkpoint 保持持久化，未沉淀工作
仍保存在 Pi transcript，下一次 compact 再吸收。

Task 可以在 Checkpoint 中标记 `completed`，但核心版本不提供独立审批机构或
发布治理。新任务使用新的 Pi session。

## 9. Checkpoint 候选验证

提交至少检查：

- schema 名称和版本；
- Task 和 Pi session identity；
- expected parent Checkpoint version；
- source frontier 与本次 Pi preparation 一致；
- source hash 可复算；
- goal、constraints 和 non-goals 未被 Update 静默改写；
- required cognition fields 存在；
- Evidence reference 格式和 provenance 合法；
- tool call/result replay unit 未被拆分；
- failed tool call 没有被表达为成功事实。

stale CAS、非法 JSON、provider failure 或 interruption 均拒绝候选，不降级为
普通摘要，也不覆盖旧 Checkpoint。

## 10. Context 内容

Checkpoint 投影应直接回答继续工作所需的问题，不复制完整数据库对象。推荐
顺序为：

```text
TASK
  goal
  acceptance criteria
  constraints / non-goals

CURRENT COGNITION
  current understanding
  confirmed facts + evidence refs
  hypotheses / conflicts
  decisions
  failed paths
  blockers / open questions

CONTROL
  current directive
  accepted next action
  next plan
  checkpoint version / provenance
```

大型 Evidence 和 Artifacts 保持外部不可变存储，Context 只包含相关引用或有界
切片。State normalization 可以合并被取代的事实，但必须保留 revision、冲突和
原始来源链接。

## 11. 最小交互面

核心版本只需要：

```text
/anchor start
/anchor status
/anchor review
/anchor cancel    # 仅 Planning
/update           # 调用 Pi compact
```

TUI 只增加 `anchor_ask` 的原生 select/input、proposal review、footer
status 和 Update working message。不替换 Pi editor、header、footer 或整体主题。

## 12. 当前实现与目标差异

截至本文日期，仓库已有：

- Pi Extension package；
- Normal/Planning/Active、无感启动和 `/anchor start`；
- Planning prompt、`anchor_ask` 和只读工具集合；
- agent-settled proposal seal、review 和幂等 SQLite 初始化；
- session identity 模式归约，tree 不回滚且 fork 不继承可写 State；
- 仅含 Task 与 immutable Checkpoints 的最小 SQLite schema；
- State context projection；
- compact 时的串行 Update；
- `anchor.checkpoint.v1`、`anchor.cognition.v3`、`anchor.transition.v1` 和 source frontier；
- Update 只消费 Pi compaction preparation 选出的 Episode；
- Checkpoint schema 检查、parent hash 和 stale version CAS；
- compact receipt 及相同 frontier 的无模型重放；
- session-scoped 默认数据库；
- 首次 compact 的 provisional Bootstrap 和原生 compact fallback；
- focused tests。

生命周期、四次真实 Update、threshold auto-compact、restart、fork isolation、
receipt crash recovery 和 mismatch fail-closed 已完成真实 provider 验收。v0.5
认知切片也已实现：`anchor.cognition.v3`、stable item/provenance、Transition
Certificate、demotion reference 校验，以及 Contract-plus-cognition bounded
projection。剩余工作是 fresh-Agent takeover、50–100 次 Update
平台期和生产 p50/p95 验收；overflow compact 仍未完成真实 provider 验收。Pi 的
model/auth 前置检查和 process-wide tool list 仍是 host 限制。

这些是后续实现目标，不应在文档中描述为已完成行为。

## 13. 实施顺序

剩余工作按认知闭环依赖顺序实现：

1. 完成 correction、failure、recall、takeover、restart 和 churn 验收；
2. 在预发布 State reset 窗口结束后移除 v2 兼容路径。

在真实长任务证明核心机制有效前，不增加并发治理、权限沙箱或多 Agent 能力。

## 14. 验收

### 14.1 模式与初始化

- Normal Pi 不增加 Anchor prompt、model tools 或 Context 投影；首次 compact 可执行
  无感 Bootstrap，失败时回退 Pi 原生 compact；
- Planning 不能使用写工具；
- 未经用户确认的 proposal 不能创建 confirmed Task；首次 compact 的 Bootstrap
  只能创建 provisional Task；
- 重复确认同一 proposal 不创建第二个 Task；
- 一个 Pi session 不能创建第二个 Anchor Task。

### 14.2 Update

- Update 输入是 previous Checkpoint 加 Pi preparation Episode，不是完整 branch；
- tool call/result 在 Episode 和保留 suffix 中均不被拆开；
- malformed submission arguments、缺失/重复 submission call、语义校验失败时的一次
  validation-only correction、interrupted、provider-failed 和 stale candidate 不改变旧
  Checkpoint；第二次校正仍失败时必须取消；
- committed Checkpoint 准确记录 source frontier 和 provenance；
- failed path 及原因进入 State，而不是变成成功事实。

### 14.3 Recovery

- 进程重启恢复同一 Task、Checkpoint 和 next action；
- 在 Checkpoint commit 后、Pi compaction append 前终止进程能够无模型重放恢复；
- receipt mismatch 会停止并报告，不会覆盖 State；
- tree navigation 不回滚或切换 Anchor Task；
- 新 session 不自动共享旧 session 的可写 State。

### 14.4 长任务

真实 provider 验收至少包含：

- 三次以上 compact；
- 一次进程重启；
- 一次用户目标修正；
- 一次工具失败及恢复；
- 一条带 Evidence provenance 的关键事实；
- fresh invocation 正确复述 goal、current directive、decision、failed path、blocker
  和 next action；
- 正常模型输入不包含完整 pre-frontier transcript；
- Context compilation 记录输入大小和 p50/p95 延迟。

## 15. 最终判断

Anchor 的最小闭环只有一件事：

```text
previous Checkpoint + uncovered Episode
                 |
                 v
          validated next Checkpoint
                 |
                 v
      next invocation's stable Context
```

Planning 或首次 compact Bootstrap 产生 Checkpoint 0，Update 推进 Checkpoint，Resume 读取 Checkpoint，
compact 丢弃 Checkpoint 已覆盖的历史。其余能力都必须证明自己直接改善这个闭环，
否则不进入核心产品。
