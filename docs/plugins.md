# Plugin

Anchor Plugin 以安装后的目录为资产。安装器可接收社区来源并放入共享 Library；运行时按 ID 从 Library 插件目录读取 `plugin.json`。来源格式只影响安装时怎样定位清单，其他资源保持来源结构。

```text
library/plugins/<id>/
├── plugin.json
├── skills/<skill>/SKILL.md
├── .mcp.json                 # 可选 MCP 声明
└── 其他来源资源
```

Graph 的 AgentNode 保存节点自己的 Plugin 引用，`Op.run` 只接受 `network` 系统能力引用，不挂载普通 Plugin。Library 校验清单与 Skill 路径，Host 在节点调用时按当前安装内容只读挂载到 `/plugins/<id>`，实际依赖视图记入 Run 观察；升级从下一次节点调用起生效，已提交步骤不重放。Plugin 文件由安装流程或操作员维护，Graph 无权添加任意宿主路径。

## 系统插件与普通插件

`network`、`graph-tasks`、`feishu`、`wecom` 是随 Anchor 发行的系统插件；`git`、Anchor Dev 和社区来源属于普通插件，即使发行包附带也不因此获得系统身份。身份由 Host 对保留 ID、当前发行的公开资源和可执行资源进行验证，不接受候选清单自报。

系统插件保留 Plugin 的发现、详情、节点选择和配置入口，但不能经普通插件安装、更新或卸载路径替换或移除。Web 显示“系统插件”与“随 Anchor 升级”，操作只消费后端的 `canReplace`、`canUninstall`，不按名称推断。系统目录损坏时仍受保护并显示不可用，不会退回普通插件或获得执行资格。

首次安装由发行资源提供；Host 启动只补齐缺失项，不覆盖现有代码。系统升级使用操作员维护路径，核验候选 digest 与现有 digest 并在 Library 发布锁内执行；普通发布不能冒用保留 ID。安装、升级或系统身份都不自动挂载、开启联网、授权 Graph 任务或提供凭据。

### 开发系统插件

系统插件的判据是启停它会改变 Anchor 专有的 Host 行为；单纯提供 MCP、Skill 或随包附带不构成系统身份。新增系统插件需要修改并构建 Anchor，不是给 `plugin.json` 增加 `system` 或 `kind` 字段；清单不接受此类身份声明，也没有动态注册系统能力的接口。

| 接入面 | 现有入口与约束 |
| --- | --- |
| 身份与公开资源 | 在 `rust/anchor-graph-host/src/system_plugins.rs` 的 `SystemPlugin`、`all_system_plugins()`、`id()`、`public_files()`、`executables()` 中登记；资源集合必须精确一致，公开文件字节由当前构建固定 |
| Library 生命周期 | 复用 `Library::install_system_directory`、`upgrade_system_directory` 和原发布锁；缺失项安装不覆盖，升级核验候选与现有摘要；不另建注册表或绕过普通安装保护 |
| Host 联动 | 行为归对应应用服务，工具适配参考 `graph_task_tools`、`channel_tools`，由 `execution/tool_assembly.rs` 注入 `NodeToolAssembly`；下层不反向调用应用，Kernel 不承载插件业务 |
| 发行与源码启动 | 平台公开资源自动消费共享清单；新增原生入口还须接通 `PlatformPackageRequest`、CLI、构建脚本和精确 inventory；源码初始化列表归 `examples/starter.json`，不要另复制系统身份清单 |
| RSI 证据 | 在 `rust/anchor-rsi/src/evidence/system_plugins.rs::source_anchors` 关联契约、实现与回归位置；当前资源身份不等于原生二进制来源、历史实现可恢复或测试已通过 |
| Web 与 API | 复用 `kind`、`canUninstall`、`canReplace`、`updateSource` 的 Host 投影，不按 ID 在前端另写系统判断；新增业务配置再沿现有配置与权限事实接入 |

不需要独立进程的 Host 能力可参考 `graph-tasks`，不必添加空的 MCP server 或工具二进制。需要任务联网时复用节点 `network` 策略和现有宿主上界，不能因系统身份自动授权；当前 `Op.run` 仍仅支持 `network` 引用，新增系统插件不会自动获得 Op 执行资格。

