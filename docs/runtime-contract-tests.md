# 小型 Runtime Graph 回归：Goose 确定性入口

当前标准回归使用现有纯 Rust `anchor-devtools`，实际经过 Host、共享
KernelScheduler、Goose ACP、授权 MCP、Bubblewrap 和 Artifact，只替换模型传输及
远程业务 endpoint。官方执行和回归入口均在 Rust workspace；历史验收证据
保留在台账和归档中，不自动成为当前源码的验收。

所有新 Run 默认使用 Kernel；回归不再选择 `execution=kernel_probe`，也不保留旧执行器。Runtime 冻结旧侧对照语料包含 30,000 个种子，常规测试读取各快照的前五分之一，共 6,000 个种子，覆盖串行、并行成功/失败、模块和 Call；罕见失败顺序由固定种子专项测试保留。Host 门面另持久化回放其中 1,000 个串行 seed，HTTP 测试经过真实 Op.run/Bubblewrap/Artifact。它们不启动 Goose，不能替代下列固定 Goose 确定性回归。快照来源与比较规则见 [M3 基线](../experiments/constitution-host-preflight/m3/snapshots/README.md)。

M4 起按迁移任务书 §10.19 只在 master 工作目录执行，使用标准 target /tmp/anchor-goose-regression-target；旧 constitution-core worktree 只读，不再使用其 -core 构建目录采证。其他独立构建工作流不得覆盖正在采证的产物。

## 标准命令

从仓库根目录运行，显式指定已经审查的 Anchor 自建 lean Goose ACP 二进制（[构建脚本](../scripts/build-goose-acp.sh)，上游 v1.53.0 源码）：

```sh
export CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target
export CARGO_BUILD_JOBS=2
cargo +stable build --manifest-path rust/Cargo.toml --locked \
  -p anchor-runner-host -p anchor-sandbox-bwrap -p anchor-wecom-tools -p anchor-docmost-tools -p anchor-git-tools --bins
cargo +stable build --manifest-path rust/Cargo.toml --locked \
  -p anchor-mcp-host --example http_fixture

ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY="$CARGO_TARGET_DIR/debug/anchor-net-relay" \
cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools --locked -- \
  regression goose-fixture --evidence-root /tmp/anchor-goose-regression
```

`regression fixture` 是同一 `run_goose_fixture` 的 CLI alias，执行相同 Goose
套件；不是旧框架开关。兼容 `run_fixture` API 也委托该函数，不另写 Runner。

需要固定依赖支持的 stable Rust、Linux、Git、Bubblewrap 和可用的
user/mount/network namespace。入口在测试前校验 Goose 可执行权限和 SHA256
`17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640`，
不下载 binary、不加载 `.env`。缺 binary、digest 不符、构建失败、必要测试
未执行或证据缺失均不能算通过。

| 参数或环境 | 契约 |
| --- | --- |
| `--workspace-root` | 仓库根；默认编译时的仓库根 |
| `--target-dir` | 默认 `CARGO_TARGET_DIR`，未设置时为 `/tmp/anchor-native-regression-target` |
| `--evidence-root` | 新私有证据目录的 parent，默认系统临时目录 |
| `ANCHOR_GOOSE_BINARY` | 已验证的固定 Goose 可执行文件的绝对路径 |
| `ANCHOR_GOOSE_LOCAL_NETWORK` | 标准回归显式设为 `1`，通过 Unix socket 与 relay 访问 Host 控制面 |
| `ANCHOR_GOOSE_RELAY_BINARY` | 与本次 Host 同一构建的 `anchor-net-relay` 绝对路径；Goose 位于其他目录时必须显式指定 |
| `CARGO_BUILD_JOBS` | 编译并发，可按机器资源设置；不是模型请求限制 |

