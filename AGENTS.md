# Anchor 开发约定

本文件只写长期有效的开发原则和约定：状态、进度、证据和未验证项一律不写在这里。

## 开工与信息来源

- 开始一项新任务先读本文件，以及与任务直接相关的 `docs/pilot-development-plan.md`、`docs/architecture.md`、`docs/product-architecture.md` 章节；已在当前会话读过且没有变化的内容不必重复通读。
- `docs/pilot-development-plan.md` 是进度和验收台账；`docs/architecture.md` 是当前实现真相；`docs/product-architecture.md` 是目标设计。目标设计未标明已实现的部分不能当作现状。

## 工作节奏

- 默认直接推进用户已经授权的工作。不要因为计划还可以继续细化、存在多个可逆实现选择，或缺少一份形式化文档而停下来询问。
- 先交付一个能运行、能验证、能被下一步复用的垂直切片，再扩展边界。不要为了“最小版本”删掉用户已经要求的产品能力，也不要为了未来可能需要而提前建设平台。
- 在保证产品行为、权限、恢复和数据一致性质量的前提下控制无效耗时：优先复用已有实现和契约，先跑与当前改动直接相关的定向检查，垂直切片完成后再跑一次必要的全量验证；不要因小编辑、重复集成或未改变相关代码而反复构建和跑全量测试。
- 只有会改变用户可见结果、持久化事实或迁移、权限边界、并发/恢复语义，或者属于不可逆外部操作的决定，才需要暂停讨论或记录架构决定。普通命名、模块拆分、错误映射和可逆实现由开发 Agent 根据仓库证据决定。
- 独立任务可以并行委派给子 Agent；同一路径只保留一个实际编辑者。主 Agent 负责边界、合并、验证和最终判断，不等待无关的形式化报告。
- 并行只用于真正独立且契约稳定的工作包；共享契约或同一代码路径由单一编辑者负责，避免通过反复同步抵消并行收益。子 Agent 已提供足够证据后，主 Agent 应直接集成和验收，不为获取重复报告继续等待。
- 恢复的产品路径是保存事实后让 Agent 核查现场并继续。底层 fail-closed 状态可以保留，但不要把 Harness attempt、内部恢复决定或审计字段包装成用户审批流程，除非产品明确要求。

## 改动与提交纪律

工作区可能同时存在多条工作流的未提交改动，据此：

- 不 `git reset`、不 `git checkout --`、不 `git clean`。
- 不把无关改动自动纳入提交；一次提交只包含你实际改动的路径。
- 不清理历史，不覆盖别人的改动；需要撤销快照或丢弃改动时先问用户。

## 硬约束

- `src/anchor/serve.py` 不得在模块级 import pydantic_ai，`tests/test_node_controlflow.py` 会拦。
- 未经真实 provider 端到端跑通的事，不写进文档说已完成。
- 一个垂直切片、用户可见行为或验收状态完成后，才在 `docs/pilot-development-plan.md` 追加一条推进记录，并更新实际受影响的验收项。单个编辑、重跑测试、修复格式或中间尝试不单独记账。

## 验证

- 先跑与改动直接相关的子集；一个垂直切片完成或跨模块语义改变后，再跑必要的全量检查。不要每改一点就重跑全量。
- 测试反馈按阶段收敛：定向测试用于快速修复，跨模块切片完成后执行一次全量回归；并发超时或环境抖动要先独立复跑受影响用例，再决定是否需要扩大验证范围，不把重复失败当作新的功能工作。
- Python 全量：`./.venv/bin/python -m pytest -q -n 8 --dist worksteal`；相关子集按改动选择。`--dist worksteal` 必须带，避免慢用例集中到一个 worker。
- Rust 按 crate/工作区运行 `cargo test`、`cargo clippy` 和 `cargo fmt`；Web 按需要运行 `npm --prefix apps/web test`、`npm --prefix apps/web run test:e2e`、`npm --prefix apps/web run build`。
- Runtime 行为回归默认使用小型 fixture Graph 加确定性本地 Provider，实际经过 Host、GraphRunner、NodeExecutionPort、io-harness 和 Rig adapter，只替换模型传输；大型 RSI、深度研究、周报等 Graph 只用于低频业务内容验收，不在常规回归中重复运行。
- 验证报告只写实际执行的命令和结果；失败、跳过、provider 未配置和未覆盖的边界必须如实区分，不为通过门槛而扩大测试范围或伪造证据。

