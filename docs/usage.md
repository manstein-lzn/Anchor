# 使用指南

当前安装使用 Rust Host、固定 Goose 与编译后的 WebUI。完整生产包与 systemd 操作见 [部署指南](rust-production-deployment.md)；本文介绍仓库开发方式和产品操作。

## 安装与本地启动

需要 Linux x86_64、Git、Bubblewrap、Rust stable，以及 Node.js 20.19+ 或 22.12+。沙箱需要可用的 user/mount/network namespace。模型调用和 Plugin 网络权限分别配置。

```sh
npm --prefix apps/web ci
cp .env.example .env
# 编辑 .env 中 Goose 与模型配置
./scripts/dev.sh start
./scripts/dev.sh status
```

开发时打开 <http://127.0.0.1:5173>，Vite 将 API 请求转发到 <http://127.0.0.1:8077> 的 Rust Host；Host 也提供已构建的 WebUI。脚本使用 `.local/rust`，分别保存 catalog、state、workspace 和 Library，首次创建只含 `true` Op 的 `dev` Graph，启动不调用模型。预构建 Host 和 Web 资产缺失时才构建；修改代码后应主动重建。可覆盖参数见 [脚本](../scripts/dev.sh) 和 [.env.example](../.env.example)。

```sh
./scripts/dev.sh restart
./scripts/dev.sh stop
```

只操作脚本持有的 PID；启动前检查端口与当前运行任务。重启会中断活动节点，之后应查看持久记录并继续。现有 `.local/demo`、旧环境和运行历史不会被自动迁移或清理。

## 模型与 Goose

使用 Goose v1.53.0 上游源码的 lean ACP-only 入口构建的静态 x86_64 musl 二进制（不含完整 CLI、调度器、bundled MCP 与平台扩展），SHA256 固定为：

```text
71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7
```

