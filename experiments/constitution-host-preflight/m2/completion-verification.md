# M2 §10.12 交回

M2 完成 Host 准入范围内的 Kernel 路径能力补齐；默认仍用旧 Runner，M3 未开始。并行分支 Op.call 按 §10.12 复用共用产品准入拒绝，不扩展校验器。常驻助手、预算、未知效果人工决定及删除/Call/Probe 全部在现有 Run 门面后接通。Trace 并发读取机制已复现，原子发布修复前同一回归 5/5 失败，恢复修复后通过；原首轮 500 瞬时字节未保存，见 [根因确认](trace-publication.md)。

里程碑：M2。比较基线为 §10.11 合并提交 490c000。前序切片一并验收：FileStore、Host 删除、Probe 与串行 Call；本轮新增 Runtime/Host 补齐，并同步 master cc919d1。

commit 列表（实现与交回）：

- 0859373 feat(kernel): add atomic recoverable FileStore removal（此前批准的 C1 基线）
- da708cb feat(host): delete Kernel Runs through the Run access facade
- 5293e65 feat(host): project Kernel measurement points as disposable JSONL
- 2582528 feat(host): execute serial Graph calls through Kernel-owned Runs
- 25b2990 feat(runtime): handle assistant yields and execution stops in Kernel scheduler
- 7752d7c feat(host): complete Kernel assistant and human recovery operations
- 4f1fc03 docs(runtime): record M2 evidence pending final master validation
- 523dba4 Merge branch master into constitution-core（第二父提交 cc919d1，含 d042ebe、dc88484）

最终证据提交为本报告所在提交；不重复计入上游 F1/F2 历史提交。

## 行为与证据

- 助手：旧 `goose_persistent_assistant` 12 项原样业务断言通过独立 target 指向 Kernel，验证连续输入、打断与 suppressed 回复、native session/workspace 保留、策略和授权。`ANCHOR_ASSISTANT_EXECUTION=kernel_probe` 是 M2 临时显式选择；Kernel 存储存在且无旧 RunStore 文件的断言防止误走旧路径。
- 执行结果：Runtime 新增 6 项回归，覆盖两轮 input/reply/yield、控制产物与崩溃窗口、wait 同 invocation、预算/Interrupted 恢复、人放弃及无效 yield 路由。Yielded 先验证路由再冻结；预算由 Code + Observation stop，保留实际成本及同 invocation 恢复，未实现 W4。
- 未知效果：Host 两项 FileStore 回归覆盖先持久端口事实再 Human 控制、多 pending、失败不释放、重启幂等和冲突/abandon 不重开；原生事实单测证明不制造工具成功或模型完成。新真实 Goose 场景强杀 Host 后先等待人核查，普通 resume 409；分别用 Completed/Abort，确认效果仅发生一次、同 native session 恢复、重复决定幂等与冲突拒绝。
- 产品准入：默认与 Kernel 对并行分支 Op.call 均在任何持久副作用前返回同一 422，见 [准入边界](parallel-call-boundary.md)。没有新增 Graph 校验裁决。
- 既有能力：FileStore 原子删除、Host 引用/Session 删除保护、无波次屏障并行、模块激活、串行 Call 和结算回调由最终 workspace 覆盖；详细既有切片见 [M2 验证](verification.md)。

## 验证

完整命令、每 target 原样结果和输出摘要 SHA256 见 [completion-checks.json](completion-checks.json)。原始日志与 Goose 场景产物位于 `/tmp/anchor-m2/section-10-12/`，不提交原始 `.log`。Goose 使用固定摘要 `17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640`，配置 local network、同次构建 relay、rustup/toolchain 和冻结的 Host binary 摘要；实际经过真实 Goose ACP、Host 与授权工具，仅模型传输为本地确定性 Provider。

最终合并树 workspace 为 2,139 / 0 / 136，严格 workspace/all-targets clippy、fmt、sandbox 构建通过；Web 311 / 0、build 通过（合并前执行，master 同步未改 Web 源码）。固定 Goose 完整串行 ignored 为 133 / 0 / 0，无失败豁免；其中 Kernel 助手 12 / 0 / 0，Kernel 人工恢复 1 / 0 / 0。相对 §10.11 的 2,113 / 0 / 123，普通通过数增加 26，忽略数增加 13；其中 master 原子 writer 4 项、本轮 Runtime 6 项/Host 4 项、两个新 target 各重复执行共用 helper 6 项；重复 helper 不算新增业务场景。

123 份标准场景报告通过既有严格校验，8 份接口/补集证据独立登记，无校验失败。真实模型请求 0；两个助手 target 复用相同场景名，唯一性按实际测试进程核查，未删除或覆盖报告。证据路径与摘要见 completion-checks.json。

完整 Goose 各实际选中 target 的原样汇总（0 选中 target 仍完整保留在 JSON）：