不同编译工作流使用独立 target，不同时覆盖正在采证的 Host binary。
构建必须包含 `anchor-sandbox-bwrap --bins`，使 `anchor-sandbox-network`
与 Host 位于同一目录；仅构建 Host 不会构建依赖 crate 的 binary。
缺少该 helper 时，已授权任务仍不能联网，外部效果用例会在模型收到工具错误后超时；
不能把已有 target 恰好保留的 helper 当作干净环境前提。
Plugin MCP/披露/托管配置场景还要求同一 target 的 `examples/http_fixture`；
上述第二条 build 显式准备该原生测试工具，普通 `--bins` 不会构建 example。
每次执行创建新 `anchor-goose-regression-*` 根，不从旧证据目录推断本次通过。
Goose 套件逐套串行运行，不与其他 Goose 回归进程并行。并发负载下出现的
失败须单独重跑，首轮和复跑结果分别保留。

`ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1` 不能代替 relay。当前 Sandbox 的任务
联网通过 slirp 保留独立 network namespace，不能访问宿主 loopback；没有
relay 的构建目录会导致 ACP 扩展初始化出现 `-32603`，模型尚未收到请求。
fixture 在缺少 relay 时直接报告构建命令及所需环境，不输出原始 ACP 或 stderr。

完整运行 Host 的全部忽略测试（包含发行包与原生接口实验）时，沿用上面的
target 和构建结果，另需 `uv` 在 PATH、`/usr/bin/python3.12` 支持 ensurepip，
以及本机已安装的 stable Rust 工具链：

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY="$CARGO_TARGET_DIR/debug/anchor-net-relay" \
ANCHOR_TEST_RUST_TOOLCHAIN=stable-x86_64-unknown-linux-gnu \
ANCHOR_PILOT_RUSTUP="$(command -v rustup)" \
ANCHOR_PILOT_RUSTUP_HOME="$(rustup show home)" \
ANCHOR_TEST_EVIDENCE_ROOT=/tmp/anchor-goose-baseline-evidence \
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --locked --no-fail-fast -- --ignored --test-threads=1
```

需要记录采证前后 binary 身份时，先按最终执行范围预编译，再计算 SHA256：

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --locked --no-fail-fast --no-run -- --ignored --test-threads=1
```

workspace 与单包命令可能聚合不同的依赖特性，从而重新生成 Host 和 relay；
上述预编译必须在 workspace 检查和所有 helper/example build 之后最后执行，再冻结身份。
helper build 即使没有源码变化也可能切换特性集合；测试命令启动时会把 Host/relay 链接回测试集合的缓存产物。
因此不能在预编译与冻结之间再 build。采证期间不执行会覆盖这些 binary 的构建；
独跑与完整套件复用冻结后的产物，并在运行后复核摘要。

普通 fixture helper 的 filtered 数不是忽略测试数；完整命令的 `--no-fail-fast`
保证一个套件失败后仍采集后续套件。真实模型与外部服务不由本地 Provider
结果代验。

M3 按 §10.15 完成恢复契约更正和回归修复：完整 Host ignored **122 / 0 / 0**，原 12 项失败逐项单独复跑各 **1 / 0 / 0**；workspace 2,153 / 0 / 125、Web 333 与构建、严格 Clippy/fmt 均通过。固定 Host/Goose/helper 前后身份一致，原 110 / 12 / 0 停止现场保留；见 [M3 最终交回](../experiments/constitution-host-preflight/m3/completion-verification.md)。S27 已批准并结清 S4，M3 已快进 master。M4 完整门槛见 [当前交回](../experiments/constitution-host-preflight/m4/verification.md)；真实模型 Provider 与外部实网未新增验收。以下 G0/F2 是各自提交的历史基线。

## Goose 比较基线（G0，2026-10-19）

基点 `e3a5778`，在独立 worktree `/var/tmp/anchor-g0` 的
`fix/goose-baseline` 分支验证。固定 Goose 摘要保持 `17e065a7…`。
共同的 `extensions/add -32603` 来自回归启动缺少 relay：原始 ACP 响应为
MCP discover 的 loopback 连接失败；stderr 的 39 字节实际是 slirp 初始化
信息。旧 `dc2a4352…` 与固定二进制在相同网络条件下结果一致，不需要更换
Goose。原本机证据位于该 worktree 的 `.local/goose-baseline-20261019/`，
已随用户要求清理 worktree，不再作为当前可访问证据；此处保留提交中的基线结论。

