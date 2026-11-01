# Harness 数据中枢：总体架构

编制：2026-10-20。状态：**用户已认可（2026-10-20），按阶段推进**。每个阶段按 [任务书](harness-data-hub-tasks.md) 实施，内核变更交回后单独审查。目标、判据和原则见 [方向文档](harness-data-hub.md)，本文只回答"代码怎么组织"。

## 1. 设计要求

- **可插拔**：整个中枢可以安装、卸载，Host 和内核不受影响。中枢内部的数据来源、存储后端、分析器和对外接口都可以替换、增减。
- **高内聚、低耦合**：每个模块只负责一件事，模块之间只通过小契约交互，依赖方向单一，由围栏测试守住。
- **不增加内核语义**：内核只多记录几样东西，不新增任何裁决规则。

## 2. 关键划分：记录是内核的一部分，分析是插件

| | 记录（写侧） | 分析（读侧） |
| --- | --- | --- |
| 是否可插拔 | **不可插拔，始终开启** | 完全可插拔 |
| 理由 | 事实是资产，漏记补不回来。没装中枢时也必须照常记录 | 分析随时可以重写、替换、删除，投影可以丢弃重建 |
| 运行位置 | Host 进程内，经 `Kernel::submit` | 独立进程，只读数据目录 |
| 代码位置 | `anchor-kernel`（小改）、新 crate `anchor-facts`、Host 内的记录适配器 | 新 crate `anchor-hub` |

这样划分之后，"可插拔"和"不漏记"两个要求就不再冲突。

## 3. 总览

```text
                 写侧（Host 进程内，始终开启）
  Goose ACP 用量/trace ─┐
  Graph 调度/节点结果 ──┼─► Host 记录适配器 ──► Kernel::submit ──► FileStore
  人工评分、实验分配 ───┘   （FactRecorder 门面）   （Clock 盖时间）   history.jsonl
                                  │
                                  └── 用 anchor-facts 的资源种类、Validator 和内容 schema

  ───────────────────────────── 进程边界：只读文件 + Host HTTP API ─────────────────────────────

                 读侧（anchor-hub，独立进程，可插拔）
  L0 来源  HistorySource ──► L1 投影 Projector ──► FactTable（可丢弃）
                                                       │
  L2 轨迹代数（纯函数）◄──────────────────────────────┘
        │
  L3 分析器插件 Analyzer：体检、停滞、工具效率、规则触发、流程挖掘、对比挖掘……
        │
        ├─► L4 对外接口：CLI、MCP（只读）、导出给 SQL 工具
        └─► L5 提议 ProposalSink ──► Host HTTP API ──► Kernel::submit（由人或 Validator 裁决）
```

## 4. 依赖规则

```text
anchor-kernel  ◄──  anchor-facts  ◄──  anchor-runner-host   （写）
                          ▲
                          └──────────  anchor-hub            （读）
```

- `anchor-hub` 只依赖 `anchor-kernel` 和 `anchor-facts`，**不依赖** Host、Runtime、GraphHost 或任何业务 crate。
- Host、Runtime 和内核**都不依赖** `anchor-hub`。删除整个 `anchor-hub`，其余代码照常编译、运行和记录。
- 中枢和 Host 之间没有代码依赖，只有两条运行时通道：只读数据目录，和 Host 已有的 HTTP API（用于提交提议）。
- `anchor-facts` 是写侧和读侧之间唯一的共享契约，必须保持很小，不放任何分析逻辑。

## 5. 写侧组件

### 5.1 内核变更（P1 已完成，冻结基点 C1 = `9a5fa57`）

内核只记录宿主盖章、提议方无法伪造的事实，不新增裁决。实际落地（都在 `rust/anchor-kernel/src/` 下）：

1. **Clock 端口**：`Clock` trait（`kernel.rs:30`），默认 `SystemClock`。
   - 时钟在构造时注入：`Kernel::with_clock(store, authority, clock)`（`kernel.rs:81`）；`Kernel::new` 等价于传入系统时钟。这样三个宪法 Validator 资源的创建时间也来自同一个时钟。
   - `submit` 在持有写锁时取一次时间，写进 `Stamp.at`（`model.rs:136`，单位为 Unix 纪元毫秒，`kernel.rs:177`）。内核只记录时间，从不根据它做判断。
