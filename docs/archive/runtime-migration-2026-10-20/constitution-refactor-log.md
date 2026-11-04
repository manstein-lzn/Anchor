# 宪法内核重构：工作记录

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

起点为分支 `constitution-core`（基于 master `fc558e2`）；M3 已快进 master，M4 起只在 `/home/mansteinl/Anchor` 工作。旧分支 worktree 只读保留。设计依据：[docs/anchor-constitution.md](../../anchor-constitution.md)；概念验证：[experiments/constitution-poc](../../../experiments/constitution-poc/README.md)。

本文记录每一刀的实际结果、数字和语义变更。它是分支内的工作记录，不是进度台账。

## 纪律

1. 测试是契约。修改既有断言必须在"语义变更"一节登记原因。
2. 每一刀结束时编译通过、测试通过。
3. 分支上不背兼容包袱；生产数据迁移在合并时单独处理。
4. 度量：同一规则的独立判断点数量、核心决策代码量、测试结果。代码行数不是目标。

## 基线

| 范围 | 结果 | 说明 |
| --- | --- | --- |
| `anchor-runtime` | 119 通过 | 未改动的 master |
| Host、GraphHost、Runtime 全部目标 | 1227 通过，12 失败，112 忽略 | 未改动的 master，`--no-fail-fast` |
| 上述 12 项失败 | 复跑后全部通过 | 环境原因：新 worktree 未构建 `anchor-sandbox-network`，网络相关测试无法启动。构建后复跑 API 测试 197 项全部通过 |
| K0 + K1 之后，Kernel、Runtime、Host、GraphHost 全部目标 | 1257 通过，0 失败，112 忽略 | 43 个测试目标。1257 = 基线 1239 项（含复跑后通过的 12 项）+ 新增 18 项（内核 17、差分测试 1） |

## 只读检查的发现

- 旧内核的核心是 `GraphRunner::run`，约 770 行的单个函数，一个循环里同时处理生命周期、选节点、拼输入、按节点类型分支、映射 9 种执行结果到 10 种 Run 状态、选路由、冻结产物、写边决定。
- 停止和暂停的检查在循环里出现 6 处。
- `GraphRunRecord` 约 20 个字段，其中约 9 个是簿记：Run 的当前状态需要从计数器和映射里拼出来。
- 内核内部结构已泄漏到内核之外：`anchor-runtime` 之外有 16 个文件读 `record.cursor`，21 个读 `invocations`，16 个读 `graph_calls`，25 个使用 `NodeCompletion`，47 个按 `RunStatus` 分支。替换内核必然牵动 Host。
- 带条件路由的数据流语义（边被判定为不走之后，如何关闭下游节点和环）属于 Graph 语言本身，不是推模型带来的复杂度。新内核不会消除它，只会把它移到 Graph 校验器（实例层）。

## K0：新内核 `anchor-kernel`

独立 crate，不依赖 Graph、模型或协议。依赖只有 serde、serde_json、thiserror。

- 六种原语：资源、提交、裁决、效果、任务、测量。
- `submit` 是唯一的改变入口：裁决、记录、结算、测量。
- 授权是端口（`Authority`），内核只询问，不实现。
- 存储由事实所有者提供（`Store`）：内存存储与文件存储。没有全局日志。文件存储先持久化记录、再替换状态；记录携带结果资源，所以"记录已写、状态未写"的崩溃窗口靠读取恢复，不重新裁决。
- 任务生命周期是内核定律：任何任务上，人可以 `stop`、`resume`、`abandon`；等待子任务或未知效果时，由宿主代码 `resume`。等待中的任务不开放空位、拒绝 Agent 的提交。这一条替代了旧 Runner 循环里的 6 处停止和暂停检查。
- 测试 17 项，覆盖 PoC E1 的全部行为，以及 PoC 没有覆盖的持久化：重启后历史完整、崩溃窗口恢复、残缺的最后一行记录被忽略、资源 id 不能逃出存储根目录。

## K1：Graph 作为 Run 的校验器

`anchor-runtime/src/graph/validator.rs`。Run 是内核的任务资源，状态是冻结的 Graph 及其进度。校验器只做两件事：`open` 说明当前接受哪个节点调用（空位 `节点#调用序号`），`judge` 应用提交的结果、选路由、判断 Run 是否结算。它不调用任何人，也不处理停止、暂停、恢复、放弃。

这一刀复用了现有 Graph 语义函数（`next_node`、`propagate_inactive_edges`、`select_route`、`refuse_module_activation`），只替换驱动方式，旧 Runner 一行未改，作为对照组。暂不支持 fanout/join 与 Op.call，`run_resource` 对它们明确拒绝。

**差分测试**：同一个随机 Graph、同一份确定性路由脚本，分别交给旧 `GraphRunner` 和"新内核 + Graph 校验器"，比较派发序列（节点、调用序号、输入产物）、最终 Graph 事实（结果、边的决定及其序号、轮数计数、模块激活、停止原因）和结束状态。

| 指标 | 结果 |
| --- | --- |
| 随机 Graph | 10,000 个，2–9 个节点，含自环、反馈环、模块作用域 |
| 一致 | 10,000 / 10,000 |
| 正常完成 / 触发轮数上限 | 7,397 / 2,603 |
| 总派发次数 / 最长单次 Run | 28,398 / 21 |
| 覆盖：模块被激活 / 模块轮数上限触发 / 存在被跳过的路径 | 1,381 / 336 / 8,977 |

差分测试在第 195 个 Graph 上发现过一个新 API 的缺陷：新侧在派发前的状态推进（关闭失效的边）之前就计算了节点输入，带入了一条已失效的边。修正为 `Dispatch` 与输入在同一次推进之后一起产生。

## K2：并行区域（fanout/join）

校验器的推进函数从"返回下一个节点"变成"返回当前开放的全部空位"：串行时一个，fanout 激活后每个未完成分支各一个，所有分支完成后是 join。并行不再是旧 Runner 里那段约 480 行的特殊执行路径（`parallel.rs`），而是"同时开放多个空位"。

- fanout 与 join 是确定性控制节点，只允许宿主代码（`Code`）提交，Agent 不能伪造一次 join。
- 多个分支同时向同一个 Run 提交时，后到者因版本过期被打回，执行者刷新版本后重新提交，按最新状态重新裁决。这不需要修改内核：版本过期本来就是可修正的裁决。
- 校验器不规定波次。旧 Runner 的"一波全部完成才开始下一波"是旧调度方式的副作用，不是 Graph 语义；新模型里是否分波由调度器决定。差分测试的调度器模拟波次，以便逐项比较。
- 暂不支持：模块作用域位于并行分支之内（`run_resource` 明确拒绝）。

**差分测试（并行）**：随机生成"前缀 → fanout → 2–3 条线性分支 → join → 后缀"，后缀带有重新进入整个区域的反馈边、绕过区域的跳跃边，所有节点有轮数上限（分支可能触发上限）。

| 指标 | 结果 |
| --- | --- |
| 随机 Graph | 5,000 个，全部通过 |
| 完成 / 触发轮数上限 / 分支触发上限导致失败 | 3,633 / 971 / 396 |
| fanout 激活次数 / 总派发次数 | 5,048 / 35,958 |
| 版本过期后重新提交 | 11,444 次 |
| 旧侧幽灵调用（S5）/ 新侧迟到被拒（S6） | 220 / 68 |

完成与轮数上限两种结局要求全部 Graph 事实逐项一致。失败结局按 S6 检查：派发序列完全一致；旧侧并入的每个结果，要么在新侧也被并入，要么是新侧历史中带产物证据的迟到被拒提交，两者不重叠、无遗漏。

## K3：Op.call

调用节点不再是旧 Runner 里约 230 行的结果矩阵，而是映射到已有的概念：

| 旧 | 新 |
| --- | --- |
| `WaitingCall` 状态 | 任务进入内核的"等待子任务" |
| 子 Run 结束后宿主重跑父 Run | 宿主代码提交 `resume`（内核定律允许宿主代码释放等待子任务的任务） |
| 端口返回的六种结果组合 | 宿主代码提交四种之一：已绑定、已完成、失败、未知 |
| 游标记住"这次调用还在进行中" | 从事实推出：已计为开始、尚无结果的调用即进行中，恢复时沿用同一序号，不重复检查轮数上限 |
| detach 不转发业务结果（Runner 构造输出） | 校验器自己构造输出，提交中的业务结果被忽略，结构上不可能泄露 |

同一个调用身份只能绑定一个子 Run，换绑被拒绝。调用节点只能由宿主代码提交。

**差分测试（调用）**：随机串行 Graph 中把一到两个节点改为 wait 或 detach 调用。旧侧使用按调用身份哈希决定结果的脚本化端口，等待中的父 Run 由宿主重新进入；新侧由宿主代码按同一哈希提交，子 Run 结束时由宿主代码释放等待。比较项在原有基础上增加了 `graph_calls` 的全部字段。

| 指标 | 结果 |
| --- | --- |
| 随机 Graph | 5,000 个，全部一致 |
| 等待后完成 / detach | 1,704 / 1,485（detach 无一泄露子 Run 业务结果） |
| 调用失败 / 接纳未知 | 497 / 383 |

差分测试在此过程中发现两个新侧缺陷，均已修正：

1. 已绑定子 Run 的调用在父 Run 恢复后被分配了新的调用序号，丢失了绑定。修正为"已开始、无结果的调用即进行中"。
2. 上述判断最初放在 `next_node` 之后；带自环的调用节点因自环边尚未判定而被 `next_node` 跳过。修正为先检查进行中的调用。

## K0.1：内核修订与中断事实

按[审查决定 D1–D5](../design-records-2026-11-04/constitution-preflight-decision.md)完成限定范围实现；`88a6e41` 的[独立验收](../../../experiments/constitution-host-preflight/k01/review.md)九项通过，未发现阻塞问题。**当前状态：H1 已验收，下一步 H2**。K0.1 切片当时未修改 Host、旧 Runner 和现有内核测试断言。

- **D1**：选择从历史求和；两种 Store 的 `load` 共用成本投影，`spent` 为所有记录 `stamp.cost` 的饱和和，不再依赖版本化快照。打回、拒绝、版本过期、待定及已结算后的拒绝都计费，计费本身不推进版本。版本过期后以 `cost=0` 重提同一工作；超预算仍立即结算失败，结算推进版本。待定批准写入目标任务时的观察记录也遵守预算。
- **D2**：换行闭合才是完整记录；追加前截去尾部未闭合字节，保留完整记录原始字节。任何闭合坏行（包括末尾坏行、空行和无效 UTF-8）均报 `Corrupt`；末尾合法 JSON 若没有换行，也不算已提交记录。崩溃窗口仍从完整记录恢复，不重新执行。
- **D3 / D5**：Code 的 stop 必须有 Observation；人的等待只能由人 resume。等待中所有身份的非生命周期提交由内核拒绝并计费。等待中的非生命周期提交统一返回 `TaskWaiting`；若该次成本同时超预算，D1 仍将任务结算为预算失败，不应用节点内容。
- **D4**：可选 `NodeSubmission.interruption` 仅 Code 可携带，Agent 显式传 `null` 也拒绝。沿用既有规范中断 completion 与普通路由，写入 `RunResult.interruption`，保留现有并行区域边界；普通 Agent 提交序列化省略空中断字段。校验器不冻结或读取产物，`ArtifactKind::Interruption` 的执行者义务留给 H4；Host 的 suppressed 与投递账本未迁移。

原三个反例已成为内核正式回归，并保留隔离旧源码上的失败证据；另新增迟到 Code 结果、全部裁决计费、重试、损坏历史和中断校验测试。三组差分只补构造字段 `interruption: None`，断言和 20,000 例规模均未改。九项检查、实际命令和结果见[本轮验收产物](../../../experiments/constitution-host-preflight/k01/README.md)。

内核依赖、Record 格式、Waiter 与 Phase 变体均未增加。原预检目录保留为回归，原始失败 JSON 保持历史身份。**已知未决项**：崩溃后宿主重复提交的去重/重复计费本轮不处理；从历史求和的读取开销未做规模验收；固定 Goose、真实 Provider 与 Host 新调度路径不在 K0.1 验收范围。

## H1 补充：旧授权单一门

`ddadb17` 把空授权摘要从"每个使用点各自判断"改成"类型本身不可为空"：`PluginBinding.authorization_digest` 由 `String` 改为新类型 `AuthorizationDigest`（`rust/anchor-runtime/src/graph/ports.rs`），`TryFrom`/反序列化拒绝空值，错误信息为 `old authorization format; offline migration required`。方向已由架构师认可；该提交本身因下列测试问题被拒，由本节的修正提交补齐。生产代码里 `authorization_digest.is_empty()` 的判断点由 14 处降到 0 处。

### 14 处判断点的来源分类

按 `6ee2b0e`（上一版 H1 提交）逐一分类。`bindings` 来源指被检查的那一侧从哪来。"处置"列是这一刀的实际处理。

| # | 位置（6ee2b0e） | 检查对象与 bindings 来源 | 处置 | 理由 |
|---|---|---|---|---|
| 1 | `anchor-runner-host/src/execution.rs:156` | 调用方传入的 bindings：已解码持久化 Run 的 `plugin_bindings`（应用路径）或 catalog 当前定义（CLI 路径） | 删除 | 已解码的持久化 Run 不可能带空值；catalog 定义由本刀改为 `AuthorizationDigest`，同样不可为空 |
| 2 | `execution.rs:170` | `resolve_plugins` 返回的当前定义 | 删除 | 当前定义的授权摘要由 `FilePluginCatalog` 计算，新类型保证非空 |
| 3 | `execution.rs:182` | admitted 侧：已解码持久化 Run | 删除 | 空值已不可能；该处原本用于在老授权与"授权已变"之间选择错误信息，现在只剩后者 |
| 4 | `tool_host.rs:144` | `from_catalog` 入参：已解码持久化 Run 或 catalog 当前定义 | 删除 | 同一类型保证；反序列化已是唯一拒绝点 |
| 5 | `tool_host.rs:193` | 同上，逐 binding 再查一次 | 删除 | 与 #4 同一规则的第二处判断，纯冗余 |
| 6 | `anchor-runtime/src/graph/runner.rs:854` | `record.plugin_bindings`：已解码持久化 Run | 删除 | 已解码记录不可能带空值 |
| 7 | `runner.rs:878` | `resolve_plugins` 返回的当前定义 | 删除 | 当前定义的授权摘要由 catalog 计算，非空 |
| 8 | `runner.rs:914` | admitted 侧：已解码持久化 Run | 删除 | 同 #3，错误信息分支合并为一条 |
| 9 | `anchor-runtime/src/graph/state.rs:352` | 解码入口：持久化字节 | 删除（由新类型接管） | 这一处才是真正的门。删除判断，改由 `PluginBinding::deserialize` 内的 `AuthorizationDigest::try_from` 拒绝，语义等价且无法被绕过 |
| 10 | `state.rs:473` | `GraphRunRecord::validate`：内存记录 | 删除 | 内存记录只由解码或构造产生，两条路都不可为空 |
| 11 | `ports.rs:186` | `same_authorization` 的 `self`：已解码持久化 Run | 删除 | 空值不可构造；保留比较即可 |
| 12 | `anchor-runner-host/src/assistant_plugins.rs:143` | 助手 bundle 的 publication 回执 | 删除 | 回执经反序列化读出，空值已被拒绝；`id != binding.id` 的结构检查保留 |
| 13 | `assistant_plugins.rs:214` | 助手 bundle 的 Turn policy | 删除 | 同上 |
| 14 | `ports.rs:177` | 归因：`skip_serializing_if = "String::is_empty"` 使空值在序列化时消失 | 删除并改为结构化 | 这正是"空值"这种旧状态得以写出的原因。新类型 `Serialize` 恒输出字段，"缺失"不再被写入，只能在读到旧文件时出现 |

计数：**旧 14 处（生产代码）→ 新 0 处**，拒绝只在反序列化一层产生。测试代码里还有 3 处 `is_empty()`/`clear()`（`legacy_plugin_tests.rs:104`、`legacy_plugin_tests.rs:119`、`preparation_tests.rs:299`），其中 2 处是构造旧格式的夹具，随本节改动一并调整；`legacy_plugin_tests.rs:104` 的断言保留（它验证的是 catalog 真的算出了摘要）。

`assistant_plugins::active_bindings` 随同收窄为 `active_plugin_ids`：旧版本在缺策略时会用 `PluginBinding { authorization_digest: String::new(), .. }` 现场合成一个绑定，这正是空授权在内存里唯一的来源；改为只返回 id 集合后，这条合成路径在类型上消失。唯一调用点 `application/plugins.rs:763` 只做 `contains_key`，改为 `contains` 后语义不变。

### 改写与恢复的测试

| 测试 | 原覆盖 | 本刀前（ddadb17） | 本刀处理 |
|---|---|---|---|
| `anchor-runtime/src/graph/tests.rs: legacy_plugin_authority_requires_offline_migration_before_execution` | runner 行为：带空授权的记录不派发节点、store 无记录、拒绝信息具名 | 退化为纯解码测试（只断言反序列化报错） | **恢复 runner 行为覆盖**：解码仍逐个断言"缺失/null/空"三种旧形状；另用 `FileRunStore` 写入旧字节后 `GraphRunner::run`，断言返回具名拒绝、`nodes.calls` 为空、文件字节不变 |
| 同上文件 `a_new_run_does_not_bind_plugins_without_a_current_authorization_baseline` | runner 行为：resolver 给出不可用基线时新 Run 失败、不绑定、cursor 与 invocations 为空、节点未被调用 | 退化为纯新类型测试 | **恢复 runner 行为覆盖**；原夹具用的空授权在新类型下不可构造，改用 `digest` 为空（resolver 返回未哈希绑定）触发同一条失败路径，原断言与错误文本 `unauthorized` 全部保留。新类型的 `try_from`/序列化断言保留在测试尾部 |
| `anchor-runner-host/src/execution/legacy_plugin_tests.rs: legacy_mcp_preparation_and_load_require_offline_migration_without_rewrite` | prepare 与 `store.load` 都具名拒绝旧授权，且文件字节不变 | 夹具同时把 `format` 改成 12 并把授权摘写成空串 | **拆成两个用例**：`(无 format 改写, 期望 old authorization format)` 与 `(format=12, 期望 old Graph Run format)`，两者都断言 prepare 拒绝、load 拒绝、字节不变；并新增 `assert_no_execution_state` 检查拒绝后没有产生任何带 run_id 的执行状态 |
| 同文件 `changed_or_ambiguous_legacy_mcp_bindings_fail_before_durable_state_changes` | 四种变更（code/resources/servers/plugin_id）下，prepare 拒绝且持久化字节不变 | 保留了"传入当前 bindings"的分支，但那条分支在新代码里报的是"授权已变" | 保留两个真实路径并各自断言正确的拒绝：应用路径只能先 `store.load`（断言 `old authorization format`）；CLI 路径传入当前 bindings 时断言 prepare 抵达持久化解码并同样拒绝旧授权。原意图（拒绝 + 字节不变 + 无执行状态）保留 |
| 同文件 `a_waiting_parents_preparation_validates_its_legacy_child_without_upgrading_either` | 等待中子 Run 的旧身份被拒绝，父子字节均不变 | 断言仅"含 offline migration required" | 断言收紧为同时含 `old authorization format`，明确拒绝来自子 Run 的授权解码（父记录本身不含 binding） |
| `anchor-runner-host/src/tool_host/preparation_tests.rs: preliminary_wiring_refuses_legacy_identity_and_validates_skill_authority` | 旧身份装配被拒；**代码改动后同一 binding 失去授权** | 末段退化为重复一次反序列化断言，丢失"装配期拒绝已变更定义"的覆盖 | **恢复装配期覆盖**：保留反序列化拒绝断言；另在 `plugin.json` 新增一个 MCP server（真正改变授权摘要的输入）后断言 `from_catalog` 拒绝旧 binding。原测试改的是 `instructions.md`，而那并不进入授权摘要，旧断言此前是由空授权短路才通过的 |
| `anchor-runner-host/src/tool_host.rs: fake_fixture_binding_does_not_require_a_filesystem_catalog` | 旧授权装配被拒 | 改为断言反序列化报错 | 保留（装配侧已不可构造空授权，类型保证替代；装配期拒绝的覆盖由上一行恢复） |
| `anchor-runtime/src/graph/tests.rs: old_run_formats_are_named_refusals_on_decode_and_file_load` | 各旧格式逐个具名拒绝 | 断言放宽为"三种旧格式之一" | 保留（夹具与期望不是一一对应的放宽仍存在，属 H1 原有风格；本刀未再放宽。待办见下） |
| `anchor-runtime/src/graph/tests.rs: newer_run_formats_are_refused_instead_of_guessed` | 新格式被拒 | 由"decode 通过 + validate 拒绝"改为"decode 拒绝" | 保留（`RunRecordFormat` 新类型使不合法格式无法构造，validate 分支随之不可达） |

