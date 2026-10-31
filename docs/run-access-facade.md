# Run 访问边界

## 当前实现

`application/run_access` 是 Host 读取与改变 Run 的唯一门面。HTTP、Pilot、Graph、会话、助手与渠道只消费安全视图、谓词或意图操作；平台和独立宿主统一调用 KernelScheduler，不再按存储分派。原执行器、整体记录存储、旧状态枚举与兼容序列化桥已删除。旧 Run 文件只检查存在性并具名拒绝，不读取内容；当前 GraphRunRecord format 为 15；1–14 及旧字段具名拒绝，不原地升级。

业务围栏从 F0 的 224 降为 0，无新增运行时豁免。`RunView` 提供 Phase、worker、Host 更新时间、只读 Graph 进度与 frontier；私有 record 既不 Deref 也无 getter。生命周期仅来自 Kernel，GraphRunRecord 不持有 status/cursor/error/recovery/recovery_submissions/updated。失败原因来自 Kernel 结算，未知效果及人工决定从已接受 history 投影；在线与只读磁盘读取共用同一纯函数。HTTP 保留 state.error 与 state.recovery 展示键，不再把这些投影塞回 Graph 记录。Graph sequence 保留结果/边决定的局部因果顺序，不等同于 Kernel version；[逐字段归属](../experiments/constitution-host-preflight/m4/field-disposition.md)列明全部字段。原 16 save / 36 mutation 的逐项去向见 [C2 审计](../experiments/constitution-host-preflight/m3/c2-destinations.md)。

所有新 Run 默认走 Kernel，`execution` 选择字段被拒绝。frontier 决定当前 invocation；未知效果 attempts/决定从已接受 Kernel history 投影。普通显式 resume 与认证 Session-call 共用 `inspect_interrupted_attempts`：先发现原生 pending，由 `observe_recovery_attempts` 复用 Runtime 的 `observation_stop` 登记 Code + Observation stop，再逐项持久保存“先核查，不盲目重放”的提示，最后 Human release，Agent 在同一原生 Session 中核查后继续。pending 不隐藏普通 resume；后台自动恢复不调用该释放 helper，不自动重放。人工 Completed/Abort API 仍可供已知结果的直接裁定，不制造工具成功或模型完成；显式 abandon 后保留观察而允许 Session 交接。并行分支 Op.call 仍由 Host 共用 `reject_snapshot` 准入拒绝，不改 GraphValidator。

唯一 worker 表属于 KernelRuns：同步登记后才返回触发结果，guard 在退出、abort 或未 poll future 丢弃时释放。abandon 同步结算并等待 worker 排空；stop 先提交停止再取消；pause 通过持久意图与事件在结果后的派发边界消费，无停止标志轮询。逐条规则和 Session 单写入者审计见 [M3 规则表](../experiments/constitution-host-preflight/m3/rules-session-audit.md)。

列表先枚举 Kernel 的 `run/` 标识，再读取各 Run；进度、相位和操作投影共用本次读出的视图。活动查询从上述 worker 表筛选，只核查匹配的活动 Run；会话接替从既有元数据查找候选，再核查后继是否已接纳，不扫描无关 Run 的历史。通用 `project_run` 仍重新观察调用方传入的旧视图，写操作继续独立校验。Kernel Store 的完整历史读取、费用计算与崩溃恢复契约保持原样，没有新增持久索引或状态缓存。

[围栏测试](../rust/anchor-runner-host/tests/run_access_fence.rs) 扫描 `anchor-runner-host/src/**/*.rs`，对 `RunStatus`、`GraphRunRecord`、`RunStore`、`FileRunStore`、`GraphRunner`、`RunCursor` 六种内部类型的精确标识符逐次计数。[业务基线](../rust/anchor-runner-host/run-access-fence.txt) 是逐文件上限：超过上限或清单外业务文件出现引用会失败；下降或删除文件时提示同步降低清单，不自动改写它。

扫描排除 `tests/`、`*tests.rs` 和精确 `#[cfg(test)]` 所保护的项，包括外部测试模块；跳过该项后恢复扫描，不丢弃同文件后续生产代码。注释和字面量不计数，导入、完整路径、别名声明和宏输入中的类型标识符计数。不展开宏或执行名称解析，不追踪别名使用、glob 导入和推断类型；因此围栏不是所有直接字段读写的静态证明，仍须结合规则表核查各入口并封装内部可变状态。

