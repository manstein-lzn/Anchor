# 小型 Runtime Graph 回归：fixture 与低成本真实模型

2026-10-06 后续方向已改为 [Goose-only](goose-runtime-migration.md)。本文所列 io-harness/Rig 路径仍是已存在的历史/兼容回归，不能当作 Goose 产品证据；迁移将复用其小 Graph 和验收事实，不复制 Runner。真实 Goose 接入首片的命令与限制见 [A112 报告](goose-acp-spike.md)。预算控制按用户决定暂不纳入新执行层验收。

## Goose AgentNode 低成本回归

Goose 接入复用本页的小 Graph 验收方法，不跑大型研究任务。标准 Host/Kernel 默认不带旧框架。先固定实际 Goose v1.53.0 binary，用纯 Rust 开发入口构建官方工具并运行确定性小图：

```sh
CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target \
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools --locked -- \
  regression goose-fixture --evidence-root /tmp/anchor-goose-regression
```

`tests/goose_acp.rs` 同时保留历史 spike 反例，并新增 native 直接 Provider、回执校验、无默认短时限取消、越权和未知副作用核查后恢复。`tests/native_plugins.rs` 的 Goose 用例读真实只读 SKILL，通过 Rust 原生 stdio MCP 查询假 WeCom，再验 workspace/Artifact。实际 Goose loop、Host、Runner、ToolPort/Sandbox 都运行，只将模型和业务 endpoint 替换为确定性本地 fixture；不加载 `.env`，真实模型及生产业务调用为零。原生历史由测试核查 SQLite，Runtime 恢复只走公开 ACP，不读取私有数据库。模型 fixture 自检与未授权启动反例不算实际启动 Goose 的场景。

`tests/goose_pilot.rs` 用两组短会话核查原有授权工具目录、同一 Goose Session 的续聊/Host 重启、请求幂等、SSE 游标重放、原生 ToolRequest/ToolResponse、显示历史、变更后中断核查与取消；保留事实缺失时拒绝创建替代会话。验收不要求模型搜索论文或遍历代码。

入口校验固定 binary hash，十二个标准 Host 套件仅选择实际 Goose fixture，逐项核查证据文件、Host/binary 身份、确定性 Provider 的请求/消费/失败记录和零真实模型声明，失败如实保留；Provider/receipt 自检不冒充实际 Goose Graph。`goose_elicitation` 覆盖普通 Pilot 原生问答/删除确认，`goose_media` 六场景覆盖混合 PNG/JPEG/WebP、超过旧 ACP 上界的大图、模型 wire `image_url`、四类媒体拒绝、workspace/Artifact 和同 Session 重启核查而非重复副作用。媒体使用视觉模型名称但所有请求仍到本地 Provider，不证明真实模型看图。

`goose_conversation` 的 11 场景核查跨 Run/同 Run 循环复用原生 Session、Host 重启、用户与完整节点隔离、跳过节点后的最近原生历史、只读 `/previous`、workspace/Artifact、冻结图片输入、丢事实/身份/模型绑定拒绝及整图清理。未知副作用场景先用现有 `/stop` 结算旧 Run，再让新轮核查现场；不把 `running` 事实猜成已停止。`goose_channel` 的三场景通过真实 Goose 和私有 Unix gateway 验证同正文两次原生工具 ID/ACK、越权连接前拒绝、丢 ACK 后同 Session 核查而非重发；不发送真实 WeCom。A119 实际执行七套 44 场景/44 份证据，零真实模型；另有真实默认模型 Chat 两轮文本/重启/Artifact 验收。

`goose_compaction` 四场景覆盖入口阈值压缩、只读 Plugin/现场重读、新回执与不可变 Artifact、摘要取消、压缩后效果已发生时 Host 重启核查，以及同 prompt 的原生 context_length_exceeded 恢复；最后一个场景不需手动 stop/resume。`goose_pilot_compaction` 三场景覆盖原生摘要/同 Session 跟进/重启、显示历史与精确 SSE 重放、摘要取消后续聊，以及保存 Graph 后中断核查不重放。fixture 的 120007 usage 只用于触发 Goose 默认 128000 上下文的阈值，不发送巨量数据或伪造 Runner 结果；摘要请求必须是原生专用 prompt 且没有工具目录。

