# R2：对外状态词汇收敛后的读侧测量

2026-10-19。源码提交 `74a4dec`。**结论：需要重想；R2 停止交回，未通过验收。** 同口径判断点 **82 → 90 → 92**，相对 R 前基线增加 12.2%，未达到减少至少 50% 的门槛；不据此推进 H3–H6。Web 改动生产文件状态分支 **81 → 79**，其中明确 Run 分支 **19 → 19**。这些是停止现场的可复跑源码测量，不能冒充已验收产品。

## 停止现场与未关闭项

HTTP/Pilot `pause` 已接为 `stop` 别名，实际取消当前节点。原 `start_and_pause_request` 在节点执行中发出 pause，要求 `record.results.len() == 1`；独立复跑得到 **0**，见 [日志](pause-independent.log)。原暂停在节点边界留下提交；现 stop 取消了尚未完成的执行。随后恢复触发旧 Runtime 的“不自动重放未知结果”保护，子 Run 可能 Failed，不能靠改状态拼写解决。Session 的原拒绝 `pause` 用例也得到 **202 而非 409**，且 stop 会改变 Session 元数据；见 [最终 11 例复跑](api-recheck.log)。

任务书要求现有测试通过、断言修改仅限状态词汇，并明确禁止为了通过而修改控制写入语义或弱化非状态断言。当前无法同时保留这些非状态断言和新的别名行为，因此按 §7 停止，未改它们。审查方需决定：是否允许把原暂停场景保留为内部旧 Runner 回归，并为 HTTP/Pilot 的即时停止另写新验收；或要求对外继续保留等待节点完成的暂停能力（后者会改变本任务“没有 pause”的决定）。本次没有替用户做该选择。

停止前未完成：旧/Kernel Run 三种处境的同一 HTTP 对照测试；Pilot 别名独立测试；effect 等待的独立 Web 控件测试；六 crate 全量回归；真实 Goose/Provider 端到端。已保留已有 Kernel HTTP 三阶段测试与新增 10×2 映射覆盖，不能把它们写成未完成项目已关闭。写入守卫未迁，BudgetStopped 等新投影动作与旧写入拒绝仍需后续审查。默认旧 Runner 未删除。

## 口径与复现

直接复用 [R measure.py](../r/measure.py) 的 Census/span/merge/PATTERNS/BASE；基线 `1d1ba54`，R `54e664e`。I=`if`，M=`=>`，W=直接字段写入；排除注释、字符串、测试，普通布尔式和 `matches!` 不产生点。仍计完整行为包络、共享适配与搬移规则，不按枚举引用数计收益。脚本断言基线四行 13/0/59/10、R 四行 13/0/58/10 与合计 82/90。源码位置、每个命中及归属见 [counts.json](counts.json) 和 [92 点逐项表](decisions.md)。

```bash
PYTHONDONTWRITEBYTECODE=1 python3 experiments/constitution-host-preflight/r2/measure.py 74a4dec --output experiments/constitution-host-preflight/r2/counts.json
node experiments/constitution-host-preflight/r2/web-status-branches.cjs 74a4dec experiments/constitution-host-preflight/r2/web-branches.json
```

输出：[Rust](measure.log)、[Web](web-branches.log)。Web 脚本使用仓库固定 TypeScript AST，需要 `apps/web/node_modules/typescript`；脚本不执行 Host/Provider，不修改历史 R 产物。

