# Harness 数据中枢

本文维护数据中枢的原则、记录、只读分析及授权冻结窗口。稳定配置、安装级关联域、质量覆盖、安全 Run/Grade 视图格式及读侧处理、跨周期统计保留，不表示 Host/Eval 仍提供安全视图导出；分析进程不获得发布权。Dreaming 专用系统按用户要求从源码删除，其专用 meta 及依赖 `kernel-dreaming` 生命周期的实验协议一并删除，不保留内置周期或自动改进接入，也不建立替代系统，见[退役说明](dreaming.md)。原[中枢联合验收](../experiments/harness-hub-v3/README.md)只代表当时结果；本轮删除后的验证及仍未覆盖的边界见[验收台账](pilot-development-plan.md)。

## 目标与已确认原则

目标是利用运行事实，找到更好的 Harness、Graph、Plugin 组织与组合，识别 Agent 行为、罕见但有效的路线、编排瓶颈和架构删减候选。专门的分析工具和算法是主力，大模型是工具的使用方之一，不以让模型通读全部原始数据代替分析接口。分析、独立评分与实际修改分别归对应事实所有者；现有接口不自动实施建议、发布或撤回资产。

用户已确认的原则继续有效：

1. **质量优先。** 同时衡量任务质量、token 与费用、时间、工具效率。人工介入、未知效果、迟到结果、冲突和停滞也是信号；流程成功、质量通过和长期收益分别判断。
2. **事实优先。** 身份、成本、时间和配置由宿主或内核盖章，不采用模型自报数字。模型对质量的判断须标来源，不能冒充权威评分；遗漏的事实以后无法补回。
3. **统一提交，提议与裁决分离。** 观察与评分复用 `Kernel::submit` 和同一种 Record，不另造通用日志或 Agent loop。内核记录时间、配置与内容，不新增基于时间、token 的裁决；业务语义由对应 Validator 处理。
4. **历史是权威，投影可重建。** `points.jsonl` 是有损摘要，不能代替历史归因到具体 Graph 或 Plugin。中枢不能直接改数据、Graph、Plugin 或配置；改进必须经过宿主的提交与裁决路径。
5. **不设默认预算。** 模型正常工作就继续，保留内核已有的预算结算规则。停滞报告不自动停止任务；是否新增自动停止策略仍须另行决定。
6. **只读与可插拔从边界保证。** 没有安装分析中枢时仍记录事实；分析工具可独立替换、卸载，不能反向成为 Host 或内核的依赖。
7. **真实 Provider、外部渠道实网和目标部署验收单独授权。** 本地回归和历史样本不补签这些结果；已授权实验也不自动授权生产切换或扩大权限。发布仍归 Graph/Library 所有者，不归 Hub。

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

共享词汇见 [`anchor-facts`](../rust/anchor-facts/src/lib.rs)：Observation/Grade 内容版本仍为 1，稳定配置身份单独升级为 v2，不整体抬高观察/评分版本：

| 资源 | 内容与约束 |
| --- | --- |
| `trace/{run}/{node}/{invocation}` | `Observation { version, invocation, facts }`；`validator/observation` 校验 invocation 归属，`RecorderOnly` 仅允许 `anchor/recorder` 以宿主代码身份 Write |
| `config/{digest}` | 不可变配置清单，`validator/configuration` 守住内容寻址；配置身份不同不改写旧清单 |
| `grade/{run}` | `Grade { version, run_id, task, task_version, checks: [{ name, passed }] }`；检查项名字唯一、不含业务值，重新评分追加历史 |

观察包含以下事实：