未新增测试函数，只强化既有断言；`anchor-runtime` 的 `AuthorizationDigest` 两个 `compile_fail` doctest 由 ddadb17 引入，本刀验证其确实失败编译（`cargo test -p anchor-runtime --doc`：2 通过）。注意 `--all-targets` 不运行 doctest，因此它们不计入全量测试数字。

### 第 1、2 项的定性

- **第 1 项（两个确定性失败的 Host 夹具测试）**：定性为**夹具过时**，不是生产路径丢失拒绝。ddadb17 使空授权摘要不可构造，而两个夹具仍假定"内存里可以持有一条空授权 binding"：`legacy_record` 在内存中清空授权摘要，`legacy_file` 把 `format` 改成 12。在新类型下 prepare 能得到的历史数据只有解码后的记录，所以拒绝要么发生在 `store.load`（旧格式），要么在"传入当前 bindings"的路径上以授权比对失败的形式出现。裁定：保留原意图改写夹具（旧数据被拒绝 + 持久化字节不变 + 未执行节点），并把 runner/装配层的拒绝覆盖补回来，见上表。生产代码只保留解码层这一道门，没有丢拒绝；丢掉的只是"逐 Plugin 指名的错误文本"，登记为 S16。
- **第 2 项（两个满载下失败的测试）**：在 `445d278` 的 6 次全量运行中未复现，但同一故障类别（`Goose conversation is already active`、进程内 env 竞态）在基线上复现了 3 次。定性为**既有 flaky，不在本切片修**，证据见"待处理与观察"一节。

### 语义变更

本刀不改变对外行为：旧数据的拒绝结论、"读取不改写"、拒绝的粒度（整条记录）都保持一致，只是拒绝点从 14 处收敛到解码一层。唯一可观察的差异是错误信息的保真度，登记为 S16。

`ports.rs` 的 `skip_serializing_if` 删除后，新写出的记录字节不变：该属性只对空值生效，而新类型不可能为空，非空摘要原本也照常写出。

## 待处理与观察：全量运行中的既有 flaky

为定性 `ddadb17` 之后全量运行里两个"满载才失败"的测试，在临时 worktree `/tmp/h1-base`（`445d278`，`git worktree add`，用后已 `git worktree remove`）用同一条全量命令跑了 6 次：

| 运行 | 结果 | 失败项 |
|---|---|---|
| 1 | 1277 通过 / 1 失败 / 112 忽略，45 targets | `runtime_management::tests::runtime_python_version_is_a_refreshable_observation_without_publishing_configuration` |
| 2 | 1278 / 0 / 112 | — |
| 3 | 1276 / 2 / 112 | `goose_acp::conversation::tests::latest_native_fact_may_be_an_authorized_background_continuation_not_chain_head`、`run_data::tests::conversation_cleanup_keeps_other_scopes_and_obeys_native_lock`（均为 `Goose conversation is already active` / `native conversation ... is already active`） |
| 4 | 1278 / 0 / 112 | — |
| 5 | 1277 / 1 / 112 | `goose_acp::conversation::tests::latest_native_fact_may_be_an_authorized_background_continuation_not_chain_head`（`Goose conversation is already active`） |
| 6 | 1278 / 0 / 112 | — |

结论：

1. **两个报告项在 `445d278` 上 6 次运行均未复现**（`api::tests::session_plugins::only_the_configured_pilot_graph_may_declare_a_plugin_required`、`goose_acp::conversation::tests::reviewed_provider_patches_load_old_history_only_for_a_new_invocation` 都是通过）。所以"由 `ddadb17` 引入"没有被证实，但也没有被排除。
2. **故障类别在 `445d278` 上确实存在且可复现**：6 次里有 2 次（第 3、5 次）出现 `Goose conversation is already active`，落在同一个 `goose_acp/conversation` 测试文件的两个兄弟测试上；另有 1 次（第 1 次）是进程内环境变量竞态导致的运行时探测失败。三者都不是本刀触碰的代码路径。
3. 因此判定为**既有 flaky，不在本切片修**，登记在此：这一类测试在同一二进制内共享进程级 conversation lease / Library 读锁 / `PROCESS_ENV_LOCK`，满载时相互竞争；`runtime_python_version` 是同一进程内 `std::env` 竞态。代表性证据：第 3 次的失败信息与报告项的一字不差。
4. 未做的验证：没有为这些测试做串行化改造或隔离，也没有扩大采样规模。若要归零，需要单独一刀（例如给共享 lease 的测试加进程级串行门），属于 H2 之前的独立工作。

## 语义变更记录

| 编号 | 场景 | 旧行为 | 新行为 | 理由 |
| --- | --- | --- | --- | --- |
| S1 | 节点提交了非法路由 | 整个 Run 失败 | 打回给提议者修正（`RouteInvalid`） | 宪法：可修正的问题返回提议者，不终止任务 |
| S2 | 所有节点都因轮数上限而停止 | Run 状态为 `Stopped`，可以 resume，但 resume 后立即再次停止 | 结算为 `Escalated`，附停止原因 | 旧状态名义上可恢复，实际上无事可做；新状态如实表达"需要人决定" |
| S3 | 对已 `Aborted` 的 Run 再次 abandon | 幂等成功，返回同一结果 | 拒绝（`TaskSettled`） | 已结算不可逆，不需要为幂等单独设规则；调用方可通过读取状态得到同样信息 |
| S4 | 退役之后再 abandon（PoC E2 发现） | 允许 | 退役只移动 Session current，未结算 Run 仍可 Human abandon | 由已批准的 S27 结清（任务书 §10.17） |
| S5 | 并行波次中某个分支触发轮数上限 | 先准备好的兄弟分支被记为"已开始"，但从未执行（幽灵调用） | 不把任何未派发的调用记为已开始 | 宪法第二条：没有发生的事不能记为发生 |
| S7 | 子 Run 的接纳结果未知 | Run 失败 | 结算为"未知" | 宪法第二条：未知是正当的终点，不应伪装为失败 |
| S8 | 有多条出边的调用节点 | 运行到该节点时 Run 失败 | 创建 Run 时拒绝 | 宿主代码无法替调用节点选择路由，这是作者错误，应尽早发现 |
| S9 | 旧版本写入的 `Deleted` 调用标记 | 恢复时按已完成处理 | H1 读取具名拒绝，需离线迁移 | 分支不背兼容包袱；旧数据迁移在合并时单独处理 |
| S6 | 并行区域已注定无法汇合（某分支触发上限） | 等本波次所有分支完成、并入结果后，下一波次才失败 | 立即结算为失败；之后到达的兄弟分支结果被拒绝，但连同产物证据完整记录在 Run 历史中 | 快速失败减少无效工作，与规格"分支失败时请求停止活动同伴"一致；被拒提交同样被记录，信息不丢失 |
| S10 | 任务历史中的成本 | 打回、拒绝、过期、待定等成本可能丢失 | 所有历史记录都计费，成本不推进版本；同一工作过期重提成本为 0 | D1，预算不能被拒绝提交绕过 |
| S11 | 残缺或损坏的历史 | 首个解析失败后静默停止，后续接受记录可能不可恢复 | 追加前去掉未闭合尾字节；闭合坏行具名报 Corrupt；完整历史字节不变 | D2，接受的事实必须可恢复，损坏不能静默吞掉 |
| S12 | Code 观察到未知效果 | Code stop 一律 HumanOnly | 有 Observation 可 stop 进入等待人；Code 无证据、Agent stop 与 Code resume 人的等待仍拒绝 | D3，安全方向与释放方向不对称 |
| S13 | 等待中的迟到节点结果 | Code/Human 可以继续写入并推进 Graph | 非生命周期提交统一 TaskWaiting 并计费；越预算仍结算失败，不应用节点内容 | D5 + D1，等待门禁由内核强制执行 |
| S14 | Graph 节点中断提交 | 无中断字段，无法保存宿主中断事实 | 仅 Code 可携带中断字段，记录规范中断结果并沿普通路由继续；非 Code 含显式 null 也拒绝 | D4，中断来自宿主观察，不能由 Agent 自述产生 |
| S15 | 旧 Run 格式、旧授权与旧 cursor | 加载时升级或派发前补写计数/授权 | H1 读取/接纳时拒绝并提示离线迁移，不改写数据；持久化记录仅接受对象 | C6 用户明确不兼容旧格式，拒绝隐式发明历史事实 |
| S16 | 空授权摘要的拒绝信息 | `Plugin \`{id}\` uses an old authorization format; offline migration required`，逐 Plugin 指明 id | `old authorization format; offline migration required`，由解码层给出整条记录的拒绝，不含 Plugin id（serde 另附行列位置） | 判断点由 14 处收敛到 `AuthorizationDigest` 反序列化一层，解码层已不持有"是哪个 Plugin"的语境；调用方只依赖 `offline migration required` 这一子串，无分支受影响 |
| S17（已批准 2026-10-17） | 宿主确认节点失败 | 计入开始次数，无结果、无出边决定；并行同波多失败时逐个覆盖 Run error，最终错误取决于波内处理顺序（`parallel.rs:278`） | Code 以 Settle 结算，保留计数和分支失败事实；首次被接受的失败决定唯一结算原因，后续失败以 TaskSettled 被拒并留在历史中；只有首个失败分支为 Failed | 旧"最后一个获胜"是波次处理顺序的偶然，新调度器无波次屏障，无从复刻；与 S6 fail-fast 一致；结算不可逆；其他失败事实不丢失。对照规则见 H2 任务书 §1 测试第 3 条 |
| S18（已批准 2026-10-17，合并 S6、S17） | 并行区内多个终结事实（节点失败、分支轮次上限）在同一波出现，例如 seed 2122：单个节点失败 + 成功兄弟的后继已达上限 | 波次屏障：收齐整波后处理，最终原因取决于波内顺序；本例为 b1_0 failed，ceased 为空 | 第一个被内核接受的终结事实决定唯一结算原因；之后的终结事实与结果以 TaskSettled 被拒并留在历史中；本例为 b0_1@1 上限 | 新模型无波次屏障，结算原因由提交顺序决定，是 S6 fail-fast 与 S17 的同一规则。对照改为 H2 任务书 §1 的三条不变量（原因正当、事实不丢不重、其余相等），seed 495/2122 用例的断言替换由审查方批准 |

S1、S2 在差分测试中被显式映射（S2：旧 `Stopped` 且存在停止原因 ⇔ 新 `Escalated`）。S1 未进入差分测试，因为路由脚本只产生合法路由。

## Host 接入预检：停止于内核边界

在 `d0f9b61` 上复现成本丢失、残缺尾行后接受记录不可恢复及 Code 无法进入等待人，按 C1 停止；原始观察与结构基线保留在[预检产物](../../../experiments/constitution-host-preflight/review.md)。审查随后将迟到 Code 结果从“不阻塞”改为 D5 必修；现由上述 K0.1 覆盖，旧建议不再作为实施依据。

## H1：旧格式清理（实现完成）

H1 已验收，下一步 H2。`445d278` 之后用户独立重跑确认 45 targets、1278 通过、0 失败、112 忽略，Clippy 与 fmt 通过。`ddadb17` 的旧授权单一门方向已获认可，但其测试问题被验收拒绝，由 `H1 补充：旧授权单一门` 一节的修正提交补齐（夹具过时、覆盖恢复见该节）。用户已决定 H1 仅做兼容删除；FileStore 随 H2 调度器接入，C2 零直接写入在 H3 结束验收。原[存储边界预检](../../../experiments/constitution-host-preflight/h1/review.md)保留为历史证据，不再是当前停止条件。

已删除格式自动迁移、managed-environment 遗留字段、旧授权升级、Deleted 调用标记及旧 cursor 计数补写。旧 Run 格式、遗留 environments、缺失授权摘要、Deleted 标记和旧 cursor 均具名拒绝，说明需离线迁移；读取不重写原始字节。当前持久化记录必须为对象，非对象序列也具名拒绝；解码保留 serde 已知字段重复键及未知字段拒绝，不经 Value 覆盖字段。未修改内核、GraphValidator 或三组差分测试；仍由旧 RunStore 单独负责 Run，无内核双写或新提交契约。

### 删除与替换的测试

以下旧 Runtime 测试名逐一退出；除授权测试改为执行前具名拒绝外，其余只为兼容/升级存在的测试删除，由统一旧格式拒绝覆盖：

- `a_format_ten_record_loads_without_inventing_plugin_views`。
- `a_managed_environment_record_migrates_by_dropping_the_field`。
- `a_rejected_legacy_plugin_does_not_partially_upgrade_other_plugins`。
- `deleted_graph_call_marker_resumes_as_completed_without_replay`。
- `format_one_cursor_migrates_under_lease_and_persists_once`。
- `format_one_terminal_run_migrates_without_changing_counters`。
- `format_seven_migrates_to_current_without_inventing_timestamps`。
- `format_twelve_migration_preserves_the_absence_of_plugin_authority_and_code_observations`。
- `legacy_plugin_authority_is_upgraded_only_when_the_original_code_is_still_installed`。
- `run_format_six_migrates_recovery_submissions_to_current_format`。
- `run_format_three_migrates_graph_calls_and_plugin_bindings_to_current_format`。
- `run_format_two_migrates_to_parallel_aware_format_three`。

Host 的 `legacy_mcp_preparation_is_read_only_and_runner_upgrade_requires_the_run_lease` 替换为 `legacy_mcp_preparation_and_load_require_offline_migration_without_rewrite`；`preliminary_wiring_accepts_only_unchanged_legacy_identity_and_validates_skill_authority` 替换为 `preliminary_wiring_refuses_legacy_identity_and_validates_skill_authority`。fake fixture 仅接受严格相等的现代 binding，不再借升级规则放行。

保留原业务断言：Run Plugin 历史顺序、停止 Run 的 Graph 修改/放弃、未完成 Run 阻止 Graph 删除、Plugin 更新影响这四处 fixture 补齐当前授权摘要；可变 MCP declaration 拒绝测试改用现代授权，以继续实际到达并验证原权限检查。没有删除这些业务/安全测试。新增旧格式 decode/FileRunStore 字节不变、旧 cursor/旧授权执行前拒绝、重复字段和非对象序列拒绝回归。

### H1 之后修复：并发 fork 导致的锁释放延迟

**根因**  
Linux flock 特性：fork 后父子进程同时持有锁文件 fd 副本，直到所有副本关闭，锁才真正释放。`goose_acp/conversation.rs:63` (flock) 和 `run_data.rs:119` (try_lock) 两处只依赖 drop 隐式关闭 fd，未显式 unlock。当 Host 高频 spawn 子进程（runtime provision、插件探测、artifact 处理）时，子进程在 exec 完成前会继承父进程的锁 fd，导致父进程 drop 后锁仍未释放。下一个请求立刻 try_lock 拿不到，误报 "conversation is already active" 返回 409。

对比实验验证：只 drop 不 unlock 时，单 spawner 20000 次中假阳性 2255 次，8 spawner 10353 次；显式 unlock 后 drop，单/8 spawner 各 20000 次假阳性均为 0。

**修复**  
- `rust/anchor-runner-host/src/goose_acp/conversation.rs:33-39`：为 `ConversationScope` 添加 Drop impl，在 drop 时显式 `flock(..., Unlock)`，参照 `graph/store.rs:97` 的 `FileRunLease::drop`。
- `rust/anchor-runner-host/src/run_data.rs:177-180`：`remove_legacy_conversation` 函数在最终 return 前显式 `lock.unlock()`。

**测试修复**  
`runtime_management.rs` 测试中 `configure_python` 辅助函数原直接 `fs::write` 写入 python 桩脚本，该 fd 会被并发测试线程 fork 出的子进程继承，导致 exec 这个脚本时触发 ETXTBSY (Text file busy)。修改为通过子进程完成写入（`sh -c 'cat > path'`），写 fd 仅存在于子进程内，不会被测试进程的其他线程 fork 继承。同时修复测试内 307、349 行同样会被 exec 的脚本覆写操作。

独立实验证实：直接 write + exec，3000 次有 56 次 ETXTBSY（单 spawner），8 spawner 时 634 次；改用子进程写入后 0 次。

**影响范围**  
生产缺陷，影响真实并发场景（多用户 Goose 会话、多 Run cleanup）。修复同时消除测试 flaky。

`ddadb17` 之后这两个替换测试的 runner/装配层覆盖曾退化为纯解码断言，已在 `H1 补充：旧授权单一门` 一节恢复；那一节另有逐测试的覆盖去向表。

### H2/H3 直接写入棘轮起点

范围仅两个 Host crate 的生产 GraphRunRecord；按静态站点计数，排除测试、metadata、Session/Goose 自身记录。构造器及局部变量整体重绑定不算字段 mutation；集合 push/retain、mem::take 与 touch 单列。save 与 mutation 不可混为一个计数。下表为 H1 提交源码行号，后续只能减少，不能增加。

| Host | save | 字段赋值 | 集合修改 | touch | mutation 合计 |
|---|---:|---:|---:|---:|---:|
| anchor-runner-host | 15 | 22 | 4 | 7 | 33 |
| anchor-graph-host | 1 | 3 | 0 | 0 | 3 |
| **合计** | **16** | **25** | **4** | **7** | **36** |

### GraphRunRecord store.save（16）

| 文件:行号 | 写入 | 用途 |
|---|---|---|
| rust/anchor-runner-host/src/application/admission.rs:103 | `self.store().save(&record)?` | 新 Run admission；先持久化不可变身份元数据，再保存记录、注册并启动执行。 |
| rust/anchor-runner-host/src/application/assistants.rs:227 | `self.store().save(&record)?` | channel assistant 新 Run：元数据、执行准备及 workspace 绑定后保存。 |
| rust/anchor-runner-host/src/application/conversations.rs:855 | `self.store().save(&record)?` | conversation 新 Run：冻结附件/元数据及执行准备后保存，随后注册并启动。 |
| rust/anchor-runner-host/src/application/session_calls.rs:184 | `self.store().save(&record)?` | stop_session_call：非活跃且未终态记录保存为 Stopped。 |
| rust/anchor-runner-host/src/application/session_calls.rs:211 | `self.store().save(&record)?` | yield_session_call：非活跃且未终态记录保存为 Stopped。 |
| rust/anchor-runner-host/src/application/session_calls.rs:392 | `self.store().save(&record)?` | execute_session_call：保存中断工具恢复上下文、清空待恢复项并进入 Running，再启动。 |
| rust/anchor-runner-host/src/application/recovery.rs:139 | `self.store().save(&record)?` | 接受 Abort 恢复决定后保存 Aborted、错误与恢复事实。 |
| rust/anchor-runner-host/src/application/recovery.rs:145 | `self.store().save(&record)?` | 一项恢复决定完成但仍有未决项，保存 WaitingRecovery。 |
| rust/anchor-runner-host/src/application/recovery.rs:150 | `self.store().save(&record)?` | 最后一项恢复决定完成，保存 Running 后重新启动。 |
| rust/anchor-runner-host/src/application/control.rs:126 | `self.store().save(&record)?` | 停止 WaitingCall 父 Run，先保存 Stopped，再停止等待中的子 Run。 |
| rust/anchor-runner-host/src/application/control.rs:137 | `self.store().save(&current)?` | 停止子 Run 后重读父记录，若被推进回 WaitingCall，再保存 Stopped。 |
| rust/anchor-runner-host/src/application/control.rs:146 | `self.store().save(&record)?` | 普通 inactive stop：保存 Stopped，保留 cursor 与完成事实、不派发工作。 |
| rust/anchor-runner-host/src/application/abandon.rs:164 | `self.store().save(&record)?` | finalize_abandoned_run：非活跃且未终态 Run 保存为 Aborted 并记录放弃原因。 |
| rust/anchor-runner-host/src/execution.rs:317 | `self.store.save(&durable).map_err(...)?` | Runner 出错释放 lease 后，重新加载仍 Ready/Running 的记录，保存 host 中断失败。 |
| rust/anchor-runner-host/src/main.rs:663 | `execution.store.save(&record)`（`newly_admitted` 条件） | standalone CLI/协议入口，仅首次 admission 保存记录；已有匹配 snapshot/input 的记录不重存。 |
| rust/anchor-graph-host/src/graph_call.rs:532 | `self.store.save(record)` | persist_child_admission：catalog 子身份元数据/可选 Session call 元数据保存后，持久化 child Run。 |