2. **配置摘要随 Principal 走**：删除了全局的 `knobs` 和 `set_knobs`，改为 `Principal.config: Option<String>`（`model.rs:34`），用 `with_config` 设置；通过 `Stamp.config()` 读取。
   - 理由：配置是宿主为某个提议者冻结的身份的一部分，"哪个配置下的 Agent 在提议"，和 principal 一样由宿主盖章。这样 `submit` 的签名不变，仍然只有一个入口。
   - **约束**：`Principal` 的相等比较也会比较 `config`。授权和 Validator 只能按 `id` 和 `kind` 判断，不能拿整个 `Principal` 做比较或者当作键。这条规则写在 `Authority` trait 和 `Principal.config` 的文档里。
   - 宿主在 P2 之前一律不填。
3. **创建事实**：`Resource.created: Option<Creation { at, config }>`（`model.rs:292`、`:296`）。
   - `Kernel::create` 和 `install` 由内核盖时间；创建者只能通过 `Resource::configured` 提供配置摘要，自带的时间会被覆盖（`kernel.rs:128`）。
   - 内核为提交生成的 effect、pending 资源，沿用该提交的时间和配置。
   - 选择"资源上的字段"而不是"在历史里写一条创建记录"：所有修改都是在内核加载的资源上进行的，Validator 只返回状态和阶段，所以这个字段在每次修改、崩溃恢复和结算之后都会保留；而且历史的条数、探针的重建、恢复投影都不受影响。
4. **测量点**：`Point` 用 `at` 和 `config` 替代了 `knobs`（`model.rs:470`、`:472`）；Host 的探针重建同步修改。
5. **只读历史读取**：`HistoryReader`（`store.rs:370`）提供 `open`、`list`、`load`、`history`。
   - 不加锁，不创建任何文件或目录；
   - 只返回以换行结尾的完整记录；
   - 被删除的资源读出来是不存在：组装完成后会再检查一次状态文件（`store.rs:403`）；
   - 文件名超长、不可能存在于文件系统上的资源，和 `FileStore` 一样读出来是不存在（`store.rs:425`）。
6. **删掉了全局序号**（`model.rs:127` 的说明）。
   - 原内核的 `Stamp.seq` 是一个跨资源的全局计数器，只有两个用途：排列先后，以及给内核生成的资源起名。但历史是按资源分区、只追加的：同一资源内的先后就是文件里的位置，跨资源的先后看时间，所以全局计数器是多余的状态。
   - P1 第一版想让它跨重启唯一，结果带来了启动扫描、"一条历史坏了整个 Host 起不来"、删除后编号重复、启动开销翻倍等问题。删掉序号，这些问题一起消失；原内核里"重启后 effect、pending 资源可能撞名"的潜在缺陷也随之修复。
   - 内核生成的资源改用"这条记录在目标资源历史里的位置"命名：`effect/{target}/{位置}`、`pending/{target}/{位置}`（`kernel.rs:468`）。历史只追加，位置天然唯一，不需要任何跨资源的协调。只有 Execute 和 Pending 两条路径需要多读一次目标资源的历史长度，普通提交不受影响。
   - 旧记录里的 `seq` 字段读取时被忽略。
7. **因果关系不进内核**：P1 第一版加过 `Submission.cause`，"原样记录，不校验，不解读"。这样的字段按定义就是内容，所以删掉了。因果用天然标识写进提交的 `content`，比如 Goose 的 `messageId`、`toolCallId`；人工批准记录的内容里本来就有 `{"approved": pending.id}`。

**已知边界**（审查登记，Host 用上 Execute 或 Pending 之前必须处理）：

- **同一个 id 删除后重建**：如果旧的 `effect/…`、`pending/…` 资源还在，新 effect 会撞名；更重要的是，旧 effect 处在 `unknown` 状态时，按前缀查找未知 effect 的判断（`has_unknown_effect`）会把它算到新资源头上，新资源的 Execute 会被拒绝。现在走不到，是因为生产 Host 不创建 effect 和 pending 资源。规则：删除资源时连同它的 effect、pending 资源一起删除，或者禁止复用资源 id。
- **嵌套 id 的前缀匹配**：`effect/run/1/` 也会匹配 `run/1/x` 的 effect，所以子资源的未知 effect 会阻断上层资源的 Execute。这是原内核就有的问题，和上一条一起处理。

新字段一律可选，旧记录读出来是"未记录"，不迁移，也不拒绝。测试见 `rust/anchor-kernel/tests/recorded_facts.rs`。其中的差分测试让两次运行的时钟和配置都不同，并且覆盖了普通工作、外部效果及其生命周期、人工批准、过期提交和预算耗尽，两次运行的裁决和所有资源完全相同。