以上完整忽略测试命令实际退出 `101`：**116 passed、3 failed、0 ignored**。
其中 Goose 套件为 112/3/0，`native_plugins` 为 2/0/0，Host 忽略单测为
2/0/0；1,114 filtered 仅表示未选择普通测试。完整命令内部逐套执行、每套
`--test-threads=1`；三个失败随后逐项独立复跑，监测期间无其他 Goose
套件运行，均为 0/1/0。它们是当前残留基线，不算全绿或真实 Provider 验收。
历史总数包含现已退役的两项 native activation spike 回归；下表仅列保留套件。

| 套件 | 通过 | 失败 | 忽略 |
| --- | ---: | ---: | ---: |
| `anchor_net_relay` | 0 | 0 | 0 |
| `anchor_runner_host` | 2 | 0 | 0 |
| `channel_startup` | 0 | 0 | 0 |
| `goose_acp` | 15 | 0 | 0 |
| `goose_channel` | 4 | 0 | 0 |
| `goose_compaction` | 4 | 0 | 0 |
| `goose_completion` | 9 | 0 | 0 |
| `goose_conversation` | 12 | 1 | 0 |
| `goose_development_environment` | 1 | 0 | 0 |
| `goose_distribution` | 0 | 1 | 0 |
| `goose_elicitation` | 10 | 0 | 0 |
| `goose_library` | 1 | 0 | 0 |
| `goose_media` | 6 | 0 | 0 |
| `goose_model_settings` | 2 | 0 | 0 |
| `goose_persistent_assistant` | 12 | 0 | 0 |
| `goose_pilot` | 2 | 0 | 0 |
| `goose_pilot_attachments` | 2 | 0 | 0 |
| `goose_pilot_authoring` | 3 | 1 | 0 |
| `goose_pilot_compaction` | 3 | 0 | 0 |
| `goose_plugin_configuration` | 1 | 0 | 0 |
| `goose_plugin_disclosure` | 1 | 0 | 0 |
| `goose_plugin_mcp` | 2 | 0 | 0 |
| `goose_plugin_summary` | 2 | 0 | 0 |
| `goose_plugin_turns` | 6 | 0 | 0 |
| `goose_provider_errors` | 4 | 0 | 0 |
| `goose_responses` | 2 | 0 | 0 |
| `goose_session_calls` | 4 | 0 | 0 |
| `goose_trace` | 2 | 0 | 0 |
| `local_inputs` | 0 | 0 | 0 |
| `local_network` | 0 | 0 | 0 |
| `multinode` | 0 | 0 | 0 |
| `native_plugins` | 2 | 0 | 0 |
| `parallel` | 0 | 0 | 0 |
| `routing` | 0 | 0 | 0 |
| `run_access_fence` | 0 | 0 | 0 |
| `standalone_graph_call` | 0 | 0 | 0 |
| `standalone_library` | 0 | 0 | 0 |
| `standard_host` | 0 | 0 | 0 |
| `writer_lease` | 0 | 0 | 0 |

G0 留下的三项失败及原因：

- `goose_conversation::next_conversation_invocation_changes_model_and_endpoint_without_losing_history`：fixture 重启只切换 Provider 启动参数，没有更新已保存的模型配置；第二个 Provider 收到 0 次请求，旧 Provider 的脚本耗尽，Run 停止而测试等待 completed 超时。后续应通过现有模型设置 API 更新该 fixture 的配置。
- `goose_distribution::extracted_goose_runtime_resumes_without_sources_or_replaying_workspace_effects`：fixture 将 `anchor-sandbox-network` 以硬链接暂存，发行 builder 在 Goose 执行前按原契约返回 `only regular, non-hardlinked files are supported`。后续应为打包输入准备独立的普通文件，保留发行检查。
- `goose_pilot_authoring::candidate_declaration_cannot_enable_network_or_credentials_and_command_cancel_can_continue`：最终仍断言 Session 插件列表为空，与当前新 Pilot 默认挂载 `network` 的契约不符；实际返回只有默认 network 项。候选越权拒绝和取消后继续已到达最终检查，但本测试整体仍失败。