### 字段赋值、集合修改与 touch（36）

| 文件:行号 | 写入 | 用途 |
|---|---|---|
| rust/anchor-runner-host/src/application/admission.rs:70 | `record.plugin_bindings = plugin_bindings` | 记录 admission 观察到的当前 Plugin bindings。 |
| rust/anchor-runner-host/src/application/admission.rs:71 | `record.plugin_bindings_initialized = true` | 标记 Plugin bindings 已初始化。 |
| rust/anchor-runner-host/src/application/admission.rs:102 | `record.touch()` | admission 保存前刷新记录时间。 |
| rust/anchor-runner-host/src/application/assistants.rs:184 | `record.plugin_bindings = plugin_bindings` | 新 assistant Run 记录 bundle Plugin bindings。 |
| rust/anchor-runner-host/src/application/assistants.rs:185 | `record.plugin_bindings_initialized = true` | 标记 assistant Run bindings 已初始化。 |
| rust/anchor-runner-host/src/application/conversations.rs:812 | `record.plugin_bindings = plugin_bindings` | 新 conversation Run 记录 bundle Plugin bindings。 |
| rust/anchor-runner-host/src/application/conversations.rs:813 | `record.plugin_bindings_initialized = true` | 标记 conversation Run bindings 已初始化。 |
| rust/anchor-runner-host/src/application/session_calls.rs:183 | `record.status = RunStatus::Stopped` | 停止未活跃、未终态 Session call。 |
| rust/anchor-runner-host/src/application/session_calls.rs:210 | `record.status = RunStatus::Stopped` | yield 非活跃、未终态 Session call。 |
| rust/anchor-runner-host/src/application/session_calls.rs:369 | `std::mem::take(&mut record.recovery)` | 清空并取走全部未决恢复项，逐项生成效果未知的恢复上下文。 |
| rust/anchor-runner-host/src/application/session_calls.rs:385 | `record.recovery_submissions.push(RecoverySubmission { ... })` | 追加中断工具调用的 Completed 恢复提交事实。 |
| rust/anchor-runner-host/src/application/session_calls.rs:391 | `record.status = RunStatus::Running` | Session call 恢复上下文记入后进入 Running。 |
| rust/anchor-runner-host/src/application/recovery.rs:123 | `record.recovery_submissions.push(RecoverySubmission { ... })` | Harness 意图持久化后追加本次恢复决定。 |
| rust/anchor-runner-host/src/application/recovery.rs:130 | `record.recovery.retain(...)` | 移除匹配 key + attempt_id 的已处理未决恢复项。 |
| rust/anchor-runner-host/src/application/recovery.rs:136 | `record.status = RunStatus::Aborted` | Abort 决定将整个 Run 置为终态；保留其他未决项作只读事实。 |
| rust/anchor-runner-host/src/application/recovery.rs:137 | `record.error = Some(format!(...))` | 写入 operator abort 的节点错误说明。 |
| rust/anchor-runner-host/src/application/recovery.rs:138 | `record.touch()` | Abort 分支保存前刷新记录时间。 |
| rust/anchor-runner-host/src/application/recovery.rs:143 | `record.status = RunStatus::WaitingRecovery` | 仍有未决恢复项，继续等待恢复。 |
| rust/anchor-runner-host/src/application/recovery.rs:144 | `record.touch()` | 继续等待恢复分支保存前刷新记录时间。 |
| rust/anchor-runner-host/src/application/recovery.rs:148 | `record.status = RunStatus::Running` | 未决恢复项全部处理后进入 Running。 |
| rust/anchor-runner-host/src/application/recovery.rs:149 | `record.touch()` | 恢复执行分支保存前刷新记录时间。 |
| rust/anchor-runner-host/src/application/control.rs:124 | `record.status = RunStatus::Stopped` | WaitingCall 父 Run 先停止，再处理等待的子 Run。 |
| rust/anchor-runner-host/src/application/control.rs:125 | `record.touch()` | 首次停止 WaitingCall 父 Run 保存前刷新时间。 |
| rust/anchor-runner-host/src/application/control.rs:135 | `current.status = RunStatus::Stopped` | 停止子 Run 后若父记录又为 WaitingCall，再次停止。 |
| rust/anchor-runner-host/src/application/control.rs:136 | `current.touch()` | 重读后的父 Run 保存前刷新时间。 |
| rust/anchor-runner-host/src/application/control.rs:144 | `record.status = RunStatus::Stopped` | 普通 inactive stop，保留执行 cursor 与完成事实。 |
| rust/anchor-runner-host/src/application/control.rs:145 | `record.touch()` | 普通 inactive stop 保存前刷新时间。 |
| rust/anchor-runner-host/src/application/abandon.rs:160 | `record.status = RunStatus::Aborted` | 非活跃、未终态 Run 由 operator abandon 置为 Aborted。 |
| rust/anchor-runner-host/src/application/abandon.rs:161 | `record.error = Some(format!(...))` | 写入 abandon reason 与 requested_at，保留放弃原因。 |
| rust/anchor-runner-host/src/execution.rs:313 | `durable.status = RunStatus::Failed` | 非 RunBusy/RunConflict 执行错误后，仅仍 Ready/Running 的持久记录置为 Failed。 |
| rust/anchor-runner-host/src/execution.rs:314 | `durable.error = Some(format!(...))` | 写入 host execution interrupted，声明未完成效果不会自动重放。 |
| rust/anchor-runner-host/src/main.rs:615 | `expected.plugin_bindings = plugin_bindings.clone()` | standalone 预期新记录绑定已解析 bindings；已有记录路径沿用加载记录。 |
| rust/anchor-runner-host/src/main.rs:616 | `expected.plugin_bindings_initialized = true` | 标记 standalone 预期新记录 bindings 已初始化。 |
| rust/anchor-graph-host/src/graph_call.rs:295 | `record.run_id = run_id.clone()` | child 构造后改用由 call identity 决定的可重试确定性 ID。 |
| rust/anchor-graph-host/src/graph_call.rs:296 | `record.plugin_bindings = referenced.iter().cloned().map(...).collect()` | child admission 记录当前 Library 解析的 Plugin bindings。 |
| rust/anchor-graph-host/src/graph_call.rs:301 | `record.plugin_bindings_initialized = true` | 标记 child Plugin bindings 已初始化。 |


RunStatus 直接赋值 11 站点已包括在 mutation 中；旧 Runtime Runner 的内部写入不在 Host 目录计数内，它随 H2 替换。基线只盘点写点，不证明并发恢复语义已迁移。验证命令、结果和未覆盖边界见[H1 验证证据](../../../experiments/constitution-host-preflight/h1/verification.md)。

## H2.1：本地确定性验收完成

按已批准 S18 统一失败对照：首次终结事实决定原因；每个失败与 Work 结果在状态或迟到历史中恰好出现一次；还原允许的迟到与上限效果后，其余事实相等。seed 495、2122 独立用例通过，生产校验器、内核及旧 Runner 未改。

串行 10000（node_failed=772）、并行失败组 5000（node_failed=775、multi_failure_waves=6、ceiling_preempted_failures=2）、调用 5000 均通过。用户“继续”后采用交回建议，原无失败回归使用同一组 5000 个 seed 单独保留原门槛：parallel_activations=5048；失败组仍为4775，不伪装成超过5000。失败哈希、注入概率、seed范围、原阈值均不变。

Runtime 127通过，另2 doctest通过；fmt、Clippy通过，内核diff为空。Kernel与Host未改，复用本轮此前37与1120通过/112忽略结果。真实Provider和Goose未新增验收。详细Tally及原始日志见[H2.1验收](../../../experiments/constitution-host-preflight/h2/h21-completion.md)。后续改为新版任务书的串行探针P；原H2.2/H2.3不再推进。

## P：串行 Kernel 探针本地交付完成

实现提交 `ff09f2c feat(host): kernel-path probe for serial Runs (P)`。同一Host准入显式选择 `execution=kernel_probe`，Run只由Kernel FileStore保存，调度器复用现有请求授权与NodeExecutionPort/ArtifactPort；默认旧Runner保留。Human stop/resume/abandon经Kernel裁决，接受后再取消worker；未知效果带Observation等待人核查，迟到结果进入拒绝历史，不自动重放。状态是Phase的只读投影，同ID双权威拒绝；未改Kernel与生产Graph裁决。

七类应用层场景、1000个共享生成器seed对照及真实Op.run/Bubblewrap/Artifact路径通过。Kernel37、Runtime127另2 doctest、runner-host1083/0失败/112忽略、graph-host46/0失败；fmt、Clippy与sandbox构建通过。真实Goose/Provider未执行，固定二进制未配置。证据及全部最终命令见[P验收](../../../experiments/constitution-host-preflight/p/verification.md)。

测量去重包络：旧193个语法判断点/1416源码行，新51个/472行（新增449行）；旧包络含未迁能力，不能把差值当同等能力净收益，实际删除旧生命周期实现0行。全Host粗估与口径、逐点源码位置、Unsupported和查询适配耦合见[测量与未决](../../../experiments/constitution-host-preflight/p/measurement.md)。fanout/join、Call、常驻助手、pause、真实Goose attempt恢复与Kernel Run删除未迁；P任务已交付，后续迁移范围按审查结论再定。

## P 与 H2.1 审查结论（2026-10-17）

审查方独立重跑四 crate：48 个 target，1295 通过 / 0 失败 / 112 忽略。内核相对 `6ee2b0e` 零改动；调度器和 `kernel_runs.rs` 中无停止/暂停轮询；差分阈值与 seed 范围未降，并行组 `parallel_activations > 5_000` 门槛移到无失败对照样本上，属于保留而非弱化。H2.1 与 P 验收通过。

结论：写入与生命周期一侧成立（stop 40→7、resume 38→10 个判断点，旧 8 处停止/暂停轮询在新路径为 0）；跨能力的总量对比（193→51）不同口径，不作为收益证据，实际删除旧代码为 0。读取一侧未证：P 把 Phase 投影回 RunStatus，`queries.rs` 复用 FileStore 文件名编码取时间。下一步为限时 2 天的读模型探针 R，见 [R 任务书](constitution-r-task.md)；其中 R0 将 `469d81f` 的锁释放修复收口为唯一文件租约类型（`run_data.rs` 提前返回路径仍未显式解锁，生产代码另有约 15 处同类 flock）。

## R0 / R：锁收口与统一读模型探针本地交付

R0 `cb2219c` 提供唯一 FileLease，在Drop显式释放，覆盖提前返回与fork继承描述符；22个底层获取站点和6个显式释放站点收口为共享实现的1获取/1释放，22个调用者继续保留原锁策略。已同步master `6394e28`，保留master原有共享读锁和可见性；master相关check与132项定向测试通过。

R `54e664e` 引入只读RunView（Phase、Graph passes、worker、宿主updated），按相位派生动作；Session条件继续由调用者处理。接受提交后记录Host观察文件，不读取Kernel路径或mtime；观察失败不改变控制。旧Runner适配和原生JSON兼容边界单列，旧Failed的缺省原因用Unclassified，不虚构原因码，也不回写兼容记录。

结论为**需要重想**：同口径I/M/W判断点82→90。四个消费者生产RunStatus引用清零，但旧Ready/Running、Paused/Stopped、预算/recovery与P孤立driver启动等规则没有消失，不能全归属Session/授权。旧适配仍读取原record，严格的单RunView输入未闭合，不记为统一迁移完成。H3–H6继续暂缓，待审查旧显示/动作契约。测量、逐项保留规则和剩余18个原文件估算见[R测量](../../../experiments/constitution-host-preflight/r/measurement.md)。

最终Kernel37、Runtime127+2 doctest、runner-host1087/112原有忽略、graph-host46、Library74、FileLease3通过，0失败；比四crate基线新增4项，另R0新增3项锁测试。fmt、Clippy和sandbox构建通过；Kernel相对6ee2b0e、生产GraphValidator相对P均零diff。真实Goose/Provider、Web与部署未新增验收。原始命令、输出、HTTP/动作/字节兼容测试及未覆盖边界见[R验证](../../../experiments/constitution-host-preflight/r/verification.md)。

## R 审查结论与 R2 决定（2026-10-17）

审查方重跑六 crate（含 file-lease、library）：58 个 target，1376 通过 / 0 失败 / 112 忽略；内核、生产校验器、Web diff 为空。R0 通过：22 个调用站点统一经共享租约，Drop 显式释放；已同步 master 本地提交 `6394e28`（未推送）。R 的负结论（82→90）成立，但前提是任务书要求对外 JSON 不变：旧 10 状态中 Ready/Running/waiting_resume、Paused/Stopped/BudgetStopped 的区分不在相位里，规则只能搬进适配。读侧复杂度来自产品状态契约，不来自运行时。

**用户决定**：对外状态收敛到内核相位（executing / waiting(human|child|effect) / settled(outcome) + reason + active + actions），允许改 API 与 Web，pause 改为 stop 的别名。R2 在新词汇下以同一口径重测，见 [R2 任务书](constitution-r2-task.md)。


## R2停止交回（2026-10-19）

源码 `74a4dec`：Run 对外收敛为 executing / waiting / settled + waiter/outcome/reason/active/actions/updated；旧状态只经一个有损映射函数进入读视图，HTTP/Pilot pause 接为 stop 别名，Web 使用新事实。**R2 已停止、未验收**：pause 即时取消后，原“当前节点完成并留下 1 条结果”的非状态断言实得 0；Session 原 pause 拒绝也变为有效 stop。未修改实际 stop/resume/abandon 规则、未改这些非状态断言，按任务书 §7 交回。审查需决定是否授权调整原暂停测试场景，或保留对外暂停能力；不以探针提交代表验收通过。

同口径测量 **82→90→92**，Web 状态分支 **81→79**，需要重想；H3–H6 继续暂缓。保留 Session 写回和内部 record 桥均计回，没有通过搬移制造收益。当前证据：[测量](../../../experiments/constitution-host-preflight/r2/measurement.md)、[92 点逐项](../../../experiments/constitution-host-preflight/r2/decisions.md)、[实际验证与停止现场](../../../experiments/constitution-host-preflight/r2/verification.md)。

Rust API 定向 188 过/11 失败，最终针对 11 例复跑仍失败；映射/动作/静止 3 例、投影 4 例、pending 1 例通过。application 最终子集 113 过/1 候选进程回收时序失败，该例独立复跑通过；1000 seed 已在初轮通过。fmt/clippy/sandbox 构建通过，Kernel 零 diff。六 crate 全量未执行；Web 33 文件/285 测试与 build 通过，定向 e2e 9 过/1 选择器失败，修复后该例独立通过。真实 Goose/Provider、部署、同一 HTTP 的旧/Kernel 三阶段对照仍未验收。

S 候选 8 条，全部待批：

| 编号 | 旧行为 → 新行为 / 影响 |
| --- | --- |
| S19 | 旧十状态/专门提示 → Phase+waiter/outcome/reason，客户端字段破坏；Ready/孤立 Running 同为等待接续和注意项 |
| S20 | 共享 RunView 的无 worker Executing 可以 resume；原 Kernel JSON 已有独立 driver 接续规则，现统一 |
| S21 | Paused/BudgetStopped 无 worker 统一静止，影响 Session 交接、准入、退役与清理 |
| S22 | stop 仅 Executing 为 true、不读 pending flag，未结算增加 abandon；控件按服务端事实显示 |
| S23 | Human 无 worker 可 resume、Child/Effect 不可；旧预算/recovery 写守卫仍在，可能与展示不一致 |
| S24 | 移除 pausing/stopping/control_requested 与请求优先级展示，接受请求但未落状态时仍显示 Executing |
| S25 | pause→stop 即时取消，影响当前节点结果、恢复、Session 取消许可；已触发停止条件 |
| S26 | 缺少 Kernel 时间观察返回 updated:null，旧 Run mtime 来源保留，时间不参与控制 |

自行判断并登记：Session 原结算 callback 原样搬回 conversations 属于写边界；Phase 序列化和旧内部桥都保留测量成本；PilotTurn/listener/投递/节点结果保持各自事实；不新增旧状态替身。未迁能力没有记作已完成。

### R2 断言修改附录

以下逐项列出测试名、旧/新断言及 S 候选。Rust **84 条展开断言修改 + 6 条轮询目标**，另加别名检查 2 条；Web **30 处直接表达式修改**，参数实例/fixture 间接变化单列。两侧统计单位不同，不相加冒充 assert 宏数量。原非状态结果、返回码、权限、时间和产物断言均保留；pending token 被删除后的替换不再检验原展示优先级，已明确登记 S24。完整机读附表：[Rust](../../../experiments/constitution-host-preflight/r2/rust-assertions.json)；审计副本：[Rust](../../../experiments/constitution-host-preflight/r2/rust-assertions.md)、[Web](../../../experiments/constitution-host-preflight/r2/web-assertions.md)。

#### 逐项既有断言、轮询与新增别名检查

##### `rust/anchor-runner-host/src/api/tests.rs`

测试 / fixture：`graph_crud_round_trips_authoring_modules_and_layout`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA001 / 修改轮询 | 等待执行结束的轮询条件 | `HTTP OK && state.status != running && state.status != ready` | `HTTP OK && projection.phase != executing` | S19：新 phase/outcome 词汇 |
| RA002 / 修改断言 | 完成结局 | `detail[state][status] == completed` | `detail[projection][outcome] == done` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests.rs`

测试 / fixture：`trigger_persists_run_and_projects_list_detail_and_control`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA003 / 修改轮询 | 等待执行结束的轮询条件 | `HTTP OK && state.status != running && state.status != ready` | `HTTP OK && projection.phase != executing` | S19：新 phase/outcome 词汇 |
| RA004 / 修改断言 | 完成结局 | `detail[state][status] == completed` | `detail[projection][outcome] == done` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests.rs`

测试 / fixture：`paused_wait_child_resumes_independently_then_releases_parent`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA005 / 修改轮询 | 子 Run 停下的轮询条件 | `store child.status == RunStatus::Paused` | `store child.status == RunStatus::Stopped` | S25：pause 别名即时 stop |
| RA006 / 修改断言 | 子 Run 的存储状态 | `store child.status == RunStatus::Paused` | `store child.status == RunStatus::Stopped` | S25：pause 别名即时 stop |

##### `rust/anchor-runner-host/src/api/tests.rs`

测试 / fixture：`start_and_pause_request (fixture)`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA007 / 修改断言 | fixture 最终存储状态 | `record.status == RunStatus::Paused` | `record.status == RunStatus::Stopped` | S25：pause 别名即时 stop |
| RA008 / 新增断言 | 新增：别名的有效操作 | `无该断言` | `alias[operation] == stop` | S25：pause 别名即时 stop |
| RA009 / 新增断言 | 新增：响应说明别名 | `无该断言` | `alias[alias] == true` | S25：pause 别名即时 stop |

##### `rust/anchor-runner-host/src/api/tests.rs`

