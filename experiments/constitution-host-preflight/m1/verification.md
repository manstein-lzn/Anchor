# M1：完成 R2 的本地验收

2026-10-19。起点 `d915314`，工作区 `constitution-core`。范围按 [全量迁移任务书 §1](../../../docs/archive/runtime-migration-2026-10-20/constitution-migration-task.md#1-m1完成-r2对外状态词汇)，在本里程碑交回；M2–M5 未开始。源码与本证据同属本次 M1 提交。**本地完成，交回统一审查；没有触发 §8 必须停下条件。**

## 实现与验收边界

- pause 是独立动作，基础可用条件为 Executing 且有 worker；HTTP/Pilot 回执为 operation=pause，不再返回 alias。
- 旧 Run 仍调用旧 Runner 的边界暂停。Kernel Run 的意图单独持久保存、通过事件交给调度边界：当前节点结果和成本先提交，再提交 Human stop，reason=paused；不取消当前节点。控制接受、意图消费和清理由同一宿主锁排序。
- 重启先核查执行端口完成事实，已完成则提交再暂停；没有新事实时先停止派发。未知效果仍先核查，不自动重放；最后节点已经结算时保留 Done。
- API、Pilot 和 Web 共用 pause/stop/resume/abandon 四动作。Session call 通用 pause/resume 仍拒绝；conversation 当前轮在 paused/budget 等待时继续占有 Session，物理静止不能替代交接规则。
- 同一 HTTP 测试经真实 Host Op，分别触发旧 Runner 和 Kernel：Executing → Waiting(Human, paused) → Settled(Done)，比较除观察时间外的完整 projection；同时检查首节点产物、单次结果、恢复后同一 commit。
- Pilot 两个独立测试检查 pause 意图与 stop cancellation 的区别及活动 Turn 授权。Web 新增未知 effect 等待、pause/stop 点击、服务端拒绝与 busy 控件测试。

Kernel 与生产 GraphValidator 零改动；未新增 Code 提交种类、执行标志轮询、旧状态替身或第二份 Run 存储。Kernel pause 意图是宿主命令，不是第二份 Run 相位。

## 验证结果

最终 workspace **1922 通过、0 失败、115 忽略**，121 个测试目标；计数见 [test-summary.json](test-summary.json)。sandbox 构建、fmt、clippy 全部通过，C1 输出为空。Web **34 文件、290 通过**，build 通过。逐命令结果：[rust-commands.json](rust-commands.json)。

| 检查 | 命令 | 原始输出 |
| --- | --- | --- |
| sandbox 二进制 | `cargo build --manifest-path rust/Cargo.toml -p anchor-sandbox-bwrap --bins` | [sandbox.log](sandbox.log) |
| fmt | `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` | [fmt.log](fmt.log) |
| workspace clippy | `cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings` | [clippy.log](clippy.log) |
| workspace 全量 | `cargo test --manifest-path rust/Cargo.toml --workspace --no-fail-fast` | [workspace.log](workspace.log) |
| C1 | `git diff 6ee2b0e --stat -- rust/anchor-kernel` | [kernel-diff.log](kernel-diff.log) |
| Web test | `npm --prefix apps/web test` | [web-test.log](web-test.log)：34 文件、290 通过 |
| Web build | `npm --prefix apps/web run build` | [web-build.log](web-build.log)：通过，保留 Vite chunk 大小提示 |

定向修复证据：[Kernel 13 项](kernel-focused.log)、[HTTP 双后端](http-comparison.log)、[Pilot 2 项](pilot.log)、[API projection 3 项](api-projection-recheck.log)、[标准 Host 3 项](standard-host-recheck-final.log)、[RSI HTTP MCP](rsi-http-recheck.log)、[Gateway 独立复跑](gateway-independent.log)。定向 Kernel 未重复 1000 seed；最终全量 workspace 已覆盖它及已有 Validator 串行/并行/调用随机对照。

与 R2 交回相比：API 子集从 188 通过/11 失败变为 200 通过/0 失败（增加同一 HTTP 对照）；Web 从 285 增至 290，通过范围从未完成的六 crate 扩为整个 workspace。

真实 Goose/Provider、native 浏览器 e2e、部署均未执行。workspace 中已有 ignore 不会由普通 cargo test 自动执行，不能以绿色总结果声称这些路径通过；没有新增 ignore。真实 Goose 确定性回归归 M3，真实 Provider 与生产迁移仍是合并前门槛。

## 失败与复跑

首轮全量 [workspace-initial.log](workspace-initial.log)、[命令结果](rust-commands-initial.json)：1916 通过、6 失败、115 忽略。失败分别为：

1. `api::runs::tests` 两项遗留旧 phase/needs_attention/三动作期望；已按新词汇迁移，所有身份拒绝仍是四动作全 false。
2. `standard_host` 两项仍轮询 state.status，导致已完成的 Run 超时；改为 projection settled/done，产物和旧事实断言保留。
3. RSI 本地 HTTP MCP 客户端在 workspace feature 合并后缺少 rustls crypto provider；测试显式初始化既用的 ring，工具发现/冻结证据断言不变。
4. Gateway `slow_callback_does_not_block_control_and_older_reply_is_suppressed` 发生时序失败；独立复跑通过，没有修改测试或业务断言。

修复期间的一轮检查在完整回看首轮最后失败后主动中断，以免继续执行昂贵随机对照；[不完整输出](workspace-interrupted.log) 不计为通过。标准 Host 首次定向复跑还遇到一次 `ExecutableFileBusy`，保留在 [日志](standard-host-recheck.log)；再次独立复跑三项全部通过，未改 fixture 或扩大超时。

初次 API 定向 198 通过/1 失败，是批量恢复 pause 期望时误改显式 stop 的断言；已恢复原 Stopped 并独立通过：[初轮](api-initial.log)、[修复](api-stop-recheck.log)。这些失败均保留，不以最终绿色覆盖历史输出。

## 断言与审查

- 既有期望修改：Rust 26 处（20 处断言期望、6 处轮询目标）、Web 4 处；参数展开和 schema fixture 增补另列，不混算宏数量。[Rust](rust-assertions.md)、[Web](web-assertions.md) 与 [工作记录三表](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#m1完成-r2-对外词汇2026-10-19)。
- 新增测试：Rust 8、Web 5；删除 0、新增 ignore 0。业务行为断言没有放宽。
- S25 作废；S19–S24、S26 七条保持待批，无新增 S 编号。S21 的 conversation 交接约束保留在 Session 层，S23 的旧预算写入拒绝仍未迁。
- 无须修改 Kernel/Validator，没有触发 §8 必须停下条件。按 §0 在 M1 交回统一审查。