## 运行时范围

永久允许范围固定在测试中，不由业务基线扩展：

| 范围 | 归属理由 |
| --- | --- |
| `application/run_access.rs`、`application/run_access/` | Run 访问、规则、意图操作与兼容序列化的归属边界；没有新增其他业务豁免 |
| `execution.rs` | 共享 Runner 的执行装配与调用 |
| `main.rs` | 宿主、CLI 与 Runtime 的组合根 |
| `node_host.rs`、`node_host/` | NodeExecutionPort 的实现与装配；根文件和同名目录共同形成一个运行时模块，不另豁免业务路径 |
| `run_data.rs` | Runtime 产物及历史数据访问 |

`anchor-graph-host` 整个 crate 属于运行时侧，不在本围栏范围内。其余 Host 文件默认为业务侧，包括当前没有类型标识符、但仍通过推断类型读写 Run 的文件。

## 意图写入与存储边界

没有业务可调用的通用 save、status setter 或可变 record。创建是纯初始值准入，其后的 Run 状态与 Graph 事实均经 Kernel::submit；测试夹具同样通过实际 Kernel 创建及 Code/Human 提交推进，不直接改写持久相位。

| 意图操作 | 权威实现（`application/run_access/`） | 保留的不变量 |
| --- | --- | --- |
| `admit` | `admission.rs` | 原准入校验、冻结 Graph/权限、元数据、保存与启动顺序 |
| `admit_conversation`、`admit_assistant_run` | `conversations.rs`、`graph_policy.rs` | 原会话去重、前驱与助手范围、Graph/Run 租约与执行登记 |
| `stop`、`pause`、`resume` | `control.rs`、`kernel_runs.rs` | 身份/Session 协调与宿主执行约束保留；显式 resume 共用未知效果核查接续；生命周期由 Kernel 控制裁决 |
| `finish_kernel_abandon` | `abandon.rs` | 先保存 abandon intent，再 Human abandon、取消并等待 worker；重启幂等收尾 |
| `record_recovery_decision` | `recovery.rs`、`kernel_runs/recovery.rs` | 保留人工 Completed/Abort；`prepare_resume` 在 worker registry 锁外，保持验证与错误优先序；先持久化端口说明，再 Human 控制；多 pending、幂等、冲突与排空检查在门面内 |
| `inspect_interrupted_attempts` | `recovery.rs` | 普通显式 resume 与认证 Session-call 的唯一核查接续；先发现/登记 native pending，逐项保存提示，再 Human release，继续同一原生 Session；后台不调用 |
| `observe_recovery_attempts` | `kernel_runs/recovery.rs` | native pending 唯一登记；锁内校验当前 Agent InvocationKey、idle、去重与决定冲突；复用 Runtime `observation_stop`，不重写内核生命周期 |
| `stop_session_call`、`yield_session_call`、`execute_session_call`、`settle_session_call` | `session_calls.rs` | Session 仲裁、交付、执行登记与写入先后不变 |
| `release_parent_after_recorded_delivery`、`kernel_child_finished` | `dispatch.rs` | 先持久交付再释放父 Run，保留等待匹配及取消传播 |
| `delete_cascaded_run`、`delete_inactive_run` | `graph_policy.rs` | 各自保留原前置条件；清理 owned data 后再删除 Run/元数据，Graph 级联继续持有 Graph 租约 |

业务 `recover` 只把决策交给 `record_recovery_decision`，不再持有原始 record。Graph/单 Run 删除也通过门面的原有按意图清理协议，不用通用可变访问器替代。

## HTTP 字节基线

[新增 HTTP 回归](../rust/anchor-runner-host/src/api/tests/run_access_json.rs) 固定 Run 身份与时间，逐字节比较 `/runs` 的原始响应，不先解析或重新序列化 JSON。通过真实 Kernel 提交构造十类相位与活动投影；按已批准 S19–S24、S26 改为 phase/waiter/outcome/reason、四动作及宿主更新时间，仍逐字节比较字段顺序，不保留旧 status、terminal、quiescent 或 needs_attention 字段。等待恢复的样本保留同一 frontier invocation 与待处理 attempt。

当前序列化保留字段插入顺序。本次 F1/F2 接口对齐仍须比较原始响应字节；新相位词汇断言调整已登记在工作记录 M1.5。新增基线不替代后续详情、Pilot、活动执行、并发控制和恢复路径的原有断言与专项核查。