- `request_context`：每次 attempt 的 task/input/已选指导摘要，保留本次请求可追踪性，不进入稳定配置身份，也不声称包含 Goose 全部会话。
- `lifecycle`：调用开始、结束、结局、模型轮数、工具次数、丢弃计数和是否恢复。
- `model_usage`：本次 prompt 已观察到的完整用量汇总、prompt 是否正常结束、各字段报告轮数，以及明细保留/省略轮数。汇总不限于前 64 轮，不混入 Goose 原生 Session 的跨调用累计数；字段只报告一部分时，数值是已报告部分，不代表总成本。
- `model_turn`：最多前 64 轮的输入、输出、缓存 token、费用及可用的耗时、首 token 时间、消息标识；缺字段为 `null`，不是零。用量报告的观察时间晚于该轮工具执行，不是模型响应结束时间；时间线计算不应把工具耗时重复算成模型耗时。
- `tool_call`：工具名、参数形状摘要、调用标识、模型请求序号、时间、成功与失败类别；可选的路径类别和命令程序名不保存完整路径或 argv。`shell_program` 只是 shell 首个命令的启发式归因，不替换原程序名；工具调用成功不代表内部命令零退出，另看 `CommandEnd`。
- `workspace_fingerprint`：Goose invocation 开始、结束时的 git HEAD、dirty 与已知锁文件相对路径和摘要。在同一命令沙箱中采集，不借宿主执行仓库配置；采集失败或非 git 现场省略，不影响调用。
- `input_wait`：已授权 `session.wait_input` 接收器进入/退出等待的宿主观察，只含 `waiting`；时间由 Kernel 盖章，不含用户或消息内容，也不伪造 Agent/模型配置。

以上为当前版本 1 词汇的扩展；新工具读取旧事实，无需改写历史。旧工具不保证理解新事实种类或可空字段，部署时须使用配套记录器与读侧。

`AgentConfiguration` v2 绑定 Graph、节点、模型及绑定、Goose、静态指令、Plugin 身份/说明、路由、Host/工具定义与冻结执行环境；Plugin 和路由等集合规范排序。`instructions_sha256` 只取结构化静态指导，不从拼接后的 task 文本删除输入；本次输入、上游产物与动态路径不进入稳定分组。环境描述不含凭据值或 Run/Workspace 来源路径，具体沙箱边界仍由[当前架构](architecture.md)定义。旧 v1 清单保持原摘要和历史，不尝试拆解或并入 v2；它仍可用于有效用量观察，但稳定配置比较明确标为旧身份。相同配置跨输入积累样本不等于不同任务、评分器和输入群体可直接比较。

`validator/grade` 不接受 Agent 评分，既不能评自己也不能评别人；当前 `GraderOnly` 写路径只允许 `anchor/grader` 的宿主代码身份。未来人工评分可沿用评分资源，但不表示已有人工入口。`anchor-eval record --state DIR --config DIGEST` 将输入评分记到评分方自己的 `kernel-grades`，持独立写入租约；评分配置摘要标识任务、期望与评分代码。评测 Graph 固定为准备 → 被测节点 → 评分，被测变体不能修改评分器。

### 独立质量工具

独立 ordinary/input-discovery 质量工具保留。`anchor-eval grade-run` 调用 `Evaluator::grade_ordinary`，从明确配置的已完成普通 Run、不可变 Artifact 与受保护套件检查结果，经 `GradeRecorder` 和 `Kernel::submit` 记录到独立 `kernel-grades`；它核对实际 Run 元数据，不接受历史 `dreaming`/`experiment` 身份或已退役的 `dreaming` 套件。`record` 仍只负责提交已有评分，不代替独立检查；具体参数以工具当前 CLI 为准。它们不依赖 Dreaming 周期、候选或自递归，不恢复专用实验执行或自动发布协议，也不允许被测 Agent 修改受保护质量口径。

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

磁盘 `DataRoot` 复用 `HistoryReader` 物理游标，只解析新增完整历史行；首次冷读仍完整校验，刷新仍枚举来源/资源、读身份与校验锚点。热刷新按已成功读取的现场撤除删除/替换/截断的资源、整根、游标及调用明细，并保留其他有效来源共享的内容寻址配置；失败刷新不发布半份新投影。可重建缓存不拥有历史，也不通过定期重启掩盖热/冷差异。冻结 v3 的历史关联另行重读保留前缀，`coverage.io` 与 `support_io` 分别计量，不能把“新增行解析为零”解释为零 IO 或总成本 O(增量)。

Hub 有界窗口重放保留已校验完整前缀的复用：物理来源开启历史支撑时，以本次严格重放读出的前缀构建投影，不另读第二遍。完整前缀摘要、snapshot、来源身份和旧格式边界仍校验；主读取仍计入 `io`，没有额外读取时 `support_io` 为零，不代表全轮零 IO。此路径未引入新缓存或存储，仍需枚举和重建全前缀投影；未重测 10k 性能，不改变后述规模限制。原收口证据（本机原件，未入库：`experiments/dreaming-context-branch/global-coverage-20261115/README.md`）同时包含当时 Run/Grade 所有者导出的优化；该专用导出链现已删除，只保留历史证据，不补签本轮回归。