A121 实际九套为 12/2/9/6/11/3/4/3/1，共 51 passed/51 份证据、286.722 秒，零真实模型；新七场景复用实际 Goose 的压缩，不启实验循环或写私有数据库。另有真实默认 deepseek-flash 显式 Chat 三轮/压缩/两次重启的短文本验收，触发只在隔离测试根临时降低 Goose 公开阈值。它不代表百万 token 压力、多次压缩、摘要错误或提问/媒体组合都已验证。

`goose_trace` 两场景覆盖 prompt 尚未完成时的原生文本/工具结果、状态配对、stop 后保留显示和同 invocation/Session 重启核查，最终检查 Artifact 与效果只一次。`goose_session_calls` 四场景实际通过 Op.call 创建 child，并用 loopback Session resolve 与本地 ACK fixture 检查 wait/detach 接纳、映射和重启结算、错误 Session/previous 拒绝，以及后台 yield、同 Session 前台完成、重启后继续原后台 Run。后者复用可信且已结算的后继原生历史，旧 receipt 仍拒绝，普通旧 Run 仍不能越过后继 resume；不等于完整渠道 supervisor 或公网交付验收。

A122 历史证据为十一套 12/2/9/6/11/3/4/3/2/4/1，共 57 passed/57 份严格证据、325.210 秒、零真实模型。A124 的当前统一入口扩为十二套，共 59 passed/59 份证据、0 failed、0 real model requests、345.323 秒；新增 Rust 文本 gateway ACK 及 Library 安装小图已进入同一入口。A124 证据为 `/tmp/anchor-g3-parallel-iXLuGH/anchor-goose-regression-mKrmns/evidence.json`，不是对真实模型或生产业务的证明。

浏览器定向入口为 `apps/web/e2e/goose-native-trace.spec.ts`（显式 `ANCHOR_TEST_GOOSE_HOST_BINARY`、`ANCHOR_GOOSE_BINARY`，实际 Rust Host/Goose、本地 Provider、零真实模型）和 `goose-trace-media.spec.ts`（仅 UI 投影 fixture）。前者检查完成前 trace、刷新后从时间线重新进入节点、canonical summary 和 Artifact；后者检查混合 Text/Image 原序、交错 tool-call 身份、thinking 折叠、真实图片解码/像素、原图查看、拒绝外链/SVG 与移动端无横向溢出。两者不能合称真实视觉模型端到端验收。

真实 vision、公网渠道、媒体输入/输出完整组合、`final_result.summary` 逐 token 增量、完整浏览器及独立发行/生产切换仍未关闭；实际状态见开发台账及 [Goose-only 计划](goose-runtime-migration.md)。固定 Goose 未暴露完成工具参数的逐 token 更新，不将普通 assistant text 冒充完成摘要。Pilot `/compact` 仍是包装后的普通文字，不冒称原生手动命令可用。`anchor-devtools regression fixture` 是显式启用 `legacy-regression` 的兼容入口，运行旧 contract 与 exact 两项原生 Plugin；它不是 Goose 产品验收。

## 目的与执行边界

常规开发回归验证执行语义，不重复运行完整 RSI、深度研究或周报。统一入口是 `scripts/rust_low_cost_regression.py`，包含确定性 fixture 和低成本 live 两层。fixture 测试目标是 `rust/anchor-runner-host/tests/runtime_contract.rs` 与 `native_plugins.rs`：使用小型 Graph、本地脚本 Provider 和 loopback MCP，实际经过生产 Host、GraphRunner、NodeExecutionPort、io-harness、Rig、Bubblewrap 和 Artifact。

