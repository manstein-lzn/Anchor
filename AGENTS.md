# Anchor 开发约定

本文件只写长期有效的开发原则和约定：状态、进度、证据和未验证项一律不写在这里。

## 每次开工先读

- `docs/pilot-development-plan.md` — 开发唯一台账：阶段、冻结契约、验收矩阵、推进记录，当前进度与证据以它为准。
- `docs/architecture.md` — 当前实现真相。
- `docs/product-architecture.md` — 目标设计；标注为目标设计的部分尚未实现。

## 改动与提交纪律

工作区可能同时存在多条工作流的未提交改动，据此：

- 不 `git reset`、不 `git checkout --`、不 `git clean`。
- 不把无关改动自动纳入提交；一次提交只包含你实际改动的路径。
- 不清理历史，不覆盖别人的改动；需要撤销快照或丢弃改动时先问用户。

## 硬约束

- `src/anchor/serve.py` 不得在模块级 import pydantic_ai，`tests/test_node_controlflow.py` 会拦。
- 未经真实 provider 端到端跑通的事，不写进文档说已完成。
- 每完成一步，往 `docs/pilot-development-plan.md` 的推进记录追加一条，并同步更新验收矩阵对应项。

## 验证

- 全量：`./.venv/bin/python -m pytest -q -n 8 --dist worksteal`
- 相关子集：`./.venv/bin/python -m pytest tests/test_pilot_turns.py -q`
- 前端：`npm --prefix apps/web test`、`npm --prefix apps/web run test:e2e`、`npm --prefix apps/web run build`
- `--dist worksteal` 必须带：默认的 `load` 会把 `tests/test_examples.py` 的四个慢用例派到同一个 worker，退化到 3 分钟。
- 开发时只跑相关子集，一个阶段做完再跑一次全量，不要每改一点跑一次。

## 框架优先

- 先查本地固定版本 PydanticAI / Harness 的公开接口及仓库已有调用，再做最小接入；不从设计自有实现开始。
- 已有 StepPersistence、FileStepStore / SqliteStepStore、continue_run、inspect_recovery；文件记录使用框架原生 JSONL + 快照等文件，不另写日志/恢复引擎。
- 不新增 Anchor 记忆或计划系统；需要上下文压缩时接现有 compaction。研究目标与证据验收归研究 Graph / Plugin，不列为通用运行时阶段。
- 接口存在、小样例通过、Anchor 接入、真实 provider 端到端通过是不同状态，必须如实记录。

## 架构默认

- 以 [产品与系统架构中的能力归属和执行不变量](docs/product-architecture.md#架构指导新能力放在哪里) 为默认决策规则；开始跨模块改动前，先确定归属层、事实所有者和调用契约。
- 默认采用 Ports and Adapters 隔离 HTTP、CLI、Web、通道和 provider 等外部接口；依赖由适配器指向稳定契约，核心执行不得依赖入口协议或研究/周报等具体业务。
- 用限界上下文的思路划分 Graph/Run、Session/Turn、Library/Plugin、Node Runtime/Sandbox 的事实与不变量。只借鉴边界和所有权，不引入完整 DDD 仪式、微服务或额外框架。
- 保持 Clean Architecture 的依赖方向：易变的外部适配依赖稳定的核心策略；模块只通过小契约交互，不读取或修改其他模块的私有可变状态。
- 保持 Graph + 显式 Plugin 资源 + Runtime 可形成独立执行闭包：完整服务、WebUI、Session、Scheduler 和平台渠道应是可组合的宿主能力。平台与独立部署复用同一 Runner，不复制执行语义；分发包不得包含密钥或擅自扩大宿主授权。
- Rust 是共享 Runtime Kernel 的长期目标语言。平台宿主与独立宿主必须调用同一个 Kernel；迁移按垂直切片逐步替换并保留既有事实/恢复契约，不建两套 Runner。PydanticAI/Harness 仍优先通过窄 Node 执行契约复用；不能因 Kernel 使用 Rust 就默认重写其 Agent 循环或宣称 Python 依赖已消失。
- 小型、可逆且不改变用户语义的实现选择由开发 Agent 根据仓库证据决定。只有决策会改变用户结果、持久事实/迁移、权限边界或并发/恢复语义时才需要用户明确；重要决定记录在对应架构文档/ADR 中。
- 不默认引入微服务、事件溯源、CQRS、通用插件框架或依赖注入容器。每个新增机制必须解决已观察到的问题或明确验收要求。
