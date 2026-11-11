# Harness 数据中枢

本文统一维护数据中枢的已确认原则、当前记录与分析契约，以及尚未实施的方向。事实记录、只读投影、分析器和 MCP 已有实现；这不等于自动改进闭环已经完成。当前验收与未关闭事项统一见[验收台账](pilot-development-plan.md)，历史任务和实验不构成新的实施授权。

## 目标与已确认原则

目标是利用运行事实，找到更好的 Harness、Graph、Plugin 组织与组合，识别 Agent 行为、罕见但有效的路线、编排瓶颈和架构删减候选，支撑 Dreaming：由全局运行事实驱动、带独立验收的离线 Harness 改进。专门的分析工具和算法是主力，大模型是工具的使用方之一，不以让模型通读全部原始数据代替分析接口。自动采纳与自身换代的目标边界见 [Dreaming 设计](dreaming.md)，不表示闭环已经实现。

用户已确认的原则继续有效：

1. **质量优先。** 同时衡量任务质量、token 与费用、时间、工具效率。人工介入、未知效果、迟到结果、冲突和停滞也是信号；流程成功、质量通过和长期收益分别判断。
2. **事实优先。** 身份、成本、时间和配置由宿主或内核盖章，不采用模型自报数字。模型对质量的判断须标来源，不能冒充权威评分；遗漏的事实以后无法补回。
3. **统一提交，提议与裁决分离。** 观察与评分复用 `Kernel::submit` 和同一种 Record，不另造通用日志或 Agent loop。内核记录时间、配置与内容，不新增基于时间、token 的裁决；业务语义由对应 Validator 处理。
4. **历史是权威，投影可重建。** `points.jsonl` 是有损摘要，不能代替历史归因到具体 Graph 或 Plugin。中枢不能直接改数据、Graph、Plugin 或配置；改进必须经过宿主的提交与裁决路径。
5. **不设默认预算。** 模型正常工作就继续，保留内核已有的预算结算规则。停滞报告不自动停止任务；是否新增自动停止策略仍须另行决定。
6. **只读与可插拔从边界保证。** 没有安装分析中枢时仍记录事实；分析工具可独立替换、卸载，不能反向成为 Host 或内核的依赖。
7. **真实 Provider、外部渠道实网和目标部署验收由用户负责。** 本地回归和历史样本不补签这些结果；已授权实验也不自动授权发布、改权限或生产切换。Dreaming 的目标是在外部预授权范围内，经独立裁决自动采纳与撤回，而非逐项请人决定；专用写路径尚未实施。

相关不等于因果。历史分析用于提出假设，配置优劣须由受控实验验证；任务分类若带业务语义，应明确使用结构特征还是人工标签。原始对话仍由 Goose 原生会话持有，结构化事实不复制完整会话、提示词或隐藏推理。

## 当前职责与依赖

| 部分 | 职责与所有者 | 边界 |
| --- | --- | --- |
| 记录侧 | `anchor-kernel`、共享词汇 `anchor-facts`、Host 的 `FactRecorder` | 随执行记录，不依赖是否安装中枢；记录失败不转成任务失败 |
| 评分侧 | `anchor-eval::GradeRecorder` | 独立评分身份与数据根，不由被测 Agent 自评 |
| 分析侧 | 独立进程 `anchor-hub` | 只读获授权数据，投影在内存中；不写任何数据根 |

```text
anchor-kernel <- anchor-facts <- anchor-runner-host   记录
                            <- anchor-eval          评分
                            <- anchor-hub           分析
```

`anchor-hub` 的产品 crate 依赖仅为 `anchor-kernel` 和 `anchor-facts`，不依赖 Host、Runtime、GraphHost 或业务 crate；其他 crate 不依赖它。`anchor-facts` 只放资源命名、schema、摘要和 Validator，不放存储或分析逻辑。中枢目前保持一个 crate，数据来源、投影后端、分析器和对外接口通过小契约分层，不为可插拔预先拆服务。

## 当前写侧契约

### 内核盖章与顺序

- `Clock` 由 `Kernel::with_clock` 注入，默认 `SystemClock`；提交在写锁内取 UTC 毫秒，记入 `Stamp.at`，不保证时钟单调。
- 配置摘要是 `Principal.config`，可由 `Stamp.config()` 读取；授权和 Validator 只按 principal 的 `id`、`kind` 判断，不能将包含配置的整个 Principal 当授权键。
- 创建通过内核 `create` / `install` 盖 `Resource.created { at, config }`；后续状态改变保留创建事实。`Point` 记录时间与配置，不再有全局 knobs。
- 同一资源按历史中的位置排序，不维护跨资源全局序号；跨资源参考时间，时间缺失或相同不推断先后。因果使用 `messageId`、`toolCallId`、模型请求序号和 invocation 等天然标识，放在提交内容，不增加内核因果字段。
- 可选字段缺失表示未记录，不猜测、不补造旧事实；旧序号字段不恢复为新的全局计数器。

