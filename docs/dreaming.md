# Dreaming 开发文档：内置 Graph 与宿主自动改进闭环

状态：开发基线，待实施。本文是 Dreaming 的统一实施依据；标为新增的接口、目录、字段和测试都不是现成能力。当前能力见[数据中枢](harness-data-hub.md)，实际实现与验收状态仍归[当前架构](architecture.md)及[统一台账](pilot-development-plan.md)。本次文档确认不等于当前部署的发布、权限变更或真实 Provider 实验授权。

## 使用本文开发

本文覆盖默认交付、Graph、权限、持久事实、实验、自动发布/撤回、自递归、API/UI、兼容与验收。后续开发者不需要重新推导产品形态，但必须核对开工时的源码；本文所述“当前”是成文时的实现，不覆盖其他工作流以后合入的变化。

- **固定契约**：身份由 Host 派生；事实经 submit；Hub 只读；评估独立；发布后核查；无法确认的业务效果不重放。实现不得用方便的旁路取代它们。
- **首版工程选择**：有限内置 Graph、应用内 Dreaming 服务、D0 的调查槽、指导槽及 revision 路径。按本文实现，不预建可替换框架；改变选择时同步修改本文及对应测试。
- **部署参数**：目标白名单、允许模型、质量容忍、统计口径、生产暴露和撤回条件由外部策略绑定。缺少参数是具名不就绪，不是让实现者猜一个生产默认值。
- **状态更新**：本文件不写推进流水。每片通过后把已实现契约迁入对应当前专项文档，本文保留未完成部分并链接过去；全部交付后退出活跃开发入口，避免维护两份当前契约。

