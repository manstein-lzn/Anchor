# Runtime 回归与候选验证

当前标准回归使用现有纯 Rust `anchor-devtools`，实际经过 Host、共享
KernelScheduler、Goose ACP、授权 MCP、Bubblewrap 和 Artifact，只替换模型传输及
远程业务 endpoint。官方执行和回归入口均在 Rust workspace；历史验收证据
保留在台账和归档中，不自动成为当前源码的验收。

| 要验证什么 | 入口与范围 |
| --- | --- |
| Runtime 调度、控制与冻结语料 | 普通 crate 测试；不启动 Goose |
| Host/Goose/工具的产品契约 | 下文标准 `regression goose-fixture`；实际执行确定性小图 |
| Release Web/Host 的 Source-Free 发行闭包 | [候选回归](#release-与-source-free-候选)；只选一个隔离解包与恢复场景，不是 DEB 安装或目标机验收 |

所有新 Run 默认使用 Kernel；回归不再选择 `execution=kernel_probe`，也不保留旧执行器。Runtime 冻结旧侧对照语料包含 30,000 个种子，常规测试读取各快照的前五分之一，共 6,000 个种子，覆盖串行、并行成功/失败、模块和 Call；罕见失败顺序由固定种子专项测试保留。Host 门面另持久化回放其中 1,000 个串行 seed，HTTP 测试经过真实 Op.run/Bubblewrap/Artifact。它们不启动 Goose，不能替代下列固定 Goose 确定性回归。快照来源与比较规则见 [M3 基线](../experiments/constitution-host-preflight/m3/snapshots/README.md)。

## 标准命令

从仓库根目录运行，显式指定已经审查的 Anchor 自建 lean Goose ACP 二进制（[构建脚本](../scripts/build-goose-acp.sh)，上游 v1.53.0 源码）：

```sh
export CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target
export CARGO_BUILD_JOBS=2
cargo +stable build --manifest-path rust/Cargo.toml --locked \
  -p anchor-runner-host -p anchor-sandbox-bwrap -p anchor-network-tools \
  -p anchor-wecom-tools -p anchor-wecom-gateway -p anchor-feishu-gateway \
  -p anchor-docmost-tools -p anchor-git-tools --bins
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

需要固定依赖支持的 stable Rust、Linux、Git、Bubblewrap、slirp4netns 和可用的
user/mount/network namespace，包括私有 PID namespace 的新 `/proc` 挂载。
入口在测试前校验 Goose 可执行权限和 SHA256
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
ANCHOR_TEST_EVIDENCE_ROOT=/tmp/anchor-goose-full-evidence \
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

## 套件与证据

入口先构建官方 WeCom、Docmost、Git 原生工具，再逐套运行以下既有 Host 测试，
使用 `--locked`、`--no-default-features` 和
`-- --ignored --test-threads=4 --nocapture` 选择需固定 Goose 的确定性场景。
这是显式运行带 ignored 标记的场景；选中后仍忽略的测试不能通过验收。
普通 helper 的过滤计数与场景通过数分别记录。
标准入口的套件集合以 [`SUITES`](../rust/anchor-devtools/src/goose.rs) 为准；
它不选择 `goose_distribution` 或 `goose_conversation`，也不等同于完整 Host ignored 命令。

| 套件 | 核查边界 |
| --- | --- |
| `goose_acp` | 实际 Agent loop、结构化完成与回执、工具权限、取消、未知效果核查后继续、角色系统提示与原生历史隔离及保留的 spike 反例 |
| `goose_completion` | 成功提交后直接结束、原生工具记录完整、Chat/Responses 同批任意回执、旧回执拒绝与纠正、混批/重复完成拒绝 |
| `goose_pilot` | 原有授权工具、同原生 Session 续聊/重启、幂等、SSE 重放、变更后的现场核查与取消 |
| `goose_provider_errors` | 结构化 Provider 错误、认证/额度失败、摘要失败、原始信息不外显、失败请求幂等和同 Session 重启续聊 |
| `goose_model_settings` | 设置变更后的下一轮/节点选择、当前执行冻结、同原生历史续聊、四种显式模型引用及无自动降级 |
| `goose_responses` | Responses 推理项与工具调用完整回传、原生会话续聊/重启、Graph 完成回执 |
| `goose_pilot_attachments` | 纯附件输入、原生图片 wire、只读文件、不可变发布、工具图片、重启/幂等与恢复引用 |
| `goose_elicitation` | 原生问答、条件删除确认/拒绝、目标变化、停止和重启不重放确认 |
| `goose_media` | MCP PNG/JPEG/WebP、混合内容顺序、大图、模型 wire 图片、媒体拒绝、Artifact 与重启核查 |
| `goose_persistent_assistant` | 常驻助手同 Run 多轮：稳定 workspace 与不可变 fs2 历史分离、逐 Turn 输入/打断与零模型等待、多用户隔离、恢复/退役/删除不越权 |
| `goose_plugin_turns` | Graph 保存后下一 Turn 增删插件、当前工具调用冻结、多用户独立生效、改名重建与重启交接保留本轮授权 |
| `goose_plugin_configuration` | 双 MCP 共用托管配置、轮换后新 invocation 生效且当前调用冻结、清除不回退环境及凭据不进入模型请求 |
| `goose_channel` | 私有 Unix gateway、原生发送身份、相同正文不同调用、越权拒绝与丢 ACK 后不重发 |
| `goose_compaction` | Goose 原生压缩、角色与授权指导保留、Plugin/现场重读、取消、重启和同 prompt 超窗恢复 |
| `goose_pilot_compaction` | Pilot 压缩、角色系统提示、续聊/重启、显示历史/SSE、摘要取消和变更后核查 |
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

### 飞书私聊专项

`goose_feishu` 不在标准 `SUITES` 中，按需显式执行。先在同一 target 构建
`anchor-runner-host`、`anchor-sandbox-bwrap` 和 `anchor-feishu-gateway` 的 `--bins`，
然后沿用上述固定 Goose、relay 与本地联网设置：

```sh
cargo +stable test --manifest-path rust/Cargo.toml --locked \
  -p anchor-runner-host --test goose_feishu -- \
  --ignored --exact private_text_roundtrip_reuses_run_and_native_history_without_duplicate_delivery \
  --test-threads=1 --nocapture
```

用例直接加载官方飞书 Graph，替换模型引用；任务通知用例另显式授予 fixture Graph 的
Graph Tasks 权限。实际监管 Gateway，经过二进制 WebSocket、消息回复/创建 API 对应的
loopback HTTP fixture、Host 与 Goose，核对两轮私聊、消息去重、ACK、同 Run/原生历史/工作区，
以及实际 Goose 工具完成事件在最终回复前触发卡片 PATCH、卡片 2.0 Markdown、私聊文件下载与
只读挂载、生成文件上传／发送、重复媒体不重下／重跑／重发。计划用例另核对原生工具建计划、
真实 Scheduler/任务 Run、结果轮显式 Markdown 通知及精确选中不可变文件外发、Gateway confirmed
账本及原始文件名。完整专项用 `--ignored --test-threads=1`，不加 `--exact` 过滤。
它们不代验真实飞书权限、应用发布或公网收发，
当前结论见[验收台账](pilot-development-plan.md)。

## Release 与 Source-Free 候选

`anchor-devtools regression candidate` 构建 WebUI，对 Rust Host、发行 builder、
官方学术工具和 WeCom Gateway 做一次 locked/offline release 构建，再只选择
`goose_distribution` 中已有的解包恢复测试。它不运行 workspace 全量或标准 Goose 套件。

### 隔离构建与命令

从仓库根目录使用已构建的 `anchor-devtools`；Goose 必须满足上文同一固定版本与摘要：

```sh
anchor-devtools regression candidate \
  --workspace-root "$PWD" \
  --goose /absolute/path/to/goose \
  --target-dir /tmp/anchor-production-candidate-target \
  --evidence-root /tmp/my-rust-candidate-evidence
```

Goose 路径优先级为 `--goose`、`ANCHOR_GOOSE_BINARY`、`ANCHOR_DISTRIBUTION_GOOSE`。
默认 target 取 `CARGO_TARGET_DIR`，未设置时为 `/tmp/anchor-production-candidate-target`，
可复用 release 缓存。候选的 `--evidence-root` 必须是尚不存在的目录，不同于标准
fixture 的 parent 参数；省略时创建新的临时证据目录，不覆盖已有证据。

构建不加载 `.env`，Web 构建显式禁用 dotenv。子进程只继承工具位置，使用私有 HOME；
独立 CARGO_HOME 只共享 registry/git 缓存，不继承用户 Cargo 配置或凭据。
不继承应用或 Provider 凭据，不修改生产配置/数据，不访问外部业务系统。
须事先备齐 Web 依赖、stable Rust 工具链及 Cargo 依赖缓存；缺缓存是未完成构建，不能当通过。

CLI 在上述隔离环境执行以下固定序列，而不是要求操作员以当前 shell 环境替代运行：

```sh
npm --prefix apps/web run build
cargo +stable build --release --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-runner-host -p anchor-distribution -p anchor-scholarly \
  -p anchor-wecom-gateway --bins
cargo +stable test --release --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-runner-host --test goose_distribution -- \
  --ignored --exact extracted_goose_runtime_resumes_without_sources_or_replaying_workspace_effects \
  --test-threads=1 --nocapture
```

最后一条可编译测试 harness，沿用同一 target。候选明确传入 `ANCHOR_TEST_WEB_DIST`、
`ANCHOR_TEST_ACADEMIC_PLUGIN`、`ANCHOR_TEST_SCHOLARLY_BINARY` 和
`ANCHOR_TEST_WECOM_GATEWAY_BINARY`；直接调用该 fixture 时省略这些输入得到的较小包，
不能满足候选验收。

### 解包恢复与证据

包内包含本次 WebUI、Host、小型 Graph/bundle、官方 academic-research Plugin 及其 Rust
scholarly binary、Gateway 和调用者提供的固定 Goose。场景实际运行解包后的 Host/Goose，
使用解包后的 bundle/Web root，核查 readiness、HTTP 页面及 JS/CSS 字节、源码缺席、
manifest 摘要、Run/节点与原生 Goose 历史、workspace/Artifact 字节和只读输入。
官方集成二进制只打包并校验摘要，不调用其业务工具。

候选入口与校验器的目标契约要求 **5 次本地模型请求**：stop 前 2 次、resume 后 3 次，恢复保持同一原生
Session，workspace 写入恰好一次；完成后的第二次重启不新增请求、不重放效果。
旧的 3 请求、仅完成后重启场景不能替代这条恢复验收。

当前保留的实现差异：[`goose_distribution` fixture](../rust/anchor-runner-host/tests/goose_distribution.rs)
断言 4 次请求，而[候选证据校验器](../rust/anchor-devtools/src/candidate/evidence.rs)仍要求 5 次。
两者尚未对齐且未复验，不能声称当前候选入口通过。

stdout 输出 JSON，并在私有证据目录保留 `report.json`：实际 argv、cwd、退出码、每条命令
日志与 SHA256、测试计数、已验证场景数、archive/manifest/Provider 摘要；进程未启动或被信号
终止时退出码为 null。观测到的场景/Provider 文件数与实际本地请求数分别记录，包括失败的
局部证据；场景保留归档、Runtime manifest、Provider transcript 与 Host 日志。

必须实际运行并通过恰好一个发行测试，得到恰好一份完整有效场景报告；exact selector 过滤
无关测试不算跳过所需场景。构建/测试失败、证据缺失或身份不符、Provider 脚本未耗尽、
额外请求或重复效果均失败。执行前的配置错误非零退出，不伪造成功报告。
这是 Source-Free 归档候选验证，**不是 DEB 安装、目标机 systemd/动态依赖验收或生产切换授权**。

维护此入口时可运行定向开发检查，不以它们代替上述实际候选场景：

```sh
cargo +stable test --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-devtools candidate::
cargo +stable test --locked --offline --manifest-path rust/Cargo.toml \
  -p anchor-devtools --test cli
```

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
验收。不为维护每个旧 smoke 另建 Runner 或扩大常规回归 Graph。
目标机发行依赖、旧数据策略、单写切换/回滚和生产授权也分别验收。

部署边界见[部署指南](rust-production-deployment.md)。运维预检通过
`anchor-devtools preflight` 输出 JSON；预检不替代实际沙箱、候选恢复或真实业务验证。

## 历史依据

历史结果只证明对应提交与场景，不计入本次通过数。旧 io-harness/Rig、已删除 smoke
及旧执行器命令不适用于当前源码，也不改写成 Goose 证据。查证按需读取
[归档入口](archive/README.md)、[固定 Goose 基线任务](archive/runtime-migration-2026-10-20/goose-baseline-task.md)、
[M3 最终交回](../experiments/constitution-host-preflight/m3/completion-verification.md)或
[M4 交回](../experiments/constitution-host-preflight/m4/verification.md)，不据此复刻旧工作树流程。
Trace 与其他单文件的持久写入、崩溃回收契约归[当前架构](architecture.md#宿主持久写入与清理)，历史反例见
[原子发布任务](archive/runtime-migration-2026-10-20/trace-evidence-atomic-task.md)。

## 维护约定

新增或修改 Runtime 产品能力时，复用现有小图、确定性 Provider 和故障控制
用例，按变化更新覆盖范围。涉及真实模型行为时另保留定向真实证据。
失败、跳过、未配置 Provider 和未覆盖边界如实区分；不凭绿色测试总数声称
全产品通过，不重建通用 Agent loop、上下文或恢复引擎。
