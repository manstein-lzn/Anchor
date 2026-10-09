# 开发指南

先读 [开发约定](../AGENTS.md)、[当前架构](architecture.md)、[产品架构](product-architecture.md) 和 [开发台账](pilot-development-plan.md) 中与任务直接相关的章节。历史归档只用于查证当时证据，不作为当前任务列表。

## 项目结构与依赖

`rust/` 是唯一 Cargo workspace，官方 Host、Kernel、工具、发行和开发检查共享 [Cargo.lock](../rust/Cargo.lock)。`apps/web/` 使用 React/TypeScript；依赖见 [package.json](../apps/web/package.json) 和其 lock。官方 Plugin 清单/Skill 位于 `plugins/`，Graph 示例位于 `examples/`。

运行与构建需要 Linux、Git、Bubblewrap、Rust stable、Node.js 与固定 Goose。先执行 `npm --prefix apps/web ci`，再按 [使用指南](usage.md) 配置 `.env`。`scripts/dev.sh` 管理独立 Rust 开发根和本次服务 PID；修改后端需要重建并重启才能加载，不因文档变化或仅验证而重启用户服务。

## 工作节奏

先确定归属层、事实所有者和小契约，交付可运行的垂直切片。Agent loop、模型历史与 compaction 复用 Goose；Graph、Run、Artifact、Sandbox、Library 和 Session/Turn 由 Anchor 保持唯一事实。

独立且契约稳定的工作包可在隔离 worktree 中并行，一个路径只保留一个实际编辑者。主轨负责共享 manifest、集成、review 和验收；获得足够证据后直接集成，不反复等待报告。变更应聚焦，避免为形式化拆分建设额外平台。

工作区可能有多条工作流的未提交改动，不 reset、checkout 丢弃或 clean；提交只含本次实际改动。`.local/`、旧环境、密钥、日志和运行历史不是清理对象，不因源码或示例迁移被覆盖。更新示例不会自动迁移用户 Graph。

## 验证

开发时先跑相关 crate/测试子集；垂直切片或跨模块语义完成后再执行一次必要的整体回归。复用同一 `CARGO_TARGET_DIR`，减少重复构建。普通文档与低影响可逆改动检查链接/格式即可，不增加镜像式测试。

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-devtools --locked
cargo +stable test --manifest-path rust/Cargo.toml --workspace --all-features --all-targets --locked -- --test-threads=8
cargo +stable clippy --manifest-path rust/Cargo.toml --workspace --all-features --all-targets --locked -- -D warnings
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check
npm --prefix apps/web test
npm --prefix apps/web run build
```

已缓存依赖时可加 `--offline`，缓存缺失要如实报告。Goose 条件集成用例不在普通 `cargo test` 中自动执行，必须通过 [确定性 Graph 回归](runtime-contract-tests.md) 显式选择；缺少 Goose、浏览器或外部配置与通过是不同结果。

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
  cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools --locked -- \
  regression fixture
```

`ANCHOR_GOOSE_BINARY` 指向 [构建脚本](../scripts/build-goose-acp.sh) 产出的 Anchor 自建 lean Goose ACP 二进制；该脚本固定上游源码与 musl 工具链摘要、校验静态 ELF，并可复现 digest。

浏览器测试使用 Node 本地 Provider 与实际 Rust Host/Goose；执行方式见回归指南。release 包用 [原生候选回归](rust-production-candidate.md) 检查。大型 RSI、深度研究、周报和真实业务服务按低频内容验收单独运行。

测试报告只写实际命令、结果和证据，区分失败、跳过、未配置、未覆盖与真实模型请求。并发超时先独立复跑受影响用例，只有新的失败或未决风险才扩大验证，不重复跑已通过全量。

## 文档归属

| 信息 | 维护位置 |
| --- | --- |
| 长期约定 | `AGENTS.md` |
| 产品方向、职责和不变量 | [产品架构](product-architecture.md) |
| 当前实现 | [当前架构](architecture.md) |
| 安装和操作 | [使用指南](usage.md)、[部署指南](rust-production-deployment.md) |
| 完成状态、实际证据和未验收项 | [开发台账](pilot-development-plan.md) |
| 旧设计与调研 | [历史归档](archive/README.md) |

一个垂直切片或验收状态完成后才记账；小编辑、格式修复与重复测试不单独记账。未通过真实 Provider 的能力不得写成真实端到端完成。
