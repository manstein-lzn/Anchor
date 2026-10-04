# Rig 能否承担 Anchor 的 Harness

核查日期：2026-10-02。结论：**Rig 适合作为 Rust Agent 执行基础，但本次核查的官方 0.43.0 组件并未提供与 PydanticAI 2.46.0 + Harness 0.36.0 相当的、可配置即用的长任务上下文管理与持久恢复组合。若“不自行实现复杂 Harness”是选型硬条件，当前不能判定 Rig 满足。**

这不等于 Rig 无法实现这些能力。窗口整理、请求 hooks、工具检索、记录与回放已有可复用实现；单纯补每请求滑窗只需薄接线。主要缺口在模型语义压缩、完整请求预算、压缩状态持久化、大输出按需回读和未知副作用恢复的一致集成，不是一个缺少的函数。

## 核查范围和证据等级

- 项目固定及实际使用：`rig-agent/core/rmcp = 0.43.0`；Python `pydantic-ai-slim=2.46.0`、`pydantic-ai-harness=0.36.0`、已安装 Monty 1.0.0。不是旧版 Rig 与最新版 Python 的错位比较。
- 核查已安装源码，下载同版本官方 `rig-memory`、`rig-cassette`、`rig-ecs`，并查看 `rig-sqlite/postgres` 的职责。发布包 VCS 指向 `654567eb64274fca00cab86cdd32c86b9913769e`。GitHub releases/latest 与 crates.io 在本次查询中均报告 0.43.0。
- 运行隔离 Rust/Python 确定性实验，不调用真实 provider、不读生产对话/凭证、不改运行时或依赖清单。源码存在、合成实验通过、Anchor 接入、真实 provider 验收四个层次分开。
- 证据目录：[审计清单](../.local/rig-harness-audit/evidence.json)、[Rust实验结果](../.local/rig-harness-audit/probe-results.json)、[Python实验结果](../.local/rig-harness-audit/python-probe-results.json)。GitHub main 递归树下载出现 IncompleteRead，改以固定发布包、tag文件及官方接口为范围；没有穷尽第三方生态，也不对多年运行作经验性保证。

## 能力对照

下表的 Python 栏是框架已提供的能力，不代表 Anchor 生产路径全部已启用。

| 能力 | PydanticAI/Harness 固定版本 | Rig 0.43.0 官方组件 | 对 Anchor 的含义 |
| --- | --- | --- | --- |
| 模型/工具循环、结构化输出、流式、并发工具 | 已有 | rig-agent 高层 Agent/Runner 已有，低层 AgentRun 可序列化 | 已有可靠复用基础，无需重造 loop |
| 一次任务内每次请求前压缩 | Compaction Capability 每请求执行，压缩改动沿当前历史持续生效 | memory 默认在新Run入口load，成功结束append；每请求可用hook/RequestPatch接策略 | 仅安装rig-memory不能保护单个长AgentRun；薄hook可实现滑窗，但不是完整Harness |
| 滑窗、按token裁剪 | SlidingWindowCompaction，估算或自定义tokenizer | SlidingWindowMemory、TokenWindowMemory、HeuristicTokenCounter | 两边都有，Rig保留工具配对；裁剪有损，不保证关键事实保留 |
| 模型语义摘要及增量摘要 | SummarizingCompaction可调用模型，已有结构化摘要提示、近期消息保留、incremental | Compactor接口、CompactingMemory协调；内置TemplateCompactor做文本拼接/截断 | 本次固定官方组件未找到现成通用LLM语义Compactor，需适配模型和验证策略 |
| 分层压缩和降级 | ClearToolResults、DeduplicateFileReads、TieredCompaction、FallbackCompaction、ClampOversizedMessages | 可通过policy/hook组合，未确认等价的现成自动策略链 | 不应把可扩展接口当作已交付的策略组合 |
| 保留任务约束和压缩可追溯性 | pin/reinject、保留用户消息、可选compaction receipts及记录引用 | 原历史可保留，通用窗口策略无等价自动任务约束pin/receipt组合 | 请求裁剪后仍需防止目标/约束丢失；摘要不是权限或完成事实 |
| 请求容量估计与压缩触发 | max_fraction/model窗口、provider usage锚定估算、近期新增消息与工具schema估算；也有估算误差 | TokenWindow只计传入messages；摘要在policy预算外；模型提示、工具schema、当前prompt、输出预留需组合处理 | 不能以窗口参数推断总请求必定不超限，两边仍需正确部署参数 |
| 大工具结果 | ToolOutputLimits支持截断、Spill/read_tool_result、模型摘要和fallback | rig-ecs有ToolResultLimit，请求级text头尾截断，JSON/图片不截；hook可改结果 | ECS能力不能直接说成classic已启用；未确认等价通用自动spill/分页回读组件 |
| 动态工具/文档发现 | ToolSearch/defer_loading，CodeMode可与之组合 | dynamic_context、retrieved_tools、MCP托管工具目录已有 | 应优先复用Rig高层接口；动态检索不等于自动实现同一套ToolSearch/CodeMode |
| CodeMode | Harness + Monty已提供 | 本次官方组件未确认通用等价物 | 当前Anchor Rust只接沙箱命令，并非CodeMode替代 |
| 记录/持久存储 | StepPersistence及File/SqliteStepStore、效果状态、continue_run/恢复检查 | rig-cassette记录/回放、Checkpoint数据结构；内存Recorder，持久store由宿主负责 | 有记录/回放机制，不等于已接耐久写入/崩溃对账 |
| 恢复 | 有continuable/interrupted快照和未结工具效果检查；宿主仍判断重放安全 | Agent.resume不load/append memory，无结果pending tool会重执行；ECS恢复也会再发未结effects | 高层resume不能直接替换Anchor未知副作用门禁 |
| 长期增长与归档 | FileStepStore/完整历史读取也会增长，快照保留可配置 | 完整AgentRun/原始provider responses增长；memory先load完整历史再整形；compaction缓存进程内 | 两边都不能仅凭框架名承诺多年高负荷可用；可见prompt有界不等于记录/恢复有界 |

