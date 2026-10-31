# Plugin

Anchor Plugin 以安装后的目录为资产。安装器可接收社区来源并放入共享 Library；运行时按 ID 从 Library 插件目录读取 `plugin.json`。来源格式只影响安装时怎样定位清单，其他资源保持来源结构。

```text
library/plugins/<id>/
├── plugin.json
├── skills/<skill>/SKILL.md
├── .mcp.json                 # 可选 MCP 声明
└── 其他来源资源
```

Graph 的 AgentNode 保存 Plugin 目录 ID。Library 校验清单与 Skill 路径，Host 在节点调用时按当前安装内容只读挂载到 `/plugins/<id>`，实际依赖视图记入 Run 观察；升级从下一次节点调用起生效，已提交步骤不重放。Plugin 文件由安装流程或操作员维护，Graph 无权添加任意宿主路径。

## 连接配置与凭据

Plugin 声明配置需求，Host 保存每个 owner 的一份默认连接配置。Web 的插件详情提供普通字段、凭据设置/轮换/清除与显式连接测试；Pilot 的专用凭据表单映射到同一事实。配置属于 Host，不修改 Library 清单，也不随 Plugin/Graph 包分发。

清单可增加 `configuration.fields`：稳定字段 ID 对应 `type`（`string`、`url`、`secret`）、`label`、`required`（默认 true）和可选 `default`（仅普通字段可设）。`configuration.environment` 按 MCP target 显式把环境名映射到字段 ID。同一个字段可被多个 server 消费，所有值与 revision 原子保存、一次解析；不能因环境名相同就隐式共享。凭据字段禁止默认值和 `${...}` 模板，只允许经 stdio 环境、Bearer 或环境 Header 注入。带配置声明的 Plugin 必须将所有非 Host 路径模板变量显式映射为普通字段，不能把可变端点留在旧凭据入口以绕过连接身份校验。

Docmost 的 `base_url` 和 `api_key` 同时供远程页面 MCP 与本地附件 MCP 使用。网页填写一次 Key，后续轮换无需修改 `.env` 或重启 Host。环境名 `DOCMOST_API_KEY` 只是 Host 向两个实现注入凭据的适配名称。

MCP 默认只读取 owner 的托管配置；进程中存在同名环境变量不会锁定网页，也不会成为缺失值的回退。清除字段保存明确的空值，不能回退到旧环境或清单默认值。更换 URL 时，未在同次更新明确设置的托管凭据会清除，避免把旧 Key 发到新服务。普通字段可回显，凭据只返回保存状态；存储目录 0700、文件 0600，凭据不进入公开 DTO、聊天、原生历史、日志投影或浏览器持久存储。

运维托管通过 `ANCHOR_PLUGIN_EXTERNAL_CONFIGURATION` 显式声明，例如 `{"docmost":{"api_key":"DOCMOST_API_KEY"}}`；旧无配置声明的 MCP 使用 `{"plugin-id":{"server.ENV_NAME":"ENV_NAME"}}`。声明的来源必须匹配插件消费的环境名，此时字段只读且仍不回显凭据。缺失的外部值直接失败，不退回托管值。切换来源属于部署授权变化，不会自动把部署凭据复制给所有用户。渠道的部署归属沿用渠道契约。

保存携带当前 `authorization_digest` 和 `revision`，冲突要求重新读取；声明/权限变化使旧配置失配。新 Turn/节点 invocation 读取最新配置，进行中的连接保留快照。恢复同一个 invocation 校验已保存的 owner、目标与配置来源绑定；在同一凭据引用内可轮换 Key，端点/来源改变不能静默接续。绑定事实不保存凭据或凭据哈希，随 Run 删除清理。旧记录没有此绑定时不据此补写历史结论。

Pilot 的 MCP 凭据请求在创建时绑定配置 revision 与不含凭据值的连接身份，提交前在同一写入锁内重新校验；配置变化、清除或旧请求同步失败后，旧表单不能覆盖新值。配置变更后可在同一 Turn 申请新请求；升级前没有版本绑定的待填写 MCP 请求需重新申请，不自动授权提交。渠道凭据保留独立生命周期。

连接测试复用执行时的准备、权限、沙箱及 MCP transport，只进行初始化与工具发现，不调用业务工具。返回结果绑定配置 revision；测试期间配置改变则标记结果过期。stdio 发现成功只证明本地工具可启动，不证明上游 Key 有效。测试成功、远程认证和真实业务验收分别记录。

## 工具与模型边界

Rust Host 使用 RMCP 读取 MCP server 的真实工具清单和 schema，并通过 Goose ACP/MCP 接入 AgentNode。工具归属来自 Plugin/server 的实际装配，说明、Skill 和 schema 经固定发现入口按需读取。Goose 负责模型循环、Provider、工具交互、原生会话和 compaction；Anchor 负责 Graph、Run、Artifact、Sandbox、Plugin 和宿主权限事实。

