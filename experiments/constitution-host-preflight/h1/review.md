# H1 存储边界预检（历史证据，切分已决定）

2026-10-16 用户已决定：H1 仅删除兼容，FileStore 随 H2 接入，C2 在 H3 验收；原停止点已解除，本文后续为当时证据。当前结果见[H1 工作记录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#h1旧格式清理实现完成)与[验证证据](verification.md)。

K0.1 验收结论保持通过。本预检未实施 H1，未修改内核、GraphValidator 或 Host 生产路径。

## 已确认的阶段依赖

[计划 H1](../../../docs/archive/design-records-2026-11-04/constitution-host-integration-plan.md#h1内核存储接入与旧格式清理)要求 Host 改用内核 FileStore，并清零 Run 的直接保存；替换 Runner 则排在 H2，生命周期迁移排在 H3。

- 旧 [Runner](../../../rust/anchor-runtime/src/graph/runner.rs) 的 `persist`（27 行）直接保存整份 Run；254–270 行在派发前保存 cursor、invocations、passes。保存节点完成结果之外还有执行开始事实，不能机械换成 NodeSubmission。
- 当前 [GraphValidator](../../../rust/anchor-runtime/src/graph/validator.rs) 的 `judge`（445 行）只接受开放空位上的节点结果或 CallSubmission；NodeSubmission 使用 `deny_unknown_fields`，没有任意状态替换或执行开始动作。
- [恢复入口](../../../rust/anchor-runner-host/src/application/recovery.rs) 的 118–150 行先写 Harness 决定，再直接保存 recovery_submissions、剩余 recovery 与 RunStatus。单次内核 resume 只改变 Phase，不能替代这些旧字段的写入。恢复事实归属和迁移需要跟执行者/生命周期一起处理。

这不证明新模型无法表达这些行为，也没有发现必须修改内核的理由。它说明严格按原 H1 边界完成“直接写入清零”，需要提前实施 H2/H3，或增加当前未授权的 Graph 提交契约。不能用接受整份 Run 快照的校验器包装来宣称满足 C2。

## 可执行证据

[独立探针](../src/bin/h1_storage_boundary.rs)通过公开 API 创建真实 FileStore（临时目录）和 Graph 任务，向开放空位提交两种内容：整份旧 Run 快照、单独 started 事实。两者均 `returned/content_invalid`，版本保持 0，Graph 状态保持不变，两次裁决写入历史。此探针只证明通用保存适配器不可直接复用；不是 Host 执行验收，也不是新调度器正确性验收。

实际执行并通过：

```sh
cargo fmt --manifest-path experiments/constitution-host-preflight/Cargo.toml
cargo run --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml --bin h1_storage_boundary
cargo clippy --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml --all-targets -- -D warnings
cargo run --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml
cargo fmt --manifest-path experiments/constitution-host-preflight/Cargo.toml --check
git diff --check
```

原有 K0.1 默认运行入口通过 `default-run` 保留。没有新增依赖、修改锁文件或删除测试。四 crate 全量回归、固定 Goose、真实 Provider、Web、部署均未执行；生产代码未修改，不重复 K0.1 全量验证。

## 对事实所有者的补充核查

执行开始与完成并非必须新增到 GraphValidator：[HostNodes::start/complete](../../../rust/anchor-runner-host/src/node_host/facts.rs) 已保存 invocation 的 durable started/completed 事实；[Goose completion_fact](../../../rust/anchor-runner-host/src/goose_acp.rs) 也能从既有执行记录返回 Completed、Resumable、Uncertain。新调度器可从开放空位导出稳定 InvocationKey，再查询执行者事实。这提供了 H2 迁移方向，进一步说明不能把旧 cursor 快照写入当作 H1 必需的新内核能力。

另需收窄恢复入口的解释：[标准 Host 的 record_recovery_decision](../../../rust/anchor-runner-host/src/node_host/execution.rs) 当前直接返回 `legacy Agent recovery is disabled in the standard Goose Host`。因此 application/recovery 中后续 Run 保存是待清理的旧路径，不能据其存在声称标准 Goose 的恢复成功路径需要新增 Graph 决定提交。该代码归属盘点不改变已经复现的 H1/H2 阶段依赖，也不要求修改内核。

本补充为只读源码核查，未执行 Goose 或 Host 场景。仍需在获准调整切分后实现并验证 H2 的执行者事实恢复，而不是用当前探针推断其已正确。

## 建议交回决定

建议明确调整阶段验收边界：H1 先完成旧格式具名拒绝、兼容分支删除及新存储门面；H2 切换生产调度器和执行者并移除其直接写入；H3 迁移生命周期与恢复后验收 C2 零直接写入。各阶段仍单独提交、编译和测试，C1/C3–C6 不变，最终 C2 不放宽。中间态不得将旧状态写入包装成内核已裁决事实，也不得双写作为两份 Run 权威。

另一可行选择是把 H1–H3 合成一个可运行的迁移切片，再按代码职责拆提交。它改变原计划的逐阶段验收粒度，因此本轮没有擅自执行。

停止点：计划第 5 节“某个 Host 行为在新模型中找不到对应的原语表达”触发边界讨论；这里具体是旧 Runner 的 dispatch 前状态写入和恢复决定状态写入，在现有 Run 提交契约中没有机械替换。待决定的是阶段切分和事实归属，不是请求修改内核。生产路径保持原状，无新增运行语义或 S 编号。