测试 / fixture：`downstream_failure_does_not_reclassify_committed_upstream_result`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA010 / 修改断言 | 失败结局 | `detail[state][status] == failed` | `detail[projection][outcome] == failed` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests.rs`

测试 / fixture：`waiting_parent_rejects_child_grant_drift_then_restores_same_child_without_replay`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA011 / 修改断言 | 停下后的子 Run 存储状态 | `child_before.status == RunStatus::Paused` | `child_before.status == RunStatus::Stopped` | S25：pause 别名即时 stop |

##### `rust/anchor-runner-host/src/api/tests/conversations.rs`

测试 / fixture：`conversation_resume_is_not_blocked_by_another_session_on_the_same_graph`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA012 / 修改轮询 | bob_run 的 wait_run_status 目标 | `RunStatus::Paused` | `RunStatus::Stopped` | S25：pause 别名即时 stop |

##### `rust/anchor-runner-host/src/api/tests/conversations.rs`

测试 / fixture：`superseded_conversation_cannot_resume_or_recover_its_wait_child`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA013 / 修改轮询 | child_run 的 wait_run_status 目标 | `RunStatus::Paused` | `RunStatus::Stopped` | S25：pause 别名即时 stop |

##### `rust/anchor-runner-host/src/api/tests/conversations.rs`

测试 / fixture：`superseded_conversation_keeps_detached_child_independent`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA014 / 修改轮询 | detached child_run 的 wait_run_status 目标 | `RunStatus::Paused` | `RunStatus::Stopped` | S25：pause 别名即时 stop |

##### `rust/anchor-runner-host/src/api/tests/conversations.rs`

测试 / fixture：`superseded_conversation_cannot_resume_or_recover_its_wait_child`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA015 / 修改断言 | 子 Run 对外相位 | `detail[state][status] == stopped` | `detail[projection][phase] == waiting` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/conversations.rs`

测试 / fixture：`conversation_orphan_running_requires_explicit_stop_before_a_new_turn`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA016 / 修改断言 | 无 worker 的 Running 对外相位 | `detail[state][status] == running` | `detail[projection][phase] == executing` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/kernel_probe.rs`

测试 / fixture：`explicit_kernel_probe_runs_real_host_ops_and_projects_existing_queries`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA017 / 修改断言 | 详情相位 | `detail[state][status] == completed` | `detail[projection][phase] == settled` | S19：新 phase/outcome 词汇 |
| RA018 / 修改断言 | 详情结局 | `detail[projection][terminal] == true` | `detail[projection][outcome] == done` | S19：新 phase/outcome 词汇 |
| RA019 / 修改断言 | 列表结局 | `list[runs][0][status] == completed` | `list[runs][0][projection][outcome] == done` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/kernel_probe.rs`

测试 / fixture：`kernel_read_view_http_tracks_running_human_wait_and_settlement_with_host_time`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA020 / 修改断言 | 运行中相位 | `running[projection][phase] == running` | `running[projection][phase] == executing` | S19：新 phase/outcome 词汇 |
| RA021 / 修改断言 | 运行中动作精确 JSON | `{pause:false,stop:true,resume:false}` | `{stop:true,resume:false,abandon:true}` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA022 / 修改断言 | 等待人相位 | `waiting[projection][phase] == stopped` | `waiting[projection][phase] == waiting` | S19：新 phase/outcome 词汇 |
| RA023 / 修改断言 | 等待人仍有 worker 的动作精确 JSON | `{pause:false,stop:false,resume:false}` | `{stop:false,resume:false,abandon:true}` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA024 / 修改断言 | 已放弃相位 | `settled[projection][phase] == aborted` | `settled[projection][phase] == settled` | S19：新 phase/outcome 词汇 |
| RA025 / 修改断言 | 已放弃结局 | `settled[projection][terminal] == true` | `settled[projection][outcome] == aborted` | S19：新 phase/outcome 词汇 |
| RA026 / 修改断言 | 已结算动作精确 JSON | `{pause:false,stop:false,resume:false}` | `{stop:false,resume:false,abandon:false}` | S22/S24：动作按相位推导、移除 pending-control 展示 |

##### `rust/anchor-runner-host/src/api/tests/platform_contract.rs`

测试 / fixture：`trigger_freezes_objective_and_manual_or_schedule_source`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA027 / 修改断言 | 去掉旧 pending-control 字段后的状态断言 | `detail[control_requested] == null` | `detail[projection][phase] == settled` | S22/S24：动作按相位推导、移除 pending-control 展示 |

##### `rust/anchor-runner-host/src/api/tests/platform_contract.rs`

测试 / fixture：`updated_tracks_persisted_pause_stop_and_resume_after_restart`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA028 / 修改断言 | pause 别名后的相位 | `paused[state][status] == paused` | `paused[projection][phase] == waiting` | S19：新 phase/outcome 词汇；S25：pause 别名即时 stop |
| RA029 / 修改断言 | 重启后状态（原查 pending-control） | `reopened[control_requested] == null` | `reopened[projection][phase] == waiting` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA030 / 修改断言 | stop 后相位 | `stopped[state][status] == stopped` | `stopped[projection][phase] == waiting` | S19：新 phase/outcome 词汇 |
| RA031 / 修改断言 | resume 完成后相位 | `completed[state][status] == completed` | `completed[projection][phase] == settled` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/run_abandon.rs`

测试 / fixture：`abandon_cancels_the_running_node_and_makes_the_run_terminal_aborted`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA032 / 修改断言 | abandon 响应结局 | `body[status] == aborted` | `body[projection][outcome] == aborted` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/run_abandon.rs`

测试 / fixture：`abandon_is_idempotent_and_rejects_terminal_unknown_or_invalid_requests`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA033 / 修改断言 | 默认 reason 的响应结局 | `defaulted[status] == aborted` | `defaulted[projection][outcome] == aborted` | S19：新 phase/outcome 词汇 |
| RA034 / 修改断言 | 已 aborted 的响应结局 | `existing[status] == aborted` | `existing[projection][outcome] == aborted` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/run_abandon.rs`

测试 / fixture：`abandon_keeps_the_assistant_binding_workspace_and_history_for_handover`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA035 / 修改断言 | 交接前 abandon 响应结局 | `abandoned[status] == aborted` | `abandoned[projection][outcome] == aborted` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/session_calls.rs`

测试 / fixture：`completed_session_execution_stays_waiting_until_delivery_settlement`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA036 / 修改断言 | 执行完成且 delivery pending 的相位 | `detail[state][status] == completed` | `detail[projection][phase] == settled` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/api/tests/session_calls.rs`

测试 / fixture：`completed_pending_wait_child_recovers_delivery_and_parent_after_restart`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA037 / 修改断言 | 子 Run 列表结局 | `child_projection[status] == completed` | `child_projection[projection][outcome] == done` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/application/kernel_runs/tests.rs`

测试 / fixture：`serial_probe_commits_artifacts_cost_and_phase_through_application`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA038 / 修改断言 | 串行 probe 完成 | `assert!(project_run(...).terminal)` | `assert!(project_run(...).phase == settled)` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/application/kernel_runs/tests.rs`

测试 / fixture：`abandon_waiting_probe_is_terminal_and_refuses_late_submission`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA039 / 修改断言 | 等待 probe 被放弃 | `assert!(project_run(...).terminal)` | `assert!(project_run(...).phase == settled)` | S19：新 phase/outcome 词汇 |

##### `rust/anchor-runner-host/src/application/liveness.rs`

测试 / fixture：`parked_or_unregistered_ready_runs_are_not_quiescent`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA040 / 修改断言 | RunStatus::Paused、worker=false | `RunLiveness::observed(status,false).is_quiescent() == false` | `RunLiveness::observed(status,false).is_quiescent() == true` | S21：Paused/BudgetStopped 统一为静止 |
| RA041 / 修改断言 | RunStatus::BudgetStopped、worker=false | `RunLiveness::observed(status,false).is_quiescent() == false` | `RunLiveness::observed(status,false).is_quiescent() == true` | S21：Paused/BudgetStopped 统一为静止 |

##### `rust/anchor-runner-host/src/application/run_view.rs`

测试 / fixture：`actions_cover_every_phase_waiter_and_outcome_with_and_without_worker`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA042 / 修改断言 | Phase::Executing、worker=false；AvailableActions.resume 分量 | `AvailableActions {stop:true,resume:false,abandon:true}` | `AvailableActions {stop:true,resume:true,abandon:true}` | S20：无 worker 的 Executing 可以 resume |

##### `rust/anchor-runner-host/src/application/run_projection.rs`

测试 / fixture：`inactive_states_reuse_control_guards_and_liveness`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA043 / 修改断言 | Ready、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA044 / 修改断言 | Ready、worker=false：注意项投影检查 | `projection.needs_attention == false` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA045 / 修改断言 | Ready、worker=false：静止检查读取来源 | `projection.quiescent == false` | `RunLiveness::observed(status,false).is_quiescent() == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA046 / 修改断言 | Running、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA047 / 修改断言 | Running、worker=false：注意项投影检查 | `projection.needs_attention == true` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA048 / 修改断言 | Running、worker=false：静止检查读取来源 | `projection.quiescent == false` | `RunLiveness::observed(status,false).is_quiescent() == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA049 / 修改断言 | Paused、worker=false：stop 表项 | `projection.actions.stop == true` | `projection.actions.stop == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA050 / 修改断言 | Paused、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA051 / 修改断言 | Paused、worker=false：注意项投影检查 | `projection.needs_attention == false` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA052 / 修改断言 | Paused、worker=false：静止检查读取来源 | `projection.quiescent == false` | `RunLiveness::observed(status,false).is_quiescent() == true` | S21：Paused/BudgetStopped 统一为静止 |
| RA053 / 修改断言 | WaitingCall、worker=false：resume 表项 | `projection.actions.resume == true` | `projection.actions.resume == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA054 / 修改断言 | WaitingCall、worker=false：stop 表项 | `projection.actions.stop == true` | `projection.actions.stop == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA055 / 修改断言 | WaitingCall、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA056 / 修改断言 | WaitingCall、worker=false：注意项投影检查 | `projection.needs_attention == false` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA057 / 修改断言 | WaitingCall、worker=false：静止检查读取来源 | `projection.quiescent == false` | `RunLiveness::observed(status,false).is_quiescent() == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA058 / 修改断言 | WaitingRecovery、worker=false：stop 表项 | `projection.actions.stop == true` | `projection.actions.stop == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA059 / 修改断言 | WaitingRecovery、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA060 / 修改断言 | WaitingRecovery、worker=false：注意项投影检查 | `projection.needs_attention == true` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA061 / 修改断言 | WaitingRecovery、worker=false：静止检查读取来源 | `projection.quiescent == false` | `RunLiveness::observed(status,false).is_quiescent() == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA062 / 修改断言 | BudgetStopped、worker=false：resume 表项 | `projection.actions.resume == false` | `projection.actions.resume == true` | S23：取消旧恢复/预算 resume 展示限制 |
| RA063 / 修改断言 | BudgetStopped、worker=false：stop 表项 | `projection.actions.stop == true` | `projection.actions.stop == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA064 / 修改断言 | BudgetStopped、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA065 / 修改断言 | BudgetStopped、worker=false：注意项投影检查 | `projection.needs_attention == true` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA066 / 修改断言 | BudgetStopped、worker=false：静止检查读取来源 | `projection.quiescent == false` | `RunLiveness::observed(status,false).is_quiescent() == true` | S21：Paused/BudgetStopped 统一为静止 |
| RA067 / 修改断言 | Stopped、worker=false：stop 表项 | `projection.actions.stop == true` | `projection.actions.stop == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA068 / 修改断言 | Stopped、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA069 / 修改断言 | Stopped、worker=false：注意项投影检查 | `projection.needs_attention == false` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA070 / 修改断言 | Stopped、worker=false：静止检查读取来源 | `projection.quiescent == true` | `RunLiveness::observed(status,false).is_quiescent() == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA071 / 修改断言 | Completed、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA072 / 修改断言 | Completed、worker=false：注意项投影检查 | `projection.needs_attention == false` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA073 / 修改断言 | Completed、worker=false：静止检查读取来源 | `projection.quiescent == true` | `RunLiveness::observed(status,false).is_quiescent() == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA074 / 修改断言 | Failed、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA075 / 修改断言 | Failed、worker=false：注意项投影检查 | `projection.needs_attention == true` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA076 / 修改断言 | Failed、worker=false：静止检查读取来源 | `projection.quiescent == true` | `RunLiveness::observed(status,false).is_quiescent() == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA077 / 修改断言 | Aborted、worker=false：pause 检查替换为 abandon | `assert!(!projection.actions.pause)` | `projection.actions.abandon == false` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA078 / 修改断言 | Aborted、worker=false：注意项投影检查 | `projection.needs_attention == true` | `(phase==waiting \|\| outcome∈{failed,aborted} \|\| (phase==executing && !active)) == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA079 / 修改断言 | Aborted、worker=false：静止检查读取来源 | `projection.quiescent == true` | `RunLiveness::observed(status,false).is_quiescent() == true` | S22/S24：动作按相位推导、移除 pending-control 展示 |

##### `rust/anchor-runner-host/src/application/run_projection.rs`

测试 / fixture：`pending_control_and_session_coordination_are_observed_once`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA080 / 修改断言 | active=true、requested=pause 的相位 | `projection.phase == pausing` | `projection.phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA081 / 修改断言 | active=true、requested=pause 的动作字段 | `assert!(!projection.actions.pause)` | `assert!(projection.actions.abandon)` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA082 / 修改断言 | active=true、requested=stop 的相位 | `projection.phase == stopping` | `projection.phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA083 / 修改断言 | active=true、requested=stop 的动作可见性 | `assert!(!projection.actions.stop)` | `assert!(projection.actions.stop)` | S22/S24：动作按相位推导、移除 pending-control 展示 |

##### `rust/anchor-runner-host/src/application/run_projection.rs`

测试 / fixture：`legacy_projection_serialization_preserves_ready_paused_and_stopped_bytes`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA084 / 修改断言 | RunStatus::Ready：serde_json::to_vec(...) == expected.as_bytes() | `{"phase":"ready","active":false,"control_requested":null,"terminal":false,"quiescent":false,"needs_attention":false,"actions":{"pause":false,"stop":true,"resume":true}}` | `{"phase":"executing","reason":null,"active":false,"actions":{"stop":true,"resume":true,"abandon":true},"updated":null}` | S19：新 phase/outcome 词汇；S22/S24：动作按相位推导、移除 pending-control 展示；S26：updated 可空 |
| RA085 / 修改断言 | RunStatus::Paused：serde_json::to_vec(...) == expected.as_bytes() | `{"phase":"paused","active":false,"control_requested":null,"terminal":false,"quiescent":false,"needs_attention":false,"actions":{"pause":false,"stop":true,"resume":true}}` | `{"phase":"waiting","waiting_for":"human","reason":"paused","active":false,"actions":{"stop":false,"resume":true,"abandon":true},"updated":null}` | S19：新 phase/outcome 词汇；S22/S24：动作按相位推导、移除 pending-control 展示；S26：updated 可空 |
| RA086 / 修改断言 | RunStatus::Stopped：serde_json::to_vec(...) == expected.as_bytes() | `{"phase":"stopped","active":false,"control_requested":null,"terminal":false,"quiescent":true,"needs_attention":false,"actions":{"pause":false,"stop":true,"resume":true}}` | `{"phase":"waiting","waiting_for":"human","reason":"stopped","active":false,"actions":{"stop":false,"resume":true,"abandon":true},"updated":null}` | S19：新 phase/outcome 词汇；S22/S24：动作按相位推导、移除 pending-control 展示；S26：updated 可空 |

##### `rust/anchor-runner-host/src/application/tests.rs`

测试 / fixture：`pending_control_projects_active_tokens_with_stop_priority`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA087 / 修改断言 | 缺少 worker 的 missing record | `project_run(...).control_requested == None` | `project_run(...).phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA088 / 修改断言 | 已登记 worker、未请求控制 | `project_run(...).control_requested == None` | `project_run(...).phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA089 / 修改断言 | pause flag=true | `project_run(...).control_requested == Some(pause)` | `project_run(...).phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA090 / 修改断言 | pause+cancellation flags=true；原 stop 优先 | `project_run(...).control_requested == Some(stop)` | `project_run(...).phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |
| RA091 / 修改断言 | 移除 worker 后 | `project_run(...).control_requested == None` | `project_run(...).phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |

##### `rust/anchor-runner-host/src/application/session_calls.rs`

测试 / fixture：`stopped_parent_cancels_wait_delivery (fixture) → active_parent_stop_cancels_wait_session_delivery_but_not_detach`。

| ID / 类型 | 情形 | 旧断言或目标 | 新断言或目标 | S 候选分组 |
| --- | --- | --- | --- | --- |
| RA092 / 修改断言 | active_parent=true，stop 后仍排空的父 Run | `project_run(parent,...).control_requested == Some(stop)` | `project_run(parent,...).phase == executing` | S22/S24：动作按相位推导、移除 pending-control 展示 |

#### fixture 关联测试

`start_and_pause_request` 直接或经 `start_and_pause` 影响以下既有测试；本表中的 fixture 断言只计一次，未重复算到每个调用者：

- `api/tests.rs::pause_resume_uses_same_frozen_run_after_graph_edit_and_service_object_restart`
- `api/tests/platform_contract.rs::busy_schedule_creates_no_run_or_source_metadata`
- `api/tests/platform_contract.rs::updated_tracks_persisted_pause_stop_and_resume_after_restart`
- `api/tests/schedules.rs::busy_due_schedule_skips_existing_admission_without_creating_a_run`
- `api/tests/run_abandon.rs::abandon_leaves_other_runs_and_their_recovery_entry_points_unchanged`

`application/session_calls.rs::stopped_parent_cancels_wait_delivery(true)` 的改动断言由 `active_parent_stop_cancels_wait_session_delivery_but_not_detach` 执行；false 分支的存储状态断言未改，关联测试为 `waiting_parent_stop_cancels_completed_pending_session_child_without_waking_parent`。

#### 动态表完整行变化核对

`application/run_projection.rs::inactive_states_reuse_control_guards_and_liveness` 的表元组列为 `(resume, stop, attention, quiescent)`；这里保留所有 10 行，避免把没修改的分量误报为修改。上文逐项 attention 新值使用实际表达式：Ready 还会经 `attention || status==Ready` 得到 true。

| RunStatus | 旧表元组 | 新表元组 | 实际新 attention |
| --- | --- | --- | --- |
| Ready | `(true, true, false, false)` | `(true, true, false, false)` | true |
| Running | `(true, true, true, false)` | `(true, true, true, false)` | true |
| Paused | `(true, true, false, false)` | `(true, false, true, true)` | true |
| WaitingCall | `(true, true, false, false)` | `(false, false, true, false)` | true |
| WaitingRecovery | `(false, true, true, false)` | `(false, false, true, false)` | true |
| BudgetStopped | `(false, true, true, false)` | `(true, false, true, true)` | true |
| Stopped | `(true, true, false, true)` | `(true, false, true, true)` | true |
| Completed | `(false, false, false, true)` | `(false, false, false, true)` | false |
| Failed | `(false, false, true, true)` | `(false, false, true, true)` | true |
| Aborted | `(false, false, true, true)` | `(false, false, true, true)` | true |

`application/run_view.rs::actions_cover_every_phase_waiter_and_outcome_with_and_without_worker` 保留 9 相位/等待者/结局 × 2 worker 的 18 组合。仅 Executing / worker=false 的 resume 从 false 改 true；其余 17 组合以及全部 stop/abandon 预期未改。

`application/liveness.rs::parked_or_unregistered_ready_runs_are_not_quiescent` 保留 10 旧状态 × 2 worker 的 20 组合。仅 Paused、BudgetStopped 的无 worker 静止预期从 false 改 true；所有有 worker 的非静止断言未改。

#### 新增测试（不混计既有断言修改）

文件：`rust/anchor-runner-host/src/application/legacy_run_view.rs`。测试：`every_legacy_status_maps_lossily_with_both_worker_observations`。

对下表每一旧状态分别执行 worker=false / true，精确断言 `(phase,waiting_for,outcome,reason,active)`；active 必须等于 worker。