层次只向下依赖，IO 仅在 L0、L4。`HistoryReader` 不取得写租约、不创建文件，只读换行结束的完整记录；资源并发删除视为不存在。作为写入者的 Host 仍通过原 Run 门面和租约读写，不改用中枢绕开串行化。

事实键为“来源根 + 资源 id + 本资源历史位置”；`source_instance`/物理指纹用于授权与准确取证，安装级 `join_namespace` 用于关联，二者不能混为一谈。Run 键为 `(namespace, run_id)`，invocation 再带 node/invocation。同一 Host 的 Run/trace/Grade 可跨其已登记根关联；独立安装的同名 ID 不混算。CLI 识别明确 Host 布局或显式类型化映射，任意独立根默认隔离。排除、普通抽样、details、分析器和 MCP 共用关联键，裸 ID 有歧义时要求范围。

投影保留主体、时间、配置、动作、裁决、任务状态和结构化观察，不带原提交业务正文；配置须通过摘要校验。Grade 仅在同域、同 Run、同任务版本和评分器语义内选同一权威历史的最新 accepted 位置；跨评分根冲突和 invocation 的 Graph 身份冲突明确报告，不以遍历顺序或跨根时间戳选择胜者，也不进入优劣前沿。旧评分留在历史，缺因果标识不伪造关系。

用量统一投影为 `complete`、`legacy_complete`、`partial`、`unknown`。旧记录只有正常结束且 lifecycle 轮数与明细数严格一致时才视为明细数量完整；旧记录中已写成零的缺字段无法追溯恢复。费用/token 比较按对应字段的完整覆盖筛选，质量评分仍保留；不将无模型观察的调用补成零成本，也不补造历史缺失的轮次。逐轮分布明确标为有界样本，不代表全部轮次。

中立 `anchor-facts::usage` 的用量汇总保留，按明确选定的 Run 去重，并核对 Agent invocation 的可信身份、attempt 与正常结束覆盖：缺观察、首条已标恢复、未完成或完成请求数不一致时正式总量为 null，已观察部分单列，费用未知不补零。普通业务基线、实验和历史 meta 的消耗不能混作同一个分母；工具调用次数也不等于独立证据键或全局调查覆盖。原 Dreaming close 的周期编排不再是当前产品路径。

`agent_ms` 只在所需模型 attempt 的生命周期均已记录时给出完整时间，`observed_agent_ms` 保留已知部分；普通 Op 和宿主等待观察不当作模型工作。64 轮明细截断不影响完整 lifecycle 起止时间，已记录的失败/中断时间也不被抹去。若首个已保存模型 attempt 已标记恢复，则此前成本与时间未知；现有恢复布尔值仍不足以发现任意中间 attempt 的记录丢失。

另有**可选调用明细**：L4 可从获授权路径下的 `goose-acp/*.evidence.json` 读取参数与结果的有界头部，供 `calls` 下钻。明细不混进权威事实行，不影响缺明细时的事实读取；它可能含业务值，不能把“事实投影不带业务正文”扩大为全部 MCP 输出均无业务内容。数据目录和证据键也可能暴露本机路径，授权与分发须按实际读取范围处理。

### 授权冻结窗口

`anchor-hub window` 与 `window-evidence` 保留为独立只读命令协议，与上述路径发现/MCP 查询分开。stdin/stdout 各接收/返回一个严格 JSON，拒绝尾随 JSON 或非空垃圾，输入与输出各限 64 MiB；共享类型归 [`anchor-facts::window`](../rust/anchor-facts/src/window.rs)。`window` 使用 `WindowRequest/WindowResponse`；`window-evidence` 使用 `EvidenceRequest/EvidenceResponse` 并要求原 manifest。两者复用同一冻结加载/投影路径，只读请求明确给出的结构化根，不递归发现 Run/评分根，不加载 Goose 调用明细，也不取得写租约或创建文件。