| 消费者 | 基线 | R | R2 | 被相位或定律取代的（逐条） | 保留的（逐条及归属） |
| --- | ---: | ---: | ---: | --- | --- |
| run_projection | 13 | 13 | **15**（1/9/5） | ①Ready/Running/孤立 driver 共用 Executing+active；②取消 pausing/stopping 与 pending token 展示；③stop/resume/abandon 按相位派生；④删除旧预算/recovery 专用显示 guard | ①身份有效性、当前助手、被取代 Session、session_call、abandon intent：授权/Session；②Phase/Waiter/Outcome 九臂：外部序列化；③updated 赋值：宿主时间观察。from_view 全部计入，无测试 helper 成本混入 |
| liveness | 0 | 0 | **0** | ①Paused/BudgetStopped/Stopped 同为 Human，统一静止；②worker 仍是必要观察 | `!worker && (Settled 或 WaitingHuman)`：相位+宿主布尔规则；零 I/M/W 不代表零规则。未迁调用者的 observed/with_registry 也在包络内 |
| conversations | 59 | 58 | **58**（33/18/7） | ①结算读 Settled；②busy 读 Executing；③后继/闭包/scope 共用静止读；④旧状态到 Turn 的显示判断由既有 TurnStatus 承接 | ①当前 Turn、scope、Graph 身份、交接链、租约、投递：Session/授权；②原 settle_channel_run 四臂+助手 Completed 特例：旧 Session 写回，原样保留 **5 点**。从临时适配搬回也计入，不能宣称此文件 RunStatus 已清零 |
| kernel_runs 投影及内部 record 桥 | 10 | 10 | **9**（0/7/2） | ①删除原独立 JSON projection；②Kernel 和旧 Run 共同走 from_view | 内部 Phase→GraphRunRecord.status/error 七臂与两次副本赋值：未迁消费者兼容桥，不回写，不对外发旧 status。保留桥成本全部计回 |
| 旧 Run 映射与共享边界（去重） | — | 9 | **10**（1/9/0） | ①只留一个旧状态→Phase 函数；②删除旧静止和旧 JSON/action 适配分支 | ①九臂有损映射：旧 Run 读取兼容；②归属 if：应用读所有权。完整 RunView/action 布尔式计零，仍列在范围里 |
| **合计（去重）** | **82**（38/32/12） | **90**（42/36/12） | **92**（35/43/14） | 旧行为区分确有退出展示，但新的序列化成本也进入同一包络 | 仍有外部序列化、旧映射、内部桥及宿主观察，不能全归到 Session/授权 |

完整包络非空非注释代码 **545 → 726 → 663 行**，R2 相对基线新增胶水 **278 行**。时间观察整文件、run_updated、Scheduler 接线在 JSON separate_costs 单列，未计入原 R 分母的成本仍不加入；project_run 里的新时间赋值已经计入 92。边界成本与包络重叠，不重复相加。

旧 Run 映射不具备真实 Kernel 原因码时仍用 `Reason::Unclassified`，detail 保留，不虚构事实。RunView 新 reason 只承接旧 effect 等待说明，不编码旧状态，不是新的状态替身。Session 写回仍用旧 RunStatus，未迁内部消费者仍需要 run_json；“旧 Run 只剩一个映射函数”的最终目标没有完成。

## Web 分支

对 R→R2 全部改动生产 TS/TSX 文件取同一并集，在基线、R、R2 用同一 AST/词表重测。计状态字符串条件的 if/三元、switch case、对象表键；每个 if/三元计一次，表键逐个计。排除类型联合、CSS、测试、action 文案键、纯属性值；保留完整文件中的其它领域分支并标归属。逐项见 [web-branches.json](web-branches.json)。

| 范围 | 基线/R | R2 |
| --- | ---: | ---: |
| 改动生产文件全部状态分支 | **81**（2 if/24 三元/55 键） | **79**（4 if/22 三元/53 键） |
| 明确 Run 投影 | 19 | 19 |
| Run/node/Turn 共用标签表 | 26 | 24 |
| 渠道/UI、监听、投递、计划、节点显示与历史结果 | 36 | 36 |
| 布尔 comparison/includes（与上表重叠，不相加） | 34 | 40 |
| 未改 PilotTurn 补充盘点（不加合计） | 9 | 9 |

