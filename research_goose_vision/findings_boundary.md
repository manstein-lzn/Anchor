# Goose + ACP 与 Anchor 愿景：扩展、权限和部署边界

日期：2026-10-06。范围：Anchor 作为 ACP client、Goose 作为节点/Pilot 执行候选。只读调研，仅写本文件；未运行模型、测试、构建或安装。恢复、compaction、预算及人工提问不在本任务范围。

## 结论

**Goose + ACP 与“Rust 执行后端、多 provider、显式 Plugin/工具资源、独立 Graph 部署”的方向契合，但不是 Anchor 产品契约的自动实现。** Goose 可以承担受控 Agent 执行及模型/扩展接入；Graph/Run/Artifact、Plugin 绑定、工具授权、网络/凭证、部署闭包仍须由 Anchor 宿主落实。纯 ACP 标准既不覆盖所有 Goose 功能，也不提供 OS 沙箱。[S1–S8；本地产品目标]

当前只能判断“候选适配方向可行”，不能判断“统一后已覆盖全部 vision”：仓库 A112 已用真实 v1.53.0 二进制验证小型无 Plugin/媒体的节点，尚未验证完整 Plugin、多 provider、媒体及独立 bundle。这里沿用既有记录，不将其写成本次新增验收。依据：`docs/goose-acp-spike.md:5`、`docs/goose-acp-spike.md:19`、`docs/pilot-development-plan.md:293`。

## 1. ACP 标准与 Goose 私有能力

| 已核验事实 | 对 Anchor 的含义 |
| --- | --- |
| ACP 协商能力并允许传入 MCP server；stdio 属基线，HTTP/SSE 要分别检查声明。Goose v1.53.0 声明 HTTP MCP，源码明确拒绝 SSE。[S1、S3] | “支持 ACP”不等于支持任意 MCP transport；Anchor 必须按固定二进制握手结果接纳。 |
| ACP 的 `_meta` 与下划线开头方法是协议允许的扩展机制，不是所有客户端必须理解的标准语义。[S2] | 能从 ACP 链路调用，不等于是可移植的纯 ACP 功能。 |
| Goose 的 `session/new._meta.enabledExtensions` 由发布版自定义解析；schema 另列 `_goose/unstable/session/extensions/add`、`.../remove`、`_goose/unstable/tools/list`、`.../permissions/set`、`_goose/unstable/resources/read` 等。[S4、S5] | 精确扩展配置、工具目录/权限管理、资源读取不能仅凭标准 ACP 推定支持；采用这些接口就形成需要版本绑定的 Goose-specific adapter。 |
| 发布版处理标准 `session/set_config_option`，识别 `provider`、`model`、`mode` 等配置 ID；provider catalog、配置/凭据管理及语音功能另有 Goose 私有方法。[S5、S6] | 可以保持标准会话调用，同时用显式适配补缺口；不能声称纯 ACP 已涵盖 Goose 配置、集成与媒体的全部能力。 |

协议证据固定为官方 ACP v1 文档提交 `06128106cf54b806a491a0fcf2b6bc7022ef57ca`。该协议快照包含的可选接口，不自动代表 Goose v1.53.0 已实现；不把 SDK 版本或后续协议设计当作发布版保证。

## 2. 唯一 anchor extension 与精确工具目录

- **v1.53.0 不能仅靠 client extension 选择实现精确替换。** `initial_session_extensions` 先加入 builtin，再处理 recipe/client 选择；未显式选择 builtin 时默认是 `developer`，配置明确禁用它才跳过。没有 client 选择/recipe 时，还合并配置启用扩展、项目 Plugin MCP 和请求 MCP。因此 `mcpServers: []` 不代表零工具，`enabledExtensions: []` 也不能单独证明禁用了 developer。[S3、S4]
- **存在后续修复，但不属于本次测试版本。** 官方提交 `540df77c30e3c1f3b51915cb18ae081931c96851`，2026-10-06，修复无 recipe 且有 client 选择时将其视为精确集合；recipe 路径仍保留 builtin 合并。不能倒推 v1.53.0 已具备该行为，也没有本次运行证据证明采用修复后的完整隔离。[S7]
- **受控环境可以构造唯一 anchor，不是 ACP 自动保证。** 既有 A112 通过隔离配置、禁用 developer、显式扩展选择及不提供 fs/terminal 回调，在实际模型请求中断言仅有两项 Anchor 工具。这只证明该 fixture 配置，不证明任意用户配置、recipe 或 Plugin 注入均无法扩展执行面。依据：`docs/goose-acp-spike.md:19`、`docs/goose-acp-spike.md:64`。
- **工具级筛选确有代码门禁。** Goose `ExtensionConfig::available_tools` 非空时限制名称；空列表的含义是所有工具可用，不是拒绝全部。扩展管理器同时过滤目录并在 dispatch 前检查，不只是提示词隐藏。标准 ACP 的 `mcpServers` 配置没有替 Anchor 定义这套工具授权契约。[S8]
- 唯一 MCP server 名称不等于唯一权限边界：其工具参数、文件路径、命令、外部资源和凭证仍需 Anchor ToolPort 校验。Goose hooks 还从启用的 Plugin 独立加载并执行本地命令，不能把“模型只看见 anchor 工具”当作“进程没有其他执行路径”。[S9；本地产品目标]