stdio server 在现有 Bubblewrap 节点边界中运行，Plugin 挂载只读；HTTP MCP 要求节点 `network:true`，凭证与 headers 由声明在绑定时展开。模型不能提供新的 host path、服务凭证或授权。节点完成使用受限 `final_result`，其路由与工具结果由宿主校验，不把普通文本当作成功完成。

远程 HTTPS MCP 由 `anchor-mcp-host` 显式启用 RMCP 的 TLS 支持，使用系统信任证书并验证服务域名；不可信证书和域名不匹配均拒绝连接。能建立 HTTPS 连接不代表凭据有效，服务返回 401 时仍需修正该插件的凭据。

当前兼容根清单、Skill/资源读取、只读挂载和 MCP；来源中的 `hooks`、`commands`、`agents` 不作为额外执行入口。当前产品不依赖旧 Tool Search、CodeMode 或另一套 Agent loop。

## 发现与读取

Pilot 与 AgentNode 共用 `PluginDisclosureCatalog` 和 `ToolDisclosure`。`anchor_plugins` 搜索本次准备并获授权的 Plugin；`anchor_plugin_read` 按 section 读取概览、Plugin 使用说明、Skill/资源目录、单个 Skill/资源或安全的 server 声明。`anchor_tools` 搜索完整授权工具目录，`anchor_tool_detail` 返回所选工具的完整 description、参数 schema 和归属，再由 `anchor_tools_call` 调用。发现入口始终可见，普通提示也说明用法；`tom` 只承载短边界提醒，不保存完整目录，也不是发现能力的前提。Pilot 的 `plugin_list`/`plugin_read` 查询已安装 Library，执行发现查询本 Turn 的冻结挂载；管理查询不赋予调用权限。

同一份冻结目录还派生不超过 16 KiB 的能力摘要，通过聚合 MCP 的 `ServerInfo.instructions` 进入 Goose 原生扩展系统提示：逐项提供 ID、名称、用途简述、准备状态、实际工具数量和读取入口，不附完整 Skill、资源、schema 或凭据。名称和描述分别取首个非空行、最多 160 字符，并以 JSON 编码；预算之外返回 `total`/`shown`/`omitted` 与完整检索入口。元数据不是执行指令，未显示不代表不可用；`available` 说明定义可发现/读取，实际调用仍取决于当前授权工具。正常推理和压缩后的继续请求都由 Goose 携带当前摘要，框架内部专用摘要请求使用自己的提示，不在此承诺内。新 Turn 或原生 Session load 后重新装配、校准扩展，当前摘要替换旧摘要，原生历史仍保留；直接 schema 和渐进 schema 模式均适用，关闭 `tom` 不影响摘要。

目录默认 20 项、最多 100 项，同时限制为 16 KiB JSON 页，并返回 `next_offset`；检索为完整 Plugin/工具元数据和 Skill 头部的大小写无关多词 AND 子串，支持中文、精确 ID 和分页兜底。显示摘要限 160 字符，检索不使用裁短后的摘要。选中详情限 256 KiB JSON，工具 schema 超限明确报错，不伪装成完整结果。UTF-8 文本文件最多 1 MiB，按字节 offset 分页（默认 16 KiB、最大 32 KiB），返回 SHA256；续页传 digest 检测正文热更新，以 `next_offset=null`/`end_of_file=true` 结束。压缩后按相同入口重读，继续使用 Goose 原生 compaction，不增加 Anchor 记忆或上下文系统。

读取配置文件的分类来自已验证、规范化的清单路径；隐藏文件、`plugin.json`、MCP 配置与 `channel.json` 不由披露工具返回。执行读取在 Library 非阻塞共享 guard 内核查当前授权摘要及公开文件分类，只读准备时与当前公开路径的交集，然后安全打开文件；正式发布持同一排他 lease。发布忙时明确拒绝本次读取以供重试，授权变化要求新的 invocation，普通正文更新仍可读。Pilot 管理入口同样在 guard 内读取定义及 Skill 元数据。披露目录是可重建投影，不新增持久资产、Plugin 历史副本或授权源。

`anchor_tools_call` 在 Bridge 保存观察前解析真实目标和参数，保留外层 ACP 原生调用身份，继续交给原授权端口。Pilot 保留 native peer/elicitation，Node 保留真实工具观察、媒体、错误、回执与取消收束；递归元工具及间接 `final_result` 被拒绝。工具 schema 的披露沿用 `ANCHOR_NODE_TOOL_DISCLOSURE`：`1` 强制、`0` 保留直接 schema 模式，未显式设置时隐藏工具超过 3 个自动启用；Pilot 固定管理工具继续直接可见。目录与单项读取入口不依赖此开关。MCP 仍按现有准备流程连接，未增加懒连接或 Code Mode，实际通过范围见 [验收摘要](pilot-development-plan.md)。

## 安装与更新

