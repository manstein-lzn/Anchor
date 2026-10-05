# Rust Runtime 迁移收尾与二进制交付重新评估

2026-10-04，Rust-native Runtime 扩展路线冻结。

当前 Rust 代码已经形成一条有价值的实验性 standalone Graph Host 垂直切片：它保留了 Graph/Run 持久化与调度、Bubblewrap Sandbox、Artifact 快照、io-harness AgentNode、Plugin/MCP 接入、部分 Graph call/并行/恢复能力，以及 Rust HTTP/React 的 Graph、Run、Artifact 首片。相关代码、测试和证据继续保留，作为后续重新评估的技术资产。二进制 Runtime 的第一阶段范围与验收见 [Rust Binary Runtime 产品化计划](rust-binary-runtime-plan.md)。

这条切片不构成 Python Anchor 的生产替代，也不代表 Session、Turn、SSE、Scheduler、Channel、Plugin 管理或完整平台行为已经迁移。当前生产路径继续是 Python `serve.py`；Rust Host 维持实验性 standalone 使用边界，不再推进 R8/R9、平台迁移、逐项兼容或新的 Runtime 抽象。

冻结的原因是继续扩展会同时维护两套尚未形成统一生产事实所有权的 Runtime，而现有切片仍有若干用户语义和恢复边界未收敛。已知限制记录如下，供以后重新评估时核对；它们是当前边界说明，不构成新的待办清单：

- Rust Op.run 已将合并后的 Graph input JSON 注入 `ANCHOR_INPUT`；release binary 的 Bubblewrap routing 回归已验证业务 Op 能读取触发输入。
- Graph 可编辑资产与执行快照仍有 DTO 边界问题，WebUI 的 `layout` 等编辑字段不能直接按执行快照保存。
- Stopped Run 的重新触发语义与产品预期不一致；当前实现仍把它视为阻止再次触发的未完成 Run。
- standalone stdio 在 Run 创建前拒绝 `Op.call detach` 和 `Op.call session`；HTTP 宿主继续拥有后台 dispatch 与 Session handoff。并行分支内的 `Op.call` 也在 admission 阶段拒绝，避免协调器不能提供 GraphCallPort 时留下部分 child admission。
- standalone bundle loader 已拒绝 Plugin 根目录 symlink；通用 Library catalog 仍保留兼容旧资源库 symlink 的行为，不能把后者直接当作可分发 bundle。
- Rust HTTP 的 `ANCHOR_API_KEYS` 已改为 JSON 数组、唯一且至少 32 字节的校验，与 Python 配置契约一致。
- Agent 的 Graph `model` 字段仍使用部署级 Provider；`network` 已映射到 `anchor_run` 并受宿主网络授权约束。声明式 `reads/writes` 已可编译并通过 Host admission，但当前仍是输入/输出接口声明，不是逐文件写权限限制。
- Python adapter 尚未覆盖 Rust 的 `waiting_recovery`、`aborted` 状态，且同步默认超时短于合法节点预算。
- Host 层仍有可恢复 Harness 错误被投影为 Failed、并行混合恢复状态无法保存、恢复路径取消观察不完整、完成后 Agent trace 不再从 Run API 投影等边界。
- 历史 Rig `NodeExecutor` 仍存在于共享 Kernel 的公开 API 中，虽然当前生产 HostNodes 使用 io-harness 唯一 loop；它不应被误认为新的生产执行路径。

## 重新评估结论

同日用户进一步明确了 Rust 的产品交付价值：Python 版本交付时需要暴露 Anchor 源码，依赖链复杂且运行开销偏高；Rust 版本可以交付编译后的二进制 Runtime，再与 Graph 包组合部署。这满足“真实用户任务证明明显收益”的重新评估条件，但只重新开启一个有边界的产品化切片：**二进制 Runtime 交付**。完整 R8/R9 平台迁移仍然冻结。

该切片的验收出口是：

