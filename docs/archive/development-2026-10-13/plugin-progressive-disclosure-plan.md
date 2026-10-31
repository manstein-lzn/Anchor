# Plugin 渐进式披露实施与验收计划

> 历史快照（截至 2026-10-13）：保留当时的方案、执行记录与验收边界，不再追加，也不作为当前实现、授权或待办。当前行为见 [当前架构](../../architecture.md)，仍未关闭的事项见 [当前验收与待办](../../pilot-development-plan.md)。正文中的“当前”、PID、部署状态和读取顺序均以记录当时为背景。

创建：2026-10-13。起始 HEAD：`dba4d725bc40afff50b30ccc1018b48b027ad2f0`。

## 用户目标与范围

用户已授权规划并由主 Agent 驱动子 Agent 开发、review 和验收：Anchor 的完整 Plugin（使用说明、多个 Skill、工具、资源、依赖与权限状态）能被 Goose 按任务发现和使用；大量挂载时不把全量目录、指令和参数 schema 塞入初始上下文。Pilot 与 AgentNode 使用一致披露契约，继续走现有 ACP/MCP 和原生调用链。

研究依据：[大规模 Plugin 研究报告](evidence/plugin_disclosure_20261013/research_report.md)。本计划是实现交接入口；完成状态只在实际验证后填写，不以写好接口或单测替代真实 Provider。

首轮完整范围：授权 Plugin 搜索/分页/精确定位、Plugin 级概览与使用说明、逐 Skill/资源只读读取、完整工具 schema 获取、工具归属、预算、Pilot/Node 接线及回归。MCP 懒连接和 Goose Code Mode 编译属于独立后续优化，当前继续使用正式 MCP 启动方式。

## 归属与不变量

- Library 继续拥有 Plugin 真身、代码、共享依赖及视图；Host 拥有环境、权限和调用准备；Goose 拥有 Agent loop、原生 Session/历史与 compaction。
- 执行目录从同一次准备的 PluginDefinition、绑定和正式工具定义派生。索引/摘要是可重建投影，不新增资产、存储、授权状态或历史代码快照。
- Pilot 管理目录可查询已安装 Library；执行发现只提供本 Turn 已挂载并授权的能力。Node 只提供本 invocation 的合法能力。搜索/读取不安装、不挂载、不改变权限。
- 保留现有 Turn 挂载冻结、Run 权限与代码观察分离、共享 Python/Rust、后续调用合法热更新语义。
- Skill/资源读取复用严格 confinement；拒绝越界、符号链接逃逸与凭据清单读取。披露凭据需求/可用性，不泄露值或私有配置。
- 完整选中指令/schema 不静默截断为“完整”；超限给明确诊断或可继续读取的入口。搜索/目录同时限制条数和字节，标记省略及续页。
- 必须有始终可见的发现入口及其使用说明，不依赖 tom 出现；tom 仅有界提醒。普通提示中的 Plugin/Skill 路径列表也受预算控制。
- 检索支持多词、中文及精确 ID/分页兜底，不承诺百分之百语义召回；不先引入向量服务或自有 Agent 框架。
- 元工具调用解析真实目标后仍走原授权执行适配。Pilot 原生 peer/elicitation、Node final_result、真实业务工具身份、观察/回执、媒体、错误、取消必须保留；不能只在工具列表外包装后绕开原路径。
- 复用现有环境变量兼容显式披露开关；执行验收明确记录所用配置，不暗中修改当前部署 `.env`。

## 实现组织

单一实现者负责共享契约及 Rust 接线，避免并行编辑同一模块。主 Agent 负责架构约束、现场验证、review、文档和最终判断；独立 review Agent 只读审查。

主要边界：

1. `plugin_view.rs`：在现有短 Library lease 内派生授权 Plugin 目录事实，随准备结果传递；保持锁外连接。
2. 一个有独立边界的披露/只读模块：Plugin/Skill/资源概览和读取，工具检索/详情，预算和可达性；复用已有端口与路径工具。
3. `tool_disclosure.rs`、`node_host.rs`、`goose_acp/pilot.rs`、必要的 bridge 接线：共享披露策略，保留真实目标调用和原生确认/完成。
4. `pilot_tools/plugins.rs` 与工具定义：管理 list 分页/搜索；read 精确选择说明、Skill 或资源，不附带其他全文。
5. `goose_acp.rs` / configuration：消除全量路径提示，提供有界发现说明；不改框架 loop 或原生历史格式。

具体 API 命名和内部拆分由实现者基于代码决定，新增契约必须有清晰范围、分页与读取语义。工具归属由装配事实提供，避免凭名称前缀猜所属 Plugin。先建立一个可运行的 Pilot+Node 垂直切片，再补边界，禁止只交 Skill 子集。

## 验收与验证顺序

### 定向契约

