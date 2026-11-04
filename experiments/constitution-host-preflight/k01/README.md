# K0.1：限定内核修订与中断校验验收

日期：2026-10-16。源码起点：`0b71475`；授权依据：[审查决定 D1–D5](../../../docs/archive/design-records-2026-11-04/constitution-preflight-decision.md)。主 Agent 集成预算/生命周期，两个 subagent 分别实现存储与中断校验，主 Agent 审查并验证。K0.1 单独提交审查，H1–H6 尚未开始。

## 九项检查

| 项 | 实现与证据 |
| --- | --- |
| 1. 原三个反例正式化，先失败后通过 | 新增 `anchor-kernel/tests/accounting_lifecycle.rs` 与 `storage_regressions.rs`。将最终测试复制到隔离的 `0b71475` 内核源码，实际得到 19 失败、1 通过、Cargo 退出 101；[原始失败输出](kernel-before.log)明确包含预算反例、Code 观察 stop 和残缺历史后的停止恢复。修复后的测试与实现同一提交 |
| 2. D1：打回计费、过期重提不重复计费 | `returned_work_exhausts_budget_on_the_second_submission_without_cost_versions`：成本 6、6、6，预算 10；第二次即结算，第三次拒绝且仍计费，累计 18。`concurrent_branches_charge_stale_work_once_and_retry_at_zero_cost` 用同版本的两分支提交复现并发的串行化顺序：接受成本 3、过期成本 4、重提成本 0，最终累计 7、版本 2。成本本身不推进版本 |
| 3. D2：尾截断、恢复、损坏拒绝、字节保留 | `torn_tail_is_removed_before_append_and_all_complete_records_remain_readable`、`accepted_stop_after_torn_tail_recovers_on_restart_when_state_write_is_lost`、`malformed_complete_records_are_corrupt_in_middle_and_at_end`、`existing_complete_record_bytes_are_preserved_including_json_whitespace`。另测无换行的完整 JSON 不构成记录。模拟仅在 tempfile 内还原旧状态文件，无真实断电 |
| 4. D3：Code 必须有观察、不能释放人的等待、Agent 不能 stop | `host_observation_can_stop_but_only_a_human_can_release_the_wait` 检查缺证据/只有 Resource 证据均拒绝、有 Observation 接受、保留原因、Code resume 拒绝。`agent_cannot_stop_even_with_observation_evidence` 与 `observed_unknown_effect_parks_the_task_without_a_human_impersonation` 检查身份及原始宿主观察反例 |
| 5. D4：中断仅 Code，可持久投影并沿路由继续 | Runtime 新增 6 项 `graph::validator::interruption_tests`；[修复前黑盒测试输出](validator-before.log)为 4 失败、1 通过。Code 结果写入 `RunResult.interruption`，Agent/Human 携带字段（含显式 null）均拒绝；保留规范 control completion 与既有并行边界；普通 Agent 序列化省略 None |
| 6. D5：任何身份等待写入拒绝、计费、不影响调用 | `waiting_rejects_non_lifecycle_work_for_every_identity_and_charges_it` 覆盖 Agent/Code/Human、Write/Execute/Settle/非生命周期 Request；另测过期、无授权与未知控制词。迟到 Code 正式反例 `stopped_task_refuses_a_late_code_result_without_advancing_state`。Op.call 的 5,000 例原差分保持不变 |
| 7. 三组差分 20,000 例、断言不放宽 | 串行 10,000、并行 5,000、调用 5,000；最终 all-targets 内执行。差分源码唯一变化是初始化器补 `interruption: None`；逐字节还原该新增行后与起点完全一致，见[结构核对](structure.json) |
| 8. 依赖不变 | `git diff 35ac2b1 -- rust/anchor-kernel/Cargo.toml` 为空；主 `Cargo.lock` 亦无变化。Record、Waiter、Phase 定义未增加字段或变体 |
| 9. 登记语义变更 | [工作记录 S10–S14](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#语义变更记录)分别记录计费、历史恢复、Code 阻塞、中断及等待门禁；未删除或放宽既有测试 |

## 实现选择与边界

D1 选择**从历史求和**：两种 Store 的 `load` 共用累计投影，`spent` 为所有记录 `stamp.cost` 的饱和和，`resource.json` 中的 spent 不具独立权威。记录成本在下一次裁决前加载，即使上一份记录没有 `after` 也计费；文件恢复先取已接受状态，再投影全部成本。预算判断复用同一 `charge`，包括批准写入目标任务的观察记录。零成本重提是宿主调用契约，本轮有正式测试，尚未接入 Host 调度器。

D5 的组合边界也明确验证：等待中的非生命周期提交始终以 `TaskWaiting` 拒绝；若该记录成本超预算，任务同时按 D1 结算为 `Failed(BudgetExhausted)`，节点内容不生效。拒绝原因与预算结算结果分别保存，不因成本消失或忽略超预算来维持等待。

D4 仅完成纯校验契约。CommitRef 只有节点/调用身份，不含 Artifact kind；校验器不做 I/O，执行者必须以 `ArtifactKind::Interruption` 冻结控制产物。真实 Host 的冻结、suppressed、投递账本和固定 Goose 场景属于 H4，本轮没有冒称完成。

保留预检目录为跨 Kernel/GraphValidator 的公开 API 回归，当前程序逐项断言修复结果，[本轮输出](evidence.json)为 `k01_regression_passed`。旧目录顶层的 evidence.json / review.md 保留原始身份和内容；其中丢弃迟到结果的建议已由 D5 替代。

已知未覆盖：崩溃后宿主重复提交造成的去重/重复计费按 D1 决定留待后续；历史求和的规模读取性能未验收。没有新增模型、Goose、Graph、HTTP 或 Session 专属内核分支。

## 命令与结果

四个相关 crate 的最终 all-targets 回归为 **1,285 通过、0 失败、112 忽略，共 45 个测试目标**，不是整个 workspace 的全量结果。实际命令、退出码、计数及八个内核/校验器源文件的 SHA256 保存在 [validation.json](validation.json)，其他检查见 [checks.json](checks.json) 与 [structure.json](structure.json)。普通 Cargo ignored 项没有被当成已执行的 Goose 场景。

从仓库根执行：

```sh
cargo build --manifest-path rust/Cargo.toml -p anchor-sandbox-bwrap --bins --locked
cargo test --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host --all-targets --locked --no-fail-fast
cargo clippy --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host --all-targets --locked -- -D warnings
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo fmt --manifest-path experiments/constitution-host-preflight/Cargo.toml --check
cargo clippy --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml --all-targets -- -D warnings
cargo run --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml > experiments/constitution-host-preflight/k01/evidence.json
```

原始成功回归及检查日志位于本机 `.local/constitution-k01/`；validation/checks 的 `raw_log` 指向具体文件。克隆仓库不携带这些日志；入库的失败日志与 JSON 可以直接查阅。首轮 `all-targets.log` 在会话中断时未取得结束统计，不作通过证据；`all-targets-final.log` 属于收尾门禁优先级修正前的 1284 项版本，本轮结论采用 `all-targets-k01.log`。

**未执行**固定 Goose、真实 Provider、Web、生产部署或 Host 新调度路径。K0.1 只交付内核/校验器契约，不代表 Host 接入通过。

## 独立复现修复前失败

在仓库根执行以下步骤；只复制到新临时目录，不切换或覆盖当前工作区。必须使用独立 target，避免不同源码快照的构建缓存混用。

```sh
k01_before_root="$(mktemp -d /tmp/anchor-k01-before-XXXXXX)"
git archive 0b71475 rust/anchor-kernel rust/Cargo.toml rust/Cargo.lock | tar -x -C "$k01_before_root"
sed -i 's/^members = .*/members = ["anchor-kernel"]/' "$k01_before_root/rust/Cargo.toml"
cp rust/anchor-kernel/tests/accounting_lifecycle.rs rust/anchor-kernel/tests/storage_regressions.rs "$k01_before_root/rust/anchor-kernel/tests/"
CARGO_TARGET_DIR="$k01_before_root/target" cargo test --offline --manifest-path "$k01_before_root/rust/Cargo.toml" -p anchor-kernel --test accounting_lifecycle --test storage_regressions --no-fail-fast
```

预期 Cargo 退出 101；该失败是检验旧缺陷的证据，不是最终源码的回归失败。`--offline` 需要已有依赖缓存。修复前 Runtime 中断日志来自同一旧校验器上的首批五项 JSON 黑盒测试；第六项普通 NodeSubmission 序列化测试在扩展字段后加入。
