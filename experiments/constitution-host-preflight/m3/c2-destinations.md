# M3：C2 写入点去向

本表逐项对应 [H1 的 C2 起点](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#h2h3-直接写入棘轮起点)：16 处 `save` 与 36 处 mutation，两个数字分别计数，不相加为生命周期规则数。基线行号保持原样；当前位置以函数名为稳定定位。范围是 `anchor-runner-host`、`anchor-graph-host` 的生产 `GraphRunRecord`，不含测试、不可变身份 metadata、Session/Goose 自有事实。纯构造器和局部变量整体重绑定不计字段 mutation。

当前源码的 C2：save **16 → 0**，mutation **36 → 0**。standalone 与其他准入入口共用纯构造器，16 处旧 `save` 和 36 处旧 mutation 均已退出。此文是写入迁移审计，不是 M3 验收结论。

## 当前事实入口

- `application/run_access/kernel_runs.rs:131` 的 `KernelRuns::create` 是准入入口，调用 `Kernel::create(run_resource(...))`；准入后的相位只由 `Kernel::submit` 接受的提交改变。
- 同文件 `KernelRuns::control` 提交 Human Request；`kernel_runs/recovery.rs` 的 `recover_attempt` 先经执行端口持久化人工说明或核查提示，再提交 Human Request，不回写整份 Graph 记录。
- `anchor-runtime/src/graph/scheduler/execution.rs` 把执行观察映射为 Work、Code Settle 或 Code Request；故障不能由 Host 直接制造第二份 Run phase。
- `run_access/reads.rs:74` 的 `read_run` 可以在临时返回值中填充 `recovery/recovery_submissions`，来源是已接受 Kernel history；该值没有保存路径。它不是恢复事实的第二个持久写入者。
- `run_access/run_observations.rs:48` 只保存 Host 的 `updated` 观察。它不参与生命周期、授权、恢复判定；历史 `touch()` 不再改写 GraphRunRecord。

以下当前 Host 路径省略 `rust/anchor-runner-host/src/`；Graph Host 路径明确写为 `graph-host/`，代表 `rust/anchor-graph-host/src/`。

## 16 处 save

| 编号 | H1 位置 | 当前去向与事实所有者 |
| --- | --- | --- |
| S01 | `application/admission.rs:103` | `application/run_access/admission.rs:83`，`admit → KernelRuns::create`；构造后一次准入，不保存可变 Run 副本。 |
| S02 | `application/assistants.rs:227` | `application/run_access/graph_policy.rs:157`，`admit_assistant_run → KernelRuns::create`；Session current assistant 指针仍由 SessionStore 保存。 |
| S03 | `application/conversations.rs:855` | `application/run_access/conversations.rs:466`，`admit_conversation → KernelRuns::create`；附件与对话身份 metadata 保持各自所有权。 |
| S04 | `application/session_calls.rs:184` | `application/run_access/session_calls.rs:65`，`stop_session_call → KernelRuns::control("stop")`；Session delivery 的 failed 单独写 metadata，不写 Run phase。 |
| S05 | `application/session_calls.rs:211` | 同文件 `yield_session_call:80 → control("stop")`；保留协调等待，不保存 Stopped 字段。 |
| S06 | `application/session_calls.rs:392` | 同文件 `execute_session_call`；读取权威视图并验证 Session 身份，复用 `inspect_interrupted_attempts` 登记原生 pending、持久化核查提示并提交 Human release，然后 `spawn_kernel`；不清空或伪造完成未知工具观察。 |
| S07 | `application/recovery.rs:139` | `application/run_access/kernel_runs/recovery.rs`，`recover_attempt` 的 Abort → Human abandon；Kernel 结算，执行端口保管工具观察。 |
| S08 | `application/recovery.rs:145` | 同一 `recover_attempt`，尚有其他 pending → Human stop；未解决项由 history 派生，不保存 WaitingRecovery 副本。 |
| S09 | `application/recovery.rs:150` | 同一 `recover_attempt`，最后一项观察持久化后 → Human resume；`application/run_access/recovery.rs:5` 根据返回值启动同一 Run。 |
| S10 | `application/control.rs:126` | `application/run_access/control.rs:48` 的 `control_expected`，先提交父 stop，再处理 wait 子闭包。 |
| S11 | `application/control.rs:137` | 删除“子回调推进后重写 Stopped”分支。父 wait/settled 的接受权在 Kernel；`kernel_child_finished` 仅核对持久绑定并唤醒调度器，旧回调不能直接写父相位。 |
| S12 | `application/control.rs:146` | 同一 `control_expected → kernel_control → KernelRuns::control("stop")`，inactive/active 共用提交入口。 |
| S13 | `application/abandon.rs:164` | `application/run_access/abandon.rs:15`，`finish_kernel_abandon → control("abandon")`；放弃意图仍是 Host 自有耐久命令，不是 Run 结算事实。 |
| S14 | `execution.rs:317` | 删除旧 Runner 报错后的整记录失败保存。`spawn_kernel` 只报告未完成驱动；确定失败由 scheduler 的 Code Settle 表达，未知端口错误保留现场，不自动重放。 |
| S15 | `main.rs:663` | `application/run_access/kernel_runs.rs:551` 的 `execute_standalone`：不存在时 `owner.create`，已存在时核对 frozen snapshot/input，再驱动同一 KernelScheduler。 |
| S16 | `graph-host/graph_call.rs:532` | `graph-host/graph_call.rs:477` 的 `persist_child_admission → CallRunAccess::admit`；Host 适配为 `run_access/kernel_calls.rs:78 → KernelRuns::create`。 |

## 36 处 mutation

| 编号 | H1 位置与操作 | 当前去向 |
| --- | --- | --- |
| M01 | `application/admission.rs:70`，`plugin_bindings =` | `run_access/admission.rs:55`，纯 `GraphRunRecord::create_with_plugins` 的准入参数。 |
| M02 | 同文件 `:71`，`plugin_bindings_initialized = true` | 同一纯构造器建立完整 Plugin 基线；Host 无字段 setter。 |
| M03 | 同文件 `:102`，`touch()` | 删除记录 mutation；Kernel 准入及只读 Host `updated` 观察替代。 |
| M04 | `application/assistants.rs:184`，`plugin_bindings =` | `run_access/graph_policy.rs:112`，`create_with_plugins` 的准入参数。 |
| M05 | 同文件 `:185`，`plugin_bindings_initialized = true` | 同一纯构造器，初始化校验归 Runtime 记录契约。 |
| M06 | `application/conversations.rs:812`，`plugin_bindings =` | `run_access/conversations.rs:424`，`create_with_plugins` 的准入参数。 |
| M07 | 同文件 `:813`，`plugin_bindings_initialized = true` | 同一纯构造器；附件身份与授权仍先冻结。 |
| M08 | `application/session_calls.rs:183`，`status = Stopped` | `run_access/session_calls.rs:65`，Human stop 提交。 |
| M09 | 同文件 `:210`，`status = Stopped` | 同文件 `yield_session_call:80`，Human stop 提交。 |
| M10 | 同文件 `:369`，`mem::take(recovery)` | 删除 mem::take；Session continuation 与普通显式 resume 共用 inspect_interrupted_attempts，经既有 recover_attempt 逐项持久化核查提示并提交，history 投影只读，不靠取空集合表示解决。 |
| M11 | 同文件 `:385`，`recovery_submissions.push` | 删除直接 push。认证 Session 协调与普通显式 resume 共用核查提示，由执行端口与 recover_attempt 依次持久化、提交；提示不冒充工具成功，不自动重放。 |
| M12 | 同文件 `:391`，`status = Running` | `execute_session_call` 在 Session 门禁后提交 resume，驱动注册不写 phase。 |
| M13 | `application/recovery.rs:123`，`recovery_submissions.push` | `kernel_runs/recovery.rs:141` 的 Human Request content 保存 `RecoverySubmission`，已接受 history 为权威来源。 |
| M14 | 同文件 `:130`，`recovery.retain` | 同文件 `recovery_facts:11` 从 pending observations 减去已接受 decisions；只读派生。 |
| M15 | 同文件 `:136`，`status = Aborted` | `recover_attempt` 提交 Human abandon；结算不可逆由 Kernel 判定。 |
| M16 | 同文件 `:137`，`error =` | 放弃原因属于 Human Request / Kernel Outcome；Host 不再把原因写入旧 error 字段。 |
| M17 | 同文件 `:138`，`touch()` | accepted submission 的 Host 观察替代，GraphRunRecord 无 mutation。 |
| M18 | 同文件 `:143`，`status = WaitingRecovery` | 多项恢复仍 pending 时提交 stop；pending 从 history 读取。 |
| M19 | 同文件 `:144`，`touch()` | accepted submission 的 Host 观察替代。 |
| M20 | 同文件 `:148`，`status = Running` | 最后一项观察成功持久化后提交 resume。 |
| M21 | 同文件 `:149`，`touch()` | accepted submission 的 Host 观察替代。 |
| M22 | `application/control.rs:124`，父 `status = Stopped` | `run_access/control.rs:48 → KernelRuns::control`，父 stop 提交。 |
| M23 | 同文件 `:125`，`touch()` | accepted submission 的 Host 观察替代。 |
| M24 | 同文件 `:135`，重读父 `status = Stopped` | 删除重复写分支；回调不得覆盖 Kernel 的 wait/settlement。 |
| M25 | 同文件 `:136`，`touch()` | 随重复写分支删除；没有第二次 Graph 记录保存。 |
| M26 | 同文件 `:144`，inactive `status = Stopped` | 共用 Human stop 提交，不区分旧持久状态 setter。 |
| M27 | 同文件 `:145`，`touch()` | accepted submission 的 Host 观察替代。 |
| M28 | `application/abandon.rs:160`，`status = Aborted` | `run_access/abandon.rs:15`，Human abandon 提交。 |
| M29 | 同文件 `:161`，`error =` | Host immutable abandon intent 保留 reason/requested_at，Kernel Outcome 保存结算原因；不写旧 error。 |
| M30 | `execution.rs:313`，`status = Failed` | 删除 Host 异常后的补写。确定失败提交 Code Settle；未知执行错误不改写为已知失败。 |
| M31 | 同文件 `:314`，`error =` | 同上；端口观察/返回错误保留原因，不制造 GraphRunRecord 错误副本。 |
| M32 | `main.rs:615`，`expected.plugin_bindings =` | `main.rs:635`，Plugin 解析后以 `create_with_plugins` 整体构造 expected，保留 snapshot/input/run_id，完整绑定作为准入参数。 |
| M33 | 同文件 `:616`，`plugin_bindings_initialized = true` | 同一纯构造器，standalone 不再单独设置初始化字段。 |
| M34 | `graph-host/graph_call.rs:295`，`run_id =` | `graph-host/graph_call.rs:269`，确定性 `child_run_id(identity)` 直接作为 `create_with_plugins` 参数。 |
| M35 | 同文件 `:296`，`plugin_bindings =` | 同一构造器接收 Library 在 publication lease 内解析的完整绑定。 |
| M36 | 同文件 `:301`，`plugin_bindings_initialized = true` | 同一构造器；缺少 Plugin 基线在派发前具名返回错误，显式 invocation authority 也不能绕过。 |

## 范围与后续

`GraphRunRecord` 格式 14 已移除 `status/cursor/environments`；保留的 parallel branch status 是 GraphValidator 的分支事实，不是 Run 生命周期。`recovery/recovery_submissions/error` 的结构收尾属于 M4，当前不作为可写 Run 权威。旧 `FileRunStore`、`RunStore`、`GraphRunner`、`RunControl` 均没有生产定义，lease 由 `FileRunLeaseProvider` 提供。

上述结论来自源码和原基线逐项核对；Runtime 迁移测试与实际命令结果见 [测试去向](runtime-test-mapping.md)、[删除检查](runtime-removal-checks.json)。本审计没有执行真实模型 Provider，也不替代主流程的 workspace、Goose 与合并后验证。C2 清零是本次源码结论，不能替代这些独立验收要求。