用 [构建脚本](../scripts/build-goose-acp.sh) 从固定上游源码构建并审查该二进制，放在稳定路径，然后配置：

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose
ANCHOR_GOOSE_BINARY_SHA256=71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7
ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1
ANCHOR_MODEL_URL=https://provider.example/v1
ANCHOR_MODEL_API_KEY=REPLACE_WITH_PROVIDER_KEY
ANCHOR_MODEL_NAME=REPLACE_WITH_MODEL_NAME
ANCHOR_MODEL_WIRE_API=responses
ANCHOR_MODEL_ALIASES={}
ANCHOR_MODEL_CONTEXT_WINDOW=128000
ANCHOR_MODEL_DIRECT=0
# Isolation is the default when the relay sits beside the Goose binary; set 0 to share.
ANCHOR_GOOSE_LOCAL_NETWORK=1
ANCHOR_GOOSE_RELAY_BINARY=/absolute/path/to/anchor-net-relay
```

`ANCHOR_MODEL_CONTEXT_WINDOW`（正整数，可选）会作为 `GOOSE_CONTEXT_LIMIT` 传给 Goose，覆盖它按模型名推断的上下文窗口——本机部署用别名与 OpenAI 兼容端点时该推断不可靠。

`ANCHOR_MODEL_WIRE_API` 为 `chat` 或 `responses`。URL 接受 HTTPS，或仅用于本地测试的 HTTP loopback IP endpoint；拒绝 URL 用户凭据、query 和 fragment。Graph 模型别名通过 `ANCHOR_MODEL_ALIASES` 映射到实际 wire 模型，新 invocation 冻结绑定；继续执行不能静默换模型。

沙箱网络**默认隔离**：只要 `ANCHOR_GOOSE_BINARY` 同目录（或 `ANCHOR_GOOSE_RELAY_BINARY` 指定的绝对路径）存在沙箱中继 `anchor-net-relay`，Goose 就运行在自己的网络命名空间里，只能通过沙箱内中继经 UNIX socket 到达 Host bridge 与模型代理，真实 provider 凭据只留在宿主侧。发行包把中继放在 `bin/anchor-net-relay`（与 Host 同目录），部署时与 Goose 二进制放同一目录即可。设 `ANCHOR_GOOSE_LOCAL_NETWORK=0` 回到共享宿主网络（此时**必须**显式 `ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1`，该模式不是 loopback-only 隔离）；找不到中继二进制时同样回退到共享模式并需要该开关。`anchor-devtools preflight` 会校验隔离模式下的中继二进制存在。

模型调用默认由 Host 代理：Goose 进程只拿到 Host bridge 的 loopback 地址和一次性 token，provider 端点与真实 API key 留在宿主侧，宿主用真实响应流式回传。`ANCHOR_MODEL_DIRECT=1` 是显式回滚开关，会让沙箱像早期版本那样自己持有端点与凭据并直接拨号；仅在代理出问题时临时使用。

`ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1` 是 Goose 进程的明确网络接入决定；业务工具和节点仍受 Host/Sandbox 授权。配置与凭据留在部署环境，不放进 Graph 包或版本库。

## Host 根目录与权限

| 配置 | 用途 |
| --- | --- |
| `ANCHOR_RUNNER_BUNDLE_ROOT` | 初始 Graph bundle 与 manifest |
| `ANCHOR_RUNNER_CATALOG_ROOT` | 可编辑 Graph catalog |
| `ANCHOR_RUNNER_STATE_ROOT` | RunStore、Session/Turn、计划、渠道和锁 |
| `ANCHOR_RUNNER_WORKSPACE_ROOT` | 节点 workspace、Artifact、输入与原生会话 |
| `ANCHOR_RUNNER_LIBRARY_ROOT` | 已安装 Plugin/tool 与授权资源 |
| `ANCHOR_RUNNER_WEB_ROOT` | `apps/web/dist` 或发行 Web 目录 |
| `ANCHOR_RUNNER_GRAPH_NAME` | 初始 bundle 对应 Graph 名 |
| `ANCHOR_RUNNER_LISTEN` | 默认 `127.0.0.1:8077` |
| `ANCHOR_RUNNER_ALLOWED_COMMANDS` | 显式命令列表，例如 `sh,git` |
| `ANCHOR_API_KEYS` | 部署 Bearer 白名单；结构见环境模板 |

可写根应彼此隔离并在 release 外。Host 在监听前验证 bundle、探测可写根并获取 deployment-writer lease；同一 state root 只允许一个 writer。第三方工具环境与本地输入只按明确注册/授权挂载，不因模型提出路径就允许读取。

正式部署使用 [deploy/systemd/anchor.env.example](../deploy/systemd/anchor.env.example)。非 loopback 监听必须配置有效唯一 Bearer secrets 并有对应网络边界；本地白名单不等于完整多租户模型。

## Graph 与 WebUI

在 WebUI 创建或打开 Graph，编辑 Agent/Op、边、反馈、子图、布局和 Plugin。保存时编译并校验，运行时冻结定义/输入。仓库示例位于 [examples/graphs](../examples/graphs)，安装所需官方工具后可以通过 Graph API 导入。

```sh
curl -X POST http://127.0.0.1:8077/graphs \
  -H 'Content-Type: application/json' \
  --data '{"name":"research"}'
node -e 'const fs=require("fs");process.stdout.write(JSON.stringify({definition:JSON.parse(fs.readFileSync("examples/graphs/deep-academic-research.json","utf8"))}))' | \
  curl -X PUT http://127.0.0.1:8077/graphs/research \
    -H 'Content-Type: application/json' --data-binary @-
```

若部署配置 API keys，向上述请求追加 `Authorization: Bearer <key>`。Graph PUT 的接受格式、资源错误和诊断以 [Web API 契约](rust-frontend-api-contract.md) 为准。Plugin 必须先安装到 Host canonical Library；名称存在不代表外部服务或模型凭据可用。

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

发行使用 [anchor-distribution](../rust/anchor-distribution/README.md)，构建后的 Web、固定 Goose、Host 与显式 Plugin 二进制进入受审闭包，状态和密钥留在包外。预检、切换盘点与候选验证使用 `anchor-devtools`，见 [部署指南](rust-production-deployment.md)、[切换准备](rust-production-cutover.md) 和 [候选回归](rust-production-candidate.md)。

日常维护运行 [确定性小图](runtime-contract-tests.md)，真实模型兼容、目标机部署和大型业务内容验收单独安排。旧数据可原地只读保留，不自动导入或覆盖；本地候选通过也不等于生产服务与历史数据已切换。
