# 使用指南

当前安装使用 Rust Host、固定 Goose 与编译后的 WebUI。完整平台安装后直接运行 `release/anchor-runtime/bin/anchor-platform start`，自动定位随包资源及 `data/` 下的私有目录，再在 WebUI 配置模型、Plugin 和开发环境，全程不要求 `.env`。完整安装与 systemd 操作见 [部署指南](rust-production-deployment.md)；下文介绍仓库开发方式和产品操作。

## 安装与本地启动

需要 Linux x86_64、Git、Bubblewrap、slirp4netns、Rust stable（通过 rustup 安装），以及 Node.js 20.19+ 或 22.12+。构建固定 Goose 还需要 C 编译工具、curl、patch、rsync、binutils、util-linux 和 coreutils。沙箱需要可用的 user/mount/network namespace；脚本会先实际探测，失败时停止并提示，不使用共享宿主网络绕过。模型调用和 Plugin 网络权限分别配置。

Debian/Ubuntu 可先安装系统工具：

```sh
sudo apt-get install build-essential curl git bubblewrap slirp4netns patch rsync binutils util-linux
```

Rust/rustup 和符合上述版本的 Node.js/npm 需预先安装。启动脚本不会自动使用 sudo 或修改系统的 namespace 策略。

```sh
git clone https://github.com/manstein-lzn/Anchor.git
cd Anchor
./scripts/dev.sh start
```

启动后打开 <http://127.0.0.1:5173>，先在「设置 → 模型服务」保存 default Flash，再回到 Pilot 对话。5173 统一转发到 <http://127.0.0.1:8077> 的 Rust Host，页面使用已构建的 WebUI，不加载 Vite 开发依赖。脚本使用 `.local/rust`，分别保存 catalog、state、workspace 和 Library；首次向可编辑 catalog 导入 8 张现有示例图，向 Library 安装 5 个官方 Plugin 及本地工具；不自动运行 Graph、调用模型或启用渠道。首次 `start` 自动执行锁定的 `npm ci`、增量构建 debug Host/relay、准备 `.local/bin/goose` 并构建缺少或已更新的 Web 资产。Goose 配方自动安装固定工具链的 musl target 和已校验的交叉编译器；首次需要联网，后续复用产物；Goose 版本、补丁或配方变动时才重新准备。`setup` 准备依赖和插件但不启动服务，示例 Graph 在 Host 启动后导入；`status`/`stop` 不下载或构建。缺少系统工具时提示具体缺项；源码构建成功后使用本机产物，不检查跨机器固定二进制摘要。

默认使用 `rust/target/debug`（或 `CARGO_TARGET_DIR/debug`），不自动选取旧 release 产物；指定自定义 Host/relay 时要求路径已经可执行。拉取更新或修改代码后执行 `./scripts/dev.sh restart`，脚本完成增量构建和必要的 Web 资产更新。开发脚本和 Vite 均不加载 `.env`；旧文件存在也不会执行其中内容。可选的路径、端口等覆盖只取显式进程设置，见 [脚本](../scripts/dev.sh) 和下方部署参数。

```sh
./scripts/dev.sh restart
./scripts/dev.sh stop
```

只操作脚本持有的 PID；启动前检查端口与当前运行任务。重启会中断活动节点，之后应查看持久记录并继续。现有 `.local/demo`、旧环境和运行历史不会被自动迁移或清理。

## 附带的 Graph 与 Plugin

源码启动后，工作流列表中可直接打开 `call-report`、`call-worker`、`revise-loop`、`parallel-audit`、`plugin-research`、`deep-academic-research` 和 `wecom-persistent-assistant`。建议先运行 `call-report`，输入 `{"report_code":"DEMO-001"}`；它仅使用 Op 和子工作流，无需模型。保存 default Flash 后可尝试写作或调研图。

插件库已安装联网、学术调研、Docmost、企微和 Anchor 管家。Docmost、企微的服务配置仍在网页按需填写；首次安装不会自动连接账号或启用监听。图与插件的用途、配置条件及其他示例见[上手示例](../examples/README.md)。

导入只在首次执行：同名内容不覆盖，后续源码更新/重启也保留用户修改和删除。显式指定 entry bundle、自定义 Host 或外部服务不自动导入。这是源码启动行为，完整平台发行包仍按自己的清单交付。

## 模型与 Goose

