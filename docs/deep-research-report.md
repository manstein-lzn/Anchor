# Anchor 多 Agent 编排与版本化工作区架构深度调研报告

## 执行摘要与主题确认

本报告以用户上传的《深度调研任务书：多 Agent 编排 + 工作区架构》为正式研究约束，因此本次调研主题**并非“未指定”**，而是明确聚焦于：**如何为 Anchor 引入持久、可变、可执行的工作区，同时保持可复现、可审计、可恢复，并验证“控制面 + 内容面 + 不可变内容引用”是否是正确架构方向。**报告沿用任务书中的 I1–I9 不变量与 RQ1–RQ10 作为评判基准。fileciteturn0file0

**推荐并正式采用的研究主题：长周期多 Agent 编排与版本化工作区的双平面架构。**选择它有三个理由。第一，影响力最高：任务书的 RQ4 直接决定 Anchor 是否能够在增加代码开发能力后继续满足 I2、I3、I8、I9。第二，可得证据在 2025–2026 年显著增加：OpenAI Agents SDK 已出现 Sandbox Agent 与 Temporal 持久执行组合，PydanticAI 与 Mastra 也分别接入 Temporal，而 Cursor 公开说明其云端 Agent 使用“每 Agent 一个 VM + Temporal 工作流”的架构。第三，时效性极高：A2A 已在 2026 年进入 1.0 阶段，MCP 在 2026 年进一步转向 stateless-first，欧盟 AI Act 自 2026 年 8 月开始进入新的执行阶段，工作区审计、日志、工具行为可追踪已从工程优化逐渐变成治理能力。citeturn12search13turn21search7turn20search2turn2search5turn15search0turn14search4turn16search0

若将主主题拆成可独立立项的研究包，可形成下表；本报告将它们全部纳入同一主报告，而不是择一研究。

| 可选研究包 | 简短说明 | 最适用场景 |
|---|---|---|
| **控制面—内容面边界** | 定义事件历史与可变工作区如何形成一个可恢复系统 | Anchor 架构决策，**优先级最高** |
| 持久 Agent 编排 | Temporal、DBOS、Restate、LangGraph 等恢复语义比较 | 判断 Anchor 是否继续自研运行时 |
| 编码 Agent 工作区 | 隔离、分支、提交、缓存、并发编辑 | 增加代码修改与测试能力 |
| Agent 沙箱与安全 | 容器、gVisor、microVM、网络、密钥 | 执行不可信模型代码 |
| MCP/A2A 互操作 | 工具协议与 Agent 间协议边界 | 对外开放 Anchor 能力 |
| 长期记忆与审计 | 记忆、检索、OTel、证据链 | 长周期研究、写作与监管场景 |

### 核心判断

**第一，双平面方向基本成立，但“两个平面都是事实源”是错误表述。**正确模型应当是：**控制面事件历史决定“发生了什么、为什么发生以及走了哪条决策路径”；内容面仅对某个已经被冻结且由不可变 revision 标识的字节树负责。**可变 workspace HEAD、正在运行的 VM、缓存、向量索引、Agent memory 都不能成为恢复真相。这个设计与 Temporal 的确定性工作流/Activity 分离、DBOS 的持久 checkpoint、OpenAI Sandbox Agent 对“outer runtime 与 sandbox session”职责的拆分具有高度一致性。citeturn2search4turn1search16turn11search5turn12search13

**第二，本轮检索未发现一个公开系统已经完整、明确地同时保证“持久编排 + 版本化可变工作区 + 控制/内容原子边界 + 端到端审计 + 可验证完成”。**这不是“行业不存在”的数学证明，而是截至 2026 年 9 月 9 日对公开一手资料的检索结论。最接近的三个设计分别是：OpenAI Agents SDK Sandbox Agent + Temporal、Cursor Cloud Agents + Temporal + 独立 VM，以及 Mastra Workspaces + Temporal；三者都非常接近 Anchor 目标，但公开资料均没有给出与 Anchor I2/I3/I9 同等严格的“事件提交与不可变 workspace revision 一致性协议”。citeturn12search13turn11search5turn2search5turn20search2turn20search10

**第三，Anchor 不应把“工作区”直接定义成共享可变目录。**生产级编码 Agent 的公开设计更常见的是“任务/Agent 独立环境 + Git/快照 + 明确合并”：Cursor 为云 Agent 使用独立 VM；OpenHands 使用隔离 runtime/workspace；Aider 以 Git commit 记录 AI 编辑；OpenAI Sandbox Agent 则把 sandbox workspace 生命周期独立出来。另一方面，Restate 的 Virtual Object 用“同一 key 单写者 + 自动排队”获得强一致性。这些证据共同支持 Anchor 的默认并发策略应是**每 revision 分叉、每分支单写、合并显式化**，而不是多个 Agent 对一个 POSIX 目录自由并发写。citeturn2search5turn3search4turn4search17turn11search3turn1search11

**第四，建议直接修订 I2，同时补充 I9 的语义。**LLM 调用本质上是非确定性的；Temporal、DBOS 等持久执行系统解决的是“重放已经发生的非确定性结果”，不是保证重新向模型请求后生成同样 token。因此 Anchor 可以承诺“法证级 replay”，不应承诺“fresh recomputation 必然得到同样模型输出”。Temporal 将非确定性 I/O 放到 Activities 并通过历史重放结果；DBOS 将非确定操作放入 checkpointed steps，正说明这一边界。citeturn2search4turn1search16turn21search7

建议的 **I2 精确修订文本**为：

> **I2 — Canonical Recovery State / 规范恢复闭包：**控制面的不可变事件历史是执行状态、决策、授权、副作用意图及状态迁移的唯一规范事实源；内容面的某个 revision 仅对被已提交事件通过不可变 `content_ref` 明确引用的内容字节负责。一个 run 的完整恢复源是“事件历史 + 其中引用且完整性已验证的不可变内容 revisions”的闭包。可变 workspace HEAD、活动 sandbox/VM 状态、缓存、向量索引、摘要与记忆均为非规范投影，不得单独决定恢复结果。任何 NodeRun 只有在其输出 `content_ref` 已持久化、完整性可验证并与控制面完成事件建立提交关系后，才能进入 Completed。

建议同时将 **I9** 精确化为：

> **I9 — 每个 run 可法证重放：**给定图版本、声明式输入、运行时/工具/策略版本、被记录的非确定性调用结果及不可变内容引用，可以重放出相同的状态转换和决策路径。重新调用外部模型/API 的 fresh recomputation 属于另一种复现实验，不承诺逐 token 或逐响应一致。

这一修改并没有削弱可复现性，反而把“历史重放”和“重新计算”两个过去容易混淆的概念拆开了。citeturn2search4turn1search16turn8search1

**最终架构建议是 Keep Anchor, not replace Anchor。**Anchor 现有 I1、I3、I4、I8 比多数通用 Agent 框架更严格；完全迁移到 Temporal、LangGraph 或 Microsoft Agent Framework，并不会自动得到 operation ledger、确定性 verifier、声明式输入隔离或版本化内容平面，反而需要在其上重新实现这些能力。更优路线是保留 Anchor 的控制面语义，**采用 durable-engine 的恢复模式、sandbox 生态、Git/快照机制、MCP/A2A 与 OTel 标准作为组件与接口层**。citeturn2search4turn8search4turn21search2turn17search0


## 研究方法、证据质量与行业分层地图

本轮检索以 2024–2026 年资料为重点，并回溯对持久工作流与虚拟化技术仍有解释价值的早期资料。检索日期为 **2026 年 9 月 9 日**。技术结论优先使用官方规范、项目文档、源码仓库、厂商的架构文档以及法律法规原文；政策部分优先使用欧盟委员会、EUR-Lex、中国国家网信办等政府来源。对厂商公布的性能或采用数字均明确标注为“厂商/基金会披露”，不将其视作独立基准。

本报告的证据分级沿用任务书：**高**表示有正式规范/源码/法规以及交叉证据；**中**表示有一手实现或官方说明，但缺少足够独立生产验证；**低**表示公开设计尚处于 Beta、Preview，或关键语义仅由厂商描述。对于“未发现某能力”的结论，仅表示本轮公开资料中没有足够证据，不推断闭源系统内部一定没有该能力。

检索策略围绕五组关键词展开：`durable execution / deterministic replay / checkpoint / journal`；`agent sandbox / workspace / VM / snapshot / git worktree`；`MCP / A2A / task / artifact / state`；`agent memory / temporal knowledge graph / shared memory`；`GenAI semantic conventions / AI Act logging / AI-generated content logs`。这一策略刻意寻找“恢复语义”和“数据所有权”，而不是依赖产品营销中的“durable”“production ready”等标签。