### 观察、配置与评分

共享词汇见 [`anchor-facts`](../rust/anchor-facts/src/lib.rs)，当前内容版本为 1：

| 资源 | 内容与约束 |
| --- | --- |
| `trace/{run}/{node}/{invocation}` | `Observation { version, invocation, facts }`；`validator/observation` 校验 invocation 归属，`RecorderOnly` 仅允许 `anchor/recorder` 以宿主代码身份 Write |
| `config/{digest}` | 不可变配置清单，`validator/configuration` 守住内容寻址；配置身份不同不改写旧清单 |
| `grade/{run}` | `Grade { version, run_id, task, task_version, checks: [{ name, passed }] }`；检查项名字唯一、不含业务值，重新评分追加历史 |

观察包含以下事实：

- `lifecycle`：调用开始、结束、结局、模型轮数、工具次数、丢弃计数和是否恢复。
- `model_usage`：本次 prompt 已观察到的完整用量汇总、prompt 是否正常结束、各字段报告轮数，以及明细保留/省略轮数。汇总不限于前 64 轮，不混入 Goose 原生 Session 的跨调用累计数；字段只报告一部分时，数值是已报告部分，不代表总成本。
- `model_turn`：最多前 64 轮的输入、输出、缓存 token、费用及可用的耗时、首 token 时间、消息标识；缺字段为 `null`，不是零。用量报告的观察时间晚于该轮工具执行，不是模型响应结束时间；时间线计算不应把工具耗时重复算成模型耗时。
- `tool_call`：工具名、参数形状摘要、调用标识、模型请求序号、时间、成功与失败类别；可选的路径类别和命令程序名不保存完整路径或 argv。`shell_program` 只是 shell 首个命令的启发式归因，不替换原程序名；工具调用成功不代表内部命令零退出，另看 `CommandEnd`。
- `workspace_fingerprint`：Goose invocation 开始、结束时的 git HEAD、dirty 与已知锁文件相对路径和摘要。在同一命令沙箱中采集，不借宿主执行仓库配置；采集失败或非 git 现场省略，不影响调用。
- `input_wait`：已授权 `session.wait_input` 接收器进入/退出等待的宿主观察，只含 `waiting`；时间由 Kernel 盖章，不含用户或消息内容，也不伪造 Agent/模型配置。

以上为当前版本 1 词汇的扩展；新工具读取旧事实，无需改写历史。旧工具不保证理解新事实种类或可空字段，部署时须使用配套记录器与读侧。

`AgentConfiguration` 绑定 Graph、节点、模型及绑定、Goose、指令摘要、Plugin 身份与说明摘要、路由；Plugin 和路由等集合规范排序。`host_sha256`、`tools_sha256` 区分 Harness 和工具定义，`envelope_sha256` 引用同根不可变环境描述；空字段不序列化，旧清单摘要不因新增可选字段改变。环境描述不含凭据值或 Run/Workspace 来源路径，具体沙箱边界仍由[当前架构](architecture.md)定义。

`validator/grade` 不接受 Agent 评分，既不能评自己也不能评别人；当前 `GraderOnly` 写路径只允许 `anchor/grader` 的宿主代码身份。未来人工评分可沿用评分资源，但不表示已有人工入口。`anchor-eval record --state DIR --config DIGEST` 将输入评分记到评分方自己的 `kernel-grades`，持独立写入租约；评分配置摘要标识任务、期望与评分代码。评测 Graph 固定为准备 → 被测节点 → 评分，被测变体不能修改评分器。

### 记录路径与缺失边界

Host 仅经 [`fact_recorder`](../rust/anchor-runner-host/src/fact_recorder.rs) 写观察；同一 state 的执行端口共用记录器和单写入租约。观察位于执行端口的 `state/kernel-facts/`，不与 Run 的裁决争用同一数据根或资源版本。

Bridge 观察工具调用，usage 接收模型用量；每次 Goose 执行 attempt 结束后由 `GooseNodePort` 将配置、用量汇总和有界明细一并提交，不逐 token、流式片段或单个工具事实提交。prompt 返回错误时也保存已收到的用量，并标为不完整；同 invocation 恢复可有多条 attempt 记录，只按汇总累计，不再次累加对应明细。记录失败写入 invocation evidence 的 `fact_recording`，不妨碍执行；进程中途崩溃仍可能丢失本次观察。

