# Rust/Rig Runtime 重构计划

本文是 `codex/rust-rig-runtime` 实验分支的开发计划，描述如何把 Anchor 逐步收敛到一个由平台宿主和独立 Graph 宿主共同使用的 Rust Runtime Kernel。它不是当前生产 Python 实现的完成声明，也不替代 [开发台账](pilot-development-plan.md)；每个阶段只有在取得对应证据后，才更新台账中的验收状态。

## 目标和不做的事

最终目标是让下面两种交付形态调用同一个 Runner 和 Runtime Kernel：

```text
完整 Anchor 平台 = API/WebUI/Session/Scheduler/Channel + Shared Rust Runner/Kernel
独立执行包     = Graph + 明确 Plugin 资源 + Shared Rust Runner/Kernel
```

迁移过程中保留现有 Graph、Run、Session/Turn、Plugin、Sandbox 和恢复事实，不建立第二套 Graph 语义。Python 宿主可以暂时作为适配层，但不能复制一套长期独立的 Rust/Python 执行规则。

本计划不包含：一次性重写 WebUI；把平台 API 塞进 Kernel；绕过 Sandbox 执行宿主命令；自造记忆、日志或调度系统；为了迁移而改变 fanout/join 的并发语义；在真实 provider 和恢复验收前移除 PydanticAI/Harness。

## 不变量

1. **唯一执行语义**：Node Runtime、Graph Runner 和恢复规则只有一个权威实现；平台与独立宿主只提供适配器。
2. **端口隔离**：模型、工具、Sandbox、事件、持久化和时钟通过小端口进入 Kernel；密钥、live client、宿主权限和平台连接不能进入 checkpoint。
3. **边界持久化**：模型请求前、模型响应后、工具批次结算后和终态都可以保存；恢复按已保存事实继续，不凭猜测重放未知副作用。
4. **局部并行**：Graph Run 仍然是单一协调单元；只有配对的 `fanout → branches → join` 区域允许并行，其他节点按既有串行规则推进。
5. **分层验收**：接口存在、provider-free 测试、Anchor 接入、真实 provider、真实 Sandbox、平台和独立分发分别记录，不能互相代替。
6. **可回退迁移**：每个垂直切片都能继续由 Python 路径运行；切换由宿主适配器控制，不修改历史事实格式而伪造迁移完成。

## 阶段和出口条件

