# H1 旧格式清理验证

2026-10-16。范围按用户修订决定：H1 只删除旧格式兼容并建立直接写入基线；FileStore 随 H2 接入，C2 在 H3 验收。实现结论、语义 S9/S15、逐名删除测试及 16 save / 36 mutation 的全部文件:行号/用途见[工作记录](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md#h1旧格式清理实现完成)。

## 最终命令与结果

```sh
cargo build --manifest-path rust/Cargo.toml -p anchor-sandbox-bwrap --bins --locked
cargo test --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host --all-targets --locked --no-fail-fast
cargo clippy --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime -p anchor-runner-host -p anchor-graph-host --all-targets --locked -- -D warnings
cargo fmt --manifest-path rust/Cargo.toml --all --check
git diff --check
```

- 最终四 crate 全目标：45 targets，1278 passed、0 failed、112 ignored；Host 主目标 894 passed、2 ignored，Runtime 121 passed。所有命令退出码 0。
- 三组差分继续通过，原断言与规模不变：串行 10,000、并行 5,000、调用 5,000。内核、GraphValidator 和差分目录无源码差异。
- Rust 生产/测试源码检索 `migrate_format|legacy_environments|can_upgrade_legacy_authorization|GraphCallStatus::Deleted` 无残留。旧标记仅在显式拒绝规则/测试 JSON 中保留，不再是枚举或升级 API。
- 旧 format 1–12、environments、缺失授权摘要、Deleted、旧 cursor：直接解码和 FileRunStore 加载均拒绝并提示 offline migration，原文件字节不变；旧 cursor/授权不能进入 Runner 派发。现代记录 roundtrip、串并行恢复、权限漂移及调用身份测试继续通过。
- 独立只读审查发现初版 Value 中转会覆盖重复键且漏过数组格式，已改为原生流式 MapAccess + remote derive；新增已知顶层/嵌套重复字段拒绝、非对象序列具名拒绝回归，最终全量包含这些修复。

## 中间失败与收敛

最初 Runtime 用例含无依赖 tempfile、路径借用及仍断言自动迁移的测试；修正夹具后，纯兼容测试按工作记录逐名删除/替换。Host 第一轮全量四项失败均因旧夹具空授权摘要；补齐当前格式字段，原业务断言不变。可变 MCP declaration 安全测试改为现代授权夹具，避免被更早的旧授权拒绝遮蔽。早期脚本引号语法错误未落盘，最终命令已重新执行并通过。

## 证据与未覆盖边界

最终原始输出仅在执行机器 `/tmp/h1-final-all.log`、`/tmp/h1-final-clippy.log`；仓库不携带，克隆后需按命令重跑。直接写入表的 52 个条目（16 save + 36 mutation）在最终源码逐行复核，非运行时调用次数。

112 个 ignored 未执行；本轮没有显式运行 ignored 固定 Goose 场景、真实 Provider、Web 或部署，也未实施离线数据迁移。通过测试不代表生产切换、新 Host 调度器、内核 FileStore 接入或 C2 清零已完成；这些仍属 H2/H3 及后续阶段。旧 RunStore 保持唯一 Run 权威，无双写。原[存储边界预检](review.md)是阶段调整前的历史证据，不再是当前停止条件。