输入接收器复用同一个 `FactRecorder` 和 trace，仅在进入/退出等待时追加宿主观察，不随轮询反复写入；领取输入、取消或正常释放时退出等待。同 key 恢复的首次观察也写入，避免沿用旧 attempt 的等待状态。此类记录不算模型 attempt、模型恢复或无结果模型调用；不改变 Session 领取、排队、打断和恢复语义。

Pilot 会话、Op 节点尚无同等完整观察，Run 节点结果提交尚未盖完整配置摘要；Op 耗时可从相邻 Run 记录推算，串行时准确、并行时只是上界。缺记录或缺 usage 是未知，不是零成本。

工具观察是事后事实，不是 `Execute` 的前置许可。生产 Host 未以 Kernel 的 effect/pending 资源记录工具生命周期；后续接入前须处理资源 id 删除重用和嵌套前缀的未知 effect 匹配边界，不能直接套用观察接口扩大权限。

## 当前读侧契约

### 分层与事实投影

| 层 | 当前模块与职责 |
| --- | --- |
| L0 来源 | `HistorySource`；磁盘 `DataRoot` 经 `HistoryReader`，测试可用 `Stored` |
| L1 投影 | `Projector` 按资源记住读取位置，增量写入 `MemoryTable`，通过 `FactTable` 提供可替换存取边界 |
| L2 查询 | `Query` 纯函数，关联 Run 与 invocation，给出时间线、空档、分组、分布、评分和工具路线 |
| L3 分析 | `Analyzer` 只拿 L2 查询，不做文件、网络或进程 IO |
| L4 接口 | `adapters` CLI 与 `mcp`，发现已授权来源、刷新投影并输出结果 |

