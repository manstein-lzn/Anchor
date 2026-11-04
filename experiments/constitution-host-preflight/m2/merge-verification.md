# §10.11：master F1/F2 合并验证

父提交为本分支 `2582528` 与 master `d968ba5`，使用 merge，不 rebase。19 个冲突路径逐项登记在[工作记录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)。门面公共接口采用 master 的安全 RunView、读取与意图操作；已批准的三相位、独立 pause、Kernel 准入、子调用、删除与 Probe 放在同一门面背后。默认入口仍是旧 Runner。Kernel 显式入口由 workspace 中的 Host 确定性与随机回归覆盖；完整 Goose ignored 沿既有默认路径执行，不代表 M2 的 Kernel 原生 Goose 能力已全量验收。

这次验证只验收合并，不宣布 M2 完成。并行 Call 裁决扩展仍按 §8 待批；常驻助手打断、执行结果补齐和原生人工恢复决定仍待实现。M3 未开始，未执行真实模型 Provider、生产数据迁移或部署。

## 验证

合并回归通过，最终 Goose 失败名单为空。完整命令、逐 target 原样结果行、退出码和日志摘要见 [merge-checks.json](merge-checks.json)。原始输出仅放在 `/tmp/anchor-m2/`，不提交。

| 检查 | 实际结果 |
| --- | --- |
| Rust workspace，all-features、locked、no-fail-fast，包含 doctest | 128 个 target；2,113 / 0 / 123 |
| workspace all-features/all-targets clippy，`-D warnings` | 通过 |
| Web 单元测试与构建 | 311 项通过；构建通过（保留既有大 chunk 提示） |
| fmt、C1（`0859373` 后 Kernel 零 diff）、工程措辞与 diff 检查 | 通过；围栏测试与 master 字节一致，业务引用 0 |
| 固定 Goose 完整 Host ignored，串行 | 40 个 target；120 / 0 / 0，无失败豁免 |

Goose 使用固定摘要 `17e065a7…`，显式 relay 与本次同一构建的 Host/沙箱 helper/原生工具，并预先构建 `http_fixture`。执行期间通过 `ANCHOR_TEST_HOST_BINARY_SHA256` 固定 Host 身份。场景原始根为 `/tmp/anchor-m2/merge-goose-final-evidence/`；直接提取未修改的 `anchor-devtools::goose::validate_report` 严格核查 109 份通用格式报告（包含标准 20 套的 105 场景），另 8 份沿用原生 API 探针、创作或摘要测试的独立报告格式。完整 ignored 的 120 项包含 2 个 Host 忽略单测及入口外补集，不能把它们全部算作标准 Graph 场景。证据校验摘要及源摘要见 [merge-checks.json](merge-checks.json)，未放宽仓库校验器。

首轮 workspace 在 Host target 为 992 / 15 / 2，因未加 `--no-fail-fast` 而在该 target 停止，不能算全量完成；输出为 `merge-workspace-final.log`。修正了 HTTP Kernel 选择丢失、operation 回执、当前 invocation 选择和会话删除守卫；状态词汇 fixture 的调整见下表。第二轮 `merge-workspace-verified.log` 暴露父 stop 跳过 paused wait 子 Run：物理静止不能代替显式停止，修复后原业务测试 `superseded_conversation_cannot_resume_or_recover_its_wait_child` 独立通过（`merge-conversation-stop.log`）。最终结果单独记录，不以定向复跑抹去先前失败。

Goose 首轮 `native_goose_compacted_effect_survives_host_kill_and_is_inspected_without_replay` 在 `/runs/{id}` 读取 trace 时收到 500（`Goose trace resource is malformed`）。保留现场 `/tmp/.tmpmXIhmh` 的 JSON 随后可正常解码；对应 writer 对 evidence 原地 truncate/write，reader 可观察半份 JSON，相关生产代码相对 master 无差异。本次仅将 trace evidence 改为私有临时文件写入、sync、rename、sync parent；没有改事实格式、恢复决定或重放规则。新增 `trace_reader_retains_complete_snapshot_during_evidence_publication`，用持有的旧文件描述符验证发布后仍可读完整旧版本，并验证新路径内容与 0600 权限。8 项 trace 单测通过（`merge-trace-publication.log`）；期间 Host 可执行文件摘要不变，首轮 Goose 没有混入新 binary。首轮完整 ignored 为 119 / 1 / 0；相同冻结 Host 上的独立 compaction 复跑为 1 / 0 / 0（`merge-goose-compaction-retry.log`），符合竞态表现。最终修复版完整 ignored 为 120 / 0 / 0，两轮及独立复跑分别保留。

严格场景校验额外发现 `goose_plugin_disclosure` 的旧报告缺少 Host/Goose 摘要，其确定性业务用例与 Provider transcript 已通过；只在原报告补充两个实际摘要字段，业务断言不变。首轮完整证据校验的失败保留在 `/tmp/anchor-m2/merge-initial-scenario-validation.json`；最终构建和完整 ignored 执行使用补齐后的报告。

二进制身份检查曾拦住一次无效重跑：在多包 build 后取的 Host 摘要，与随后 `cargo test` 重新链接的 Host 不同。该轮已主动停止（退出 143），输出为 `merge-goose-identity-aborted.log`，不计为产品行为失败或通过。最终命令先以同一 package/features 执行 `cargo test --no-run`，再取摘要；没有降低或移除身份断言。

## 断言与覆盖登记

相对 master 的词汇适配沿用 S19–S24、S26 和 M1 的独立 pause 决定；相对合并前本分支新增的两处 fixture 迁移为下表 MRG10/MRG11。未放宽产物、成本、路由、授权、不重放、Session 投递或删除保护断言；未新增 ignore。

