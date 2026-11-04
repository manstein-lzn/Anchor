# M3 §10.15 交回与验证

M3 的实现与规定回归已完成，待用户审查 S27（承接 S4）并决定是否合回 master。本轮从 `4f13ac4` 继续，按任务书 §10.15 恢复 master 的显式 resume 契约并修复首轮固定 Goose 的全部 12 项失败。没有合回 master、再次同步 master、部署或自动开展 M4/M5。

首轮 **110 / 12 / 0** 及 12 项逐项再次失败的证据保留在 [§8 停止交回](verification.md)、[checks.json](checks.json)；不能改写为首轮通过。本轮结果、逐 target 原样结果、12 项单独复跑 argv/退出码/日志 SHA256 见 [completion-checks.json](completion-checks.json)。原始日志仅在本机 `/tmp/anchor-m3/section-10-15/`，不随仓库分发。

## 结果与范围

| 实际执行 | 结果 |
| --- | --- |
| 最终 `cargo +stable test --workspace --locked --no-fail-fast --manifest-path rust/Cargo.toml` | 129 targets，**2,153 / 0 / 125**，退出 0。 |
| 原 12 项失败逐项单独复跑 | **12 项各 1 / 0 / 0**，退出均 0；下文逐项贴出原样结果。 |
| 完整 Host `--ignored --test-threads=1` | 41 targets，**122 / 0 / 0**，退出 0；无失败豁免。 |
| 恢复定向单测 | 27 / 0 / 0。 |
| Goose 执行边缘单测 | 9 / 0 / 0。 |
| MCP Host 定向 | 25 / 0 / 2；原 2 项 ignored 在这条普通命令中未执行。 |
| 列表/详情动作投影独跑 | 1 / 0 / 0。 |
| Web `npm --prefix apps/web test` | 39 文件，333 / 0。 |
| Web `npm --prefix apps/web run build` | 退出 0；保留 Vite 的已有大 chunk 提示。 |
| 严格 workspace/all-target Clippy、fmt check | 均退出 0。 |
| Host、sandbox/工具 bins、MCP example、全部 Host tests no-run | 均编译通过。 |

本轮首个 workspace 为 2,152 / 1 / 125；唯一失败是列表/详情投影夹具仍期待 pending 时 resume=false。按 §10.15 改为 true，保留两入口投影一致和其他动作断言；单独复测后再完整运行 workspace，得到上表最终结果。首轮日志 `workspace-1.log`、单独复测 `projection-retry.log`、最终 `workspace-final.log` 均保留并有摘要，不将首轮写成全绿。

首次完整命令在 workspace 之后因 Cargo 的依赖特性组合不同重新编译了 Host/relay。发现身份变化后主动中止（验证进程退出 143，`goose-full-feature-rebuild-interrupted.log`），不把未完成运行算通过；原 12 项独跑结果和身份摘要另存。随后用完整命令对应的 Host 单包 `--no-run` 预编译，再冻结身份，重新独跑全部 12 项并完整运行套件，得到本文最终结果。没有改动源码或重跑普通 workspace。准备差异和未完成原样结果在 completion-checks.json 中单列。

