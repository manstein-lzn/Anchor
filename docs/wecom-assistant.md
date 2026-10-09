# 接入 Anchor 企业微信助手

当前链路是原生 `anchor-wecom-gateway` → Rust Host 的 Session/Turn → 普通助手 Graph → Goose ACP 与授权 MCP 工具 → 网关投递。业务消息不经过 Pilot；Gateway 负责平台传输与投递账本，Host 负责用户隔离、Run、取消和会话事实。运行环境为 Linux，客户端可在其他电脑或手机上使用。

本文说明已接入代码的契约与配置方法；常驻助手的真实 provider 隔离两轮已通过，但不是实际企微 transport 验收。该常驻切片尚未部署，仍待用户私聊验收，不能把旧链路的部署/媒体证据当作本次常驻切换完成。

## 机器人和成员身份

在企业微信智能机器人管理入口选择 API 模式、长连接，取得 Bot ID 和 Secret。默认平台端点为 `wss://openws.work.weixin.qq.com`；群 Webhook 地址不能替代这组凭证。无需向公网开放本机 HTTP 回调，网关与 Host 通过本机端点交互。

使用实际私聊事件中的 `body.from.userid` 配置成员名单。它不是显示姓名、手机号或个人微信号，也不能未经核对当成自建应用通讯录账号。接入不会读取或代答本人已有的个人私聊。

## 部署配置

通过服务管理器提供配置环境；不要把凭证写入 Graph、Plugin 包或聊天。若使用部署的 EnvironmentFile，将其权限限制为服务用户可读；二进制不自动加载仓库 `.env`。

```dotenv
WECOM_BOT_ID=后台提供的机器人ID
WECOM_BOT_SECRET=后台提供的机器人Secret
ANCHOR_WECOM_USERS=已核实的成员userid
ANCHOR_WECOM_SEND_USERS=
ANCHOR_WECOM_GRAPH=wecom-assistant
ANCHOR_WECOM_REPLY_NODE=assistant
ANCHOR_API_KEYS=["至少32字节的随机密钥"]
ANCHOR_API_KEY=与服务白名单匹配的密钥
```

多位成员用英文逗号分隔；空名单拒绝所有成员，`*` 表示明确允许全部平台成员。主动发送名单为空时继承入口名单。`ANCHOR_API_KEYS` 也保护管理 API。修改启动配置后按部署流程重启，保持一个 Bot 只有一个网关。

按 [Rust 部署指南](rust-production-deployment.md) 安装 Host、固定版本 Goose、`anchor-wecom-gateway` 和 `anchor-wecom-tools`。通过现有 Library/Graph API 安装 [助手示例](../examples/graphs/wecom-assistant.json) 及原生 WeCom Plugin，确认回复节点挂载 `wecom`。独立应用 API 包不包含通道声明，不能替代完整机器人部署。

常驻循环须显式安装为独立 Graph [wecom-persistent-assistant](../examples/graphs/wecom-persistent-assistant.json)，并选择 `ANCHOR_WECOM_GRAPH=wecom-persistent-assistant`、`ANCHOR_WECOM_REPLY_NODE=assistant`；不要原地替换已有 `wecom-assistant` 或改写旧 Run。这里的配置结果节点是 Agent `assistant`，投递由图内 `reply` Op 承担。样例可编译不表示手动/standalone 获得 Session host operation 授权；可信渠道接纳才绑定获准的 Session 和节点范围。生产切换仍需主轨独立确认，不因安装样例自动发生。

常驻助手和一次性助手的 Host 等待路径都不设置默认模型耗时上限；网关到 Host 的 HTTP 请求默认也不设置超时。只有明确配置 `ANCHOR_CHANNEL_WEBHOOK_TIMEOUT_MS` 时才启用外部请求超时；超时只表示投递观察窗口结束，不代表 Run 或 Goose 执行已停止。未知投递必须按账本核查，不能盲目重放。

Host 使用 `ANCHOR_RUNNER_BUNDLE_ROOT`、`ANCHOR_RUNNER_CATALOG_ROOT`、`ANCHOR_RUNNER_LIBRARY_ROOT`、`ANCHOR_RUNNER_STATE_ROOT` 和 `ANCHOR_RUNNER_WORKSPACE_ROOT` 区分资源、持久事实与工作区。部署配置完成后启动 `anchor-runner-host serve`；监听地址由 `ANCHOR_RUNNER_LISTEN` 配置。不要同时启动另一个共享状态根的服务。

Host 的 ChannelSupervisor 监管 Plugin 声明的原生 Gateway，设置本机回调地址、私有控制 socket 和 descriptor。正常部署不手动启动第二个网关。Gateway 的独立配置和协议见 [原生 Gateway README](../rust/anchor-wecom-gateway/README.md)。