- 10/100/1,000 项合成目录；名称、长描述和多工具 Plugin；初始定义/提示、单页输出有界，末页/冷门条目仍可精确读取。
- 中文、多词检索，同名/相似工具及稳定 Plugin 归属；搜索与详情提供完整复杂 schema。
- Plugin 无 Skill、只有工具/资源时可发现和使用；多个 Skill 时只读取所选项；读资源不附全部说明。
- Node 授权集合与 Pilot 管理集合分离；未挂载/越界/凭据清单/撤销权限不能经元工具绕过。
- 本 Turn/后续 Turn、代码热更新与依赖视图沿现有契约；元工具不能改变执行授权。
- 普通结果、多媒体、业务错误、原生确认、取消和 Node 完成回执保持真实目标语义。

### 实际 Goose 确定性回归

复用小型 fixture Graph 和本地 Provider，实际经过 Host、GraphRunner、NodeExecutionPort、固定 Goose ACP、授权 MCP；只替换模型传输。覆盖 Pilot 和 Node 的发现→读取→详情→真实调用、原生完成、原生问题/确认、压缩/接续后重读。按需扩展现有 fixture，不跑大型业务 Graph。

### 主 Agent review 与真实 Provider

主 Agent 独立复核授权边界、调用投影、预算和唯一事实来源，并运行隔离 Host 的真实 Provider 切片：Pilot 与小型 AgentNode 实际通过发现/读取/工具详情执行一项随机标记任务，核对工具参数、实际回执、产物和最终状态。大规模上下文成本由合成目录证据验证，真实任务不得仅凭模型声称成功。

隔离实例使用独立 state/catalog/library/workspace/端口，不改运行中的 8077/5173；不发送外部业务消息。模型凭据从已有配置安全读取，不写入证据。

### 收尾

定向通过、垂直切片完成后，一次必要 Host all-targets 回归、Host Clippy `-D warnings`、workspace fmt。按真实改动增加其他 crate 检查；无 Web 改动不重跑 Web。真实 Provider、确定性 fixture、源码接线和未覆盖边界分开记录。

验收后更新 `docs/architecture.md` 当前实现、`docs/product-architecture.md` 实际影响段落，并在 `docs/pilot-development-plan.md` 追加一条对应切片证据。仅提交/部署用户要求的范围；开发与验收不自动重启现有服务；用户后续已授权本次 commit/push，未要求部署。

## 当前交接状态

2026-10-13：本切片的开发、子 Agent 只读审查与主 Agent 验收完成，台账项 **A222**。实现者使用 GPT-6.1-sol / max，主 Agent 负责架构边界、最终检查及真实 Provider 验收；代码和验收证据随本次提交固化，未部署。可复用脚本、实际命令、二进制身份和验证范围见 [验收证据](evidence/plugin_disclosure_20261013/acceptance/README.md)。

### 已实现契约

- Pilot 与 AgentNode 共用 `anchor_plugins`、`anchor_plugin_read`、`anchor_tools`、`anchor_tool_detail`、`anchor_tools_call`；支持完整 Plugin 概览、说明、多个 Skill、资源、安全声明及完整工具参数。Pilot 的 `plugin_list`/`plugin_read` 仍是已安装 Library 的管理查询，不赋予执行权限。
- 执行目录从同一次 Library 准备事实派生，工具归属来自实际装配。发现入口始终可见，普通提示不再平铺全部 Plugin/Skill 路径；`tom` 仅提供短提醒。工具 schema 开关沿用 `ANCHOR_NODE_TOOL_DISCLOSURE`：`1` 强制、`0` 直接提供 schema，默认隐藏工具超过 3 个时启用；Pilot 固定管理工具直接可见。
- 搜索使用完整元数据及 Skill 头部，多词 AND 子串，支持中文、精确 ID 与分页。目录默认 20/最多 100 项，同时受 16 KiB JSON 页限制；单项详情最多 256 KiB。UTF-8 文件最多 1 MiB，字节页默认 16 KiB/最多 32 KiB，返回 digest 和 `next_offset`，正文变更可检测；续读以 `next_offset=null` 结束。
- 配置资源按已验证规范路径排除。读取持 Library 非阻塞共享 guard，核查当前授权及旧/当前公开路径交集；正式发布持相同排他 lease，busy 明确拒绝并允许重试。Pilot 管理定义与 Skill 元数据在同一 guard 内读取，首次安装忙时不会越过 guard 扫现场。
- 元工具解析真实目标后保留原 ACP 调用身份及原授权执行路径；实际业务工具名/参数进入观察，原生确认、问题、结果/媒体、错误、回执、取消及完成工具继续沿既有链路。递归元工具和间接 `final_result` 被拒绝。没有新增持久资产、Agent loop、上下文或恢复系统。

### 实际验收