master 的 [详情/abandon 字节回归](../rust/anchor-runner-host/src/application/run_access/http.rs) 覆盖真实 Kernel 场景的完整详情与 abandon 原始序列化；上游的 [Pilot 字节回归](../rust/anchor-runner-host/src/application/run_access/tests.rs) 固定 Run 列表/详情及十种状态的 Session retained-record 输出。record 部分与重构前直接序列化的字段顺序逐字节比较，不用解析后的对象等价替代。HTTP 更新时间取 Host 的接受提交观察；字节基线使用本次观察值，独立控制测试验证更新时间与只读列表/详情一致。

## 规则度量：全 Host 读取规则

本节保留 master F1/F2 的规则测量基线，用于本分支门面对齐；旧状态映射和兼容 wire 描述属于该上游基线，本分支当前相位与对外契约以上文及迁移任务书为准。迁移前固定为 `e3a5778`，来源均为当时 `src/` 相对路径与行号。统计独立判断体，不统计 helper 调用、类型引用或状态赋值；既有 `RunStatus::is_terminal` 调用不算重复实现。1→1 表示归属收口，不虚报减少；新增相位 0→1 不改 wire。复合规则与子规则存在包含关系，以下分组不能相加冒充无重叠总量。Runtime 内核与 NodeExecutionPort 的执行规则不属于业务重复实现统计。

| 谓词/规则 | master 原语义与来源（`src/` 相对路径） | 迁移前 → 当前独立实现数 |
| --- | --- | --- |
| `RunView::phase` | Ready/Running→executing；Paused/Stopped/BudgetStopped→waiting human；WaitingCall→waiting child；WaitingRecovery→waiting effect；其余 settled | 0 → 1（新增内部视图映射，不改 wire） |
| `is_settled` | 仅 Completed/Failed/Aborted；Runtime 权威集合与 admission 原枚举补集收为一处，见下文 | 2 → 1 |
| `is_quiescent` | 无执行登记且终态或 Stopped；Paused/BudgetStopped 不算静止；`application/liveness.rs:32` | 1 → 1 |
| `needs_attention` | 既有投影 phase 为 failed/aborted/waiting_resume/waiting_recovery/budget_stopped；活动 stop/pause 的临时 phase 不提示；`application/run_projection.rs:89` | 1 → 1 |
| `can_pause` | active、非终态、非 Session 调用、无待处理控制；`application/run_projection.rs:99` | 1 → 1 |
| `can_stop` | active 时尚未请求 stop；inactive 时复用状态校验；`application/run_projection.rs:100` | 1 → 1 |
| `can_resume`（观察） | inactive、非 Session 调用、预算校验通过，且普通校验通过或有待恢复 attempt；`application/run_projection.rs:83`。身份、助手、放弃意图、后继等额外门槛仍由同一门面投影按原次序补查 | 1 → 1 |
| `validate_inactive_control` | 依次检查 WaitingRecovery 的非 stop、终态、inactive pause；保留原错误及顺序；`application/control.rs:229` | 1 → 1 |
| `validate_resume_budget` | BudgetStopped 必须显式预算策略；`application/control.rs:245` | 1 → 1 |
| `validate_control_identity` | metadata 与冻结 record 的 Graph digest 相同；`application/control.rs:208`、`application/recovery.rs:44` | 2 → 1 |
| `can_abandon` | Completed/Failed 不允许，其余允许；`application/abandon.rs:204`、`:301`；原 Aborted 幂等响应和并发结果分类保留 | 2 → 1 |
| `legacy_status` | API 原十种状态词汇；`api.rs:227`，Pilot 的 serde 状态词汇相同 | 显式映射 1 → 1（生成点移至门面） |

### 重复状态与恢复判断

`T` 表示 Completed/Failed/Aborted。来源扫描覆盖 Host 全部业务生产文件，排除测试与既定 Runtime 范围；原始状态判断位置见[扫描证据](../.local/run-access-facade-20261019/f1-final/before-state-sites.log)。多入口不同短路与错误顺序仍由用例保留，只有共同判断体收敛。