本轮仅补齐测试环境、缺 relay 的明确提示及基线文档；未改既有断言、忽略标记、
超时、生产权限或恢复语义。首次受其他工作流并发影响的复跑日志单独保留，
不作为独立确认；最终逐例日志和并发监测在本机证据内。没有新增真实模型、
外部渠道、部署或 source-free 成功验收。后续任务应与这份名单比较，新增
失败须独立确认，不能把残留项悄然计为通过。

## F1 后的 G0 fixture 修正（2026-10-19）

F1 完成提交 `0116074` 保留旧断言，并以 G0 三项为失败上限；随后按用户要求
单独修正以下测试，不改变生产实现、权限、持久格式或恢复规则：

- 模型切换通过现有 `PUT /settings/model` 更新保存的配置及 `models.worker` alias。
  仅替换启动参数不会覆盖持久配置；原有切换 endpoint、模型和保留原生历史的断言不变。
- 发行输入将暂存的 `anchor-sandbox-network` 硬链接替换为独立普通文件。
  builder 的普通文件与链接检查保持不变，不通过放松发行约束来修正 fixture。
- Pilot 最终断言改为插件数量恰为 1 且 id 为 `network`，不是允许任意非空列表。
  理由是新 Pilot 默认挂载 network，原空列表断言已不符合当前契约；候选自报联网声明
  不能授予权限、宿主凭据不授权、真实命令取消后回收及继续核查的检查全部保留。

三项按标准环境、逐项 `--ignored --exact --test-threads=1` 复跑，各为 1/0/0；
证据见[修正验证](../.local/run-access-facade-20261019/g0-fixes/README.md)。
这不是另一次完整回归或真实 Provider 验收；G0 表保留为迁移比较基线。

标准入口现已通过：20 套、105 项通过，0 失败、0 忽略，逐项核验 105 份有效场景证据；
入口外补集另行串行执行，15 项全部通过。两次命令覆盖合计 120 项，不写成一次完整 ignored
命令通过。全部使用固定 Goose、实际 Host/ACP 与确定性模型传输，真实模型请求为 0，
未使用生产数据或部署。命令、原始报告与此前证据校验失败见 [F2 验证](../.local/run-access-facade-20261019/f2/README.md)。

证据修正独立于门面重构：会话删除场景补齐真实 Host/Goose 身份、原生事实、Provider
transcript 与删除后的现场清单，Plugin Turn 的 runtime 使用既有契约值并保留原描述。
原重新授权测试循环产生两份报告，与“每个通过测试恰有一份有效证据”的既有检查不符；
两参数分支现显式列为两例，共用原实现、全部断言与固定 Goose 执行条件。因此测试数比
G0 的 119 项增加 1，默认忽略数也增加 1，实际场景和覆盖不变；未放松校验器、断言、超时或权限。

## Trace 证据与宿主持久文件的原子发布

Goose trace 的 `store_evidence` 使用共享 `write_durable`：在目标同目录用唯一临时文件写完字节并 fsync，rename 覆盖后 fsync 父目录。既有文件保留权限位，新文件沿用普通创建的 `0666 & umask`；写入或发布失败由临时文件所有者清理，不增加依赖。序列化字节、文件名、调用先后、错误校验和已有断言/超时不变。

该辅助函数的六处生产调用全部获得原子替换，不保留原地截断路径：

| 调用点 | 文件 | 并发读取与处理依据 |
| --- | --- | --- |
| `goose_acp::trace::store_evidence`（原 `goose_acp.rs` 直接调用） | `{stem}.evidence.json` | `/runs/{id}` 的 trace 投影可与发布并发；必须只见完整旧/新版本 |
| `goose_acp.rs` 启动配置 | `config/config.yaml` | 当前在 Goose 启动前、会话租约内写入，执行本身已顺序化；共享辅助函数统一改为原子替换，不另留原地写例外 |
| `node_host/assistant.rs` 等待输入完成 | workspace 的 `input.json` | 节点交接虽然有顺序，workspace 仍可被现场核查与保留读取；统一原子发布，不让观察方读到半份 JSON |
| `node_host/execution.rs` 的 Sandbox InvalidRequest 分支 | `{stem}.failed` | 完成事实查询可读取失败原因；避免首次发布时看到空/不完整文本 |
| `node_host/execution.rs` 的非成功退出分支 | `{stem}.failed` | 同上，包括带 stdout/stderr 的多字节失败原因 |
| `node_host/facts.rs::complete` | `{stem}.json` | `load_host_completion_fact` 的查询/恢复读取不与写文件共享锁；避免把半份完成 JSON 判成损坏 |