## 对话与投递

常驻助手是普通三节点循环：`wait_input (Op.host session.wait_input) → assistant (Agent) → reply (Op.host session.reply) → wait_input`，不在 Kernel 中加入 Session/WeCom 依赖或专用 Runner。首条可信消息惰性创建该 Session 的 current assistant Run；后续每条输入仍有独立 Turn，但复用 Run、cursor、同节点稳定 workspace 与逐节点 Goose 原生历史。等待节点把本轮输入作为不可变产物交给 Agent，不覆写 `Run.input`，空闲等待不调用模型。Session 保存 Turn/幂等、current 与历史 binding、retire 事实；Run 拥有执行进度，逐轮 invocation/fs2 Artifact 不可变，不因工作目录复用而合并。

同一通道 Session 串行，不同用户隔离。新消息打断旧轮，Host 等 Goose 与工具执行者真正退出，保存未提交文件 checkpoint 与 `Yielded` 事实，再由普通 Runner 提交 `Interruption` 控制 Artifact，沿 `assistant → reply → wait_input` 接续。旧回复标为 `suppressed`，不发送、不假装旧 Agent/Turn 成功，也不把整个助手 stop 后静默复活。已发生的外部操作不回滚，未知结果须先核查现场，不能自动重放。新实例仅在首个 Agent invocation 从可信获准的 previous 文件快照初始化一次，之后直接维护本节点 `/workspace`；跨图或跨用户历史不自动迁移。

沿用既有一次性 `wecom-assistant` 时，每条输入仍对应独立 Run：新消息取消旧 Run，待旧执行退出后接续原生 Goose 会话；上轮工作文件通过授权的只读 `/previous` 提供。被取消、且模型尚未看到的用户消息仍作为 `input.interrupted_messages` 带入新 Run（最多 8 条、每条截断到 500 字符；纯附件消息只提示文件数量），这一旧路径不挂载被打断轮的附件，不能与常驻助手的 Turn 快照授权混淆。

Goose 负责 Agent loop、Provider、原生历史和 compaction；Anchor 保存 Session/Turn、Graph/Run、产物和权限事实。原生会话文件与 Host 状态应按部署备份契约一起保存，不能把单个 JSONL 当作完整会话。

网关持久保存投递事实，相关平台 ACK 才算确认；超时、断线或重启后的未知结果不会自动重发。Host settlement 重试只同步账本，不再次发送平台消息。Graph 完成、调用已接纳、平台确认与收件人已读是不同状态。

常驻 `reply` 只读本 Turn 绑定的确切 Agent commit 与 `reply_for`，不等待整个 Run Completed。`wecom_attach_image` 的登记按 Turn 保存，下一轮没有重新登记就不复用旧图片；同 Run 多轮不绕过既有发送 claim/ACK 账本。

### 停止、恢复与退役