- **先授权再投影**：每个来源使用 opaque `source_instance` 与明确 scope。`resources` 是准确资源 ID 集合，空集合不读取任何资源正文；`all` 才显式覆盖该结构化根。过滤发生在资源读取、投影和配置关联之前；未授权配置不会随 trace 被隐式读入。根枚举仍会检查目录项与链接元数据，不承诺完全不触碰范围外元数据。
- **绑定冻结前缀**：当前 manifest v3 绑定分析器 v5、时钟、列表 limit、安装关联域、来源物理身份、scope、每资源 `after/through`、物理游标、历史前缀、重建状态和安全视图摘要；不含宿主路径。`advance_from` 从已关闭窗口推进，重放使用原 `manifest`，不能混用两者。状态从冻结前缀的权威 `Record.after`/保留状态重建，不以当前 resource.json 冒充历史状态。旧 v1/v2 窗口保留原身份与准确只读重放，不补装 v3 关联域/视图/纵向语义；新捕获写 v3，升级分析域须明确重新建基。本机保留的旧窗口引用全部退休后删除对应读取器。
- **重放不漂移**：原 manifest 排除后续新增资源/记录/评分，使用同一时钟与前缀输出。来源替换、授权范围改变、前缀编辑/截断/删除、缺失权威快照、不可读或不支持版本明确失败。游标绑定同一历史文件实例、偏移及校验锚点，只有未换行尾部可以暂不纳入；失效不冒充连续新增历史。
- **按冻结键取证**：每次最多接受 16 个唯一 `{root,resource,place}`，root 为请求授权的 opaque 来源 ID，坐标须落在原 manifest 内且确实被投影。追加后只返回原前缀里的既有结构化 `table::Row`，不返回原 Record/submission/after 或 Goose 内容；不存在、未投影或范围外的键明确失败。响应绑定原 manifest 摘要；调用方须核对字段闭集、类型、嵌套结构及准确键集合，不能将只读协议当作任意路径读取授权。
- **排除规则一致**：`exclude_runs` 同时约束物理事实和 Run/Grade 安全视图的分析投影、历史支撑及取证；限定关联域的键只排除该域，跨域重名的非限定键明确失败。过滤不改写已冻结视图、manifest 摘要或连续性身份。
- **覆盖不混单位**：coverage 区分 sources、seen_resources、seen_records、included_in_summary、investigated、deferred、unavailable；输入资源/记录数与摘要信号数不能相加为一个覆盖率。冻结摘要的 investigated 为 0；它不会因后来下钻而更新，成功捕获的 unavailable 为 0 不代表生产健康。信号/范例列表受 limit 约束，遗漏单列；既有评分 front 仍可能增长，超过传输上限明确失败，不截断后宣称完整。

安全视图保留的是 [`anchor-facts::view`](../rust/anchor-facts/src/view.rs) 的 `SafeRunView`、`SafeGradeView`、`SafeViewSnapshot` 格式与 Hub 消费端。窗口 CLI 经 `adapters::run` 调用 `window::{capture,signal_page,evidence}`，共用 `load_window`，将调用方明确提供的视图经 `table::project_safe_view` 投影后交给 `Query` 和分析器。这条链不调用 Host 或 Eval 生成视图。

Host 原 `safe_run_views` / `verify_safe_run_prefix`、`application/run_access/discovery.rs` 和 `application/safe_views.rs` 随专用系统删除，没有替代导出 API。当前 Eval 只保留上述 `record` 与 `grade-run` 写侧入口，不提供 safe-grade 导出。保留 DTO 和分析格式不等于仍有 Run/Grade 所有者导出链。

调用方须逐类明确授权观察根及 `runs-view-*`、`grades-view-*`，facts-only 读取权不隐含其他来源；已声明的安全视图缺失时窗口明确失败。安全视图分支只投影白名单字段，不打开原 Run/Grade 所有者数据根，无 input/result/prompt/异常正文或 Record.after；普通路径发现及显式物理来源读取仍遵守各自授权，不由视图格式授予。

安全视图的字段闭集、摘要、事实位置和来源/范围绑定由共享类型校验；推进通过 `continues` 核对旧资源位置与已保留条目，重放及取证使用原 manifest 内的视图。owner 根指纹和历史摘要是视图携带的声明，Hub 不回读原所有者根来证明其当前历史仍一致。显式物理来源另经 `HistoryReader` 校验根身份、游标和准确前缀；不能把这一保证扩大到安全视图背后的原根，也不存在跨根原子快照。

来源格式仍表达 `Production`、`Experiment`、`DreamingMeta`、`Unknown`，供保留历史的分层分析；明确标为实验/meta 的视图不进入普通发现与评分前沿。物理事实默认没有可信来源标签，分类缺失或冲突保持 `Unknown`，不能当作生产身份。Hub 不再读取旧 control/oracle 来验证 Development/Holdout，不能宣称仍自动执行原导出的逐项实验筛选或纠正误分类；视图摘要有效也不证明来源标签可信。