## 实验：单次长工具循环

使用各框架自己的合成模型，连续产生30次工具调用，再给最终答案，共31次模型请求。以下是功能触发范围验证，不是token成本或性能公平基准；两侧窗口参数不同，不比较压缩优劣。

| 配置 | 结果 | 能证明什么 |
| --- | --- | --- |
| Rig高层Agent + PolicyMemory(SlidingWindowMemory保留2条)，起始2条历史 | memory.load只执行1次；模型看到的历史从3条递增到63条 | memory入口窗口不自动约束当前Run后续工具循环 |
| Rig额外薄hook，每次请求应用现成SlidingWindowMemory保留6条历史 | 请求消息最多7条（包含额外当前prompt），配对校验通过；返回的完整transcript仍62条 | 扩展点有效，窗口能力容易接；上下文view有界不意味着持久状态有界 |
| Harness SlidingWindowCompaction(max_messages=10,keep_messages=4) | 31次请求反复触发压缩，请求最多9条消息，最后工作历史8条 | 已配置的框架compaction会在同一长任务内部持续执行 |

Rig薄hook仅用于验证接入可行性，不保留任务约束、不生成语义摘要、不保存摘要、不计工具schema或模型全部输入，未加入Anchor。第一次实验误把当前prompt同时放入patch与请求，断言失败；按公开接口改为只替换history后，最终请求通过规范工具配对检查。不能用一段能运行的hook宣称完整Harness完成。

## 实验：滚动摘要、预算和重建

使用110条合成历史，每条约1KiB，没有真实用户数据。

- 默认TemplateCompactor搭配只保留2条近期消息，加载结果始终3条消息，但序列化体积从 **10,535 B增长到114,458 B**。默认“summary”不是语义压缩，也不是固定大小。
- TokenWindowMemory使用同一HeuristicTokenCounter、历史预算1000，接CompactingMemory后返回内容按同一估算器计 **28,559 tokens**。原因是摘要明确在policy预算之外；这是本地估算，不是provider计费token。
- 配置TemplateCompactor的512字节上限后，视图为2717字节（还包括近期两条历史和序列化开销），底层仍保存110条原消息。框架已有可用裁剪旋钮，但它会丢弃文本，不能替代语义保留。
- 同一wrapper重复load不重复处理旧前缀；重建wrapper后重新处理108条旧消息，计数从108增至216。实验只重建内存对象，未做kill进程；它与源码所述“摘要和watermark为process-local，重启会recompact”一致。
- 仅把发送视图裁成2条消息，AgentRun仍保留111条完整历史，序列化约119,878字节。原始记录保留本身不是错误，但长期运行需另外解决记录布局、读取范围和归档。
- TokenWindow遇到单条最新消息大于预算可返回空历史，不能自动推断“最近用户任务永远保留”。集成必须明确哪些内容不可丢弃。

## 不应忽略的 Rig 现成能力

此前只审AgentRun时，低估了Rig高层模块。高层Agent具备hooks、模型路由、工具并发、上下文/工具检索及record_to；官方rig-cassette提供effects回放与校验，rig-ecs提供持久世界的序列化/恢复和请求级工具text限制。它们都是真实复用候选，不能说Rig只有provider SDK。