未改的邻接协议：`*.started` 不经过该辅助函数，用 `create_new` 唯一认领；读取方只判存在即不确定，不解析写入中的内容，因此本任务不把认领改成可覆盖的 rename。Goose `store_fact` 原本已临时写入、fsync、rename、父目录 fsync，不另建第二套实现。

新增回归通过通道强制“读一段旧文件 → 另一线程完整发布新文件 → 继续读原文件”的交错，反复验证旧文件描述符的完整快照与新打开后的 trace 投影；另有持续并发重写/读取，只接受两个完整版本。前者在原地重写实现的第一次发布就失败，不依赖压力碰撞或放宽时间。修复前后原始输出、权限/失败清理测试及最终验收见[本机证据](../.local/trace-evidence-atomic-20261019/README.md)。这不是跨文件事务、真实 Provider 或公网渠道验收。

本次 workspace 实际 1,999 通过、0 失败、123 忽略；文档完整 Host ignored 命令实际 120/0/0，未使用时序失败豁免。标准入口另行执行为 20 套、105/0/0，105 份场景报告全部通过严格校验，真实模型请求为 0；与完整 ignored 重复覆盖的场景不累计为新测试。严格 workspace/all-target Clippy 与格式检查通过。

### 临时写入的崩溃回收

共享 writer 位于 `durable_write`。HTTP 在取得 deployment writer lease 后、启动恢复前清理；framed/stdin 在写入请求取得同一 lease 后、开始执行前清理，`status` 不触发清理或新建 writer lease，保持只读。清理为 best effort：忙目录、无法检查或无法删除的目录只记统计，不使宿主启动失败；不按文件年龄推断写入是否结束。

writer 从创建临时文件前到 rename、父目录 fsync 完成持有父目录共享锁，临时文件创建、目标权限读取、发布和失败析构均通过同一个 `/proc/self/fd/<dirfd>` 目录句柄路径操作，避免父目录路径被替换后锁与实际写入落点分离。cleaner 尝试同目录非阻塞排他锁，活动 writer 存在时跳过，进程退出后锁由内核释放。复用既有 `tempfile`、`rustix` 和 `anchor-file-lease`，不新增依赖或旁路锁文件，保留目标 JSON 字节、权限规则与原子发布语义。

清理只删除保留命名 `.anchor-write-` 加六位 ASCII 字母数字的普通文件，并在删除后同步目录；逐级按目录 fd 打开且不跟随符号链接。范围是 state 下的 `facts`、`goose-acp`、`goose-acp-spike` 当层，以及 `.goose-process` 下 invocation/conversation 的 config 当层；wait-input 的 workspace 通过 Run 门面、既有 invocation 归属和已初始化的节点绑定确定，覆盖稳定与未绑定目录、合法的点开头 Run ID，不按目录外形扫描其他节点或外来目录。不递归用户工作文件，不清 Library、Artifact 或其他临时文件前缀。被占用或检查失败的残留留待下次启动，不增加定时回收线程。

回归包含真实子进程持锁写入/fsync 后被强杀的残留、活动 writer 跳过、父目录在临时文件创建前/后被替换、同层/符号链接/用户子目录边界及 HTTP/framed 两个真实入口。执行命令、原始输出和未覆盖范围见[本机证据](../.local/durable-write-cleanup-20261019/README.md)；不是断电、真实 Provider 或部署验收。

## 套件与证据

入口先构建官方 WeCom、Docmost 原生工具，再逐套运行以下既有 Host 测试，
使用 `--locked`、`--no-default-features` 和
`-- --ignored --test-threads=4 --nocapture` 选择需固定 Goose 的确定性场景。
这是显式运行带 ignored 标记的场景；选中后仍忽略的测试不能通过验收。
普通 helper 的过滤计数与场景通过数分别记录。