插件库页面支持从 GitHub tree URL 或本地插件目录预检、安装与更新；确认使用同一个应用安装前置条件和 Library 发布锁，Pilot 的 `plugin_install` 保留原生确认入口（见 [开发与交付](pilot-development.md#候选准备测试与交付)）。本地导入只接受普通文件列表，最多 2048 个文件、解码后合计 32 MiB；联网获取源码或依赖需要明确授权。卸载展示会话/未结束 Run 的阻止原因及 Graph 影响，并确认删除插件自身数据。操作员 CLI（`anchor-library --root <Library> install --directory <候选目录> --id <id>`）**拒绝**安装声明了运行期依赖的插件——它没有求解依赖、落地视图的宿主能力，装了也起不来；`--review` 仍可完整查看该候选。Plugin 自己声明运行期依赖时，宿主在安装时按部署授权的来源求解并落地到共享存储，再为该插件版本生成视图；求解或取包失败就不装，并把工具原文报给人。声明写在清单顶层：`"dependencies": {"python": ["idna==3.6"]}`（清单顶层字段是**封闭白名单**：`name`/`description`/`version`/`author`/`homepage`/`repository`/`license`/`keywords`/`interface`/`skills`/`mcpServers`/`configuration`/`dependencies`/`network`/`mutable`/`hooks`/`commands`/`agents`/`apps`，其他键一律拒绝；这是有意的，宁可让作者发现问题，也不要静默忽略一个拼错的字段），要用视图的解释器就把 server 的 `command` 写成 `${ANCHOR_PLUGIN_VIEW}/bin/python`。插件自己的可变数据用顶层 `"mutable": ["data/", "cache.db"]` 声明（相对插件目录、不含 `..`、不能覆盖 `plugin.json`、`.mcp.json` 或外置 MCP 声明文件；末尾 `/` 表示子树）：这些路径是插件进程**唯一**可写的地方，平台从不读、不复制、不覆盖它们，代码摘要与插件包清单也不包含它们——同一份代码无论数据长到多大，摘要都不变。插件只有一份（`plugins/<id>/`），Graph 包只写 `{"id": …}` 引用：升级插件不需要改包，正在运行的 Run 从**下一次节点调用**起用新代码，已完成步骤不回滚、不重放，它实际用过的视图记进 Run 记录作为观察。OAuth 从 Plugin 面板显式授权，owner-bound token 保存在操作员 Library，凭证不进入 Graph 包。授权、刷新与 MCP 连接有确定性本地测试；真实 provider 和回调部署需独立验收，不能声明所有社区 server 兼容。

## Pilot 任务联网

插件清单顶层 `"network": true` 声明联网需求，远程 HTTP/SSE MCP 自动产生需求。已安装的 `network` Plugin 可以主动启用，也会由学术调研等已挂载 Plugin 依赖启用；依赖关系不额外写挂载开关。Session 的明确禁止联网优先，stdio Sandbox 与宿主侧 HTTP/MCP/OAuth 都执行该权限，同一回合使用冻结快照。网络能力、工具与独立 Graph 的兼容边界见 [任务联网能力](network-plugin.md)。

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

## 界面与交互

Pilot、Graph 和 Run 共用插件卡片、搜索与详情：列表显示名称、用途和状态；标识、内容摘要、Skill、MCP 与使用说明放在详情中。手机以抽屉打开会话插件，详情支持键盘关闭并恢复焦点。

会话选中后预读一次插件列表，仅面板展开时继续轮询；反复开关共用未完成的请求并保留已确认列表。首次等待显示加载占位，读取失败可重试，收到空列表后才显示未安装；切换会话取消旧请求。Host 列表读取实时目录信息和当前依赖视图，不读取程序文件正文计算代码摘要；详情与执行继续读取完整定义，不缓存跨请求的权限或可用性。

列表先显示 50 个卡片，可继续加载；搜索和筛选覆盖完整目录，改变条件后重新从首批显示。插件数量和已挂载数量始终统计完整结果。

| 入口 | 操作及事实来源 |
| --- | --- |
| 插件库 | 安装/更新/卸载和当前凭据管理；预检、确认与执行结果分别显示 |
| Pilot 会话 | 开关提交挂载请求，以 Host 返回状态为准；仅操作后显示保存与接续反馈，不常驻解释内部 Turn 机制 |
| Graph 节点 | 复选框编辑插件引用；常驻助手经操作员保存后下一 Turn 生效，普通 Graph 用于后续 Run；查看详情不提交草稿 |
| Run 节点 | 只读展示保存的绑定；常驻助手显示当前或最近一轮冻结的插件，不读取当前 Library 补写历史，也不提供挂载或授权操作 |

联网插件的主动开关与依赖启用分别展示；联网许可缺失时显示未知并禁用许可修改。挂载请求可能在当前回复结束后继续原任务，网络许可修改只影响后续 Turn。OAuth 操作仅在当前库详情提供，读取失败显示未知，可重试；撤销失败保留最后确认的状态，授权窗口打不开时提供手动链接。

插件配置详情展示普通字段及缺失、已保存、部署管理或授权声明失配，已保存不等于外部服务验证成功。详情页与 Pilot 使用同一 owner-bound 存储；独立保存只完成同一目标、仍有效的待填写请求，旧请求不会覆盖新值，已处理请求的历史保留。工作流的消息触发对话框复用相同凭据组件，旧 Run 详情不提供当前配置修改。