| 层次 | 已执行结果 | 证据与边界 |
| --- | --- | --- |
| 定向契约 | 披露、管理发布 guard 与冻结挂载合计 28 passed；Bridge 11 passed | 覆盖大目录、中文/长描述尾部搜索、UTF-8 分页、复杂 schema、权限、凭据配置及热更新读取 |
| 固定 Goose + 确定性 Provider | 7 次 Cargo 调用、10 个实际 Goose 场景全部通过 | Pilot/Node 披露、混合媒体、ask_user、删除确认 true/false/decline/cancel、compaction 后重读和本地 WeCom mock；不是实际企业微信发送 |
| Host 与 GraphHost all-targets | 30 targets，910 passed / 0 failed / 86 ignored，退出 0 | 分别为 Host 869 和 GraphHost 41；含 target 间共享 fixture 的重复运行，不作为 unique test count；默认 ignored 不因此视为已执行 |
| 等价 lint 收尾后的最终检查 | 20 disclosure passed，Host build、all-targets Clippy `-D warnings`、workspace fmt check 均退出 0 | 全量检查后只将 frontmatter 的手动切片改为等价 `strip_prefix`；最终真实 Provider 使用该收尾后的二进制，没有冒充又跑一遍全量 |
| 最终真实 Provider | `deepseek-flash`；Pilot 与 AgentNode 均 completed | Library 1,000 Plugin；Pilot 挂 10 个，Node 挂 1,000 个；目标 MCP 1,001 工具。两者真实读取说明、正确 Skill、随机 token 资源及完整 schema，各有独立 MCP 调用日志，产物、trace、回执及 final_result 一致，没有加载无关 Skill 正文 |
| 规模测量 | 10/100/1,000 工具的初始披露定义均为 3,223 B；1,000 完整 schema 为 493,671 B | 合成 ToolDefinition 序列化测量，不是 tokenizer 计数，也不是完整 Pilot prompt 或 1,000 MCP server 启动测量 |

最终真实 Provider Host SHA256：`acb782f176db656dc50c0490fe62163cdee1a02423b398132a010bbbd8eb34fd`；固定 Goose SHA256：`71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7`。Session 删除 HTTP 200、Host 退出 0、监听关闭，没有 panic 或存活所属子进程；部署 `.env` 与共享解释器字节未变。最终概要见 [final-real-provider.json](evidence/plugin_disclosure_20261013/acceptance/final-real-provider.json)，回归明细见 [checks.json](evidence/plugin_disclosure_20261013/acceptance/checks.json)。

### 收敛边界

本次通过完整 Plugin 披露及原授权调用链，不扩写为公共市场搜索、安装/authoring、真实外部业务、1,000 个 MCP server 并发连接或部署验收。真实场景预置隔离 Library，只启动目标 MCP；MCP 懒连接与 Goose Code Mode 未实施。

早期确定性 fixture 的失败与修正见验收证据：大 debug 复制件触发 MCP mount 超时、旧 bundle 携带 Plugin 快照违反现有 id-only 契约、误删 Pilot 所属 Graph 被拒，以及本地业务 mock 被 loopback 策略拒绝。修正 fixture 后相应场景通过，未因此修改全局超时或扩大宿主权限。

当前开发部署仍为原 Host PID 184037，SHA256 `984578f85d9bc460a8868f53236f8e99c04bc61d69f616993adcb67cb42d2f15`，显式配置 `ANCHOR_NODE_TOOL_DISCLOSURE=0`；隔离验收显式使用 `=1`。本次未更改 8077/5173、独立 8078、模型配置或共享依赖。

2026-10-13 后续 A223：Goose 原生动态扩展的两项确定性接口实验完成，明确 ACP add 的轮内刷新缺口、原生 manager 的全量 schema 与管理边界。后续按需激活的实施方向见 [能力复用路线](../../goose-native-capability-reuse-plan.md)；此实验不关闭 MCP 懒连接，不改变 A222 的真实验收身份。

## 持续能力摘要与会话挂载更新（A224）

2026-10-13 用户授权主 Agent 驱动子 Agent 实现、review 和验收，现已完成本切片。当前实现与实际范围见下文及[独立验收证据](evidence/plugin_disclosure_20261013/capability-context/README.md)；A222/A223 的验收不回填本切片。代码与证据随本次提交固化，未部署。

### 边界与实施