| 旧 RunStatus | phase | waiting_for | outcome | reason | worker 覆盖 |
| --- | --- | --- | --- | --- | --- |
| Ready | executing | None | None | None | false、true |
| Running | executing | None | None | None | false、true |
| Paused | waiting | human | None | paused | false、true |
| Stopped | waiting | human | None | explanation | false、true |
| BudgetStopped | waiting | human | None | budget | false、true |
| WaitingRecovery | waiting | effect | None | explanation | false、true |
| WaitingCall | waiting | child | None | explanation | false、true |
| Completed | settled | None | done | None | false、true |
| Failed | settled | None | failed | explanation | false、true |
| Aborted | settled | None | aborted | explanation | false、true |

每组合另有七个独立断言：

1. `actions.stop == (phase==executing)`。
2. `actions.resume == (!worker && (phase==executing || waiting_for==human))`。
3. `actions.abandon == (phase!=settled)`。
4. `waiting_for` 字段是否存在与 waiter 是否存在一致。
5. `outcome` 字段是否存在与 outcome 是否存在一致。
6. 不存在旧 `status` 字段。
7. 不存在旧 `actions.pause` 字段。

此外单独断言 `Stopped` 无 error 时映射为 `Waiting::Human {reason:"stopped"}`。新增测试共 9 个源码 assert 宏（循环内 8 个，循环外 1 个），展开后 161 次断言。归属：新词汇、动作按相位推导、无 worker Executing resume、S23：取消旧恢复/预算 resume 展示限制。


#### Web 逐项断言

| 编号 | 文件 | 测试名 | 原断言 | 新断言 | 行为分组 |
| --- | --- | --- | --- | --- | --- |
| W1 | `apps/web/e2e/goose-native-trace.spec.ts` | real Rust Host and Goose retain active trace, canonical completion and recorded Plugin metadata after refresh | `expect(during.state.status).toBe('running')` | `expect(during.projection?.phase).toBe('executing')` | S19/S20：Run 新词汇/worker 观察 |
| W2 | `apps/web/e2e/goose-native-trace.spec.ts` | real Rust Host and Goose retain active trace, canonical completion and recorded Plugin metadata after refresh | `expect.poll(async () => (await (await request.get(\`${base}/runs/${run}\`)).json()).state.status).toBe('completed')` | `expect.poll(async () => (await (await request.get(\`${base}/runs/${run}\`)).json()).projection.outcome).toBe('done')` | S19/S20：Run 新词汇/worker 观察 |
| W3 | `apps/web/e2e/graph-calls-real.spec.ts` | real backend: author wait and detach calls, follow results and concurrent run chains | `expect(detail.state.status).toBe('running')` | `expect(detail.projection?.phase).toBe('executing')` | S19/S22/S23/S24：子 Run phase/outcome 与三动作 |
| W4 | `apps/web/e2e/graph-calls-real.spec.ts` | real backend: author wait and detach calls, follow results and concurrent run chains | `expect(detail.calls[0].status).toBe('running')` | `expect(detail.calls[0].projection.phase).toBe('executing')` | S19/S22/S23/S24：子 Run phase/outcome 与三动作 |
| W5 | `apps/web/e2e/graph-calls-real.spec.ts` | real backend: author wait and detach calls, follow results and concurrent run chains | `expect(waitDetail.state.status).toBe('completed')` | `expect(waitDetail.projection?.outcome).toBe('done')` | S19/S22/S23/S24：子 Run phase/outcome 与三动作 |
| W6 | `apps/web/e2e/graph-calls-real.spec.ts` | real backend: author wait and detach calls, follow results and concurrent run chains | `expect(detail.state.status).toBe('completed')` | `expect(detail.projection?.outcome).toBe('done')` | S19/S22/S23/S24：子 Run phase/outcome 与三动作 |
| W7 | `apps/web/e2e/graph-calls-real.spec.ts` | real backend: author wait and detach calls, follow results and concurrent run chains | `expect(detail.calls[0].status).toBe('running')` | `expect(detail.calls[0].projection.phase).toBe('executing')` | S19/S22/S23/S24：子 Run phase/outcome 与三动作 |
| W8 | `apps/web/e2e/graph-calls-real.spec.ts` | real backend: author wait and detach calls, follow results and concurrent run chains | `expect((await json(request, \`${base}/runs/${detail.calls![0].run}\`)).state.status).toBe('completed')` | `expect((await json(request, \`${base}/runs/${detail.calls![0].run}\`)).projection.outcome).toBe('done')` | S19/S22/S23/S24：子 Run phase/outcome 与三动作 |
| W9 | `apps/web/e2e/graph-calls.spec.ts` | mocked: show wait/detach child Runs, source links and active controls | `expect(page.getByRole('button', { name: '暂停' })).toBeVisible()` | `expect(page.getByRole('button', { name: '放弃', exact: true })).toBeVisible()` | S19/S22/S23/S24：子 Run phase/outcome 与三动作 |
| W10 | `apps/web/e2e/parallel-real.spec.ts` | real backend: paired authoring, simultaneous native nodes, joined artifacts and invalid-pair rejection | `expect(detail.state.status).toBe('completed')` | `expect(detail.projection?.outcome).toBe('done')` | S19/S20：Run 新词汇/worker 观察 |
| W11 | `apps/web/e2e/parallel.spec.ts` | mocked: show simultaneous branch execution and pending join in one Run | `expect(page.locator('.execution-node.state-running')).toHaveCount(2)` | `expect(page.locator('.execution-node.state-executing.is-live')).toHaveCount(2)` | S19/S20：Run 新词汇/worker 观察 |
| W12 | `apps/web/e2e/recovery.spec.ts` | unknown tool result resumes through the normal Agent context path | `expect(page.getByText('Agent 上次工具调用的结果未记录')).toBeVisible()` | `expect(page.locator('.canvas-head .pill', { hasText: '等待你处理：Agent 上次工具调用的结果未记录' })).toBeVisible()` | S19/S20：Run 新词汇/worker 观察 |
| W13 | `apps/web/e2e/recovery.spec.ts` | inactive running Run can be resumed from its existing detail page | `expect(page.locator('.execution-node.state-running')).toHaveCount(0)` | `expect(page.locator('.execution-node.is-live')).toHaveCount(0)` | S19/S20：Run 新词汇/worker 观察 |
| W14 | `apps/web/e2e/recovery.spec.ts` | inactive running Run can be resumed from its existing detail page | `expect(page.locator('.execution-node.state-waiting_resume')).toHaveCount(1)` | `expect(page.locator('.execution-node.state-executing')).toHaveCount(1)` | S19/S20：Run 新词汇/worker 观察 |
| W15 | `apps/web/e2e/recovery.spec.ts` | inactive running Run can be resumed from its existing detail page | `expect(page.getByText('宿主重启后尚未接续')).toBeVisible()` | `expect(page.locator('[aria-label="活动节点"]')).toContainText('等待接续')` | S19/S20：Run 新词汇/worker 观察 |
| W16 | `apps/web/src/ChannelsPage.test.tsx` | waits for the executor to exit before allowing retirement and binds it to the current Run | `expect(container.textContent).toContain('正在停止…')` | `expect(container.textContent).toContain('等待你处理：stopped')` | S19/S21：等待人/worker 排空/静止语义 |
| W17 | `apps/web/src/ChannelsPage.test.tsx` | does not infer resumability when controls are denied and uses human quiescence for retirement | `expect(button('退役实例').disabled).toBe(true)` | `expect(button('退役实例').disabled).toBe(false)` | S19/S21：等待人/worker 排空/静止语义 |
| W18 | `apps/web/src/RunDetailPage.test.tsx` | exposes the server-projected actions for %s | `expect(buttons.map(button => button.textContent)).toEqual(['停止', '继续'])` | `expect(buttons.map(button => button.textContent)).toEqual(['停止', '继续', '放弃'])` | S19/S22/S23/S24：统一等待文案/三动作 |
| W19 | `apps/web/src/RunDetailPage.test.tsx` | exposes the server-projected actions for %s | `expect(control.mock.calls).toEqual([['stop'], ['resume']])` | `expect(control.mock.calls).toEqual([['stop'], ['resume'], ['abandon']])` | S19/S22/S23/S24：统一等待文案/三动作 |
| W20 | `apps/web/src/RunDetailPage.test.tsx` | exposes the server-projected actions for %s | `expect(element.textContent).toContain(status === 'waiting_call' ? '等待调用结果' : status === 'ready' ? '等待执行' : '等待恢复上下文')` | `expect(element.textContent).toContain(label)` | S19/S22/S23/S24：统一等待文案/三动作 |
| W21 | `apps/web/src/RunDetailPage.test.tsx` | uses the projection for cursor and control hints despite conflicting legacy fields | `expect(element.querySelector('[aria-label="活动节点"]')?.textContent).toContain('宿主重启后尚未接续')` | `expect(element.querySelector('[aria-label="活动节点"]')?.textContent).toContain('等待接续')` | S19/S22/S23/S24：统一等待文案/三动作 |
| W22 | `apps/web/src/Timeline.test.ts` | keeps the restart message for a Run the host no longer owns | `expect(factsFor({ running: false, status: 'running', updated: '2026-09-29T10:30:00', projection: { phase: 'waiting_resume', active: false, control_requested: null, terminal: false, quiescent: false, needs_attention: true, actions: { pause: false, stop: true, resume: true }, } })).toEqual({ label: '运行状态', value: '宿主重启后等待接续', })` | `expect(factsFor({ running: false, updated: '2026-09-29T10:30:00', projection: { phase: 'executing', active: false, reason: null, updated: null, actions: { stop: true, resume: true, abandon: true }, } })).toEqual({ label: '运行状态', value: '宿主重启后等待接续', })` | S19/S20：Timeline phase/attention |
| W23 | `apps/web/src/Timeline.test.ts` | reports a resident segment by its own window, not the Run lifetime | `expect(factsFor({ running: false, status: 'stopped', activity: [] }, { resident: true, end: +new Date('2026-09-28T09:00:00') })).toEqual({ label: '这段时长', value: '暂无记录', end: '暂无记录', })` | `expect(factsFor({ running: false, activity: [], projection: { phase: 'waiting', waiting_for: 'human', active: false, reason: 'stopped', updated: null, actions: { stop: false, resume: true, abandon: true } } }, { resident: true, end: +new Date('2026-09-28T09:00:00') })).toEqual({ label: '这段时长', value: '暂无记录', end: '暂无记录', })` | S19/S20：Timeline phase/attention |
| W24 | `apps/web/src/Timeline.test.ts` | includes waiting recovery in the same attention filter used by the Timeline | `expect(entry.status).toBe('waiting_recovery')` | `expect(entry.status).toBe('waiting')` | S19/S20：Timeline phase/attention |
| W25 | `apps/web/src/parallel.test.ts` | shows both active branches, animates both selected inputs, and leaves join pending | `expect(nodes.filter(node => node.data.state === 'running').map(node => node.id)).toEqual(['branch-a', 'branch-b'])` | `expect(nodes.filter(node => node.data.state === 'executing' && node.data.live).map(node => node.id)).toEqual(['branch-a', 'branch-b'])` | S19/S20：Run 新词汇/worker 观察 |
| W26 | `apps/web/src/parallel.test.ts` | shows both active branches, animates both selected inputs, and leaves join pending | `expect(toFlowNodes(region, 'parallel', { ...state, active: {} }, observation).filter(node => node.data.state === 'running')).toEqual([])` | `expect(toFlowNodes(region, 'parallel', { ...state, active: {} }, observation).filter(node => node.data.live)).toEqual([])` | S19/S20：Run 新词汇/worker 观察 |
| W27 | `apps/web/src/parallel.test.ts` | projects native Rust branch cursors while leaving completed branches and join inactive | `expect(toFlowNodes(region, 'parallel', native, observation).filter(node => node.data.state === 'running').map(node => node.id)) .toEqual(['branch-a', 'branch-b'])` | `expect(toFlowNodes(region, 'parallel', native, observation).filter(node => node.data.state === 'executing' && node.data.live).map(node => node.id)) .toEqual(['branch-a', 'branch-b'])` | S19/S20：Run 新词汇/worker 观察 |
| W28 | `apps/web/src/parallel.test.ts` | keeps retained %s branch cursors quiet instead of rendering execution | `expect(nodes.filter(node => node.data.state === phase).map(node => node.id)).toEqual(['branch-a', 'branch-b'])` | `expect(nodes.filter(node => node.data.state === view.phase).map(node => node.id)).toEqual(['branch-a', 'branch-b'])` | S19/S20：Run 新词汇/worker 观察 |
| W29 | `apps/web/src/parallel.test.ts` | keeps retained %s branch cursors quiet instead of rendering execution | `expect(nodes.filter(node => node.data.state === 'running')).toEqual([])` | `expect(nodes.filter(node => node.data.live)).toEqual([])` | S19/S20：Run 新词汇/worker 观察 |
| W30 | `apps/web/src/parallel.test.ts` | keeps unknown Run observations quiet while retaining definition-only editor nodes | `expect(unknown.filter(node => node.data.state === 'running')).toEqual([])` | `expect(unknown.filter(node => node.data.live)).toEqual([])` | S19/S20：Run 新词汇/worker 观察 |

参数化断言的具体取值（同一表达式的每个实例）：

| 文件/测试 | 原预期 | 新预期 | 行为分组 |
| --- | --- | --- | --- |
| RunDetailPage / exposes… / waiting_call | 按钮停止、继续；调用stop/resume；等待调用结果 | 按钮停止、继续、放弃；调用stop/resume/abandon；waiting/child 显示等待子任务 | S19/S22/S23/S24：三动作/等待词汇 |
| RunDetailPage / exposes… / ready | 按钮停止、继续；调用stop/resume；等待执行 | 按钮停止、继续、放弃；调用stop/resume/abandon；executing+inactive 显示等待接续 | S19/S20/S22：三动作/Ready合并 |
| RunDetailPage / exposes… / waiting_recovery | 按钮停止、继续；调用stop/resume；等待恢复上下文 | 按钮停止、继续、放弃；调用stop/resume/abandon；waiting/effect+reason 显示等待核查效果：工具结果未记录 | S19/S22/S23/S24：三动作/等待词汇；此fixture故意独立给动作事实以检查UI不自行授权 |
| RunInspector / uses… / waiting_resume / node-summary | 等待接续 | executing+inactive 仍显示等待接续 | S20：worker观察 |
| RunInspector / uses… / waiting_resume / call-records | 本节点：等待接续 | 本节点：等待接续 | S20：worker观察 |
| RunInspector / uses… / waiting_recovery / node-summary | 等待恢复上下文 | 等待核查效果：工具结果未记录 | S19：统一reason |
| RunInspector / uses… / waiting_recovery / call-records | 本节点：等待恢复上下文 | 本节点：等待核查效果：工具结果未记录 | S19：统一reason |
| RunInspector / uses… / paused / node-summary | 已暂停 | 等待你处理：paused | S19/S21：Human统一 |
| RunInspector / uses… / paused / call-records | 本节点：已暂停 | 本节点：等待你处理：paused | S19/S21：Human统一 |
| parallel / keeps retained… / waiting_resume / branch identities | 两分支state=waiting_resume | 两分支state=executing，inactive | S20：worker观察 |
| parallel / keeps retained… / waiting_resume / live identities | state=running的节点为空 | live=true的节点为空 | S20：worker观察 |
| parallel / keeps retained… / waiting_recovery / branch identities | 两分支state=waiting_recovery | 两分支state=waiting，waiting_for=effect | S19：等待词汇 |
| parallel / keeps retained… / waiting_recovery / live identities | state=running的节点为空 | live=true的节点为空 | S20：worker观察 |
| parallel / keeps retained… / paused / branch identities | 两分支state=paused | 两分支state=waiting，waiting_for=human，reason=paused | S19/S21：Human统一 |
| parallel / keeps retained… / paused / live identities | state=running的节点为空 | live=true的节点为空 | S20：worker观察 |
| parallel / keeps retained… / stopped / branch identities | 两分支state=stopped | 两分支state=waiting，waiting_for=human，reason=stopped | S19/S21：Human统一 |
| parallel / keeps retained… / stopped / live identities | state=running的节点为空 | live=true的节点为空 | S20：worker观察 |

fixture 的间接断言变化与未覆盖边界：

- `Timeline.test.ts / includes waiting recovery…`：原通过手工切换 needs_attention=false 检查attention；新将 waiting/effect 切换为 settled/done 后仍断言false，禁止隐藏的attention旗标。原true/false断言均保留。
- `RunDetailPage.test.ts / shows recovery guidance…`：原恢复专用文案来自waiting_recovery分支；现只来自projection.reason。通过state.error带旧文案但projection无该reason验证不泄露旧字段；原正反文案断言保留。
- `ChannelsPage.test.ts / does not infer resumability…`：旧stopped但quiescent=false的fixture现在映射waiting/human+inactive，按新语义允许退役（disabled=true→false）；Run controls仍全部拒绝。属于Human静止统一的可见行为变化。
- `e2e/recovery.spec.ts / unknown tool result resumes…`：旧waiting_recovery fixture允许resume；为保留原resume成功操作验收，新fixture显式waiting/human并携同一恢复说明。它只验证人等待后的接续，不证明effect等待可以resume；effect按新动作规则不允许resume，停止条件后未新增独立effect组件测试。恢复副作用、产物、实际provider未由这个mock验证。
- `RunDetailPage / exposes…` 的三个fixture都故意提供stop/resume/abandon=true，验证UI只读Host动作；不代表这些phase真实Host都会授权这三项。
- 其余typed Run/CallRecord、mocked e2e的projection只将旧Run词汇与flags迁为新字段；Session Turn、native branch status、listener、节点完成/中断事实不迁移。原非状态断言全部保留。
- `Timeline` 的finished类选择器迁为settled（原宽度、时间、几何断言未变）；AST清单只统计expect表达式，selector绑定在变量赋值中的变化需按此补记。


## R2 审查结论（2026-10-19）

审查方复现 API 失败（10+ 项，均与 pause 语义或旧状态字段相关），内核 diff 为空。R2 未验收。

1. **pause→stop 是任务书的设计错误，不是实现问题。** 旧 pause 的含义是"当前节点做完后停在边界"，这是宿主调度意图，不是一个状态。收敛的对象是状态词汇，不是控制动作：pause 应保留为动作（Executing 且有 worker 时可用，由宿主在节点边界提交 stop），结果状态是 waiting / human / reason=paused。S25 作废。
2. **测量的判定标准本身达不到。** conversations 一个消费者在基线就占 59/82 点，几乎全是 Session、授权、交接和投递，宪法从未打算消除它们；无论读模型如何设计，所选消费者都不可能减少 50%。P、R、R2 三次合起来的结论是：写入与生命周期一侧的定律成立；Host 读侧的复杂度主要在 Session/授权层，不随运行时收敛而消失。
3. 因此不再继续读侧探针。是否全量迁移改为按"统一定律、崩溃与计费正确性、删除旧 Runner"的价值决定，而不是按"Host 变小"决定。

**用户决定（2026-10-19）**：全量迁移，见[全量迁移任务书](constitution-migration-task.md)。

## M1：完成 R2 对外词汇（2026-10-19）

按 [全量迁移任务书](constitution-migration-task.md) 交回 M1。pause 恢复为独立宿主控制：旧 Run 保持原边界暂停；Kernel Run 持久保存意图，经事件在当前节点结果/成本提交后、下一派发前提交 Human stop，reason=paused。重启先恢复端口已有完成事实；最终节点已结算就保留 Done。stop 仍立即取消。控制与边界消费共用锁，旧意图清理不删除后来接受的意图。未修改 Kernel 或生产 Validator，未增加执行标志轮询。

HTTP、Pilot、Web 共用四动作；Session call 通用 pause/resume 仍拒绝。物理静止不等于 Session 可交接：conversation 暂停或预算等待时保留当前轮占有，显式 stop 或结算后才允许交接/删除；原 409、产物、不重放与投递断言保持。已删除仅供旧调用点使用的 `observed_liveness` 包装，直接复用同一 RunView 与静止规则。

本地验证：workspace 1922 通过、0 失败、115 既有忽略；Web 290 通过、build 通过，fmt/clippy/C1 通过。结果与命令见 [M1 验收](../../../experiments/constitution-host-preflight/m1/verification.md)。真实 Goose/Provider、native 浏览器端到端未执行，不作为已验收能力。M2–M5 尚未开始；不再以读侧判断点下降 50% 决定迁移，R/R2 历史测量保持原样。