| 套件 | 核查边界 |
| --- | --- |
| `goose_acp` | 实际 Agent loop、结构化完成与回执、工具权限、取消、未知效果核查后继续及保留的 spike 反例 |
| `goose_completion` | 成功提交后直接结束、原生工具记录完整、Chat/Responses 同批任意回执、旧回执拒绝与纠正、混批/重复完成拒绝 |
| `goose_pilot` | 原有授权工具、同原生 Session 续聊/重启、幂等、SSE 重放、变更后的现场核查与取消 |
| `goose_provider_errors` | 结构化 Provider 错误、认证/额度失败、摘要失败、原始信息不外显、失败请求幂等和同 Session 重启续聊；本轮执行边界见[记录](../.local/provider-errors-20261018/verification.md) |
| `goose_model_settings` | 设置变更后的下一轮/节点选择、当前执行冻结、同原生历史续聊、四种显式模型引用及无自动降级 |
| `goose_responses` | Responses 推理项与工具调用完整回传、原生会话续聊/重启、Graph 完成回执 |
| `goose_pilot_attachments` | 纯附件输入、原生图片 wire、只读文件、不可变发布、工具图片、重启/幂等与恢复引用 |
| `goose_elicitation` | 原生问答、条件删除确认/拒绝、目标变化、停止和重启不重放确认 |
| `goose_media` | MCP PNG/JPEG/WebP、混合内容顺序、大图、模型 wire 图片、媒体拒绝、Artifact 与重启核查 |
| `goose_conversation` | 跨 Run 与循环原生历史、用户/节点隔离、只读前驱、冻结图片、身份拒绝与整图清理 |
| `goose_persistent_assistant` | 常驻助手同 Run 多轮：稳定 workspace 与不可变 fs2 历史分离、逐 Turn 输入/打断与零模型等待、多用户隔离、恢复/退役/删除不越权 |
| `goose_plugin_turns` | Graph 保存后下一 Turn 增删插件、当前工具调用冻结、多用户独立生效、改名重建与重启交接保留本轮授权 |
| `goose_plugin_configuration` | 双 MCP 共用托管配置、轮换后新 invocation 生效且当前调用冻结、清除不回退环境及凭据不进入模型请求 |
| `goose_channel` | 私有 Unix gateway、原生发送身份、相同正文不同调用、越权拒绝与丢 ACK 后不重发 |
| `goose_compaction` | Goose 原生压缩、Plugin/现场重读、取消、重启和同 prompt 超窗恢复 |
| `goose_pilot_compaction` | Pilot 压缩、续聊/重启、显示历史/SSE、摘要取消和变更后核查 |
| `goose_trace` | prompt 活跃期原生文本/工具结果、状态配对、停止与重启后保留显示和 Artifact |
| `goose_session_calls` | wait/detach、child Run、Session 映射、local ACK、重启结算及 yield/前台/续行 |
| `goose_library` | 本地 Library 安装后实际 Goose 小图接线 |
| `native_plugins` | 实际只读 Skill、原生 stdio MCP、loopback 业务结果及 workspace/Artifact |

模型和业务服务均为本地确定性 fixture，不调用真实模型、不发送公网消息、
不发布文档、不读取生产数据。视觉模型名和 fixture usage 仅驱动实际 Goose
的传输/压缩分支，不证明真实图片理解或真实 token 费用。

顶层 `evidence.json` 保存实际命令、退出码、构建结果、测试计数、耗时、
固定 Goose 身份和失败原因。日志含 `fixture.log` 与每套独立日志，场景根在
`fixture/<suite>/`。每个通过测试必须有一份有效场景证据，匹配本次 Host
binary、固定 Goose digest、已完成且无错误的确定性 Provider transcript，
并声明零真实模型调用及未使用生产数据。启动前权限拒绝的反例按原有独立
契约核查，不能冒充实际 Goose Graph 执行。

场景核查实际 Run/节点历史、工具请求与结果、workspace、Artifact manifest
及文件 bytes/hash。Goose 原生历史由 Goose 持有；ACP 显示投影不是另一套
恢复日志。Runtime 通过公开 ACP 恢复。缺 usage 是未知，不写成零成本；
模型调用、完成声明和业务内容质量
分别记录。