与上一轮停止交回相比，workspace 和 Web 计数不变，固定 Goose 从 110 / 12 / 0 变为 122 / 0 / 0。与 M2 最终 2,139 / 0 / 136 相比，workspace 通过数增加 14、忽略数减少 11；12 个重复 ignored 的来源与逐项名单仍见 [原交回的忽略数解释](verification.md#忽略数解释)，不是删掉业务覆盖。

## 修复与断言

- 普通人的显式 resume 与认证 Session-call 共用 `inspect_interrupted_attempts`：发现真实 native pending 后经 `observe_recovery_attempts` 和 scheduler 共用的纯 `observation_stop` 保存 Code + Observation，逐项持久化“先核查，不盲目重放”的提示，再 Human release；Agent 在同一个原生 Session 核查后继续。后台自动恢复不调用释放 helper。
- 人工 Completed/Abort API 保留；执行准备移出 worker registry 锁，验证与错误优先序、幂等、冲突、持久说明先于控制的顺序保持。持久失败回归同时覆盖 Session-call 和普通人工 API。
- spike/version 1 未完成只在自己的 completion 边缘转已有 Failed；native/version 2 仍为 Resumable。会话身份损坏在模型/工具派发前返回 Failed，保留零新请求和历史字节断言。
- 配置、授权、沙箱输入和 MCP 初始化的已知失败保留类型至 Goose；IO、传输不明、取消、超时及清理失败保持 unavailable。MCP 的单独 EOF 不足以成为已知初始化失败，须观察子进程实际退出。没有按错误文字分类、全局改 Uncertain 或改 Kernel。
- Plugin Turn 三个夹具改为 HTTP 枚举 + Kernel 门面读取；同 Run/workspace/原生 Session、冻结快照、用户隔离与下一轮撤权断言保留。

人工恢复用例改名为 `kernel_observed_native_effect_accepts_durable_completed_or_abort_without_replay`：固定 Goose 产生实际未知效果、强杀 Host 后，夹具以实际 Code + Observation 提交准备已登记 pending，不再用普通 resume 准备。保留 Completed/Abort 202、效果一次、same Session、观察落盘、幂等 202、冲突 409、Abort 无新模型请求和无旧 runs 文件断言。唯一退出的恢复断言是裁定明确更正的“第二次普通 resume 必须 409”，动作投影改为允许显式核查。

更正逐条登记为 [M2 行为更正：恢复回到 master 契约](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#m3-1015m2-行为更正恢复回到-master-契约)，不新增 S 候选。M3 断言登记共 13 类（原 A1–A10，续加 A11–A13）；原 [Runtime 73 项映射](runtime-test-mapping.md)、[Host 39 项映射](host-test-mapping.md) 与重复 target 12 项映射保留，另列上述人工恢复改名。业务断言放宽 0，既有测试新增 ignore 0。

## 原 12 项失败的单独复跑

每条直接执行原 target 的同一 Rust test binary，只选择一个测试，参数 `<test> --ignored --exact --test-threads=1`，工作目录 `rust/anchor-runner-host`。环境与完整命令一致，仅证据根为 `/tmp/anchor-m3/section-10-15/goose-solo-final-evidence`。测试 7 是 §10.15 批准的改名，其余测试名不变。

| # | 测试 | 原样 test result |
| --- | --- | --- |
| 1 | `native_goose_unknown_external_effect_reopens_same_session_and_checks_before_continuing` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 22 filtered out; finished in 9.01s` |
| 2 | `real_goose_external_effect_then_host_kill_is_spike_failclosed_not_native_safe_recovery` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 22 filtered out; finished in 8.10s` |
| 3 | `native_goose_unknown_channel_result_resumes_same_session_and_inspects_without_resending` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 9 filtered out; finished in 9.94s` |
| 4 | `missing_predecessor_fact_fails_closed` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 8.63s` |
| 5 | `missing_predecessor_model_binding_fails_closed` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 8.70s` |
| 6 | `missing_predecessor_scope_fails_closed` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 8.68s` |
| 7 | `kernel_observed_native_effect_accepts_durable_completed_or_abort_without_replay` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 18.06s` |
| 8 | `managed_configuration_rotates_atomically_without_restarting_or_changing_live_invocations` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 45.92s` |
| 9 | `graph_mcp_startup_errors_settle_without_repolling_completed_resolution` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 11.53s` |
| 10 | `add_call_remove_preserves_run_native_history_workspace_and_frozen_run_snapshot` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 11.27s` |
| 11 | `default_restart_handover_restores_same_turn_grants_then_next_turn_revokes` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 10.51s` |
| 12 | `save_during_actual_tool_keeps_current_turn_and_other_users_adopt_independently` | `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 12.46s` |

## 完整固定 Goose

完整运行在 12 项独跑全过之后执行。所有 Cargo 构建产物使用 `/tmp/anchor-goose-regression-target-core`，relay 使用该目录，未与 master 共享。Host、固定 Goose 和全部 helper 在独跑之前冻结身份，独跑及完整套件后分别复核，七个文件的 SHA256 全部一致；[前后身份](completion-binary-identities.json)。固定 Goose 的 SHA256 为 `17e065a79cb0508413af714724aed51d6b76972dda9feebdda701330134fb640`，未更换或在采证中重建 Host/helper。

实际完整命令：

```sh
CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target-core \
CARGO_BUILD_JOBS=2 \
ANCHOR_GOOSE_BINARY=/var/tmp/anchor-goose-build/artifacts/goose-acp.first \
ANCHOR_GOOSE_LOCAL_NETWORK=1 \
ANCHOR_GOOSE_RELAY_BINARY=/tmp/anchor-goose-regression-target-core/debug/anchor-net-relay \
ANCHOR_TEST_RUST_TOOLCHAIN=stable-x86_64-unknown-linux-gnu \
ANCHOR_PILOT_RUSTUP=/home/mansteinl/.cargo/bin/rustup \
ANCHOR_PILOT_RUSTUP_HOME=/home/mansteinl/.rustup \
ANCHOR_TEST_EVIDENCE_ROOT=/tmp/anchor-m3/section-10-15/goose-full-final-evidence \
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --locked --no-fail-fast -- --ignored --test-threads=1
```

41 targets 的原样结果全部保存在 completion-checks.json；汇总不是只选择通过的 target。所有 fixture 使用固定 Goose 与确定性本地 Provider，实际经过 Host、ACP、授权 MCP、Sandbox 和 Artifact；不能据此声称真实模型 Provider 或外部渠道实网通过。

## M3 交回

| 项 | 结论 |
| --- | --- |
| 提交顺序 | `0d029eb` 旧侧快照先于 `6e91e5c` 删除；`e4a9c68` 为格式整理。 |
| master 同步 | `b61c325` 仅在结束时合并 master `0aa5197` 一次；本轮未再 merge，不 rebase；`4f13ac4` 保留 §8 停止现场。§10.15 修复与本报告在随后提交中交付。 |
| C1 | 相对 `0859373` 的 `rust/anchor-kernel` diff 为空；Store trait、生命周期与裁决未改。 |
| C2 | save 16→0、mutation 36→0，52 项去向见 [清单](c2-destinations.md)。 |
| 规则与 Session | [八类规则、27 行所有权审计](rules-session-audit.md) 按独立判断体计数；普通 pending 写前阻挡 2→0、隐藏动作 1→0，唯一核查接续与观察登记分别 0→1，未把新增门禁藏在下降数中。 |
| 旧类型与围栏 | `rust/` 的旧类型及停止/暂停轮询名称零命中；Run 访问围栏与 master `0aa5197` 字节一致，业务引用零上限。 |
| S 候选 | 唯一 S27（承接 S4）：退役只移动 current 指针，未结算 Run 仍可 abandon；待用户裁定。 |
| 停止条件 | 本轮没有新增触发；此前恢复契约冲突已由 §10.15 裁定。 |
| 后续 | 由用户决定合回 master；M4 残余结构与 M5 测量不自动推进。 |

W4 未裁定；真实模型 Provider、外部渠道实网、部署和全产品联合验收本轮未执行。生产数据迁移工具按 §10.13 取消，没有清理或修改已有开发数据；新 state/workspace 根与保留范围仍见 [本地开发状态](local-development-state.md)。