未知：污染配置/项目目录、运行中扩展变更、跨节点/用户复用时的撤权，以及全部旁路的隔离效果；本次未做全面源码审计。不能以“没有出现权限请求”作为安全证明。

## 3. Plugin、skills 与 resources：承载契合，但产品契约仍缺

- Goose 固定版本支持 `SKILL.md`，从用户/项目 `.agents/skills`、兼容目录及启用 Plugin 发现技能；短说明与按需加载方向契合 Anchor 的渐进披露。`SkillsClient` 提供 `load_skill`，也支持读取列出的 supporting files。[S10]
- Goose 官方 Plugin 指南定义自己的 manifest、skills 与 hooks 包；固定源码另有启用 Plugin 的 MCP server 导出。它与 Anchor Plugin 是不同产品对象，不是同名即兼容。[S9、S11]
- Goose 扩展管理器有 MCP resources/prompts 的列举和读取代码；这证明接入载体存在，**不证明 Anchor 知识资源已按目录、来源、只读挂载和权限契约接通**。[S12]
- Anchor 的能力资产还要求显式节点挂载、唯一来源、短目录与说明入口、工具独立环境、运行绑定来源/内容摘要，以及包不得扩大宿主授权。这些由 Anchor Library/Plugin/Host 拥有，MCP 或 skills 本身不会自动写入 Anchor Run。依据：`docs/product-architecture.md:296`、`docs/product-architecture.md:467`。

最小接线方向（推断，不是架构决定）：由 Anchor 解析并冻结 Plugin，向 Goose 提供短目录及经过授权的说明/资源读取工具；业务调用仍通过 anchor MCP → 既有 ToolPort。若采用 Goose 原生 skill/plugin 发现机制，必须限制发现根及自动更新/可执行 hooks，不能隐式吸收用户 home 或项目中的额外能力。官方指南支持 Plugin 自动更新，但本次未验证其在 ACP 路径中的执行时机与完整关闭方式。[S10、S11]

未知：既有 Anchor/Codex 风格 Plugin 的完整兼容、Plugin OAuth/真实业务 MCP、受限模式下的 supporting files、所有资源类型、绑定摘要与分发清单。**MCP 可连接、技能可发现、Anchor Plugin 产品契约完成是三个不同状态。**

## 4. 权限、网络、credential 与进程隔离

- ACP 工具权限请求是 agent 可以使用的协调机制；协议不要求所有执行都通过客户端权限请求。工具展示信息本身也不授予权限。它不定义 Anchor 的路径/命令/网络策略，更不安装 OS 防火墙。[S13]
- Goose ACP fs/terminal 包装按 client 能力桥接部分操作；没有声明这些能力，不能据此推定本机 developer 工具被禁用。固定源码存在直接本地执行的 fallback；因此“拒绝客户端回调”与“禁用本机工具/限制进程”必须分别验收。[S14]
- 普通 stdio MCP 接入直接启动配置命令，追加环境变量并设置工作目录；它不是强制沙箱。相关源码路径没有把工作目录当成文件访问边界，也没有显示为所有子进程建立独立网络/凭证隔离。[S15]
- 通过 ACP 启动外部 Goose，能隔离语言库依赖和进程生命周期，**不能自动隔离文件系统、网络、环境、keyring、OAuth cache 或 MCP 后代**。provider secrets 源码包含 secret store 与 provider cache 两类来源；宿主必须明确哪些凭据可见。[S15、S16；权限责任推断]
- A112 访问本地 model proxy/MCP 使用显式共享网络授权；文档明确 Bubblewrap `--share-net` 不是 loopback-only 防火墙。业务工具仍经原 Anchor 沙箱；任意 Goose 程序的进程级 outbound 隔离未验收。依据：`docs/goose-acp-spike.md:19`。