| 阶段 | 交付 | 出口条件 |
| --- | --- | --- |
| R0 事实与契约 | 本计划、架构边界、运行身份和错误/恢复术语 | 契约能映射现有 Node/Run/Sandbox；未决权限和持久化变化单独记录 |
| R1 AgentNode Kernel | `NodeRequest`、`NodeOutcome`、`CompletionPort`、`ToolPort`、Rig `AgentRun` checkpoint、结构化 route | provider-free 工具循环、取消、非法 route、模型/工具边界恢复通过；真实单节点 smoke 通过 |
| R2 持久化边界 | `CheckpointStore` port、原子文件适配器、版本与身份校验、边界保存约定 | 进行中：端口、原子文件保存、pending model/tool 失败后续行、缺失/删除和路径拒绝已通过；未知外部副作用、并发写入语义和宿主集成仍待做 |
| R3 Provider 与流式 | provider 配置适配、stream 事件、超时、取消传播、请求/响应观测 | 进行中：OpenAI-compatible chat/responses、模型/工具超时、pending 状态保留、provider-free 流事件/取消、真实 provider 流式 smoke、可组合请求观测，以及中断后 checkpoint 重载并换 provider 续行的确定性测试已通过；真实进程/provider 断线中断仍待做 |
| R4 Sandbox Adapter | Rust Sandbox port；Bubblewrap、只读输入、网络权限、命令超时和取消适配 | 部分完成：独立 Bubblewrap host adapter 已实现；本机真实 bwrap 只读挂载 smoke 和 fake-helper 策略/超时/取消测试通过。真实网络隔离、host 路径 TOCTOU、真实 bwrap 超时/取消和 Python/平台接线未验收 |
| R5 串行 Graph Runner | 展开后 Graph 快照、Run 状态、串行路由、不可变提交输入、恢复与停止 | 进行中：首个 Kernel Runner vertical slice、十种 provider-free Python `run()` 场景（含明确失败语义差异、未启动 SCC 闭包及嵌套模块回边重入）、format 1→2 Run 迁移、本机跨进程 lease 退出释放、结果顺序/cursor 输入事实校验和四个 test-port 跨进程 Runner 故障窗口均通过；生产 host/provider 崩溃恢复、更广语义覆盖仍待做。fanout/join admission 拒绝；平台与 CLI 接线留到 R8 |
| R6 fanout/join | 复用现有配对契约；分支活动身份、乱序收束、失败/停止/崩溃恢复 | 现有 A31 代表性 Graph 在 Rust Runner 上通过；不引入嵌套或隐式并行 |
| R7 Op.call 与 Plugin/MCP | wait/detach Graph call 通过既有 admission 语义复用同一 Runner；Plugin manifest、MCP stdio/HTTP、凭证与 Sandbox 绑定 | Graph call 不产生第二种调度语义；Plugin 闭包可独立启动；工具恢复、权限拒绝和资源变更检查通过 |
| R8 宿主适配与独立包 | Python 平台调用 Rust Runner；`anchor-graph` 使用同一 Runner；最小 bundle/兼容清单 | 同一 Graph 在平台和独立宿主产生一致 Run/提交/恢复事实；bundle 不含密钥、不扩大授权 |
| R9 迁移与收缩 | RSI、周报、企业微信助手逐个切换；旧 Python 执行路径只保留兼容用途 | 每个 Graph 通过真实 provider、Sandbox、恢复和平台验收；达到条件后再移除 Python Kernel 依赖 |

## 当前进度和下一步

R0 已由现有架构约束和本计划冻结；R1 已完成实验纵向切片，代码位于 `rust/anchor-runtime`，真实 provider smoke 已通过。R2 的 checkpoint 端口、文件适配器和 pending model/tool 失败续行已通过；R3 已加入可选模型/工具超时、独立 `StreamingCompletionPort` 和 `ObservedCompletionPort`。`execute_with_store_and_policy` 让宿主在同一次执行中组合持久化和超时策略，旧的 `execute_with_store` 委托给默认策略入口。确定性测试覆盖 unary timeout、流式取消后序列化/重载 pending model step 并用新 provider 续行，且终态会清除 pending step；这不等价于真实进程退出或 provider 断线恢复。真实 provider 流式 smoke 已通过。R4 已新增独立 `rust/anchor-sandbox-bwrap` host adapter；命令/网络授权与 host path roots 由 adapter policy 所有，Kernel 保持无子进程能力。本机真实 Bubblewrap 已通过只读输入挂载和 `workspace_readonly` 负向 smoke；fake helper 覆盖输出配额、设置失败、超时和取消。网络隔离、真实 bwrap 超时/取消、TOCTOU 和 Python/平台接线仍待验收。R5 首个串行 Graph Runner slice 已在 `rust/anchor-runtime/src/graph.rs` 落地：快照与能力 admission、持久 Run 状态/cursor、RunStore 原子文件+OS advisory lease、节点/产物 port、按边路由与回边、ceiling/module activation、budget/pause/stop/failure、固定 commit 输入、完成事实 crash-window reconciliation。provider-free oracle 当前覆盖十种 Python `run()` 场景，包含未启动循环 SCC 安全闭包与二层嵌套模块经根图回边重入；闭包仅在未运行组件的外部 ingress 已否决/来源已证明 inactive 时写 false 边，selected/未决 ingress 与历史已运行组件保持保守。RunStore 还校验每节点结果 sequence 顺序及 cursor 输入 commit 与 selected 入边精确一致。Run format 1→2 迁移与本机跨进程 lease 退出释放有定向证据；workspace 57 项 runtime、8 项 Bubblewrap 测试，Clippy/fmt/diff 通过。仍是 fake Node/Artifact ports，oracle 覆盖有限；完整 Runner 崩溃恢复、真实宿主 adapter 或 R8 接线仍未完成，因此 R5 继续进行，不宣称 Rust Runtime 替代 Python。