### S 候选

S25 按任务书作废；无新增 S 编号。其余 7 条（S19–S24、S26）**已由用户批准（2026-10-19）**。

| 编号 | 当前结论 / 待审查边界 |
| --- | --- |
| S19 | 三相位、waiter/outcome/reason 取代旧 Run 状态与专用提示；旧客户端字段破坏，Ready 进入接续/注意展示 |
| S20 | Executing 无 worker 可恢复驱动，旧 Ready 与孤立 Running 共用规则 |
| S21 | Human 无 worker 统一物理静止；conversation 当前轮的占有/交接约束留在 Session 层，原暂停 409 保留；准入、退役与清理的其余影响仍待审查 |
| S22 | stop 仅 Executing；未结算增加 abandon；M1 按已批准修正恢复 pause=(Executing && worker) |
| S23 | Human 无 worker 可 resume，Child/Effect 不可；未迁旧预算写入守卫仍可能拒绝，展示与写入边界待审查 |
| S24 | 无 pausing/stopping/control_requested 展示，请求未落相位前仍显示 Executing；实际暂停接受由 operation=pause 回执确认 |
| S26 | 缺 Kernel 时间观察为 updated:null；旧 mtime 来源保留，时间不参与控制 |

### 断言修改

计数为既有期望表达式/目标位置，参数展开在逐项登记内列清：Rust **26 处**（20 处断言期望、6 处轮询目标），Web **4 处**；fixture 仅增补 actions.pause 的改动另列。新增 Rust 8 项、Web 5 项测试，无业务断言放宽。RSI HTTP MCP 测试仅显式初始化 workspace 既用的 ring crypto provider，原工具发现和冻结证据读取断言不变。

| 登记 | 测试 / 旧 → 新 / 依据 |
| --- | --- |
| MR01 | api/tests.rs / paused_wait_child_resumes_independently_then_releases_parent / 轮询；child.status Stopped → Paused；M1 纠正 S25，恢复边界暂停 |
| MR02 | 同上 / 结果断言；child.status Stopped → Paused；同上；恢复和父子释放断言保留 |
| MR03 | api/tests.rs / start_and_pause_request / 响应 operation；stop → pause；M1 独立控制动作 |
| MR04 | 同上 / 响应 alias；alias=true → 不含 alias；M1 取消别名协议 |
| MR05 | 同上 / 存储状态；Stopped → Paused；M1；原 results.len()==1 不变 |
| MR06 | api/tests.rs / waiting_parent_rejects_child_grant_drift_then_restores_same_child_without_replay；child_before.status Stopped → Paused；M1；授权拒绝、产物、首结果与不重放断言不变 |
| MR07 | api/tests/conversations.rs / conversation_resume_is_not_blocked_by_another_session_on_the_same_graph / 轮询；Bob Stopped → Paused；M1；下一轮仍 409、原结果与调用次数不变 |
| MR08 | 同文件 / superseded_conversation_cannot_resume_or_recover_its_wait_child / 轮询；暂停后 child Stopped → Paused；M1；随后显式 stop 仍 Stopped，旧轮恢复拒绝不变 |
| MR09 | 同文件 / superseded_conversation_keeps_detached_child_independent / 轮询；暂停后 detached child Stopped → Paused；M1；随后显式 stop 及 detach 边界不变 |
| MR10 | api/tests/kernel_probe.rs / kernel_read_view_http_tracks_running_human_wait_and_settlement_with_host_time / Executing actions；stop/resume/abandon 原值保留，增加 pause=true；M1；有 worker 的 Executing 可暂停 |
| MR11 | 同上 / Waiting actions；原三动作保留，增加 pause=false；M1；等待时不可暂停 |
| MR12 | 同上 / Settled actions；原三动作保留，增加 pause=false；M1；结算时不可暂停 |
| MR13 | application/legacy_run_view.rs / every_legacy_status_maps_lossily_with_both_worker_observations；actions 无 pause → pause==(executing && worker)；M1；10 状态 × 2 worker，共 20 个参数实例 |
| MR14 | application/run_projection.rs / legacy_projection_serialization_preserves_ready_paused_and_stopped_bytes / Ready；精确字节 JSON actions 增加 pause:false；M1；其余字段字节不变 |
| MR15 | 同上 / Paused；精确字节 JSON actions 增加 pause:false；同上 |
| MR16 | 同上 / Stopped；精确字节 JSON actions 增加 pause:false；同上 |
| MR17 | application/run_view.rs / actions_cover_every_phase_waiter_and_outcome_with_and_without_worker；AvailableActions 增加 pause==(Executing && worker)；M1；9 相位/waiter/outcome × 2 worker，18 个参数实例 |
| MR18 | api/runs/tests.rs / list_and_detail_share_status_control_and_attention_projection / phase 参数期望；ready/waiting_call/waiting_resume/waiting_recovery/budget_stopped/completed → executing/waiting/executing/waiting/waiting/settled；S19/S20；六实例见逐项登记 |
| MR19 | 同上 / resume 参数期望；WaitingCall、WaitingRecovery true→false；BudgetStopped false→true，其余不变；S23；只改动作投影期望，旧写入守卫未动 |
| MR20 | 同上 / stop 参数期望；WaitingCall、WaitingRecovery、BudgetStopped true→false，其余不变；S22；Executing 才可 stop |
| MR21 | 同上 / attention 断言；projection.needs_attention → waiting 或 executing+inactive 的事实推导；Ready/WaitingCall false→true；S19/S20；同时断言旧字段不存在，六实例见逐项登记 |
| MR22 | 同文件 / incomplete_run_identity_does_not_offer_inactive_controls / detail；三个动作 false → 四个动作 false，增加 abandon=false；S22；身份拒绝保持全部动作不可用 |
| MR23 | 同上 / list；三个动作 false → 四个动作 false，增加 abandon=false；同上；不放宽授权拒绝 |
| MR24 | tests/standard_host / standard_host_runs_op_without_goose_model_or_runtime_switch：失败断言 state.status!=failed → projection.outcome!=failed；S19，产物断言保留 |
| MR25 | 同上 / 完成轮询 state.status=completed → projection settled/done；S19 |
| MR26 | 同文件 / old_state_root_still_allows_op_without_becoming_goose_recovery：完成轮询 state.status=completed → projection settled/done；S19，旧事实断言保留 |
| MW01 | platform-rust / native Rust Host…：Run pill 已暂停 → 等待你处理：paused；M1/S25 作废 |
| MW02 | 同上 / scheduled Run：status=completed → projection settled/done；S19/S20 |
| MW03 | pilot-rust / Goose Pilot controls…：control_requested=pause → projection executing+active；新增独立 operation=pause 工具回执检查；S24/M1 |
| MW04 | 同上 / Run pill：已暂停 → 等待你处理：paused；M1/S25 作废 |

逐项参数、原/新值与 fixture schema 清单：[Rust](../../../experiments/constitution-host-preflight/m1/rust-assertions.md)、[Web](../../../experiments/constitution-host-preflight/m1/web-assertions.md)。R2 登记仍代表当时现场，本节覆盖已作废的别名断言。

### 删除测试映射

| 删除数量 | 对应新路径或理由 |
| --- | --- |
| 0 | 未删除测试、未新增 ignore；暂停/恢复、stop 优先、重启两种完成事实、HTTP 双后端三阶段、Pilot 独立控制和 Web effect 控件为新增覆盖 |

## 尚未完成

- 并行模块校验器切片已获批准并完成；Host 多槽位调度尚待接入。
- 全量 Host 迁移按 M1–M5 执行，H3–H6 阶段划分已被取代；M1 已验收，S19–S24、S26 已批准（2026-10-19）。M2 的并行/Call/助手、结果与恢复决定/删除，M3 默认切换，M4 旧 Runner 删除，M5 测量仍未完成。全局 C2 尚未验收，16 save / 36 mutation 为旧路径棘轮起点。
- 旧 Runner 的删除。
- M1 的本地验收不替代真实 Goose/Provider、部署与生产数据离线迁移；W4 未决定。当前实现缺时间观察为 null，不伪造历史时间。
- 共享 conversation lease / Library 读锁 / `PROCESS_ENV_LOCK` 的既有多测试 flaky（见"待处理与观察"）。

## M1 审查结论（2026-10-19）

审查方重跑：workspace 121 个 target，1922 通过 / 0 失败 / 115 忽略；Web 34 个文件、290 项通过；内核相对 `6ee2b0e` 零 diff，生产校验器未改。M1 通过。

pause 由调度边界钩子（`DispatchBoundary`）在节点结果提交之后、下一次派发之前消费持久化的暂停意图，提交人的 stop（reason=paused），不读执行标志，符合 C4。

S21 的影响已核查：
- 助手退役（`assistants.rs`）此前要求静止，旧 Paused 不算静止，所以暂停中的助手不能退役；现在 Paused 视为静止，暂停中的助手可以退役。
- WeCom 替换旧渠道 Run 前（`api/wecom.rs`）不再先把 Paused 转为 Stopped。

两者都与"暂停 = 等待人"一致，没有发现会在可恢复的 Run 上执行破坏性清理的路径。

缺项：§10.1 规则清单表未交，并入 M1.5 补交。另有 19 个原始日志入库，§10.6 措辞有歧义，从 M1.5 起执行。

**用户决定（2026-10-19）**：批准 S19–S24、S26。S23 中旧 Run 可能出现"显示可恢复、写入被拒"的不一致，属于过渡现象，到 M3 旧 Run 退出后消失；在此之前不为它补旧规则。

## M1.5：master 同步与语义规则清点（2026-10-19）

先以 `c6454c3` 合并 `master@2b3b7a2`，再按 §10.8 仅合入 F0 `e3a5778`；使用 merge，未 rebase，未合入 F1 `06f7b8a` 及后续提交。M1 的独立 pause 与新相位词汇保持；master 的模型托管配置、Plugin 配置/测试、源代码启动和消息入口迁移保留。Kernel 相对 `6ee2b0e`、生产 GraphValidator 相对 M1 均无改动。规则清单与验证结果见 [M1.5 证据](../../../experiments/constitution-host-preflight/m15/verification.md)。

### 冲突解决

| 文件 | 解决依据 |
| --- | --- |
| `apps/web/src/ChannelSessions.test.tsx` | 保留 master 的新消息会话入口与打开/关闭检查；保留 M1 的 phase/waiter/actions fixture 和业务断言 |
| `apps/web/src/ChannelsPage.tsx` | 接受 master 拆分删除；M1 的 RunControls、静止和文案逻辑接入 `ChannelSessions.tsx`，消息触发配置由 `MessageTrigger.tsx` 负责 |
| `docs/pilot-development-plan.md` | 保留迁移待办；master 新的真实模型/周报/Docmost 结论取代已失效旧待办 |
| `rust/Cargo.toml` | 保留 `anchor-kernel` workspace 成员 |
| `rust/anchor-runtime/Cargo.toml` | 保留 Kernel 依赖与 test-fixtures feature |
| `rust/anchor-devtools/Cargo.toml` | 保留 master 新增 distribution 依赖与共享 file lease |
| `rust/Cargo.lock` | 合并两侧依赖并经 locked 构建验证，不升级 registry 版本 |
| `rust/anchor-rsi/tests/evidence.rs` | 两侧同样显式初始化 ring provider，保留单一调用；原 HTTP MCP 断言不变 |
| `rust/anchor-runner-host/src/credential_store.rs` | 保留 master 的 crate 可见 lease，使用共享 FileLease |
| `rust/anchor-runtime/src/graph/store.rs` | 复用 master 读/写租约 helper，保留 H1 旧格式具名拒绝与记录校验 |
| F0 的 `docs/pilot-development-plan.md` | 保留本分支迁移行与上游门面行，按本分支实际验收状态更新，不把 master 的未合入 F1 写成当前实现 |

自动合并后的编译适配：Plugin 配置的字符串边界显式读取 `AuthorizationDigest::as_ref()`，持久 JSON 保持字符串；新配置准入仍使用 `same_authorization`，移除 master 恢复引用的旧升级接口，延续 C6。Web 新入口使用同一 `runQuiescent` / `runPhaseLabel`，新增的浏览器 fixture 改为新相位并增加 abandon 动作。

按 §10.8，`kernel_runs.rs` 及其目录、`run_view.rs`、`legacy_run_view.rs`、`run_json.rs`、`run_observations.rs` 原样归入 `application/run_access/`。`read_view` 从 queries 移入该边界，原生 abandon 的提交与返回事实也归入 `KernelRuns::abandon`，调用顺序、结果与权限不变。围栏测试与 F0 逐字一致，无新增运行时豁免；五个业务上限同步下降，总上限 224 → 192。完整 F1/F2 接口留待 master 完成后一次性对齐。

最终本地验收：workspace 2023 通过 / 0 失败 / 122 既有忽略，Web 311 及构建、消息管理浏览器 2 项通过，fmt/clippy 与围栏通过。Goose 最终完整 117 通过 / 2 失败 / 0 忽略，失败为 §10.9 的模型端点切换与 Pilot 默认 network 挂载两项，均在 G0 已验收名单内；新增失败 0。真实模型 Provider 未验收，M2 未开始。原始输出在 `/tmp/anchor-m15/`，只提交四份摘要与机器证据。

### S 候选

| 新增数量 | 结论 |
| --- | --- |
| 0 | 保持已批准的 S19–S24、S26；S25 作废。旧 Run 的 S23 过渡边界按用户决定保留 |

### 断言修改

Rust 共 21 处登记，另有 Web fixture 1 处。下列迁移旧 HTTP 状态、控制回执与 F0 字节期望；原存储事实、Session/Turn 状态、产物、成本、权限拒绝和不重放检查保留。临时未配置 relay 的失败不用于放宽断言。

| 登记 | 位置 / 旧 → 新 |
| --- | --- |
| M15R01–04 | `tests/goose_acp.rs` 四处 HTTP 完成断言：state.status=completed → projection.outcome=done |
| M15R05 | 同文件重启后的未完成断言：state.status!=completed → outcome!=done |
| M15R06 | `tests/goose_trace.rs::assert_live`：state.status=running → projection.phase=executing，active=true 保留 |
| M15R07 | `tests/goose_session_calls.rs::ack`：HTTP 完成断言改 outcome=done；pending 投递与 inactive 不变 |
| M15R08–10 | 同文件同 Run 续行：失败拒绝改 outcome!=failed；异常停止改 waiting/human；完成轮询改 settled/done。原调用数、同一历史与效果断言保留 |
| M15R11 | `tests/goose_plugin_mcp.rs`：state.status=failed → outcome=failed；原错误内容/零 Provider 调用/无 panic 不变 |
| M15R12–13 | `tests/support/goose_fixture.rs::wait_status`：失败拒绝与目标轮询改读 phase/waiter/outcome/reason，保留 inactive 及预算/失败拒绝；ready 是 Executing 且无 worker，paused 额外要求 reason=paused，waiting_call 要求 Waiting/Child |
| M15R14–15 | `tests/support/runtime_fixture.rs::wait_status`：同上，仍拒绝 failed 与 budget；不加入旧 status 回退 |
| M15R16 | `tests/goose_persistent_assistant.rs::action`：控制回执 asked → operation；HTTP 202 与 Run 身份、动作值检查保留 |
| M15R17 | F0 `api/tests/run_access_json.rs`：十种 status 的 phase 参数改为三相位；Ready/Running=Executing，三种人的等待及 Child/Effect=Waiting，三种结算=Settled（S19/S20） |
| M15R18 | 同上 resume 参数：WaitingCall/WaitingRecovery true→false，BudgetStopped false→true；其余保留（S23） |
| M15R19 | 同上 stop 参数：Paused/BudgetStopped/WaitingCall/WaitingRecovery/Stopped true→false，仅 Executing 可 stop（S22） |
| M15R20 | 同上增加 waiter/outcome/reason 逐字节期望；暂停/预算/停止各保留具体 reason，Failed/Aborted 的缺说明为空字符串（S19） |
| M15R21 | 同上完整响应字节：移除 status 与 control_requested/terminal/quiescent/needs_attention，增加 actions.abandon 与 projection.updated；身份、时间、进度、trigger 和原始字节比较保留（S19/S22/S24/S26）。测试名 `run_status_http_json_bytes_match_the_master_contract` → `run_status_http_json_bytes_match_the_phase_contract`，没有删除覆盖 |
| M15W01 | master 新增 `e2e/message-management.spec.ts` fixture：running → executing，移除 state.status，增加 actions.abandon；浏览器业务断言未改 |

`ChannelSessions.test.tsx` 冲突解决保留 M1 既有新词汇断言，没有新增断言放宽。

### 删除测试映射

| 删除数量 | 对应新路径或理由 |
| --- | --- |
| 0 | 本里程碑未删除测试、未新增 ignore；master 消息管理组件/测试拆分随合并保留 |

## M1.5 审查结论（2026-10-19）

M1.5 通过。合并提交为 `c6454c3`（master@2b3b7a2）和 `aae4bb3`（仅 F0）。

- 围栏测试与 master F0 逐字节一致，没有扩大豁免，上限从 224 降到 192。内核零 diff。
- 审查方定向重跑了围栏、run_access、JSON 字节、kernel_runs 和 Kernel，全部通过；全量 workspace 与 Goose 将在 M2 交回时重跑。
- Goose 相对 G0 基线没有新增失败。首次带 relay 的全量运行发现了 5 个 M1 词汇迁移遗漏，均已修复。
- 规则清单表明，双路径并存期间计数基本持平，收敛要在 M3 兑现，目标见迁移任务书 §10.10。

## M2：并行模块作用域边界（2026-10-19）

按迁移任务书 §10.10 开工，第一项并行能力开工时确认了 §8 停止条件：旧 Runner 已支持并行分支内共享模块作用域与重入上限；GraphValidator 的 run_resource 明确拒绝，parallel_frontier 也没有模块激活裁决。仅接通调度器无法关闭 M2 的全部 Unsupported；该扩展现已获用户批准，Kernel 不需改动。

同一 Graph 的确定性复现通过：旧侧两分支各执行一次、模块计数为 1，重入按上限失败；内核准入返回具名 Unsupported。既有模块测试独立通过。初次诊断未改生产代码与既有测试；规则计数沿用 M1.5 源快照，不声称收敛。边界、最小扩展范围、完整临时测试源码与实际命令见 [M2 证据](../../../experiments/constitution-host-preflight/m2/verification.md)。触发停止后未执行无关全量或 Goose 验证。

### S 候选

| 新增数量 | 结论 |
| --- | --- |
| 0 | 用户已明确批准并行模块裁决扩展，结算顺序沿已批准 S6/S17/S18；没有新增未批语义 |

### 断言修改

| 修改数量 | 说明 |
| --- | --- |
| 0 | 既有业务断言未改；临时测试只复现能力缺口，不计为能力通过 |

### 删除测试映射

| 删除数量 | 对应新路径或理由 |
| --- | --- |
| 0 | 未删除既有测试、未新增 ignore；临时诊断副本执行后移除，完整源码随验收摘要保存 |

### 已批准的并行模块校验器切片

入口、模块激活、成员 passes 与嵌套激活清零全部在 parallel_frontier 内，以纯函数预演与裁决实现；同一并行激活内共享模块只首次外部进入时计数，后续激活重入递增。只接受当前提交槽位的模块事实，避免提前记录其他开放分支的激活；沿现有 conclude 处理模块上限，迟到结果与失败拒绝留史并计费。没有新增持久化字段，Kernel 与旧 Runner 未改。

旧 Runner 模块上限用例已复制到内核，增加两次激活、异序与延迟进入、成员/嵌套计数重置、上限后迟到成功/失败覆盖。当前分支的原并行生成器会清空模块计数且生成无作用域分支，因此另增 5,000 个共享/独立模块样本，原种群、seed、注入概率、业务断言与覆盖阈值保持不变。模块样本为无失败注入且高模块上限的补充，模块上限/迟到失败由定向测试验收。

Runtime 全部单元、集成和 doctest 分阶段通过，原三组差分、S17/S18 独立 seed 与新增模块组通过；fmt 与 runtime all-targets clippy 通过。新增单元测试 5，既有断言修改 0，既有测试删除 0，新增 ignore 0。命令、完整 Tally 与原始输出入口见 [M2 验证](../../../experiments/constitution-host-preflight/m2/verification.md)。这完成校验器前置切片，Host 并行调度与其余 M2 能力仍未完成；未新增 Goose 或真实模型 Provider 验收。