责任区分：ACP adapter 负责能力协商与协议请求校验；Anchor Host/ToolPort 负责每次工具调用的产品授权；Sandbox/部署环境负责进程、文件、网络和凭据的强制隔离。三者不能互相替代。多租户凭证隔离、真实 OAuth、代理/重定向/DNS/出站限制和平台间沙箱差异均为未知，不声称 ACP 自动保证。

## 5. 多 provider、模型与媒体选择

- 官方固定版本 provider 文档覆盖多个云端/本地 provider，以及兼容 API 接入；多 provider 方向契合，不必将 Goose 固定为某一个模型 transport。[S17]
- 发布版从 Goose 配置解析默认 provider/model，标准 `session/set_config_option` 可切换对应选项，私有 catalog 接口补充枚举与设置。Anchor 的模型别名、允许列表及实际 invocation 模型绑定仍属宿主产品事实，不能自动等同 Goose 配置 ID。[S3、S5、S6；`docs/product-architecture.md:115`]
- v1.53.0 ACP 握手声明 image/embedded context，audio 为 false；输入转换接收 image，嵌入资源只处理文本，audio 不进入该转换分支。不能因 schema 有音频、Goose 另有 dictation/live-voice 私有接口，就写标准 ACP prompt 音频已支持。[S1、S3、S5]

未知：Anchor 多 provider/别名切换的真实端到端、每个模型的图片能力与格式映射、二进制资源/音视频、媒体 Artifact 来源/权限及真实 provider 凭据。现有 spike 固定 fixture model 且拒绝媒体，不能补这些验收。

## 6. 官方 Rust 二进制与独立 bundle 闭包

- 官方 v1.53.0 于 **2026-10-02T18:44:11Z** 发布，提供 Linux GNU/musl、macOS 和 Windows CLI archive。Linux 发布 workflow 以 Cargo 构建 `goose-cli --bin goose`，将编译出的 `goose` 打包；CLI 自带 `goose acp` stdio 入口，无须为了该入口另装 Python agent 包。[S18、S19]
- Linux x86_64 musl `.tar.bz2` 的官方 archive digest 为 `4124f3b56dcebf1f396ddddaa66d68cf710318dcf947d8c81de6eb5866af11d7`；A112 解包 binary 的 hash 是另一层身份，不能混用。官方资产/API 元数据已核验，本次未重新下载或启动二进制。[S18；`docs/goose-acp-spike.md:29`]
- **“Rust 官方 binary”不等于“完整部署没有外部依赖”。** stdio 扩展仍会执行声明的外部命令；若 Plugin 选 Python/Node 或外部 ACP agent，其解释器、工具、认证及服务仍属于部署闭包。Goose 进程与 MCP 协议隔离这些依赖，不会消除它们。[S15]
- Anchor 当前目标是标准生产平台与官方可执行工具不要求 Python/venv，而允许第三方 Plugin 自有依赖；独立包必须包含同一 Anchor Runner、Graph、显式 Plugin/资源和受控 Goose binary，配置/密钥/本地授权由部署者提供。Goose recipe 或 ACP session 不能替代这个 Graph 闭包。依据：`docs/product-architecture.md:93`、`docs/product-architecture.md:103`。

未知：无 Python 干净机器安装验收、最小依赖清单、跨平台 Sandbox、离线启动/禁止隐式下载更新、license 分发清单、版本升级兼容及完整独立 bundle。只核实官方预编译入口与架构契合，不宣布 Anchor 标准部署/分发已完成。

## 最小验收建议（全部是建议，本次未执行）

