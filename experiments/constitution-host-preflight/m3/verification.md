# M3 交回与验证

> 历史现场：本文保留 §8 停止时的 110 / 12 / 0 与原独跑失败。§10.15 已裁定并完成修复，当前 M3 结果为完整固定 Goose 122 / 0 / 0、原 12 项各自独跑通过，见 [§10.15 交回](completion-verification.md)。下文不是当前未关闭待办。

本轮按任务书 §10.13/§10.14 执行，起点 `4d3d19d`，分支 `constitution-core`。当前触发任务书 §8 的恢复业务断言冲突，按停止条件交回，M3 尚未完成；下列通过项只代表已执行范围。所有 Cargo 构建目录为 `/tmp/anchor-goose-regression-target-core`，relay 使用该目录，未与 master 共用产物。本轮没有清理已有开发状态，没有部署或合回 master。

## 交付

- 默认 HTTP、standalone、Graph/Session Call、常驻助手均通过同一 KernelScheduler；旧执行器、旧 Run 存储/生命周期/控制类型与临时兼容桥删除。HTTP execution 选择明确不接受。
- 旧侧快照在删除前单独提交：串行 10,000，并行有失败/无失败各 5,000，模块 5,000，Call 5,000；两个命名空间共 60,000 次旧执行先完成双对照。删除后按原差分契约比对冻结事实，不要求逐字节相同，不放宽 H2 的三条不变量或 S18。
- Kernel Phase 是唯一 Run 生命周期；worker 只有 KernelRuns.active 一张表。注册在派发前可见，未 poll 的任务被丢弃也释放登记；abandon 的首次及幂等成功均等待同一表排空。
- 保留未提交 Host/Goose 历史的前驱删除保护；只开放但尚未开始的槽位不额外保留。认证 Session-call 的未知效果核查沿既有端口保存观察，再经 recover_attempt 提交，普通控制不能绕过。
- 具名不接受旧 RunStore 与旧 Graph record 格式，不实现迁移工具。[本地开发状态](local-development-state.md) 给出新 state/workspace 根的命令与保留范围，本轮未执行这些命令。

## 提交与同步

| 提交 | 内容 |
| --- | --- |
| `0d029eb` | 冻结 30,000 个旧侧 seed，旧执行与快照双对照先通过。 |
| `e4a9c68` | 快照比较格式整理。 |
| `6e91e5c` | 唯一 Kernel 执行权威，删除旧实现，迁移夹具与控制/恢复/会话路径。 |