原 `dreaming`/`experiment` Run 元数据和已有历史回执只用于查证及来源隔离，不再作为新实验的生命周期协议。已知的历史实验/meta 不得重标为普通或生产样本；缺少可信分类时须披露未知，不补造生产来源。发现误分类的旧视图不能作为可信生产分析依据，也不改写原 manifest。历史可读不授予普通任务续跑、普通重新评分或恢复自递归的权限。

窗口捕获、重放和取证由调用方显式提供获授权来源与原 manifest，不再由 Host 启动 Dreaming 握手、周期协调或专用 `dreaming_signals/dreaming_evidence` 工具。保留的只读窗口接口不拥有周期、策略、待办或自动发布事实。

冻结分析器 v5 保留 v4 的稳定普通抽样规则：探索列表一半名额按 invocation 身份 SHA 排序，与质量、费用和失败分类无关。样本保存规则、序号、身份摘要、总体规模与准确证据坐标；这是发现入口，不是质量或缺陷结论，不另建抽样存储。v5 先选当前增量证据再限制每条 5 个引用；历史支撑仍参与统计与排名，内部 Health 汇总不再受公开报告 2000 个引用上限截断。旧 v4 窗口及持久分页 offset 继续按旧算法重放，新捕获/推进才使用 v5，不回写旧结果。旧窗口若命中曾遗漏的安全视图排除规则，则明确失效，不重现泄漏，也不悄悄改变旧分页顺序。

退役前的 100/1k/10k 多源、多周期基准与修复前基线分开保存。10k 的 322,500 条事实在冷读/空增量/+100/+1 时，新增解析为 322500/0/100/1，但空增量的历史支撑仍重读 322,500 行、354,883,936 字节；对应 Hub 子进程约 18.336/51.948/62.302/39.729 秒，峰值约 2.54 GiB。最大模型摘要 15,838 字节，请求/响应约 49.7/38.2 MB，均在当时边界内；这只证明该稀疏评分 fixture，不保证任意万图长历史都能装入协议，也不是删除后的重测。

退役前的独立 Host 所有者导出测试对 10k 个真实 Run/Grade 各一条事实，导出约 4.128 秒、请求 26,143,751 字节、子进程峰值 227,300 KiB。这是已删除导出链的历史测量，不表示当前仍可导出；它与多源 Hub fixture 不同，不能相加为端到端耗时。额外分项测量区分投影、查询、序列化及 facts-only 热刷新，注明已有驻留数据、OS 页缓存和未测物理 IO/净分配；没有新增缓存权威或掩盖全前缀重建。原 8 KiB 整体摘要断言的失败保留，保留格式要求旧核心 ≤8 KiB、纵向部分 ≤16 KiB、整体 ≤256 KiB；当时核对的三档字段和列表上限、准确命令、身份、失败和分项测量见[联合规模证据](../experiments/harness-hub-v3/README.md)，不补签本轮验证。

### 覆盖与跨周期比较

冻结窗口的 `applicability` 分别描述 Run、观察、Grade 和稳定配置。Run/观察/Grade 来源未获授权为 `not_granted`，已授权安全视图的 owner 缺失为 `missing_source`，范围内无记录为 `empty_in_scope`，并区分 `no_matching_grades`、`partial_coverage`、`conflict`、`analyzable`；稳定配置另报告旧身份与内容缺失。普通 CLI 不知道 Host 授权或配置来源关系时保留 `not_authorized_or_missing_source`。不可读或前缀损坏使准确捕获失败，不输出“部分成功且健康”的窗口。分母未知为 null；无 Grade 不把执行成功当质量，缺 token/费用不删除质量样本，也不补零成本。

v3 `support_history` 在当前冻结上界内重建历史支撑，`baseline_at_ms` 绑定前次关闭窗口时钟。比较区间为基线 `[0, baseline_at_ms)` 与观察 `[baseline_at_ms, now_ms)`，不是等长滑窗。Run 结局/质量按结算时间、未结按创建时间分桶；效率按 invocation 开始时间分桶，长寿命 Run 的新 invocation 不错误归入首轮。迟到 Grade/重评分按同一权威历史更新当前查询，保留缺失、冲突与 `revised_prior_interval`；原窗口不被回写。旧支撑证据不能借当前 delta 取证授权读取，仍须准确原 manifest。