### 5.2 `anchor-facts`：事实词汇表（P2 已实现）

`rust/anchor-facts`，只放写侧和读侧都需要的契约，依赖只有 `anchor-kernel`、serde 和 sha2：

- **资源命名**：
  - `trace/{run}/{node}/{invocation}`：一次 AgentNode 调用的观察；
  - `config/{digest}`：配置清单，按内容寻址。
- **Validator**：
  - `validator/observation`：只接受宿主代码以 Write 提交、内容合法、并且属于该 trace 的观察。Agent 不能提交关于自己的观察；
  - `validator/configuration`：配置清单创建后不可修改。
- **授权**：`RecorderOnly`，只允许 `anchor/recorder` 以宿主代码身份写 `trace/…`，并且只按 id 和 kind 判断。
- **内容格式**（版本 1）：`Observation { version, invocation, facts }`。事实分三种：
  - `lifecycle`：开始、结束、结局、轮数、工具次数、是否恢复；
  - `model_turn`：token、费用、耗时、首 token 时间、`messageId`、观察时间。注意观察时间是 Host 收到用量报告的时间，Goose 要等这一轮请求的工具调用跑完才报告，所以它不是模型响应结束的时间；一轮最晚在它的第一个工具调用开始时结束（P3 用真实数据发现，见 [P3 报告](../experiments/harness-hub/p3/report.md)）；
  - `tool_call`：工具名、参数形状摘要、开始和结束时间、Goose 的 `toolCallId`、它回应的是第几次模型请求、是否成功、失败类别。
- **配置清单** `AgentConfiguration`：Graph 摘要、节点、模型及绑定、Goose 二进制、提示词摘要、Plugin 说明摘要、Plugin 身份、路由。摘要与 Plugin 和路由的顺序无关。
  - P6 起还记录 `host_sha256`（Host 可执行文件的摘要）和 `tools_sha256`（交给模型的工具定义摘要，与顺序和键序无关）。Graph 公共执行边界另记 `envelope_sha256`，规范 JSON 复用同根 `config/{envelope_sha256}` 不可变资源；signals/configurations 显示其短摘要，读取单个配置同时返回环境描述，contrast/gate 支持按该字段分组。三个字段为空时不序列化，旧清单的摘要不变。
- **不带业务内容**：工具参数只记形状摘要，错误只记类别，提示词只记摘要。

- **评分**（P4 已实现）：
  - 资源 `grade/{run}`，Validator `validator/grade`：只接受人或宿主代码以 Write 提交，模型不能评分，无论评自己还是评别人；内容必须是名字唯一的检查项列表，并且属于该 Run；
  - 授权 `GraderOnly`：只允许 `anchor/grader` 以宿主代码身份写 `grade/…`。以后人工评分走同一种资源，提交者是人；
  - 内容 `Grade { version, run_id, task, task_version, checks: [{name, passed}] }`，只有检查项名字和是否通过，不带业务值；
  - 提交者的配置摘要是评分器配置的摘要，即任务定义、期望值和评分代码。同一个 Run 重新评分时，在同一段历史里追加一条记录。

实验分配等 P6 用到时再加。

### 5.3 Host 记录器（P2 已实现）

- `rust/anchor-runner-host/src/fact_recorder.rs`：对外只有 `FactRecorder::record`。由围栏测试保证，Host 里只有这个模块会提交观察、或者打开 `kernel-facts` 数据根。
- **观察事实存在执行端口自己的数据根 `state/kernel-facts/`**，有独立的写入租约，不放进 Run 的数据根。用的是同一个 `Kernel::submit`、同一种 Record 格式。这样做：
  - 记录时的写盘不会和 Run 的裁决争用写锁；
  - 执行端口不需要去拿 Run 的内核；
  - 符合"每个所有者存自己的历史"。
- **记录点**：
  - `goose_acp/bridge.rs` 在 Host 侧观察每次工具调用的开始和结束；
  - `goose_acp/usage.rs` 给每轮模型对话记下观察时间；
  - `GooseNodePort::execute` 在调用结束时组装配置清单和事实（`goose_acp/facts.rs`），提交**一条**记录。