| 编号 | 测试与具体变化 | 依据 |
| --- | --- | --- |
| MRG01 | `run_access::tests::projected_schedule_rule_*`：原已知旧 status 7 项 unfinished=true 改为 `projection.phase` 的 executing/waiting=true、settled=false；旧字符串、缺失及畸形值为 false。原未知值拒绝保留，测试改名为 `projected_schedule_rule_recognizes_only_public_phases`。 | S19 |
| MRG02 | `phase_maps_each_legacy_state_without_exposing_its_vocabulary`：Paused/BudgetStopped 的通用 error 改为 reason=paused/budget；Done 的 reason 改为 None；其余映射保留。 | S19 |
| MRG03 | `predicates_*`：Ready attention false→true；Paused quiescent false→true；Paused/Stopped attention false→true、stop true→false；BudgetStopped quiescent false→true、resume false→true、stop true→false；WaitingCall attention false→true、resume/stop true→false；WaitingRecovery stop true→false；Aborted abandon true→false。测试改名 `predicates_follow_public_phase_waiter_and_outcome_distinctions`，其余表项不变。 | S19–S23 |
| MRG04 | `active_and_session_observations_do_not_reinterpret_durable_state`：stop/pause 按 Executing 与 worker，不按 pending control；Session 仍收窄 pause/resume；attention 不被 pending 意图覆盖。新增 abandon 与 phase 不变检查，原 active/quiescence 检查保留。 | S22/S24、独立 pause |
| MRG05 | `pilot_session_retained_records_keep_*`：完整 record 字节期望仅移除唯一 status 字段；保留其余字段、顺序、缺失 Run 过滤与空列表。改名 `pilot_session_retained_records_keep_progress_bytes_order_and_filtering`。 | S19 |
| MRG06 | `pilot_read_tools_keep_*`：旧 status/phase/control_requested/terminal/quiescent/needs_attention 改为 executing、reason、actions.abandon、updated；record 仅移除 status，保留字节级比较。mtime 固定为 Unix 秒 1。改名 `pilot_read_tools_keep_phase_json_bytes_and_progress_field_order`。 | S19/S22/S24/S26 |
| MRG07 | `http::tests::detail_preserves_*`：10 个旧状态改为三相位及 waiter/outcome/reason；resume/stop/abandon 按 M1 动作表；移除 state.status 与 control_requested。其余身份、拓扑、输入、cursor、passes、产物及完整字节比较保留。改名 `detail_preserves_phase_json_bytes_for_each_legacy_fixture`。 | S19/S22–S24/S26 |
| MRG08 | `http::tests::abandon_response_preserves_*`：移除旧 status=aborted 字段；三种 abandon action、abandoned、reason、requested_at 的字节检查保留。改名 `abandon_response_preserves_phase_contract_bytes_and_action_distinctions`。 | S19 |
| MRG09 | `channels::tests::channel_queries_preserve_each_legacy_state_and_startup_action`：10 个状态的 wire 改为 executing/waiting/settled；completed/unsuccessful/executor/settled、助手保留与启动结算的业务期望全部不变。 | S19 |
| MRG10 | `api::tests::schedules::timeline_marks_persisted_admission_blockers_busy_across_history_pages`：fixture 的 running/paused status 改为 projection.phase=executing/waiting；missed_busy、历史跨度与时序断言不变。 | S19 |
| MRG11 | `api::tests::wecom::wecom_progress_reports_only_settled_state_for_a_finished_turn`：SSE status=completed→settled；settled=true、无 update、无额外状态文案及未知身份拒绝不变。 | S19 |

`dispatch_success_and_stopped_execution_rules_cover_every_legacy_state` 保留 master 的全部期望：Paused/BudgetStopped 物理静止，但没有显式停止；父 Run stop 仍必须转递到这些 wait 子 Run。Session 额外保留当前轮占有规则，不把静止视为交接授权。新增两项门面 Kernel 测试覆盖权威相位刷新、wait-child/worker 和 pause/budget 限制。

## 删除测试映射

| 原测试或分支 | 去向与理由 |
| --- | --- |
| master 的 `deleted_graph_call_marker_only_relaxes_the_incoming_reference` 中 Deleted 参数分支 | 本分支 H1/S9 已删除该枚举并具名拒绝旧标记（离线迁移），该行为已不存在。其余六种调用引用、父对象保护和无写入断言保留在 `retained_graph_calls_protect_the_incoming_reference`，没有删除整个测试。 |
| 上表六个改名测试（MRG01、03、05、06、07、08；通配名称各对应同一函数） | 原测试原位改名，不删除覆盖；具体等价名见各行。 |
| master 的 `legacy_mcp_preparation_is_read_only_and_runner_upgrade_requires_the_run_lease`、`preliminary_wiring_accepts_only_unchanged_legacy_identity_and_validates_skill_authority` | 沿用 H1/C6 已登记的 `legacy_mcp_preparation_and_load_require_offline_migration_without_rewrite`、`preliminary_wiring_refuses_legacy_identity_and_validates_skill_authority`；离线迁移拒绝与无重写断言保留。 |
| master 的 `run_status_http_json_bytes_match_the_master_contract` | 沿用 M1.5 的 `run_status_http_json_bytes_match_the_phase_contract`，登记 M15R17–M15R21 保持有效。 |
| 合并前的 `operator_reapproves_changed_plugin_for_reused_and_admitted_graph_digests` | 接受 master 已验的两例拆分：`operator_reapproves_changed_plugin_for_reselected_graph_digest` 与 `operator_reapproves_changed_plugin_for_initially_admitted_graph_digest`。两参数仍共用原实现与全部断言，每例各生成一份证据；忽略数随拆分增加 1，实际场景不变。 |
