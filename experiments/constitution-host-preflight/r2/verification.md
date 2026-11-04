# R2 停止现场验证

2026-10-19，源码 `74a4dec`。**未通过 R2 验收，未运行六 crate 全量。** 先执行定向检查；pause→stop 与非状态断言冲突后，按任务书停止扩展及全量验收，没有修改控制写入守卫。以下只记录实际执行结果；早期日志行号可能与定版源码有 fmt 差异。

## Rust

命令均从仓库根目录执行，manifest 为 `rust/Cargo.toml`。二进制简称 `BIN=rust/target/debug/deps/anchor_runner_host-2cefc3c824f4cf45`，是本次 cargo test 编译的同一目标；源码提交后没有再改 Rust。

| 实际命令/范围 | 结果 | 原始输出 |
| --- | --- | --- |
| `cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host application:: -- --test-threads=4` | 113 通过 / 1 失败 / 0 忽略；失败为新增 updated 字段的精确 JSON 预期，后来修复 | [application.log](application.log) |
| `cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host run_projection::tests -- --nocapture` | 3 通过 / 同一字节预期失败 | [projection-initial.log](projection-initial.log) |
| `cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host application::run_projection::tests` | 最终 4 通过 / 0 失败 | [projection-final.log](projection-final.log) |
| `cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host api::tests:: -- --test-threads=4` | 188 通过 / 11 失败 / 0 忽略；199 个定向 API 测试 | [api.log](api.log) |
| `$BIN api::tests::pause_resume_uses_same_frozen_run_after_graph_edit_and_service_object_restart --exact --nocapture` | 独立复跑失败；results.len() 实得 0，原非状态断言为 1 | [pause-independent.log](pause-independent.log) |
| `$BIN` 加上述 API 日志全部 11 个失败用例全名 `--test-threads=4` | 最终 0 通过 / 11 失败；旧 Paused 目标已改 Stopped，失败仍涉及 HTTP 返回码、缺结果、未知效果禁止重放 | [api-recheck.log](api-recheck.log)（含全部用例名） |
| `$BIN application::liveness::tests application::legacy_run_view::tests application::run_view::tests` | 3 通过；映射 10 状态×2 worker、动作 9 phase/waiter/outcome×2 worker、静止表 | [rules-final.log](rules-final.log) |
| `cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host pending_control_projects_active_tokens_with_stop_priority` | 最终 1 通过；只验证新 phase，不再验证删除的请求优先级展示 | [pending-final.log](pending-final.log) |
| `$BIN application:: --skip application_probe_matches_old_runner_on_one_thousand_serial_seeds --test-threads=4` | 113 通过 / 1 失败 / 0 忽略；显式不重复已通过的 1000 seed 对照。失败为候选子进程回收时序 | [application-final.log](application-final.log) |
| `$BIN application::plugin_candidates::tests::candidate_startup_and_call_cancellation_reap_the_actual_processes --exact --nocapture` | 该候选回收用例独立复跑 1 通过，无代码/断言改动 | [candidate-independent.log](candidate-independent.log) |
| `cargo build --manifest-path rust/Cargo.toml -p anchor-sandbox-bwrap --bins` | 通过 | [sandbox.log](sandbox.log) |
| `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` | 通过，输出为空 | [fmt.log](fmt.log) |
| `cargo clippy --manifest-path rust/Cargo.toml -p anchor-file-lease -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host -p anchor-library --all-targets -- -D warnings` | 通过，六 crate/all-targets 编译及 lint | [clippy.log](clippy.log) |

六 crate 的 `cargo test ... --no-fail-fast` **未执行**：已触发停止条件。FileLease/Kernel/Runtime/graph-host/Library 本轮没有独立全量测试结果，不复用 R 的通过数冒充本轮结果。未新增 ignore、未删除测试；定向排除 1000 seed 的命令在表内明确披露，初轮该 1000 seed 通过。

C1：`git diff 6ee2b0e 74a4dec --stat -- rust/anchor-kernel` 输出为空；生产 GraphValidator 相对前一提交 `b1e9bc1` 也无差异。其余 18 文件的生产写入规则未迁，原 Session callback 仅搬回 conversations，原内容不变；session_calls.rs 仅有测试断言修改，api.rs 的旧状态序列化表因新边界而删除。

## Web

Web 浏览器命令从仓库根目录用 `npm --prefix apps/web` 调用，并设 `ANCHOR_BROWSER_BINARY=/opt/google/chrome/google-chrome ANCHOR_WEB_TEST_PORT=5192`；表内列 npm 子命令。其余命令在 apps/web 运行；先安装依赖后执行。初次 build 因缺 node_modules 中的 tsc 失败（127），`npm ci` 成功后进入下面的最终检查。未改 package-lock。

| 命令 | 结果 | 输出 |
| --- | --- | --- |
| `npm test -- RunDetailPage RunInspector Timeline parallel persistentAssistant ChannelsPage` | 过滤词误纳 3 个 Playwright suites，3 suite 失败，原 285 单测通过；随后改用正确的 npm test | [原始错误](web-initial-filter-error.log) |
| `npm test` | 33 文件 / 285 测试通过 | [web-test.log](web-test.log) |
| `npm run build` | TypeScript（含 e2e）与 Vite 构建通过 | [web-build.log](web-build.log) |
| `npm run test:e2e -- e2e/recovery.spec.ts e2e/graph-calls.spec.ts e2e/parallel.spec.ts e2e/timeline.spec.ts --workers=2` | 10 例，9 通过 / 1 失败；新 reason 同时显示在多个位置，旧文案选择器不唯一 | [web-e2e.log](web-e2e.log) |
| `npm run test:e2e -- e2e/recovery.spec.ts --grep 'unknown tool result' --workers=1` | 将选择器明确为状态 badge 后，失败例独立复跑 1 通过；未再跑全部 10 例 | [web-e2e-recovery.log](web-e2e-recovery.log) |

Web 保留原恢复成功、时间、几何、产物和操作断言。human 恢复 fixture 仅证明服务端提供 resume 后 UI 可接续，不能证明旧 effect 可以 resume。原 effect→新 Human 的夹具变化与所有状态断言逐项登记在 [Web 清单](web-assertions.md)。真实后端 e2e 只做了类型适配，没有执行；没有真实 Goose/Provider、真实渠道投递或部署验收。

## 原始失败的处理

11 个 API 失败不是通过删除用例、增加忽略或修改非状态断言消除。定向复跑后的现场包含：①pause alias 在提交前取消，缺少原完成产物；②恢复会因已开始但没有终态结果而拒绝自动重放；③Paused/BudgetStopped 的新静止规则会改变旧占用/准入拒绝；④Session pause alias 原 409 变 202。第一、二项涉及执行与恢复事实，第三、四项涉及旧预期与新行为，均需任务书层面裁定后才能继续。

断言变更、S19–S26 待审查项与未完成验收，见 [测量交回](measurement.md) 和 [工作记录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#r2停止交回2026-10-19)。