- 从同一次准备的冻结、获授权 `PluginDisclosureCatalog` 生成名称、ID、用途简述及读取入口；状态来自实际准备结果，不把未挂载的 Library 管理目录投影为执行能力。普通规模提供全部摘要；总字节预算之外明确标注还有更多条目及完整搜索入口，不让省略意味着不存在。
- 通过 Anchor 聚合 MCP 的 `initialize.instructions` 接入固定 Goose 原生扩展系统指令；每次正常推理请求沿框架发送当前摘要。摘要不依赖对话历史或 `tom` 存活；说明、Skill、资源与完整 schema 继续按需读。Pilot/AgentNode、直接 schema 及渐进 schema 模式共用同一投影。
- 新 Turn 从当前 Library 和冻结挂载重新派生摘要、目录及工具。原生 Session load 后以当前扩展替换旧执行投影，不能把旧摘要或扩展集合当授权。
- 网页挂载复用 Session 挂载事实、Pilot 接纳 gate、当前执行的冻结快照及既有幂等 continuation。空闲点击仅影响下次请求；活动 Turn 正常收束后以最新能力接续原任务。观察 source Turn、保存挂载及登记接续需要一致，保留 catalog → Library 锁顺序，不能在完成/停止竞态下唤醒旧任务。
- 多次点击合并为同 source Turn 的一次接续；失败、中断、停止、删除、归属变化、归档或新用户 Turn 取代旧请求时不自动续接。Web 根据宿主事实解释何时生效，并沿已有轮次跟随显示后续执行。

不增加 Kernel 业务策略、Plugin 状态库、记忆/计划系统、Agent loop 或 Goose 私有接口。MCP 懒连接仍按独立后续切片推进。

### 验收顺序

1. 摘要预算、状态与执行授权的一致性，挂载 source/接续幂等及并发/停止/新消息边界的定向 Rust 回归；Web 状态与轮次跟随定向测试。
2. 固定 Goose + 确定性 Provider 实际验证摘要位于系统提示、压缩后仍在、新 Turn/load 后更新和真实调用；浏览器验证点击与跟随。只替换模型传输，保留正式 Host/Runtime/ACP/MCP 路径。
3. 隔离真实 Provider 任务不点名 Plugin 或发现入口，核对主动选用、实际参数、回执、独立工具日志和产物；再验证同一会话挂载/停用后更新。真实模型与确定性边界证据分别记录。
4. 主 Agent review 后一次必要的 Host 全量回归、Clippy/fmt 与 Web 检查；实际改变相关代码后才扩大/重跑。验收完成再更新当前架构和台账。开发与验收不自动提交、部署或修改当前服务。

### 实际验收与边界

| 路径 | 实际结果 | 证据范围 |
| --- | --- | --- |
| 摘要与会话加载 | 24 项披露测试、MCP initialize 1 项、固定 Goose 的两个 schema 模式加载测试通过 | `tom` 关闭时摘要仍在；同一原生 Session 的挂载、停用、Host 重启加载均刷新当前系统摘要 |
| 原生压缩 | 确定性 entry/reactive 两场通过；分别 8/5 个普通推理请求保留摘要 | 保留框架专用摘要请求的独立提示；压缩后重读 Plugin/现场，旧回执不能完成，无业务重放 |
| Web 挂载与控制 | 120 Web 单测、12 浏览器测试及隔离目录 build 通过 | 实际 Host+Goose 验当前 Turn 冻结、多次切换合并与停止；停止/新消息/重开竞态另有 mock 浏览器及 Rust 回归 |
| 真实 Pilot | 自然任务主动选用 Plugin，同 Session 活动挂载后一次接续，能力核查不重复出报告；真实原生压缩通过 | Host `59d501f2…`；压缩后正常请求含当前摘要，但模型从压缩历史回答，没有再次调用工具。aggregate 后续在 Node 失败，保留为 Pilot 单独通过证据 |
| 最终真实 AgentNode | 999 个挂载 Plugin，摘要显示 87/省略 912，目标在省略部分仍被查到；读取说明/Skill/资源/schema 后仅一次业务调用，JSON 产物与原生完成一致 | 最终 Host `38fde27e…`；13 个真实模型请求，最新实际回执通过，无无关 Skill 正文请求；与 Pilot 验收使用不同 Host 构建 |
| 收尾 | Host all-targets 30 targets：893 passed / 0 failed / 90 ignored；最后的 Bridge 12 项与 native guessed-receipt 1 项、build、Clippy/fmt 通过 | 全量运行早于最后的回执提示/错误文案修正，没有宣称最后又跑一遍全量；其他原生用例仅同步文案断言，未复跑 |

回执验收发现原提示把所有非完成工具误称为业务工具，且真实错误没有解释回执不匹配；已修正文案并保留校验逻辑。早期真实模型出现多读无关 Skill、能力接续重复已完成报告，以及 fixture 的输入长度、终态采样和产物格式问题，均在[证据说明](evidence/plugin_disclosure_20261013/capability-context/README.md)保留，未改写失败为通过。

本片复用 Goose 原生扩展系统提示、Session load 与 compaction；Library 和 Host 仍拥有资产及授权，没有第二套目录存储、记忆、Agent loop 或恢复引擎。MCP 仍按已有准备流程连接；懒连接、Code Mode、Plugin 安装、外部业务和 1,000 个 MCP server 启动不属于本片验收。