| 已收敛规则 | 原位置 | 迁移前 → 当前独立实现数 |
| --- | --- | --- |
| `has_stopped_execution`：状态为 `T ∪ {Stopped}` 或其补集，不含执行登记 | `application/control.rs:19`、`application/session_calls.rs:103`、`api/wecom.rs:858`、`api/turns.rs:33`、`api/turns.rs:51` | 5 → 1 |
| `can_dispatch`：detached Run 仅 Ready 可派遣/恢复 | `application/dispatch.rs:51`、`application/dispatch.rs:78` | 2 → 1 |
| `waits_for_invocation`：WaitingCall 与 cursor node/invocation 共同匹配 | `application/control.rs:71`、`application/dispatch.rs:172` | 2 → 1 |
| `waits_for_children`：是否等待子 Run，含 stop/abandon 与释放登记后复核 | `application/control.rs:123`、`application/control.rs:132`、`application/abandon.rs:281`、`application/dispatch.rs:234` | 4 → 1 |
| `has_succeeded`/`succeeded`：成功必须 Completed；不等于完整投递许可 | `application/conversations.rs:171`、`application/dispatch.rs:149`、`application/session_calls.rs:308`、`application/session_calls.rs:425`、`api/files.rs:196`、`api/wecom.rs:710` | 6 → 1 |
| `recovery_target`：cursor、attempt、WaitingRecovery，首次校验按原因映射、租约内复核统一冲突 | `application/recovery.rs:104`、`application/recovery.rs:206` | 2 → 1 |
| `recorded_recovery_decision`：同 invocation/attempt 查重 | `application/recovery.rs:56`、`application/recovery.rs:91` | 2 → 1 |
| `has_pending_recovery`：等待恢复且有 pending attempt | `application/control.rs:177`、`application/run_projection.rs:87` | 2 → 1 |
| `occupies_conversation_execution`：active 或 Ready/Running | `application/conversations.rs:708`、`api/wecom.rs:693` | 2 → 1 |
| `channel_turn_status`：普通/助手结算；助手 Completed 不结束 Turn | `application/conversations.rs:123`、`application/conversations.rs:160` | 2 → 1（保留 assistant 参数） |
| `is_abandoned`：已 Aborted；入口/结束/启动复核 | `application/abandon.rs:205`、`application/abandon.rs:291`、`application/abandon.rs:349` | 3 → 1（复用 settled + can_abandon） |
| `can_abandon`：非 Completed/Failed | `application/abandon.rs:221`、`application/abandon.rs:301` | 2 → 1 |
| `validate_control_identity`：元数据与冻结 Graph 一致 | `application/control.rs:208`、`application/recovery.rs:44` | 2 → 1 |

以上 13 条去重规则，独立判断体 **36 → 13**；只统计本表列明的原规则体，不含后述单点规则、相位或 Runtime 权威终态集合。

### 单点规则与复合用例

| 当前权威实现 | 迁移前来源/保留语义 | 迁移前 → 当前独立实现数 |
| --- | --- | --- |
| `is_settled` | Runtime 既有终态权威 + `application/admission.rs:44` 的七状态补集 + `api/schedules.rs:319` 的七状态投影枚举；`projected_run_is_unfinished` 解码已知 wire 状态后委托同一终态权威，未知/控制中词汇保持原 false | 3 → 1 |
| `channel_progress_is_settled` | `api/wecom_progress.rs:373`；T/Stopped/BudgetStopped，missing 为 true；无 run 仍为 false | 1 → 1 |
| `ChannelReplyRun::from_record` 的 unsuccessful 投影 | `api/wecom.rs:692`、`api/wecom.rs:744`；Failed/Aborted/Stopped，两类 reply 共用 | 2 → 1 |
| `can_continue_session_call` | `application/session_calls.rs:308`；Completed 先短路，Failed/Aborted/BudgetStopped 阻止，其余继续 | 1 → 1 |
| `needs_recovery_replay` | `application/recovery.rs:69`；已存同一决定且 Running、未登记才 kick | 1 → 1 |
| `ensure_wait_closure_settled` | `application/conversations.rs:389`；保留登记、wait 子图闭包；handover 不允许 missing child、Graph cascade 允许 | 1 → 1 |
| `successor_reads_predecessor` | `application/conversations.rs:581`；missing successor 保留前驱；终态/已有后继不再读；首 invocation 注入证据 | 1 → 1 |
| `visible_active_nodes` | `application/graphs.rs:154`；仅活动 Running，排除助手 WAIT_INPUT，含活动并行 cursor | 1 → 1 |
| `executable_assistant_binding` | `assistant_plugins.rs:274`；active cursor 与 input commit 顺序查找，缺绑定原错误 | 1 → 1 |
| `latest_assistant_binding` | `assistant_plugins.rs:458`；cursor 优先，结果按 sequence 回退 | 1 → 1 |
| `delete_inactive_run` | `application/deletion.rs:244`；单 Run 所有权、入向 Graph call、Artifact 身份与清理；不强加 Graph cascade 静止条件 | 各 1 → 1 |