**截至 2026 年最重要的量化信号如下。**

| 指标 | 最新公开值 | 年份 | 解读 | 来源 |
|---|---:|---:|---|---|
| MCP 已发布 server 数 | 超过 10,000 | 2025 | 互操作生态已明显跨过实验期；由 Linux Foundation AAIF 披露 | citeturn13search1 |
| AGENTS.md 已采用开源项目 | 超过 60,000 | 2025 | “仓库内声明 Agent 指令”正在成为轻量约定 | citeturn13search1 |
| A2A 支持组织 | 超过 150 | 2026 | A2A 已形成跨厂商治理与生产采用势头；基金会披露 | citeturn13search2 |
| A2A 核心协议 | 1.0 于 2026-03 发布；JS SDK 1.0 GA 于 2026-07 | 2026 | 协议开始由快速演化阶段进入稳定化 | citeturn15search0turn15search1 |
| Firecracker microVM 官方启动指标 | <125 ms | 当前官方资料 | 可支持较细粒度 microVM；是实验室/厂商指标，不等于 Anchor 实际冷启动 | citeturn5search1 |
| Firecracker 官方额外内存指标 | <5 MiB / microVM | 当前官方资料 | 说明 microVM 与传统 VM 成本差距可显著缩小 | citeturn5search1 |
| 中国生成式 AI 产品备案量 | 300 余款 | 2025 | 网信办网站刊载专家解读中的行业规模信号 | citeturn18search6 |
| 中国生成合成服务 | 3,000 余个 | 2025 | 内容治理已覆盖较大服务规模 | citeturn18search6 |
| EU 高风险 AI 日志最低保存期 | 至少 6 个月，除非其他法律另有规定 | AI Act | 对未来高风险 Agent 审计架构具有直接参考价值 | citeturn16search12 |

### 分层地图

下面的地图反映本报告建议的技术分层。关键点是：**OTel、MCP、A2A、Memory 都不能进入 Canonical Recovery State；它们必须围绕控制面和不可变内容引用工作。**

```mermaid
flowchart TB
    U[用户 / API / 外部 Agent]

    subgraph Interop[互操作层]
        MCP[MCP<br/>Agent ↔ Tool/Data]
        A2A[A2A<br/>Agent ↔ Agent]
        AGENTS[AGENTS.md<br/>Repo Instructions]
    end

    subgraph CP[Anchor 控制面：Canonical Execution Truth]
        GV[Immutable Graph Version]
        RUN[Run / NodeRun State]
        EV[Append-only Event History]
        OP[Operation Ledger]
        VER[Verifier / Approval / Policy]
        SCH[Lease / Scheduler / Watchdog]
    end

    subgraph Boundary[规范内容边界]
        REF[content_ref<br/>artifact://digest<br/>workspace://id@revision/path]
        COMMIT[Content Commit / Reconciler]
    end

    subgraph Content[内容面]
        WM[Workspace Manager]
        SB[Sandbox Session]
        MUT[Mutable Working Tree<br/>非规范]
        REV[Immutable Workspace Revision]
        ART[Content-addressed Artifacts]
        CACHE[Dependency / Retrieval Cache<br/>非规范]
    end

    subgraph Projection[投影与可观测]
        MEM[Memory / Vector / KG<br/>可重建投影]
        OTEL[OpenTelemetry GenAI<br/>Trace / Metrics]
    end

    U --> Interop
    Interop --> CP
    GV --> RUN
    EV --> RUN
    RUN --> SCH
    RUN --> VER
    RUN --> OP
    RUN --> REF
    REF --> WM
    WM --> SB
    SB --> MUT
    MUT --> COMMIT
    COMMIT --> REV
    COMMIT --> ART
    REV --> REF
    ART --> REF
    CP --> MEM
    CP --> OTEL
    Content --> OTEL
    CACHE --> SB
```

这一分层与多个正在形成的行业模式相吻合：Temporal/PydanticAI 将确定性控制流程和非确定性工具/模型调用分开；OpenAI Sandbox Agent 明确把 approvals、tracing、handoffs、resume bookkeeping 留在外层 runtime，把命令、文件和环境隔离交给 sandbox session；MCP 则只负责模型应用与工具/数据服务的协议化连接。citeturn21search7turn11search5turn14search11

MCP 与 A2A 的治理在 2025–2026 年发生了重要变化。A2A 于 2025 年转入 Linux Foundation；MCP、goose、AGENTS.md 于 2025 年 12 月成为新成立 Agentic AI Foundation 的首批项目。因而 Anchor 现在可以把 MCP/A2A 视为比 2024 年更可信的供应商中立互操作边界，但不能因此把它们当作执行状态协议。citeturn13search0turn13search1

OpenTelemetry 的 GenAI 语义约定已经涵盖 agent、workflow、tool、model、token、retrieval 等属性和 `invoke_agent`、`execute_tool`、`invoke_workflow` 等操作名，但相关 GenAI conventions 截至当前仍处于活跃演进中，部分属性已从主 semantic-conventions 仓库迁至独立 GenAI 约定。因此它适合做**可观测标准出口**，不适合作为 Anchor 的审计事实源。citeturn17search0turn17search4


## 控制面、工作区、沙箱与关键内容边界

**RQ1 — 持久化 Agent 编排。置信度：高。**

行业已经形成三种主要恢复模型，而且它们解决的是不同问题。

| 系统 | 核心恢复模型 | 非确定性处理 | 长等待/HITL | Anchor 可借鉴部分 | 证据 |
|---|---|---|---|---|---|
| **Temporal** | 事件历史 + deterministic workflow replay | I/O、模型、工具应置于 Activity | 原生 durable workflow / signal/update 模式 | History replay、workflow/activity 分离、版本迁移纪律 | citeturn2search4turn12search13 |
| **DBOS** | PostgreSQL checkpoint：保存 workflow 输入与 step 输出，恢复时重新运行 deterministic workflow | 非确定性工作封装成 step | 长工作流可恢复 | “控制状态与业务 DB 元数据原子提交”思想尤其重要 | citeturn1search16turn1search9 |
| **Restate** | Durable execution journal + durable state | LLM、工具、状态变化进入 journal | 支持 durable sessions / object state | Virtual Object 单写者、线性一致状态 | citeturn1search3turn1search11turn1search19 |
| **Inngest** | step 结果持久化和 memoize，每 step 可独立重试 | 非确定逻辑应放 `step.run` | `waitForEvent` 可悬停且不占进程 | step checkpoint 与无 worker 等待 | citeturn0search2turn0search7 |
| **LangGraph** | 每 superstep checkpoint，thread persistence | 节点/状态恢复；interrupt 后节点从开头重执行 | Interrupt 可无限期等待 | checkpoint、time travel、pending writes | citeturn8search4turn8search1 |
| **Microsoft Agent Framework** | superstep 末 checkpoint 全执行状态 | executor/message/state 快照 | checkpoint 支持 pause/resume | 图验证、BSP/superstep barrier、checkpoint | citeturn9search6turn21search2 |
| **CrewAI Flows** | flow-state snapshot/persistence | 应用层管理 | 支持持久 flow 与 fork | 简洁 flow persistence，不宜替代 Anchor event semantics | citeturn21search3 |
| **Mastra** | 本地 snapshot；2026 可映射到 Temporal | Temporal 模式下 step→Activity | suspend/resume | 可作为“框架 + durable engine”典型 | citeturn20search1turn20search2 |
| **PydanticAI** | 原生 Temporal durability capability | model/tool/MCP 调用路由为 Temporal Activity | 继承 Temporal durable semantics | 证明“Agent 框架不用自己发明 durable runtime” | citeturn21search7 |

Temporal 与 Anchor 的理念最接近之处，不是 API，而是**“决定执行路径的代码必须可重放，非确定性结果必须被历史化”**。DBOS 又提供了另一个关键启示：如果应用业务状态与执行 checkpoint 在同一个数据库事务域内，可以得到非常强的崩溃一致性；Anchor 虽不能把大型工作区字节直接放入控制数据库，但可以把这一思想应用到“workspace revision 已准备完成”与 `NodeCompleted` 元数据的提交协议中。citeturn2search4turn1search9

Anchor 自研语义仍有明显价值。Temporal、LangGraph、CrewAI 等不会自动强制 I1 图不可变、I3 operation ledger、I4 verifier-gated completion 或 I8 声明式输入可见性；这些属于应用/平台语义而不是通用 durable engine 的职责。因此，“用 Temporal 替换 Anchor”与“让 Anchor 借鉴或接入 Temporal”是两个完全不同的决策，后者明显更合理。citeturn2search4turn8search4turn21search3

**RQ2 — 编码 Agent 工作区。置信度：高。**

