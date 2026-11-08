# Run 访问边界

## 当前实现

`application/run_access` 是 Host 读取与改变 Run 的唯一门面。HTTP、Pilot、Graph、会话、助手与渠道只消费安全视图、谓词或意图操作；平台与独立宿主共用 `KernelScheduler`，没有第二条执行或生命周期写入路径。本文维护当前边界，不维护迁移前状态映射、规则数量或拆迁步骤；验收状态统一见[验收台账](pilot-development-plan.md)。

- **生命周期唯一。** Run 的 `Phase` 只来自 Kernel；失败原因来自结算，未知效果和人工决定从已接受的 history 投影。`GraphRunRecord` 不保存 status、cursor、error、recovery、recovery_submissions 或 updated。Graph `sequence` 只表达结果与边决定的局部顺序，不等同于 Kernel version。
- **只读视图。** `RunView` 暴露相位、worker、Host 更新时间、只读进度和 frontier；内部 record 私有，无 `Deref`、原始 getter 或可变访问器。frontier 从 GraphValidator 派生，不是另存的执行游标。原生 Session 与工具完成事实仍由执行端口持有，不从 Graph 记录猜测。
- **不保留旧路径。** 当前内核资源中的 `GraphRunRecord` 使用 format 15；1–14、旧字段和 `execution` 选择字段不被接受，不原地升级。旧 `runs/*.json` 只检查存在性并返回具名错误，不读取旧记录内容；离线迁移不由门面自动实施。

## 读取与投影

相位、worker、静止、成功和已投递是不同事实，不用一个宽泛布尔量替代。生命周期动作建议只由同一读取视图派生；身份、冻结 Graph、当前助手、Session 协调、放弃意图与后继接替可以进一步收窄操作，不能扩大 Kernel 许可。投影不是授权，写入必须重新校验现场，保留各用例的短路与错误顺序。

列表先枚举 Kernel 的 `run/` 标识，逐条读取并投影，不同时保留全部 Run 的完整记录；每条记录仍经过完整格式与事实校验。进度、相位和操作投影复用本次读出的视图。活动查询从唯一 worker 表筛选后只核查匹配的 Run；会话接替从既有元数据找候选，再核查后继是否已接纳，不扫描无关 Run 的历史。通用 `project_run` 对传入的旧视图重新读取，不将调用者的快照作为当前事实。

在线与只读磁盘访问共用生命周期和恢复的纯投影。一次读取只解码一次当前记录，恢复与 frontier 共用该记录和同一份派发预览；预览仍在临时副本上执行原有 Validator 计算。恢复投影在现有资源租约内借用历史条目，不复制全部 submission；回调只做纯投影，不重入存储。HTTP 详情复用本次视图及其更新时间，通用旧视图投影仍重新读取现场。Host 的接受提交更新时间是单独的展示观察，不参与生命周期、授权或恢复裁决。读取优化没有新增持久索引或状态缓存，也不改变 Kernel Store 的完整历史、费用计算与崩溃恢复契约。

## 运行时范围

永久允许范围固定在测试中，不由业务基线扩展：

| 范围 | 归属理由 |
| --- | --- |
| `application/run_access.rs`、`application/run_access/` | Run 读取、规则、意图操作与协议投影的归属边界 |
| `execution.rs` | 共享 Runner 的执行装配与调用 |
| `main.rs` | 宿主、CLI 与 Runtime 的组合根 |
| `node_host.rs`、`node_host/` | NodeExecutionPort 的实现与装配；根文件和同名目录共同形成一个运行时模块，不另豁免业务路径 |
| `run_data.rs` | Runtime 产物及历史数据访问 |

`anchor-graph-host` 是独立运行时 crate，不在此 Host 围栏的扫描范围。其余 Host 文件默认为业务侧，即使通过类型推断而没有被扫描到内部标识符，也不能绕过门面。运行时豁免只说明装配与执行归属，不授予业务旁路写入权。

## 意图写入与存储边界

没有业务可调用的通用 save、status setter 或可变 record。创建只接受经准入校验的初始值；其后的生命周期与 Graph 事实均经 `Kernel::submit`。测试夹具也通过实际 Kernel 创建和提交推进，不直接改写持久相位。

| 意图 | 门面内的归属 | 不得破坏的协议 |
| --- | --- | --- |
| 准入与启动 | `admission`、`conversations`、`graph_policy` | 校验并冻结 Graph、输入、权限与身份，保留去重、元数据、租约及启动顺序 |
| 控制与执行登记 | `control`、`kernel_runs` | 控制交 Kernel 裁决；worker 互斥与排空不成为第二份相位 |
| 放弃 | `abandon` | 先保存不可变意图，再申请 Human abandon、取消并等待 worker；同一意图幂等，重启可收尾 |
| 未知效果核查 | `recovery`、`kernel_runs/recovery` | 匹配完整 InvocationKey 与 attempt，执行端口持久记录在前，Kernel 控制提交在后 |
| Session-call 与子 Run | `session_calls`、`kernel_calls`、`dispatch` | 认证协调、持久绑定和投递顺序保留；记录交付后才释放父等待 |
| 删除 | `deletion`、`graph_policy`、`kernel_runs/deletion` | 先预留不可执行的删除范围，清理 owned data，再删 Run 与元数据；单 Run 与 Graph 级联各有前置条件 |

写入宿主独占部署 writer 租约，`KernelRuns` 另持数据根的 writer 租约；第二个 Host writer 不能并行打开同一根。只读 stdio status 不取得部署 writer 租约、不触发写入宿主的清理。Graph、Run、Library 引用租约和 Session 协调仍各守自己的边界，不由读取视图或一个通用锁替代。

