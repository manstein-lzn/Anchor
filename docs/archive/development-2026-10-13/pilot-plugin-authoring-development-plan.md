# Pilot 自主获取与开发 Plugin：专项开发计划

> 历史快照（截至 2026-10-13）：保留当时的方案、执行记录与验收边界，不再追加，也不作为当前实现、授权或待办。当前行为见 [当前架构](../../architecture.md)，仍未关闭的事项见 [当前验收与待办](../../pilot-development-plan.md)。正文中的“当前”、PID、部署状态和读取顺序均以记录当时为背景。

编制日期：2026-10-11。用户后续已明确授权按 S1–S4 实施。下面保留编制时的基础、目标和接入约束；实际交付与证据以开发台账为准，计划中的要求不自动成为完成状态。

本文保存已讨论的目标、边界、代码接入点、实施顺序和接续开发上下文。[开发台账](pilot-development-plan.md) 仍是实际交付与验收状态的唯一台账；[当前架构](../../architecture.md) 描述实现，[产品与系统架构](../../product-architecture.md#架构指导新能力放在哪里) 描述目标。本文不另建进度台账，也不把计划写成已完成能力。

## 1. 用户目标与已确认的取舍

用户向 Pilot 描述需求后，Pilot 能检查本地 Plugin 生态，选择复用、获取、改造或新开发一个 Plugin，完成下载、配置、依赖准备、自测、安装，并指导用户在 Anchor 中使用。用户也可以直接给出仓库或候选来源，无需先手写一个完整插件再让 Pilot 安装。

本次讨论已收敛以下边界，接续开发不必重新开展同一轮方案比较：

1. 首期把 **Python 和 Rust** 的完整路径做通。Pilot 根据实际需求、已有插件和环境选择实现方法及库，平台不规定业务框架或固定模板。这不改变第三方 Plugin 原有的语言开放契约。
2. Python 复用已实现的依赖声明、解析、授权来源、共享包存储与插件视图，补齐候选开发和自测接线，不重新做一套环境管理。
3. Rust 复用现有二进制 Plugin 能力；使用共享工具链，默认使用已可用版本。遇到项目明确要求的版本或 nightly 时，经现有宿主授权使用 rustup 按需准备，不自研版本管理器，不为每个插件安装一份编译器。
4. 候选就是 Session 工作区中的普通目录，不新增“候选资产”、命名环境或开发平台。Cargo.toml/Cargo.lock 管理 Rust 构建依赖；不新增类似 Python 视图的 Rust 运行时依赖资产。
5. 保持现有安装确认、覆盖确认、不可读二进制处理和安装前置条件。安装与 Session 挂载是不同动作；新挂载和网络开关沿用下一 Turn 生效的语义，不热改当前工具集合。
6. Goose 继续负责 Agent loop、原生历史和 compaction。Anchor 补齐 Host 工具、工作区、应用用例和资源清理，不新增通用记忆、计划、恢复引擎，也不更换 Runtime 架构。
7. 用户先前要求先讨论方案，计划编制之后已授权实施本专项。可以直接完成代码、回归、真实 provider 隔离验收与文档；未授权改动现有开发/企微服务、部署、提交或恢复旧 Session 开关。

## 2. 代码现状与真正缺口

下表是编制计划时的代码核查结果。“已有”表示存在实现基础；真实 provider 端到端证据仍以台账为准。

| 领域 | 已有基础 | 本次需要补齐 |
| --- | --- | --- |
| Plugin 管理 | `plugin_list`、`plugin_read`、`plugin_review`、带原生确认的 `plugin_install`；安装包含依赖落地和并发前置条件 | Pilot 自己取得或写出候选，并在安装前准备和测试它 |
| 工作区操作 | AgentNode 的 `anchor_read`、`anchor_edit`、`anchor_run` 已经过 ToolPort 和 Sandbox | 接给 Pilot，并统一下载、编辑、构建、审查和安装使用的 Session 工作区 |
| Pilot 工作区 | 已挂载 stdio Plugin 使用稳定的 Session 级工作区；Goose 另有内部目录 | 当前候选路径却相对部署 workspace root 解析，不能直接视为已统一 |
| 任务联网 | `network` 可主动启用或由已挂载业务 Plugin 的需求派生启用；Session 明确禁止优先，Turn 冻结权限 | 新增的命令、下载、准备和候选测试也必须使用同一有效权限，不能走 Host 隐式联网旁路 |
| Python 依赖 | 声明属于 Plugin；已有解析、取包、校验、共享存储、视图、安装锁和回收 | 将准备从正式安装中提取，支持未安装候选的测试环境；候选不能冒充已安装 Library 视图 |
| Rust Plugin | 可审查带源码/构建文件的二进制包，安装保留可执行位，MCP 可启动相对 `bin/...` 程序 | 沙箱中的工具链、适合构建的超时、候选可执行授权及完整开发验收 |
| 生命周期 | 普通 Run 已清理 invocation 所属 Goose 数据；Graph 删除会按保留的 Run 元数据寻找共享 scope | Pilot Session 文件清理；全部会话 Run 先被逐一删除时，共享 scope 所有权不能随元数据一起丢失 |
| 指导信息 | 存在默认 Pilot instructions 和 Plugin authoring Skill | 默认指令限制搜索/项目遍历，Skill 含旧的人工准备依赖说明，需要与新角色一致 |

Plugin 目录的源码、依赖和工具信息目前也没有完整披露给 Pilot：`plugin_read` 主要读 instructions/Skill，短目录只给 id、名称、描述等。应扩展必要的资源读取，帮助 Pilot 基于本地现状判断和复用。

## 3. 归属层、事实所有者与权限

| 职责或事实 | 归属 | 调用契约 |
| --- | --- | --- |
| 选择来源、语言、库、编写与修正代码 | Pilot / Goose | 使用授权工具与真实返回结果，不在 Host 中硬编码开发决策循环 |
| Session/Turn、挂载开关、网络上限 | Session 与 Host 装配 | 每 Turn 冻结一次；准备、测试和命令共享同一权限结果 |
| 候选准备、自测、安装与删除协调 | Host 应用用例 | MCP/API 适配器转发小契约，不直接修改 Library、Session 的私有状态 |
| 已安装 Plugin、正式依赖视图及发布锁 | Library 与现有安装用例 | 保留摘要复核、替换条件、依赖授权和安装确认 |
| 候选源码、构建结果和临时准备资源 | Session 工作区及其受控准备空间 | 可续做、可核查、随所属 Session 回收，不是新的可发布资产 |
| 文件、进程、网络与可执行入口权限 | Sandbox / Host 授权 | 候选声明和模型输出不能新增权限，不能借构建或测试绕过禁止联网 |
| 原生 Session、模型历史与压缩 | Goose ACP | Anchor 只保留绑定、工具观察和清理所需所有权，不解析或复制一套通用历史 |
| Run/Graph 及共享 Goose scope 的保留关系 | Host 资源生命周期用例 | 只清理已证明归属且不再被有效执行、接续或交接引用的资源 |

新能力不进入通用 Kernel 的业务阶段，不在 Runtime 中出现 Python/Rust 开发专属 Agent loop。平台入口、Responses 入口与未来其他宿主入口复用相同工具和应用用例。

### 3.1 工作区约定

以现有 `<workspace_root>/pilot/<hash(owner, session)>` 为 Session 任务工作区的复用起点，由 Host 统一生成和持有绑定。Agent 只看到受限的工作区视图，例如 `/workspace`，候选参数使用相对目录。

必须区分三个根：

- **部署 workspace root**：用于 Run/Graph 工作区定位和清理，现有 `PilotTools.workspace_root` 也承担这个职责。
- **Pilot Session task workspace**：用于当前会话的下载、编辑、构建、候选审查/测试/安装，以及挂载 Plugin 的任务现场。
- **Goose 内部目录**：ACP 进程现场与原生历史，继续由 Host 绑定和 Goose 管理，不作为用户项目目录。

不能简单把 `PilotTools.workspace_root` 改成 Session 子目录，否则会改变 Graph/Run 删除所用的根。候选读取迁移到明确的 Session 绑定；旧的部署根候选路径不默认为本 Session 所有，更不能扩大为任意宿主目录读取。已发出的安装前置条件必须按原有摘要和路径约束复核，不能重解释旧确认。

### 3.2 网络、外部依赖与凭据

下载、git/cargo 命令、Python 依赖准备、候选 MCP 及正式挂载工具均受当 Turn 冻结的任务联网权限约束。部署授权的包来源是额外约束，不替代 Session 的网络上限；禁止联网时可复用本地已有包和工具链，需要取包时返回可解释的缺口。

未安装候选不能通过声明 `network:true` 自己启用网络，也不能自动挂载 `network` 或修改 Session 开关。Provider 控制通道沿用现有独立配置，不与任务联网混为一谈。

普通配置进入候选文件；密钥与授权继续通过已有环境变量名称、owner-bound OAuth 等契约提供，候选及分发包不得携带密钥。准备/测试不授予宿主 Shell 权限或候选声明中的任意凭据。

## 4. 四个垂直切片

默认顺序为 **S1 → S2 → S3 → S4**。每片先形成可运行、可验证、能复用的路径，再扩展边界；不先搭完整开发平台。下面的交付条件是要求，不是完成状态。

### S1：统一 Pilot 工作区，接入已有文件与命令工具

**交付行为：** 用户要求获取或编写 Plugin 后，Pilot 能在自己的 Session 目录中查看、创建和修改文件、执行授权命令，并对同一目录调用现有审查/安装流程。

实现要点：

1. 从现有 Pilot Plugin 工作区计算和 Sandbox 构建中提取可复用的 Host 绑定；工作区初始化不再依赖本 Turn 恰好挂载了一个 stdio Plugin。
2. 复用 `NodeTools` 的 `anchor_read`、`anchor_edit`、`anchor_run` ToolPort。共享代码和校验，不复制另一套文件/命令实现；AgentNode 原有 workspace 语义不变。
3. 用一个小的 Host 内部 Turn context 传递 Session 工作区、Sandbox、冻结的有效联网权限、取消信号和工具集合。它是装配信息，不是新持久化环境或上下文系统。
4. `plugin_review` 与原生 `plugin_install` 改用同一 Session 任务目录解析候选。Graph/Run 管理继续使用部署根。
5. 普通 Turn 和 Responses 两个入口都接入，保留 ACP 工具定义、调用分发、工具观察、SSE 和取消链路，避免仅其中一个入口可用。
6. 扩展 `plugin_read` 的必要源码、依赖和构建资源读取，复用现有 Library 资源读取约束和大小限制；不向模型开放任意绝对宿主路径。
7. 调整默认 Pilot instructions 与 authoring 指导：用户请求开发/获取时可以完成相关搜索、文件操作与命令执行；只有讨论方案时仍然不执行。指导必须在默认 Pilot 路径可获得，不能依赖部署中未安装的 `anchor-steward` 恰好存在。

**验收条件：** 两个 Pilot 入口均能创建并审查同一候选；下一 Turn 可继续读写；不同 Session 隔离；越界路径被拒绝；取消能终止命令；禁止联网对命令有效。既有 Graph/Run 删除路径及 AgentNode 工具回归通过。

### S2：Python 候选准备与安装前自测

**交付行为：** Pilot 可以开发一个带第三方 Python 依赖的 Plugin，在正式安装之前准备依赖、启动 MCP、发现或调用工具、修正错误，再走原有安装流程。

建议增加两个薄工具，名称和可选参数按现有 MCP 风格落地；下表是行为契约，不要求先建立通用任务协议：

| 工具 | 建议输入 | 返回事实 | 不产生的事实 |
| --- | --- | --- | --- |
| `plugin_prepare` | Session 相对 `directory`，可选 `id` | 候选摘要、声明检查、实际解析的包集合、可用准备结果或明确失败原因 | 不发布 Plugin，不建立正式 Library 视图，不挂载 Session |
| `plugin_test` | 同一 `directory`，可选工具调用（server、工具名、参数） | 被测试摘要、实际使用的依赖集合、MCP 启动/工具目录/调用结果、退出或取消事实 | 不将发现工具加入当前 Turn 的长期工具集合，不代替安装确认 |

无工具调用参数时，`plugin_test` 至少实际启动候选 MCP 并读取工具目录；有参数时调用指定工具。协议启动成功与业务功能正确是不同验收结果，返回中必须可以区分。

实现要点：

1. 在 Host 应用用例中组织准备/测试，从现有 `plugin_dependencies` 安装过程提取解析、取包、校验和视图准备能力。保留 installer 对正式发布及 Library 锁的所有权，不在适配器中直接拼私有存储。
2. 候选准备结果放在 Session 受控准备空间，身份绑定候选摘要及实际依赖集合。已有包可通过受控接口复用；共享存储的写入仍走现有授权和并发控制。不要把候选写入 `library/views` 伪装成正式插件：当前回收只认识已安装 Plugin 的引用。
3. 复用现有目录审查/临时 staging、Plugin catalog、`SandboxLaunch`、`PluginToolHost` 和 MCP stdio 连接。候选测试用临时布局适配已有 `/plugins/<id>/...` 路径，不以正式安装作为测试前置条件。
4. 准备与测试绑定实际候选摘要。代码、清单或依赖变化后，原测试结果不再覆盖新候选；安装仍进行原有确认后摘要复核。
5. 正式安装可以复用包字节，但必须独立生成并拥有正式视图。若安装实际解析到的依赖集合与测试集合不同，结果明确披露，由 Pilot 收紧声明或对实际集合重测，不能把候选测试直接当作安装版本验证。
6. 测试使用当前冻结权限、进程取消与有限输出保留。失败只返回实际结果供 Pilot 修正，不增加自动重试开发循环或用户审批状态机。

**验收条件：** Python 候选在未安装状态下完成依赖准备和真实 MCP 调用；安装后按 Session 挂载语义在下一 Turn 可用；删除测试开发目录和候选准备资源后，已安装 Plugin 仍能启动。候选变化、离线缓存命中/缺包、失败与取消、不同 Session 并发、共享包回收均不破坏正式插件。

### S3：Rust 通过同一工作流构建、测试和安装

**交付行为：** Pilot 在 Session 工作区创建或取得 Cargo 项目，用现有命令工具完成构建和修正，按同一候选工具测试二进制，随后安装使用。

实现要点：

1. 经现有 `tool_environment`/Sandbox 授权把 cargo、rustc、必要 linker、rustup 和系统构建资源提供给开发命令。只在宿主装好工具或允许 `sh` 不等于沙箱内已经可用。
2. 默认复用共享工具链；项目声明确有要求时，使用 rustup 现有多版本能力。共享工具链和下载缓存的挂载、写权限及并发遵守 Host 授权；项目源码、锁文件、`target` 和待打包产物在 Session 工作区，不写入已安装 Plugin。
3. cargo 的 crate 解析与构建遵循 Cargo.toml/Cargo.lock；支持需要的取包权限，离线时实际检查缓存。build.rs、proc-macro 等构建期执行同样处于沙箱中，不借编译绕过任务联网限制。
4. 现有 `anchor_run` 使用固定 30 秒超时。提供适合构建的可配置时限/宿主上限，保留取消、超时后进程退出与结果观察；不通过无限后台任务绕开取消。复用这一命令能力，不再做一套 Rust 构建执行器。
5. 当前 Sandbox 会拒绝任意 `/workspace/...` 顶层可执行命令。候选测试通过既有 Plugin launcher 提供限定目录和入口的执行授权，复用 `/plugins/<id>/...` 映射；不全局开放宿主可执行路径。
6. 候选至少包含声明的 `bin/...`、可读源码、Cargo 清单/锁文件和可复现的构建说明。已有审查只能识别是否存在源码/构建文件，不能证明二进制与源码一致，结果不能夸大这项保证。
7. 在实际候选/正式 Plugin 运行沙箱中验收目标架构、动态库及资源路径。Rust 二进制不必然是静态链接；不得保留对开发 workspace、`target` 或工具链私有路径的运行依赖。

`plugin_prepare` 对 Rust 做同一候选检查及可用条件披露；具体 cargo 命令由 Pilot 使用 `anchor_run` 执行，`plugin_test` 复用 S2 的候选启动/调用路径。不新增 `dependencies.rust` 运行时视图。

**验收条件：** 带至少一个第三方 crate 的 Rust MCP Plugin 在真实沙箱中完成构建、候选调用、安装和下一 Turn 使用；编译超过原 30 秒限制时可完成或清楚超时；取消终止构建；删除开发目录后正式插件仍可运行。默认工具链和项目特定版本需求分别验证，不能把宿主 shell 中的成功当作沙箱可用。

### S4：资源生命周期收口与完整用户路径验收

**交付行为：** 用户可以持续开发、取消后核查继续、安装使用并删除所属 Session/Run/Graph；工作区和 Goose 数据按真实归属清理，不误删共享资源或遗留无主 scope。

生命周期工作包括两条独立边界：

- **Pilot Session：** 当前删除入口主要删除 SessionStore 事实。增加 Host 应用用例，协调 Session 任务目录、候选准备资源、Goose 内部目录和 Session 事实；复用现有运行中 Turn/渠道/保留 Run 前置条件，并排除删除与新 Turn 接纳的竞态。在事实删除前保留重试所需资源绑定，部分文件清理失败后仍可核查并继续，不能仅靠目录 hash 猜归属。
- **Graph 会话共享 scope：** 普通 Run 的 invocation 数据清理已有实现；会话 Run 刻意保留共享历史，而 Graph 删除当前从保留的 Run 元数据收集 scope。补齐“所有会话 Run 已先逐一删除”的所有权缺口，在移除元数据前保留最小资源归属/引用事实，最后一个有效引用消失后才可回收。继续保留 lineage 墓碑、原生 scope lease 和交接保护，不把墓碑冒充恢复游标。

具体持久化位置和版本兼容在实施前依据现有绑定格式确定，必须明确新增事实的唯一所有者和迁移规则。旧资源缺少可靠归属时不能静默推断删除；需要调查的历史资源与新实现正确清理分开报告。这是有限的资源所有权补齐，不扩建通用恢复/清理平台。

Session 删除不删除正式安装 Plugin、共享包存储或共享工具链；Graph/Run 清理也不能删除其他幸存 Run 的原生历史。已安装 Plugin 的卸载及依赖回收继续由已有 Library 用例承担。

**验收条件：** 覆盖 Session 删除、普通 Run 删除、共享会话 Run 保留/释放、Graph cascade 删除，以及先删光 Run 再删 Graph；活动 Turn/Run 和有效交接阻止清理；清理失败可重试且保留足够事实。完成下面的端到端验收矩阵后，再记录本专项完整交付。

## 5. 代码接入地图

以下链接定位编制计划时的实现。接续开发先核对当前符号，不把行号或此处描述当成不会变化的接口。

| 工作包 | 主要路径与符号 | 注意点 |
| --- | --- | --- |
| Pilot 装配 | [pilot_tools.rs](../../../rust/anchor-runner-host/src/pilot_tools.rs)：`pilot_plugin_sandbox`、`frozen_plugin_tools`、`PilotTools`、`workspace_directory` | Session 任务根与部署根分开；冻结网络的结果需要供新工具复用 |
| 两个入口 | [api/turns.rs](../../../rust/anchor-runner-host/src/api/turns.rs)、[api/responses.rs](../../../rust/anchor-runner-host/src/api/responses.rs) | 都会创建 Pilot 工具/执行上下文，不遗漏 Responses |
| ACP 与原生交互 | [goose_acp/pilot.rs](../../../rust/anchor-runner-host/src/goose_acp/pilot.rs)、[pilot_interaction.rs](../../../rust/anchor-runner-host/src/goose_acp/pilot_interaction.rs) | 工具定义/分发、取消、原生安装确认；Graph 删除仍用部署根 |
| Prompt 与原生目录 | [pilot_host.rs](../../../rust/anchor-runner-host/src/pilot_host.rs)：`scope`、`INSTRUCTIONS` | Goose 内部 scope 绑定与任务工作区独立 |
| 本地资源读取 | [pilot_tools/plugins.rs](../../../rust/anchor-runner-host/src/pilot_tools/plugins.rs)、[resource_read.rs](../../../rust/anchor-runner-host/src/resource_read.rs) | 复用 confined resource read，不开放任意文件读取 |
| 文件/命令工具 | [node_tools.rs](../../../rust/anchor-runner-host/src/node_tools.rs)：`NodeTools`、`DEFAULT_TIMEOUT` | ToolPort 复用；构建时限、输出和取消 |
| 工具环境与沙箱 | [tool_environment.rs](../../../rust/anchor-runner-host/src/tool_environment.rs)、[anchor-sandbox-bwrap](../../../rust/anchor-sandbox-bwrap/src/lib.rs) | 现有授权入口、只读环境和执行目录；不能只改命令白名单 |
| Python 准备 | [plugin_dependencies.rs](../../../rust/anchor-runner-host/src/plugin_dependencies.rs)：`land`、[dependency_provision](../../../rust/anchor-runner-host/src/dependency_provision/mod.rs)、[plugin_view.rs](../../../rust/anchor-runner-host/src/plugin_view.rs) | 提取准备与发布，候选空间与正式视图区分，取包和 build 的权限不能混淆 |
| 安装用例 | [application/plugins.rs](../../../rust/anchor-runner-host/src/application/plugins.rs)：`plugin_install_precondition`、`install_plugin_if_unchanged` | 保留确认前后复核、正式 Library 锁及实际依赖结果 |
| 候选审查和回收 | [anchor-library](../../../rust/anchor-library/src/lib.rs)、[review.rs](../../../rust/anchor-library/src/review.rs)、[reclaim.rs](../../../rust/anchor-library/src/reclaim.rs) | 复用目录审查/staging；正式视图回收只认识正式 Plugin 引用 |
| MCP 测试 | [tool_host.rs](../../../rust/anchor-runner-host/src/tool_host.rs)：`McpToolConfig`、`SandboxLaunch`、`PluginToolHost`；[anchor-mcp-host](../../../rust/anchor-mcp-host/src/lib.rs)：`connect_sandboxed_stdio` | 复用真实启动、tools/list 与 tools/call，不复制 MCP 客户端 |
| Session 删除 | [api/sessions.rs](../../../rust/anchor-runner-host/src/api/sessions.rs)、[SessionStore](../../../rust/anchor-platform-session/src/store.rs) | 从仅存储删除提升为 Host 生命周期协调，保留现有前置条件 |
| Run/Graph 清理 | [run_data.rs](../../../rust/anchor-runner-host/src/run_data.rs)、[application.rs](../../../rust/anchor-runner-host/src/application.rs)、[run_deletions.rs](../../../rust/anchor-runner-host/src/run_deletions.rs) | 先保留共享 scope 归属，再移除其发现所依赖的元数据 |
| 原生会话归属 | [goose_acp/conversation.rs](../../../rust/anchor-runner-host/src/goose_acp/conversation.rs)、[node_host.rs](../../../rust/anchor-runner-host/src/node_host.rs)：`conversation_hint_for` | scope lease、Graph/Session/Run 引用与前驱历史保护 |
| 开发指导 | [plugin-authoring/SKILL.md](../../../plugins/anchor-steward/skills/plugin-authoring/SKILL.md)、[Plugin 文档](../../plugins.md) | 清除与现有依赖准备能力冲突的指导，确保默认 Pilot 可获得 |

## 6. 验收与证据要求

所有场景先用小型候选、fixture Graph 和确定性本地 Provider 检查，实际经过 Host、Goose ACP、授权 MCP 与 Bwrap，只替换模型传输。真实 provider 验收单独执行，不能由工具单测或 fixture 通过代替。研究、周报、RSI 大图不作为本专项常规回归入口。

| 场景 | 必须看到的结果 | 主要切片 |
| --- | --- | --- |
| 用户给需求，Pilot 复用本地插件 | 能读必要源码/依赖信息，解释选择并实际使用相应资源 | S1 / S4 |
| 用户给仓库或要求联网寻找 | Pilot 在获授权网络下取得候选并继续配置、自测、安装；搜索与下载有真实工具证据 | S1 / S4 |
| 从自然语言开发 Python Plugin | 带第三方包，未安装时测试，原生确认安装，挂载后下一 Turn 调用成功 | S2 / S4 |
| 从自然语言开发 Rust Plugin | 带第三方 crate，沙箱构建和候选测试，安装后实际 MCP 调用成功 | S3 / S4 |
| 安装产物独立 | 删除开发目录和候选准备空间后，Python/Rust 正式 Plugin 仍能使用 | S2 / S3 / S4 |
| 网络明确禁止 | 下载/依赖准备/编译期/候选调用均不能绕过；缓存能用时给实际结果，缺包时明确失败 | S1–S4 |
| 本 Turn 改挂载或网络开关 | 当前 Turn 不热更新，下一 Turn 使用新快照；安装本身不构成自动挂载 | S1–S4 |
| 取消、重开和压缩后继续 | 进程真正退出，Pilot 核查文件及工具结果后继续；不自动重放安装确认或未知副作用 | S1–S4 |
| 候选与依赖变化 | 旧测试不能覆盖新摘要；安装集合变化可见；旧确认不能安装被替换内容 | S2 / S3 |
| 安装失败或依赖准备中断 | 正式 Plugin 和视图不处于半发布状态，保留足够结果供核查/重试 | S2 / S4 |
| 并发与清理 | Session 隔离；共享存储正确；活动引用保留；部分清理失败可重试 | S2 / S4 |
| 删除全部会话 Run 后删 Graph | scope 仍有可证明的归属，最终释放；幸存接续/交接历史不受影响 | S4 |

可复用的现有检查入口：

- [goose_pilot.rs](../../../rust/anchor-runner-host/tests/goose_pilot.rs)、[goose_pilot_compaction.rs](../../../rust/anchor-runner-host/tests/goose_pilot_compaction.rs) 与 [goose_fixture.rs](../../../rust/anchor-runner-host/tests/support/goose_fixture.rs)：实际 Pilot/ACP、跨 Turn 与压缩路径。
- [native_plugins.rs](../../../rust/anchor-runner-host/tests/native_plugins.rs)、[session_plugins.rs](../../../rust/anchor-runner-host/src/api/tests/session_plugins.rs)：Plugin 启动、确认、挂载与冻结权限。
- [goose_conversation.rs](../../../rust/anchor-runner-host/tests/goose_conversation.rs)、[goose_persistent_assistant.rs](../../../rust/anchor-runner-host/tests/goose_persistent_assistant.rs)、[conversations.rs](../../../rust/anchor-runner-host/src/api/tests/conversations.rs) 和 `run_data` 测试：共享历史、删除和接续保护。
- [session-plugins.spec.ts](../../../apps/web/e2e/session-plugins.spec.ts)：安装/挂载结果在用户界面的可见性；仅界面受影响时扩展 Web 检查。

执行顺序遵守仓库约定：先运行直接相关的 crate/test 子集；切片完成或跨模块语义改变后再执行一次必要全量回归，并完成相关 `cargo test`、`cargo clippy`、`cargo fmt`。Web 有改动才按需执行测试、E2E 和 build，不为文档或未改代码重复构建。

真实 provider 验收至少走通一个带第三方依赖的 Python Plugin 和一个带第三方 crate 的 Rust Plugin，并保留工作区、工具观察、安装摘要、实际依赖/工具链、下一 Turn 调用与清理后的调用证据。未配置 provider、失败、跳过和未覆盖边界必须分别记录。

单次编辑、格式修复或重跑测试不单独记账。只有垂直切片或用户行为实际完成时，才更新 [开发台账](pilot-development-plan.md) 的相应验收项并追加推进记录；只有真实 provider 路径通过后，才将对应端到端能力写为完成。

## 7. 持久化与兼容性检查点

以下检查点随所属切片实现，不是增加额外审批流程：

- **候选路径语义：** 从部署根收敛到所属 Session 任务根。明确新旧调用及未完成确认的处理，不扩大旧路径权限。
- **准备结果：** Session 拥有候选准备资源，Library 拥有正式发布。摘要和实际依赖用于核查，不恢复已删除的命名环境或按 Run 固定依赖模型。
- **执行授权：** 共享工具链挂载、构建时限、候选二进制授权均沿用 Host/Sandbox 契约，新增适配不能放宽整个部署的文件或网络权限。
- **资源归属：** 若扩展 Goose binding 或新增最小清理事实，明确版本、唯一所有者、旧数据兼容和删除重试；保留已有 lineage 墓碑语义。
- **正式产物：** 安装包和视图不引用开发目录；候选消失不影响正式 Plugin。普通配置与密钥契约保持一致。

依赖相关文档还存在部分早期迁移描述，不能据此恢复已经删除的“命名环境 + 每 Run 冻结绑定”机制。实施时以最新代码、标准的现行模型及台账证据核对；无关旧文档修订不作为当前切片的前置工作。

## 8. 上下文压缩与下一开发会话接续

本节是接续入口。压缩摘要或开启新的开发会话时，必须保留本文件路径、已实施切片、实际证据和下一步；不依赖原聊天完整保留。

### 接续时按此顺序读取

1. 根目录 [AGENTS.md](../../../AGENTS.md) 和本文件，恢复用户确认的目标与四片顺序。
2. [开发台账](pilot-development-plan.md) 的最新相关记录，判断有哪些实际交付；没有完成记录不能因为本计划存在就视作完成。
3. [产品与系统架构的归属规则](../../product-architecture.md#架构指导新能力放在哪里)、[当前架构](../../architecture.md) 的 Pilot/Goose/Plugin 相关章节、[任务联网契约](../../plugins.md#pilot-任务联网)。
4. `git status --short`、本次涉及路径的 diff 和第 5 节的代码，核对计划编写后已经发生的变化；只读取相关路径，不重跑此前已获得的全部证据。

### 必须保留的上下文

- 核心目标是 Pilot 自己获取、配置、测试、安装或开发 Plugin，首期 Python/Rust；不是要求用户手动准备完整候选。
- 基础已经有：联网、安装用例、AgentNode workspace 工具、Python 共享依赖、Rust 二进制 MCP。缺口主要是 Pilot 装配、候选准备/测试、工具链接入和生命周期闭环。
- 部署根、Session 任务根、Goose 内部目录不能混用，尤其不能直接替换 `PilotTools.workspace_root` 破坏 Graph/Run 清理。
- 当前 Turn 权限冻结；明确禁止联网优先；候选不能自授权；安装确认和挂载语义保留。
- Python 候选准备不是正式 Library 视图；Rust 使用 Cargo/rustup 现有能力；不新增依赖资产、Agent loop 或记忆系统。
- Run 删除已有清理能力，但共享 scope 不能仅依赖即将被删除的 Run 元数据；Session 文件清理也需要 Host 协调。
- 文档与 fixture 通过不等于真实 provider 端到端完成；证据与验收只进总台账。

### 编制时的工作区边界

本次计划编制前，工作区已有联网 Plugin、Session Plugin WebUI、依赖与架构文档等未提交改动。另有 `docs/wecom-persistent-run-development-guide.tmp.md`，与本专项无关。不要 reset、checkout、clean 或自动纳入提交；实施前逐路径核对 diff，并保留其他工作流改动。

已讨论并处理过的 Graph 目录可见性、联网 Plugin 部署等问题见台账 A204–A206，属于本专项的现状背景，不要在恢复时重新当作待开发任务。不要为本计划重启服务、恢复旧 Session 开关或改部署。

### 下一步与交接格式

实施已获授权。接续时先核对现有代码、台账和保留证据，继续尚未通过的行为与边界；已经获得的定向证据不重复执行，已经落地的 S1 接线不重新实现。两个 Pilot 入口、候选准备/业务调用、共享 Rust 工具链和资源归属清理必须共同保持成立。

后续每次交接至少保留以下内容；它是开发交接模板，不是产品里的新日志协议：

```text
专项计划：docs/pilot-plugin-authoring-development-plan.md
当前工作包：S1 / S2 / S3 / S4（写明已完成行为及剩余边界）
本轮修改路径：只列实际修改
实际验证：命令、结果、证据位置；fixture / 真实 provider 分开
未完成或失败：具体缺口、环境条件、是否存在运行中的进程
下一步：一个可直接执行的动作
工作区保护：与本专项无关的改动或资源
用户新决定：如有，更新本专项计划的对应边界；完成状态仍只记总台账
```