### Host 多槽位调度切片与删除存储边界

KernelScheduler 以开放槽位唯一标识管理并发执行，已完成的单个分支提交后补位；慢分支不形成波次屏障。立即就绪的执行事实先提交，未完成兄弟继续执行。等待、结算或端口错误后收集已派发任务，迟到结果/失败仍获得内核裁决，StaleVersion 重提成本为零。fanout/join 的纯控制产物构造抽为旧 Runner 与新调度器共用函数；NodeExecutionRequest 继续复用原授权构造函数。冻结使用 ArtifactFreezeContext 保留种类与输入来源。

旧 Runner 的异序分支/join、分支失败及共享模块重入用例复制到实际调度器；另覆盖无波次后继、停止后迟到与恢复、端口错误/panic 的兄弟收集。Host FileStore 路径验证并行 pause 在当前结果提交后进入人的等待、兄弟迟到留史、不取消执行、draining 时拒绝 resume，以及恢复不重复执行。实际调度器另跑串行 10,000、并行失败 5,000、原无失败并行 5,000、模块 5,000；复用既有 S18 检查器，原断言与门槛未改。validator/differential 的可见性只在 cfg(test) 下扩大，裁决规则未改变。

此时发现下一项必须停下的具体边界：Store/FileStore 没有公开删除操作；Host 不拥有其私有资源路径编码。仅改 phase/worker 前置条件无法完成 Run/Graph 删除。未复制私有布局、未加隐藏资源的持久标记、未改 Kernel。按任务书 §8 交回；最小建议为只给具体 FileStore 增加 remove(id)，无需扩充 Store trait 或生命周期。现场与授权范围见 [删除边界](../../../experiments/constitution-host-preflight/m2/deletion-boundary.md)。M2 未完成，Op.call 等后续切片与 M3 未开工。

| 登记 | 数量与结论 |
| --- | --- |
| S 候选 | 0；沿已批准并行模块与 S6/S17/S18，新增 Code Write 仅提交既有 fanout/join 控制事实 |
| 既有断言修改 | 0；S18 检查器按原样复用 |
| 删除测试映射 | 0；既有测试均保留，无新增 ignore |
| 新增测试 | Runtime 9（含 3 组实际调度器随机对照），Host 1 |

本次只完成并行垂直切片并记录存储边界，不替代完整 M2 验收。实际命令与结果见 [M2 验证](../../../experiments/constitution-host-preflight/m2/verification.md)；未新增 workspace/Web/Goose/真实模型 Provider 验收。


### 已批准的 FileStore 删除操作

用户已批准具体 FileStore 的 remove(id)，原存储边界解除。删除通过 rename 到 `.deleting-<encode(id)>` 后 sync parent、remove_dir_all 后再次 sync parent 完成；读取幂等清理遗留目录，正常与删除目录并存时保留正常资源。单资源读写/删除复用 anchor-file-lease，锁文件位于独立 `.locks` 目录并留存，list 不暴露内部目录。Store trait、MemoryStore、Kernel 生命周期、Reason/Phase/Waiter 均不改。

新增存储测试 7 项通过，包含两线程 100 轮并发 load/remove（同实例和独立实例）；Kernel 全量 44 通过、clippy 通过，无旧断言修改/删除。当前例外只涉及 FileStore 存储、锁依赖和测试，kernel.rs/model.rs/lib.rs 相对 6ee2b0e 零 diff。实际命令与证据见 [M2 存储操作](../../../experiments/constitution-host-preflight/m2/deletion-boundary.md)。Host 删除与其余 M2 切片继续，不将存储测试当作真实 Provider 或完整 M2 验收。

### Host Run/Graph 删除

Kernel 删除已接到 run_access 门面和获批 FileStore.remove。删除预留与 create/control/drive 注册互斥，排队 worker 重新读存储后才可执行；Run/Graph 清理保留旧 Session、引用、对话继任和 wait-child 保护。新增 10 项定向回归、Host bin/tests clippy 与围栏通过，旧业务断言无改动。删除业务内部类型围栏 3 → 1；测试围栏文件原样。证据与保留清理边界见 [Host 删除](../../../experiments/constitution-host-preflight/m2/host-deletion.md)。

### Host Kernel 测量投影

任务书 §10.4 Probe 已接到 KernelRuns.open。投影只消费 Point，按只读 JSONL 追加；启动从保留的 Graph history 重建，不参与裁决、恢复、投递或删除。投影 I/O 失败只诊断。新增 3 项定向用例通过，覆盖实际 Host 并行派发、人的控制、拒绝、重建与投影故障；样本含 12 个 Point（accepted 7 / returned 2 / refused 3），见 [分组统计与命令](../../../experiments/constitution-host-preflight/m2/probe-counts.json)。无跨 Run 时序保证，已删除资源的历史不在重建范围；未新增对外 API、Kernel 字段或业务断言修改。

### 串行 Op.call 与启动恢复

串行 Call 的父子 Run 均走 Kernel。共享 InProcessGraphHost 保留准入、身份核查、输入暂存与结果导出；宿主在 Bound 持久化后执行或观察子 Run，并提交 resume 及 Completed/Failed/Uncertain。父 worker 释放与子 Run 结算交错时重新核查持久等待，结算只释放一次。detached child 启动恢复按 Kernel Executing 相位选取，完成事实先于执行检查；已有 `.started` 时带 Observation 停止等待人核查，不重放效果。

新增 Code 提交 AdmissionFailed，处理宿主在准入锁内证明不存在子 Run 资源和准入元数据的失败，保持 graph_calls 为空。任务书 §8 允许登记后新增 Code 提交；已 Bound 的调用不能使用该提交抹去事实，Agent 无权使用。登记与实测失败现场见 [call-admission.md](../../../experiments/constitution-host-preflight/m2/call-admission.md)。

4 项复制的 API 用例、3 项 Runtime 调用用例、Done/Failed/Unknown 回调竞态、2 项 Kernel 重启用例和原旧路径重启用例通过。5,000 seed 调用对照通过：waited 957、detached 1,140、failed 514、unknown 392。三 crate 回归通过；随后最终恢复选择改动由定向重启用例验证，最终 clippy/fmt 通过，完整 target 及日志哈希见 [call-checks.json](../../../experiments/constitution-host-preflight/m2/call-checks.json)。既有业务断言修改 0、删除 0、新增 ignore 0。

并行 Call 组合仍触发 §8：准入已允许，但 parallel_frontier 固定派发 Work，绕过调用端口。此裁决扩展不在并行模块授权中，已保留可执行诊断和四点建议，见 [parallel-call-boundary.md](../../../experiments/constitution-host-preflight/m2/parallel-call-boundary.md)。M2 未完成；助手、执行结果与人工恢复决定仍未关闭。新增 §10.11 要求当前切片提交后立即合并 master F1/F2，剩余能力在统一门面后实现，不能继续沿旧入口形状扩展。

## K0.2：FileStore 删除与资源租约

任务书 §10.11 已追认 `0859373` 的 FileStore 删除、anchor-file-lease/sha2 依赖和资源级阻塞排他租约。C1 从此以 `0859373` 为基线，后续 Kernel 变更仍须交回。Store trait、MemoryStore、生命周期及 Graph 裁决未变；锁路径为 `.locks/<sha256(id)>`，只隔离单资源文件访问，不定义提交权限或任务状态。

FileStore 的 load/create/commit/history/remove 各自取得同一资源租约；list 只枚举规范编码的目录，不取资源租约。每个操作使用独立文件描述符；load 已持锁时调用不加锁的 history_at，避免递归获取同一租约。同步文件操作使用阻塞等待，是为了在短暂的读取/删除竞争后继续观察一致结果；非阻塞会把正常竞争变成 Io 错误，且现有 Store 没有 Busy 或异步重试契约。

当前锁序为 Host `kernel_runs` 缓存互斥量 → 非阻塞 `.writer.lock`（仅打开时取得、持有至 owner 释放）；单次创建/控制/派发注册按 active → deleting → Kernel writer（提交时）→ FileStore 资源租约。删除在 active/deleting 下预留后释放这两把互斥量，再经 FileStore.remove 取资源租约；读取也可直接取资源租约。FileStore 不回调 Host，不持资源租约获取 Host 互斥量或 Kernel writer；每次 Store 操作结束便释放，后续 Probe/观察发生在 Store 操作之外，因此没有反向等待环。

Host `.writer.lock` 暂不能删除。资源租约只保护一次文件操作；Kernel 的 load→裁决→commit 跨多个 Store 调用，writer mutex 仅覆盖同一 Kernel 实例，跨进程仍需排除第二个裁决者。它也不代替 active/deleting 对 worker 与控制的协调。M3 保留该锁，除非后续能证明单写入者由等价机制保证；不能因为 FileStore 已有 flock 就删去宿主写入者边界。

## §10.11：同步 master F1/F2

按任务书 §10.11，以 `2582528` 为本分支父提交、`d968ba5` 为 master 父提交执行 merge，不 rebase。采用 master 的安全 RunView、读取与意图操作接口；保留本分支已批准的新相位、独立 pause、对外词汇和 M2 Kernel 路径。默认入口仍是旧 Runner，不在本次合并中开始 M3。以下登记合并产生的 19 个冲突路径及处理依据；合并回归通过，见[合并验证](../../../experiments/constitution-host-preflight/m2/merge-verification.md)。

| 冲突路径 | 处理依据 |
| --- | --- |
| `docs/architecture.md` | 保留 master 的门面所有权、安全视图与意图写入边界，保留本分支 Phase/S21 和显式 Kernel 路径；M2 未完成能力链接到当前验收入口。 |
| `docs/pilot-development-plan.md` | 保留 master F0/F1/F2 的已验结论，并明确其测试数只对应上游提交；保留本分支 M2 未关闭项，移除“F1 尚未合入”的失效待办。 |
| `docs/run-access-facade.md` | 保留 master 接口、规则清单与 36→13 的上游测量；本分支新相位和 wire 以已批准契约为准；本次合并回归通过，M2 全量能力验收仍未完成。 |
| `rust/anchor-runner-host/run-access-fence.txt` | 接受 master 业务引用零上限的清单；新增原生访问归门面，不恢复业务例外，不扩大测试固定的运行时范围。合并回归已实测通过，围栏测试文件与 master 字节一致。 |
| `rust/anchor-runner-host/src/api/runs.rs` | HTTP 读取和序列化采用 master 门面；显式 `execution=kernel_probe` 选择及本分支 phase/waiter/outcome/actions、独立控制回执保留，入口不持有可写 Run。 |
| `rust/anchor-runner-host/src/application.rs` | 采用 master 模块与可见性边界；保留 KernelRuns 缓存和既有宿主装配，生产 store、原始枚举及执行准备留在 run_access 内。 |
| `rust/anchor-runner-host/src/application/abandon.rs` | 保留业务的 abandon intent、取消等待和启动收尾；Run 终结委托门面，Kernel 分支保留 Human abandon 及原生 worker 排空处理。 |
| `rust/anchor-runner-host/src/application/admission.rs` | 业务只保留 Graph 准入租约等用例协调，读取改用 run_views；实际接纳、冻结、保存和启动迁到门面 admission，显式 Kernel 选择在该门面后实现。 |
| `rust/anchor-runner-host/src/application/conversations.rs` | 接受 master 的安全视图、谓词和会话意图接口；保留本分支 paused/budget 轮次占有、继任和 wait-child 保护，跨新旧存储读取经同一门面。 |
| `rust/anchor-runner-host/src/application/deletion.rs` | 保留 Graph/单 Run 删除的业务协调与 Session 保护；Run 清理和实际存储删除迁到门面 deletion，接回 M2 的 Kernel 删除预留与 FileStore.remove。 |
| `rust/anchor-runner-host/src/application/dispatch.rs` | 启动恢复枚举采用 run_views/can_dispatch；执行、子 Run 释放与持久写入采用 master 门面 dispatch，保留原生 detached 子 Run 与结算回调。 |
| `rust/anchor-runner-host/src/application/liveness.rs` | 业务生产判断归门面，旧模块只保留测试兼容入口；本分支“无 worker 且 Settled 或 Waiting(Human)”语义保留，不恢复 master 的旧 Paused/BudgetStopped 例外。 |
| `rust/anchor-runner-host/src/application/queries.rs` | 用 master 的 read_run/run_views 安全视图替代业务原始枚举；原生活动观察和旧 active registry 合并只用于查询，不写回状态。 |
| `rust/anchor-runner-host/src/application/recovery.rs` | 业务 recover 只转交门面 record_recovery_decision；master 的恢复目标校验、幂等与保存协议归门面。原生未知效果人工决定仍是 M2 未完成项，不因合并接口而记为支持。 |
| `rust/anchor-runner-host/src/application/run_access.rs` | 以 master 的私有 Cow、RunView/progress/谓词及意图模块为公共边界；内接本分支 Kernel 所有权、Phase 和宿主时间观察，不暴露原始 record 或新增业务可变入口。 |
| `rust/anchor-runner-host/src/application/run_access/control.rs` | 采用 master 的 stop/pause/resume 意图入口和原用例核验顺序；在门面内按所有权调用 Kernel 控制，保留本分支独立 pause 与节点结果后暂停边界，不恢复旧业务 control 实现。 |
| `rust/anchor-runner-host/src/application/run_projection.rs` | 生产 Run 投影采用 master 门面 projection；旧文件保留只读冻结拓扑与测试兼容入口，M1 已批准的新字段、动作和独立 pause 期望保留。 |
| `rust/anchor-runner-host/src/assistant_plugins.rs` | 采用 master 的 RunView、progress、共享可执行绑定谓词和 active_bindings 返回接口；保留非空 AuthorizationDigest/C6，旧快照缺冻结授权时明确要求离线迁移，不构造空授权。 |
| `rust/anchor-runner-host/src/pilot_tools/runs.rs` | 列表和详情委托 master 的 Pilot 门面序列化；控制保持本分支 operation 回执与独立 pause，不在工具适配层解释或改写 Run 状态。 |

接口移动涉及的 `run_access/admission`、`abandon`、`conversations`、`dispatch`、`deletion`、`recovery`、`projection`、`reads` 等新路径属于以上冲突的归属调整，不另建第二套业务规则。K0.2 的依赖、资源租约与 C1 基线见前节，本次合并不扩大 Kernel 授权范围。

本次合并回归通过：workspace 128 个 target，2,113 / 0 / 123；严格 workspace/all-targets clippy、fmt、Web 311 项及构建通过；固定 Goose 完整串行 ignored 为 120 / 0 / 0，满足 §10.11 的无失败门槛。C1 相对 `0859373` 零 diff，围栏测试与 master 字节一致，业务引用为 0。合并修复保留 Kernel 显式选择、operation 回执、当前 invocation、投递/删除与父 stop 子调用的原业务保护；另修复 trace evidence 原子发布竞态并补齐插件披露报告身份。原先失败和独立复跑均保留在[合并验证与断言登记](../../../experiments/constitution-host-preflight/m2/merge-verification.md)，不以定向通过抹去失败。本次不验收 M2 完成；并行 Call 裁决扩展仍按 §8 待批，助手、执行结果补齐、原生人工恢复决定、真实 Provider 和生产数据迁移仍未关闭。

## M2 §10.12：门面后能力补齐与交回

§10.12 已纠正先前并行 Call 的范围判断：Host 从 `82a726d` 起就由共用 reject_snapshot 拒绝该组合。默认与 Kernel 路径均复用该产品准入规则，HTTP 422 在任何 Run、metadata、Artifact 副作用前返回；不修改 parallel_frontier 或 Call 校验器。前文“待批扩展”已被本结论取代，见 [准入边界](../../../experiments/constitution-host-preflight/m2/parallel-call-boundary.md)。

KernelScheduler 补齐 Yielded、预算、Interrupted、Suspended/Cancelled 和人放弃的端口结果；不再有“后续切片”能力拒绝。Yielded 在冻结前验证路由，冻结 Interruption 产物后由 Code Write 提交；预算以宿主实际 model_requests 成本及 Observation stop，等待人恢复，不裁定 W4。常驻助手经既有 admit_assistant_run 门面显式选择 Kernel，绑定与活动 invocation 由 frontier 派生，结算回调继续交给 Session；默认仍用旧 Runner。完整助手回归发现空闲 wait_input 未接收 pause 事件，保留原断言修复为每 worker Notify 唤醒、Suspended 后由原调度边界提交 stop；不取消工作、不新增轮询。两个删除测试构造器只补对应事件字段。

未知效果 API 由门面内 Kernel history 投影和执行端口接通。先持久化人工核查事实，再提交 Human 控制；普通 resume 和可用动作均阻止未核查 attempt，多个 pending 全部核查后才释放。相同决定幂等，冲突或持久化失败不释放，排空/删除预留互斥；已结算不重启。原生 Goose 只追加现有 tool_observation.operator_decision，不制造工具结果、NodeCompletion 或 Goose history。新原生工具观察带开始时间；attempt ID 基于稳定工具身份，step=0 表示没有旧 Harness step，旧时间缺失显示 unknown。

Trace 原子发布是 `490c000` 已有实现，本轮确认了截断/并发读取的机制根因。修复前同一回归连续 5 次断言失败，原子替换后通过；原首轮 500 的瞬时字节不可获取，不声称逐字还原。具体 fsync/rename 顺序见 [根因确认](../../../experiments/constitution-host-preflight/m2/trace-publication.md)。

### S 候选

| 候选 | 结论与依据 |
| --- | --- |
| 无新增 | 预算等待人恢复、人放弃、Interruption、未知效果核查沿既有任务书与已批准语义；并行 Call 是 §10.12 明确的产品准入裁定。W4 未裁定。 |

### 断言修改

| 范围 | 修改与等价覆盖 |
| --- | --- |
| 既有业务断言 | 放宽 0、删除 0。旧助手 12 项业务断言由新 goose_kernel_persistent_assistant target 原样 include；fixture 按 target 选择 Kernel，并增加只存在 Kernel 存储的断言。 |
| 测试兼容投影 | runtime_fixture 的 record_for/runs 支持只读 Kernel 枚举，并由 frontier 派生旧夹具所需 cursor、当前 invocation/prepare passes；不写任一存储，不作为产品第二份状态。新恢复测试直接核对 Kernel 相位、历史与端口事实。 |
| 新覆盖 | Runtime host_operations/kernel 6 项、Host 恢复 2 项、原生观察幂等 1 项、HTTP 共用准入 1 项；新增 Goose ignored target 复用助手 12 项，另加原生未知效果 Completed/Abort 端到端场景。 |

### 删除测试映射

| 删除范围 | 映射 |
| --- | --- |
| 无 | 既有测试删除 0，旧测试新增 ignore 0。新 Goose target 的 ignored 属性保留对真实 pinned Goose/Bubblewrap 的依赖。 |

M2 的最终命令、原样 Goose target 结果、失败与复跑、规则清单及宪法检验统一见 [M2 交回](../../../experiments/constitution-host-preflight/m2/completion-verification.md)。C1 相对 `0859373` 零 diff，围栏与 master F2 字节一致；本轮没有 Kernel 修改。M3 未开始，默认切换与删除旧 Runner 由用户决定。真实模型 Provider、生产迁移与部署均未新增验收。

### §10.12 收尾同步 master

同步 master cc919d1（包含 d042ebe 原子 durable writer 与 dc88484 网关通知闸门），使用 merge、不 rebase。冲突按文件登记：

| 冲突路径 | 处理依据 |
| --- | --- |
| docs/pilot-development-plan.md | 保留 master 的原子发布、网关确定性回归与 F1/F2 审查结论；保留本分支 M2 当前验收入口，上游统计仍绑定各自证据。 |
| rust/anchor-runner-host/src/goose_acp/trace.rs | 使用 master 的共享 write_durable 原子发布，移除重复 writer；保留 master 两项交错/连续读写回归及本分支单快照回归。旧单快照夹具先显式设置 0600 再替换，以验证权限保留；既有权限断言不变，首次创建权限遵循 master 的 umask 契约。 |