唯一 worker 表归 `KernelRuns`：返回触发结果前同步登记，guard 在退出、abort 或未 poll future 被丢弃时释放。登记、控制和删除预留共用互斥边界，删除按 active → deleting 的锁顺序核查，不能在清理期间重新启动 Run。`stop` 先提交再取消；`pause` 以持久意图与事件在结果后的派发边界消费；`abandon` 返回前等待 worker 排空。意图文件是待处理命令，不是另一份 Run 状态。

## 恢复与 Session

普通显式 resume 与认证 Session-call 共用 `inspect_interrupted_attempts`：发现原生 pending，复用 Runtime 的 `observation_stop` 登记 Code + Observation stop，逐项持久保存“先核查，不盲目重放”的提示，再由 Human 控制释放；Agent 在同一原生 Session 中核查后继续。pending 不隐藏普通 resume，也不将内部 attempt 包装成逐项用户审批。

核查目标必须属于当前 Agent invocation，worker 已排空；相同决定幂等，不同决定冲突，已结算相位不可反转。执行准备可能读取 Run 视图，因此须在 worker registry 锁外完成；决定写入时仍在锁内复核身份、目标与删除预留，先持久化端口说明再提交控制。保留人工 Completed/Abort API，但它们不制造工具成功或模型完成，未知效果不自动重放。

空闲助手原地接续也复用 resume 链，只接纳无 worker、单一未绑定输入等待 frontier、无已保存 pending 和 abandon 意图的实例，Session 还需无 Running Turn。这是已观察事实的准入筛选，不证明没有尚未观察的原生事实，也不另授后台释放未知效果或盲目重放的权限。

Session 的 Turn、current assistant、对话链、投递、绑定和授权由 `SessionStore` 或 Host metadata 的各自所有者保存，不另存 Run phase；retire 当前助手也不等于结算 Run。认证 Session host 持有自己的协调权，普通 pause/resume 不能代替。Run Done 不等于外部投递成功；子回调须匹配父等待中的 node、invocation 和 child 身份，pending delivery 未落成结果前不能释放父等待。

交接和删除必须区分当前执行、前驱仍被继承、投递未结束和记录缺失；缺文件不推断完成。显式 abandon 后保留恢复观察，但不再让这些观察阻挡合法 Session 交接。Session 绑定和继承信息不产生另一份 Run 生命周期，旧 Run 记录也不能冒充 Goose 原生恢复游标。

## HTTP 字节基线

HTTP、Pilot 和渠道共用门面投影，不各自定义生命周期。公开相位和操作字段以 [API 契约](rust-frontend-api-contract.md)为准；HTTP `state.error`、`state.recovery` 是只读展示投影，不回写 Graph 记录。

[列表回归](../rust/anchor-runner-host/src/api/tests/run_access_json.rs)、[详情与 abandon 回归](../rust/anchor-runner-host/src/application/run_access/http.rs)及 [Pilot 回归](../rust/anchor-runner-host/src/application/run_access/tests.rs)以真实 Kernel 事实构造场景，固定输出值和序列化字段顺序。当前列表用例解析响应、选择行后重新序列化比较；这些基线不能描述为所有路由未经解析的完整 HTTP 响应字节验证，也不能代替并发、恢复和实际投递验收。

## 围栏能力与限制

[围栏测试](../rust/anchor-runner-host/tests/run_access_fence.rs)扫描 Host `src/**/*.rs`，逐次计数六个精确标识符：`GraphRunRecord`、`RunStore`、`RunCursor`，以及继续防止重新引入的 `RunStatus`、`FileRunStore`、`GraphRunner`。这份检测清单不表示退役类型仍有生产实现。

[业务基线](../rust/anchor-runner-host/run-access-fence.txt)当前没有例外，业务上限为零；新能力应扩展门面，不增加业务豁免或运行时范围。扫描器按逐文件上限判定，超过上限或清单外出现引用即失败；计数下降或文件删除只提示同步降低清单，不自动改写。

扫描规则由共享[词法扫描器](../rust/anchor-runner-host/tests/common/mod.rs)实现：

- 排除 `tests/`、`*tests.rs` 和精确 `#[cfg(test)]` 保护的项及外部测试模块；允许属性内空白，跳过该项后继续扫描同文件生产代码，其他 cfg 表达式仍计数。
- 忽略注释和字符串、字符等字面量；导入、完整路径、raw identifier、别名声明及宏输入中的精确类型标识符计数，不把名称前后缀当命中。
- 不展开宏、不做名称解析，不追踪别名使用、glob 导入或推断类型。因此它不是所有直接字段访问的静态证明，仍须依靠私有封装、按意图用例与业务回归守住语义边界。

## 验证与剩余边界

历史拆迁过程留在[归档任务](archive/runtime-migration-2026-10-20/run-access-facade-task.md)；规则与 Session 所有权核查见 [M3 审计](../experiments/constitution-host-preflight/m3/rules-session-audit.md)，字段取舍与删除验证见 [M4 字段归属](../experiments/constitution-host-preflight/m4/field-disposition.md)和 [M4 交回](../experiments/constitution-host-preflight/m4/verification.md)。这些只代表各自阶段，不把旧函数、状态映射或历史测试数量继续当当前规范。

单 writer、完整历史读取与必要的 Run/metadata 扫描仍有规模成本，没有新增分布式写入、持久索引或跨版本迁移保证。文档与静态围栏不能证明真实 Provider、外部渠道实网、长期负载或目标部署已通过；具体通过范围和未关闭事项只维护在验收台账。
