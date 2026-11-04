# K0.1 独立验收

审查对象：`88a6e41a1c71d5d0bc9ae2da3e5127ac75e4efee`。依据：[D1–D5 与九项门槛](../../../docs/archive/design-records-2026-11-04/constitution-preflight-decision.md)，并核对 Claude Code 会话 `40258599-7ff8-4bc8-907b-d0a4ee21d58d` 中的授权。该授权包含 D4 中断扩展；节点失败结算仍属于 H2。

## 结论

**K0.1 九项门槛通过，未发现阻塞该切片的正确性问题，可以按既有计划进入 H1。** 本结论覆盖内核与纯 Graph 校验器契约，不代表 Host 接入、常驻助手或真实 Provider 端到端验收通过。

## 逐项核对

| 项 | 结论与证据 |
| --- | --- |
| 1 | 原三个反例已纳入正式测试；读取入库修复前输出，确认成本、Code stop 与尾行恢复对应失败。修复前共 19 失败、1 通过的证据无内在矛盾；本次未重建旧源码运行。新增正式测试与修复同在目标提交。 |
| 2 | 正式测试验证 6+6 超过预算 10 时第二次结算、第三次拒绝仍计费；两分支接受/过期/零成本重提累计 7，版本只推进两次。Store 以历史成本饱和和投影 spent，预算结算另行推进版本。 |
| 3 | 定向测试通过：追加前尾截断、接受 stop 后模拟丢失状态写入再重启恢复、完整坏行 Corrupt、原完整行字节保留。没有进行真实断电实验。 |
| 4 | Code 无 Observation 拒绝、有 Observation 接受；Code 不能释放人的等待，Agent stop 拒绝。 |
| 5 | 仅 Code 可携带 interruption，显式 null 同样检查身份；规范控制 completion、RunResult.interruption 和普通路由均有测试。产物冻结属于 H4 执行者义务。 |
| 6 | Agent/Code/Human 等待中非生命周期提交均 TaskWaiting 且计费；超预算同时结算失败但不应用内容。Op.call 原差分通过。 |
| 7 | 本次实际运行三组差分共 20,000 例通过；对照起点，唯一源码变化为构造器补 interruption: None，断言、种子和例数未放宽。 |
| 8 | 对照 35ac2b1，内核 Cargo.toml 与主 Cargo.lock 无变化；无新增 Record 字段或 Waiter/Phase 变体。 |
| 9 | 工作记录 S10–S14 已登记本次语义变化，包含 D1 与 D5。 |

## 本次执行与历史证据

主审本次实际执行，均退出 0：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime --lib --tests --locked
cargo clippy --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host --all-targets --locked -- -D warnings
cargo fmt --manifest-path rust/Cargo.toml --all -- --check
cargo fmt --manifest-path experiments/constitution-host-preflight/Cargo.toml --check
cargo run --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml
git diff --check
```

结果：Kernel 37 项、Runtime 128 项通过，0 失败、0 忽略；包含串行 10,000、并行 5,000、调用 5,000 例。Clippy、两项格式检查与公开 API 探针通过，探针 status 为 `k01_regression_passed`。输出由本审查会话工具记录保留，本文件为结论记录。

另行委派两项只读专项审查：D2 存储恢复与 D4 中断校验/证据，均未发现确证的新缺陷。存储审查者另执行以下命令，8 项通过、退出 0（已包含在主审 37 项内，不重复累计）：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-kernel --test storage_regressions --locked --offline --target-dir /tmp/anchor-d2-review-target
```

历史证据另行核验：[validation.json](validation.json) 的八个源码 SHA256 现场重算全部匹配；读取其指向的本机原始日志，45 个测试目标合计 **1,285 通过、0 失败、112 忽略**。这不是本次重跑四 crate 全量的结果。修复前原始输出与检查入口见[原验收说明](README.md)。历史失败证据本次仅核验原日志与源码对应关系。

## 保留边界

- H1–H6 未实施，Host 仍使用旧 Runner；节点失败结算留 H2。
- H4 尚须实现中断控制产物冻结、不冻结被中断工作区，以及 suppressed、投递账本迁移。
- 固定 Goose/bwrap 常驻助手回归、真实 Provider 端到端、本轮 Web 与部署均未执行。
- 崩溃后宿主重复提交的去重/重复计费，以及历史求和规模性能仍未关闭。
- S4（退役后 abandon）未在本切片实施或关闭。
- 存储回归使用临时目录模拟状态写入丢失；真实断电、磁盘满、write/fsync 故障注入及多 Kernel 同根并发未专项验证。存储审查者尝试的额外仓库外复现因临时文件跨工具不可见未运行成功，不计入通过证据。