main.rs 与 goose_acp.rs 自动合并；恢复模块和助手接入均保留。最终合并树 workspace 130 个 target，2,139 / 0 / 136；固定 Goose 完整串行 ignored 133 / 0 / 0，含 Kernel 助手 12 项和人工恢复 1 项；严格 clippy/fmt、sandbox 构建通过。Web 311 项与 build 在合并前通过，合并未改 Web 源码。原样结果、失败与中止登记见 M2 交回。


## M3 §10.13：唯一 Kernel 路径与旧执行器退出

先以 `0d029eb` 冻结旧侧 30,000 个 seed（串行 10,000、并行有失败/无失败各 5,000、模块 5,000、Call 5,000），两个命名空间分别完成旧执行与快照双对照后才删除旧 Runner。快照头、比较契约与尺寸见 [冻结基线](../../../experiments/constitution-host-preflight/m3/snapshots/README.md)。M3 首轮固定 Goose 完整验收失败，恢复业务断言冲突触发任务书 §8；该现场保留在下文。§10.15 已裁定按 master 的显式恢复契约继续，后续结论见本节末尾。

全部新 Run 默认 Kernel，HTTP 不再接收 execution 选择。旧存储仅具名拒绝；GraphRunRecord 格式 14 不再保存生命周期或 cursor。唯一生命周期来自 Kernel Phase，唯一 worker 表是 KernelRuns.active。Host、standalone、Graph/Session Call 与助手共用该执行路径，旧执行器、旧存储类型、兼容投影和控制轮询均退出。C2 的 [16 save / 36 mutation 去向](../../../experiments/constitution-host-preflight/m3/c2-destinations.md) 已逐项清零；[规则与 Session 所有权](../../../experiments/constitution-host-preflight/m3/rules-session-audit.md) 单独审计，不用消费者数量冒充规则计数。

### S 候选

| 候选 | 结论与现场 |
| --- | --- |
| S27（承接 S4） | **已批准，S4 同时结清（任务书 §10.17）。** 退役只移动 Session current 指针；未结算 Run 仍可 Human abandon。按任务书 §3.4 对齐 Kernel，不把 retired 解释成结算。`application::run_access::abandon::tests::retired_assistant_can_be_abandoned_without_restoring_its_session_binding` 验证真实 Session 退役后放弃，保留原退役指针和 Run 进度。 |

### 断言修改

| 编号/范围 | 修改及保持的业务约束 |
| --- | --- |
| A1 生命周期词汇 | status/cursor/旧 phase 改读 Kernel Phase、frontier 与独立 operation 回执；不构造可写兼容记录。各消失测试逐项见下表。 |
| A2 action/静止/关注 | 旧双路径矩阵改为真实 Kernel create/control/complete；stop 默认 reason 为 stopped，pause 为 paused；worker 排空仍阻挡并发控制；pending recovery 对静止的限制保留，普通 resume 的核查授权按 §10.15 更正。 |
| A3 恢复错误文字 | 已结算 resume 精确拒绝 `Kernel control refused: TaskSettled`；新 recovery 决定精确拒绝 `settled Run cannot record recovery decisions`。相同既有决定仍幂等，验证先于端口写入。 |
| A4 放弃原因与同步返回 | detail 保存原 reason（例如 plugin_update），不拼旧 abandoned 文字。正常及重复成功均等待同一 worker registry 排空，其他终态仍拒绝。 |
| A5 执行观察 | 开放 frontier 不等于执行已开始；时间/暂停夹具等到端口 durable started 再停止。保留原结果数、成本与产物断言，不以增加预期结果掩盖竞态。 |
| A6 未提交对话继承 | 前驱保留读取 Host 与 Goose 原生 started 事实；未 commit 的 workspace/native history 仍保护，未开始的开放槽位不延长保留。后继首轮证据与缺少 admission 的原业务断言保留。 |
| A7 Session-call 核查 | 认证 Session 协调仍可持久化未知效果核查提示后继续同一 Run；通过既有 recover_attempt 与端口观察，不清空数组、不自动重放工具。普通控制/恢复仍不能越过 Session 协调。 |
| A8 旧数据及入口 | execution 选择具名拒绝；旧 RunStore 与旧 Kernel record 格式具名拒绝。用户明确取消迁移工具，旧本地状态不自动删除。 |
| A9 持久错误与路由 | 非法 route、Yielded、Op.run 纯授权失败仍按原业务终态和一次成本断言验证；不放宽 S18，不修改 GraphValidator 裁决。record_of 保持纯 decode，结构校验在 load 边界。 |
| A10 ignored 重复 target | 删除 goose_kernel_persistent_assistant 对普通 assistant 模块的重复 include；原 12 个业务用例仍在默认 Kernel target，未新增 ignore。独立 Kernel 未知效果 recovery 用例保留。 |

### 删除测试映射

| 范围 | 映射与理由 |
| --- | --- |
| Runtime 原主模块 73 项 | [逐项映射](../../../experiments/constitution-host-preflight/m3/runtime-test-mapping.md)，原测试名全部保留；夹具改为真实 Kernel 提交，损坏记录测试只修改用于验证拒绝的测试副本。 |
| Host 39 个消失函数 | [逐项映射](../../../experiments/constitution-host-preflight/m3/host-test-mapping.md)，包含重命名、合并、旧机制退出及无规则 forwarder；每项给出等价目标或行为退出理由。 |
| Goose 重复 12 项 | 同一模块曾被两个 target 编译，M2 的 136 比字面 124 多 12；只删除重复 target，12 个普通 assistant 场景保留，名单与实际最终计数在交回证据。 |

按用户 §10.13 决定不做生产数据迁移，不修改现有开发状态；交回提供 [新开发状态根与保留范围](../../../experiments/constitution-host-preflight/m3/local-development-state.md)。W4、真实模型 Provider、外部渠道与部署不由固定 Goose/本地 Provider 验收代替。M4/M5 由用户后续授权；本分支不自行合回 master。


### M3 结束同步 master

以 `6e91e5c` 为本分支父提交、`0aa5197` 为 master 父提交执行一次 merge，不 rebase。两个实际冲突及上游旧夹具适配如下；完整合并后验收由 M3 交回证据记录。

| 路径 | 处理依据 |
| --- | --- |
| `rust/anchor-runner-host/src/main.rs`（冲突） | 保留 master 抽出的 durable_write 模块和取得 deployment writer 后的 startup cleanup；保留本分支 Kernel standalone 用例及纯 create_with_plugins，不恢复 PreparedExecution 的旧入口或可变 record。共享 write_durable 仍为唯一实现。 |
| `rust/anchor-runner-host/src/application/run_access/reads.rs`（冲突） | 接入 master 的 durable_input_workspaces，只返回已记录 wait_input 的工作目录供启动清理；保留本分支 Kernel 读取、旧格式拒绝与恢复 history 投影，不恢复旧 store/resume_interrupted_attempts。 |
| `rust/anchor-runner-host/src/durable_write.rs`（上游夹具适配） | master 新测试使用已删旧存储并直接设 invocations，改为 fixtures::create 与真实 Code completion，所有清理/保留断言不变。测试释放 writer 后再模拟新 Host 启动清理。 |
| `apps/web/src/App.races.test.tsx`（上游夹具适配） | 新导航竞态测试改用共用 settled/done 投影与 abandon 动作字段；删除旧 status 与只属于旧投影的布尔值，导航和删除反馈断言不变。 |
| `rust/anchor-runner-host/tests/writer_lease.rs`（上游断言词汇适配） | 新 stdio cleanup 成功断言从 status=completed 改为 projection.outcome=done；保留清理生效、status 只读不触发清理、deployment lease 不被创建的业务断言。 |

其余自动合并采用上游 Web 凭据收起、异步生命周期保护、Pilot transcript 与启动临时文件回收；文档归档原样保留，不把历史证据当本次验收。上游新增 durable_write 测试首次合并编译因旧类型引用失败，已在定向修复后重跑，不能将该首轮写成通过。


### M3 §8 停止交回（2026-10-20）

合并后普通 workspace 为 2,153 / 0 / 125（129 targets），Web 39 文件 333 项及构建通过，严格 clippy/fmt、sandbox/Host/helper 构建通过。merge `b61c325` 的提交说明误写 2,154，实际日志为 2,153；不改写历史。固定 Goose 完整 Host ignored 为 **110 / 12 / 0**（41 targets），退出码 101；122 项全部实际执行，12 项失败逐项独立复跑全部再次失败，原样结果与最终确认见 [交回](../../../experiments/constitution-host-preflight/m3/verification.md) 和 [checks.json](../../../experiments/constitution-host-preflight/m3/checks.json)。Host、固定 Goose 与 helper 的运行前后身份一致，使用独立 target `/tmp/anchor-goose-regression-target-core`。

业务断言冲突：旧普通 Run/渠道恢复要求一次显式 resume 后由 Agent 在同一原生 Session 核查并继续；M2 人工恢复用例要求这次 resume 只发现并停驻，直到显式决定。两者均禁止重放未知效果。按任务书 §8，本轮不修改这些业务断言或普通恢复语义，待用户裁定；[现场与选项](../../../experiments/constitution-host-preflight/m3/recovery-contract-decision.md)。这不是已批准的 S 变更，既有 S27 仍是本轮唯一 S 候选。

其余未关闭项为 spike/会话身份已知失败映射、人工 recovery 重入锁、配置与 MCP 预检错误映射，以及三项仍读旧 runs 目录的夹具。只读定位与建议边界见 [完整回归清单](../../../experiments/constitution-host-preflight/m3/goose-regressions.md)；本轮未实施这些修复，不能把它们写成已通过。停止条件触发后的交回提交只更新证据与当前结论。

C2 save 0、mutation 0；Kernel 相对 `0859373` 零 diff，GraphValidator 裁决未改；旧类型扫描零命中。业务断言放宽 0，既有测试新增 ignore 0。M3 未达到 Goose 零失败门槛，不合回 master，不自动开展 M4/M5。真实模型 Provider、外部渠道与部署未验，W4 未裁定；旧数据迁移工具按用户 §10.13 决定取消，开发状态处理见交回链接。


### M3 §10.15：M2 行为更正：恢复回到 master 契约

用户已裁定普通人的显式 resume 授权 Agent 在同一个 Goose 原生 Session 核查后继续。普通 Run 控制与认证 Session-call 共用 `inspect_interrupted_attempts`：先把尚未登记的真实 native pending 经 Code + Observation 保存，再持久化核查提示，经既有 `recover_attempt` 提交 Human release；不补造工具成功或模型完成事实。后台自动恢复不调用这条释放路径。人工 Completed/Abort 继续作为另一条路径；其执行准备移到 worker registry 锁外，校验与持久写入顺序不变。

这是对齐 master 的 M2 行为更正，不新增 S 候选。S27（承接 S4）已在任务书 §10.17 批准。

| 人工恢复用例准备与断言 | §10.15 更正 |
| --- | --- |
| 准备真实未决效果 | 仍启动固定 Goose、产生一次真实外部效果、确认 native observation 的 result 为 null，再强杀 Host。 |
| 登记 pending | 在 Host 关闭期间读取真实原生事实，以 scheduler 共用的 `observation_stop` 构造并实际提交 Code + Observation；不直接改写 Kernel 文件，也不调用具有核查授权的普通 resume 作为准备。 |
| 测试命名 | `kernel_unknown_native_effect_requires_a_durable_human_decision_before_continuing_or_abandoning` 改为 `kernel_observed_native_effect_accepts_durable_completed_or_abort_without_replay`。 |
| 普通动作投影 | 已登记 pending 的 resume 为 true；删除原“第二次普通 resume 必须 409”的断言，这是裁定明确更正的契约。列表/详情/门面 JSON 的同一动作建议同步调整。 |
| 不重放与同 Session | 保留外部效果严格一次、重启前后原生 Session 身份相同、无旧 runs 文件等断言。 |
| Completed | 保留成功 202、继续后的产物与路由、原生 operator observation 落盘、相同决定重试 202、冲突决定 409。 |
| Abort | 保留成功 202、Aborted、原因与恢复决定落盘、无新增模型请求、效果仍一次、相同决定幂等及冲突拒绝。 |
| 持久失败 | Session-call 与普通人工 API 共用的失败回归继续检查 pending、Kernel/metadata/native 字节、无 active worker；准备不在 registry 锁内重入查询。 |

其余回归仅修执行边缘：spike/version 1 未完成在自身 `completion_fact` 映射 Failed，native/version 2 保留 Resumable；已知会话身份损坏在任何模型/工具派发前返回 Failed；配置必填缺失、授权过期、沙箱非法请求和已观察到的 MCP 初始化失败通过类型保持为 KnownFailure。IO、传输不明、取消、超时与清理失败保持 unavailable，通用 Uncertain 与 Kernel 均未修改。Plugin Turn 夹具改由 HTTP 枚举身份并读取 Kernel 门面，保留历史、冻结快照及多用户权限断言。

### §10.15 追加断言登记

| 编号/范围 | 修改及保持的业务约束 |
| --- | --- |
| A11 普通恢复授权与人工准备 | 按上表更正已裁定的动作期待与准备过程；普通 Run/渠道的核查、同 Session、效果一次断言原样保留。认证 Session-call 的 pending 准备共用真实 Code observation。 |
| A12 Plugin Turn 夹具 | 三个原场景用 GET /runs + Kernel 只读 record 取代已删除的 runs 目录读取；同 Run/workspace/Session、冻结 Graph/plugin_bindings、Alice/Bob 权限隔离和后续撤权断言不变。 |
| A13 resolver 类型保持 | 原已完成 future 不重 poll 回归增加 KnownFailure 分支，分别断言 Graph、Host、KnownFailure 分类；不通过捕获所有 Unsupported 或错误文字匹配放宽边界。 |

原 A1–A10 与 Runtime 73 项、Host 39 项逐项映射继续有效；本轮只增加上表人工恢复测试改名映射，没有删除业务测试或新增既有 ignore。最终命令、原样结果和未覆盖范围统一见 [§10.15 交回](../../../experiments/constitution-host-preflight/m3/completion-verification.md)。


§10.15 最终结果：workspace **2,153 / 0 / 125**，Web **333 / 0** 及构建通过，完整固定 Goose **122 / 0 / 0**，原 12 项失败逐项独跑各 **1 / 0 / 0**，严格 Clippy/fmt 通过；Host/Goose/helper 采证前后身份相同。首个 workspace 有一项旧动作期待失败，单独修正/复测后最终全量通过，首轮日志如实保留。[最终证据](../../../experiments/constitution-host-preflight/m3/completion-verification.md) 与 [逐 target 原样结果](../../../experiments/constitution-host-preflight/m3/completion-checks.json)。C1 零 diff、C2 save 0/mutation 0、旧类型扫描零、围栏不变。M3 已通过 §10.16 独立复验；§10.17 批准 S27、结清 S4 并授权合并。§10.18 要求先合入 `dfd3427` 并重跑门槛，交回审查后由 master 一侧快进；本分支不自动开展 M4/M5。


### M3 §10.18：同步 master 与合并验收

`376b256` 提交用户审查与 S27/S4 裁定；merge `cf23ea8` 的另一父提交为 master `dfd3427`。三处内容冲突按业务采用 master、执行保留 Kernel 处理，Session crate 与 master 字节一致。每个 Kernel worker 锁点及查询正文哈希的结论见 [冲突与专项审计](../../../experiments/constitution-host-preflight/m3/merge-conflicts.md)；保留 master 查询锁、live metadata 和 canonical-account 重启回归，未移植旧执行器表。

全部门槛通过：workspace **2,197 / 0 / 126**（132 targets）、Web **356** 及构建、浏览器 **10 / 0**、完整固定 Goose **123 / 0 / 0**（41 targets）；新增 canonical-account 用例独跑亦通过，七个二进制身份前后一致。C1 零 diff、C2 save 0/mutation 0、围栏不变，master 新增 45 个 Rust 测试全部保留。3 类机械夹具/词汇适配，删除测试 0、既有测试新增 ignore 0、业务断言放宽 0、新增 S 0；S27 已批准并结清 S4。[最终交回及逐 target 证据](../../../experiments/constitution-host-preflight/m3/merge-verification.md)。

本分支包含 `dfd3427`，待审查后由 master 一侧快进；未修改 master 工作区，M4/M5 未开始。W4、真实模型 Provider、外部渠道实网与部署仍未验。

### M4 §10.19：字段归属与旧机制收尾

M3 已快进 master，M4 在 /home/mansteinl/Anchor 实施。GraphRunRecord 删除 error/recovery/recovery_submissions/updated 及 touch，恢复读取归 RunView 的 Kernel history 投影，失败原因归 Kernel 结算；HTTP 展示键保留，Web 失败及恢复提示保留。sequence/format 的保留理由及全部业务字段归属见 [字段表](../../../experiments/constitution-host-preflight/m4/field-disposition.md)；四个指定 CompletionFact 均为当前执行端口协议，无可删。schema 15 对旧 1–14 及旧字段具名拒绝，不迁移开发数据。

M4 全部门槛通过：workspace 2,200 / 0 / 126、Web 359 及构建、完整固定 Goose 123 / 0 / 0，七个二进制身份前后一致；Clippy/fmt、围栏、C1/C2 通过。结论及最终证据集中于 [M4 交回](../../../experiments/constitution-host-preflight/m4/verification.md)，不以本段代替验收。

#### S 候选

| 候选 | 结论 |
| --- | --- |
| 无新增 | 0 条；Kernel、Graph 裁决、Session 控制及业务行为不变。 |

#### 断言修改

| 范围 | 处置 |
| --- | --- |
| M4A1–M4A9 | 状态展示、旧记录容器/时间、C6 及机制名称共 9 类；[逐项登记](../../../experiments/constitution-host-preflight/m4/assertion-mapping.md)。业务断言放宽 0，新增 3 项 Web 展示/动作回归。 |

#### 删除测试映射

| 范围 | 映射 |
| --- | --- |
| 旧机制测试及 helper | 34 个函数改名或等价替换；删除测试 0、新增既有 ignore 0、冻结快照修改 0；[机器清单](../../../experiments/constitution-host-preflight/m4/test-mapping.json)。 |

M5 未开始，不推送；W4、真实模型 Provider、外部渠道实网与部署未验。


### M5 §10.20：最终测量与迁移交回

M1–M4 已验收。任务书先单独提交为 e60374f，M5 以该 master 提交冻结全部度量；完成后迁移任务结束。C2 仍为 save 0 / mutation 0，Kernel 相对 0859373 零 diff。逐 crate 生产/测试行数、八类规则两版位点、手写 Session/授权/执行端口判断、测试函数与历史登记汇总统一见 [最终测量](../../../experiments/constitution-host-preflight/final/measurement.md)，不再沿用 P/R 的包络数字作为当前结论。

探针没有新增实现：schema 15 保留数据目录的在线点与 history 重建全等，截断/缺失原生重建不改变 resource/history；现有 Probe 三测试通过并保留 12 点样本。M5 仅改文档与测量脚本；因基点相对 M4 有独立 RSI 源码更新，重跑 workspace 2,200 / 0 / 126、fmt/围栏通过。Web 359 与构建、完整固定 Goose 123 / 0 / 0 引用 M4，未重跑完整 Goose；原始日志未入库。

| 登记 | M5 结论 |
| --- | --- |
| S 候选 | 0，无语义/Kernel/Graph 裁决修改；历史有效 S 8 条及去向见最终测试汇总。 |
| 断言修改 | 0，业务断言无放宽。 |
| 删除测试映射 | 删除 0、新 ignore 0，无需新增等价替换映射；M1–M4 各阶段条数与链接见 [登记汇总](../../../experiments/constitution-host-preflight/final/test-mapping-summary.md)。 |

W4、完整真实模型 Provider 业务、渠道实网、部署、时间事实与保留政策不由本次测量裁定；生产离线迁移用户已决定不提供，旧 worktree/分支何时删除由用户决定。统一登记在 [迁移后待决清单](../../../experiments/constitution-host-preflight/final/measurement.md#迁移后待决清单)。本轮未推送、未清理开发数据、未删除旧 worktree/分支；长期、规模与对抗压力未验。