新能力至少验证资源或清单冒用失败、普通替换/卸载受保护、未启用时无 Host 工具、启用后仅获得授权范围，以及进行中 Turn 不被在线配置扩权；已有通用生命周期用例复用，不为每个 ID 复制同类测试。生命周期回归见 `rust/anchor-library/tests/system_plugins.rs`，实际执行边界依[统一回归指南](runtime-contract-tests.md)验证。操作员维护命令见[开发指南](development.md#系统-plugin-维护)。

## 内置开发说明

源码初始列表与完整平台发行包均安装 Anchor Dev（目录 ID `anchor-steward`）及独立 `git` Plugin。Anchor Dev 仅包含入口说明和 Graph、Plugin、Rust、Python、pixi、Git 六项按需 Skill，不含 MCP server；在 Pilot 插件面板启用后才进入执行披露。它不自动启用 Git、初始化环境或扩大联网与凭据权限。Git 工具仍由 `git` 的挂载和 Host 配置控制。关闭 Anchor Dev 不删除共享工具链、项目或缓存，也不撤销其他已授权工具；具体环境边界见 [Pilot 开发契约](pilot-development.md)。

## Graph 任务

官方 `graph-tasks` 同样随源码初始列表与完整平台发行包提供，但不自动挂载或授权。它不启动自己的 MCP server；挂到 AgentNode 后，由 Host 将 `graph_tasks_*` 工具关联到该 Plugin 的按需披露目录。仅安装或打开 Session 开关不够：须在 Graph 编辑器给这个节点选择可见 Graph，按需开启构建 Graph、候选 Plugin 开发和新建 Graph 的能力上限，再由操作员保存。新建或 Agent 编写的源 Graph 也须经这一保存步骤；已开始的普通 Run 或已冻结 Turn 不因此扩大权限。字段示例见 [作者契约](rust-graph-authoring-contract.md#按需-graph-任务授权)。

既有目标按批准的确切定义及静态调用闭包执行；新建 Graph 只能用节点授权的已安装 Plugin、网络与沙箱命令。其中 `graph_tasks.network` 只允许新建的子任务联网，不修改当前助手的联网策略。Agent 不能修改源助手或无关资产，不能让子图继续委派任务。后续运行同时检查原授权与当前授权，撤回可见范围立即收窄；资产改版不自动执行未经批准的新内容。身份、请求键和结果路由由 Host 派生，不接受模型传入。普通独立宿主若没有这些 Host 端口，不因携带此 Skill 而获得调度或 Session 能力。

工具支持立即异步运行、一次/多时刻/有限次数/重复计划，以及原会话的结果处理；即时调用遇到同 Graph 未结束 Run 返回冲突，计划则保留未接纳时刻等待重试。候选 `prepare`/`test` 复用当前沙箱与环境，只验证所测范围，不安装、不挂载、不取得新增凭据；正式安装仍走既有预检和确认。完成结果与用户来信分开，发送消息须核对实际渠道回执。调度、冻结与恢复的事实归属见 [当前架构](architecture.md#按需-graph-任务与结果输入)。

助手创建且已成功收束的一次性 Graph 会自动退出 Web 主列表，收进“已完成的一次性任务”；可展开或搜索，定义与运行记录保留。同图仍有待执行计划、未完成 Run、未处理结果、未执行的定义修改或共享引用时不收纳，周期计划及手工 Graph 也不因此收纳。再次安排或执行后自动回到主列表；这只是目录展示，不取消计划、删除历史或改变任务权限。

## 连接配置与凭据

Plugin 声明配置需求，Host 保存每个 owner 的一份默认连接配置。Web 的插件详情提供普通字段、凭据设置/轮换/清除与显式连接测试；Pilot 的专用凭据表单映射到同一事实。配置属于 Host，不修改 Library 清单，也不随 Plugin/Graph 包分发。

清单可增加 `configuration.fields`：稳定字段 ID 对应 `type`（`string`、`url`、`secret`）、`label`、`required`（默认 true）和可选 `default`（仅普通字段可设）。`configuration.environment` 按 MCP target 显式把环境名映射到字段 ID。同一个字段可被多个 server 消费，所有值与 revision 原子保存、一次解析；不能因环境名相同就隐式共享。凭据字段禁止默认值和 `${...}` 模板，只允许经 stdio 环境、Bearer 或环境 Header 注入。带配置声明的 Plugin 必须将所有非 Host 路径模板变量显式映射为普通字段，不能把可变端点留在旧凭据入口以绕过连接身份校验。

Docmost 的 `base_url` 和 `api_key` 同时供远程页面 MCP 与本地附件 MCP 使用。网页填写一次 Key，后续轮换无需修改 `.env` 或重启 Host。环境名 `DOCMOST_API_KEY` 只是 Host 向两个实现注入凭据的适配名称。

MCP 默认只读取 owner 的托管配置；进程中存在同名环境变量不会锁定网页，也不会成为缺失值的回退。清除字段保存明确的空值，不能回退到旧环境或清单默认值。更换 URL 时，未在同次更新明确设置的托管凭据会清除，避免把旧 Key 发到新服务。普通字段可回显，凭据只返回保存状态；存储目录 0700、文件 0600，凭据不进入公开 DTO、聊天、原生历史、日志投影或浏览器持久存储。

运维托管通过 `ANCHOR_PLUGIN_EXTERNAL_CONFIGURATION` 显式声明，例如 `{"docmost":{"api_key":"DOCMOST_API_KEY"}}`；旧无配置声明的 MCP 使用 `{"plugin-id":{"server.ENV_NAME":"ENV_NAME"}}`。声明的来源必须匹配插件消费的环境名，此时字段只读且仍不回显凭据。缺失的外部值直接失败，不退回托管值。切换来源属于部署授权变化，不会自动把部署凭据复制给所有用户。渠道的部署归属沿用渠道契约。

保存携带当前 `authorization_digest` 和 `revision`，冲突要求重新读取；声明/权限变化使旧配置失配。新 Turn/节点 invocation 读取最新配置，进行中的连接保留快照。恢复同一个 invocation 校验已保存的 owner、目标与配置来源绑定；在同一凭据引用内可轮换 Key，端点/来源改变不能静默接续。绑定事实不保存凭据或凭据哈希，随 Run 删除清理。旧记录没有此绑定时不据此补写历史结论。

Pilot 的 MCP 凭据请求在创建时绑定配置 revision 与不含凭据值的连接身份，提交前在同一写入锁内重新校验；配置变化、清除或旧请求同步失败后，旧表单不能覆盖新值。配置变更后可在同一 Turn 申请新请求；升级前没有版本绑定的待填写 MCP 请求需重新申请，不自动授权提交。渠道凭据保留独立生命周期。

HTTP MCP 可声明 `channel_identity_headers`，例如 `{"X-Channel-Sender":"sender_id"}`，请 Host 证明本次回答的是哪位渠道成员。可选字段为 `source`、`account`、`conversation_kind`、`conversation_id`、`sender_id`；值只来自 Host 已准入的渠道输入，不来自模型、配置或工具参数。同名 Header 不能出现在 `headers`、`http_headers`、`env_http_headers` 中，也不能是 `Authorization`。声明属于宿主授权，计入 `authorization_digest` 并在安装审阅中显示；未声明的 Plugin 授权不变。声明了的 server 只在常驻助手处理渠道消息时连接：非渠道调用、一轮里折叠了不同成员的消息、或值无法作为 Header 发送时，该 server 不启动，其他 server 照常。连接测试不代表任何成员，因此不连接此类 server，结果为“不支持”并说明原因。服务端应同时校验 Plugin 自己的凭据，只信任来自该凭据请求中的这些 Header。

连接测试复用执行时的准备、权限、沙箱及 MCP transport，只进行初始化与工具发现，不调用业务工具。返回结果绑定配置 revision；测试期间配置改变则标记结果过期。stdio 发现成功只证明本地工具可启动，不证明上游 Key 有效。测试成功、远程认证和真实业务验收分别记录。

## 工具与模型边界

Rust Host 使用 RMCP 读取 MCP server 的真实工具清单和 schema，并通过 Goose ACP/MCP 接入 AgentNode。工具归属来自 Plugin/server 的实际装配，说明、Skill 和 schema 经固定发现入口按需读取。Goose 负责模型循环、Provider、工具交互、原生会话和 compaction；Anchor 负责 Graph、Run、Artifact、Sandbox、Plugin 和宿主权限事实。

stdio server 在现有 Bubblewrap 节点边界中运行，Plugin 挂载只读；HTTP MCP 要求本次有效联网授权，凭证与 headers 由声明在绑定时展开。模型不能提供新的 host path、服务凭证或授权。节点完成使用受限 `final_result`，其路由与工具结果由宿主校验，不把普通文本当作成功完成。

远程 HTTPS MCP 由 `anchor-mcp-host` 显式启用 RMCP 的 TLS 支持，使用系统信任证书并验证服务域名；不可信证书和域名不匹配均拒绝连接。能建立 HTTPS 连接不代表凭据有效，服务返回 401 时仍需修正该插件的凭据。

当前兼容根清单、Skill/资源读取、只读挂载和 MCP；来源中的 `hooks`、`commands`、`agents` 不作为额外执行入口。当前产品不依赖旧 Tool Search、CodeMode 或另一套 Agent loop。

## 发现与读取

Pilot 与 AgentNode 共用 `PluginDisclosureCatalog` 和 `ToolDisclosure`。`anchor_plugins` 搜索本次准备并获授权的 Plugin；`anchor_plugin_read` 按 section 读取概览、Plugin 使用说明、Skill/资源目录、单个 Skill/资源或安全的 server 声明。`anchor_tools` 搜索完整授权工具目录，`anchor_tool_detail` 返回所选工具的完整 description、参数 schema 和归属，再由 `anchor_tools_call` 调用。发现入口始终可见，普通提示也说明用法；`tom` 只承载短边界提醒，不保存完整目录，也不是发现能力的前提。Pilot 的 `plugin_list`/`plugin_read` 查询已安装 Library，执行发现查询本 Turn 的冻结挂载；管理查询不赋予调用权限。

单项 schema 与调用入口遇到本次未提供的工具名时，共用指向当前 `anchor_tools` 目录的错误提示；工具名必须来自本次发现结果，不能根据旧对话猜测。错误不推荐未授权工具，也不自动扩大挂载范围。

同一份冻结目录还派生不超过 16 KiB 的能力摘要，通过聚合 MCP 的 `ServerInfo.instructions` 进入 Goose 原生扩展系统提示：逐项提供 ID、名称、用途简述、准备状态、实际工具数量和读取入口，不附完整 Skill、资源、schema 或凭据。名称和描述分别取首个非空行、最多 160 字符，并以 JSON 编码；预算之外返回 `total`/`shown`/`omitted` 与完整检索入口。元数据不是执行指令，未显示不代表不可用；`available` 说明定义可发现/读取，实际调用仍取决于当前授权工具。正常推理和压缩后的继续请求都由 Goose 携带当前摘要，框架内部专用摘要请求使用自己的提示，不在此承诺内。新 Turn 或原生 Session load 后重新装配、校准扩展，当前摘要替换旧摘要，原生历史仍保留；直接 schema 和渐进 schema 模式均适用，关闭 `tom` 不影响摘要。

目录默认 20 项、最多 100 项，同时限制为 16 KiB JSON 页，并返回 `next_offset`；检索为完整 Plugin/工具元数据和 Skill 头部的大小写无关多词 AND 子串，支持中文、精确 ID 和分页兜底。显示摘要限 160 字符，检索不使用裁短后的摘要。选中详情限 256 KiB JSON，工具 schema 超限明确报错，不伪装成完整结果。UTF-8 文本文件最多 1 MiB，按字节 offset 分页（默认 16 KiB、最大 32 KiB），返回 SHA256；续页传 digest 检测正文热更新，以 `next_offset=null`/`end_of_file=true` 结束。压缩后按相同入口重读，继续使用 Goose 原生 compaction，不增加 Anchor 记忆或上下文系统。

读取配置文件的分类来自已验证、规范化的清单路径；隐藏文件、`plugin.json`、MCP 配置与 `channel.json` 不由披露工具返回。执行读取在 Library 非阻塞共享 guard 内核查当前授权摘要及公开文件分类，只读准备时与当前公开路径的交集，然后安全打开文件；正式发布持同一排他 lease。发布忙时明确拒绝本次读取以供重试，授权变化要求新的 invocation，普通正文更新仍可读。Pilot 管理入口同样在 guard 内读取定义及 Skill 元数据。披露目录是可重建投影，不新增持久资产、Plugin 历史副本或授权源。

`anchor_tools_call` 在 Bridge 保存观察前解析真实目标和参数，保留外层 ACP 原生调用身份，继续交给原授权端口。Pilot 保留 native peer/elicitation，Node 保留真实工具观察、媒体、错误、回执与取消收束；递归元工具及间接 `final_result` 被拒绝。工具 schema 的披露沿用 `ANCHOR_NODE_TOOL_DISCLOSURE`：`1` 强制、`0` 保留直接 schema 模式，未显式设置时隐藏工具超过 3 个自动启用；Pilot 固定管理工具继续直接可见。目录与单项读取入口不依赖此开关。MCP 仍按现有准备流程连接，未增加懒连接或 Code Mode，实际通过范围见 [验收摘要](pilot-development-plan.md)。

## 安装与更新

插件库页面支持普通插件从 GitHub tree URL 或本地插件目录预检、安装与更新；确认使用同一个应用安装前置条件和 Library 发布锁，Pilot 的 `plugin_install` 保留原生确认入口（见 [开发与交付](pilot-development.md#候选准备测试与交付)）。本地导入只接受普通文件列表，最多 2048 个文件、解码后合计 32 MiB；联网获取源码或依赖需要明确授权。卸载展示会话/未结束 Run 的阻止原因及 Graph 影响，并确认删除插件自身数据。操作员 CLI（`anchor-library --root <Library> install --directory <候选目录> --id <id>`）**拒绝**安装声明了运行期依赖的插件——它没有求解依赖、落地视图的宿主能力，装了也起不来；`--review` 仍可完整查看该候选。Plugin 自己声明运行期依赖时，宿主在安装时按部署授权的来源求解并落地到共享存储，再为该插件版本生成视图；求解或取包失败就不装，并把工具原文报给人。声明写在清单顶层：`"dependencies": {"python": ["idna==3.6"]}`（清单顶层字段是**封闭白名单**：`name`/`description`/`version`/`author`/`homepage`/`repository`/`license`/`keywords`/`interface`/`skills`/`mcpServers`/`configuration`/`dependencies`/`network`/`mutable`/`hooks`/`commands`/`agents`/`apps`，其他键一律拒绝；这是有意的，宁可让作者发现问题，也不要静默忽略一个拼错的字段），要用视图的解释器就把 server 的 `command` 写成 `${ANCHOR_PLUGIN_VIEW}/bin/python`。插件自己的可变数据用顶层 `"mutable": ["data/", "cache.db"]` 声明（相对插件目录、不含 `..`、不能覆盖 `plugin.json`、`.mcp.json` 或外置 MCP 声明文件；末尾 `/` 表示子树）：这些路径是插件进程**唯一**可写的地方，平台从不读、不复制、不覆盖它们，代码摘要与插件包清单也不包含它们——同一份代码无论数据长到多大，摘要都不变。插件只有一份（`plugins/<id>/`），Graph 包只写 `{"id": …}` 引用：升级插件不需要改包，正在运行的 Run 从**下一次节点调用**起用新代码，已完成步骤不回滚、不重放，它实际用过的视图记进 Run 记录作为观察。OAuth 从 Plugin 面板显式授权，owner-bound token 保存在操作员 Library，凭证不进入 Graph 包。授权、刷新与 MCP 连接有确定性本地测试；真实 provider 和回调部署需独立验收，不能声明所有社区 server 兼容。

## Pilot 任务联网

固定 ID 为 `network` 的系统插件是任务联网的策略入口，也提供通用 HTTP 请求和下载工具。Pilot 与 Graph 节点共用一个三态控件，不再同时提供主动挂载开关与禁止联网按钮。实际网络权限由 Host/Sandbox 执行；学术调研等业务 Plugin 继续使用自己的客户端，不需要把请求转交给联网工具。

### 启用与授权

| 策略 | 结果 |
| --- | --- |
| `auto`：自动（按插件依赖） | 仅在已挂载 Plugin 声明联网需求时启用；最后一个依赖者关闭后停止，没有需求时任务工具在无网沙箱中运行 |
| `enabled`：开启联网 | 主动启用，业务客户端与通用联网工具可用，仍受宿主授权约束 |
| `denied`：关闭（禁止联网） | 所有任务工具均不能联网；需要联网的 Plugin 工具不装配，离线工具继续运行，插件依赖不能重新开启 |

Graph 把策略写在节点的 `plugins` 引用中，例如 `{"id":"network","network":"denied"}`；同一个角色或命令可被多个节点复用，改一处节点不修改其他节点或角色。联网引用不是普通的默认或必需挂载：保留 `required/default` 不会覆盖策略，`denied` 无需有效 provider 也能离线执行，`auto` 只按其他插件的需求解析 provider，`enabled` 则主动解析。`Op.run` 的插件面板只显示联网策略。普通 Plugin 的选择不会删除已保存的 `auto` 或 `denied`。子任务中的策略引用不另外要求将 `network` 加入插件允许列表，但实际联网仍受 `graph_tasks.network` 上界约束。字段与旧定义的读取边界见 [Graph 作者契约](rust-graph-authoring-contract.md)。

Pilot Session 使用同一三态语义，由 Host 将既有 `plugin_mounts.network` 与 `network_allowed` 一次性保存，不建立另一份策略事实；依赖启用只是当前挂载集合的投影，不额外写入挂载开关。网页与 Responses API 新建 Pilot 默认 `enabled`，用于初始化与开发；已有会话不补开，用户设置 `auto` 或 `denied` 后不会因续聊或重启再次主动开启。渠道和业务 Graph 不增加此默认，其联网设置归 Graph 节点。

联网能力要求 Library 已安装且验证有效的 `network` 系统插件；损坏或缺少时显示不可用，不自行下载，也不因业务 Plugin 的联网声明绕过校验。策略与有效访问分别展示；因 `denied` 显示不可用不妨碍用户修改策略，Host 对开启请求重新校验。

联网策略变更只影响下一 Turn，不安排自动接续；同一 Turn 重试使用首次冻结的挂载和授权。常驻助手由操作员保存 Graph 后在下一条消息采用新策略，普通 Graph 用于后续 Run；已开始的普通 Run 和已冻结 Turn 不被重写。冻结授权无法读取时本次调用失败，不以实时设置补开网络。模型 Provider、渠道连接属于宿主控制通道，不受任务联网策略影响。

### 联网声明

清单顶层 `"network": true` 表示该 Plugin 的任务工具需要联网，不是授权；省略为 false，非布尔值拒绝。远程 HTTP/SSE MCP 的连接本身产生联网需求，Host 自动识别。该字段不改变 `dependencies.python` 的包依赖语义，也不引入通用 Plugin 依赖解析器。

官方学术调研与企业微信声明了联网需求，Docmost 的远程 MCP 自动产生需求。stdio 进程按本轮有效授权使用 Bubblewrap 网络隔离，HTTP MCP 与 OAuth 装配使用同一授权。`denied` 对本次 Agent/任务工具整体生效，不是只关闭通用 HTTP 工具；当前不按域名或单个 Plugin 划分出站网络。

### 通用联网工具

| 工具 | 输入、结果与限制 |
| --- | --- |
| `network-http_request` | HTTP(S) 方法、headers、文本 body；返回最终 URL、状态码、响应 headers、文本及截断标记；默认读取 64 KiB、最多 1 MiB，超时 30 秒，最多 5 次重定向 |
| `network-http_download` | 下载到工作区相对路径，默认最多 16 MiB、上限 64 MiB；超限不留下目标文件，已有文件不覆盖 |

下载逐级通过目录 FD 打开且不跟随符号链接，写入限于 Host 授权的工作区。用法见[联网 Skill](../plugins/network/skills/network/SKILL.md)，官方发行构建见[发行 builder](../rust/anchor-distribution/README.md#complete-platform)；系统资源随发行安装，不走普通插件候选替换。实际验收范围统一见[验收台账](pilot-development-plan.md)，本地服务或浏览器夹具不等于真实模型/公网验收。

## 官方工具和外部工具环境

官方学术 CLI、Docmost 附件 MCP、WeCom 应用 MCP 和机器人 Gateway 都是 Rust 二进制。Docmost Plugin 的远端 HTTP MCP 使用 Host 注入的共享 `api_key`；本地 `anchor-docmost-tools` 只上传授权的 `/in/publish/assets` 普通文件，不绕过发布门禁。

操作员仍可登记第三方工具的显式 entrypoint、environment 和 imports。Library 的 `tools/<id>/tool.json` 可选 `plugin` 字段把该登记限定到指定 Plugin 的 MCP 启动；省略则沿用部署共享环境。限定登记不会进入普通节点命令或其他 Plugin，带依赖视图的 MCP 也保留自己的限定登记。登记由 Host 启动读取，变更需要重启 Host。Host 保留解释器符号链接、版本化解释器、显式环境路径及 PYTHONPATH 等外部格式兼容；使用其他语言的第三方工具由部署者提供其运行环境。这不增加官方构建、运行或回归的解释器依赖。环境必须由操作员稳定维护，Graph 与 Plugin 不自行安装依赖或扩大挂载。

## Git 项目维护

`git` 是普通本地 MCP Plugin，调用沙箱 PATH 中的本机 Git，与 AgentNode/Pilot 的编辑、命令工具共用 `/workspace`；安装它不需要修改或重启 Host。`git_run` 接受原生 Git 参数，可克隆、维护分支、查看差异、提交和推送。`git` 本地入口始终可用，SSH/HTTPS 入口分别在相应凭据完整后出现；凭据复用 owner-bound 插件配置，只注入对应 MCP，不自动继承宿主 SSH 身份。远程操作沿既有联网授权，本地开发可离线。插件不独立实施仓库/分支权限，实际访问范围由 Git 服务账号决定。安装、参数和验证边界见 [Git Plugin](../rust/anchor-git-tools/README.md)。

操作员也可通过 `plugin="git"` 的工具登记只读授权原有 SSH 路径，并配置 `ssh_config`，由 `host` MCP 入口复用原有身份、主机信任与别名；仅填写配置路径不会扩大沙箱挂载。该宿主授权接入需要支持限定登记的 Host，普通 Git 插件安装仍独立于它。`Host key verification failed` 只说明主机信任校验失败，不能推断私钥缺失。

## Pilot 获取、开发与交付

Pilot 在 Session 工作区获取和编辑候选，经 `plugin_prepare`、`plugin_test`、审查和原生确认安装；正式包与依赖视图独立于开发目录。私有 Python 环境、Rust 工具链、冻结权限、取消与清理的唯一操作契约见 [Pilot 开发指南](pilot-development.md)。

管家不能修改或删除定义它自己的 Graph，也不能安装为它自己提供指令的 Plugin；操作员接口承担这类维护。完整工具格式见 [作者 Skill](../plugins/anchor-steward/skills/plugin-authoring/SKILL.md)。

## 企业微信

`plugins/wecom` 的应用 API MCP 入口是 `bin/anchor-wecom-tools`，提供 `wecom_send_text`、`wecom_send_markdown` 和 `wecom_get_user`。通道声明 `channel.json` 指向 `bin/anchor-wecom-gateway`；Host 的 ChannelSupervisor 按服务生命周期监管唯一网关，不在每个 Run 中新建平台连接。

Bot 长连接凭证与应用 API 凭证分开配置；普通助手 Graph 按成员允许名单接收私聊，Host 保存每位用户的 Session/Turn 与取消事实。Gateway 保存 ACK、unknown 和 suppression 投递账本，不自动重发未知平台请求。当前 Gateway 的官方媒体 URL 下载、AES 解密和富媒体回传尚未完成公网验收；Host 的本地规范化附件契约不能替代这些能力。

安装、配置和验收边界见 [企业微信助手接入](wecom-assistant.md)、[原生应用工具](../rust/anchor-wecom-tools/README.md) 与 [Gateway](../rust/anchor-wecom-gateway/README.md)。真实服务和真实 Provider 的范围见 [验收摘要](pilot-development-plan.md)。

## 飞书

`plugins/feishu/channel.json` 声明 `bin/anchor-feishu-gateway`，复用 Host 的渠道管理和常驻助手生命周期；不声明 MCP server，不给模型应用凭据。每张 Graph 独立配置 `FEISHU_APP_ID`、`FEISHU_APP_SECRET`、接收/回复成员 `open_id` 和启用意图，须 `private_only:true`。支持文本、图片和文件接收、Goose 真实活动进度卡及 Markdown 回复。Host 挂载 `feishu_send_message({content})`，仅在可信任务结果轮通知原私聊；`feishu_send_file({path,content,context_files?})` 可在用户轮发送工作文件，任务轮仅发送确切选中的不可变产物，两者都不能指定机器人或收件人。普通来信仍用 `session.reply`，任务结果轮须以通知工具的 confirmed 为外发证据，不把 handled 当成已发送；文件和介绍分别确认。大小、文件保留、格式和额外应用权限归[飞书助手](feishu-assistant.md)。Graph Tasks 的计划与网络权限另行授权；不支持群聊、音视频、内联外发图片、通讯录或任意收件人发送。飞书后台需要先有在线长连接，再保存订阅方式、添加接收消息事件并发布应用。实现与真实业务验收分别记录，不以连接状态代替收发成功。

## 界面与交互

Pilot、Graph 和 Run 共用插件卡片、搜索与详情：列表显示名称、用途和状态；标识、内容摘要、Skill、MCP 与使用说明放在详情中。手机以抽屉打开会话插件，详情支持键盘关闭并恢复焦点。

会话选中后预读一次插件列表，仅面板展开时继续轮询；反复开关共用未完成的请求并保留已确认列表。首次等待显示加载占位，读取失败可重试，收到空列表后才显示未安装；切换会话取消旧请求。Host 列表读取实时目录信息和当前依赖视图，不读取程序文件正文计算代码摘要；详情与执行继续读取完整定义，不缓存跨请求的权限或可用性。

列表先显示 50 个卡片，可继续加载；搜索和筛选覆盖完整目录，改变条件后重新从首批显示。插件数量和已挂载数量始终统计完整结果。

| 入口 | 操作及事实来源 |
| --- | --- |
| 插件库 | 普通插件安装/更新/卸载和当前凭据管理；系统插件只显示发行升级说明，预检、确认与执行结果分别显示 |
| Pilot 会话 | 除联网外的插件用开关提交挂载请求，联网控件提交三态策略，以 Host 返回状态为准；仅操作后显示保存与接续反馈 |
| Graph 节点 | 插件开关与联网控件编辑本节点草稿；常驻助手经操作员保存后下一 Turn 生效，普通 Graph 用于后续 Run；查看详情不提交草稿 |
| Run 节点 | 只读展示保存的绑定；常驻助手显示当前或最近一轮冻结的插件，不读取当前 Library 补写历史，也不提供挂载或授权操作 |

联网策略、依赖启用与有效访问分别展示；Host 未返回策略时显示未知并禁用修改，不从有效访问反推策略。写入失败保留最后确认的模式与原始错误。其他插件的挂载请求可能在当前回复结束后继续原任务，联网策略修改不触发接续。OAuth 操作仅在当前库详情提供，读取失败显示未知，可重试；撤销失败保留最后确认的状态，授权窗口打不开时提供手动链接。

插件配置详情展示普通字段及缺失、已保存、部署管理或授权声明失配，已保存不等于外部服务验证成功。详情页与 Pilot 使用同一 owner-bound 存储；独立保存只完成同一目标、仍有效的待填写请求，旧请求不会覆盖新值，已处理请求的历史保留。工作流的消息触发对话框复用相同凭据组件，旧 Run 详情不提供当前配置修改。