公开产品的趋向不是所有 Agent 共享一个永久目录，而是隔离任务执行环境、再通过 Git、快照或显式状态恢复连接不同阶段。

| 实现 | 工作区组织 | 版本/恢复机制 | 并发含义 | Anchor 启示 |
|---|---|---|---|---|
| **Cursor Cloud Agents** | 云端 Agent 各自运行在独立 VM | 任务级环境；Temporal 编排 | 天然避免多个 Agent 写同一个机器目录 | 强支持“一 Agent/任务一 workspace fork” citeturn2search5turn4search15 |
| **OpenAI Sandbox Agent** | persistent sandbox workspace，可执行命令、修改文件、产出 artifacts | `SandboxSessionState` 可保存/恢复；provider 可重新水合 | Session 是明确资源边界 | 与 Anchor 双平面最接近 citeturn11search0turn11search4turn11search5 |
| **OpenHands** | Docker runtime 中挂载 `/workspace` | conversation fork 可复制事件和 workspace metadata | runtime 隔离 | 适合作为 coding sandbox UX 参考，不是 canonical-state 参考 citeturn3search4turn3search18 |
| **SWE-agent** | 在 sandbox 中按 base commit clone repo | 可 reset；task image 可缓存 | 每 task 独立执行 | base revision + task image 是很好的声明式启动参数 citeturn3search22turn3search1 |
| **Aider** | 本地 Git repo | AI 文件修改自动 commit，可 `/undo` | Git branch/commit 提供冲突边界 | “AI 写操作 → revision”纪律值得 Adapt citeturn4search17 |
| **Claude Code** | 用户本机项目目录，权限系统约束编辑/Bash | 依赖 Git 用户流程 | 本身不构成强隔离边界 | 适合 IDE/CLI 权限 UX，不宜作为服务端安全模型 citeturn4search7turn4search1 |
| **AgentScope Runtime/2.0** | Docker/gVisor/BoxLite/K8s 等 sandbox | runtime 管理 sandbox 生命周期与状态服务 | 支持异步/并发 sandbox | 中国生态中较接近“runtime + sandbox”组合 citeturn22search1turn22search6 |
| **Dify AgentBox/Sandbox** | 预构建多语言 Docker 运行环境 | 容器化代码执行 | 主要解决执行环境，而非 revision semantics | 可参考镜像/依赖打包，不作为内容事实源 citeturn22search2turn22search9 |

这里最重要的架构结论是：**工作区实例与工作区 revision 必须分离。**实例是可变且短暂的运行资源；revision 是不可变、可寻址、可验证的逻辑内容版本。OpenAI Sandbox Agent 已经明确区分“manifest 描述初始内容”和“live sandbox state”，Fly 也明确指出内存/VM suspend snapshot 会因部署或代码变化失效，且应用仍必须能够冷启动。这些事实反对把活动 VM snapshot 当作唯一恢复真相。citeturn11search5turn6search10

**RQ3 — 沙箱与隔离。置信度：高。**

| 隔离级别 | 安全/兼容特点 | 典型实现 | Anchor 建议 |
|---|---|---|---|
| 进程 / bubblewrap | 成本最低；依赖宿主内核与配置正确性 | Anchor 现状、process sandbox | 保留给可信低风险工具，不应成为任意代码默认边界 |
| OCI 容器 | 生态最好、依赖缓存方便；共享宿主 kernel | Docker、OpenHands | 普通开发任务的成本优先档 |
| 用户态 kernel | 在 OCI 接口下增加 syscall 隔离；兼容性有成本 | gVisor | 高风险、多租户 Linux workload 的中间档；gVisor 官方明确其与普通容器相比增加隔离层，并可能影响 syscall-heavy 性能。citeturn5search0turn5search10 |
| 轻量 VM | 每 sandbox 独立 kernel，隔离更强 | Firecracker | 推荐作为互联网可访问、执行未知代码的高风险档；官方公布启动 <125 ms、额外内存 <5 MiB，但必须以 Anchor 自身 benchmark 验证。citeturn5search1 |
| Kata | OCI/Kubernetes UX + VM 隔离 | Kata Containers | 若已有 K8s，大规模强隔离的可选方案。citeturn5search3 |
| 完整 VM | 最成熟隔离，资源与启动成本更高 | 云 VM | 特殊合规、GUI、复杂 kernel workload |

商业 sandbox 服务已经把“session persistence、snapshot、fork、egress policy”做成产品能力。E2B 使用 Firecracker microVM 并提供暂停/恢复；Daytona 提供持久 sandbox、snapshot 和 fork，并支持域名级 firewall/出站代理；Modal 支持 filesystem、directory 和 memory snapshots，但不同快照类型具有不同生命周期；这意味着 Anchor 很适合抽象一个 `SandboxProvider`，而不应该将核心语义绑定到 E2B、Daytona 或任一云实现。citeturn5search2turn5search4turn6search1turn6search13turn6search3turn6search2

网络策略上，**默认拒绝出站 + 域名/服务级 allowlist + 代理层审计**明显优于直接给 sandbox 完整互联网。OpenAI 对 Codex 的生产安全说明采用受管理的网络策略、网络代理和对未知域名的批准流程；Daytona 也把 domain allowlist、block-all 和 outbound proxy 作为 sandbox firewall 能力。这构成了两个独立实现对同一模式的支持。citeturn7view0turn6search13

密钥则应采用“**reference，不是 value**”原则。OpenAI Sandbox Agent 的环境配置可把秘密值标记为 ephemeral，`EnvValueReference` 只持久化查找元数据；OpenAI 内部 Codex 环境也把 OAuth 凭据置于操作系统凭据存储并实施 workspace-bound authentication。Anchor 因而应让 Agent 得到一个能力句柄或代理凭据，而不是长期 API key；所有外部访问在 operation ledger 中记录 capability、目标、授权和返回状态。citeturn11search5turn7view0

**RQ4 — 控制面与内容面的边界。置信度：高，是本报告最关键结论。**

本轮公开资料中最接近 Anchor 目标的三个参考设计如下：

| 排名 | 设计 | 已做到什么 | 与 Anchor 的核心差距 |
|---|---|---|---|
| **最接近：OpenAI Agents SDK Sandbox Agent + Temporal** | 外层 runtime 管 approvals、tracing、handoffs、resume；sandbox 管命令、文件、环境；Temporal integration 将 sandbox create/read/write/command 等封装为 Activity，worker 重启后仍可恢复 | **没有公开定义 `NodeCompleted ↔ immutable workspace revision` 的原子提交协议；Sandbox API 仍是 session-centric，不是 content-revision-centric**。当前相关集成还有 Beta/Preview 属性。citeturn11search5turn12search13turn12search8 |
| **Cursor Cloud Agents + Temporal + 每 Agent VM** | 经 Cursor 工程师公开介绍，云 Agent 每个运行在独立 VM，由 Temporal workflow 编排 | 强生产信号；但公开材料没有披露内容寻址、revision 生命周期和完整审计闭包。citeturn2search5 |
| **Mastra Workspaces + Temporal** | Mastra 已支持 persistent workflow snapshot、Temporal backend，并在 2026 年加入远程 filesystem/workspace 能力 | 已在一个框架内出现“durable orchestration + mutable workspace”，但公开语义没有达到 Anchor 对 event/revision 原子边界的要求。citeturn20search1turn20search2turn20search10 |

DBOS 虽然不是最接近的 workspace 产品，却提供了**最值得 Anchor 借鉴的提交语义**：当应用业务数据库事务和 durable-execution 记录位于同一 PostgreSQL 事务中时，可以把两者一起提交，从而避免“一边完成、一边没记录”的窗口。Anchor 的内容 blob/repo 很可能不能和事件数据库直接跨存储原子提交，因此应把 DBOS 的思想改造成 **prepare → durable content → control commit → reconciliation** 协议。citeturn1search9turn1search2

推荐的内容引用格式应进一步收紧为：

```text
artifact://sha256:<digest>

workspace://<workspace-id>@<immutable-revision>/<path>
```

其中 `<immutable-revision>` 可以是 Git commit、Merkle tree root、内容快照 digest 或内部不可变 snapshot ID，但**不能是 `main`、`latest`、branch HEAD、sandbox-id 或任何随时间改变的名字**。一个 `content_ref` 一旦进入事件历史，其解析结果必须永远不漂移。

推荐的 Node 提交协议是：

```text
Declared input content_refs
        ↓
Resolve immutable revisions
        ↓
Fork writable sandbox/worktree
        ↓
Agent/tool execution
        ↓
Deterministic/domain verifier
        ↓
Freeze working tree
        ↓
Create immutable revision + digest
        ↓
Durability/read-after-write verification
        ↓
Append NodeCommitted(
   input_refs,
   output_refs,
   operation_ids,
   verifier_result,
   runtime/tool/policy versions
)
        ↓
Advance graph
```

