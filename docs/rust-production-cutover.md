# Rust 生产切换检查与准备

`anchor-devtools cutover` 只做旧数据盘点和切换前置检查。默认 dry-run，不初始化 Rust state、不调用 Host、不改动旧记录；不会搬移、删除、转换或双写数据，也不会读取凭据内容。旧数据保持原地只读；Run/Session/Turn 导入和 secret 配置必须由后续独立授权流程处理。

## 检查

检查 legacy 默认根 `~/.anchor`，并明确给出 Rust Host 的独立根：

```sh
anchor-devtools cutover \
  --legacy-root ~/.anchor \
  --rust-state-root /var/lib/anchor-rust/state \
  --rust-workspace-root /var/lib/anchor-rust/workspaces \
  --rust-catalog-root /var/lib/anchor-rust/catalog \
  --rust-library-root /var/lib/anchor-rust/library \
  --config /etc/anchor/runtime.json
```

命令向 stdout 输出 JSON inventory 和 `decision`：`legacy_read_only` 表示可保留旧事实只读并让新 Host 使用独立 Rust roots；`migration_candidate` 只表示 legacy root 为空、检查门槛满足，**不代表已迁移**；`blocked` 表示发现未完成 Run/Turn、ID 冲突、可疑凭据路径、活跃/未知写锁、可写 legacy 根未确认、根目录交叠或 inventory 不完整。`blocked` 以退出码 2 返回。检查是保守快照；它不能代替服务停机和部署编排。

旧部署的 Run 在 `workspaces/<graph>/runs/<run>/run.json`，Session/事件在 `sessions/<id>/`，原生会话/Turn 与工作记录在 `state/pilot-conversations.sqlite`、`state/pilot-turns.sqlite`、`state/pilot-steps/`；Library 在 `library/`。工具盘点文件路径/元数据，仅读取 Run 和 Session JSON，以及以只读 immutable 模式查询 Turn ID/status。SQLite 存在 WAL、无法读取、符号链接或特殊文件会 fail closed。凭据扫描仅输出路径，不读取或复制 secret 值。

Rust Host 以 `ANCHOR_RUNNER_STATE_ROOT` 独占 Run、Artifact、metadata 和平台 Session/Turn 持久状态；启动时取得 `deployment-locks/.deployment-writer.lock` 单写 lease，Run lock 在 `runs/`。Workspace、catalog 和 Library 分开配置，不可指向 legacy 的可写目录。Host 启动时仍必须重新取得 writer lease；本工具的锁检查只是当下观察，不能预留 lease。

旧 Scheduler 没有持久 host-wide lease，Library install lock 只保护安装，不代表旧服务已停止。因此在报告满足其他条件后，操作员仍需明确确认停掉旧服务和将旧根挂为只读：

```sh
anchor-devtools cutover \
  --legacy-root ~/.anchor \
  --rust-state-root /var/lib/anchor-rust/state \
  --rust-workspace-root /var/lib/anchor-rust/workspaces \
  --rust-catalog-root /var/lib/anchor-rust/catalog \
  --legacy-writer-stopped \
  --legacy-read-only-confirmed \
  --credentials-reviewed
```

这些 flags 是操作者对外部事实的声明，不会停止服务、改挂载或迁移凭据。没法确认时保持 `blocked`。若有凭据路径，应先在 Rust 部署侧单独配置新 secret，再确认 `--credentials-reviewed`；工具永远不处理 secret。

## 准备清单

只有检查结论非 `blocked` 时，显式 `--prepare --output-dir <新建或专用空目录>` 才会写两个权限收紧的文件：`cutover-manifest.json` 和 `backup-index.json`。准备动作原子且幂等，目录必须隔离，输出目录不可与任何数据根交叠。`backup-index.json` 是路径和文件元数据索引，**不是数据备份**；准备不复制业务文件，也不代表完成迁移或切换。正式操作前仍须用受控备份流程制作并验证完整备份。

## 单写与回滚

- 每个 Rust 部署只配置一个 canonical `ANCHOR_RUNNER_STATE_ROOT`，由 Rust Host deployment lease 拒绝并发 writer；不可把它配置到 legacy 数据根或复用 legacy 的可写 workspace/Library。
- 切换窗口停止 legacy writer；legacy 根保持只读。禁止同一 Run/Session/Turn 双写。切换检查不能消除检查后到 Host 启动前的竞态，Host lease 是最终单写门禁。
- 回滚前停止 Rust writer，保留完整 Rust state 和旧部署原始根；不能把 Rust 文件反向放进旧根。对切换后已接纳的 Run/Session/Turn 做逐项核对/保留后，才可恢复旧服务；ID 冲突、未完成执行和凭据分别复核。不得静默丢弃新端事实。
- 这个工具提供回滚前置条件清单，不执行生产切换、恢复、导入或回滚。实际 provider、服务端和生产数据验收不由临时目录单测替代。

## 定向测试

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-devtools cutover --locked
```