fixture 只替换模型传输与远程业务服务，不伪造节点成功、Run 记录或 Artifact，不 mock Runner、持久化和沙箱。AgentNode 仍通过工具完成动作，通过 `final_result` 完成或选择反馈路由。Provider 按模型分别保存响应队列，记录实际请求，拒绝意外请求，并检查响应是否全部消费。live 不使用脚本 Provider，由真实模型自行调用 `anchor_run` 和 `final_result` 完成固定文件任务。

低成本真实模型层与真实业务内容验收仍是不同出口。本地夹具通过不能关闭真实 Provider、OAuth、企业微信公网投递或生产切换的验收项。本文只定义入口、协议和覆盖范围，不宣称真实模型层已通过或全特性已覆盖；实际验收以当次证据为准。

## 统一入口

需要仓库固定依赖支持的 Rust 工具链、Linux、Git、Bubblewrap 和可用的 user/mount/network namespace。环境不足时测试失败，不静默跳过隔离。

仓库根目录执行；默认 `--mode all`，先构建两个原生工具 binary、执行完整 fixture 测试，再执行 serial、feedback、parallel 三个真实模型 Graph：

```sh
./.venv/bin/python scripts/rust_low_cost_regression.py
```

分层运行或选择真实用例：

```sh
./.venv/bin/python scripts/rust_low_cost_regression.py --mode fixture
./.venv/bin/python scripts/rust_low_cost_regression.py --mode live
./.venv/bin/python scripts/rust_low_cost_regression.py --mode live \
  --case serial --case feedback
```

| 参数 | 契约 |
| --- | --- |
| `--mode all` | 默认；fixture 成功后才运行所选 live 用例，默认三个全部运行 |
| `--mode fixture` | 零真实模型调用，不加载 `.env`；不接受 `--case` |
| `--mode live` | 只运行真实模型层，不执行 fixture |
| `--case` | 可重复选择 `serial`、`feedback`、`parallel`；仅作用于 all/live，未指定时全部运行 |
| `--target-dir` | 默认取 `CARGO_TARGET_DIR`，未设置时为 `/tmp/anchor-runtime-contract-target` |
| `--binary` | 指定已 build 的 Host；默认 `<target-dir>/debug/anchor-runner-host`，live 单独运行前需确保 binary 存在 |
| `--timeout` | 默认 180 秒，每个真实 Graph 的等待超时，不是整个套件、token 或 request 的硬限额 |
| `--evidence-root` | 指定新证据根的 parent，默认系统 temp；每次创建新的 `anchor-low-cost-*` 根，不复用旧证据 |

all/live 从环境或仓库 `.env` 获取 `ANCHOR_MODEL_API_KEY`、`ANCHOR_MODEL_URL`、`ANCHOR_MODEL_NAME`。缺少任一 provider 配置时在执行前退出，exit code 为 2，不请求模型，不能算通过，也不会降级成 fixture 成功。live 默认使用 Responses wire，可由 `ANCHOR_MODEL_WIRE_API` 指定当次传输；当次通过不代表所有 provider/wire 组合兼容。

统一入口为 fixture 和每个 live Graph 分配隔离的 bundle、state、workspace 与证据根，复制 Host binary 并记录摘要。它不读取或修改生产数据，不切换生产 backend，不发送消息或发布内容；live 只调用已配置的模型服务，Agent 工具沙箱禁网。

### fixture 直接运行

零模型 fixture 也提供 Rust 原生入口，不需要 Python/venv；它调用同一组 Rust 测试、原生 MCP 构建和原有场景证据，不复制执行语义：

```sh
CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/tmp/anchor-runtime-contract-target \
  cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools -- \
  regression fixture --workspace-root . \
  --target-dir /tmp/anchor-runtime-contract-target --evidence-root /tmp
```