如果在“内容已冻结、控制事件尚未提交”之间崩溃，该 revision 是**孤儿 prepared revision**，可由 reconciler 回收或重新关联；如果 `NodeCommitted` 已写入但 revision 无法读取，则系统必须进入 `INCONSISTENT_CONTENT` / reconciliation 状态，**绝不能重新从当前 mutable workspace 猜测内容然后继续执行**。这与 durable systems 将完成边界建立在可持久化结果之上的原则一致。citeturn1search16turn12search13


## 共享状态、协议、记忆、审计、可靠性与框架比较

**RQ5 — 共享可变状态与并发。置信度：高。**

对于 Anchor，不同共享状态模式的适用性非常不同。

| 模式 | 冲突语义 | 确定性 | Anchor 判断 |
|---|---|---|---|
| 不可变消息/产物 | 无原地写冲突 | 最高 | **Adopt，默认数据流** |
| 每 Agent Git branch/worktree | merge 时显式冲突 | 高 | **Adopt，代码内容默认模式** |
| 单写者 workspace | 同一 revision lineage 串行写 | 高 | **Adopt** |
| 共享事务 KV | 由数据库事务/锁控制 | 中高 | Adapt，仅用于结构化 coordination |
| CRDT | 自动合并可交换更新 | 取决于数据类型 | Adapt，仅适合明确 CRDT 数据，不适合任意源码树 |
| Blackboard / 全局 mutable state | 多 Agent 任意读写 | 低 | **Avoid 作为 canonical state** |
| 共享 POSIX 工作目录 | 文件级竞争、工具副作用难隔离 | 低 | **Avoid 作为默认并发模型** |

Restate Virtual Objects 提供了很好的结构化参照：同一 key 上的写 handler 被串行化，状态与执行日志由同一系统协调，从而得到线性一致的单写语义。Git/Aider 则展示了另一种适用于源代码的策略：把修改变成 revision，再通过明确的版本历史进行 undo/merge。两类系统共同支持“**冲突应被建模，而不是让 POSIX 最后写入者获胜**”。citeturn1search11turn4search17

共享缓存可以存在，但只能是性能投影。依赖镜像、包下载、Git object、编译缓存适合以 `(runtime image digest, lockfile digest, arch, toolchain version)` 等键隔离；语义检索缓存则必须包含模型/embedding/index/schema 版本。SWE-agent 对重复任务缓存环境镜像，说明依赖环境缓存具有明显工程价值，但缓存命中不能改变 canonical input。citeturn3search1

**RQ6 — MCP、A2A 与其他互操作。置信度：高。**

| 协议 | 真正解决的问题 | 明确不解决的问题 | 对 Anchor 的角色 |
|---|---|---|---|
| **MCP** | LLM host/client 与 tools、resources、外部服务之间的标准接口；当前标准 transport 包括 stdio 与 Streamable HTTP | 不定义 Graph、workspace consistency、operation ledger、全局 Agent 调度 | Anchor 暴露 Tool/Artifact/Search/Approval capability 的首选协议 citeturn14search4turn14search11 |
| **A2A** | 独立 Agent 的 discovery、message/task/artifact 和跨系统协作 | 不定义共享内存一致性，也不要求两个 Agent 共用 filesystem | 将一个 Anchor Run/Agent 作为远程 Agent capability 暴露 citeturn14search12turn13search0 |
| **AGENTS.md** | 仓库内向 coding agents 提供项目级操作指令 | 不提供执行、状态、权限或持久性 | 作为 workspace revision 中的声明式 repo instruction 输入 citeturn13search1 |
| OTel GenAI | 跨框架 trace/metric attribute | 不负责恢复和授权 | 标准化可观测出口 citeturn17search0 |

MCP 在 2026-07 规范中进一步采用 stateless-first 思想：请求尽量自包含，无法完全无状态时优先携带 state reference，而非依赖隐含长连接状态。这与 Anchor 的 I8“声明式输入”实际上高度相容。MCP 的演化反而强化了“传引用，不分享隐式全局状态”的方向。citeturn14search3turn14search4turn14search10

A2A 已经定义 task/status/artifact 等跨 Agent 语义，但它们是**协议对象**，不是分布式事务或共享工作区协议。Anchor 不应把 `A2A task id` 等价于内部 `Run id`，而应通过一个 adapter 保留内部事件、operation ID 和 content revision，再投影成 A2A task 状态。citeturn14search12turn15search0

**RQ7 — 长期上下文与记忆。置信度：高。**

Letta 的 memory block 可以持久化、动态挂载，甚至被多个 Agent 共享；Zep 则把事实放入 temporal knowledge graph，在新事实到来时使旧事实失效但保留历史；Mem0 也可以同时维护 embedding 与图结构。这些都说明长期 Agent memory 正从“聊天记录”走向结构化、可更新知识层。citeturn19search6turn19search16turn19search3turn19search18

但这恰好说明 Anchor **不能**把 memory 当 Canonical State。Letta 的共享 block 允许一个 Agent 修改后其他 Agent 立刻看到，这对个性化 Agent 很方便，却直接违反 Anchor I8 的默认“节点只能看到声明输入”；Zep/Graphiti 的事实提取又包含模型生成与自动失效逻辑，也不适合作为不可争议的事实源。Anchor 应让 memory store 存储 `source_event_ids`、`source_content_refs`、抽取器版本和有效期，并把每次节点实际检索到的 memory slice 固化进该节点输入快照。citeturn19search12turn19search3turn19search10

因此推荐的数据层次是：

```text
Canonical:
  Event history
  Immutable content revisions
  Operation ledger
  Verification decisions

Derived:
  Conversation summary
  Long-term memory
  Vector index
  Temporal knowledge graph
  Search cache

Ephemeral:
  Model context window
  Sandbox mutable tree
  Shell process state
  VM memory
```

**RQ8 — 可观测性与审计。置信度：高。**

事件溯源、审计日志和分布式 tracing 应严格区分。

| 数据 | 目的 | 是否 Canonical | 是否允许采样/丢失 |
|---|---|---:|---:|
| Anchor event history | 恢复和状态机 | **是** | 否 |
| Operation ledger | 副作用授权、幂等、对账 | **是** | 否 |
| Content revision manifest | 内容完整性 | **是** | 否 |
| Audit evidence record | 合规调查 | 建议与 canonical event 建稳定引用 | 原则上否 |
| OTel spans/events | 性能、调试、跨服务关联 | 否 | 可以按策略采样 |
| Prompt/tool content telemetry | 深度诊断 | 否 | 必须考虑 PII/secret 过滤 |

OpenTelemetry 已提供 agent、workflow、tool、retrieval、token 等 GenAI 属性，但官方文档同时警告 prompt、tool arguments/results、retrieved content 可能包含敏感数据。OpenAI 对内部 Codex 的设计也把 prompt、tool approval、tool output、MCP usage、network allow/deny 等导出到 OTel/安全系统。最佳实践因此不是“把所有事件都存 OTel”，而是 **canonical audit → 可选择性投影 OTel**。citeturn17search0turn7view0

监管趋势支持 Anchor 提前建设这一层。EU AI Act Article 12 要求高风险 AI 系统在其生命周期内具备自动事件日志能力；相关 provider/deployer 在适用情形下要保存自动日志，法规基准最低为六个月。2026 年 8 月起欧盟 AI Act 的一批执行和透明度规则已经开始实施，不过 Annex III 高风险系统的具体适用日期因 2026 年调整延后至 2027 年 12 月，嵌入受监管产品的高风险系统则延至 2028 年。citeturn16search12turn16search0turn16search5

中国目前更明确的强制要求集中在生成式 AI 服务、安全治理和生成合成内容标识。《人工智能生成合成内容标识办法》及配套强制标准自 **2025 年 9 月 1 日**实施；在特定情况下，服务向用户提供未加显式标识的生成内容前，需要依法留存相关日志。2025 年 11 月网信部门已公开查处一批没有落实显式/隐式标识要求的应用。因此对面向中国公众提供生成式服务的 Anchor 产品层，内容 provenance、日志与输出标识不能被当成远期能力。citeturn18search0turn18search3turn18search10

**RQ9 — 可靠性与成本。置信度：高。**

最重要的设计原则是把三类重试分开：

```text
业务重试
Agent 判断“方案不够好，再尝试另一条路径”
        ↓
节点重试
runtime 判断 tool/model activity 暂时失败
        ↓
请求重试
HTTP/RPC SDK 对瞬时网络错误 retry
```