控制完整许可仍包含身份、现有助手、abandon intent、Session 协调与前驱约束；`project_run` 统一对外操作投影，写前核验继续使用同一谓词和原用例顺序。终态、静止、成功、已投递是不同事实，不以一个宽泛布尔量替代。

后续不可把相同子条件误当成完整规则相同：Session 投递先检查 active，文件读取先判断 Completed；普通渠道与常驻助手结算对 Completed 的处理不同；恢复前置验证与租约内复核的错误顺序不同；Graph 级联删除与单 Run 删除、等待闭包交接的缺失记录处理也不同。这些差别必须封在按意图命名的谓词/校验中，不改变原短路与错误顺序。

## 验证与剩余边界

本次 §10.11 合并回归已通过：workspace 2,113 / 0 / 123、Web 311 项及构建、严格 clippy/fmt、固定 Goose 完整串行 ignored 120 / 0 / 0；无失败豁免，见[命令、原始输出入口与断言登记](../experiments/constitution-host-preflight/m2/merge-verification.md)。下列结果单独保留为 master 的 F1/F2 上游基线；M2 的最终验证、规则清单和未覆盖边界见 [M2 交回](../experiments/constitution-host-preflight/m2/completion-verification.md)。以下数字仍只属于上游历史基线。

- 最终读取切片：38 个门面定向回归通过；Rust workspace 1,993 通过、0 失败、122 忽略，严格 workspace/all-target clippy、fmt、Web 306 测试及构建通过。原始日志见[最终读取证据](../.local/run-access-facade-20261019/f1-final/README.md)。未新增真实 Provider 或生产业务验收。
- Goose 三项 fixture/断言在 F1 `0116074` 完成后由 `cc99091` 独立修正，三项串行定向回归各 1/0/0；严格场景证据修正另交 `7a23d30`，未放宽校验器或更改既有断言。[理由与验证](runtime-contract-tests.md#f1-后的-g0-fixture-修正2026-10-19)不作为 F1/F2 行为重构的一部分。
- F2 定向恢复/控制各 18 项、计划 22 项与删除 68 项通过，围栏为业务 0、运行时 197；workspace 实际 1,995/0/122，严格 workspace/all-target clippy、fmt、Web 306 及构建通过。该 workspace 命令在证据测试拆分前执行；拆分只增加 1 个默认忽略项，实际场景不变，后续编译、严格 clippy 与实际 Goose 回归均通过。Graph 级联删除归门面，计划投影委托同一终态权威，未知/控制中状态与非字符串字段保留旧行为。
- 标准 Goose 入口 20 套、105/0/0，105 份场景证据全部通过严格校验；入口外补集另行串行执行 15/0/0，合计覆盖 120 项，没有残留失败，不声称一次完整 ignored 命令通过。原参数测试拆分使测试数比 G0 多 1，覆盖不变。最终命令、原始报告及此前中止/证据失败均保留在 [F2 证据](../.local/run-access-facade-20261019/f2/README.md)。未新增真实 Provider、生产数据、外部渠道或部署验收。

M2 §10.12 的规则表以 `490c000` 为比较基线，见 [独立判断点清单](../experiments/constitution-host-preflight/m2/completion-measurement.md)。双路径并存会增加恢复门禁，不能用上游 36→13 的数字代表本轮收敛；M3 已完成默认切换和旧实现删除，Session 单写入者审计见 [当前规则表](../experiments/constitution-host-preflight/m3/rules-session-audit.md)。任务书 §10.15 的恢复契约更正与回归已完成，完整固定 Goose 122 / 0 / 0、原 12 项逐项独跑均通过；S27 已批准并结清 S4，M3 已快进 master；当前 M4 证据见 [交回](../experiments/constitution-host-preflight/m4/verification.md)，M3 历史结果见 [§10.15 交回](../experiments/constitution-host-preflight/m3/completion-verification.md)。