| Target | 原样结果 |
| --- | --- |
| unittests src/main.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 1016 filtered out; finished in 31.88s |
| tests/goose_acp.rs | test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out; finished in 177.04s |
| tests/goose_channel.rs | test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 50.87s |
| tests/goose_compaction.rs | test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 54.30s |
| tests/goose_completion.rs | test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 107.30s |
| tests/goose_conversation.rs | test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 171.13s |
| tests/goose_development_environment.rs | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 24.57s |
| tests/goose_distribution.rs | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 90.82s |
| tests/goose_elicitation.rs | test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 110.38s |
| tests/goose_kernel_persistent_assistant.rs | test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 191.71s |
| tests/goose_kernel_recovery.rs | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 27.33s |
| tests/goose_library.rs | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 12.59s |
| tests/goose_media.rs | test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 81.62s |
| tests/goose_model_settings.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 24.64s |
| tests/goose_native_activation_spike.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 2.64s |
| tests/goose_persistent_assistant.rs | test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 188.65s |
| tests/goose_pilot.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 23.93s |
| tests/goose_pilot_attachments.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 31.46s |
| tests/goose_pilot_authoring.rs | test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 52.69s |
| tests/goose_pilot_compaction.rs | test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 41.62s |
| tests/goose_plugin_configuration.rs | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 60.08s |
| tests/goose_plugin_disclosure.rs | test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 13.06s |
| tests/goose_plugin_mcp.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 45.58s |
| tests/goose_plugin_summary.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 26.70s |
| tests/goose_plugin_turns.rs | test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 108.87s |
| tests/goose_provider_errors.rs | test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 58.66s |
| tests/goose_responses.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 26.95s |
| tests/goose_session_calls.rs | test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 67.96s |
| tests/goose_trace.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 25.11s |
| tests/native_plugins.rs | test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 61.37s |

## 对照与测量

生产 scheduler 对照旧 Runner：串行 10,000 seeds；并行失败 5,000、原无失败 5,000；并行模块 5,000；串行 Call 5,000。最终 Graph 事实与派发多重集通过；失败组继续用既有 S18 三条不变量，差异 0。Host FileStore 串行 1,000 及原 validator 同组对照同时保留。没有把已有样本数当作新增覆盖。模块随机组不注入节点失败，模块上限和迟到失败由定向覆盖；并行 Call 不属于产品准入组合。已有逐 seed 输出的统计仍是串行失败 772 / 激活模块 1,310，并行失败 775，模块激活 4,588，Call waited 957 / detached 1,140 / failed 514 / unknown 392；来源 `/tmp/anchor-m2/scheduler-tests.log` 与 `kernel-call-differential.log`。最终 workspace 重新执行相同对照，不把这些旧统计称为新增样本。

Probe 的真实 Host/Kernel fixture 样本与按校验器、身份、动作、裁决、原因码汇总见 [probe-counts.json](probe-counts.json)、[probe-sample.jsonl](probe-sample.jsonl)：12 points、accepted 7 / returned 2 / refused 3、成本 4。Probe 可重建、故障不阻断控制；不宣称真实模型调用。规则清单以 `490c000` 为基线，逐位置与计数见 [独立判断点](completion-measurement.md)、[机器数据](completion-counts.json)。双路径并存的恢复门禁增加如实登记，不冒称收敛已兑现。

## 宪法与交回边界

“完美”的三个条件：新增提交映射、恢复门面属于提议/裁决分离与未知即未知；Goose 观察、HTTP 投影和测试开关属于可整块替换的边缘。核心无需改动，C1 相对 `0859373` 零 diff。没有发现可在本轮删除的 Kernel 职责；旧 Runner 是已设 M3 退役条件的对照，不属于核心。

四个极限：智能增长不改变宿主盖章和人工目的；规模增长仍按资源单写入者与纯 Graph 裁决隔离；时间增长只追加 Kernel history，人工观察不改写未知工具结果，测量投影可重建；有对手时身份、成本、路由和已结算/等待中的迟到结果仍由原边界裁决。新适配未把 Goose、Graph、Session 或恢复业务放进 Kernel。恢复历史扫描随单 Run 历史增长，未做无限规模性能承诺。

S 候选 0；既有业务断言放宽 0、删除测试 0、旧测试新增 ignore 0，均在 [工作记录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#m2-1012门面后能力补齐与交回) 登记。旧助手断言原样 include，新 fixture 的 cursor/prepare passes 仅为测试兼容投影，绝不写回持久事实。C2 棘轮和删除行数属于 M3 之后，本轮不报虚假清零。

停止条件未触发。并行 Call 的先前疑点已由 §10.12 裁定，内核未再修改。收尾发现 master 已推进到 cc919d1（原子发布与网关确定性时序修复）；已通过合并提交 523dba4 同步该提交，不 rebase。两处冲突已逐文件登记在工作记录：验收文档保留双方结论，trace writer 使用 master 共享原子发布，双方回归均保留。

需要审查方决定：是否继续 M3。默认切换与删除旧 Runner、§10.5 Session 单写入者检验、生产数据离线迁移、W4、真实模型 Provider、外部渠道与部署未实施或未新增验收。固定 Goose + 本地 Provider 的通过不等于真实模型业务验收。