这三层不能共用一个 retry counter。否则模型可能在 HTTP retry 已发生副作用后重新进入业务循环，导致重复发送邮件、重复付款或重复 PR。Temporal 把 Activity retry 与 Workflow 决策分开，Inngest 也区分 step durability 和请求失败；Anchor 应进一步通过 operation ledger 把“是否允许重试”绑定到 operation ID。citeturn2search4turn0search4

人类介入也应分层：**Approval** 是副作用前授权；**Correction** 是给状态机新的声明输入；**Takeover** 是终止/冻结 Agent 自主权并将 workspace 交给人；**Reconciliation** 是处理未知外部结果。LangGraph interrupts、Prefect pause/suspend、OpenAI Agents RunState 等都证明持久 HITL 不需要占用 worker。citeturn8search1turn1search10turn10search2

成本控制不应破坏 I6。推荐把 token、时间、API 花费当作**调度信号和升级阈值**，而不是简单“达到数字就杀掉 Agent”。真正的安全中止条件应更多关注重复副作用、无进展循环、连续相同工具调用、workspace digest 长时间不变化、验证指标不提升，以及模型/工具持续失败；这一点与 Anchor 现有自适应 watchdog 方向相容。模型路由、cache、context pruning 则用于减少正常运行成本；Mastra 2026 的 token-pruning 修复也反映长期 multi-step loop 中上下文膨胀是真实工程问题。citeturn20search17

**RQ10 — Agent 编排框架分层比较。**

| 候选 | 本质层级 | 编排模型 | 持久性 | HITL/审计能力 | Anchor 判断 |
|---|---|---|---|---|---|
| **LangGraph** | Agent/workflow framework | state graph / supersteps | checkpointer 强 | interrupts、time travel、trace 生态 | **Adapt**：成熟 checkpoint；但 shared graph state 与 I8 有张力。citeturn8search4turn8search1 |
| **AutoGen / AG2** | multi-agent library/framework | conversation/team | `save_state/load_state` | 有状态，但保存运行中 team 可能不一致 | **Avoid 作为 durability substrate；Adapt agent patterns**。citeturn8search0turn8search3 |
| **CrewAI** | Agent + Flow framework | crews + event-driven flows | `@persist` state snapshot | flow/HITL 较完整 | **Adapt**，但 mutable flow state 不是 Anchor canonical semantics。citeturn21search3turn21search12 |
| **Microsoft Agent Framework** | Agent + graph workflow framework | modified Pregel/BSP | superstep checkpoint | checkpoint、resume、共享状态 | **Adapt**，图验证和 superstep 很值得借鉴。citeturn9search6turn21search2 |
| **Google ADK** | Agent framework | agent/tool/session 模型 | 依赖 session/runtime；公开资料已支持 session 持久 sandbox code execution | 有 sandbox、评估与观测生态 | **Adapt tools/sandbox**；本轮未找到 Temporal-class replay 证据。citeturn20search16turn20search3 |
| **OpenAI Agents SDK** | 轻量 Agent SDK | agents/handoffs/tools | RunState + external durable integrations | approvals、tracing、Sandbox Agent | **Adapt 强烈推荐**，特别是 Sandbox outer/inner split；不宜绑定供应商语义。citeturn10search0turn10search2turn12search5 |
| **LlamaIndex Workflows** | event-driven workflow library | typed events/steps | 可管理 workflow state；不是完整 durable engine | InputRequired/HumanResponse | **Adapt 事件 UX，不作为 Anchor runtime**。citeturn21search6turn21search8 |
| **Mastra** | TypeScript Agent/workflow framework | graph/workflow | snapshot，且可运行在 Temporal | suspend/resume、tracing | **Adapt；2026 年特别值得跟踪**。citeturn20search1turn20search2 |
| **PydanticAI** | Agent library | Agent + tool loop | 原生 Temporal capability | durable model/tool/MCP | **Adopt 其 integration pattern**，不是替代 Anchor。citeturn21search7 |
| **Temporal AI integrations** | durable runtime substrate | deterministic workflows + Activities | 最强一档 | HITL、history、recovery | **Adapt/可选 backend**，不要让业务不变量退化成 Temporal-specific semantics。citeturn2search4turn12search13 |

整体趋势非常明确：**Agent framework 与 durable engine 正在解耦后重新组合**，而不是每个 Agent 框架继续自己发明完整可靠性层。PydanticAI、Mastra、OpenAI Agents SDK 均已经出现与 Temporal 的正式集成，这为 Anchor 的“供应商中立控制语义 + 可插拔 durable backend”提供了较强证据。citeturn21search7turn20search2turn12search13


## 候选评估矩阵、成熟度与中外案例

以下总表只把**真正进入 Anchor 架构决策短名单的组件**作为“候选”，而不是把任务书中的所有扫描对象强行视为可互换产品。否则把 MCP、Firecracker 与 LangGraph 放在同一个“谁最好”的排名中没有意义。

| 候选 | 层 | 编排模型 | 持久性 | 工作区/内容 | 并发 | 审计 | HITL | 隔离 | 治理/成熟度 | 结论 |
|---|---|---|---|---|---|---|---|---|---|---|
| Temporal | Durable runtime | Workflow/Activity | **强** | 外接 | Workflow deterministic | history 强 | 强 | 外接 | 成熟开源+公司生态 | **Adapt / 可选 backend** citeturn2search4 |
| DBOS | Durable runtime | code workflow/steps | **强** | 外接 | DB transactional | DB history 强 | 应用层 | 外接 | 新一代、文档语义清晰 | **Adapt transaction pattern** citeturn1search16turn1search9 |
| Restate | Durable runtime | journal/functions/objects | **强** | 外接 | Virtual Object 单写 | journal 强 | durable session | 外接 | 活跃 | **Adapt single-writer** citeturn1search3turn1search11 |
| LangGraph | Agent framework | state graph | 强 checkpoint | 无专门版本化 workspace | shared state | 中强 | 强 | 外接 | 生态成熟 | **Adapt** citeturn8search4 |
| Microsoft Agent Framework | Agent/workflow | BSP graph | checkpoint 强 | 外接 | superstep barrier | 中强 | 强 | 外接 | 2026 活跃 | **Adapt** citeturn9search6turn21search2 |
| OpenAI Agents + Temporal | Agent SDK + runtime | Agent outer loop + Temporal | **强** | **Sandbox Agent** | sandbox session | tracing/approval 强 | 强 | 多 provider sandbox | 新能力，部分 Preview | **最重要参考设计之一** citeturn12search13 |
| PydanticAI + Temporal | Agent + runtime | agent run / activities | **强** | 外接 | 由 app/sandbox 管 | Temporal history | 可实现 | 外接 | 活跃 | **Adapt** citeturn21search7 |
| Mastra + Temporal | Agent + runtime | workflow | **强** | Remote FS / workspace | app 管 | traces | 强 | workspace sandbox | 快速发展 | **Adapt/观察** citeturn20search2turn20search10 |
| Cursor Cloud Agents | Coding product | Agent task + Temporal | **强编排证据** | 每 Agent VM | 隔离任务 | 未公开完整 canonical audit | 异步任务 | VM | 商业生产产品 | **参考生产拓扑** citeturn2search5 |
| OpenHands | Coding agent | conversation/event | 中 | Docker workspace | 通常任务隔离 | event/fork | 有 | Docker | 开源生态 | **Adapt UX/runtime** citeturn3search4turn3search18 |
| AgentScope 2.0/Runtime | Agent/runtime | agent runtime | 状态服务 | sandbox/filesystem | async sandbox | logs/traces | 支持 steering | Docker/gVisor/K8s | Apache-2.0，中国生态活跃 | **Adapt；中国优先参考** citeturn22search1turn22search13 |
| Dify | Agent workflow platform | visual workflow | workflow state | sandbox/AgentBox | 平台级 | observability | 平台流程 | Docker sandbox | Dify Open Source License 含附加条件 | **参考平台层，不做内核依赖** citeturn22search0turn22search2 |
| Daytona | Sandbox | session/snapshot | sandbox persistent | **强** | fork | provider logs | N/A | VM/container backend | 商业+SDK | **SandboxProvider 候选** citeturn6search1turn6search13 |
| E2B | Sandbox | sandbox session | pause/resume | **强** | per sandbox | provider | N/A | Firecracker microVM | 商业生态成熟 | **SandboxProvider 候选** citeturn5search2turn5search4 |
| Firecracker | Isolation primitive | N/A | N/A | block/FS 由上层管理 | per microVM | N/A | N/A | microVM | Apache-2.0、成熟底层 | **Adopt via provider/self-host** citeturn5search1 |
| MCP | Protocol | client-server | 非 runtime | resource/tool refs | 不定义 | protocol telemetry | consent 模型 | 不定义 | AAIF/LF | **Adopt interop** citeturn13search1turn14search4 |
| A2A | Protocol | agent task/message | task-level protocol semantics | artifact | 不定义共享 FS | protocol | input/task states | 不定义 | Linux Foundation | **Adopt interop** citeturn13search0turn15search0 |