源码启动固定 Goose v1.53.0、仓库补丁和上游 Cargo.lock，构建静态 x86_64 musl lean ACP-only 二进制，不含完整 CLI、调度器、bundled MCP 与平台扩展。`scripts/dev.sh start` 调用[构建脚本](../scripts/build-goose-acp.sh)准备本机产物，并自动将实际摘要传给 Host，同时定位 relay 隔离模型控制网络。源码用户无需设置 `ANCHOR_GOOSE_BINARY_SHA256`。

下载完整平台包时，安装器按该包的发行规则核验二进制，工具已随包提供；当前固定发行产物说明见[发行 builder](../rust/anchor-distribution/README.md)。源码启动不受该成品摘要约束。

Host 可以在没有模型配置时启动。Pilot 发送消息或继续回复遇到服务不可用时，网页读取 Host 的默认模型执行状态；确认配置尚未齐备后，提示先配置默认服务的地址、Flash 模型和 API key，并提供「前往模型设置」按钮。原文字和附件保留，保存设置后返回即可重试；已配置或状态读取失败时保留原错误，不推断是配置缺失。

WebUI「设置 → 模型服务」固定提供 `default`、`backup` 两组，每组填写服务地址、接口协议、模型服务商提供的 API key，以及 `flash`、`pro` 两个模型名。两组独立保存，可只填写一部分；未填全会显示待配置。密钥不回显、不保存在浏览器，即使两组地址相同，也能分别使用和清除不同账户的密钥。连接测试会区分模型服务的认证失败（401）、权限拒绝（403）、接口错误、限流和临时故障，不把 Anchor 管理接口的 400 显示为上游服务状态。

点击「测试 Flash」或「测试 Pro」会发送一条小请求，可能产生少量费用；测试不保存草稿，保存也不会自动测试。**保存后，Pilot 下一轮使用 default 的 Flash 模型；当前正在执行的一轮保持原配置，无需重启服务。** 已有对话会保留历史。Graph 未指定模型时也使用 default Flash，可明确选择两组服务的 Flash/Pro；backup 不会自动接管失败任务。旧配置可预填 default 的地址、协议与 Flash，Pro 留空；首次在原地址保存可沿用密钥，随后各自独立。更换地址或协议需重新填密钥。

测试等待上限为 20 秒。显示「已连接，但没有收到完整模型结果」时，说明服务已经响应，本次还没验证到模型输出；不能据此认定密钥错误或网络不通，也不能当作模型已可用。

模型服务地址、协议、模型名与 API key 只从 Host 私有管理存储读取，环境变量中的 `ANCHOR_MODEL_URL/API_KEY/NAME/WIRE_API/ALIASES` 不再作为配置或回退源。default 尚未保存时，仅兼容已有托管配置；保存之后缺少字段或密钥会明确失败，不退回旧服务。历史服务凭据用于恢复旧任务，可在网页折叠的凭据区域更新或清除。旧 `.env` 和安装中的 `config/model.env` 不会自动导入、覆盖或删除；请在 WebUI 核对并保存当前服务。新安装不接受 `--model-config`。

仅准备源码依赖、不启动服务：

```sh
./scripts/dev.sh setup
```

手动重建 Goose 时使用 `scripts/build-goose-acp.sh --output "$PWD/.local/bin/goose"`；日常启动的 `--ensure` 按配方输入标记和本机输出校验和复用缓存，输入更新或文件缺失/损坏则重新构建。`.sha256` 和 `.build-record.json` 是可重建的构建记录，摘要不要求与其他机器相同。源码、交叉编译器的下载仍核验固定摘要，依赖按 `--locked` 编译。

完整平台启动器直接定位随包二进制，不从调用进程继承部署或凭据设置。开发者可显式覆盖 `ANCHOR_GOOSE_BINARY`、`ANCHOR_GOOSE_RELAY_BINARY` 等路径；默认源码启动会在所选 Goose 输出路径准备本机产物并自动传递其摘要。

`ANCHOR_MODEL_CONTEXT_WINDOW`（正整数，可选）会作为 `GOOSE_CONTEXT_LIMIT` 传给 Goose，覆盖它按模型名推断的上下文窗口——本机部署用别名与 OpenAI 兼容端点时该推断不可靠。