在 M3 结束同步时，以 `6e91e5c` 为分支父提交合并 master `0aa5197` 一次，merge、不 rebase。实际冲突为 `main.rs` 与 `run_access/reads.rs`；master 的启动清理、Web 与 Pilot 业务修复保留，上游旧夹具接到本分支 Kernel 门面。逐文件依据见[工作记录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#m3-结束同步-master)。merge 提交为 `b61c325`，workspace 最终为 2,153 / 0 / 125（129 targets）。提交说明误写 2,154，实际日志与本文为 2,153。

## 检查

全部命令从仓库根运行，环境共同包含 `CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target-core`、`CARGO_BUILD_JOBS=2`。workspace 测试另设 `RUST_TEST_THREADS=4`。

| 已执行检查 | 结果与原始输出 |
| --- | --- |
| 合并前 `cargo test --workspace --no-fail-fast --manifest-path rust/Cargo.toml` | 129 targets，2,135 passed / 0 failed / 124 ignored；`/tmp/anchor-m3/workspace-before-merge-2.log`。这是最后三项边界修复前的全量，不能代替合并后验收。 |
| 最终前驱保留定向 | 3 / 0；`/tmp/anchor-m3/native-retention-final.log`。 |
| 最终 abandon 定向 | 8 / 0，含真实 retire 与两个排空交错；`/tmp/anchor-m3/abandon-retired.log`。 |
| 最终 Session-call 定向 | 15 / 0，含原生事实写失败不派发与普通控制拒绝；`/tmp/anchor-m3/session-recovery-boundaries.log`。 |
| 合并后 durable-write 定向 | Host 单测 10 / 0、writer integration 2 / 0；`/tmp/anchor-m3/durable-write-merge.log`。 |
| `cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings` | 通过；`/tmp/anchor-m3/clippy-after-merge-2.log`。 |
| Web `npm --prefix apps/web test` | 39 文件 / 333 测试通过；`/tmp/anchor-m3/web-test-final.log`。最终导航夹具复跑也执行全 333 项并通过，`/tmp/anchor-m3/web-app-races.log`。 |
| Web `npm --prefix apps/web run build` | 通过；`/tmp/anchor-m3/web-build-final-3.log`，Vite 保留已有大 chunk 提示。 |

合并后 workspace 129 targets，2,153 / 0 / 125，`/tmp/anchor-m3/workspace-after-merge-2.log`。最终 fmt check、sandbox/Host/工具 bins 构建与全部 Host tests no-run 均通过，日志为 `/tmp/anchor-m3/goose-build-bins.log`、`goose-build-mcp.log`、`goose-tests-no-run.log`。固定 Goose 完整套件已结束：41 targets，110 passed / 12 failed / 0 ignored，退出码 101。12 项逐项独立复跑均再次失败（每项 0 / 1 / 0），原样结果见 [checks.json](checks.json)；全部失败不作豁免，见 [未关闭回归](goose-regressions.md)。

## 失败与修复

原始日志在本机 `/tmp`，未入库；不能把下列失败写成全程通过。固定 Goose 完整首轮的 12 项失败另见 [未关闭回归](goose-regressions.md)，均保留业务断言。

- 合并前首轮 workspace 因生产路径引用仅 cfg(test) 导出的 Host fact helper 编译失败；改用所有者模块后，全量第二轮通过。首轮 `/tmp/anchor-m3/workspace-before-merge.log`。
- 新前驱保留回归首次因测试未创建事实父目录失败（2 通过 / 1 失败）；补夹具目录后 3 / 0。`native-retention.log` 与 `native-retention-retry.log` 均在 `/tmp/anchor-m3/`。
- 合并后首轮 workspace 因 master 新 durable-write 夹具引用已删旧存储类型编译失败；改用真实 Kernel create/Code completion，原清理断言保留。`/tmp/anchor-m3/workspace-after-merge.log`。
- Web 首次构建发现 master 新导航测试还用旧投影词汇；第一次修订又把 phase 放进只含拓扑事实的 state，TypeScript 如实拒绝。修订为唯一 projection 后构建通过。`/tmp/anchor-m3/web-build.log`、`web-build-final.log` 与 `web-build-final-2.log`。
- 严格 clippy 首轮发现两个多余借用与两处测试 get/is_none 写法；等价整理后第二轮通过，未允许 warning。`/tmp/anchor-m3/clippy-after-merge.log` 与 `clippy-after-merge-2.log`。
- 更早的定向迁移失败及对应复跑保留在 `/tmp/anchor-m3/`；上述普通非忽略范围的 Runtime、Host 路由/成本、删除/投递、插件拒绝与恢复用例通过修实现或合法夹具收敛，没有放宽业务断言；固定 Goose 的剩余失败单独列明，不能算入这些通过项。

## 宪法、棘轮与测试去向

- C1 相对 `0859373` 的 `rust/anchor-kernel` diff 为空；不修改生命周期、Store trait 或 GraphValidator 裁决。Validator 唯一相关改动是删除已退出的 branch cursor 初始化字段。
- C2 save **16 → 0**、mutation **36 → 0**，52 项逐项见 [C2 去向](c2-destinations.md)。公开只读视图不写回记录；恢复数组仅为 Kernel history 投影，残余结构收尾留 M4。
- Rust 的 `GraphRunner|RunStatus|FileRunStore|stop_requested|pause_requested` 扫描零命中；三个 principal 改为 `anchor/executor`、`anchor/host`、`anchor/operator`。测量 Probe 的本义保留。
- [八类规则、27 行 Session 所有权及三个条件/四极限](rules-session-audit.md) 按独立判断体列明；不以调用者数量或行数宣称规则收敛。
- [Runtime 原 73 项逐项映射](runtime-test-mapping.md)，原测试名全保留；[Host 39 个消失函数逐项映射](host-test-mapping.md)，包含改名、合并和已退出旧机制，不等于删除 39 项业务能力。业务断言放宽 0，既有测试新增 ignore 0。
- S 候选 1：S27（承接 S4），退役只移动 Session current 指针，未结算 Run 仍可 abandon。10 类断言修改及删除映射登记在[工作记录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#m3-1013唯一-kernel-路径与旧执行器退出)。本轮无新增 Code 提交种类。

与 M2 最终交回 2,139 / 0 / 136 相比，workspace 通过数增加 14、忽略数减少 11；其中删除重复编译的 12 个 ignored，新增 Session-call 恢复 ignored 1。Web 311 → 333，增加 22。计数变化不能作为新增业务能力或恢复验收的替代证明。

## 忽略数解释

M2 workspace 136 ignored、字面属性 124，多出的 12 来自 `goose_kernel_persistent_assistant.rs` 对普通 assistant 测试模块的重复 include。M3 删除这个重复 target，普通 target 的原 12 场景全部保留并默认走 Kernel；独立 Kernel 未知效果恢复场景也保留。本轮另新增一个需要真实固定 Goose 的 Session-call 未知效果核查场景，故最终实际 workspace 125，Host 完整 ignored 选择并执行 122 项，110 通过 / 12 失败 / 0 忽略。

重复的 12 个用例均来自 `goose_persistent_assistant.rs`：

1. `group_members_share_native_history_while_private_and_other_groups_are_isolated`
2. `two_turns_reuse_run_workspace_and_native_session_without_mutating_fs2_snapshots`
3. `new_input_interrupts_gated_execution_suppresses_late_reply_and_retains_uncommitted_files`
4. `concurrent_users_keep_separate_runs_workspaces_and_native_histories`
5. `idle_wait_pause_resume_dispatches_no_model_until_new_input_and_stop_exits`
6. `host_restart_requires_explicit_resume_when_automatic_recovery_is_disabled`
7. `host_restart_automatically_resumes_the_instance_with_its_workspace_and_session`
8. `reply_images_are_bound_to_exact_turn_receipts_and_not_reused_by_text_only_turns`
9. `interrupted_file_is_readonly_in_pending_scope_until_replacement_reply_is_confirmed`
10. `retired_instance_rejects_resume_and_new_instance_seeds_workspace_and_native_session_once`
11. `attachments_are_readonly_and_turn_scoped_while_native_history_keeps_distinct_images`
12. `other_user_tools_do_not_replace_an_awaiting_completion_receipt`

## 固定 Goose

二进制 `/var/tmp/anchor-goose-build/artifacts/goose-acp.first` 已实际核对 SHA256 为 `17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640`。完整串行 ignored 的 122 项已实际执行，110 / 12 / 0；41 targets 的原样结果在 checks.json，7 个 target 失败。Host、Goose 和全部 helper 在完整套件前后 SHA256 一致，随后使用同一批产物逐项独立复跑失败项；最终身份摘要见 [binary-identities.json](binary-identities.json)。这次固定 Goose 验证失败，不用普通测试或定向通过代替。

真实模型 Provider、外部渠道实网和部署本轮未验。W4 未裁定，生产数据迁移已按用户决定取消。M3 交回后由用户决定是否合回 master；M4 只余 GraphRunRecord 残余字段与 CompletionFact 收尾，M5 测量收尾不自动推进。


完整命令实际为：

```sh
CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target-core \
CARGO_BUILD_JOBS=2 \
ANCHOR_GOOSE_BINARY=/var/tmp/anchor-goose-build/artifacts/goose-acp.first \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY=/tmp/anchor-goose-regression-target-core/debug/anchor-net-relay \
ANCHOR_TEST_RUST_TOOLCHAIN=stable-x86_64-unknown-linux-gnu \
ANCHOR_PILOT_RUSTUP=/home/mansteinl/.cargo/bin/rustup \
ANCHOR_PILOT_RUSTUP_HOME=/home/mansteinl/.rustup \
ANCHOR_TEST_EVIDENCE_ROOT=/tmp/anchor-m3/goose-final-20261019 \
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --locked --no-fail-fast -- --ignored --test-threads=1
```

独立复跑直接执行首轮日志记录的同一 Rust test binary，每次只选择一个失败用例，参数为 `<test> --ignored --exact --test-threads=1`，工作目录为 `rust/anchor-runner-host`，环境同上，仅证据根改为 `/tmp/anchor-m3/goose-solo-before-fixes-20261020`。实际完整 argv、每项原样结果、退出码、日志摘要在 checks.json；期间无编译或源码实现变更。

固定 Goose 首轮 12 个失败已全部逐项独立复现，各项均 0 / 1 / 0、退出码 101；整个采证结束后再次核对 Host、Goose 与 helper，全部 SHA256 与首轮开始前一致。

## §8 停止与审查决定

普通 Run 的首次显式 resume 存在两组冲突业务断言：旧 Goose 与渠道场景要求 Agent 载入同一原生 Session 核查后继续；M2 人工恢复场景要求该次调用停驻，直到显式 recovery 决定。两者都禁止重放未知效果。[冲突现场与两个可选契约](recovery-contract-decision.md) 已呈交用户，尚未收到裁定；本轮不改这两组业务断言，不修改相关恢复控制语义。

其余失败包含 spike/身份预检未转终态、人工 recovery 重入锁、配置/MCP 预检错误适配和旧目录夹具遗漏；[回归清单](goose-regressions.md) 列明边界与未实施的最小修复方向。按任务书 §8，本轮停止新增实现，只收齐已启动验收的结果与独立复跑证据。没有新增失败豁免，也没有将其归因于环境以通过门槛。

里程碑：**M3 未完成，触发 §8 交回**。C2 save 0、mutation 0；S 候选 1（S27）；断言修改 10 类，删除测试均有映射。业务断言放宽 0。M4/M5 未开工，不把 constitution-core 合回 master；master 只作为结束同步父提交合入本分支。当前未关闭门槛是恢复契约裁定、上述回归修复及固定 Goose 全绿。