### 对照 Anchor 不变量

这里仅评估能够承担“系统运行时/控制面”角色的候选；MCP、A2A、Firecracker 等组件若对 I1–I9 打勾没有架构意义。

图例：✅ 原生或高度一致；⚠️ 必须由 Anchor/应用补齐；❌ 核心模型存在直接冲突。

| 候选 | I1 | I2 | I3 | I4 | I5 | I6 | I7 | I8 | I9 | 关键说明 |
|---|---|---|---|---|---|---|---|---|---|---|
| Temporal | ⚠️ | ✅ | ⚠️ | ⚠️ | ✅ | ⚠️ | ✅ | ⚠️ | ✅* | History replay 很强，但 Graph immutability、ledger、verifier、输入可见性属于应用层。citeturn2search4 |
| DBOS | ⚠️ | ✅ | ⚠️/✅ | ⚠️ | ✅ | ⚠️ | ✅ | ⚠️ | ✅* | DB transaction 可非常强；外部副作用仍需幂等。citeturn1search6turn1search9 |
| Restate | ⚠️ | ✅ | ⚠️ | ⚠️ | ✅ | ⚠️ | ✅ | ⚠️ | ✅* | Journal 与 Virtual Object 很强，但不是声明式 DAG runtime。citeturn1search3turn1search11 |
| LangGraph | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ✅ | ⚠️ | ✅ | ❌/⚠️ | ⚠️ | Shared graph state 与 I8 不同；interrupt 前副作用须自己保证幂等。citeturn8search1turn8search4 |
| MS Agent Framework | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ✅ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | checkpoint/superstep 强，但 shared state 与 Anchor 范式不同。citeturn21search2turn9search6 |
| OpenAI Agents + Temporal | ⚠️ | ⚠️/✅ | ⚠️ | ⚠️ | ✅ | ⚠️ | ⚠️ | ⚠️ | ✅* | Sandbox split 极有参考价值；仍需 Anchor 定义 content commit。citeturn12search13turn11search5 |
| PydanticAI + Temporal | ⚠️ | ✅ | ⚠️ | ⚠️ | ✅ | ⚠️ | ✅ | ⚠️ | ✅* | 非确定调用自动 Activity 化，模式优秀。citeturn21search7 |
| Mastra + Temporal | ⚠️ | ⚠️/✅ | ⚠️ | ⚠️ | ✅ | ⚠️ | ⚠️ | ⚠️ | ✅* | workspace 与 durable workflow 已开始汇合，但 canonical boundary 不够严格。citeturn20search2turn20search10 |
| AgentScope 2.0 | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ⚠️ | ✅ | ⚠️ | ⚠️ | Runtime/sandbox/observability 完整度高，但公开 evidence 未显示 event-sourced canonical replay。citeturn22search1turn22search13 |

`✅*` 指**历史法证 replay**，而不是重新请求 LLM 后保证相同响应。

### 成熟度评分

评分为本报告的分析判断，不是厂商评分。1 表示公开证据弱，5 表示公开证据强；“迁移友好”中的 5 表示接入 Anchor 的预期侵入较低。“失败公开性”尤其要谨慎理解：它反映本轮检索到的公开 postmortem/失败材料，而不是产品实际故障率。

| 候选 | 生产使用 | 发布节奏 | 失败公开性 | 文档语义 | 治理清晰 | 迁移友好 |
|---|---:|---:|---:|---:|---:|---:|
| Temporal | 5 | 5 | 3 | 5 | 4 | 3 |
| DBOS | 4 | 5 | 2 | 5 | 4 | 3 |
| Restate | 4 | 5 | 2 | 5 | 4 | 3 |
| LangGraph | 5 | 5 | 2 | 5 | 3 | 2 |
| Microsoft Agent Framework | 3 | 5 | 1 | 4 | 4 | 2 |
| OpenAI Agents + Temporal sandbox | 3 | 5 | 1 | 4 | 3 | 3 |
| PydanticAI + Temporal | 3 | 5 | 1 | 5 | 3 | 3 |
| Mastra + Temporal | 3 | 5 | 1 | 4 | 3 | 3 |
| OpenHands | 4 | 4 | 2 | 4 | 3 | 3 |
| AgentScope | 3 | 5 | 1 | 4 | 3 | 3 |
| Daytona | 3 | 5 | 1 | 4 | 3 | 4 |

Temporal 具有公开的大规模使用信号，并且 Cursor 已公开说明将其用于云 Agent 编排；而 OpenAI Agents + Temporal sandbox、Mastra + Temporal 等能力明显更新，虽然方向非常重要，但成熟度不能与运行多年的通用 durable engine 等同。citeturn2search5turn12search8turn20search2

### 代表性案例

**国际案例：Cursor Cloud Agents。**Cursor 的公开工程分享称，其云 Agent 体系由每 Agent 一个 VM 和 Temporal workflow 进行编排。这是本研究最重要的生产证据之一：它说明“持久编排 + per-agent isolated workspace”不仅是理论模式，已经进入主流 coding-agent 产品。但公开资料没有披露每个文件 revision 如何被控制面原子引用，因此不能据此推断 Cursor 已满足 Anchor I2/I9。citeturn2search5

**国际案例：OpenAI 内部 Codex 安全部署。**OpenAI 公开描述的 Codex 企业内部部署采用 sandbox 与 approvals，控制可写路径、网络与受保护资源；网络访问通过受管理策略、代理和域名许可控制，Agent 行为通过 OTel 记录 prompt、tool approvals、outputs、MCP 使用和 network allow/deny 等信号。它为 Anchor 的“sandbox + capability approval + egress broker + telemetry”提供了非常完整的安全参考。citeturn7view0

**中国案例：AgentScope。**AgentScope Runtime 在 2025–2026 年持续发展出 tool sandbox、Agent-as-a-Service、状态管理、sandbox 生命周期和 observability，并支持 Docker、可选 gVisor、Kubernetes 等后端；其 Runtime 能力随后被整合进 AgentScope 2.0。它说明国内生态同样在向“框架之外增加 production runtime 与 sandbox”演进。公开资料目前仍未展示 Anchor 所需的“immutable workspace revision ↔ event commit”语义，因此建议作为内容面和部署层参考，而不是控制事实源。citeturn22search1turn22search6turn22search13

**中国生态案例：Dify。**Dify 已形成 Agentic workflow、RAG、自托管平台和独立的多语言 AgentBox/Sandbox 执行组件，显示低代码 Agent 平台也在把代码执行独立成安全 runtime。但其公开架构更偏应用平台与工作流，不提供 Anchor 所要求的 event-sourced content revision 模型；此外其主仓库使用基于 Apache 2.0、带附加条件的 Dify Open Source License，因此如果 Anchor 直接复用其核心组件，需要单独进行许可证审查。citeturn22search0turn22search2


## 参考架构、I2 修订与 Adopt / Adapt / Avoid

### 推荐的 Anchor 双平面参考架构

Anchor 应保留现有 Graph → Run → NodeRun 作为**逻辑执行模型**，新增 Workspace/Content subsystem，但不要让节点直接获得“workspace_id 后随便读”。节点输入仍应由 graph edge 声明，区别只是值类型增加 `content_ref`。

推荐的数据模型可以概括为：

```text
GraphVersion
  graph_digest
  node_schema_versions
  policy_version

Run
  graph_version
  initial_inputs
  runtime_bundle_digest

NodeRun
  declared_input_snapshot
  input_content_refs[]
  model_request/result refs
  tool_calls[]
  operation_ids[]
  verifier_result
  output_content_refs[]
  workspace_base_revision
  workspace_output_revision

WorkspaceRevision
  workspace_id
  parent_revision[]
  tree_digest
  manifest_digest
  runtime_image_digest
  lockfile_digests[]
  created_by_node_run
  created_at

SandboxSession        # 非 canonical
  provider
  sandbox_id
  hydrated_from_revision
  mutable_head
  lease
  network_policy
```

这种设计允许“知识工作”和“代码工作”共用一个内容引用系统：研究报告的 PDF、JSON 和数据文件通常走 `artifact://sha256…`；代码仓库和目录树走 `workspace://…@revision/path`。二者都满足“event 中只保存稳定引用，内容存储自己负责 bytes”的原则。

**工作区不应成为第二个独立真相源。**更准确的概念是“**Recovery Closure**”：控制事件决定哪些内容 revision 属于 run；内容存储决定这些 revision 的 bytes。没有事件引用的 workspace revision 对执行语义而言只是 orphan；有事件引用却缺失 bytes 则是系统完整性故障。这样既承认“不可能把大型 workspace 全放进事件日志”，又不放弃 I2 的核心精神。