`longitudinal` 输出分布/尾部、错误族、任务版本/评分器/配置与来源类别分层、组成变化及未知暴露，列表受限且总数单列。统计只使用实际记录的 Graph、配置与来源身份，不内置 Dreaming 的生产部署观察或变更归属协议；指导、模型及 Plugin 的实际暴露缺事实时保持未知，不以部署时间代替使用身份。成本字段覆盖不完整则不输出节省；`observational_only=true` 明确所有前后统计只是线索，不是因果结论或发布许可。历史支撑不因再次参与统计而成为新增证据；这不是宿主工作队列或自动调查服务。

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

`health`、`profile`、`contrast` 的完整性/可空成本语义对应输出版本 2，`signals` 当前为 5；探索用 `gate` 保留版本 1，只是只读分析查询，不自动采纳或发布。依赖 `kernel-dreaming` 生命周期的专用 Eval 实验协议及 `anchor-hub experiment-gate` 随系统删除，不再作为当前 CLI 能力或后续待办；独立 ordinary/input-discovery 质量工具不继承其实验或自递归权限。

评分任务的 Harness 前沿比较平均质量、token、Agent 时间，至少 3 个 Run 才参与；支配需三个维度均不差，且质量优势超过 0.05 或成本差异通过统计检验。冲突和旧配置身份不能被静默合并。它不保证方差和尾部更好。`frugal_route`、`rare_route` 只是可调查路线。工具信号使用 `same_tool_after_failure`、`consecutive_same_shape_failures` 描述弱线索；同工具或同参数形状不证明同一操作、重试或因果修复，同一 request 的并行调用不按列表次序推断纠正。工具失败与真实命令非零退出分别解释，旧记录缺字段不能倒推具体参数。

### CLI 与只读 MCP

CLI 提供 `health`、`profile`、`stall`、`contrast`、`gate`、`signals`、`rows`、`mcp`、`tool`、`window` 和 `window-evidence`。前述常规查询路径可指向内核数据根或含这些根的授权目录；两个窗口命令只接收上述显式来源协议。`stall --follow SECONDS` 持续跟读，`tool NAME --args JSON` 复用同一 MCP 查询。具体参数以 [`adapters`](../rust/anchor-hub/src/adapters.rs) 为准，不在此复制构建与实验脚本。

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

用户已决定：**中枢不并入 `anchor-rsi`**。中枢是 Harness 事实的分析基础设施，`anchor-rsi` 提供代码、文档、生态等业务审查能力；普通 Graph/Plugin 可在授权范围内使用只读接口，不附带调查 Graph、自动改进编排或新的部署权限。