## 浏览器与真实验收边界

浏览器定向入口保留 `apps/web/e2e/goose-native-trace.spec.ts` 与
`pilot-goose-elicitation.spec.ts`，显式配置 `ANCHOR_TEST_GOOSE_HOST_BINARY`
和 `ANCHOR_GOOSE_BINARY` 后运行实际 Rust Host/Goose、本地 Provider。
`goose-trace-media.spec.ts` 只验证 UI 投影。需先构建 WebUI；缺配置而跳过
不算通过，UI 图片解码也不等于真实 vision 端到端验收。

```sh
ANCHOR_TEST_GOOSE_HOST_BINARY=/absolute/path/to/anchor-runner-host \
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
ANCHOR_BROWSER_BINARY=/absolute/path/to/chrome \
ANCHOR_REAL_PROVIDER=0 \
npm --prefix apps/web run test:e2e -- \
  goose-native-trace.spec.ts pilot-native.spec.ts pilot-goose-elicitation.spec.ts
```

`ANCHOR_BROWSER_BINARY` 可省略以使用已安装的 Playwright 默认 Chromium。
其他原生 spec 使用同一隔离 Host/Provider helper；`real-provider.spec.ts`
只有显式 `ANCHOR_REAL_PROVIDER=1` 才进入真实模型验收。

固定 Goose 未暴露 `final_result.summary` 参数的逐 token 增量，普通 assistant
text 不能冒充 canonical 完成摘要。Pilot 手动 `/compact` 的公开接入和任意
长上下文、多次压缩、摘要错误及媒体/问答组合应按当次证据单独判断。

真实 Provider 的自主工具选择、完成/反馈、当次 model/wire 兼容性须另作
小型端到端验收；确定性回归不能关闭这类验收。完整 RSI、深度研究、周报，
真实 OAuth、公网 WeCom/Docmost/学术服务和完整渠道媒体组合仍是低频业务
验收，本轮暂缓。不为维护每个旧 smoke 另建 Runner 或扩大常规回归 Graph。
目标机发行依赖、旧数据策略、单写切换/回滚和生产授权也分别验收。

当前本地 source-free 候选包检查见
[Rust Production Candidate](rust-production-candidate.md)；
部署边界见 [部署指南](rust-production-deployment.md)，运维预检通过原生
`anchor-devtools preflight` 执行。

## 历史证据

旧 Runtime contract、原生 Pilot/Plugin live 驱动、三份 live fixture Graph 和
依赖 Harness 私有记录的 Rust 业务 smoke 已移除；其命令不能用于当前源码。
旧指南与代码可在 commit `45d4bbfa189aafb5a41bd8a8b05295e98749ca6b` 查看，
例如 `git show 45d4bbfa189aafb5a41bd8a8b05295e98749ca6b:docs/runtime-contract-tests.md`。
[开发台账](pilot-development-plan.md) 和归档中的旧 io-harness/Rig 记录保留，
不改写为 Goose 已通过。

已有 Goose 历史验收也保留其原范围：A119 七套 44 场景；A121 九套 51 场景，
含七项压缩组合；A122 十一套 57 场景；A124 十二套 59 场景、零真实模型、
345.323 秒。A124 证据根为
`/tmp/anchor-g3-parallel-iXLuGH/anchor-goose-regression-mKrmns/evidence.json`。
A119 的真实 Chat 两轮/重启、A121 的真实短文本压缩/重启证据只证明当时的
配置和短会话，不表示清理后的源码重跑通过或所有 Provider/vision/生产验收
已完成。实际推进和新验收结果继续维护在开发台账。

## 维护约定

新增或修改 Runtime 产品能力时，复用现有小图、确定性 Provider 和故障控制
用例，按变化更新覆盖范围。涉及真实模型行为时另保留定向真实证据。
失败、跳过、未配置 Provider 和未覆盖边界如实区分；不凭绿色测试总数声称
全产品通过，不重建通用 Agent loop、上下文或恢复引擎。