- stop/resume 作用于当前实例，保留 current binding；收到停止请求不等于执行者已退出。
- **Host 进程启动时默认自动恢复**。恢复的判定只看绑定与活执行：`channel_assistants` 中仍有未 retired 的 current 绑定，且该 Run 在本进程没有活执行（记录缺失、仍停在 `running`/`ready`，或已终态都算），就自动恢复；用户已显式 retire 的实例保持关闭，下一条可信消息才接纳新实例。开关 `ANCHOR_ASSISTANT_AUTO_RESUME=0`（也接受 `false`/`off`/`no`）关闭该默认行为，**由 Host 进程在启动时读取**，不需要 supervisor 透传。
- 自动恢复复用与手动恢复相同的原子交接：先把 current 绑定退休、把新 Run 置为 current，并把「已领取但未提交」的在途 Turn 改判给新 Run、换成新 Run 的 wait key；再把仍停在 `running` 等非终态的陈旧 Run 对账为 `stopped`（此时它已不是 current，不会顺带把刚交接的 Turn 结算掉）；最后以 `previous_run = 旧 Run` 接纳新 Run。顺序保证交接已提交而接纳未执行时，重试仍接纳同一个目标 Run id，不产生第二个 current 实例。
- 自动路径写入独立会话事件 `channel.assistant_auto_resume{from,to}`；底层交接本身仍写 `channel.assistant_handover`，所以一次自动恢复会同时留下「发生了交接」和「由重启自动触发」两条事实。若接纳未完成（例如目标 Run 只有不可变 metadata 而没有 Run 记录），恢复**不移动任何绑定**，只按有界次数与退避重试并如实记录错误，不制造每启动一次就换一个目标 id 的链。
- 新 Run 的首个 work 节点准备在其 `.prepare.lock` 内一次性继承旧 Run 的稳定现场（`seed_stable_workspace`）：只复制 Agent 文件本身，不带锁、owner fact、`previous-inputs/` 或旧 Artifact；目标已存在则校验一致后 no-op，失败不留半成品。来源只取宿主事实（本次 Run 的 `conversation.previous_run` + 会话绑定表中确已退休的来源），不从图内容、用户输入或路径字符串推断；跨 Graph 版本或跨实例时不继承，旧现场与旧 Artifact 字节不变。
- 进程重启后若不自动恢复（开关关闭）或尚未恢复，须通过现有 Run 控制入口显式继续，并核查保存的 cursor、Goose 历史和现场，不自动重放未知效果。
- `GET /channel-sessions/{session}/assistant` 返回 `{ "assistant": ..., "needs_recovery": bool }`；`assistant` 含 `session_id`、`run_id`、`wait_node`、`work_node`、`reply_node`，无 current 时为 `null`。`needs_recovery` 是派生值而非存储字段：绑定存在且其 Run 在当前进程没有活执行（含 Run 记录缺失）即为 `true`。请求遵循管理 API 的鉴权与 owner 范围，不能读取别人的实例。
- `POST /channel-sessions/{session}/assistant/retire` 接收 `{ "run_id": "当前实例ID" }`。先显式 stop 并等真实 `Stopped`，或确认 Run 已 `Completed`/`Failed`/`Aborted`；宿主 active 执行者、Session 的 Running Turn 和 pending/sending/unknown delivery 均阻断退役，冲突返回 409，不能用 retire 隐式取消/结算未知投递。
- explicit retire 保留旧 Run/历史 binding 与退役事实、解除 current，幂等重试不创建实例；retired Run 禁止 resume 或重新绑定。下一条可信消息才惰性接纳新实例。合法保存新版 Graph 不热更新旧 Run 的快照、模型或资源授权；新版通过退役后的新实例生效。
- 退休实例和历史轮次可以经 `DELETE /runs/{run}` 单独删除，但 Host 按血缘判定，不按“看起来没在用”判定：不能删 Session 仍指向的 current assistant binding（先 retire），不能删投递未结算（pending/sending/unknown）的渠道轮次，且每个更新的 Run 要么已经不可能再执行（终态，或已有自己的后继因而被拒绝 resume/recover），要么已经完成会读取它的首轮执行——继承稳定现场、只读 `/previous` 挂载和 Goose 原生前驱身份都发生在更新的那一轮的第一个 invocation 里。删除顺序不限：删除前宿主先落不可变墓碑 `state/run-deletions/<run>.json`（记 Run、Graph bundle 身份、Session、回复节点与被删 Run 自己当时的上一个 Run），血缘遍历遇到缺失前驱时用同一会话的墓碑继续往后走，所以删中间某一环不会孤立更早的历史，幸存血缘仍是一条链、只有一个头；若幸存环节都已是被删环节的前驱，则该会话没有活动链，下一条消息重新开链。没有墓碑的缺失前驱仍然 fail-closed。删过的 Run 不能恢复，墓碑不随删除移除。要一次清掉整条会话历史可以删除整张 Graph，但会连 Graph bundle 一起删除。

### 插件/图更新的操作顺序

替换 Graph/Plugin 资源（`PUT /graphs/{graph}` 换 bundle）在存在未收束 Run 时会被前置条件挡下：`has_unfinished_plugin_run` 对任何 `plugin_bindings` 非空、状态不是 `Completed`/`Failed`/`Aborted` 的同 Graph Run 返回真，所以 `Stopped`/`Paused`/`WaitingRecovery`/`WaitingCall` 等一律算未收束。Run 一旦接纳，它的 Graph 快照与 Plugin binding 就冻结到该 Run 结束，续跑只使用冻结资源。更新顺序因此是：

1. `POST /runs/{run}/abandon`，body `{"reason":"plugin_update"}`（body 可省，默认 `operator`；其他 reason 返回 422）。它先落一份不可变 `abandon-intent`（Host 中途崩溃也会在下次启动收成终态），再沿既有 stop 链真正取消在跑的节点执行，最后把 Run 落成终态 `Aborted` 并写明审计理由。请求幂等：同一 Run 重复调用返回同一 body、不重写记录；换 reason 返回 409；`Completed`/`Failed` 返回 409。
2. 刷新 bundle：`PUT /graphs/{graph}`，此时前置条件已满足，可以替换 graph.json 与 Plugin 包。
3. 重启 Host 或等待既有自动交接：abandon 不删除任何事实，被放弃实例的 current 绑定、稳定 workspace、Artifact、Turn 历史与未提交现场都原样保留，自动恢复/显式恢复按 `previous_run = 旧 Run` 把同一个 Session 交接给新 Run。
4. 新 Run 在新摘要下执行；旧 Run 作为不可变历史保留（终态 `Aborted`，`POST /runs/{run}/resume` 返回 409）。