PilotTurn 读 Session Turn；listener、投递、parallel 分支和节点已提交结果仍保留自己的状态。不把它们改成 Run Phase，也不把这些保留事实算成 Run 简化收益。查询不再输出独立 needs_attention/quiescent；Web 用新事实派生展示，最终控制授权仍看服务端 actions。

## S 候选（8 条，全部待批）

| 候选 | 旧行为 → 新行为及影响 |
| --- | --- |
| S19 | 旧 Run status/十多种 phase 和特殊提示 → 三相位、waiter/outcome/reason；旧客户端字段不兼容，Ready 与孤立 Running 同显示等待接续并进入注意项 |
| S20 | 共享 RunView 的 Executing 无 worker 由不可 resume → 可 resume；原 Kernel JSON 已另行允许启动 driver，现规则统一 |
| S21 | 无 worker Paused/BudgetStopped 原不静止 → 与 Stopped 同静止，影响会话交接、准入、退役与清理读取 |
| S22 | stop 原可在部分非 Executing 处境展示且 pending stop 抑制重复按钮 → 仅 Executing 为 true，且不读 pending flag；非结算 Run 增加 abandon 操作 |
| S23 | 原预算 guard/recovery 特例决定 resume 展示 → Human 无 worker 可 resume、Child/Effect 不可；旧写入守卫保留，存在展示与实际拒绝的边界 |
| S24 | pausing/stopping/control_requested 与 stop 优先级不再对外显示；执行中请求未落状态时仍显示 Executing，用户需等待投影刷新 |
| S25 | HTTP/Pilot pause 从等待节点边界 → stop 即时取消；未完成结果、恢复与 Session 拒绝行为均受影响，已触发停止现场 |
| S26 | 原缺观察的 Kernel 时间查询报不可用 → updated:null；旧 Run mtime 精确来源保留，时间不影响控制 |

断言逐项见工作记录 [R2 附录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#r2停止交回2026-10-19)，另有 [Rust 清单](rust-assertions.md)、[Web 清单](web-assertions.md)。没有因暂停别名失败而删测试、加 ignore 或修改非状态产物断言。

## 三个问题

**1. 按标准的结论。** 需要重想。完整包络 82→92，未减少≥50%，其余判断也不全是 Session/授权。收敛词汇确实移除了旧展示区分，但 Session 包络已有 58 点，大于总基线一半 41；九臂序列化和九臂旧映射仍需计数。不能把 Session 从分母摘走，也不能把这个语法比例当成 Phase 本身无用的证明。当前还有明确验收冲突，R2 不支持批准全量迁移。

**2. 外推其它 18 文件。** 按 92/82≈1.122 机械外推，每 100 个同口径点约为 112 点，不能预期减少一半；这些文件尚未按同包络逐一测量，不能拿原 RunStatus 标识符数乘比例当真实点数。最难仍是 application/{control,abandon,recovery,session_calls,dispatch}.rs 及 api/wecom.rs：Session 仲裁、持久 abandon intent、父子释放、授权和投递属于 Host/Session，不由 Phase 删除；Goose 未知效果与已开始未完成的节点需要端口证据，Phase 仅有等待类别，不能替代这些事实。Ready/Running、Paused/Stopped 等旧展示差异已获准有损，当前难点主要是这些执行与 Session 事实，不足以归因于相位缺一个旧状态。原 api.rs 的旧状态字符串表已在本刀范围内退出；其余文件生产路径未迁。

**3. 用户会在意什么。** S25 最实质：停止当前节点与等它完成会改变产物和恢复结果，Session 上原本拒绝的入口也会变成有效取消；应先裁定。S23 的“显示继续但写入拒绝”和 effect 无继续入口影响用户能否把工作接上；S21 可能提前允许交接或退役，需按现场事实复核。S19 的第三方客户端破坏、S24 缺少请求已接收反馈也值得明确告知。S20 与 S26 表达宿主重启和未知时间，语义较直接，但仍需对应 HTTP 验证。