- **粒度**：每次调用一条记录，内容里是这次调用的全部事实，每个事实自带发生的时间。实测每次提交要同步写盘 3 次，约 20 毫秒；事实都是在调用结束时一起上报的，逐条提交不会多带信息，开销却按事实数成倍增长。真实模型下，每次调用的记录开销固定约 39 毫秒，平均占 Run 时间的 0.7%，见 [P2 报告](../experiments/harness-hub/p2/report.md)。
- **记录失败不影响执行**：失败写进 invocation evidence 的 `fact_recording`；数据根不可用时不记录，执行照常进行。
- **边界**：
  - 调用中途崩溃会丢失这次调用的事实；
  - Op 节点、Pilot 会话、Run 历史中的节点结果提交，都还没有记录或盖配置摘要。Op 节点的耗时可以由同一 Run 相邻记录的内核时间差得出（串行时准确，并行时是上界），P3 的中枢已经这样计算，不必为此改 Runtime。

### 5.4 评分记录与评测任务集（P4 已实现）

- `rust/anchor-eval`：对外只有 `GradeRecorder::record` 和命令行 `anchor-eval record --state DIR --config DIGEST < grade.json`。评分存在评分方自己的数据根 `kernel-grades`，有独立写入租约，用的是同一个 `Kernel::submit`。围栏测试保证 workspace 里只有这个 crate 会记录评分。
- 评测任务集在 `experiments/harness-hub/eval/`。每个任务是一个 Graph，由评测运行器组装：
  1. 固定的 `prepare` Op 写入合成输入；
  2. 待测的 AgentNode，以后的变体只改这一段；
  3. 固定的 `grade` Op 用 `graders.py` 检查输出，打印检查项，总是正常退出。

  评分器不在任何变体能改到的范围内。运行结束后，运行器从 Run 自己的历史里读出 `grade` Op 打印的检查项，经 `anchor-eval` 记成评分。
- 详见 [P4 报告](../experiments/harness-hub/p4/report.md)。

## 6. 读侧组件：`anchor-hub`（P3 已实现 L0–L4）

先做成**一个 crate**，内部按层分模块，用围栏测试守住层次依赖；等某个分析器需要很重的外部依赖时，再把它拆成单独的 crate，实现同一个 trait。这符合开发约定"不为追求目录数量而拆分"。

| 层 | 模块 | 职责 | 可替换的实现 |
| --- | --- | --- | --- |
| L0 来源 | `source` | trait `HistorySource`：列出资源，按序读取记录，支持从某个位置继续读 | 数据目录（经 5.1 第 5 项的 `HistoryReader`）；内存（测试用）；以后可加多 Host 汇总 |
| L1 投影 | `table` | `Projector` 把 Record 展开成事实行，每次提交一行；按资源记录读到哪里，支持增量；trait `FactTable` 负责存取 | 先用内存实现；以后加列式文件或嵌入式分析数据库。投影可以随时删掉重建 |
| L2 轨迹代数 | `query` | 纯函数，没有 IO：筛选、按配置分组、构建轨迹（按内容里的天然标识连成图，例如 `messageId`、`toolCallId`）、对齐、比较、片段统计、还原实际流程、比较分布 | 不需要替换，只会新增算子 |
| L3 分析器 | `analyzers` | trait `Analyzer`，见第 7 节 | 每个分析器是一个插件，按配置启用 |
| L4 对外接口 | `adapters` | CLI、只读 MCP 服务、导出 | 新增接口不影响下层 |
| L5 提议 | `proposals` | trait `ProposalSink`：把建议交给 Host | 先写到文件供人看；之后接 Host HTTP API，作为一次提交 |

层次依赖只能向下：L2 不能依赖 L3，L3 不能直接做 IO，所有 IO 都在 L0、L4、L5。

**事实的键和顺序**（见 5.1 第 6 项）：

- 每一行事实的键是"数据根 + 资源 id + 在本资源历史中的位置"，位置由读取顺序得出；
- 同一个资源内部按位置排序；跨资源的顺序用 `Stamp.at`，时间相同或缺失时不推断先后；
- 内容里的因果标识可能悬空（被引用的一方没有记下来），悬空的边保留并标记，不丢弃，也不报错。

**P3 的实现**（`rust/anchor-hub`，详见 [P3 报告](../experiments/harness-hub/p3/report.md)）：