该入口仅接受 `regression fixture`，不静默降级或冒充 live；真实模型入口仍使用上面的脚本。每次新建私有 `anchor-native-regression-*` 证据根，保存实际命令、退出码、完整测试汇总与场景摘要，并核对场景的 Host binary SHA-256。失败、缺失证据、忽略/过滤测试或二进制不一致均不能报告通过。完整 Run/节点历史、workspace 与 Artifact 字节/hash 仍在原 fixture 场景 JSON 中。

保留原始 Rust 测试入口，从仓库根目录执行：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --test runtime_contract -- --test-threads=4 --nocapture
```

磁盘不足或并行开发需要独立构建目录时，给命令设置 `CARGO_TARGET_DIR=/tmp/anchor-runtime-contract-target`。不同编译工作流不得同时覆盖同一个 target 中的 Host binary。

每个用例都有独立临时 bundle、state、workspace 和 Provider。Host 使用清空后的环境，不加载 `.env`、继承模型凭证或读取生产数据。模型与 MCP 地址只指向测试创建的 loopback 服务。测试支持普通 Chat completion 和 Chat SSE 工具调用响应。

直接运行 Cargo 测试时，默认证据目录是 `.local/runtime-contract/<test-process-id>/`。可通过绝对路径 `ANCHOR_TEST_EVIDENCE_ROOT` 指定新的独立目录；统一入口会将它设置为当次新根下的 `fixture/`。不要复用旧目录判断本次运行是否通过。失败时打印并保留临时根，便于检查原生记录和文件。

## fixture 用例与断言

Runtime/Session 与原生 MCP 测试包含下列独立场景；wait/detach 调用分别输出证据，因此证据份数可以多于测试数，实际数量以当次汇总为准。系统 Pilot 不经过隐藏 Graph，复用同一 Host、io-harness 和 Rig 会话边界。

| 场景 | 确定性动作 | 验收事实 |
| --- | --- | --- |
| Op → Agent → Op | 固定文件写入、读取与提交 | 工具注册、模型 alias、真实工具结果、只读输入、Artifact bytes/hash、Git 投影与 native Artifact 身份 |
| 完成协议纠错 | 非法 route 或缺失 summary 后提交正确结果 | 同一 invocation、错误分支不提交、纠错进入模型请求、工具效果不重复 |
| Provider 错误分类 | 一次 HTTP 503 后成功；HTTP 400 终止 | 请求数、重试输入、同 invocation、工具效果一次；terminal 不重试 |
| 反馈返工 | writer/reviewer 各两次，最终收束 | 工作区延续、两版 Artifact 不变、输入谱系、反馈摘要进入下一轮 |
| 独立 Run 隔离 | 两个独立 Host 根执行相同定义 | 无文件、输入或模型上下文串扰 |
| fanout/join | 两个 Agent 分支生成固定文件 | join 谱系、精确合并字节、完成后重开不重放 |
| 暂停与重启 | 在 Provider 检查点请求 pause，提交当前节点后停止 Host | 同一 Run 续行、已提交节点不重放、修改 catalog 不改变冻结 Graph |
| 并行中断 | 一个分支提交，另一个工具已完成但模型响应未返回时杀 Host | 原 invocation 继续、已完成分支不重放、双方工具效果一次 |
| 停止收束 | 在模型请求检查点 stop，再重启 Host | 停止已收束、下游未启动、历史和 workspace 保留、重启不自动重放 |
| Graph call | 最小 Agent 子 Run，分别 wait/detach | 父子身份、输入、wait 结果交接、子工具效果一次、重启后身份稳定 |
| Plugin/MCP | 读取 Skill/资源，调用本地 MCP，提交固定文件 | 只读挂载、实际工具 schema/result、一次持久副作用、Artifact bytes/hash |
| 原生官方 MCP | 独立打包的 Rust WeCom 成员查询和 Docmost 图片上传，经真实 stdio 与沙箱访问 loopback 服务 | binary/资源冻结、原生工具结果进入历史和下一次模型请求、冻结输入不变、HTTP 次数、精确 Artifact；不发送企业微信消息或向真实 Docmost 发布 |
| Plugin 漂移 | 固定资源摘要后修改资源 | 启动前拒绝，无 Run、Provider 或 MCP 调用 |
| 原生会话与媒体 | 同一用户两轮、另一用户一轮，附文件和 1×1 PNG | 提交幂等/冲突、前轮用户消息与完成摘要、用户隔离、跨 Run workspace 隔离、只读附件、图片 wire bytes |
| Scheduler | HTTP 创建 once 计划，ticker 启动小型 Agent Graph；重启读取过期计划 | CRUD、UTC timeline、实际定时 Run、停机错过不补跑、计划关闭 |
| 平台 Session 生命周期 | 实际 HTTP 创建、改名、重启、读取与事件 cursor；不运行 Agent | 独立事务存储、无 Harness store、零 Provider 请求；缺 request_id 的 Turn 提交明确拒绝 |
| 原生 Pilot 会话 | 两轮读取固定 Graph、回复固定文本；重投、SSE cursor 与进程重启 | 同请求身份/冲突、原生历史、只读工具、跨 Turn provider call ID 隔离；重连不调用模型，无隐藏 Graph |
| Pilot 停止与中断 | Provider 检查点 stop、阻止同 Session 并发，再在首个响应中 kill Host | 停止前不派发工具、保留原生 prompt；启动标记中断，重投不重跑，新输入接回历史而不恢复纯聊天 Run |
| Pilot Graph 变更 | 一个 Turn 创建/更新/启动两节点固定 Graph，Op 写种子、Agent 读取并写一次 receipt | 真实宿主 executor 在 Pilot 回复后继续运行；Turn/native/Run 来源关联，精确 Artifact/workspace、Agent 原生历史、重投和重启不重放 |
| 条件 Graph 删除 | HTTP 目标快照、定义变化后旧条件拒绝、真实 Op 产物、Host 重启后条件删除 | 旧条件不删除变化的目标；重启不改变未修改目标的条件；Run/workspace 清理复用原路径，保存删除前 Artifact bytes/hash；不等于 Pilot 删除确认/拒绝 |
| Pilot Run 控制 | 实际 tool 请求暂停/恢复/停止，同一两节点 Graph 在 Provider 检查点切换 | Graph 安全边界、Run ID 不变、前节点 effects 只一次、`run_status`/`session_wait` 核查事实；不把控制请求视为完成 |

会话跨轮模型上下文检查用户消息和完成摘要；每个 invocation 的工具历史单独通过公开 `trace_messages` 读取，不假定所有历史工具输出都重新注入下一轮。完成后的 `.run` 恢复 sidecar 清理不应使原生历史消失；读取使用 io-harness 的公开 Store 接口，不读写私有 SQL。

## live 用例与成本边界

### 原生 Pilot 短会话

`scripts/rust_native_pilot_smoke.py --binary <built-host>` 是额外显式验收入口，不替换原 `scripts/rust_pilot_smoke.py` 的 Python Pilot 兼容验收。它只接受环境中的 provider 配置，缺配置 exit 2，不加载 dotenv 或隐式构建。两轮各只调用一次 `graph_read` 读取随机标记并回复；检查原生 tool input/result、真实第二轮请求中的前轮回复、SSE、幂等、重启和 usage。仅读取一个本地小定义，不运行 Graph、检索论文或发布内容。常规回归无需重复调用它；默认三个 live Graph 的范围不变。

`scripts/rust_native_pilot_mutation_smoke.py --binary <built-host>` 单独验证 F2b1：一个 Turn 严格顺序调用 `graph_create`、`graph_update`、`graph_run` 后回复，预期四个真实 provider 请求。更新后的 Graph 仅一个 Op 写随机 receipt，无 AgentNode、Plugin、网络或外部业务动作；driver 核查三个工具的真实结果、完整冻结快照、一次 invocation、workspace/Artifact 字节及 hash、Turn/native/Session/Run 来源关联、幂等和重启前后请求记录不变。它不重试模型，不把 budget stopped 当成功；缺 provider、额外动作、伪造结果或不完整 usage 均失败。仍须显式配置环境和现有 binary，不隐式加载 dotenv 或构建。原生记录成功状态为 `succeeded`，不同于业务 Turn 的 `completed`。运行时常规回归仍使用确定性 Provider，真实验收成本及失败证据记录于台账。

浏览器零模型验收为 `apps/web/e2e/pilot-native.spec.ts`。设置 `ANCHOR_TEST_NATIVE_PILOT_BINARY` 后，它以独立根启动原生 Host、受控 HTTP Provider 和现有 React build；未设置 binary 时跳过，不能算通过。测试运行时不启动 Python 服务。

Graph 定义位于 `tests/fixtures/runtime_regression/`，任务仅涉及固定文件、工具和本地 Plugin，不做业务研究。所有 Agent 使用 `models.regression` alias、`network=false`、`wall_time_limit_seconds=60`，真实模型须先执行工具并检查结果，再通过 `final_result` 完成或路由，不能用普通文本冒充完成。

| Graph | 固定任务与预期断言 | Agent invocations |
| --- | --- | --- |
| `serial.json` | producer 写 `b'seed\n'`；worker 实际读取 producer、Plugin skill 和 `b'fixture-resource\n'` 资源，校验只读，写 `report.txt=b'seed\nfixture-resource\n'`、`effects.txt=b'once\n'`；route=verify，verify 校验并复制为 `verified.txt` | 1 |
| `feedback.json` | writer/reviewer 各两回；保留各自 workspace，draft 从 `b'draft-v1\n'` 改为 `b'draft-v2\n'`，notes 始终为 `b'retained\n'`；review 从 `b'revise once\n'` 改为 `b'approved\n'`，双方 effects 各追加一次；首次 reviewer summary 包含 `revise once`，路由 writer→reviewer→writer→reviewer→done，done 复制为 `final.txt` | 4 |
| `parallel.json` | seed 写 `b'seed'`；fork 配对 join，left/right 写 `b'seedL'`/`b'seedR'` 及各自 `b'once\n'` effects，route=join；verify 合并为 `b'seedLseedR'`，复制 `join.json` 并核验两个分支谱系 | 2 |

默认三个 Graph 合计 7 次 Agent invocation；invocation 不等于模型请求数。工具回合、纠错和传输重试可能增加请求，不能承诺精确固定的真实 request/token 值或费用。`--timeout` 和 Agent wall time 约束等待时间，不是 token/request 硬限额；实际调用与 usage 必须从当次原生 provider recordings 和证据读取。

### 原生官方 MCP 的短模型验收

修改官方原生 MCP/适配后，可额外运行 `scripts/rust_native_plugins_smoke.py`。它显式接收三份已构建 binary，固定副本与摘要；只接受环境中的模型配置，缺配置 exit 2，不隐式编译或降级。它不属于默认三个 live case，但两个原生 Plugin 的确定性组合已经自动纳入统一 fixture 层。

```sh
./.venv/bin/python scripts/rust_native_plugins_smoke.py \
  --binary /tmp/anchor-runtime-contract-target/debug/anchor-runner-host \
  --wecom-binary /tmp/anchor-runtime-contract-target/debug/anchor-wecom-tools \
  --docmost-binary /tmp/anchor-runtime-contract-target/debug/anchor-docmost-tools