网页中的接口协议为 `chat` 或 `responses`。URL 接受 HTTPS，或仅用于本地测试的 HTTP loopback IP endpoint；拒绝 URL 用户凭据、query 和 fragment。Graph 的四个设置引用与旧别名规则见 [作者契约](rust-graph-authoring-contract.md#模型选择)。每个新 invocation 冻结绑定；恢复同一个未完成 invocation 仍使用原模型与服务。

沙箱网络**默认隔离**：只要 `ANCHOR_GOOSE_BINARY` 同目录（或 `ANCHOR_GOOSE_RELAY_BINARY` 指定的绝对路径）存在沙箱中继 `anchor-net-relay`，Goose 就运行在自己的网络命名空间里，只能通过沙箱内中继经 UNIX socket 到达 Host bridge 与模型代理，真实 provider 凭据只留在宿主侧。发行包把中继放在 `bin/anchor-net-relay`（与 Host 同目录），部署时与 Goose 二进制放同一目录即可。设 `ANCHOR_GOOSE_LOCAL_NETWORK=0` 回到共享宿主网络（此时**必须**显式 `ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1`，该模式不是 loopback-only 隔离）；找不到中继二进制时同样回退到共享模式并需要该开关。`anchor-devtools preflight` 会校验隔离模式下的中继二进制存在。

工具披露（AgentNode）：默认**自动**——当某个节点挂载的工具里会有超过 3 个被藏起来时，宿主只列出 Anchor 自己的核心工具加两个披露工具 `anchor_tools`（一行摘要检索、分页）与 `anchor_tools_call`（按名调用），其余工具用时再取；被藏起来的工具**无法被直接调用**（Goose 会拒绝当轮未广告的调用），只能经 `anchor_tools_call` 到达。隐藏工具不超过 3 个时不披露（两个元工具本身约 890 字节，比省下的 schema 更贵）。`ANCHOR_NODE_TOOL_DISCLOSURE=1` 强制披露、`=0` 关闭；`ANCHOR_NODE_ALWAYS_VISIBLE`（JSON 数组）可收窄始终可见的工具集合。

模型调用默认由 Host 代理：Goose 进程只拿到 Host bridge 的 loopback 地址和一次性 token，provider 端点与真实 API key 留在宿主侧，宿主用真实响应流式回传。`ANCHOR_MODEL_DIRECT=1` 是显式回滚开关，会让沙箱像早期版本那样自己持有端点与凭据并直接拨号；仅在代理出问题时临时使用。

`ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1` 是 Goose 进程的明确网络接入决定；业务工具和节点仍受 Host/Sandbox 授权。模型配置与凭据保存在 Host 的私有管理存储，不放进部署环境、Graph 包或版本库。

「设置 → 开发环境」可初始化 Python 和 Rust，显示进度、停止并在失败后重试。完整发行包提供固定的初始化工具；初始化前需要勾选允许联网下载。关闭页面不会取消初始化，重新打开会读取 Host 上的操作状态。Python 当前版本自动检测，点击「刷新状态」会重新读取，不需要联网或初始化。已有解释器不代表依赖准备工具齐备；缺少工具时页面会禁用初始化并说明原因，已有解释器仍可使用。源码开发部署也可通过显式进程设置登记固定 uv、摘要与软件源，检查时复用原解释器，见下方部署参数；部署管理的版本只读显示，不能在网页修改。完整平台安装自动提供这些固定工具。

## 配置插件与 MCP

在插件库打开插件详情，填写服务地址及访问凭据后保存，再点击「测试连接」。Docmost 的网页操作与附件工具共用一份 Key；轮换后下一次调用直接使用新值，无需编辑 `.env` 或重启。更换服务地址时需要重新填写 Key；清除后后续调用会提示缺失，不会使用旧环境值。

「已保存」只表示 Host 保存成功；「初始化与工具发现成功」也不代表页面写入或附件上传已通过。凭据不会回显。默认配置属于当前登录身份，旧共享 local Session 不会自动获得个人凭据。

仅当部署者通过 `ANCHOR_PLUGIN_EXTERNAL_CONFIGURATION` 明确托管字段时，网页显示「由部署管理」。具体声明及恢复边界见 [Plugin 连接配置](plugins.md#连接配置与凭据)。

## Host 根目录与权限

| 配置 | 用途 |
| --- | --- |
| `ANCHOR_RUNNER_BUNDLE_ROOT` | 可选的部署管理入口 bundle；对应 Graph 禁止经 HTTP 改写，普通业务 Graph 不设置此项 |
| `ANCHOR_RUNNER_CATALOG_ROOT` | 可编辑 Graph catalog |
| `ANCHOR_RUNNER_STATE_ROOT` | RunStore、Session/Turn、计划、渠道和锁 |
| `ANCHOR_RUNNER_WORKSPACE_ROOT` | 节点 workspace、Artifact、输入与原生会话 |
| `ANCHOR_RUNNER_LIBRARY_ROOT` | 已安装 Plugin/tool 与授权资源；**插件只有这一处家**。未设置时回退到 `CATALOG_ROOT`（单根部署把库根与图目录放同一处）；两者都没设的部署没有插件家，插件相关操作具名拒绝——**不会**退到 `BUNDLE_ROOT` 旁边（图包只带引用，不是插件家） |
| `ANCHOR_RUNNER_WEB_ROOT` | `apps/web/dist` 或发行 Web 目录 |
| `ANCHOR_RUNNER_GRAPH_NAME` | 可选的部署管理入口 Graph 名，与 bundle 使用同一身份及修改保护 |
| `ANCHOR_RUNNER_LISTEN` | 默认 `127.0.0.1:8077` |
| `ANCHOR_RUNNER_ALLOWED_COMMANDS` | 显式命令列表，例如 `sh,git` |
| `ANCHOR_RUNNER_LISTEN` | 必须监听 loopback（默认 `127.0.0.1:8077`）。Anchor 没有登录，远程访问经 SSH 隧道或操作员自己的已认证入口；旧配置里的 `ANCHOR_API_KEYS` 会让 Host 拒绝启动，删掉即可 |
| `ANCHOR_DEPENDENCIES_PYTHON`、`ANCHOR_PILOT_PYTHON_ENABLED` | 开发部署已有 Python 路径及可选 Pilot 使用许可；完整平台在网页初始化 |
| `ANCHOR_DEPENDENCIES_UV`、`ANCHOR_DEPENDENCIES_UV_SHA256`、`ANCHOR_DEPENDENCIES_SOURCES` | 开发部署显式登记固定依赖工具与软件源；三项配套，完整平台由发行 manifest 提供 |
| `ANCHOR_RETENTION_DAYS`、`ANCHOR_RETENTION_MAX_GB` | 已结束 Run 的自动回收：超过天数（默认 30），或 Run 数据总量超过上限（默认 20 GB，从最旧的删起），就回收。启动时执行一次，之后每小时一次。`0` 或 `off` 关闭对应限制；数据中枢的事实和评分不回收。见[设计卡 003](design-cards/003-data-retention.md) |

可写根应彼此隔离并在 release 外。Host 在监听前验证 bundle、探测可写根并获取 deployment-writer lease；同一 state root 只允许一个 writer。第三方工具环境与本地输入只按明确注册/授权挂载，不因模型提出路径就允许读取。

正式平台部署使用 [systemd 模板](../deploy/systemd/anchor.service)，由安装启动器设置以上路径与控制参数，无需环境文件。服务只监听本机，没有登录；需要远程访问时由操作员自行加一层认证入口。

## Graph 与 WebUI

在 WebUI 创建或打开 Graph，编辑 Agent/Op、边、反馈、子图、布局和 Plugin。保存时编译并校验，运行时冻结定义/输入。「运行工作流」打开输入表单，校验 JSON 对象并保留该工作流上次填写的草稿。Graph 和 Run 页面可把对象引用带入 Pilot 草稿，由用户发送后开始协助。仓库示例位于 [examples/graphs](../examples/graphs)，安装所需官方工具后可以通过 Graph API 导入。

「插件库」支持从本机目录或 GitHub 目录链接安装插件，预检展示来源、权限和替换影响，确认后执行。安装期间可离开页面或刷新，再从操作列表查看进度；Host 重启后按 Library 实际状态重新检查。插件详情可配置、轮换和清除声明的凭据，也可预检卸载影响后确认卸载。被运行或会话引用的插件按 Host 前置条件处理。

「运行 → 定时计划」可创建、编辑、暂停、启用和删除计划，支持一次性、固定间隔、每天、每周和每月，按服务所在时区执行。重新启用只安排未来时点；暂停或删除计划不会删除既有 Run。

需要定时发送企业微信通知时，在执行节点挂载渠道 Plugin，并在任务输入与指令中明确机器人 Graph 和真实接收人，见[定时通知配置](wecom-assistant.md#业务工具与附件边界)。

企业微信入口在「图编排 → 消息触发」绑定当前工作流并启停，每个 Host 仍只有一个入口。绑定其他工作流时先保存当前图的绑定，再启用。「运行看板 → 消息会话」查看各对话的实例、打开 Run，以及按 Host 允许的操作停止、恢复、退役或清理。监听图的「查看会话」也打开同一管理界面；具体权限与恢复边界见 [企业微信](wecom-assistant.md)。

```sh
curl -X POST http://127.0.0.1:8077/graphs \
  -H 'Content-Type: application/json' \
  --data '{"name":"research"}'
node -e 'const fs=require("fs");process.stdout.write(JSON.stringify({definition:JSON.parse(fs.readFileSync("examples/graphs/deep-academic-research.json","utf8"))}))' | \
  curl -X PUT http://127.0.0.1:8077/graphs/research \
    -H 'Content-Type: application/json' --data-binary @-
```

本机 API 不需要凭据。Graph PUT 的接受格式、资源错误和诊断以 [Web API 契约](rust-frontend-api-contract.md) 为准。Plugin 必须先安装到 Host canonical Library；名称存在不代表外部服务或模型凭据可用。

AgentNode 负责模型工作，OpNode 负责确定性命令/调用。Plugin 直接挂到 AgentNode。反馈边允许带具体总结返工，节点在同 Run 的已提交产物基础上继续；读写声明、只读输入和 Sandbox 权限由运行时检查。研究完成来自目标与证据，不能以轮次数代替内容验收。

内联模块展开到同一 Run。`Op.call` 的 `wait`/`detach` 产生可追溯父子 Run；`call.session` 保留可信用户/来源。配对 `fanout`/`join` 允许独立串行分支并发并统一收束；嵌套、交叉或不支持的调用组合保存/接纳时拒绝。定义见 [组合设计](graph-composition-design.md)。

## Run、历史与恢复

Run 页面呈现实际路径、轮次、状态、节点对话、工具请求/结果、workspace 与 Artifact。节点之间通过不可变产物和只读输入交接，Git 视图可用于提交绑定的审查。Artifact 是产物权威，前端缓存和模型摘要不是执行记录。

暂停/停止请求先保存，节点收束后才显示最终状态。继续原 Run 保留 cursor、invocation 与 Goose 原生 Session。进程中断可能留下工具结果未知事实；Agent 读取已保存历史和现场，核查效果后继续，运行时不盲目重放外部操作。

删除会检查活动执行和 Graph 调用引用。持续会话中的 Run 共享原生 scope，单条删除受到限制；整 Graph 删除仍遵守调用/执行与精确确认门禁。已开始 Run 不因 Graph 编辑改变身份。

## Pilot Session

Pilot 用自然语言查询和管理 Graph、Run、Plugin 与 Artifact。Session 保留长期对话和运行关联；每次输入使用稳定 `request_id`，刷新通过 SSE 游标接回同一 Turn。页面关闭不等于停止执行。

```text
POST /sessions
POST /sessions/{session}/turns
GET  /sessions/{session}/messages
GET  /sessions/{session}/turns/{turn}/events?after=<cursor>
POST /sessions/{session}/stop
GET  /sessions/{session}/turns/{turn}/questions
POST /sessions/{session}/turns/{turn}/questions/{question}/answer
```

必要提问保持同一 Turn；回答可接受、拒绝或取消，重复相同回答幂等。删除确认绑定精确目标与资源状态。服务重启中断在途问题，不重放旧确认；保存回答不代表外部操作已经完成。

## Plugin 与业务示例

Plugin 保留 `plugin.json`、Skill 和资源，MCP 可用 stdio 或 HTTP。官方可执行文件使用 Rust，并在沙箱只读挂载。第三方 Plugin 可声明自己的运行环境，需部署者准备并授权；清单不是自动安装器。

详细操作见 [Plugin 指南](plugins.md)。企业助手、学术研究、RSI 和周报分别见 [企业微信](wecom-assistant.md)、[RSI](rsi.md) 和 [周报](weekly-work-report.md)。Host 自动监管 Graph 声明的原生 WeCom Gateway；不要同时为同一渠道启动第二个独立网关。

Docmost、公网企业微信与公共学术服务需要实际配置与真实业务验收。确定性本地测试检查接线、权限、结果、历史和文件，不证明业务内容质量或公网协议已经全部通过。

## 触发、健康与发行

手动 `/trigger`、Webhook、计划与 Responses 子集使用相同 Run 接纳。`/health` 返回进程存活，`/ready` 在配置 Graph 可用时返回 ready；它们不发送模型请求。Responses SSE 经反向代理时需要关闭缓冲并允许长连接。

发行使用 [anchor-distribution](../rust/anchor-distribution/README.md)，构建后的 Web、固定 Goose、Host 与显式 Plugin 二进制进入受审闭包，状态和密钥留在包外。预检与候选验证使用 `anchor-devtools`，见 [部署指南](rust-production-deployment.md) 和 [候选回归](rust-production-candidate.md)。

日常维护运行 [确定性小图](runtime-contract-tests.md)，真实模型兼容、目标机部署和大型业务内容验收单独安排。旧数据可原地只读保留，不自动导入或覆盖；本地候选通过也不等于生产服务与历史数据已切换。