但它们的职责边界明确：cassette默认Recorder是内存Vec，Checkpoint<S>是driver-owned state的serde envelope；ECS save_world返回对象，不是现成耐久文件store。回放已完成effects和安全恢复结果未知的外部操作是不同能力。选择ECS也没有在本次核查中补齐自动语义压缩，而且会引入另一个较大的运行时组织方式，不能仅为找“harness”名称替换现有Graph架构。

`rig-sqlite` / `rig-postgres` 本次下载包提供向量存储，不能据名字当作ConversationMemory或StepPersistence文件/数据库后端。

## Python现有接线也不能美化

- Anchor普通Node只有提供conversation_id时默认安装200→40滑窗；Pilot默认滑窗，可按配置增加摘要。
- `src/anchor/node/context.py` 有Anchor自己的token预算、超窗重试、/kept spill和JSONL观测，但本次搜索只发现测试直接调用context_capabilities，不能当作默认生产路径已启用。
- Harness的ToolOutputLimits、AskUser为框架能力，Anchor生产没有直接实例化前者；Pilot提问使用自己的session_ask+PydanticAI deferred。
- MCP defer_loading与可用时自动CodeMode确有生产接线。
- 原生StepPersistence已接入，但Anchor仍补了恢复判断；FileStepStore默认保留与整段历史/事件读取也存在长期增长问题，不能声称Python已经证明“运行几年无问题”。

因此需同时区分“Rig vs Harness框架可提供什么”与“Anchor两条路径现在启用了什么”。用户要求的是前者至少覆盖核心需求，不能通过把Python未启用的能力删出比较来降低这个要求。

## 选型判断与下一步

- **作为Rust模型/Agent基础：可以继续保留候选资格。**工具循环、高层扩展、结构化输出、检索及MCP有明确复用价值。
- **作为少量接线即可替代PydanticAI/Harness的长任务内核：目前不通过。**最关键的自动语义压缩、压缩后的durable状态、大输出spill和安全恢复组合仍需要显著集成/实现，不能承诺只是配置。
- **多年高负荷稳定：两者都没有因本次源码或合成实验获得这个结论。**短请求窗口、工作状态、长期存储、任务分段与负载控制是不同验收层次。
- 不继续默认自研缺失Harness，也不立即推翻已验证的Rust Graph/Artifact/Sandbox模块。先将Harness替代作为独立选型门槛，保留当前生产路径；若用户维持“不自行实现复杂Harness”，下一轮应比较可复用的完整Agent执行引擎，而非继续扩大Rig周边迁移。

## 主要源码来源

- [Rig 0.43.0 release](https://github.com/0xPlaygrounds/rig/releases/tag/v0.43.0)
- [AgentRun职责与恢复边界](https://github.com/0xPlaygrounds/rig/blob/654567eb64274fca00cab86cdd32c86b9913769e/crates/rig-agent/README.md#the-run-protocol)
- [高层Run历史加载](https://github.com/0xPlaygrounds/rig/blob/654567eb64274fca00cab86cdd32c86b9913769e/crates/rig-agent/src/agent/runner.rs#L446)
- [CompactingMemory预算与process-local边界](https://github.com/0xPlaygrounds/rig/blob/654567eb64274fca00cab86cdd32c86b9913769e/crates/rig-memory/src/lib.rs#L815)
- [TemplateCompactor](https://github.com/0xPlaygrounds/rig/blob/654567eb64274fca00cab86cdd32c86b9913769e/crates/rig-memory/src/lib.rs#L1007)
- [Agent.resume语义](https://github.com/0xPlaygrounds/rig/blob/654567eb64274fca00cab86cdd32c86b9913769e/crates/rig-agent/src/agent/completion.rs#L635)
- [Cassette内存Recorder](https://github.com/0xPlaygrounds/rig/blob/654567eb64274fca00cab86cdd32c86b9913769e/crates/rig-cassette/src/effect_log/recorder.rs#L23)
- [ECS工具结果text限制](https://github.com/0xPlaygrounds/rig/blob/654567eb64274fca00cab86cdd32c86b9913769e/crates/rig-ecs/src/agent/content/parts.rs#L167)
- [本地固定Harness compaction说明](../.venv/lib/python3.12/site-packages/pydantic_ai_harness/compaction/README.md)
- [本地固定Harness ToolOutputLimits说明](../.venv/lib/python3.12/site-packages/pydantic_ai_harness/tool_output_limits/README.md)
- [本地固定Harness StepPersistence说明](../.venv/lib/python3.12/site-packages/pydantic_ai_harness/step_persistence/README.md)

复跑：`CARGO_TARGET_DIR=/tmp/anchor-rig-audit-target cargo run --manifest-path .local/rig-harness-audit/probe/Cargo.toml --quiet`；`./.venv/bin/python .local/rig-harness-audit/python_probe.py`。仅合成实验，不是生产环境验收。