| 开发任务 | 本文入口 |
| --- | --- |
| 安装、计划、升级和系统身份 | [默认自带](#默认自带发行初始化启用与升级)、[生命周期接线](#初始化调度与启停的执行协议) |
| Graph、节点、工具和文件协议 | [D0](#出厂-d0-的完整-graph)、[节点交付](#节点交付与原生操作协议) |
| 类型、submit、状态、幂等和并发 | [事实模型](#submit-细腰一种变更资源不造第二套工作流)、[开发数据契约](#开发数据契约与版本) |
| A/B、评分、留出及自动采纳 | [独立实验](#独立实验既测质量也证明改的确实是这一版)、[实验接口](#实验接纳与评估进程契约) |
| Graph/指导/Plugin/常驻助手与撤回 | [发布](#发布生产观察与撤回)、[并发与恢复](#发布并发幂等与故障核查) |
| 改进 Dreaming 自身 | [自递归](#自递归d0-提议-d1但-d0-不能给-d1-改考题) |
| 全局读取、管理与实际开工 | [规模](#全局尺度与读取成本)、[管理面](#管理-api-与用户界面)、[交付](#完整交付顺序与验收) |

## 目标与边界

**Dreaming 是由全局运行事实驱动、带独立验收的离线 Harness 改进；在预授权边界内，裁决通过就自动落地，并用后续生产事实继续判断、改进或撤回。**

用户已确定：

- 从跨 Graph、跨时间的结构化信号发现高影响问题，再按需取证；不让 Agent 逐个阅读所有 Run 历史。
- 复用 `Kernel::submit`，留下提议、实验、裁决、生效与后续结果；人不进入正常的逐提案工作流。
- Dreaming 自身也可以通过同一条路径换代，但不能修改当前候选所受的验收规则。
- 接受一定生产波动以换取进化速度；不以强化学习为前置条件。
- 二进制交付下，自动改进不包含修改 Anchor、Goose 的源码或可执行文件。

采用用户确定的产品形态：**完整平台默认自带 `dreaming` Graph，不要求用户下载示例、手工导入 Graph 或配置源码路径。** 首次启用、默认范围与具体工程拆分按本文作为首版开发选择；它们不改变自动裁决、自动落地和自递归的目标。

由此推出三个边界：

1. **全局发现不等于全局发布。** 发现可以覆盖所有获授权的结构化事实；写入必须绑定准确资产、版本、权限与生产影响范围。
2. **内容无关适用于发现，不足以独立证明质量。** 工具少调用、Run 成功或另一个模型赞同，都不能单独证明任务完成得更好。没有可信质量口径的目标可以调查、实验，但不能因此自动采纳。
3. **固定的是裁决与执行底座，不是改进策略。** 人设置目标、可改范围和风险条件；模型负责找问题、生成候选、安排调查。宿主负责身份、权限、独立验收结果的接入、机械裁决、发布和撤回。

## 现有能力与真正缺口

| 已有部分 | 可以直接复用 | 不能据此宣称已有的能力 |
| --- | --- | --- |
| Kernel、facts、Host 记录器 | 同一种 Record、配置身份、观察、资源版本、effect 生命周期 | `Accepted` 不等于正确，Kernel 不执行资产发布，也不自动核实证据内容 |
| `anchor-hub` | `signals`、按需下钻、配置比较、质量非劣 `gate` | 报告不是发布许可；有界输出不等于物理增量读取或访问隔离 |
| `anchor-eval` | 独立评分身份、评分根与单写入租约 | 当前是 GradeRecorder，不是能独立运行并验真的完整实验评估器 |
| Scheduler、Runner、Goose ACP | 定时接纳、普通 Graph、隔离节点、Artifact、现有模型循环 | 不需要第二个调度器、Agent loop 或恢复引擎 |
| Graph 与 Library 所有者 | 编译校验、发布锁、Plugin 候选暂存、摘要及安装前置检查 | 没有 Dreaming 机器授权入口；Plugin 没有现成的多版本灰度与可靠撤回闭包 |

实现核对点：[`Kernel::submit`](../rust/anchor-kernel/src/kernel.rs)、[`JudgeCtx`](../rust/anchor-kernel/src/model.rs)、[Graph 发布](../rust/anchor-runner-host/src/application/graphs/updates.rs)、[Plugin 安装](../rust/anchor-runner-host/src/application/plugins.rs)、[评分记录器](../rust/anchor-eval/src/lib.rs)、[Hub gate](../rust/anchor-hub/src/analyzers/gate.rs)。

**主要缺口不是再加一个会总结的 Agent，而是把“可信实验结果 → 受控发布 → 实际生效回执 → 生产撤回”接成宿主拥有的写路径。**

## 总体形态

```text
已有 Scheduler
    ↓ 接纳本轮固定版本 D0
普通有限 Dreaming Graph（已有 Runner / Goose）
    ├─ 只读 Hub：全局信号 → 选重点 → 按需取证
    ├─ 隔离候选：生成可验证的资产差异
    └─ 请求实验、提交提议
             ↓
Host Dreaming 应用服务
    ├─ 普通实验 Run → 受保护评估进程 → 独立评分与绑定回执
    ├─ Kernel::submit → 确定性 Validator → Execute 许可
    └─ 现有 Graph / Library 所有者 → 实际生效观察
             ↓
Host 生产观察与撤回 → submit 结果 → 下一轮 Hub / Dreaming
```

在 `anchor-runner-host::application` 下增加 `dreaming` 应用边界，不增加独立控制服务。它不是通用管理权限代理：Dreaming Graph 只拿读取、候选准备、请求实验和提交提议的窄工具，不能直接写正式 Graph、Library、评分根或裁决配置。发布由宿主执行已裁决的 intent，不依赖提议 Agent 再调用一次 shell。

日常编排使用普通有限 Graph，不先做常驻自改循环。复用现有 Scheduler 的 occurrence/request 身份与接纳语义；同一 Dreaming 尚未结束时不重叠执行。当前 Scheduler 会保留并追赶未接纳时点，本方案不暗改为跳过；窗口没有新增有效事实或待处理结果时，Graph 在确定性入口直接结束，不重复启动模型调查。触发时冻结本轮 Graph、策略及事实窗口坐标，历史不完整则明确保留缺口。

Hub 保持独立只读程序，Host 不链接 `anchor-hub`。未安装 Hub 或评估器时，普通业务仍执行、仍记录事实，只是不能启动或推进对应的 Dreaming 自动改进。

## 默认自带：发行、初始化、启用与升级

### 出厂内容与来源

采用“发行物中的不可变 D0 + catalog 中的可进化实例”，不是在 Rust 中写死整个 Dreaming 流程，也不是只能复制一次的示例。

| 随完整平台交付的内容 | 所有者与用途 |
| --- | --- |
| `graphs/dreaming/graph.json` 与 Graph bundle manifest | 出厂 D0；含编排、指令及 layout，使用普通 Graph 作者契约 |
| `bin/anchor-hub`、`bin/anchor-eval` | 固定版本的结构化分析、独立检查与判定执行；不要求开发机源码或 Python |
| `harness/input-discovery.md` | 首个可进化建议性指导槽的出厂数据；替代现有 Rust 字符串，权限规则不放入其中 |
| `evaluators/dreaming/`、`evaluators/input-discovery/` | 内置评估包、任务/检查清单及兼容版本；受保护安装，不进入 Agent 的目录挂载 |
| 扩展的 runtime inventory | 绑定 Graph、指导数据、评估包、工具版本及全部文件摘要；只证明出厂字节，不代表当前资产版本或实验已通过 |

Graph 的仓库权威源放 `resources/graphs/dreaming/`，不再在 `examples` 复制一份。指令仍属于 Graph/普通技能资产，可用确定性构建将同目录的指令文件装配进 JSON，但交付只使用普通 bundle。系统工具由 Host 注入，不为开启 Dreaming 新增一个拥有全局管理权的 Plugin。

`PlatformPackageInventory` 增加明确的 Graph、指导与评估包清单，平台格式从 3 升至 4；修改 `anchor-distribution` 的构建、校验、归档及原生安装闭包，并把 Hub/Eval 纳入 release 构建。独立 Graph 包不被强制安装整个 Dreaming。兼容旧平台 inventory 的读取若为本机迁移所需，只保留到该安装迁移并核对完成，不长期维护两个平台格式。

### Host 初始化与默认行为

1. 组合根先校验发行资源，初始化既有 Library 和 Graph 服务，再经统一 Graph 发布用例把缺失的 D0 安装到 `catalog/dreaming`；安装 intent 与结果通过 submit 记录。没有模型时也能完成资产的静态安装，模型就绪属于运行前检查，不把它混进 bundle 的 schema 校验。
2. 宿主记录该 Graph 来源身份和出厂 seed 身份，建立 Dreaming 控制资源；不能靠 Graph 名叫 `dreaming`、JSON 自报系统身份或公开 trigger 参数取得工具权限。已有同名非受管资产返回可诊断的冲突，不覆盖、不自动改名并授予权限。
3. 默认提供唯一的每日计划，首版预置 Host 本地时间 `03:00`，用现有 keyed schedule create 幂等创建并保存 schedule 引用；首次启用前保持停用。时间规则只归 Scheduler，不在 Dreaming 配置再存一份。
4. 默认安装不等于静默消耗模型凭据和修改用户资产。新安装先显示 `not_configured`；首次模型设置或 Dreaming 设置中一次启用并确认目标范围，之后自动执行。已有安装升级不自动扩大授权。用户不必为每条提议批准。
5. 一次启用可采用内置预设：允许改进 Dreaming 自身及明确列出的建议性指导槽，只使用既有模型/工具权限及配套受保护评估包。普通用户 Graph、Plugin 和全局指导推广均须在目标范围中明确加入；空授权不解释成全目录。
6. 停用先经 submit 撤销未来实验/发布授权，再停计划。已生效版本的生产观察、未知效果对账和已授权撤回仍保留；不能因停用 Dreaming 而让坏版本失去恢复路径。缺模型、缺评估包或缺权限分别显示原因，不让整个 Host 启动失败。

手动“现在做梦”也接纳同一个普通 Graph。只有可信管理入口、受管计划或已授权自测入口能由 Host 赋予周期上下文；直接复制 Graph 或把它作为普通任务运行，不继承特权。中断接纳重用原 request 与冻结快照，不能在重试时悄悄换成当前新版。

### 出厂与演进版本不互相覆盖

重启只补齐确实缺失的未初始化资源，不把 catalog 中的 D1、D2 恢复成 D0；目录损坏与“尚未初始化”分开。正式停用/移除后的记录作为持久意图保留，后续启动不凭文件缺失把它复活。

产品升级保留演进实例，把新出厂 Graph 当作新的 seed，而不是强行覆盖。运行前核对 native operation、工具和评估 schema 的兼容版本：兼容则继续；不兼容则停止新的 Dreaming 执行，按已有预授权恢复策略验证并切换新 seed，或明确显示 `incompatible`。新增权限必须从外部授权，不能借升级自动取得。

用户仍可在普通 Graph 画布查看、编辑或复制 Dreaming。外部手改产生的未知版本不能自动继承旧的机器发布证明，需通过一次外部重绑定或同一受保护验证流程加入可信谱系。当前版本始终读取 catalog，控制资源只保存来源/授权和可验证谱系，不维护另一个“当前 Graph”指针。

### 初始化、调度与启停的执行协议

初始化复用 Graph 的静态编译、资源闭包校验和 bundle writer，拆开当前 `inspect_graph_definition` 混合执行的静态检查与模型就绪检查；普通保存/接纳仍保留完整校验，不能为了 seed 安装全局跳过模型引用检查。初始化期间不发起 Provider 请求。

| 场景 | 严格执行顺序与恢复 |
| --- | --- |
| 首次安装 | 保存 seed 安装 intent → 幂等写 bundle → 核对字节 → 绑定来源 → keyed 创建停用计划 → 保存 schedule 引用；中断后核查并继续，不盲覆盖已存在目录 |
| 首次启用 | 外部提交完整策略 → 保存启用 intent → 启用已绑定计划 → 记录观察；中途失败保持明确的不就绪，可按同请求重试 |
| 定时执行 | ticker 核对 control 中的确切 schedule ID、owner、受管 Graph 来源与当前策略，转入 Dreaming 接纳；不能仅凭 `graph == "dreaming"` 或 schedule input 判断 |
| 手动执行 | 可信管理用例派生主周期上下文；与定时执行共用同一控制级单活动周期守卫，不另开第二个 Dreaming |
| 停用 | 先持久撤销新实验/应用能力，再停计划并观察结果；即使停计划失败，新 occurrence 也不能获得权限。已接纳 Run 可结束只读整理，不能继续创建实验或发布 |
| 计划被外部修改/删除 | 由 schedule 权威判断真实状态；受管引用失配显示 `schedule_conflict`，不把其他同名计划接管为系统计划，也不自动补建已删除计划 |
| 外部恢复/重新绑定 | 核对原 control 版本、资产摘要及兼容性，记录明确管理动作；重新建计划使用新的安装代次身份，不复用已删除计划的创建 key |

Scheduler 已有 occurrence request 继续作为接纳身份；不改变其待接纳时点追赶语义。计划接纳回执与 cycle 绑定同一 request。只有元数据而没有 Run 时属于中断接纳，不能推进计划或事实窗口。默认计划的规则编辑复用现有计划 API；更换来源/owner/Graph 会使受管绑定失效，须经外部重绑定。

系统初始化使用固定 seed 来源能力，不能调用假装用户操作的 Operator 入口。主周期的授权上下文必须在 `PreparedExecution` 校验时已经可见：由 run_access 传入同一份已验证的接纳上下文，随后按已有中断接纳协议保存；恢复从持久元数据与 control 交叉核验。既改执行前 host-operation 检查，也改节点分派，不能伪造 assistant metadata 绕过前者。

### 发行清单 v4 的最小新增字段

保留既有 `files`、`executables`、Plugin 与运行时身份。下列清单条目只引用同一份文件表，不再复制字节清单；摘要沿用现有规范化及路径校验原语：

| 新字段 | 条目必填字段 | 约束 |
| --- | --- | --- |
| `graphs` | `id`、`bundle_path`、`graph_digest`、`bundle_manifest_sha256`、`protocol` | `dreaming` 恰有一个；bundle 仍只含标准 `graph.json` / `manifest.json`，指令在构建时装配 |
| `guidance` | `slot`、`path`、`sha256`、`schema_version` | 首版只有具名 `input-discovery`，不能用路径模式扩展可改槽 |
| `evaluators` | `id`、`package_path`、`manifest_sha256`、`protocol_version`、`suite_version` | 包内任务/检查文件也必须在既有 `files` 中；不含 Provider 凭据 |

`graphs[].protocol` 绑定 Graph 实际使用的 host-operation、Dreaming 工具和评估协议版本。Hub/Eval 属于既有 `executables` 表，不另造 tools 安装注册表。修改 `PlatformPackageRequest` 的显式构建输入，并同时升级 builder、归档校验、`validate_runtime` 格式/路径白名单、安装后校验、原生 DEB/RPM 闭包及 Host seed loader；只改打包器不足以交付。缺必选文件、重复身份、摘要不符、未知协议必须在安装或就绪检查具名失败。

## 出厂 D0 的完整 Graph

```text
open（Host：窗口、结构化信号、未结变更）
  → triage（Agent：选题、影响、可证伪假设）
  → scope（Host：绑定调查范围）
  → fanout
      ├─ investigate-1 ─┐
      ├─ investigate-2 ─┤
      ├─ investigate-3 ─┤→ join
      ├─ investigate-4 ─┤
      └─ explore ───────┘
  → compose（Agent：候选、预测、实验建议）
  → review（独立 Agent：反例与证据质疑）
  → process（Host：提议 → 实验 → 独立裁决 → 自动应用）
  → close（Host：回执与窗口结算）
```

四个调查槽和一个探索槽复用 `rsi-signals` 的有效形态；这是 D0 可被后续候选改变的编排，不是 Host 硬写的调用预算。共享调查角色，通过普通节点 `with` 绑定槽位，避免复制四份指令。无题可查的槽位只产出明确的 `skipped`；按当前固定并行契约，这仍可能需要一次模型回答，必须计入 D0 成本，不虚报为零。全周期无工作则在 open 直接结束；不为省空槽调用先改并行引擎。explore 读取正向范例和稳定抽取的普通样本，避免只追已有失败分类。

| 节点 | 类型、输入与输出 | 重要执行规则 |
| --- | --- | --- |
| `open` | `op.host dreaming.open`；产出 `cycle.json`、有界 `signals.json`、coverage、历史变更摘要 | 冻结来源水位与策略引用；新增有效工作为空则直接路由到 close；不把原始目录交给模型 |
| `triage` | Agent；产出 `agenda.json`，含信号 ID、假设、影响依据和不选原因 | 看全局摘要，不逐个遍历 Run；不能选择政策外的写目标 |
| `scope` | `op.host dreaming.scope`；产出按槽位的任务与证据访问句柄 | 核实信号来自本窗口，句柄绑定 invocation/事实范围；无效项逐条返回，不扩大读取范围 |
| `investigate-*`、`explore` | Agent；产出 `findings.json`，区分 observed/inferred/unknown 与可复跑核对 | 只用获准的只读接口及候选现场；可以问模型原因，不能把解释当作验收 |
| `compose` | Agent；产出 `proposals.json` 和不可变候选 Artifact | 列准确 base、候选字节、预期改善、反例、所需实验；目标与评分规则不能由候选改写 |
| `review` | 独立 Agent；只读候选及 findings，产出 `review.json` | 只提供批评和实验建议，无写候选权、评分写权或最终采纳权 |
| `process` | `op.host dreaming.process`；读取固定 Artifact 引用，返回每项变更/实验/判定回执 | 逐项经同一服务提交，不整批以一个 PASS 放行；不在持有资产锁时等待模型或实验 |
| `close` | `op.host dreaming.close`；产出 `dreaming-report.md/json`、结果链接、未决项 | 报告只投影真实历史，不能自行把实验通过改成生产完成；自升级候选由 Host 在本 Run 结束后激活 |

需要补证或修订时，process 返回逐项原因，沿普通 Graph 反馈边回到 compose 或 triage；`inconclusive` 留待更多合法样本或后续周期，不能伪装成成功，也不靠固定轮数强行收敛。D1 可以改变这套编排，但删除 review、process 或 close 都不能绕过 Host 对每次工具调用、裁决和生效的独立校验。

### Native operation 与 Agent 工具

当前 `op.host` 的实际执行先要求 Session 助手绑定，不能直接借来执行 Dreaming。在 `node_host` 的中立契约增加小的 `HostOperationPort`，由组合根注入 Dreaming 适配器；原 Session 操作保持原处理路径，不能借此增加下层调用 application 的引用。普通 Runtime 只承载 `operation` 与结构化参数，不包含 Dreaming 业务规则。

Agent 侧沿用 `NodeToolAssembly` 装配，注入下列窄工具；不新增通用 MCP 写服务：

| 工具 | 最小参数与返回 | 宿主校验 |
| --- | --- | --- |
| `dreaming_signals` | 本周期内的筛选/分页 → 信号与覆盖 | 窗口、来源与安全字段白名单；不能传任意数据根 |
| `dreaming_evidence` | scope 句柄、事实键、所需字段 → 有界证据/缺口 | 原生 invocation、范围、明细授权；不能伪造路径或读取别的调查句柄 |
| `dreaming_target` | 获准目标 ID、base 摘要 → 可编辑内容、只读边界及依赖身份 | 只返回可改资产内容，不输出凭据、留出包或整个源仓库 |
| `dreaming_candidate_prepare` | 目标、base、相对文件选集 → 暂存候选摘要和诊断 | 宿主冻结字节；复用候选准备、标识校验和路径检查，不安装到正式目录 |
| `dreaming_candidate_test` | 候选引用、开发检查 ID → 开发测试事实 | 只跑获准开发测试，不能指定任意宿主命令，不能写权威 Grade |
| `dreaming_propose` | 候选引用、信号/核对引用、预测与实验建议、请求 ID → change 引用 | 与 process 使用同一提议用例；同键不同内容冲突，不允许提交可信回执槽位 |
| `dreaming_change` | 本作用域 change 引用 → 实验进度、判定与生效观察 | 不按模型提供的 owner/Run 扩大范围；不会返回留出答案 |

工具不接受 caller 自报 cycle、owner、权限或已批准状态。Host 从冻结的 `DreamingRunContext` 与原生 invocation 派生身份。候选修改任意 node/agent 名字也不能给自己增加评分、发布或策略编辑权限；普通复制 Graph 没有上下文时，工具既不列出也不能直接调用。

### 节点交付与原生操作协议

所有新增的 JSON 交付包含 `schema_version: 1`；对象字段使用 snake_case，未知字段在可信输入边界拒绝。Graph 沿用 `writes`、`final_result.files`、`CommitRef` 和 Artifact 规则，不引入另一套文件交接。文件名是节点内路径，多个调查节点各自交付 `findings.json` 不发生覆盖。

| 文件 | 必需业务字段 | 不允许作为事实的内容 |
| --- | --- | --- |
| `cycle.json` / `signals.json` | cycle 引用、窗口引用、策略引用、coverage；signal ID、类型/版本、分组键、样本/消耗/影响统计、事实引用和缺口 | 任意宿主根路径、完整原始对话、模型自报成本 |
| `agenda.json` | `items[]` 中的本窗口 signal ID、target ID 或待查目标、假设、预测指标、影响依据、反证、`slot`；`unselected[]` 的信号及原因 | 自报授权、最终根因或已批准结论 |
| `scope.json` | 每槽 `selected/skipped/invalid`、任务及 Host 签发的不透明证据句柄 | 模型拼接的文件系统访问权 |
| `findings.json` | signal/target 引用、`observed/inferred/unknown`、精确证据引用、复跑核对、候选建议或无改动理由 | 仅靠口头解释成立的质量结论 |
| `proposals.json` | `items[]` 中的候选引用、假设、预测、证据、开发检查结果和实验建议；允许空数组 | Grade、采纳证明、策略覆盖字段 |
| `review.json` | 对准确候选引用的反例、缺口、追加检查建议 | 最终 PASS、发布授权 |
| `process.json` | change 引用、已记录实验/决策引用、每项真实状态、退回原因、`next` | 将接纳当实验完成，将许可当已生效 |
| `dreaming-report.json/md` | 本周期覆盖、结果引用、预测与实测、总消耗、未知/未决项；Markdown 是同一结构投影 | 独立于历史的成功状态 |

`op.host` 参数只保留固定 operation 与协议版本；输入从本 invocation 的 `input_commits` 解析，不接受任意 Run 的“最新文件”。下游文件引用为 `{commit: CommitRef, path: relative_path}`，Host 核对 commit 属于已授权交接，解析既有 Artifact 身份并检查 schema。

```json
{
  "ops": {
    "open": {"host": {"operation": "dreaming.open", "schema_version": 1}},
    "scope": {"host": {"operation": "dreaming.scope", "schema_version": 1}},
    "process": {"host": {"operation": "dreaming.process", "schema_version": 1}},
    "close": {"host": {"operation": "dreaming.close", "schema_version": 1}}
  }
}
```

这是待实现的 Op 片段，不是完整可运行 Graph。完整 D0 在交付切片中生成并经普通编译器验收。原生端口复用 `NodeExecutionRequest` 的 key、输入提交、取消和 `NodeExecutionOutcome`；Host 完成节点文件落盘后返回 `Completed(NodeCompletion)`，由 Runner 沿现有 ArtifactPort 冻结/发布节点交付并提交结果，不让原生端口再发布一遍。候选与实验清单等独立业务 Artifact 仍在相应用例中冻结。Op 自身 `model_requests=0`，实验模型消耗记在实验 Run，不重复记到 Op。

业务分支用 `output.next` 枚举表达：open 的 `work` → triage、`no_work` → close；process 的 `revise` → compose、`retriage` → triage，其余已处理/无候选/证据不足 → close。`NodeCompletion.route` 必须写对应的直接后继节点 ID，即 `triage/close/compose`，不能把 `work/revise` 当 route，也不新增命名边机制。不用抛异常表示正常退回。process 可以返回每项不同结果，整体路由只决定是否确有新工作可交给模型；纯等待实验不绕回模型轮询。

process 可等待普通实验和独立评估，但不持有资产锁、Kernel 锁或策略提交 gate。实验/变更由 Host 拥有，即使父 Run 中断也不丢失。取消只结束当前等待并停止尚未发生的工作；已接纳实验按原 Run 核查，已生效版本继续观察。重试相同 invocation 复用原提议和实验，不能再建一批。

窄工具的 schema 复用下文 `TargetRef`、`CandidateRef`、`FactRef` 和错误类型。`candidate_prepare` 的路径只相对于本 invocation Workspace，符号链接、越界、mutable 与未选文件沿现有选集规则处理；准备成功返回的是不可变候选身份，之后工作区继续修改不改变它。所有返回的证据/候选/报告仍按 Artifact 投影读取。

缺必交文件、非法 schema、未知 operation/版本属于具名失败；中断后先查同 invocation 的完成事实和已发布 Artifact，再决定能否恢复。不能让 `close` 成为唯一窗口保存者，也不能把关闭失败解释成整轮什么都没做。

## 可进化层与固定层

| 范围 | 允许的方向 | 必须满足的边界 |
| --- | --- | --- |
| 普通 Graph 指令、技能说明 | 改写指导、拆分/合并步骤、复用有效范例 | 准确目标与 base/candidate 摘要；不得改变用户目标或扩大权限 |
| Graph 结构与模型选择 | 改编排、在已授权模型集合内选择 | 对应输入输出、资源闭包和质量回归通过；常驻实例另有生命周期条件 |
| 普通 Plugin | 说明、代码及已授权依赖范围内的候选 | 不含系统 Plugin；无新增凭据、来源授权或可变数据迁移；先具备版本与撤回闭包 |
| 宿主建议性指导 | 将已证明需要迭代的建议性片段提取为版本化数据后改进 | 明确可改字段；删除被替代的 Rust 文案，不保留互相覆盖的两套指令 |
| Dreaming 自身 | Graph、指令、技能、信号选择和实验策略 | 与普通候选相同的独立验收、发布和撤回路径 |
| 固定底座 | 不在自动改进范围内 | Anchor/Goose 二进制、Kernel 规则、宿主权限、用户目标、评分器、受保护留出集、当前采纳规则 |

初次实现 Dreaming 必须发布一个支持上述边界的新二进制；这不同于允许 Dreaming 修改二进制。并非所有现有 Harness 行为都能立刻自改。例如当前 `graph/logic.rs` 的输入目录建议仍是 Rust 字符串，不能把改源码伪装成更新 Graph。先从已有可编辑资产验证闭环，再按证据抽出真正需要迭代的指导片段，不预建任意 system prompt 覆盖平台。

现有 Agent 自身来源保护、系统 Plugin 保护和 Validator 的人类授权规则不放宽。专用机器发布入口消费宿主验证过的授权凭据，不冒充 `Operator` 或 `Human`，也不把模型给出的 `approved: true` 当作授权。

## submit 细腰：一种变更资源，不造第二套工作流

### 事实所有者

由 Host 应用服务独占一个 `kernel-dreaming` 数据根，沿用 `FileStore`、Record 与单写入租约。它拥有有限的预授权策略与每项 `change/<id>` 的历史，不能借用事实记录器、Run 或评分器的写身份。增加这个根是为了拥有可靠的自动发布 intent 与回执，替代报告之外的手工确认和 shell 发布，不另加数据库或通用事件平台。

策略的建立、更新与撤销只由可信外部管理入口经 submit 记录，Dreaming 工具只有读取权。变更资源绑定策略快照供纯裁决使用，实际应用仍核对当前授权，不让快照成为另一份长期有效的授权来源。

每个变更只保存以下必需内容及不可变引用：

- 信号/事实位置、假设、目标身份、base 与 candidate 的准确摘要、可审查差异。
- 外部授予的策略版本与授权范围：允许的目标/字段、模型和依赖来源、业务效果范围、验收版本、上线观察及撤回条件。
- 实验清单、普通实验 Run、独立评分、受验证评估回执的引用。
- 应用/撤回 intent、实际版本观察、生产暴露与结果；未知值保留为未知。

候选正文与实验清单可用既有 Artifact 保存，不给每个中间概念另建一套资产注册表。Graph/Library 仍是已安装资产的权威；变更历史证明为什么尝试发布及观察到了什么，不维护第二份“当前生效版本”。实验执行进度仍归 Run，不复制到变更账本。

### 持久对象与类型契约

| 对象 | 权威位置与关键字段 | 不拥有的事实 |
| --- | --- | --- |
| `control/dreaming` | `kernel-dreaming`；受管 Graph 来源、seed/兼容版本、外部授权策略、schedule 引用、保留/移除意图 | 不复制 Scheduler 时间规则或 catalog 当前定义 |
| `cycle/<id>` | 同根；触发 request、绑定 Run、冻结策略引用、输入窗口及来源覆盖、候选/结果引用、窗口处理回执 | 不保存 Run 的 executing/done/worker 状态 |
| `change/<id>` | 同根；`TargetRef`、`CandidateRef`、预测、核对、实验与验收引用、应用及生产观察 | 不复制完整 Graph、Plugin 或实验 Run 进度 |
| `ExperimentManifest` | 受保留的不可变 Artifact；精确处理臂、任务版本、输入摘要、配对/重复身份、执行配置、评分器与留出批次 | 不接受 Agent 自报完成数，不替代 Run/Grade |
| `Grade` 与评估回执 | 评分根；检查结果、准确 Run/任务/评分配置；回执补实验绑定、覆盖与判定 | 不由 candidate_test 或 review 节点写入 |
| `TargetRef` / `CandidateRef` | 宿主类型：目标类别、稳定身份、基线版本；Artifact 选集、规范摘要、资源/依赖/授权闭包摘要 | 不接受任意宿主绝对路径或只有可变文件名的候选 |

`cycle/<id>` 只新增此前缺失的跨来源窗口事实，必要性是重试不能变更“这轮究竟分析了什么”。ID 由可信 request 身份和已有摘要原语派生；同 request 不同内容冲突。原生节点的 invocation 用于候选/实验请求幂等，不能由模型选择新的 owner 绕过去。

Run 元数据增加可选的 `DreamingRunContext`，区分主周期、开发实验、留出验证与自递归评测，绑定 cycle/change/experiment/assignment 引用；旧元数据无此字段即无该身份。该上下文由接纳门面写入，普通 `/trigger` 请求不能提交。结构事实同步记录这些关联，防止把实验 Run 当作生产改善样本，或把 Dreaming 自己产生的新日志当成必须再次做梦的业务变化。

预授权策略使用类型化字段，而非自然语言“可以优化整个系统”：

- `targets`：稳定 Graph/Plugin/指导槽身份、可改字段、允许的结构与模型集合；影响用户目标、凭据/网络/宿主路径、外部动作授权的字段一律不由 Dreaming 扩大。
- `evidence`：已注册来源身份、结构化读取范围、允许下钻的明细类别；不把本机文件路径当权限。
- `evaluation`：任务包、固定评分器与留出版本、质量容忍、改进指标/最小效果、统计规则及样本不足处理。
- `production`：允许的目标范围、暴露选择、观测充分条件、严重失败条件、停止扩展与撤回规则、可用恢复版本。
- `recovery`：对已接受执行、回执缺失、版本冲突、撤销授权和自身失败的处理；不授权重放未知业务效果。

模型只能建议实验输入与策略改进，Host 从当前策略和受保护任务包生成最终清单。默认没有任意任务请求/token/费用上限；样本、实验终止及生产暴露条件按显式政策定义，不能偷偷变成新的全局预算。

### 从提议到实际效果

| 步骤 | 提交者与动作 | 裁决/执行要求 |
| --- | --- | --- |
| 提议 | Agent `Request` | Validator 检查目标范围和候选形状；受理提议不是允许发布 |
| 取证与实验 | Host 接纳普通 Run；评估方写独立 Grade | 待实验、样本不足是领域状态/开放槽位，不使用表示等待人的 `Effect::Pending` |
| 接入可信证据 | Host `Write` 受验证回执 | 核对来源、实验全量清单、版本与评分；Agent 不能填写这个槽位 |
| 发布裁决 | Host `Execute` | 纯 Validator 检查本资源内的可信回执与策略快照；拒绝不满足条件的候选 |
| 资产生效 | 现有事实所有者执行 IO | 在同一目标锁与前置条件下重新确认策略仍有效、base 未变，再发布 |
| 观察与后续 | Host `Write` 观察，必要时新的 `Execute` 撤回 | 记录实际版本与生产结果；撤回也是新事实，不改写旧裁决 |

必须补齐两处边界，不能只把一个 `evidence` 数组塞给 Kernel：

- 当前 `JudgeCtx` 只拿本资源和 principal，不能跨根做 IO；`Evidence` 也没有通用的数据根身份。由 Host 从固定注册的只读来源解析“来源身份 + resource + history place/version + 内容摘要”，核实后将小型可信回执落入本变更资源。禁止接受模型提供的任意本机路径或自制 gate JSON。
- 已接纳的冻结策略不等于永远有效。最终写入时必须在发布边界重新核对当前策略/撤销版本，和 base 的 CAS 一起受锁保护。提议后策略撤销、Graph 被人工编辑或 Plugin 已更新，都返回冲突，不能沿用旧批准。

Kernel 仍只执行通用提交、版本、身份与 effect 规则；Dreaming 的质量、收益和范围语义属于注册的 Validator 与宿主应用服务，不进入 Kernel 核心。

变更状态由已接受的回执投影为 `proposed → testing → qualified → applying → observing → retained/withdrawn`。`returned/rejected/inconclusive/conflict/unknown` 保留具体原因；不创建并行审批任务或用 `Pending` 代替机器等待。候选内容或 base 改变就是新候选，旧实验不能继续为它背书。发布后允许持续追加生产结果，不用 Task 的 `Settle` 把后续撤回锁死。

全周期和每项变更都可部分成功：一项候选不合格不撤销另一项已经观察到的改善；跨 Graph 推广拆成准确目标各自的 Execute 与回执，不承诺整批原子发布。没有合格候选时合法结束为 `no_change`，不能为了交差强制发布。

### 不伪装跨存储事务

接受 `Execute` 的历史是 intent，随后由 Kernel 创建 effect；两者当前也不是原子事务。Host 确认原 effect 存在后，在 IO 前记录 `started`，再进入已有资产所有者的写路径，依据实际摘要/版本观察记为 `succeeded/failed/unknown`。Graph 文件、Library 和 Kernel 历史之间同样没有原子事务：

- 崩溃发生在任意边界时，Host 启动后核查现场；符合 intent 的结果补观察，不凭“请求曾获准”重复执行。
- 现场处于第三个版本或效果不明时，保留 `unknown/conflict` 并停止该变更的继续写入，不覆盖后续人工修改。
- Kernel 已有 unknown-effect 阻挡不替代应用层的单活动 intent 约束；同一目标不允许两个发布者同时认为自己拥有部署权。
- 发布前 intent 或 started 不能持久化时，不执行 IO；发布后回执写入失败则保持待核查，不能假装未发生或盲目再做一次。普通模型用量记录的尽力而为策略不适用于自动改资产。

## 开发数据契约与版本

以下为首版必需字段；引用既有类型的地方直接复用，不另造近似结构。内部结构用 Rust 类型，持久 content/工具/API 边界用显式带版本 DTO；不要把 UI 投影反序列化成可信命令。

### 身份、引用与摘要

| 类型 | 首版字段及语义 |
| --- | --- |
| `PolicyRef` | `resource`、`version`、`digest`；指向 control 中准确的外部策略版本和规范化内容 |
| `FactRef` | `source_instance`、`resource`、`place`、`record_digest`；`place` 为历史中从 1 开始的位置，含拒绝/退回记录，不是 resource.version |
| `GradeRef` | `fact: FactRef`、`grader_config`；Host 校验来源属于受保护评分根、记录确为 accepted Grade 且绑定目标 Run |
| `ArtifactSelectionRef` | 既有 Artifact ID、相对文件选集及选集摘要；不能只有可变的 Workspace 路径 |
| `TargetRef` | tagged union：`graph {id}`、`guidance {slot, scope}`、`plugin {id, activation}`；scope 为准确 Graph 集或显式 global；activation 为准确 Graph pin 范围或 default |
| `BaseRef` | 目标权威的版本/摘要、授权摘要、执行闭包摘要和可恢复内容引用；没有基线不能自动发布 |
| `CandidateRef` | target、base、`ArtifactSelectionRef`、候选内容摘要、执行闭包摘要；由候选准备入口生成，工具不能自行填写为可信 |
| `DreamingRunContext` | `schema_version`、`kind: cycle/dev/holdout/meta`、`cycle`、策略引用、受管来源证明；实验类另有 change/experiment/assignment，meta 绑定隔离实例身份 |
| `WindowManifest` | 每个 source/resource 的实例身份、`after` 与 `through`、完整度/缺口；周期冻结的上界、抽样规则与清单摘要 |

`source_instance` 是 Host 注册的来源实例身份，不是目录显示名；重绑定到另一个根不得继续使用旧身份。引用解析由 Host 的注册来源适配器完成，不向模型暴露根路径。`record_digest` 使用既有规范化 JSON/摘要原语对该完整 Record 计算，任何来源/位置/摘要不匹配都不采信。物理文件游标只是可重建读取优化，不替代 `FactRef`。

目标 ID 校验、摘要、路径选集与规范化复用已有实现。JSON 对象规范化后取摘要，原文件字节身份沿既有 Artifact/Catalog 规则；两者不能混称同一个 digest。候选闭包包含实际 Graph/Plugin revision/依赖视图/模型绑定/指导/工具身份，不含凭据值。

所有新业务 content 使用 `schema_version: 1`。同 schema 新增可选字段必须证明不改变旧摘要；权限或语义新增字段不能以“忽略未知字段”兼容。旧 `PluginRef` 没有 revision 时仍按原规则输出 bare string 或原对象；`None` 不输出新字段，不改变已有 GraphSnapshot 摘要。

`RunMetadata` 新增 `dreaming: Option<DreamingRunContext>`，用 default 和 skip-none，首版保留 metadata format 2 的旧读取行为，不把它与 GraphRunRecord format 15 混为一谈。旧记录无字段即无能力。当前二进制可读取旧 metadata；更早二进制可能因 deny-unknown 无法读新增记录，因此产品二进制降级不是 Dreaming 撤回，禁止自动替换旧二进制来恢复。

### 策略与候选的结构

`DreamingPolicyV1` 包含 `enabled`、`targets`、`evidence`、`evaluation`、`production`、`recovery`；每个可改目标都必须有适用的评估与恢复配置，不能靠空列表或通配路径表示“全部允许”。控制资源另存 `removed` 等外部意图，不从 Graph 文件是否存在推断。

| 策略域 | 实现必须检查的字段 |
| --- | --- |
| targets | 准确 TargetRef、可改面枚举、允许模型绑定、依赖来源、生产影响范围。Graph 可改面为 instructions/models/topology；指导仅 content；普通 Plugin 为 docs/code/既有授权来源内的依赖 |
| evidence | 已注册 source_instance、结构字段、明细类别与目标 scope；过滤和分页不是授权的替代品 |
| evaluation | suite/evaluator/任务与留出版本、必查任务/检查项、质量容忍、声明的收益指标及方向/最小效果、统计方法/参数、缺样本与失败执行规则 |
| production | 允许的具名暴露范围、分配规则及对照、结果延迟/充分条件、严重失败、停止扩展和撤回条件 |
| recovery | 允许恢复的准确前版/seed、兼容要求、停用/撤销后的观察与补偿权限、未知效果处置 |

用户目标、输入输出契约、工具权限、凭据/网络/宿主路径、评分和采纳规则不属于可改面。新增 topology 可以改变节点，但不能凭节点名称复制权限；授权所有者按新闭包逐项重绑定原授权与策略的交集。目标闭包不能满足任务接口或独立质量验证时不发布。

`ProposalV1` 只含：`candidate`、`signal_refs`、`evidence_refs`、`hypothesis`、`prediction`、`counterexamples`、`development_check_refs`、`experiment_suggestion`。预测绑定指标 ID、方向、目标任务范围和可证伪条件；不接受 `approved`、任意命令、Grade 或裁决字段。Host 追加 owner、原生 invocation、策略及时间。

### 提交者与写入槽

| 提交者 | 可用动作/资源 | 必须拒绝 |
| --- | --- | --- |
| Dreaming Agent | 对其 scope 内 change 的 `Request/propose`；只读工具 | Write/Execute、策略、可信回执、Grade、正式资产 |
| `anchor/dreaming` Code | control/cycle/change 的受限观察、可信回执与 Execute；本根具名 guidance 的受验证激活；自己 endpoint 的 effect 生命周期 | 任意其他根写权、Agent 参数自报的机器身份 |
| 独立 evaluator Code | 原评分根的 Grade 与绑定评估回执 | 改候选、策略或正式资产 |
| 可信外部管理者 | 经现有管理身份建立/撤销策略、主动停用/恢复 | 通过 Graph JSON 或普通 Agent 工具模拟此身份 |

Authority 按 principal 的 id/kind 和准确资源规则判断，不把 config digest 当授权，不给所有 Code 通配写权。guidance 写只接受已验证 Execute 绑定的 slot/scope、内容引用和 CAS，经相同策略 gate 应用/撤回；Agent 无权直接写激活选择。注册固定 Validator 属于 Host 组合，不允许候选更新 validator 资源。策略管理的 Pilot 适配如有接入，必须走现有原生用户授权事实；模型复述“用户同意”无效，Dreaming 本身不暴露该管理工具。

### 状态转换与不变量

主状态是历史投影，不是另一份可直接覆盖的 status 文件；异常原因、实验状态和生产观察作为具名结果保留。

| 起点 | 事实与前提 | 后续投影 |
| --- | --- | --- |
| 未建变更 | 合法候选 Request 被受理 | proposed |
| proposed | 实验清单已冻结，assignment 开始接纳 | testing |
| testing | 可信回执证明范围、质量与收益全部合格 | qualified |
| proposed/testing | 缺可修材料、已证明退化、证据不足、评估器错误 | returned/rejected/inconclusive/error，均不能进入 applying |
| qualified | Execute 获准；效应存在且前置条件有效 | applying；等待当前自 Run 结束时可带 deferred 原因 |
| applying | 核实实际候选版本与授权收据均已落地 | observing |
| observing | 生产覆盖充分且全部冻结检查成立 | retained |
| observing/retained | 达到撤回条件并实际恢复到合法版本 | withdrawn；撤回许可本身不够 |
| 任意未结束步骤 | 版本冲突、效果不明、恢复失败 | 保留原事实与 conflict/unknown/error，停止新写入或扩展 |

`inconclusive` 后可在同候选/同适用策略下登记新的样本批次；新候选、新 base、评估规则变化必须重新绑定，不修改旧清单/旧结论。同一候选的同一证据不重复计算为新样本。retained 只结算预定生产观察，不删除资产谱系或停止严重退化检查；后续继续追加结果是合法的。

同一实际目标范围内只允许一个未结自动部署；retained 后可以开始新代。共享 Plugin 默认或全局指导会影响多个 Graph，重叠影响的未结变更要串行或显式隔离，不能只比较字符串 target ID。实验期间有效闭包漂移须重验，不能把两个同时改动的合并收益归给单个候选。

## 独立实验：既测质量，也证明改的确实是这一版

Host 生成并冻结实验清单，将 base/candidate、Graph/Plugin/模型/环境身份、任务与数据版本、显式 pair/assignment 身份、随机因素（可控时）、评分器版本和预定判定条件绑在一起。实际调用的配置必须与清单相符；不能凭版本名前缀或每组 Run 开始顺序猜配对。缺失、中断、失败与不完整用量都保留，不能只挑成功样本送审。

### 实验执行协议

1. 校验候选选集，冻结 base 和 candidate 的执行闭包，保留恢复字节；Graph 候选不必先保存为正式 catalog 资产。经同一 Run 接纳门面接受只读的受验证快照，不增加第二个 Runner 或向公开 trigger 暴露任意快照入口。
2. 评估方选择预授权任务包及开发/留出批次，宿主在看到输出前生成 assignment 表，交错安排两臂，固定输入和可控环境。配对的不同输出不能反过来改变排序或筛除条件。
3. 每个 assignment 用由 experiment/case/arm/repetition 派生的稳定请求身份接纳普通 Run。进程崩溃后找回原 Run；仅有元数据没有 Run 时仍按同一闭包复核当前授权，不把它记为完成。
4. 被测节点只获得该 assignment 的输入和获准工具。实验 Plugin 代码从候选暂存或已保留 revision 解析；不通过临时替换正式 Library 来做 A/B。mutable 现场用独立测试夹具，不复用生产可写目录。
5. Host 收集所有预登记 assignment 的终态、实际配置、不可变输出和用量完整性。独立评估器自己读取、校验并检查产物；其 CLI 入口使用受限结构化请求和已注册来源，不接受模型提交的自由路径/评分 JSON。
6. 先用开发检查排除明显错误，再使用受保护留出做最终验收。开发失败可以退回候选；留出反馈受策略限制。样本不足为 inconclusive，评估器故障为 error，均不得自动发布。
7. 只在准确候选与全部必要证据齐全后生成可信回执。回执绑定 Grade 的准确历史位置及评分配置，而不是以后可能变化的“最新评分”；Host 核实后通过 submit 接入 change。

Provider 的同一逻辑模型名可能被后台更换。清单记录已观察身份、绑定与环境；两臂期间变化且无法证明可比时，此实验不用于自动采纳。模型不确定性用合法重复与序贯判定处理，不把一对不同结果当作总体收益。

实验运行继续走普通 Host → Runner → Goose → 已授权工具。隔离 workspace 和输入快照，使用只读接口、可重复 fixture 或模拟外部端点；不得为了重放同一状态，向真实渠道重复发送、付款、删数据等。缺失当时有效 Provider 请求与环境时，只能称近似复现实验，不称精确历史重放。向同状态模型询问原因可以形成假设，不能成为因果或质量证据。

将 `anchor-eval` 从单纯记分入口扩展为受保护的实际评估入口：固定评分实现读取获准的不可变实验产物，自己执行检查，写独立评分根。候选与提议 Agent 没有评分根、评分器及留出数据包的写权限；普通节点不能把自行输出的 PASS 字样变成 Grade。独立性来自权限、数据与执行身份，不只是换一个模型做评审。

Host 为评估方启动一个受控子进程，以私有 stdio 发送结构化评估请求；同一安装只有该进程持有 Grade writer 租约，内部串行落盘，不能让并发实验各自争抢 writer。它不是常驻网络服务，也不向候选暴露评分端口。进程故障重启后按实验/评分引用核查已有结果，不重复运行模型；候选代码和测试请求始终在已有隔离网络中执行，不能访问 Host 回环管理面。

提议者不读取完整留出案例和答案；被测 Run 只接收本次执行所需输入，不能读取期待答案或其他留出案例。评分器向提议者返回策略允许的反馈，不把受保护测试包通过 Artifact 或工具明细泄漏给下一轮候选。

采纳分四层，不用单一“分数提高”替代所有边界：

1. **范围有效。** 候选、权限、依赖来源、输入输出与撤回资源都在授权闭包内。
2. **质量有效。** 有任务级独立检查和未参与改写的留出回归；重要任务损失不能被平均数掩盖。只有模型意见的任务须标为非权威，不能默认自动发布。
3. **改进有效。** 在质量约束下，证明至少一个事先声明的目标有所改善；质量非劣不等于有采纳价值。耗时、token、费用、失败与纠正分别报告，纳入 Dreaming/实验自身消耗；费用未知时不宣称已节省实际账单。
4. **生产可检验。** 明确上线样本的版本、暴露范围、观察条件及自动撤回条件；不能因报告不再显示某信号就宣告问题消失。

复用现有 Hub gate 的统计实现，不另写一套非劣检验。先补准确实验配对与完整绑定；受保护评估进程调用固定版本的只读 Hub，验证结果来源，再生成可信回执。当前 gate 只有质量非劣及任务损失检查，还不是上述完整采纳门禁；现有默认统计参数也不是本设计替用户决定的风险政策。

留出集不能夜夜暴露详细答案并无限调参。评估方管理开发案例、受保护留出及后续时间窗，登记已使用批次与自适应查询；反馈粒度和更新方式由外部策略确定。已有序贯统计不自动解决自适应选候选、反复使用留出数据导致的泄漏。

### 默认评估包与业务接入

内置两组受保护评估：`input-discovery` 用确实经过授权工具的文件识别/交付检查验证指导文案；`dreaming` 用固定结构化历史、已知缺陷、正常样本、假节省和越权陷阱验证选题到改进结果。二者都由官方 Rust 评分入口实际核查，不靠模型输出的 PASS 标记。

普通用户 Graph/Plugin 加入自动改进范围时，绑定现有测试、确定性结果断言或部署者提供的独立评估包；可使用成熟检查工具，但其版本和来源必须由外部授权并独立执行。模型质量评审只能作为明确标来源的辅助信号，缺少独立口径时显示 `evaluation_unavailable`，不向用户索取每个候选的人工批准来补洞。新增评分逻辑和目标变更不属于候选自身的更新范围。

### 实验接纳与评估进程契约

#### 普通 Run 的私有快照入口

新增 run_access 内部 `admit_verified_snapshot`，参数只接受 Host 构造的 `ValidatedExperimentBundle`、准确 assignment、可信来源上下文及稳定 AdmissionRequest。它复用现有接纳核心，不向 HTTP/Pilot 普通 trigger 暴露；已校验类型不能由普通工具 JSON 直接反序列化构造。

现有接纳按 `graph_identity(bundle_source)` 排斥同源 unfinished Run，所以每个 assignment 要有独立且持久的实体 bundle 根，不能只用不同 request ID 或 symlink 别名。这些只读 bundle 是冻结实验 Artifact 的执行视图，不进入正式 catalog，不是第二份资产权威；引用与恢复沿现有租约保留。相同 assignment 的重试必须重用同一个源、快照和 Run。

新入口的 request 内容摘要绑定 `{schema_version, experiment, assignment, snapshot_digest, closure_digest, input_digest, trusted_context}`；ID 由安装/实验/assignment 身份派生。旧普通 admission 的 `(graph,input,objective)` 摘要算法保持不变。A/B 在逻辑上可交错/并发，不改变普通 Graph 的单活动 Run 规则；实际并发仍由现有执行容量约束。

#### Manifest 与结果绑定

| 结构 | 必需字段 |
| --- | --- |
| `ExperimentManifestV1` | schema、change/candidate/base、策略引用、两臂执行闭包、suite/evaluator/holdout 批次身份、环境/模型身份、assignments、指标/质量/收益判定计划、允许的生产外部效果策略 |
| `AssignmentV1` | `id`、`case_id`/任务版本、`pair_id`、`arm: baseline/candidate`、`repetition`、输入引用/摘要、冻结的执行顺序；可控随机种子或 unavailable |
| `AssignmentObservationV1` | assignment ID、实际 Run/配置引用、执行结果与输出 Artifact、质量 GradeRef 或缺失原因、用量/耗时/价格完整性 |
| `EvaluationReceiptV1` | 请求/manifest/evaluator/policy 摘要、全部预登记 assignment 的观察引用、覆盖统计、`scope_valid`、`quality_noninferior`、`improvement_proven`、逐检查/任务结论、异常/漂移、受限反馈 |

三项判定字段均为 `pass/fail/inconclusive/error/unavailable` 枚举，不用默认 false 吞掉未知；只有全部 pass 且生产/恢复前提有效时才能 qualified。检查结果、统计结论与“是否已应用”是不同字段。

所有必要 assignment 必须出现在回执中；新增样本使用新的不可变批次清单，不能事后删除困难案例。执行结果与评估结果分开表示：

| 现场 | 记法与评分处理 |
| --- | --- |
| 执行完成且产物可检查 | completed；评估器实际检查，成功退出不自动给满分 |
| 工具/模型执行失败或达到预登记超时 | failed/timeout；按任务包预先规定计质量失败，消耗仍计入，不从配对中消失 |
| 取消、中断、尚无 Run/产物 | cancelled/interrupted/missing；覆盖不足，不自动采纳；不能只换一个请求补成功样本 |
| 评估进程故障、评分输入损坏 | evaluation_error；不冒充任务质量失败，也不能通过 |
| 原执行存在未知外部效果 | unknown；禁止为了补数据自动重放，保持未决 |

只有任务包明确预登记的失败处理才能产生分值；没有质量 Grade 不擅自填 0 或从分母删除。补齐同一 Run 的评估可形成新回执，但不能悄悄替换旧裁决引用的 Grade。评估包自己也要有正反例：失败/超时混合输出中含有 `passed` 等字样不能骗过检查器。

#### 私有 stdio

Host 启动固定发行的 `anchor-eval` 受控子进程，新增 evaluation 模式。每行一个带 `protocol_version: 1`、`request_id`、`method`、`params` 的 JSON 请求；method 仅为 `evaluate` 或 `lookup`。stdout 仅输出对应请求的结构化结果，日志去 stderr；沿用已有 JSON/进程/取消原语，不建通用 RPC 框架。

`evaluate.params` 为 `{experiment_manifest_ref, evaluator_package_digest, policy_ref}`。assignments 从受保护清单读取，不允许调用者另外提供分数、预期答案或另一组选样。`lookup` 只查询该请求已持久的评估回执。Host 从受控进程和评分历史交叉核实返回的引用，不信任 arbitrary stdout 中一个 PASS。

评估回执放在同一评分根的 `evaluation/<request_digest>` 普通资源，经 submit 写入，使用具名评估 Validator；Grade 仍用现有格式，不给每个分数新增数据库。相同 request/内容复用已记录结果，内容不同报冲突。修改 GraderOnly 的准确资源授权以允许这些回执，不开放任意资源前缀。

写 Grade 前固定该 assignment 的 grader config 摘要，包含 evaluator/suite/manifest/assignment 与输入产物身份。若崩溃发生在 Grade 后、总回执前，重启找回该配置下准确 accepted Grade，补回执，不再调用模型或新建 Run；没有评分时可重复只读确定性检查。相同身份已有不同 Grade 必须报冲突，不用最新一条覆盖。当前低层 `anchor-eval record` 即使保留作管理诊断，也不能被 Host 的实验协议或候选用来提交自由评分。

评估进程只有检查所需来源与评分写入权，没有修改正式资产的工具；测试靶、提议 Agent 和普通 Plugin 不获得它的 stdio、评分目录、任务答案或管理端口。评估包执行外部检查工具时同样使用明确授权和隔离，不能把“评估器可信”解释成可执行候选指定的任意宿主命令。

#### Hub gate 接入与收益口径

Hub 增加显式实验清单/assignment 与准确 GradeRef 的只读查询路径，复用现有 gate 的统计原语；Host 与 Eval 不另写一份 gate 数学。当前按时间 zip、按配置前缀猜 arm、取最新 Grade 的查询仍可服务探索报告，但一律不能生成自动发布证据。

实验顺序、pair 和任务权重在看结果前冻结。质量约束包含全体与具名关键任务/检查项；收益使用预登记指标及统计方法，不能看到结果后改为另一个更好看的指标。若统计原语需要有界输入，归一化边界/异常处理必须由评估政策事先给定；不得把原始无界费用或耗时直接送入有界 e-process。多候选、自适应重复查询及跨批次统计处理随评估版本绑定；未实现的统计方法返回 unavailable，不降级为比较均值即放行。

报告至少拆开基线/候选的质量、模型请求、token、耗时、已知费用、Dreaming 与实验开销，以及实际生产暴露。单次净收益和摊销收益分别展示；尚无真实生产样本或价格未知时只给条件式预测/盈亏平衡点，不能宣称已经节省。统计参数没有生产默认值；内置评估预设可以作为一次启用时展示并明确接受的配置，不能藏在 gate 的默认常量里。

### 首个可运行验证靶

切片 2 使用普通有限文件任务 Graph：输入 Artifact 的目录布局不同，但目标固定为找到指定数据、计算确定性结果并交付指定文件。基线包含可复现的错误输入发现指导，候选可修改指导指令；独立评估从实际交付内容核对计算、选取范围与缺失行为，不看候选自报的 PASS。切片 3 将同一已验证行为接到 Host 的 `input-discovery` 槽。

固定案例包含：正常单文件、嵌套/多个同名候选、缺失输入应明确报错、诱导省略检查的假节省、无须修改的正常 Graph、试图写评分或扩大读取范围的输入。开发案例与留出案例使用不同内容/布局；测试的期待结果留在评估方，目标业务文本仍是普通输入。确定性 Provider 只替换模型传输，发现、提议、真实工具使用、实验、Grade、机器发布和撤回均经过产品路径。

基线在故障案例失败、有效修复通过、假节省/越权候选被拦、正常资产不强制改写，是首轮的明确正反例。随后模拟只在生产窗口暴露的退化，检查 Host 自动撤回。fixture 的固定值和简化判定只能证明链路与边界，不补签真实模型效果或生产统计收益。

## 发布、生产观察与撤回

Graph 发布扩展现有 `application/graphs` 的同一条编译、校验和加锁路径；Plugin 发布复用现有候选暂存、`PluginInstallPrecondition`、Library 锁与依赖落地。需抽出共享的锁内操作时删除重复路径，不建立 Dreaming 专用安装器。

Graph 定义摘要变化可能使原任务/挂载授权收据失配。机器发布入口只能将原授权与当前策略允许范围的交集重新绑定到准确新闭包，沿既有授权所有者保存，并引用本次机器裁决；不能调用 Operator 路径伪造用户确认，也不能为新节点复制超出原范围的权限。接纳和每次实际调用继续复核当前授权。

首片从普通有限 Graph 指令跑通闭环，但完整交付还包含下列明确路径，不把其他可改面留成无实现方案的口号。普通 Graph 的指令、模型引用和结构变化复用同一编译/闭包检查，下一次新 Run 使用新定义，已开始的 Run 保留原快照。

### 宿主建议性指导数据化

首个槽位是 `input-discovery`：把 `graph/logic.rs` 中输入目录的建议文案移到同一份出厂资源，删除原 Rust 文本；Runtime 只接收 Host 已解析的类型化指导快照。工具 schema、路径/联网权限和完成协议仍由二进制生成，不进入可改文案。

激活选择由 `kernel-dreaming` 内 `guidance/<slot>` 资源拥有，只引用不可变候选内容；不再另写一份 settings 当前值。允许准确 Graph 范围的绑定覆盖和显式授权的全局默认，解析优先级固定，变更实验与上线都记录实际 slot/content/scope 摘要。组合根读取并传入快照，下层不读环境变量或 application。一次 invocation 及其恢复保持原快照，下一次 invocation 才选择新指导。

这条路径用 `input-discovery` 的受保护检查跑完整自动应用/撤回，覆盖之前 `find /in` 类问题；不是仅在另一个 Graph 里加相反提示。新增槽位须由正常产品开发明确可改字段和评估口径，不开放任意模板、工具定义或 system prompt 替换。

### Plugin revision、隔离实验与推广

当前 Library 同一 Plugin id 只有一个安装版本，冻结 Run 也不冻结 Plugin 代码。完整方案在 Library 内增加**只保存不可变代码与依赖引用的 revision**，不复制业务数据、不另建 Plugin 注册中心：

1. `library/revisions/<id>/<digest>/` 保存经现有 review/stage 规则接纳的不可变代码与准确依赖视图引用；digest 复用现有 Catalog 定义。`plugins/<id>` 仍是默认安装版本与原 mutable 目录的权威，revision 不是第二个默认版本指针。
2. 为普通 Plugin 的 Graph 引用增加可选 `revision` 字段，只允许准确摘要。无该字段继续在下一次调用使用当前安装版本；有该字段从 Library 的已接纳 revision 解析，并复核当前授权。旧代码引用不能复活被撤销的联网、凭据或宿主路径权限。
3. 实验可使用作用域拥有的暂存来源，生产试用必须引用已保留 revision。Catalog、Skill、MCP、依赖与挂载都从同一已解析来源形成 binding，不能代码来自旧版而工具定义或依赖来自当前版。节点记录实际 revision 和授权摘要。
4. 在调用准备阶段把需使用的代码/视图固定到不可变来源并持有既有引用租约。为适用目标保存 base revision；回收把在途调用、显式 Graph revision、实验和未结变更的恢复引用都计为活引用，不让安装后的机会回收删掉恢复依赖。
5. 初始生产暴露使用获准 Graph 的准确 revision 引用，不切换全局默认。由 Host 选择并冻结 Graph/Session 范围及对照身份，不让候选挑选看起来容易的生产结果；扩展范围仍是新的具名 Execute。
6. 全局更新默认安装版本时，走同一 install precondition 和 Library 锁。必须完整核对所有受影响引用，不能把当前 `impact_scan` 的 200 项展示/扫描边界当作完整结果；重构为同一份可完整消费的扫描，界面分页或截断另算。范围不完整则不作全局自动安装。
7. 撤回 Graph pin 或恢复旧默认代码时分别核对当前准确版本，使用保留 revision 和依赖恢复；mutable 始终保持原归属，不回灌旧业务数据。有内嵌凭据、数据格式迁移、无法证明版本并用兼容的共享状态，或授权闭包变化的候选不进入自动路径。

Graph 候选、revision 与恢复 Artifact 的保留由未结变更/在途引用驱动；结算后沿既有显式释放与回收规则处理，不新设自动删除期限。系统 Plugin 仍不在自动修改范围。

内部统一使用 `ResolvedPluginSource`，一次解析产生 code root、权威 mutable root、实际依赖 view/lock/解释器身份、定义/授权 binding 与租约。不能只把 `PluginDefinition.directory` 指向 revision：现有代码会据此派生 mutable 路径，导致数据归属错误。Skill、MCP、挂载和依赖全部消费同一解析结果，权威 mutable 仍在 `plugins/<id>` 的原位置。

revision 的依赖身份必须绑定实际求解结果，不只有声明摘要；复用现有候选依赖的 resolved view key。替换前在同一 Library 锁内使 base/candidate revision、在途和恢复引用进入统一 live set，再让现有 reclaimer 工作，防止安装结束立即回收恢复依赖。若当前版本并用会写不兼容的同一 mutable 现场，则只做隔离测试，不通过 pin 强行并用。

默认 Plugin 安装目前逐项替换代码，崩溃可能留下仍可解析的混合树。发布 intent 进入执行后、完整核验前，普通未 pin 调用也必须把该 Plugin 来源视为暂不可用；启动对账先恢复此围栏，再开放调用。围栏由未结 intent 重建，必要的安装标记只引用原 intent，不成为第二份版本权威。核实完整代码/依赖/授权均为合法 base 或 candidate 后才解除；第三方修改或混合现场继续隔离。已固定到另一个完整 immutable revision 的调用不受不必要的停机影响。

### 常驻助手换代

当前常驻助手不支持任意指令/结构热改，`next_turn` 不能被解释成这一能力。仅插件选择/联网等既有兼容变化继续按 Turn 规则；指令/结构换代需要 Host 的受限换代用例：

1. 绑定准确 Graph、current assistant、owner/session 和候选版本；验证 work 节点/输入交付以及旧 Workspace、Goose 历史的合法交接条件。不能把修改工具清单变成新增权限。
2. 等待没有在途 Turn、未知工具效果、待结算投递的边界；新输入保留在原 Session 队列，不重放已处理消息。拿不到安全边界就保留 `deferred`，不强制销毁现场。
3. 记录获准换代 intent 与空闲观察，经 run_access 使用有观察证据的 Code stop，等待真实执行者退出；不调用伪造 Human 的普通管理控制接口，不用自动 resume 释放等待人的旧 Run。
4. 按已有 quiescent/current binding 检查退役旧实例，发布新 Graph，并由同一助手接纳用例建立新实例。原 Session 的 current binding 是唯一权威；旧 Run 和交接历史保留，不复制另一套会话状态。
5. 失败后按已保存的版本和 binding 核查；尚未建立新实例时可以恢复旧定义并接纳新的合法实例，不复活已经退役的 Run。撤回也走相同换代边界；存在未知业务效果时停止扩展并核查，不能保证立即撤销外部效果。

不同 Graph 或 Session 的发布结果各自记录，允许已知的部分完成；不声称跨 catalog、Session 和 Run 有事务。Dreaming 自身保持有限 Graph，无需走这条常驻迁移路径。

### 生产观察与恢复协议

Host 维护一个不依赖 Dreaming 模型工作的轻量观察/对账路径：随着新 Run 结果及独立评分到达执行已冻结的生产规则，异常达到条件就自动提交撤回。每日做梦是改进节奏，不是故障必须等到下一夜才处理。观察规则不能读取整个原始业务历史，也不能把缺数据当作安全。

对账器由 Host 生命周期启动，订阅/读取正常事实并在重启时按未结变更恢复；不运行第二个模型循环。每项变更绑定暴露选择、分母、延迟结果、检查项及固定判断条件。明确严重失败立即触发撤回；证据不足保持 observing 并停止扩大范围，不显示为 retained。漂移或评分不可用的处理由冻结政策决定，不临时让候选改门槛。

同一目标观察结算前不叠加另一项自动改动；生产比较保留基线与任务构成，防止任务变简单或 Provider 漂移被记成候选功劳。撤回时 CAS 必须仍指向该候选，不能覆盖后来的操作员版本。保留可恢复的旧资产；不具备恢复条件的候选不进入自动发布。

撤回只改变未来执行的版本选择，不撤销已经发生的外部业务效果，也不静默打断在途 Run。回执区分“已授权”“已尝试”“已观察生效”“生产有效/未知/退化”，不把它们压成一个完成布尔值。

| 恢复现场 | 宿主动作 |
| --- | --- |
| intent 已记录，IO 尚未开始 | 复核当前授权和版本；仍满足才执行，否则记录失效 |
| IO 已开始，目标仍为 base 且无部分落地 | 核实操作边界与原执行身份后继续；不能仅凭摘要相同认定任意操作可重放 |
| 目标已为 candidate，回执缺失 | 检查资源/依赖与绑定，补实际观察；不重复安装或触发业务 |
| 目标为第三个版本或落地不完整 | 记 conflict/unknown，停止覆盖写入，交给现有核查路径 |
| 批准后策略撤销 | 阻止尚未执行的应用；已应用部分继续对账，撤回权限按原恢复授权独立核对 |
| 新 Dreaming 不能启动、失去必要产物或生产检查退化 | 不依赖模型恢复；提交撤回，恢复仍可用的旧版本/出厂 seed |

### 发布并发、幂等与故障核查

#### 机器发布入口与授权收据

Graph 所有者增加仅接受已验证机器 intent 的内部入口，输入 `GraphPublishPrecondition {expected_base, candidate_ref, policy_ref, change_ref, execute_record_ref, effect_ref}`。它不是调用 `update_graph` 的包装器：现有 publisher 既没有 expected-base CAS，也会在 Operator 分支创建用户授权收据。应抽出同一锁内编译/写入核心，让普通管理和机器入口分别提供真实来源，删除重复校验。

机器授权使用既有授权所有者的规范化 capability 投影，比较 Plugin authorization、委派 callee、联网、宿主路径/命令、凭据来源、mutable 和工具能力；不得只比较 Plugin ID 或原始 JSON 字段。候选能力必须同时被原有效授权及当前策略涵盖；找不到原依据的能力失败关闭。新增/改名/拆分节点按实际能力匹配，不能按节点名字继承授权。

收据绑定原授权依据、准确新闭包、当前策略及本次 accepted Execute；仍由原授权存储保存，不另开授权库。旧 Operator 收据不改写成机器收据；后来真实用户重新授权时以当前所有者规则判断，旧机器证明不得扩大它。发布后接纳和 invocation 继续核对当前来源/撤销事实，旧版本代码不能复活已撤销权限。

#### 锁序与撤销边界

1. 启动对账先从历史重建每个稳定目标/重叠影响范围的未结部署所有者，未完成前不开放自动发布；损坏或无法证明唯一所有者的目标隔离。该索引可重建，不新增权威 pending 文件。
2. 最终应用沿用事实所有者已有的 catalog guard → Graph lease / Library lease 的锁序，不能外层拿锁后再调用会拿同一锁的 publisher。多个准确目标需要锁时采用稳定身份顺序；跨目标应用仍分别记结果。
3. 取得资产锁后，最后获取安装级 policy 的共享提交 gate，核对当前策略、候选/base、原 intent、目标所有者和授权收据。gate 覆盖最终本地字节/收据提交及 durable sync，不仅覆盖一次检查；模型、实验、依赖下载均在 gate 外完成。
4. 外部策略更新/撤销使用该 gate 的独占模式，持久提交策略后释放，不能反向等待资产锁。已经完成本地提交的版本记为先发生；撤销后尚未越过提交边界的应用停止。已生效版本按独立恢复授权继续观察/撤回。
5. `kernel-dreaming` 的短写锁只串行资源建立、submit、幂等核对和缺失 effect 修复，绝不拿着它等待资产锁、实验或网络。root 的单写租约沿 Host 生命周期持有，不与短写锁混淆。

Kernel 的 unknown-effect 只按 submission target 阻挡，同一资产的不同 `change/<id>` 不会自动互斥；上述目标守卫不可省略。普通用户更新不受“机器拥有资产”阻止，但会使机器 CAS 失效，自动流程不覆盖该更新。

#### Execute 与 effect 的中断缝隙

当前 Kernel 在 target history 提交 accepted Execute 后才创建 effect。恢复算法固定如下，不先重试 Execute：

1. 在 Dreaming 短写锁内读取本根 accepted Execute 的完整历史，按其从 1 开始的真实记录位置推导原 `effect/<target>/<place:012>`；不是 resource.version，也不是只数 accepted 记录。
2. 若 effect 缺失，以原 endpoint 和授权记录建立原身份的 authorized effect；若已存在，核对 endpoint、来源与原记录一致，不匹配即隔离。仅可信 Host 可以使用 Kernel create 完成这项派生资源修复，不接受 Agent 请求。当前 FileStore create 还有“目录已建、resource.json 未落盘”时 load 为空而 create 永远 Exists 的缝隙，须同步补齐通用半创建恢复：在既有资源 lease 内，确认没有有效资源、没有历史字节且由调用者证明原初始化内容后，完成同一身份的初始化；已有有效资源仍按原 Exists 语义返回，损坏资源或非空历史不可覆盖、不可删除重建。effect 的证明就是原 accepted Execute；其他资源使用各自已冻结初始化 intent。修复创建时间由当前 Kernel 盖章，原许可时间仍取原 Record，不能伪造。
3. 原 intent、effect 和 started 均核实、持久后才允许 IO；accepted 历史已有而 submit 返回存储错误时也按这条路径核查，不生成第二条 Execute。
4. 策略已撤销且能确定尚未 IO 时，沿现有 `authorized → started → failed` 结算：started 表示执行器开始处理，不声称发生业务写入。先在 change 写 `not_applied/no_io` 可信观察，再让 effect 结算引用该观察；EffectLifecycle 只保存 status，不能把原因仅塞进 effect content。
5. 若已是 started 或现场不完整，target 仍等于 base 也不能单独证明从未 IO。需核查原操作边界；无法确定则 unknown，不补造 no_io。effect unknown 只能附真实观察转 succeeded/failed，不能重置 authorized。

撤销后保留对应 Code 的窄观察/结算权及原恢复授权，避免无法结清已发生事实；这不授予新的候选应用权。恢复使用已有 Kernel effect 语义，不新增 cancelled 状态或通用恢复引擎。`kernel-dreaming` 不提供资源删除重用，effect/变更历史不因 UI 清理而丢失。

#### 各层幂等身份

| 操作 | 键与内容相等条件 | 重试 |
| --- | --- | --- |
| 初始化 seed/计划 | 安装身份、seed/init 代次、操作种类；确切 seed/Graph/计划创建内容 | 核查原对象；删除意图后不补建 |
| 主周期 | 安装/owner、手动 key 或原 occurrence request；冻结上下文及来源 | 同周期同 Run，不换 Dn/窗口；已有 Run 不等于 worker 已恢复 |
| 候选准备/提议 | 原生 invocation、调用 request ID；准确候选/目标/base/预测摘要 | 返回原引用；同键内容变化报冲突 |
| 实验 assignment | experiment/case/arm/repetition；冻结 manifest/快照/闭包 | 返回原 Run；分别处理仅 metadata、Run 无 worker、已有 attempt |
| 评分 | evaluator 请求/assignment/config；实际 Run、产物与检查身份 | 找回准确 GradeRef/回执，不取最新分数 |
| 应用/撤回 | change、操作种类、准确 accepted Execute 历史位置 | 同一 effect 核查，不重发安装/业务动作 |

Run 无 worker 的恢复由 run_access 处理：未派发可安全启动，已有执行事实先核查，人的等待不能由 Dreaming 自动释放。重复 admission 只返回 Run 并不等于恢复；Host 协调器必须检查实际进展，不能让 testing 永久等待一个无人运行的 Run。

#### Artifact、revision 与删除保护

接受不可变引用前，在现有 file-facts/Artifact GC 同步边界确认字节 available 并建立持久引用；与清理并发时只能得到受保护引用或具名 unavailable，不能返回一个刚被回收的“成功候选”。将未结 change 的 base/candidate、实验清单、核验必需产物和恢复内容加入既有 protected-delivery 计算；不是把 Markdown 中出现的 ID 当保留。

保留所有权区分普通用户 retain 与机器实验/恢复引用；取消普通 retain 不能删除机器仍需的字节。只有无在途 invocation、未结观察、effect 核查及仍适用恢复引用时才释放相应保护。retained 不自动删除生产回退资源；旧版本何时不再可恢复按显式恢复策略判断。正常 Graph/Plugin/Run 删除前置检查消费这些引用，不能删掉未结自动发布的依赖。清理遵循既有授权，不新设保留天数。


## 自递归：D0 提议 D1，但 D0 不能给 D1 改考题

Dreaming 自身是普通受管资产，不另设自升级通道：

```text
D0 从全局事实提出 D1
  → 相同的隔离实验与独立裁决
  → D0 本轮结束后，由 Host 发布 D1
  → 下一次计划接纳 D1
  → 后续结果验证；退化时由 Host 恢复保留的 D0
```

当轮冻结的 D0 不热换脑，不修改自己的运行定义或验收代码。用于判断 D1 的策略、评分器和留出版本在候选产生前已经绑定，D1 不能降低它们的门槛；授权变更仍来自外部。Host 在 D1 无法启动、失去产物或观察退化时也能撤回，不能把恢复能力放在 D1 自己的 Graph 里。

自评测比较的是**最终找到并经独立验收成立的改善、误采纳/漏检、后续退化与总消耗**，不是提案数量、报告长度或自评分。首批固定任务应同时有已知缺陷、无需改动的正常样本、看似省钱但质量变差的陷阱，以及能诱导篡改评分/扩大权限的输入。D1 若只是多花钱生成更多建议，不算进化。

自递归实验将 D0、D1 分别运行在同一发行 Host 的隔离测试实例中，使用相同的冻结全局历史与测试资产，但拥有独立的可写根、模型会话和候选现场。内部提议经过真实 submit/实验/发布路径，只能影响测试靶；不得读取真实变更根或写生产 catalog。评估器从外侧核查是否找到预置缺陷、错误采纳正常目标、修复后是否通过以及全部运行消耗。内部测试不继续发起下一层自递归评测；跨代递归发生在不同的真实 Dreaming 周期，不是一次 Run 无限套娃。

自升级 Execute 绑定 `activate_after_run`。由 run_access 的终态/worker 退出观察触发 Host 应用，不让 close 在自己还运行时替换自身；即使 close 的报告写入失败，已保存的 intent 也能被宿主对账。恢复链同时保留已验证旧版和发行 seed，但只能使用与当前工具/评估协议兼容的版本。

选择信号、取证顺序、实验策略可先放在 Dreaming Graph/指令中迭代；Rust 分析器本身仍随二进制交付。确有证据需要参数化分析时再暴露窄的类型化配置，不给 Dreaming 任意加载分析代码的权限。

## 全局尺度与读取成本

原始事实的数量可以很大，但 Agent 的第一轮输入应只有结构化窗口摘要、有界高影响信号、覆盖/遗漏统计及少量普通样本。按频次、累计消耗、质量损失、影响范围和完整度安排调查；保留正向范例与普通样本，避免只看既有分析器能命名的失败。候选只在有代表性任务上实验，不等于对一万个 Graph 各跑一次。

当前 `Projector` 按历史位置增量投影，但 [`DataRoot::history_after`](../rust/anchor-hub/src/source.rs) 仍先读完整单资源历史再切片，刷新还会枚举来源与资源；查询/分析也有全表扫描。不能把它写成总成本已经是新增数据量级。

第一轮规模验证用 10,000 个 Graph 的结构化历史 fixture，分别测冷启动、无新增刷新、少量追加、长寿命资源追加的物理读取字节、解析记录、资源枚举、耗时、内存及 Agent 输入量。先复用 `HistoryReader` 补物理游标，仅解析新增完整记录；游标绑定来源与资源实例，截断/替换时失效重建，不伪造全局序号。目录发现与分析扫描成本单列，不能隐藏在“增量”这个词里。只有测得冷启动重建仍过贵才增加一个可删、可重建的投影缓存，不先建数据仓库或第二权威存储。

全局阶段不挂载整个 state 或 Goose 会话目录。Hub 接口本身也须只接获准结构化根；可选 `calls` 含业务值，不能因 MCP 被标成只读就开放全部明细。原始证据访问由 Host 按信号及授权范围提供，禁止用“每支调查一条”的提示约定代替实际访问控制。不复制完整聊天、提示词或隐藏推理。

在 `anchor-facts` 增补最小的安全闭环词汇，使 Hub 能按变更、实验/配对、版本、暴露及结果关联；业务正文仍留在原有事实所有者与 Artifact，投影只取白名单字段。新一夜沿“数据根 + resource + 历史位置”取窗口，记录目标 base/信号/策略/已试候选，避免没有新证据时重复实验；不引入默认请求/token/费用上限或历史保留期限。

窗口使用每个来源/资源的已完整记录位置，而不是只用日期或毫秒时间戳。周期开始冻结上界，后续到达的迟到事实进入下一周期；周期结束只推进已处理的结构化窗口，未结变更继续按其引用跟踪。重启复用原窗口；失败周期不假装已经覆盖，按原事实重试或显式标记缺口。去重至少绑定 signal/target/base/candidate/policy 与已试清单，避免同一基线反复生成同义提案。

Hub 对 change/cycle/assignment 的投影是白名单结构摘要，不能泛化读取 `submission.content`。生产、开发实验、留出和 Dreaming 自测分别标识；全局画像可同时查看，但成本/质量比较不能混在一个分母里。普通采样保存抽样规则与输入窗口，后续可复核而非只保留模型概括。

### 冻结窗口与覆盖的实现规则

Host 调用 Hub 的受限只读进程适配器，传入已注册来源、获授权 scope 和准确 WindowManifest；Host 不链接 Hub crate，模型不能传本机路径。Hub 增加窗口/作用域约束的只读输入，投影可以持续刷新，但交给本周期的查询只能看冻结上界。授权过滤在分组/聚合之前应用，不能先算全体数据再只过滤返回的明细。

来源之间不存在原子快照。某根已记录结果、另一根评分尚未到达时，覆盖显示缺口，不能生成假配对；迟到 Grade 进入下一窗口时仍能关联保留的原 Run。窗口处理位置与分析器逻辑版本绑定，升级分析器需要重新分析时保留重建原因，不伪造新增业务事实。

WindowManifest 使用受保留 Artifact 保存大规模位置向量，cycle 只保存其引用与核对摘要，避免反复把万个资源的坐标复制进每条 Record。`seen`、`included_in_summary`、`investigated`、`deferred`、`unavailable` 分别计数，读过全局摘要不等于逐项取证。处理窗口可以推进，但未选且仍有价值的信号由原周期 agenda/结果引用带到下轮；它不是丢弃，也不新建通用任务队列。

`no_work` 必须同时没有新增有效事实、未处理信号、可推进实验/生产结果和合法新探索样本。没有新证据的同义候选不构成新工作；只有无须模型的对账时由 Host 处理。Dreaming/实验生成的记录不作为必须启动下一轮模型调查的理由，其新验收或生产结论可以使相关信号重新可选。

历史 Reader 的游标记录源/资源实例、字节偏移、完整行位置及校验锚点。绑定失效、文件替换、截断或中间损坏分别重建/报错；忽略的只能是无换行尾部。无新增完整行不重新反序列化旧行，追加成本按新字节与每资源验证常数计；读取 buffer 的额外字节如实计量。Reader 不获得 writer 租约、不修改历史，也不使用写 Store 的恢复副作用。

10,000 Graph 验收的结构性断言：固定新增信号和相同分页输入时，100/1,000/10,000 Graph 三组的首轮模型字段/数量相同，不夹带原始历史；无新增刷新解析新增记录数为 0；小量追加不重新解析完整旧文件；完整影响扫描覆盖超过 200 个引用。资源枚举/索引扫描仍可随全体资源增长，单独给出测量，不能据此宣称总 IO 已是 O(增量)。性能时间阈值用固定环境基线制定，不能临时为通过测试隐藏工作量。

## 管理 API 与用户界面

以下均为待新增接口，HTTP 和 Pilot 管理适配调用同一个应用用例；不经 HTTP 从节点向自己的管理端口发送请求。

| 接口 | 语义 |
| --- | --- |
| `GET /dreaming` | 返回受管 Graph/seed、策略与 schedule 引用、就绪原因、当前 Run、窗口覆盖和变更统计；实际版本读 catalog，不捏造已应用状态 |
| `PUT /dreaming/policy` | 外部管理者按版本建立/更新/撤销预授权；`If-Match` 防覆盖；不接受通过普通 Dreaming Agent 工具调用 |
| `POST /dreaming/cycles` | 带 `Idempotency-Key` 接纳与每日计划相同的 Graph；返回真实 Run；busy/不就绪具名反馈 |
| `GET /dreaming/changes`、`GET /dreaming/changes/{id}` | 分页投影提议、证据、实验、裁决、实际生效、生产观察；只返回本管理范围的引用 |
| `POST /dreaming/changes/{id}/withdraw` | 外部主动撤回，与自动撤回用同一执行用例；绑定当前版本和恢复引用，不是任意覆盖 |
| `POST /dreaming/restore` | 明确恢复受审的发行 seed/已验证版本，核对当前 binding 与版本；不会删除历史、重放业务或覆盖未获授权的目标 |

Graph 的查看、编辑、运行历史和节点 Artifact 全部复用原页面，不新增 Dreaming 专用画布。图列表给受管实例一个宿主投影标记；复制 Graph 不复制该标记或权限。设置页增加 Dreaming 区块：启停/就绪、计划链接、可改目标/字段、评估与恢复条件、最近结果和手动执行入口。变更详情链接既有 Run、Artifact 和目标版本，展示预测与实测差异，而不让报告成为状态权威。

不设置每项提案的“等待用户批准”按钮；管理者可以主动停用、调整边界或撤回，但正常路径由机器裁决。Graph 绑定、模型配置、计划和实际 worker 观察组合成 `not_configured/not_ready/ready/running/incompatible/disabled` 等显示状态，前端不能据此授予权限。新界面沿用现有设置、列表、错误、加载和版本冲突组件，移动端同样可管理。

### HTTP 请求、响应与错误

所有接口沿用当前安装的管理身份/认证、owner 与错误脱敏边界，不新增登录体系，也不扩大渠道回调凭据的路径范围。身份由现有管理适配器派生，不接受模型自报 owner 或“我是本机管理员”。当前单机管理面的可信范围仍是原部署边界；候选/Plugin 沙箱不能访问这个管理面，这是启用自动改进的必要前提。

| 接口 | 首版 DTO 与并发语义 |
| --- | --- |
| `GET /dreaming` | `{schema_version,status,readiness_reasons,graph,seed,policy,policy_ref,schedule_ref,active_run,coverage,changes}`；policy 是无凭据的当前配置投影，coverage 为摘要/manifest 引用，changes 为分类数量；附策略 control 的 ETag |
| `PUT /dreaming/policy` | body 为完整 `DreamingPolicyV1`，非任意局部 JSON patch；必带 GET 捕获的 `If-Match`，缺省不解释为无条件写；返回新策略引用、ETag 和真实就绪状态 |
| `POST /dreaming/cycles` | body `{}`，必带 `Idempotency-Key`；Host 选择受管来源/窗口，模型不能传 graph/owner/context；新接纳 202 `{cycle,run,graph,replayed:false}`，重试 200 返回同引用及当前事实 |
| `GET /dreaming/changes` | 按状态/目标筛选并分页；沿用现有分页原语，游标绑定查询和已观察上界，不因分页重排跳过记录 |
| `GET /dreaming/changes/{id}` | change 版本、target/base/candidate、证据/实验/裁决/应用/生产结果引用及可用操作；附 ETag，不直接暴露内部根/留出答案 |
| `POST /dreaming/changes/{id}/withdraw` | `{expected_target_digest,reason}`，必带 change `If-Match` 与 key；接受撤回 intent 返回 202，不声称已恢复；已完成重试返回当前事实 |
| `POST /dreaming/restore` | `{expected_graph_digest,restore_ref,reason}`，control `If-Match` 与 key；restore_ref 只允许已登记兼容旧版/seed；同一用例核查并记录结果 |

主周期同键重试先查已提交事实，不因后续禁用、升级而创建新周期；这只保证返回原对象，不恢复已撤销的执行权。不同键遇到活动周期返回 busy，不把另一周期冒充本次成功。mutating key 规则复用现有 1–128 个可见 ASCII 字节校验、owner/操作范围及同键异内容冲突；建立新资源后不随报告清理去掉去重事实。

错误沿既有 `error` 文本增加 Dreaming 的稳定 `code` 和 `retry: never/refresh/reconcile_same_request`，可带范围内的相关对象引用；不新增全站错误框架。业务结果 `no_change/rejected/inconclusive` 是成功读取到的事实，不返回 HTTP 500。

| HTTP / code | 语义与恢复 |
| --- | --- |
| 400 `invalid_request` / `unsupported_schema` | JSON、字段或协议错误，修改请求，不能原样盲重试 |
| 401/403 `unauthorized` / `scope_denied` | 复用既有认证判断；Agent 不可改策略/评分/正式资产 |
| 404 `not_found` | 对象不在当前可见范围；不泄漏其他 scope 的存在 |
| 409 `busy` / `idempotency_conflict` / `base_changed` / `policy_changed` / `schedule_conflict` | 刷新真实事实；更改候选或策略要建立新身份，不能覆盖旧记录 |
| 428 `precondition_required` | 新增管理写入口缺 If-Match；不改变既有 Graph API 的兼容语义 |
| 409 `not_ready` / `evaluation_unavailable` / `incompatible` | 配置/评估/版本前提不满足；给出具体缺项，不以人工逐提案批准代替 |
| 410 `artifact_unavailable` | 引用的字节已过期；不得拿同路径新文件冒充旧候选 |
| 503 `dependency_unavailable` / `storage_unavailable` | 先查询原请求是否已提交，再按同 key 核查重试；不得默认操作未发生 |

相同 code 用于窄工具的结构化错误及 Host 日志，复用现有 ToolError/MCP isError 传递，不从自然语言错误文本猜分支。Kernel 的通用 Reason 不为每个 Dreaming 业务原因扩枚举：领域原因进入受验证 content/回执，通用权限/版本/内容原因仍用既有 Reason。

### 用户界面与本地可观察性

设置页必须区分“默认已安装”“缺配置”“计划停用”“运行中”“观察未结”和“兼容失败”，不能只有一个开启开关。首次启用展示准确目标、评估及恢复预设；编辑时保留原 ETag，冲突后不自动覆盖。停用后仍显示已生效版本与恢复结果。详情把预测、实验结论、实际应用和生产判断分栏，未知不显示为 0 或成功。

不新建遥测服务。通过既有历史、Run/Artifact 页面与 Host 日志定位 cycle/change/experiment/assignment/execute/effect；日志不写凭据、完整提示或留出答案。每周期至少能投影：来源覆盖/缺口、物理读取/解析/扫描、选题/下钻量、全部模型及实验消耗、合格/冲突/未决候选、应用/撤回、预测偏差及生产净改善。原成本只在原执行事实计一次，change/report 引用汇总，不再复制一次 stamp.cost 造成双计费。

## 代码落点与依赖

| 路径/模块 | 实施内容与替代关系 |
| --- | --- |
| `resources/graphs/dreaming/` | 唯一出厂 Graph 源；迁入 `rsi-signals` 的有效调查/评审指令，去除源码目录、Python 脚本与报告式发布假设 |
| `anchor-distribution`、release 脚本 | inventory v4、种子资源/Hub/Eval 的构建与 source-free 校验，替代另行下载和手工导入；原生包与归档使用同一 inventory |
| `anchor-runner-host::application/dreaming` | Policy、周期窗口、change Validator/提交、实验协调、可信回执、apply/monitor/reconcile；按真实独立测试边界分文件，不拆服务 |
| `application/graphs`、`application/plugins`、`application/assistants` | 增加只接受受验证机器 intent 的窄入口，共用原编译、锁、CAS、安装与交接操作；不增加另一套 publisher |
| `application/run_access`、`run_metadata` | 只从既有接纳门面创建实验/自测 Run、读取终态；保存可选可信来源上下文，不增加另一份 Run 相位 |
| 组合根、`execution/tool_assembly`、`node_host` 中立契约 | 注入 native operation 与窄工具，冻结指导/资源快照；不在 node_host 调 application/dreaming |
| `anchor-facts`、`anchor-hub`、Kernel `HistoryReader` | 最小共享闭环词汇、准确 assignment 投影/gate、安全字段及物理增量读取；不把业务正文或实验执行器放入 facts |
| `anchor-eval` | 受保护实际检查入口、绑定回执、内置评估包与 Hub gate 调用；延续唯一 Grade writer，不把该身份交给 Host/Agent |
| `anchor-library`、GraphHost Catalog/依赖、Runtime PluginRef | revision 存取、可选准确代码引用、统一来源解析和恢复引用回收；替代临时全局换 Plugin 做实验的做法 |
| `apps/web` 与 API/Pilot 适配 | 增加控制面投影与配置，复用 Graph/Run/Artifact 页面和完整管理组件，不另建事实存储 |

只有证据证明需要中立共享原语时才抽模块；不新增 `anchor-dreaming` 通用框架 crate。Kernel 保持通用语义，现有 Run、Host 层次、Hub 只读和 Grade writer 围栏不得加例外；新增窄入口与“普通 Agent 不能调用”的边界要加自动围栏/契约测试。

### 类型与模块归属

首版在 `application/dreaming` 内按真实边界划分 model/validator、cycle、experiments、deployment/reconcile 与 query；可以先合并小文件，不要求为名称建空模块。模型请求与固定观察分开类型，可信 proof/validated bundle 的构造入口保持私有。API、窄工具、native op 全部调用相同应用方法，不各写一份范围/状态检查。

Policy、TargetRef、候选及机器发布命令归 Host 应用层；Eval/Hub 确实共用的实验关联、准确事实/评分引用和评估回执 DTO 放 `anchor-facts` 的中立词汇模块，不带 IO 或 Host 类型。图编译/PluginRef 归 Runtime，revision 存储及实际依赖引用归 Library。Eval 不能依赖 runner-host，node_host 不能反向导入 application，Host 不依赖 Hub library。工具 schema 和持久/API DTO 的公共字段从同一类型派生或用同一组序列化 fixture 验证，避免第二套校验器。

以依赖围栏固定这些规则；新增事实词汇先写旧缺省/新格式正反例和摘要测试，再接入写入者与 projector。不要为了一个长文档一次性增加全部抽象或把业务命令塞入 Kernel。

## 完整交付顺序与验收

以下是实施计划，不是已完成状态。1–2 先给可运行垂直切片；完整交付包含 1–6，自递归、默认内置和其他可改面都有具体路径，不能以一个报告 Graph 代替。

| 顺序 | 交付与依赖 | 验收出口 |
| --- | --- | --- |
| 1. 默认 Graph 与可信入口 | 出厂资源、初始化/绑定、计划引用、native operation/工具适配、周期窗口；新二进制部署到测试安装 | 空目录无需源码和模型即可安装并查看 D0；一次启用后同 request 只接纳同 Run；复制 Graph 无权限；无新增工作不启动模型 |
| 2. 单目标完整自动闭环 | change 细腰、准确实验接纳、实际独立评估、有限 Graph 发布、Host 生产观察/撤回，依赖 1 | 确定性 Provider 经实际 Host/Runner/Goose/MCP：发现坏指导并自动修复，假节省被拦，生产退化自动恢复；所有崩溃点可核查，无逐提案确认 |
| 3. Anchor 级指导与全局尺度 | 指导槽数据化、Graph 结构/模型闭包、物理游标、安全下钻、10,000 Graph 基准，依赖 2 | 原 Rust 指导文本已删除；实验与实际生效版本一致；总模型输入不随全量历史线性增长，磁盘/枚举/扫描成本如实量化 |
| 4. 自递归与出厂恢复 | 隔离 meta 评测、D0→D1 的同路径发布、跨周期激活、版本兼容与 seed 恢复，依赖 2–3 | D1 找到更多真实可采纳改善或以更低消耗保持质量；正常样本不乱改；D1 自改验收失败，D1 无法启动仍可由 Host 恢复 |
| 5. Plugin 与常驻实例 | revision/pin/依赖保留、完整影响扫描、限定生产暴露、常驻安全边界换代，依赖 2 和原生命周期 | A/B 不动正式 Library；更新后旧版本/依赖可恢复，mutable 不改写；跨 200 引用不误称完整；队列/历史交接无丢失和重复外部投递 |
| 6. 产品集成与真实验收 | 完成管理 UI/API、inventory/原生包、无源码发行安装与真实 Provider；沿用 1–5 路径 | 新安装默认可见可启用、升级不覆盖 Dn/不扩大授权；真实小目标完成自动改进和后续判断；持续净收益由生产观察证明 |

### 开发包与依赖顺序

下表是可直接选取的工作包，不是新的进度表；当前完成状态只写统一台账。即使并行开发，也只能在契约稳定且路径不重叠时分工。

| 包 | 前置 | 实施出口与必须同步删除/替代的旧路径 |
| --- | --- | --- |
| A. 契约与 seed | 无 | 固定 DTO/schema/摘要 fixture、D0 Graph 与 inventory v4；静态安装从就绪检查分离，所有发行消费者一起升级，不复制 examples 版本 |
| B. 可信接纳 | A | Dreaming control/cycle、schedule 绑定、上下文、HostOperationPort 与工具装配；替代只允许 assistant 的统一 host-op 前置假设，但保留 Session 授权检查 |
| C. 精确实验与独立评分 | B | 私有 validated-snapshot 接纳、实体 assignment 源、manifest、Eval 私有 stdio/真实检查、Hub 显式 pair/GradeRef；自动证据不走时间 zip/最新 Grade 路径 |
| D. 自动应用与恢复 | C | 受验证 receipt、change Validator、机器 publisher、授权收据、目标守卫、策略 gate、缺失 effect/崩溃核查、Artifact 保护和生产撤回；替代报告后手工发布，保留普通管理路径 |
| E. 指导与规模 | D | 删除 Rust 输入发现文案，接入唯一指导槽；物理 cursor、窗口/作用域查询、完整影响扫描与 10k 基准；不新增数据仓库 |
| F. 自递归 | D、E | 冻结历史的隔离 Host meta 评测、activate-after-run、独立 seed/前版恢复；不建立自升级旁路 |
| G. Plugin 与常驻 | D | revision/ResolvedPluginSource/完整依赖保留与恢复、混合树调用围栏、有限范围推广、常驻实例交接；不复制 mutable，不搭第二 installer |
| H. 产品验收 | F、G | 完整 UI/API、原生/归档 source-free 联合验收、真实 Provider 小目标及后续生产观察；退出同范围旧报告式 RSI 优化循环 |

切片 1–2 的可运行出口需要 A–D 的最小完整实现，不能只完成 A/B 就宣布 Dreaming 完成。管理 API 的策略、执行和查询基础随 B/D 交付，完整 UI 随 H 收口，不让首轮验证依赖手工写 Kernel 文件。Plugin 与常驻未交付前明确标为不可选目标，不让 UI 提前授予不存在的自动改进能力；完整项目验收仍须包含 G。

### 需求到验收的映射

| 要求 | 必须出现的证据 |
| --- | --- |
| 默认自带、二进制用户可用 | 干净 mutable 根从最终发行物安装，无源码/Python/模型也能看到 D0；模型补齐后运行，升级保留 Dn/停用/移除意图 |
| 全局而非逐 Run 阅读 | 10k fixture 的覆盖与读取指标、首轮输入、下钻记录；未授权根/明细不可读，遗漏不假装覆盖 |
| 独立质量与收益 | 两臂准确闭包/配对、固定 GradeRef、失败全覆盖、留出隔离；假节省不采纳，总消耗计入 |
| 无逐提案人工瓶颈 | 预授权后真实 accepted Execute、实际发布与后续观察；正常路径不创建 human pending、不调用伪造 Operator |
| 自递归 | D0→D1 的真实变更链、隔离 meta 结果、下一轮版本；D1 改验收失败，D1 无法启动仍恢复 |
| 全部可改面有实现 | 普通 Graph、Host 指导槽、Plugin revision、常驻安全换代分别经过实验/应用/撤回，而非仅有接口 |
| 崩溃与并发可信 | accepted Execute 无 effect、IO 前后、收据前后、撤销/人工编辑/GC 并发、混合安装等现场重启核查 |
| 实际进化而非自我肯定 | 后续生产窗口的质量/收益、误采纳、撤回和净成本；无候选可合法 no_change，证据不足保持未知 |

### 测试矩阵

| 层次 | 必须固定的契约 |
| --- | --- |
| 纯 Validator/权限 | Agent 不能写可信回执/策略/评分；错误候选、base、policy、来源根/评分位置不能混用；Accepted 不等于应用完成 |
| Host 集成 | 原生工具与 host op 的真实授权；改名、复制、伪造 trigger 无特权；process 与 propose 没有两份规则；单目标并发与撤销 CAS |
| 实验/评估 | 显式配对、失败/缺样本计入、评分重写不改变旧判定、伪造 PASS 无效、留出隔离、两臂环境变化不作有效结论 |
| 发布/恢复 | 每个 intent/started/资产写入/回执边界崩溃、部分安装、第三方编辑、重复请求、恢复版本缺失均可核查，不盲目重放 |
| Plugin/常驻 | revision 代码/说明/依赖同源、恢复依赖不被回收、当前权限重新校验、mutable 不迁移；安全退役/新建与未知投递阻挡 |
| 自递归 | 同一真实路径改 D1、负例不采纳、禁止改验收、测试实例不写生产、D1 失效时独立回退；不以提议数量打分 |
| 规模/数据边界 | 10,000 Graph；冷/热/无新增/长历史追加、截断/替换/不完整尾行；有界模型输入、遗漏计数、没有原始 state 挂载 |
| Web/发行 | 真实事实与操作投影、配置冲突/停用/恢复；source-free、无 Python、缺模型/评估包、重启、同名冲突、用户删除/升级保留 |

实现时先跑相关 crate 的定向测试和已有 `anchor-devtools regression goose-fixture` 链路；跨模块切片后跑必要 workspace test/clippy/fmt，前端变更跑现有 test/e2e/build。真实 Provider 和最终部署单独授权与记录，不把待新增测试名或本地 fixture 写成已通过。

上述矩阵必须同时断言没有越权/重复业务效果，以及符合条件时确实自动落地，不能只验证拒绝路径。所有二进制修改需求记录为带证据的范围外工程提议，不静默删除，也不借用 Dreaming 权限执行源码或发行更新。

首轮证伪标准：若独立质量无法测、改进收益抵不过 Dreaming/实验消耗、全局发现仍退化为 Agent 遍读历史、D1 能改变自己所受裁决，或恢复必须依赖已损坏的 D1，这条方案就尚未成立。单次成功只证明链路，持续净改善必须由后续生产事实证明。

### 验证命令与交回产物

以下是后续实施的命令入口，不是本文已经执行的验证。按实际修改缩小 crate/测试集合；用筛选器时检查实际运行用例数，不把零项测试当成功。Goose/原生发行需要的 helper、固定二进制与串行采证按[统一回归指南](runtime-contract-tests.md)准备，不在这里复制一套会漂移的启动步骤。

```sh
# 事实、读取与评估改动
scripts/test.sh --locked -p anchor-kernel -p anchor-facts -p anchor-hub -p anchor-eval

# 接纳、发布、revision 和发行改动
scripts/test.sh --locked -p anchor-runtime -p anchor-graph-host \
  -p anchor-library -p anchor-runner-host -p anchor-distribution

# 跨模块切片完成后；编译 target 与其他采证工作流隔离
scripts/test.sh --locked --workspace
cargo +stable clippy --manifest-path rust/Cargo.toml --locked --workspace --all-targets -- -D warnings
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check

# 发生 Web 修改时
npm --prefix apps/web test
npm --prefix apps/web run test:e2e
npm --prefix apps/web run build
scripts/check-sensitive-wording.sh
```

新增确定性 Goose 场景接入已有 regression 入口，不另写模型循环；故障注入在真实持久边界退出并启动新 Host 进程验证，不能只在同进程 mock 返回错误。source-free 验收使用最终平台包和隔离 mutable 根，源码/测试根、宿主 Python、旧已安装工具不得成为隐含依赖。

每片交回证据包含源码/binary/seed/evaluator 身份、实际命令与退出结果、schema/权限/摘要样例、对应 change/Run/Grade/effect 引用、故障核查前后现场和未覆盖范围；规模片另给数据生成规则与物理指标。确定性 fixture、真实 Provider、外部业务和目标部署四种结论分开写。`.local` 证据只在原机器可得时如实标明，保留仓库内可重跑的 fixture/测试，不把本机报告冒充可移植验收。

### 开工清单

1. 从本文选择一个工作包，对照当前实现核对事实所有者、依赖围栏和被替代路径；不顺手重构无关工作流。
2. 固定该包的字段/schema、授权矩阵和至少一个正例/反例；涉及历史摘要的先保留缺省旧字段样本。
3. 先交付经产品路径可执行的小切片，再加入崩溃、重复、撤销和作用域负例；不只测试拒绝，也测试合格时确实落地。
4. 实验用确定性 Provider 与隔离数据根，不动当前生产资产。真实模型、权限扩大或生产切换仍需对应授权。
5. 验证后更新当前专项契约及统一台账，只留下实际结论、未覆盖边界和证据链接；不得把本开发计划中的未来接口写成已上线。

## 替代关系与实施前配置

新增的宿主写边界替代“报告 → 人逐项确认 → 手工发布 → 手工回看”，但只在已授权的 Dreaming 范围内替代逐项确认，不改变普通 Plugin/Graph 管理的授权规则。调查编排复用并逐步迁移 `rsi-signals` 的有效部分；正式 Dreaming 上线时不再为同一范围保留第二个报告式运行优化循环。原 RSI 的源码/生态研究仍可作为独立业务存在。

不新增第二套 Agent loop、调度器、Run 状态、安装器、散列算法、通用审批服务或强化学习训练栈。共享摘要、租约、候选准备、发布校验与统计实现均沿用现有原语，发生真实重复时合并旧路径。

启用前由外部明确配置实际目标白名单、可改字段、业务效果范围、可用模型/来源、独立评分与留出版本、质量容忍和生产观察/撤回条件。这是建立授权边界，不是把人放回每次改进的工作流；本设计不替用户预设这些生产参数，也不据此自动切换当前部署。