### Adopt / Adapt / Avoid

| 类别 | 决策 | 理由 | 主要代价 |
|---|---|---|---|
| **Adopt** | Immutable `content_ref` | Git SHA、digest、immutable snapshot 都能稳定绑定事件与内容 | 需要 revision store 与 GC |
| **Adopt** | 每任务/分支独立 writable workspace | Cursor、coding agent 实践与单写者模型均支持 | 更多磁盘/clone/fork 开销 citeturn2search5turn1search11 |
| **Adopt** | freeze-before-complete | Node complete 前先得到 durable revision | 提交延迟增加 |
| **Adopt** | 默认拒绝网络 + egress broker | OpenAI 与 Daytona 均使用受控出站模式 | 维护 allowlist 与代理 citeturn7view0turn6search13 |
| **Adopt** | Secret reference / capability token | 防止 secret 被 prompt、workspace snapshot 或 trace 持久化 | 需要 secret broker citeturn11search5 |
| **Adopt** | MCP / A2A 为外部协议 | 供应商中立治理迅速成熟 | 需要内部语义 adapter citeturn13search0turn13search1 |
| **Adopt** | OTel 为 telemetry export | 生态标准正在形成 | 需做 PII/secret redaction citeturn17search0 |
| **Adapt** | Temporal Workflow/Activity 模式 | 与 I2/I5/I9 高度一致 | 不要把 Anchor Graph 等同 Temporal Workflow citeturn2search4 |
| **Adapt** | DBOS atomic transaction 思路 | 可用于 content prepared / event commit 一致性 | 跨 object store 仍需 saga/reconciler citeturn1search9 |
| **Adapt** | Restate single-writer keyed state | 适合 workspace branch ownership | 不能直接套用到整个文件系统 citeturn1search11 |
| **Adapt** | OpenAI outer-runtime / sandbox split | 与 Anchor 双平面非常接近 | 当前 Sandbox Agent 能力仍较新 citeturn11search5turn12search13 |
| **Adapt** | Aider 式自动 Git commit | AI 修改天然变成 revision | 需要处理巨大仓库、二进制数据和非 Git workspace citeturn4search17 |
| **Adapt** | temporal memory / KG | 很适合长周期上下文 | 必须保留 provenance，不能成为事实源 citeturn19search3 |
| **Avoid** | 多 Agent 默认共享同一个 mutable POSIX tree | 难以恢复文件级 race 与中间态 | 仅在受控实时协作编辑器中可能合理 |
| **Avoid** | branch HEAD / `latest` 进入事件历史 | 引用会漂移，破坏重放 | 在纯 UI 导航中可使用，但 commit 时必须 resolve |
| **Avoid** | VM memory snapshot 作为唯一恢复源 | snapshot 生命周期、代码变更等可使其失效 | 交互式 notebook 加速可使用，但需冷启动路径 citeturn6search10 |
| **Avoid** | vector memory 作为 canonical state | 抽取、索引、embedding 都可变化 | 用作检索投影完全合理 |
| **Avoid** | OTel trace 代替事件日志 | trace 可以采样、丢失且语义为观测而非事务 | 调试系统可只使用 trace，但不可承担恢复 |
| **Avoid** | MCP/A2A 承担 workspace consistency | 协议没有定义该语义 | 跨 Agent 通信仍应积极采用 |
| **Avoid** | 对外部副作用笼统声称 exactly-once | 网络故障可能留下 unknown outcome | 对同一事务数据库可以实现更强保证，外部 API 仍需 operation ledger/reconciliation |
| **Avoid** | 仅按固定 token/时间预算杀 Agent | 可能中断健康长任务，违反 I6 | 硬成本限额可作为组织政策，但应区分“预算停止”和“故障停止” |

### 短中长期战略

**短期，建议用约三个月完成架构原型而不是先换 runtime。**实现 `workspace_revision`、`content_ref`、WorkspaceManager 和 `ContentPrepared → NodeCommitted` 边界；仅支持单写者 workspace；选 Docker/gVisor 与一个 microVM/Sandbox SaaS 做两个 provider；网络默认拒绝；所有 secret 使用 broker reference；建立故障注入测试，重点模拟“revision 写成功但 event 失败”“event 尝试提交时 content store 超时”“worker 在 verifier 后崩溃”等情况。Temporal/DBOS 的恢复模型和 OpenAI Sandbox Agent 的生命周期已经为这一原型提供了充分模式依据。citeturn2search4turn1search9turn11search5

**中期，约三至十二个月，增加并行 workspace fork/merge、Git worktree/Merkle snapshot 后端、依赖缓存与内容 GC；把 Anchor tools 暴露成 MCP server，把跨 Anchor 实例的 Agent 服务暴露成 A2A；将 canonical events 投影成 OTel GenAI spans，同时建立 telemetry redaction。**A2A 1.0 与 MCP 在 Linux Foundation 治理下的成熟，意味着这部分现在比自造协议更有长期价值。citeturn15search0turn13search1turn17search0

**长期，约十二至二十四个月，可把 durable runtime 做成 backend abstraction。**建议至少验证 `Anchor-on-Temporal` 和一个第二后端（DBOS 或 Restate），衡量是否能逐步替换自研 lease/watchdog 的底层实现，但保持 Anchor Graph、operation ledger、verifier、content commit 等上层语义不变。这样才能满足 I7，而不是从“模型供应商锁定”转变成“工作流供应商锁定”。citeturn2search4turn1search16turn1search3


## 风险登记、未来情景、决策日志与开放问题

### 风险登记册

| 风险 | 触发条件 | 后果 | 缓解措施 | 相关不变量 |
|---|---|---|---|---|
| 控制/内容 split-brain | revision 与 NodeCompleted 跨系统提交，中途崩溃 | event 指向不存在内容或产生 orphan | prepare/commit + reconciler + integrity check | I2、I9 |
| Mutable ref 漂移 | event 保存 `main/latest` | replay 读取不同内容 | commit 时强制 resolve immutable revision | I2、I9 |
| 多 Agent 文件冲突 | 两 Agent 写同一 workspace | 非确定覆盖、中间态损坏 | per-branch single writer + explicit merge | I8、I9 |
| 重试重复副作用 | timeout 后未知远端结果 | 重复付款/发送/发布 | operation ID + idempotency key + UNKNOWN reconciliation | I3 |
| Sandbox escape | 不可信代码利用 kernel/runtime 漏洞 | 主机/跨租户失陷 | 风险分级：container→gVisor→microVM；patch/ephemeral host | I3 |
| 数据外泄 | sandbox unrestricted Internet | 源码/PII/secret 外传 | default-deny egress、proxy、domain allowlist、审计 | I3 |
| Secret 被持久化 | env/trace/workspace snapshot 包含 token | 长期凭据泄漏 | secret reference、ephemeral injection、redaction | I3 |
| Cache poisoning | 多租户共享可写 cache | 供应链或数据污染 | immutable keyed cache、tenant namespace、signature | I8、I9 |
| 依赖不可复现 | `latest` package/image | fresh rebuild 行为变化 | image digest、lockfile digest、registry snapshot | I9 |
| 模型非确定性 | fresh replay 重新请求模型 | 路径变化 | 区分 forensic replay 与 recomputation，保存 provider response | I9 |
| Memory poisoning | 用户/工具写入错误长期记忆 | 后续 run 被隐式污染 | provenance、scope、TTL、检索结果进入显式 input snapshot | I2、I8 |
| Trace 泄露 PII | 全量记录 prompts/tool args | 合规与安全风险 | OTel content opt-in、redaction、分类保留策略 | I2、I3 |
| MCP confused deputy | MCP server/tool 权限过宽 | Agent 借合法服务完成越权操作 | capability-scoped auth、per-tool policy、approval | I3 |
| A2A 身份混淆 | 远端 Agent 身份/权限不可靠 | 未授权工作或伪造结果 | signed identity、gateway、内部 operation ledger | I3 |
| Storage 爆炸 | 每 node 都保留完整 workspace snapshot | 成本失控 | Git/Merkle dedup、delta、tiering、reachability GC | I6 |
| Snapshot 过度依赖 | provider snapshot 过期或失效 | 长任务无法恢复 | snapshot 只作加速；immutable revision 可重新 hydrate | I2、I9 |

这里最需要原型验证的是第一项。DBOS 表明同事务域可以获得很强的 exactly-once transaction 语义；但 Anchor 的 event DB 与内容对象存储通常跨存储，因此必须用 reconciliation，而不能依赖一个不存在的跨数据库“神奇事务”。citeturn1search9

### 未来三至五年情景分析