为什么不能 `stop → 替换 → 续跑`：`stop` 只把 Run 停在 `Stopped`，它仍可 resume、也仍算未收束，会继续挡住 `PUT`；即便绕过该前置条件，续跑用的仍是旧 Run 冻结的 Graph 快照与 Plugin binding，拿不到新资源，「在新摘要下续跑」这个语义本身不成立。`retire` 也不能替代：它只解除 current 绑定，不取消未收束的 Run，且要求 Run 先到 `Stopped` 或终态。因此 `abandon` 是插件/图更新场景唯一的一次性、明确不可续跑的动作，它只改状态与审计理由，不做停止/退役/删除的隐式替代。

Graph cascade 删除须先保护仍 active/未收束执行、共享历史与未结算投递，再 retire 目标助手，按 owned Run/Turn/invocation bindings 清理 input/reply/yield/checkpoints、Artifact 与工作区；不能按整个根目录扫除，也不能把停止/退役等同于删除。opt-in 切换不清理已有 39 个生产 workspaces，不原地迁移或冒充旧恢复游标。

## 业务工具与附件边界

显式挂载 Plugin 才能查询或操作业务系统。需要联网的 MCP 要求节点 `network=true`，仍受宿主授权；运行中不得原地修改已冻结资源。Bot ID/Secret 与自建应用的 Corp ID/Agent ID/Secret 是两套凭证。

- `wecom_send_message(userid, content)` 通过受限宿主控制通道主动发送 Markdown。仅在用户明确要求或已经授权的定时任务中调用，目标须在允许名单内；普通回复不重复调用发送工具。
- `wecom_attach_image(path)` 是受限回复节点工具，选择工作区或授权只读输入中的真实 PNG/JPEG。登记图片不等于平台成功投递：Host 在交给网关前会重新校验数量、单张/合计大小、PNG/JPEG 与 md5，网关再用三阶段上传换 `media_id` 并以原始回调帧单独发出一条 image 消息，投递结果记录在网关 `deliveries`（`image:<inbound>:<index>`）中，只有确认后才算平台接收。出站任意文件类型平台未证实，本轮不支持。
- 自建应用 MCP 由 `anchor-wecom-tools` 提供 `wecom_send_text`、`wecom_send_markdown` 和 `wecom_get_user`，另行配置 `WECOM_CORP_ID`、`WECOM_AGENT_ID`、`WECOM_SECRET`。未配置的可选 MCP 不影响基础机器人聊天。

Rust Host 支持可信入口提交规范化 Base64 附件，冻结 SHA-256、大小与 MIME，并以 `/in/channel` 只读挂载；一次性助手按 Run，常驻助手按可信当前 Turn 保存和选择，不把 Run 的所有历史附件开放给模型。入口拒绝宿主路径、非法文件名、伪造类型和越界数据；Goose/provider 是否能理解图片需单独验证。

常驻等待 invocation 还固定一份有界 `interrupted_messages` 快照：只选当前输入之前、最近一次 confirmed 投递之后最多最近 8 条中断输入，附文本及真实附件引用，分别只读挂载 `/in/channel-pending/<turn>`。模型提供的路径不能扩大范围；后续轮次越过 confirmed 边界后不再自动挂载这批附件。原生 Goose 历史可以保留过去的消息/图片上下文，但不等于 Sandbox 获得全历史文件授权。

Host 首次 wait binding 查询最近 9 条来检测截断，只授权最近 8 条并保存 `pending_truncated`；`output.interrupted_messages_truncated=true` 时，Agent 必须说明更早中断输入未自动纳入/未完整阅读，不能声称信息全保留。这个标记不扩大有界 scope，不改变数据库 schema；旧 facts 默认 `false`。

官方 image/file/voice 回调由原生 Gateway 自己接入：回调只带短时下载 URL 与 AES key，网关在自身进程内完成 HTTPS 下载与 AES-256-CBC 解密（限额 16 项、20 MiB/项、50 MiB/事件），只把解密后的 `{name,data_base64,media_type}` 交给 Host 的既有契约，临时 URL 与 key 不进入 Host 或 ledger；ledger 只保存描述符，所以重启恢复时会重新下载，URL 过期就如实失败而不是伪造成功。含媒体的 mixed 消息支持文本与媒体混排。boundary 仍然明确：不支持 video/audio 回调；Host 不做旧文本提取、Office/PDF/OCR，文档内容以 `/in/channel` 中的真实字节可读为前提，能否理解由模型与授权工具决定；不通过模型提示词绕过这些边界。