- L0：`DataRoot` 经 `HistoryReader` 只读；`Stored` 把任何内核 `Store` 当来源，测试用。
- L1：`Projector` 按资源记住读到的位置，增量投影；`MemoryTable` 是第一个 `FactTable`。只投影 Harness 事实（主体、时间、配置、动作、槽位、裁决、任务状态，以及观察记录里的事实），其他提交内容、结局说明和证据不投影，所以投影里没有业务内容。配置清单只在摘要和名字一致时收录。
- L2：Run 的节点结果按 `run/{run_id}` 和槽位 `{node}#{invocation}` 与观察记录的 invocation 关联；时间线、空档及其位置（启动、工具到下一轮、收尾）、任务步骤（相邻记录的时间差）、分组、连续重复、分布。
- L4：`anchor-hub health|stall|rows [--json] PATH...`，自动发现路径下的所有内核数据根；`stall --follow` 持续跟读。
- 围栏测试在 `rust/anchor-hub/tests/fences.rs`，覆盖第 9 节第 1 到 3 项。
- P6 加入对比分析器 `contrast`（`anchor-hub contrast [--by FIELD] [--reference PREFIX]`）：
  - 按配置清单的某个字段（默认 `host_sha256`）把评过分的 Run 分组；
  - 每个任务比较质量、Agent 用时、token、轮数、工具失败；
  - 跨任务时，把用时和 token 除以该任务对照组的均值再合并，做 Mann-Whitney 检验；工具失败率做两比例检验；
  - 质量下降超过 0.05 时报告 `contrast_quality_loss`。

  统计函数是 L2 的纯函数。
- P4 加入评分：L1 把 `validator/grade` 的记录投影成评分行；L2 的 `Query::grades` 给出每个 Run 最新被接受的评分；体检报告新增质量部分，按任务和任务版本列出平均分、全对比例、各检查项通过率，以及这些 Run 的 token、Agent 耗时和工具调用分布。某个检查项在至少 3 个 Run 里有一半以上失败时，报告 `check_failing`。
- P7 加入信号分析器 `signals` 和只读 MCP 接口（见 P7 任务书）：
  - `signals` 把体检和停滞的发现按"类型 + 位置"合并，每条信号有稳定 id、Harness 空间坐标（配置、Graph、节点、模型、工具、Host）和有界的证据；
  - 每个评分任务求 (质量, token, Agent 用时) 上没被支配的 Harness 前沿，被支配的 Harness 本身是信号 `dominated_harness`；
  - 正向离群作为范例：同任务同节点里 token 不到中位数一半的 `frugal_route`，以及只有一个调用走过、成本不高于中位数的 `rare_route`；
  - 表有上限，放不下的按类型计数（`omitted`），不静默丢弃；
  - `anchor-hub mcp PATH...` 在 stdio 上讲 JSON-RPC，工具 `signals`、`report`、`invocations`、`invocation`、`configurations`、`records` 都只读、分页、有上限，每次调用前增量刷新。没有新增依赖。
- `run_access/reads.rs` 不改用 `HistoryReader`：它的调用方都在作为写入者的 Host 进程内，读时拿租约正好和提交串行；中枢不经过它。

## 7. 插件契约

### 7.1 分析器

每个分析器声明：

- **身份**：id 和版本。结论里会带上它们，方便追溯是哪个版本得出的。
- **所需字段**：比如"需要时间"、"需要因果标识"。缺字段的数据，分析器明确报告"不适用"，不猜测。
- **运行方式**：批量，或持续跟读。停滞检测这类需要近实时结果的分析器用跟读。
- **输出**：
  - Findings：结构化的结论，每条都引用具体的记录作为证据；
  - Proposals：可选的改进建议。

分析器只能拿到 L2 的查询接口，拿不到文件和网络，所以天然是只读、可测试、可以随意增删的。

第一批内置分析器：Harness 体检报告、停滞检测、工具效率、内核规则触发统计。之后加流程挖掘、对比挖掘、新颖性检测。P3 实现了前两个；工具效率和内核规则触发统计读的是同一批事实，先作为体检报告的组成部分（工具统计、裁决统计和 `not_accepted` 发现），需要单独演进时再拆出来。

### 7.2 选择策略（以后再做）

多臂老虎机、进化搜索这类策略也是插件，trait 名暂定 `Policy`。它们只产出"实验分配"或"配置变更"的提议，Host 收到后作为提交，由人或 Validator 裁决；实验分配本身作为事实落盘。策略永远不直接改配置。

## 8. 提议如何回到系统

- 中枢不能写数据目录，只能通过 Host 的 HTTP API 提交提议。
- 提议以 `analyzer/<id>` 这样的身份、`PrincipalKind::Code`、`Action::Request` 提交，内核和 Validator 按规则裁决，需要人决定的就等人。
- 这样中枢的建议和其他任何提议走同一条路，被采纳、被拒绝都会留在历史里，本身又成为下一轮分析的数据。

## 9. 围栏测试