**基准情景：协议标准化，状态语义仍分散。**到约 2029–2031 年，MCP/A2A 很可能进一步成为工具和跨 Agent 互操作的默认边界；durable engine 与 Agent framework 的组合也会更加常态化。但 workspace revision、side-effect ledger、memory provenance 等内部一致性问题仍由应用/runtime 层解决。支持这一推断的信号包括 MCP/A2A 转入 Linux Foundation 治理、A2A 达到 1.0，以及 2026 年多家 Agent framework 开始直接集成 Temporal。citeturn13search0turn13search1turn15search0turn21search7turn20search2

这一情景下，Anchor 的最佳竞争力不是再创建一个 Agent 协议，而是成为**具有严格可恢复语义的 Agent runtime**：内部自有 Graph/event/content invariants，对外使用 MCP/A2A/OTel。

**加速收敛情景：sandbox/workspace 变成 durable-engine 一级资源。**OpenAI Agents SDK 与 Temporal 已经开始把 sandbox create、command、read/write 等操作转为 durable Activities；Mastra 同时发展 Temporal 与 remote workspace；这可能演化成通用 `DurableSandboxSession`、`SnapshotRef`、`WorkspaceRevision` 等行业 API。若两三年内 Temporal、Restate、DBOS 等至少两家形成成熟、供应商中立的版本化 workspace primitive，Anchor 应重新评估自建 WorkspaceManager 的范围，把更多基础设施下沉给生态。citeturn12search13turn20search2turn20search10

**监管驱动情景：可追溯性由“好工程”升级为产品准入要求。**EU AI Act 已把自动日志和可追踪性写入高风险系统要求，中国也已经对生成合成内容标识与相关日志形成实际执法。未来高风险金融、医疗、政府 Agent 很可能要求更完整的“谁批准、什么工具执行、用什么输入、产生什么内容、是否经过人工验证”的证据链。这个情景下 Anchor 的 I2/I3/I4 不再只是工程偏好，而会成为明显商业优势。这里的后三年推断属于架构预测，法规当前事实则已有明确一手依据。citeturn16search12turn18search0turn18search10

**碎片化情景：各 Agent 平台继续携带自己的 memory、workspace 与 protocol extension。**即使 MCP/A2A 获得广泛采用，各厂商仍可能在 identity、sandbox、memory、approval、billing 上扩展不同机制。这个情景反而要求 Anchor 更坚持 I7：内部对象模型不应直接使用某厂商的 `thread_id`、`sandbox_id` 或 `conversation_id` 作为 canonical identifier，而应通过 adapter 映射。MCP 2026 stateless-first、A2A 的标准 task/artifact 结构为协议层提供共同部分，但没有消除内部状态差异。citeturn14search4turn14search12

### 决策日志

| 架构选项 | 本轮结论 | 主要依据 |
|---|---|---|
| 保持纯不可变 artifact，不增加 workspace | **反对作为唯一方案** | Coding agents 的生产形态需要持久 writable environment；Cursor、OpenAI Sandbox、OpenHands 均为此提供证据。citeturn2search5turn11search0turn3search4 |
| 引入持久共享 mutable workspace | **反对默认采用** | 并发与 replay 语义过弱；per-agent VM、Git revision、single-writer 更可靠。citeturn2search5turn1search11turn4search17 |
| 双平面：控制事件 + 内容 revision | **支持** | 与 durable execution、Sandbox Agent、Git/snapshot 实践最一致。citeturn12search13turn1search16 |
| 把 workspace 视为第二个 canonical truth | **反对** | live workspace/snapshot 可漂移或失效；应仅把被 event 引用的 immutable revision 纳入 recovery closure。citeturn6search10turn11search5 |
| 全面替换 Anchor 为 Temporal | **当前反对** | Temporal 强在 durability，但不会自动提供 I1/I3/I4/I8；迁移不能省掉 Anchor 的关键平台语义。citeturn2search4 |
| 为 Anchor 增加 Temporal backend | **支持原型验证** | 多个 2026 Agent framework 已证实这一组合模式。citeturn21search7turn20search2turn12search13 |
| 默认 microVM 执行所有任务 | **不建议一刀切** | Firecracker 成本已经较低，但 workload 和风险差异明显；应风险分级。citeturn5search1 |
| 网络全开、靠 Agent 自律 | **明确反对** | OpenAI、Daytona 均采用网络策略/代理/allowlist。citeturn7view0turn6search13 |
| MCP 作为 Tool API | **支持 Adopt** | 生态、治理、协议成熟度均显著提高。citeturn13search1turn14search4 |
| A2A 作为跨 Agent API | **支持 Adopt** | Linux Foundation 治理且 1.0 已发布。citeturn13search0turn15search0 |
| MCP/A2A 作为内部状态协议 | **反对** | 两者都没有定义 Anchor 所需的 canonical workspace transaction。citeturn14search4turn14search12 |
| Memory/vector DB 成为 canonical | **明确反对** | Letta/Zep/Mem0 都体现 memory 的可变、抽取和检索性质。citeturn19search12turn19search3turn19search18 |
| OTel trace 成为 audit log | **反对** | OTel 是 observability convention；应由 canonical events 投影。citeturn17search0 |

### 未解决问题与后续研究方向

**最重要的开放问题是跨存储 commit protocol 的实测。**目前可以从 DBOS 得到事务设计启发，却还不能仅靠文档决定 Anchor 应采用“双写 + reconciler”“transactional outbox + prepared revision”，还是把 workspace metadata 与 event store 放进同一个数据库、实际 bytes 放 object store。应通过故障注入比较三种方案的 crash windows、恢复复杂度和吞吐。citeturn1search9

**其次是 Git commit 与通用 workspace snapshot 的统一抽象。**Git 对源码文本极佳，但数据分析、Office 文档、大型二进制和生成数据可能更适合 Merkle snapshot。`workspace://id@revision/path` 应隐藏后端差异，同时 revision manifest 记录后端类型和 tree digest。这个问题目前没有足够行业标准证据，建议由原型决定。

**第三是合并语义。**代码可以借助 Git 三方 merge；结构化 JSON 可以进行 schema-aware merge；研究报告或 notebook 可能需要文档级 merge；二进制内容通常只能人工选择。Anchor 不应构建一个声称能自动解决所有冲突的“通用 CRDT filesystem”，而应让 workspace type 声明 merge policy。

**第四是 sandbox portability。**E2B、Daytona、Modal、Firecracker/Kata 的 session/snapshot 能力差异明显；Anchor 需要定义最小 `SandboxProvider` contract：`create_from_revision`、`exec`、`read/write`、`freeze_revision`、`network_policy`、`secret_ref`、`terminate`。Pause-memory、fork-memory 等高级能力只能是 capability negotiation，不能进入核心恢复假设。citeturn5search4turn6search1turn6search2

**第五是网络身份与外部副作用。**MCP/A2A 能标准化调用和 Agent 通信，但不能替代 capability-based authorization。需要进一步原型验证“operation ledger → short-lived credential → egress proxy → remote API”这一完整链路，以及 UNKNOWN 结果的自动/人工 reconciliation。

**第六是法规适用性。**EU AI Act 的 Article 12 日志义务主要针对高风险 AI 系统，不能简单宣称所有 Anchor 部署都必须保留六个月日志；中国《标识办法》也有明确服务适用范围。真正产品化前应针对 Anchor 的金融、医疗、政府、企业开发场景分别做法律适用性矩阵，而不是把最严格规则无差别套用。citeturn16search12turn18search0

**第七是长期可重复性应量化为多个等级。**建议下一阶段定义并测试：R0“事件法证 replay”、R1“同 revision/runtime、模型结果 stub 的 deterministic simulation”、R2“固定模型版本的 fresh re-execution”、R3“当前模型重新执行”。只有 R0/R1 应成为 Anchor 硬 SLA；R2/R3 应记录差异，而不是错误地声明完全一致。Temporal、DBOS 与 PydanticAI 的 durable execution 模式都支持这种分层。citeturn2search4turn1search16turn21search7

**综合置信度：高。**对“Anchor 应保留控制面语义，并新增以 immutable revision 为规范边界的内容面”“默认采用 per-task/per-branch single-writer workspace”“OTel/MCP/A2A/Memory 均不进入 Canonical Recovery State”三个结论，公开一手证据已经较充分。对“应该最终使用哪一个 durable engine”“应该默认使用哪一家 sandbox 服务”“哪种 snapshot 后端最合适”仍只有中等置信度，因为这些问题高度依赖 Anchor 的任务时长、repo 大小、租户风险级别、成本和实际故障分布，下一步应由故障注入和工作负载 benchmark，而不是更多产品功能对比来定夺。citeturn2search4turn12search13turn1search11turn17search0