成员名单控制助手入口；业务 MCP 使用操作员配置的凭证。对话隔离不能代替下游系统的逐用户数据授权。审批、借款与报销等 API 仍需按实际业务模板接入。

## Docmost 和验收

可在助手节点显式挂载 `docmost` 并开启授权网络，以服务环境中的 `DOCMOST_API_KEY` 访问知识库；原生附件上传工具见 [Docmost README](../rust/anchor-docmost-tools/README.md)。搜索、读取和页面变更均以当轮实际工具及权限为准，不声称本机配置已经部署成功。

常规运行验证使用 [确定性 Goose fixtures](runtime-contract-tests.md)，经过实际 Host、共享 Runner、Goose ACP 与授权 MCP，替换模型传输。Gateway 的本地传输测试为：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-wecom-gateway --all-targets --locked
```

常驻路径的定向 integration 位于 `rust/anchor-runner-host/tests/goose_persistent_assistant.rs`，需要固定真实 Goose 二进制与本机 Bubblewrap，但模型传输为确定性本地 Provider。按 fixture 文档准备隔离环境后可运行：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --test goose_persistent_assistant --locked -- --ignored --test-threads=1
```

上述确定性 integration 已实际执行 9 项并全部通过，最终一次整文件串行回归证据目录为 `/tmp/anchor-persistent-goose-final-20261008-t4kBUZ`。覆盖同 Run 两轮/稳定 workspace 与旧 fs2 不变；等待零模型调用、暂停/停止与重启后显式 resume；新输入打断时真实退出、checkpoint/Yielded/Interruption 与旧回复 suppressed；双用户/重复事件身份隔离；当前与 pending 附件只读且有界、图片按 Turn/回执不串轮；显式退役与新版一次性 seed。领取后、产物前重启、截断提示、未知投递阻断删除与 owned 清理由 Session/Host 定向测试另行覆盖；不据此宣称全部业务 Graph 或真实企微已验收。

自动恢复新增的 `host_restart_automatically_resumes_the_instance_with_its_workspace_and_session`（旧 Run 对账、自动交接事件、同 Goose session、新实例继承现场、旧 fs2 不变）与改写为开关关闭路径的 `host_restart_requires_explicit_resume_when_automatic_recovery_is_disabled` **尚未在本机执行**：需要固定真实 Goose 二进制与本机 Bubblewrap，按上节流程复跑后才更新证据目录与通过项。非 Goose 环境中 Agent 能力未配置，Host 定向测试只能证明恢复决定、交接、在途 Turn 改判、陈旧 Run 对账与重试幂等，不能证明接纳成功后的活实例。

2026-10-08 主轨真实 provider 隔离两轮已实际通过，证据为 `/tmp/anchor-persistent-provider-20261008-85r0_d_6/evidence.json`。读取 `.env` 后以子进程环境白名单剔除所有 WeCom 生产凭据/roots，未使用实际企微 transport；同 Run `assistant-e62aff4a-38fb-4391-b9e1-5266dbdfcd9e`、稳定 workspace 同 inode，`proof.txt` 从 `first\n` 延续为 `first\nsecond\n`，第一轮 fs2 仍为 `first\n`，回复后回到 `wait_input` 并显式 stop 退出。两次 Agent completion 的 `model_requests` 分别为 8/5，计数来自节点 completion，不等同于精确累计预算证据。该证据不覆盖实际企微、并发/打断/媒体等全部边界，不表示生产已切换；台账由主轨切片收尾更新。

真实账号的验收应分别检查基础收发、跨轮事实、双用户隔离、补充消息取消、进程重启（默认自动恢复后的实例、Goose 会话与现场；以及 `ANCHOR_ASSISTANT_AUTO_RESUME=0` 时仍要求显式 resume）、允许名单，以及媒体：出站图片以网关 `deliveries` 的 `image:<inbound>:<index> = confirmed` 加上用户实际看到图片为准（不能只看登记或 ACK）；入站附件以 Host 附件 manifest 的 name/sha256/size/media_type 与 `/in/channel` 内实际字节为准。常驻助手的实际企微仍待用户私聊验收，尚未部署。历史部署证据保留在开发台账；本地 fixtures 或隔离真实 provider 不等于真实公网投递，未跑通的路径不写成已通过。