1. **精确执行面**：固定 v1.53.0/hash，覆盖干净和污染配置、recipe、项目 Plugin；检查实际模型收到的全量目录，直接调用未列出工具也必须被拒绝；空 `available_tools` 不能误作 deny-all。
2. **Plugin 垂直切片**：一个已有 Anchor Plugin，短目录 → 说明/资源只读读取 → 真实本地工具 → Run 绑定摘要/Artifact；额外 Plugin、hooks、自动更新不得隐式加入。只替换模型传输，不用假 ACP 代替真实 Goose。
3. **安全反例**：分别测试 Goose 进程与业务工具的越界文件/命令、未授权 MCP/出站目标、宿主敏感环境与假 credential canary；拒绝 fs/terminal 回调不能代替 OS 边界。
4. **provider/媒体**：至少两种本地确定性 provider 协议配置与模型别名绑定，加一个图片输入及文本 resource；不支持的 audio/blob 明确拒绝，不静默降级。真实 provider 接入另作低频验收。
5. **部署闭包**：干净、无 Python/venv 且禁止隐式下载的环境中，官方固定 Goose + Anchor 原生 Host/工具运行一个独立 Graph；平台与 standalone 使用同一 Runner。含 Python 第三方 Plugin 的包单列依赖，不改变标准包声明。

## 已核验官方来源 URL

所有 Goose 源码/文档 URL 默认固定 **v1.53.0**；唯一后续版本对照是 S7。通过 web.run 打开官方页面及 GitHub 官方 API 读取固定源码/协议核验；搜索查询共 3 个，之后未新增搜索。本次没有模型或产品验收结果。

- **S1 ACP 初始化与 MCP 协商**：https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/06128106cf54b806a491a0fcf2b6bc7022ef57ca/docs/protocol/v1/initialization.mdx ；https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/06128106cf54b806a491a0fcf2b6bc7022ef57ca/docs/protocol/v1/session-setup.mdx
- **S2 ACP 扩展边界**：https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/06128106cf54b806a491a0fcf2b6bc7022ef57ca/docs/protocol/v1/extensibility.mdx
- **S3 Goose server**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/acp/server.rs （`initial_session_extensions`、`selected_builtin_extensions`、`convert_acp_prompt_to_message`、`on_initialize`）
- **S4 client 私有 meta**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/acp/server/new_session.rs （`meta_goose_extensions`）
- **S5 私有方法及 schema**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/acp-schema.json ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/acp/server/custom_dispatch.rs
- **S6 provider/model 配置调用**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/acp/server/dispatch.rs （`SetSessionConfigOptionRequest`）
- **S7 非发布版修复**：https://github.com/aaif-goose/goose/commit/540df77c30e3c1f3b51915cb18ae081931c96851
- **S8 精确工具限制**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/extension.rs （`is_tool_available`）；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/extension_manager/mod.rs （目录过滤、`dispatch_tool_call_inner`）
- **S9 hooks 本地命令**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/hooks/mod.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/agent.rs （`HookManager::load`）
- **S10 skills**：https://github.com/aaif-goose/goose/blob/v1.53.0/documentation/docs/guides/context-engineering/using-skills.md ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/skills/client.rs
- **S11 Goose Plugin 与 MCP 导出**：https://github.com/aaif-goose/goose/blob/v1.53.0/documentation/docs/guides/context-engineering/plugins.md ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/plugins/mcp_servers.rs
- **S12 resources/prompts**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/extension_manager/mod.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/acp/server/resources.rs
- **S13 ACP 权限请求与工具展示**：https://raw.githubusercontent.com/agentclientprotocol/agent-client-protocol/06128106cf54b806a491a0fcf2b6bc7022ef57ca/docs/protocol/v1/tool-calls.mdx
- **S14 fs/terminal fallback**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/acp/fs.rs
- **S15 stdio 进程边界**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/agents/extension_manager/stdio.rs ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/subprocess.rs
- **S16 凭据来源**：https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose/src/providers/provider_secrets.rs
- **S17 多 provider**：https://github.com/aaif-goose/goose/blob/v1.53.0/documentation/docs/getting-started/providers.md
- **S18 官方 release/API**：https://github.com/aaif-goose/goose/releases/tag/v1.53.0 ；https://api.github.com/repos/aaif-goose/goose/releases/tags/v1.53.0
- **S19 官方 binary 构建/入口**：https://github.com/aaif-goose/goose/blob/v1.53.0/.github/workflows/build-cli-linux.yml ；https://github.com/aaif-goose/goose/blob/v1.53.0/crates/goose-cli/src/cli.rs

本地产品目标：`docs/product-architecture.md:52`、`docs/product-architecture.md:93`、`docs/product-architecture.md:103`、`docs/product-architecture.md:296`、`docs/product-architecture.md:467`。当前事实以 `docs/architecture.md:257` 和 `docs/goose-acp-spike.md:1` 为准，不将目标设计当作完成状态。