1. **依赖围栏**：`anchor-hub` 的依赖里不能出现 Host、Runtime、GraphHost 和业务 crate；反过来，任何 crate 都不能依赖 `anchor-hub`。
2. **只读围栏**：`anchor-hub` 里没有任何写数据目录的代码路径，只允许写它自己的投影目录。
3. **层次围栏**：L2、L3 不做 IO；L3 只经过 L2 访问数据。
4. **记录门面围栏**：Host 里只有记录适配器调用内核提交观察，其他模块只能调用 `FactRecorder`。
5. **内核围栏**：内核变更审完后重新冻结 C1，内核不出现基于时间、token 的判断。
6. **评分写入围栏**：workspace 里只有 `anchor-eval` 记录评分（`rust/anchor-eval/tests/fences.rs`）。

## 10. 和 `anchor-rsi` 的关系（用户已决定：不合并）

`anchor-rsi` 现在是一个只读的 MCP Plugin，给 RSI Graph 提供代码、文档和 Run 元数据等业务审查证据（见 [RSI](rsi.md)）。它和中枢不是同一层：

- 中枢是平台层的基础设施，只看 Harness 事实，不看业务语义；
- `anchor-rsi` 是业务 Plugin，RSI Graph 是由大模型驱动的使用方。

**决定**：中枢不并入 `anchor-rsi`。`anchor-rsi` 改为通过中枢的只读 MCP 接口获取运行数据，替代它现在自己发现 Run 元数据的那部分。这样运行事实只有一个分析入口。

## 11. 推进顺序与验证门槛

每个阶段都是能独立交付的切片；门槛不通过就停下来重新考虑，不继续往上建。

| 阶段 | 内容 | 门槛 |
| --- | --- | --- |
| P0 | 噪声测量：用真实 Provider 把小任务重复跑 N 次，用现有的 usage 和 trace 数据统计方差。不改代码 | G0：确认效率指标的方差小到能检测出差异，并得出每次对比需要的样本量 |
| P1 | 内核变更（5.1） | 单独审查通过；C1 重新冻结 |
| P2 | `anchor-facts`（5.2）和 Host 记录器（5.3）作为同一个垂直切片，实测写盘开销。**已完成**，见 [P2 报告](../experiments/harness-hub/p2/report.md) | 开销在可接受范围内；记录失败不影响执行 |
| P3 | `anchor-hub` 的 L0–L2，体检报告、停滞检测两个分析器，CLI。**已完成**，见 [P3 报告](../experiments/harness-hub/p3/report.md) | G1：用真实 Run 的数据发现至少一个以前不知道的问题。**通过** |
| P4 | 评测任务集：一组结果可以自动判断对错的任务，作为便宜、客观的质量信号。**已完成**，见 [P4 报告](../experiments/harness-hub/p4/report.md)：基线下质量几乎总在上限，受控实验以效率为目标、以质量为约束 | 任务集能稳定复跑，判分可信。**通过** |
| P5 | 只读 MCP 接口，流程挖掘、对比挖掘；按第 10 节调整 `anchor-rsi` | 分析结论能落到具体的配置和节点上 |
| P6 | 受控实验和 `Policy` 插件。**实验 1 已完成并采纳**，见 [实验 1](../experiments/harness-hub/p6/experiment-1.md)；`Policy` 插件未做 | G2：在能承受的样本量内检测出效率差异。实验 1 中，工具失败率的差异在每组 100 次时显著（p < 0.0001），用时和 token 7% 量级的差异在这个样本量下不显著。G3：未做 |
| P7 | 在中枢上的 RSI：`signals` 分析器、只读 MCP 接口（**已实现**），按信号编排的 RSI Graph、带可证伪预测的提议账本（未做）。见 [任务书 §P7](harness-data-hub-tasks.md#p7-在数据中枢上的-rsi) | G4：RSI 读取的上下文不随 Run、Graph、Plugin 的数量增长；G5：提议的预测能被下一轮数据判定为成立或不成立 |

`anchor-facts` 推迟到 P2，和它的第一个使用方一起交付，不提前建一个没人用的契约。

细腰扩展到 Graph、Plugin 保存和 Session 投递是一条独立的迁移任务线，可以和 P3 之后的阶段并行。

## 12. 用户决定（2026-10-20）

- 本架构整体认可；
- 中枢不并入 `anchor-rsi`，见第 10 节；
- P0 使用 Anchor 平台已保存的渠道和 Flash 模型来跑，不必节省 token；
- 保留策略以后再定。