1. 生成可重复构建、版本固定的 Rust Runtime 二进制；核心 Runtime 不需要 Python、Anchor Runtime 源码或 Python 依赖。按 2026-10-05 用户澄清，Plugin 保留既有生态及任意语言，外部解释器和业务依赖可由部署者授权提供，只做适配接线。
2. Graph 包只携带 Graph 定义、明确声明的 Plugin 资源和无密钥 manifest；Runtime、模型配置、凭证、状态和工作区由部署环境提供。
3. 在本机隔离的干净部署环境中用至少一个真实用户 Graph 完成真实 provider、Sandbox、Artifact、重启和恢复验收；物理新机器不是前置条件。
4. 同一 Graph 在 binary host 与当前 Rust HTTP host 上产生一致的 Run、Artifact 和恢复事实；不能以“能启动”代替一致性验收。
5. 实测安装体积、启动时间、内存和执行开销，再与 Python 基线比较；provider 网络延迟占主导的场景不能预先宣称 Rust 会明显提速。
6. 明确 Graph/Plugin 中仍需携带的脚本或资源。编译 Runtime 只隐藏 Anchor Runtime 源码，不会自动隐藏所有 Plugin 脚本、Graph JSON 或业务代码。

当前已完成首个 provider-free 发行包垂直切片：`scripts/package_rust_runtime.py` 生成确定性归档，归档只含 release ELF、format-1 bundle、部署说明和运行清单；边界测试覆盖 symlink、未知顶层资源、Plugin 目录不匹配和非 ELF binary。`scripts/rust_runtime_package_smoke.py` 已把打包、源码排除、解压和最小环境启动固化为可重复验收入口，并将 Runtime cwd 固定在独立 deployment root；最近 evidence 为 `.local/rust-runtime-package-4okpp2ld/evidence.json`。随后同一真实 Graph 已从解压后的 release 发行包运行，真实 Provider 和本机 MCP fixture 均完成，证据 `.local/rust-multinode-t2_n_qn4/evidence.json`；`scripts/rust_runtime_recovery_smoke.py` 又验证了暂停点、进程重启和同一 Run 续行，最近证据 `.local/rust-runtime-recovery-cs0irsvt/evidence.json`。`scripts/rust_python_baseline.py` 的首轮 5 次冷进程对照显示 Rust 简单 Op 场景速度较慢，但发行归档和峰值 RSS 更小；该结果包含 Sandbox/持久化实现差异，不是语言级速度结论，证据 `.local/rust-python-baseline-tqkga_ov/evidence.json`。这证明了本机隔离部署环境中的 binary + Graph bundle 真实端到端、已持久化暂停点恢复和首轮交付收益；本机 fixture 仍不等于业务 MCP，跨存储故障窗口仍未验收。不同 Linux 发行版兼容性暂不属于本阶段范围。

第一条垂直切片只解决上述交付闭环，不顺带建设 Rust Session/Pilot/Scheduler/Channel，也不把 Python 平台一次性切换为 Rust。若该切片不能证明交付、安装或运行运维上的可测收益，就停止后续迁移。

收尾前已执行并通过：

- `cargo test --workspace --all-targets`
- `cargo clippy --workspace --all-targets -- -D warnings`

上述验证证明当前代码可构建并通过既有测试，不等同于真实 provider 的完整平台验收。

2026-10-05 收口复验补充：在补齐上述 standalone admission 后，`cargo test --manifest-path rust/Cargo.toml --workspace --all-targets`、`cargo clippy --manifest-path rust/Cargo.toml --workspace --all-targets -- -D warnings` 和 `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` 均通过。`scripts/rust_runtime_package_smoke.py` 从 release binary 生成无源码发行包并在独立目录完成 Run/Artifact；`scripts/rust_runtime_recovery_smoke.py` 从发行包启动 HTTP Host，重启后续行同一 Run 并完成 Artifact。证据分别见 `.local/rust-runtime-package-ip7s0veu/evidence.json` 和 `.local/rust-runtime-recovery-qzor4wv0/evidence.json`。这次收口没有把 standalone Graph Host 提升为 Python Anchor 平台替代，也没有宣称四个业务 Graph 均已 Rust 验收。