## 框架优先

- 先查固定版本框架的公开接口和仓库已有调用，再做必要接入；不从设计自有 Agent loop、上下文系统、恢复引擎或日志格式开始。
- 已有 StepPersistence、FileStepStore / SqliteStepStore、continue_run、inspect_recovery；文件记录使用框架原生 JSONL + 快照等文件，不另写日志/恢复引擎。
- 不新增 Anchor 记忆或计划系统；需要上下文压缩时接现有 compaction。研究目标与证据验收归研究 Graph / Plugin，不列为通用运行时阶段。
- 接口存在、小样例通过、Anchor 接入、真实 provider 端到端通过是不同状态，必须如实记录。

以上 Python 规则适用于 legacy PydanticAI/Harness 路径。Rust-native Runtime 以固定版本 io-harness 作为 Agent loop、上下文、compaction 和单节点恢复基础，Rig 作为 Provider transport adapter；Anchor 负责 Graph、Run、Artifact、Sandbox、Plugin 和宿主权限事实。Rust 只补产品契约确实缺失的能力，优先复用成熟 crates，不复制另一套 Agent loop。

## 架构默认

- 以 [产品与系统架构中的能力归属和执行不变量](docs/product-architecture.md#架构指导新能力放在哪里) 为默认决策规则；开始跨模块改动前，先确定归属层、事实所有者和调用契约。
- 默认采用 Ports and Adapters 隔离 HTTP、CLI、Web、通道和 provider 等外部接口；依赖由适配器指向稳定契约，核心执行不得依赖入口协议或研究/周报等具体业务。
- 用限界上下文的思路划分 Graph/Run、Session/Turn、Library/Plugin、Node Runtime/Sandbox 的事实与不变量。只借鉴边界和所有权，不引入完整 DDD 仪式、微服务或额外框架。
- 保持 Clean Architecture 的依赖方向：易变的外部适配依赖稳定的核心策略；模块只通过小契约交互，不读取或修改其他模块的私有可变状态。
- 保持 Graph + 显式 Plugin 资源 + Runtime 可形成独立执行闭包：完整服务、WebUI、Session、Scheduler 和平台渠道应是可组合的宿主能力。平台与独立部署复用同一 Runner，不复制执行语义；分发包不得包含密钥或擅自扩大宿主授权。
- Rust 是共享 Runtime Kernel 的目标语言。平台宿主与独立宿主必须调用同一个 Kernel；迁移按垂直切片逐步替换，不建两套 Rust Runner。io-harness 是 Rust-native AgentNode 的默认执行基础，Rig 只提供模型传输适配；PydanticAI/Harness 仅作为现有 Python 路径或兼容适配，不能把兼容层误称为 Rust 产品核心。
- Rust 迁移保持 Anchor 的产品形态和用户可见能力，而不是 Python/Harness 的内部格式；Graph、AgentNode、OpNode、Plugin、Run、Session、观察/控制/恢复、产物和独立 Graph 包是需要持续验收的产品边界。Rust 可以重新设计存储、事件、API、Session 和 bundle manifest，但语义变化必须明确记录并验证。
- 小型、可逆且不改变用户语义的实现选择由开发 Agent 根据仓库证据决定。只有决策会改变用户结果、持久事实/迁移、权限边界或并发/恢复语义时才需要用户明确；重要决定记录在对应架构文档/ADR 中。
- 不默认引入微服务、事件溯源、CQRS、通用插件框架或依赖注入容器。每个新增机制必须解决已观察到的问题或明确验收要求。
- 新职责按稳定边界分模块；不要把无关职责继续堆进大型文件，也不要为了追求目录数量而拆分。只有出现真实耦合、并发/恢复不变量或独立测试边界时才抽取模块。
- 文档和 ADR 服务于后续开发，不是审批材料。只为会影响后续选择的架构决定写 ADR；常规实现、测试结果和当前进度写入对应台账或提交说明即可。