旧 `rsi-signals` 产品示例不恢复，原内置 Dreaming 的仅报告模式随专用系统退役。原 `rsi` 的源码/生态审查与普通[只读 MCP Plugin 示例](../examples/rust-rsi/plugins/harness-hub/README.md)保留，现有安装与历史 Run 不在本轮迁移或删除，见 [RSI 历史边界](rsi.md#信号驱动的变体)。

窗口请求、逻辑来源/范围、取证坐标及分页请求共用 `anchor-facts::window` 校验；Hub 的 `table::{Key,Body,Row}` 使用中立 `anchor-facts::projection`，不复制字段清单。结构化 Row 的 wire 形态不变，仍不包含原始提交正文。物理历史校验属于 Hub；调用方负责来源授权及传入视图的可信性，共享格式与读侧校验不构成现成的 Host/Eval 导出链。本轮删除专用接入，不删除这些共享词汇或放宽读取范围。原验证见收敛记录（本机原件，未入库：`experiments/dreaming-consolidation/README.md`），不补签为本轮回归。

信号表有界不等于 Agent 总读取量有界。当前数据目录同时可作为节点只读输入，shell 可直接读取原始目录；上游 Artifact 与 MCP 查询没有按单条信号强制隔离。“一支一条”是注意力约定，不是访问控制，不能将 `reads` 或少量 MCP 调用数冒充全部读取范围。收紧数据挂载、改变访问契约或开展线上实验均须在对应任务内明确授权。

## 未实施方向与待定政策

以下是已讨论方向，不是现成接口或自动执行许可：

- **持续净收益**：只读分析、受控比较和独立 Grade 不证明长期生产净改善，也不授予自动发布/撤回权限。Dreaming 专用系统退役不改变历史无收益或证据不足的结论；不通过增加 Hub 写接口建设替代闭环。
- **分析扩展**：完整流程/序列模式挖掘、轨迹对齐、新颖性检测和反事实重放仍须按实际能力逐项建设。多臂老虎机、贝叶斯优化、进化搜索和信用分配需要受控实验及足够样本，不以已有路线摘要或少量真实运行代替验收。
- **事实范围**：Graph 保存、Plugin 安装和 Session 投递尚未统一纳入 Kernel 提交；Pilot/Op、调用中途事实、原始响应引用各有缺口。扩展要复用事实所有者与既有提交契约，不另建一套 Agent 记忆或恢复日志。
- **保留政策**：history、`points.jsonl` 与内容库的保留策略仍待用户决定，见[待决清单](../experiments/constitution-host-preflight/final/measurement.md#迁移后待决清单)。可丢弃投影不授权删除权威历史；不静默新增保留期限、旧数据迁移或清理规则。

## 验收边界与证据

2026-11-13 的原联合验收中，五项审计缺陷由具名正反例关闭：稳定配置不再混入任务输入、独立安装不混算、热投影撤除已删除对象、弱工具线索不冒充修复，以及质量缺失明确披露。当时的安全 Run/Grade 接入、三周期迟到/重评分与 idle 不重复唤醒通过所有者事实及编译 Hub 验证；固定 Goose 的十输入测试实测十份请求上下文共用一个配置。结论只覆盖当时具名确定性 fixture，不表示原导出链仍可用，不估算未标注生产数据的准确率，也不补签本轮删除后的回归。

原联合证据还包含当时普通 Graph、指导槽、Plugin 的自动闭环及有限自递归测试。这些专用产品路径已退出当前契约，原确定性通过不证明真实 Provider 或持续生产收益，也不补签本轮源码删除后的构建。Pilot/Op/中途 attempt 采集等保留能力的缺口见[统一台账](pilot-development-plan.md)；原构建、命令及限制见[联合证据](../experiments/harness-hub-v3/README.md)与原生记录（本机原件，未入库：`experiments/harness-hub-v3/native.json`）。

完整用量、可空字段与输入等待观察已通过本地契约回归；固定 Goose＋本地确定性 Provider 验证了成功与取消时的用量落盘，超过 64 轮及恢复计数由确定性单测覆盖。同源历史复核保留旧缺失轮次和未知成本，未补造历史观察。2026-11-06 配套记录器与读侧已部署：六个原助手在同 Run 恢复等待，各新增一条 accepted `input_wait=true`，Hub v2 只读核对 `waiting_input_tasks=6`，旧历史前缀不变；该次部署核验没有新增模型/工具观察，不作为真实 Provider、新用量记录器业务效果或长期告警效果验收。当前结论见[验收台账](pilot-development-plan.md)，原始结果见本地验证（本机原件，未入库：`.local/rsi-optimization-20261106-113634/verification.md`）与上线核验（本机原件，未入库：`.local/rsi-rollout-20261106-140628/evidence/verification.json`）。

已有记录、读侧、评分和受控实验结论分别见 [P1](../experiments/harness-hub/p1/verification.md)、[P2](../experiments/harness-hub/p2/report.md)、[P3](../experiments/harness-hub/p3/report.md)、[P4](../experiments/harness-hub/p4/report.md) 与[实验 1](../experiments/harness-hub/p6/experiment-1.md)；噪声与样本量依据见 [P0](../experiments/harness-hub/p0/report.md)。这些是各自阶段证据，不是当前所有接口、新构建或全部业务的联合验收。

P7 首次真实 Flash 盲测命中 **2/3，未通过内容验收**；评审要求修订，原 Run 因 gate 解释器失败未发布。解释器修正与无模型回放不补签真实模型完整流程，详见 [P7 报告](../experiments/harness-hub/p7/report.md)。**P7 当时的 G4 业务规模门槛和 G5 下一轮闭环均未通过**：缺同输入旧图的成本/质量对照，也没有提议账本及下一轮预测判定事实。该旧调查 Graph 已退出产品交付，历史未通过项不转为本轮继续实施的待办，也不以反复盲测调到答案代替验证。