## 每阶段记录

每个阶段必须同时更新：

- `docs/pilot-development-plan.md` 的验收矩阵和推进记录；
- 本计划的阶段状态与证据位置；
- Rust 单元/集成测试以及 `cargo fmt`、Clippy、`git diff --check`；
- 真实 provider、Sandbox、平台或独立宿主证据（如果该阶段要求）。

阶段完成只表示该阶段出口条件满足，不表示整个 Anchor 已经完成 Rust 重构。

### R4 Sandbox 契约归属

Rust 请求表达执行意图和可验证的边界字段；真实授权与主机资源始终由宿主适配器持有。`workspace_readonly`、`tool_dirs`、环境变量、输出预览上限和 spill 配额现已进入请求契约。`SandboxResult` 另区分宿主 spill 文件、沙箱可见路径和无法完整保留的 `incomplete` 输出。环境变量值及宿主 spill 目录在 Debug 输出中脱敏。

| 数据/能力 | 所有者和适配要求 |
| --- | --- |
| argv、工作区只读相对路径、网络意图、超时、输出预览/保留配额 | Kernel/节点调用方提出；适配器必须在结果中如实反映执行、超时、取消和输出完整性。`network=enabled` 只是请求，不是授权 |
| 实际 workspace、只读输入源、tool_dirs、spill host directory、env 值、取消句柄 | 宿主拥有并绑定；Graph/模型不得自行挑选宿主绝对路径、注入任意凭证或扩大授权。host spill 目录必须由宿主预创建在受控 Run 存储内；adapter 校验前不创建请求路径 |
| 路径安全、符号链接与挂载、网络 namespace、子进程终止、输出与磁盘限额 | 真实 adapter 的强制职责。独立 bwrap adapter canonicalize host paths 并校验 host policy roots，拒绝 `workspace_readonly` symlink 和保留挂载目标覆盖；当前不宣称可抵御并发宿主文件系统改写（TOCTOU） |
| `spilled_host_paths`、`visible_spill_paths`、`incomplete` | 适配器产生的执行事实；host path 仅供宿主后续读取/清理，不传给模型或普通日志；模型可见的只能是 sandbox 内路径。丢失超出配额的数据时必须设置 `incomplete=true` |

Bubblewrap adapter 位于独立 host crate `rust/anchor-sandbox-bwrap`，不把进程执行权加入 Kernel。`BubblewrapPolicy` 显式配置命令 allowlist、workspace roots，以及使用到的只读输入/tool/spill roots 和 sandbox destination roots；spill 目录必须先由宿主创建，adapter 只 canonicalize 和授权校验，不创建请求路径；网络另需 host policy 开关。环境值通过清理后的子进程环境传递，不放进 bwrap argv。双流并发读取并按 preview 与共享 spill quota 限制；reader 或 spill 落盘不完整会设置 `incomplete`。Bubblewrap `--info-fd` 启动握手未成功时返回错误，不把 setup 失败记作 `Completed`；超时/取消会 kill 并 wait。

本机真实 bwrap smoke 已验证只读输入绑定和 `workspace_readonly` 写保护；fake-helper 覆盖 host policy 拒绝、路径 symlink、输出界限、setup 失败、超时与取消。这不等价于网络 namespace 负向证明，也未覆盖真实 bwrap 的超时/取消。A35/R4 尚未完成；不得接入 Python Sandbox/平台或宣称替代现有运行时。不能实现的字段必须拒绝请求，不能静默降级。

### R5 串行 Graph Runner 契约（冻结）