这里的增量是投影位置语义，不是物理读取复杂度保证：当前磁盘 `DataRoot::history_after` 仍读取单资源完整历史后切片，每次刷新也会枚举来源与资源。全局规模尚未验收；游标与读取成本验证见 [Dreaming 设计](dreaming.md#全局尺度与读取成本)。

层次只向下依赖，IO 仅在 L0、L4。`HistoryReader` 不取得写租约、不创建文件，只读换行结束的完整记录；资源并发删除视为不存在。作为写入者的 Host 仍通过原 Run 门面和租约读写，不改用中枢绕开串行化。

事实键为“数据根 + 资源 id + 本资源历史位置”。投影保留主体、时间、配置、动作、裁决、任务状态和结构化观察，不带原提交的业务正文；配置清单须通过摘要校验。Run 节点槽位与已接纳观察 invocation 关联，因果标识缺失时保留缺口，不伪造关系。最新被接受评分用于当前质量比较，旧评分仍在历史。

用量统一投影为 `complete`、`legacy_complete`、`partial`、`unknown`。旧记录只有正常结束且 lifecycle 轮数与明细数严格一致时才视为明细数量完整；旧记录中已写成零的缺字段无法追溯恢复。费用/token 比较按对应字段的完整覆盖筛选，质量评分仍保留；不将无模型观察的调用补成零成本，也不补造历史缺失的轮次。逐轮分布明确标为有界样本，不代表全部轮次。

`agent_ms` 只在所需模型 attempt 的生命周期均已记录时给出完整时间，`observed_agent_ms` 保留已知部分；普通 Op 和宿主等待观察不当作模型工作。64 轮明细截断不影响完整 lifecycle 起止时间，已记录的失败/中断时间也不被抹去。若首个已保存模型 attempt 已标记恢复，则此前成本与时间未知；现有恢复布尔值仍不足以发现任意中间 attempt 的记录丢失。

另有**可选调用明细**：L4 可从获授权路径下的 `goose-acp/*.evidence.json` 读取参数与结果的有界头部，供 `calls` 下钻。明细不混进权威事实行，不影响缺明细时的事实读取；它可能含业务值，不能把“事实投影不带业务正文”扩大为全部 MCP 输出均无业务内容。数据目录和证据键也可能暴露本机路径，授权与分发须按实际读取范围处理。

### 分析器与信号

每个分析器声明 id、版本、所需字段和批量/跟读方式。输出含 `not_applicable`、测量摘要和引用事实键的 Findings；字段不足明确报告不适用，不猜测。当前接口包括：

| 分析 | 用途与限制 |
| --- | --- |
| `health`、`stall` | 记录/用量完整性、质量检查、工具/命令失败、规则触发、活动未知、启动空档和慢调用；报告不直接控制 Run |
| `profile` | 分任务或 Graph 展示成本分布、轮数、上下文增长、模型/工具耗时及高低成本差异，不把测量写成因果结论 |
| `contrast` | 按配置字段分组比较评分、Agent 时间、token、轮数和工具失败，默认 `host_sha256`；跨任务按对照归一化，统计比较不代替受控输入 |
| `gate` | 对受控实验做质量非劣判定并保留任务级损失检查；只输出判定，不自动采纳变体或写配置 |
| `signals` | 将问题和正向范例压成有界表，供 Agent 选择后下钻，而不是读取全部运行史 |

`signals` 按类型、位置与 detail 形态合并发现，不把不同失败类别混为一条；信号有稳定 id、权重、受影响范围、配置/Graph/节点/模型/工具/Host/环境坐标和证据计数，每条最多引用 5 个事实键。信号和范例默认各保留 20 条，未展示项按类型计入 `omitted`。

`stall` 版本 2 将旧 `stalled_task` 改为 `idle_task`：长期无记录只说明活动未知，不证明卡住。执行中 Run 若有尚无节点结果、最后已接纳状态为 `input_wait=true` 的接收器，则计入 `waiting_input_tasks`，不报该 Run 的 `idle_task`；节点结果优先于遗留等待状态。最近已接纳观察也推进活动时钟。没有新等待事实的旧 Run 不按节点名猜测；等待事实不是进程存活心跳，也不证明并行分支全部健康。模型/工具时间缺失、明细省略或工具事实丢弃时，不把未覆盖区间诊断为已证空闲；MCP 标明时间线完整性与 gap 的观察范围。

`health`、`profile`、`contrast`、`signals` 的上述完整性/可空成本语义对应输出版本 2；`gate` 保留版本 1 的质量判定，不因成本未知删除质量样本，也不据此宣告节省。

评分任务的 Harness 前沿比较平均质量、token、Agent 时间，至少 3 个 Run 才参与；支配需三个维度均不差，且质量优势超过 0.05 或成本差异通过统计检验。它不保证方差和尾部更好。`frugal_route`、`rare_route` 是可调查的低成本或少见路线，不等于已经发现可推广的优化。工具路线按调用顺序组装并合并连续重复；同形状换值后恢复、路径类别、工具失败和命令非零退出分别解释，旧记录缺字段不能倒推具体参数。

### CLI 与只读 MCP

CLI 提供 `health`、`profile`、`stall`、`contrast`、`gate`、`signals`、`rows`、`mcp` 和 `tool`；路径可指向内核数据根或含这些根的授权目录。`stall --follow SECONDS` 持续跟读，`tool NAME --args JSON` 复用同一 MCP 查询。具体参数以 [`adapters`](../rust/anchor-hub/src/adapters.rs) 为准，不在此复制构建与实验脚本。

`anchor-hub mcp PATH...` 在 stdio 上逐行处理 JSON-RPC 2.0，支持 `initialize`、`ping`、`tools/list`、`tools/call`。Plugin 使用 `command` 和 `args: ["mcp", "<数据目录>"]` 接入；不因声明只读就自行获得目录或网络授权。

| 工具 | 读取内容 |
| --- | --- |
| `signals` | 有界信号、前沿与范例 |
| `report` | `health`、`profile`、`stall`、`contrast` 或 `gate` 报告，可按 code、subject 筛选 |
| `invocations`、`invocation` | 按 Run、节点、配置、结局、工具、任务筛选调用摘要，或读取某次调用的生命周期、轮次、工具与证据 |
| `configurations`、`records` | 按摘要查配置及环境描述，或按事实键读取结构化行 |
| `calls` | 按调用、工具、程序或结局读取可选明细，可分组看相同失败的范围与实例 |
| `check` | 对本服务只读查询做确定性断言，返回通过/失败/错误；不写评分或提议事实 |

每次工具调用先增量刷新，Host 运行时也可读取。列表默认每页 20、最多 200 项，报告 `total` 与后续页位置；单 invocation 明细也截断并报告总数。工具标为只读；业务调用错误以 `isError` 返回，不因此退出服务。工具 schema 与返回值以 [`mcp`](../rust/anchor-hub/src/mcp.rs) 为准。

依赖、只读和层次边界由 [`anchor-hub` 围栏](../rust/anchor-hub/tests/fences.rs)守住，观察写入仅归 Host 记录门面，评分写入仅归 [`anchor-eval`](../rust/anchor-eval/tests/fences.rs)。不得为新分析器打开旁路写入。

## RSI 与数据权限

用户已决定：**中枢不并入 `anchor-rsi`**。中枢是 Harness 事实的分析基础设施，`anchor-rsi` 提供代码、文档、生态等业务审查能力，RSI Graph 是使用方；替代旧 RSI 自行分析运行数据的路径仍是后续集成方向，不能写成已经全部切换。

[`rsi-signals`](../examples/graphs/rsi-signals.json) 已通过独立的[只读 Plugin](../examples/rust-rsi/plugins/harness-hub/README.md)使用中枢：triage 选择信号，调查槽和 explore 并行，综合后独立评审，gate 决定发布报告或返回修订。原 [`rsi`](rsi.md) 保留；信号变体没有自动改代码、Graph、Plugin 或部署的权限。

信号表有界不等于 Agent 总读取量有界。当前数据目录同时可作为节点只读输入，shell 可直接读取原始目录；上游 Artifact 与 MCP 查询没有按单条信号强制隔离。“一支一条”是注意力约定，不是访问控制，不能将 `reads` 或少量 MCP 调用数冒充全部读取范围。收紧数据挂载、改变访问契约或开展线上实验均须在对应任务内明确授权。

## 未实施方向与待定政策

以下是已讨论方向，不是现成接口或自动执行许可：

- **Dreaming 闭环**：预授权策略、实验分配、变更账本、机器发布及生产撤回尚未形成当前写路径。采用完整平台默认自带的普通 Dreaming Graph 使用只读分析，由 Host 的受限应用边界接入独立验收，经 `Kernel::submit` 裁决后自动落地，并以同一路径改进自身；人不逐项批准。出厂初始化、Graph/工具、实验、版本发布及验收顺序统一见 [Dreaming 实施方案](dreaming.md)，不再以给 Hub 增加 `ProposalSink` 或 HTTP 写接口为方向；Hub 保持只读。
- **分析扩展**：完整流程/序列模式挖掘、轨迹对齐、新颖性检测和反事实重放仍须按实际能力逐项建设。多臂老虎机、贝叶斯优化、进化搜索和信用分配需要受控实验及足够样本，不以已有路线摘要或少量真实运行代替验收。
- **事实范围**：Graph 保存、Plugin 安装和 Session 投递尚未统一纳入 Kernel 提交；Pilot/Op、调用中途事实、原始响应引用及实验分配各有缺口。扩展要复用事实所有者与既有提交契约，不另建一套 Agent 记忆或恢复日志。
- **保留政策**：history、`points.jsonl` 与内容库的保留策略仍待用户决定，见[待决清单](../experiments/constitution-host-preflight/final/measurement.md#迁移后待决清单)。可丢弃投影不授权删除权威历史；不静默新增保留期限、旧数据迁移或清理规则。

## 验收边界与证据

完整用量、可空字段与输入等待观察已通过本地契约回归；固定 Goose＋本地确定性 Provider 验证了成功与取消时的用量落盘，超过 64 轮及恢复计数由确定性单测覆盖。同源历史复核保留旧缺失轮次和未知成本，未补造历史观察。2026-11-06 配套记录器与读侧已部署：六个原助手在同 Run 恢复等待，各新增一条 accepted `input_wait=true`，Hub v2 只读核对 `waiting_input_tasks=6`，旧历史前缀不变；本轮没有新增模型/工具观察，不作为真实 Provider、新用量记录器业务效果或长期告警效果验收。当前结论见[验收台账](pilot-development-plan.md)，原始结果见[本地验证](../.local/rsi-optimization-20261106-113634/verification.md)与[上线核验](../.local/rsi-rollout-20261106-140628/evidence/verification.json)。

已有记录、读侧、评分和受控实验结论分别见 [P1](../experiments/harness-hub/p1/verification.md)、[P2](../experiments/harness-hub/p2/report.md)、[P3](../experiments/harness-hub/p3/report.md)、[P4](../experiments/harness-hub/p4/report.md) 与[实验 1](../experiments/harness-hub/p6/experiment-1.md)；噪声与样本量依据见 [P0](../experiments/harness-hub/p0/report.md)。这些是各自阶段证据，不是当前所有接口、新构建或全部业务的联合验收。

P7 首次真实 Flash 盲测命中 **2/3，未通过内容验收**；评审要求修订，原 Run 因 gate 解释器失败未发布。解释器修正与无模型回放不补签真实模型完整流程，详见 [P7 报告](../experiments/harness-hub/p7/report.md)。**G4 规模门槛和 G5 下一轮闭环均未通过**：缺同输入旧图的成本/质量对照，也没有提议账本及下一轮预测判定事实。后续状态与关闭条件只维护在[验收台账](pilot-development-plan.md)，不以反复盲测调到答案代替验证。