```

一个 Op 生成 `/in/publish/assets/panel.png`，一个 60 秒 Agent 实际读取两个 Skill、核验只读，查询 loopback WeCom 随机成员、上传到 loopback Docmost，再按真实返回的随机姓名/attachment ID 写结果。验收同时检查实际 tool call/result、业务 HTTP 次数、原始 Run input、workspace、Artifact bytes/hash 和 provider usage。Fixture 中只有假业务凭据，复制包的服务配置仅指向 loopback；不连接真实企业微信/Docmost，也不执行发送、生产发布或 Python 工具。Run 等待的 `--timeout` 只接受 0–180 秒范围，仍不是精确 token/request 预算；HTTP 客户端为包含大 debug binary 的资源准入保留 30 秒，原其他验收客户端默认仍为 5 秒，不自动重试 mutation。证据保留在新建的 `/tmp/anchor-native-plugins-*`（或指定 `--evidence-root`）目录。

## 证据

### fixture 证据

每份 JSON 保存测试断言摘要、Run 持久事实、实际 Provider 请求、原生模型/工具历史、Artifact manifest、文件和 workspace 的 bytes/hash，以及小文件文本。另保存实际执行的 Host binary、测试 executable、Graph 和 bundle manifest 的 SHA-256，避免将较早的证据当作修改后源码已通过。

Host executable 为每个临时根固定 inode；后续 target 重建不会替换正在执行的 fixture binary。`elapsed_ms` 是该 Host fixture 从创建到采证时的耗时，不是首次编译时间或业务模型性能基准。Provider 返回的 usage 是夹具值，不代表真实模型 token 使用。

### live 与统一入口证据

统一入口打印当次新根并写入顶层 `evidence.json`；fixture 日志保存在 `fixture.log`，场景证据在 `fixture/`，live 的 serial/feedback/parallel 各有独立子根和 `evidence.json`。每个 live 根保存公开 `GET /runs/{id}` 返回的原生 traces、Run 持久事实、workspace/Artifact 文件字节与 SHA-256、执行 Graph/bundle 摘要，以及 io-harness native provider recordings。不凭终端显示的完成文本或旧 root 判断成功。

`provider_attempts` 按原生录制尝试统计，逐次保存实际 usage；`usage_reported_attempts` 和 `usage_complete` 区分有报告、部分报告和缺失。`reported_tokens`/`reported_total_tokens` 只汇总已报告值，部分 usage 不是完整总量；完全缺失时为 null，不能把缺 usage 写成 0。fixture 的零真实模型调用和夹具 usage 必须与 live 的真实调用统计分开。选择部分 `--case` 通过也不能描述为三个 live Graph 全通过。

## 尚未覆盖

### fixture 未覆盖、由 live 补充的边界

- 真实模型自行选择工具、执行固定文件任务和结构化完成/反馈路由，以及当次配置的 provider/wire 传输；fixture 的脚本响应不能提供这类证据。live 只定义上述三种小型 Graph 的验收出口，是否通过仍需查看实际执行结果。

### 整个套件仍未覆盖的边界

- Responses wire 的完整兼容矩阵、截断/混合增量响应和 WebUI 摘要/SSE 重连；live 只涉及当次配置，不覆盖所有传输变体。
- Python Session/Turn 的完整入口、多用户并发、替换取消、企业微信网关和真实 OAuth 授权。
- 正在执行且效果未知的外部工具、跨存储故障窗口、外部 exactly-once；并行中断用例的工具结果已经持久化。
- Provider 重试耗尽、模型 alias 配置漂移、所有准入负例和未支持的嵌套/并行 Graph call。
- Scheduler 非 UTC/DST、occurrence 消费到 admission 之间退出的窗口。

这些边界应继续增加独立小型 Graph 和外部控制用例，不扩大主图来掩盖未覆盖项。fixture/live 均不替代完整 RSI、深度研究、周报的业务内容评价、真实服务授权/投递/发布或生产切换验收；这些仍需独立、低频且明确授权的出口。

## 维护约定

新增或变更受支持的 Runtime 特性时，同步补充相应的小型 Graph 或故障控制用例，并更新覆盖矩阵。涉及真实模型工具调用、完成或反馈协议时，补充定向 live 证据。尚无覆盖的能力必须明确列为未覆盖，不能用整套回归绿色宣称全产品特性通过。这只是回归工作方式，不增加产品机制。