- Rust Runner 输入是与 Python `graph.json` 同形的**展开后快照**。Runner 不重新解释 YAML/模块引用，也不在恢复时读取 Graph 工作区的当前版本；模块展开与独立 Graph bundle 编译另属宿主/打包工作。
- Runner 拥有 Graph Run 状态：稳定 Run 身份、快照摘要、状态、cursor、节点 pass/invocation、边决定、提交历史和 stop/pause 原因。节点 `AgentCheckpoint` 仍只保存一次 AgentNode 的 Rig 协议状态，二者不能互相替代。
- 每次调用节点执行 port 前先持久化 cursor；节点工作通过节点执行 port，产物通过 artifact/workspace port 冻结为不可变 commit。下游输入必须引用上游精确 commit，不能挂载会继续变化的工作目录。
- 恢复核对 Run 状态与节点完成事实。完成事实已经持久化但 Run 尚未提交时补记结果而不重跑；事实缺失/冲突或外部副作用结果不明时 fail closed，不猜测重放。commit、节点事实与 Run 状态的多存储窗口必须有明确错误结果。
- 串行调度复用 Python 的入口、全部入边决定/至少一条新选中边、多出口唯一 route、回边、回合 ceiling 和作用域计数语义。多出口缺 route 属节点契约错误；`max_steps` 是累计 provider-request 预算，不可未经证据直接映射成 Rig `max_turns`。
- R5 明确只承接串行协调；遇到 fanout/join 在 admission 时拒绝，R6 才启用配对区域。Runner 不静默忽略插件或 `Op.call`：节点工作由声明能力的执行 port 承接，暂未提供的 Node kind/capability 必须在执行前拒绝。`Op.call` 的 wait/detach 语义在 R7 由同一 Runner 与专用 call port 实现，不建立第二个调度器。
- pause 只在节点边界停下；stop 取消当前节点并保留 cursor/checkpoint 事实。R5 做 Kernel 与 Python oracle 的 provider-free 对照，不接 Python 平台或独立 CLI；宿主接线归 R8。

R5 首片已覆盖展开快照 admission、入口/路由/未选分支、循环回边与回合上限、module scope 重入、不可变 commit 输入、预算停止续行、完成/失败事实的 RunStore crash window 对账、Uncertain fail-closed 和 pause/stop 边界；另以 Python `graph.to_dict()` 生成的 `one-search.snapshot.json` 验证真实序列化形状能被 Rust admission 读取。provider-free Python oracle 直接调用 Python `run()`，覆盖十种场景并逐项比较可映射的状态、输入、执行顺序、passes、ceased、cursor、skip 与边决定。失败场景明确固定一项有意差异：Python 会在单出口失败后记录 selected edge；Rust 保留已启动计数、清除确定失败 cursor、不结算边或运行下游。`Uncertain` 保留 cursor。pause-before-dispatch确认没有创建pass、边或cursor。diamond convergence 要求未选源节点传播 false 边，使所有入边可决后仍可执行有选中输入的 merge；Rust 还覆盖多级跳过、未运行循环防过度传播、较新 false 输入撤销旧 selected 输出、模块 activation ceiling 的本轮边事实。GraphRunRecord format 2 记录在 Runner 持 lease 后迁移旧 format 1，旧 cursor 的启动计数只补记一次。跨进程子进程测试覆盖 FileRunStore lease 在进程退出后释放，以及 cursor-only 后、durable Completed fact 后、durable Failed fact 后、durable Artifact commit 后四种 Runner 崩溃点；父进程以新 Runner 与 test ports 恢复并核对是否重放。该证据限于 test ports 和本机进程，仍不是生产 host/provider 或平台端到端恢复验收。FileRunStore load/save均执行跨字段完整性校验，拒绝身份、边引用及序号损坏；旧 format 1先按明确迁移规则转换再校验。阶段出口仍要求覆盖更多图/失败组合、精确 Run 状态对照和必要的持久化故障路径；平台/CLI 接线留 R8。不能证明等价的语义先返回明确不支持/不确定状态，不能以默认值改写行为。
