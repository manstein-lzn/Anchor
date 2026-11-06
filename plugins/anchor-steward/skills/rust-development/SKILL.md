---
name: rust-development
description: 复用 Host 已授权 Rust 工具链，在项目工作区构建、测试并核对真实结果。
---

# Rust 开发

Pilot 先调用 `platform_status({})` 核查冻结工具链。确需初始化且已授权联网时，用 `runtime_prepare({"kind":"rust"})`，可按实际需求给 `version`；不改部署托管字段。配置变化不更新当前 Turn；`changed:true` 后结束当前 Turn，让 Host 自动续接，再检查能力。`development_prepare` 是 Python 私有环境工具，不是 Rust 初始化器。

使用共享只读 rustup/Cargo 工具链与缓存，Host 提供 Session 可写 `CARGO_HOME`；不要复制编译器、读取 Cargo 凭据或绕过沙箱。检查项目 `Cargo.toml`、`Cargo.lock`、`rust-toolchain.toml` 和现有构建说明，先查已装版本。只有确需且当前已获联网授权时，才直接执行 `rustup toolchain install <版本>`；禁止 self-update、修改共享 default 或自造工具链入口。

Pilot 示例（`anchor_run` 没有 `cwd`，子目录用已授权 shell）：

```json
{"command":["sh","-c","cd project && cargo --version && rustc --version && cargo test --locked"],"timeout_seconds":600}
```

按项目实际入口先跑相关测试，再必要的 crate/workspace 测试、`cargo clippy --locked` 与 `cargo fmt --check`。已有锁文件优先 `--locked`；离线缺包/缺工具链如实报告，不把退出失败当通过。不要为跑测试启动真实服务或擅自下载依赖。Plugin 交付保留源码/锁/说明与稳定 `bin/` 入口，后续审查按 Plugin Skill。

AgentNode 只能使用当前沙箱实际登记的工具与权限；没有 Pilot 的 `platform_status` / `runtime_prepare` / `development_prepare` 管理工具。缺环境交 Pilot/用户处理。Pilot 命令最高 600 秒；AgentNode 默认 30 秒且上限由部署决定，不照抄 600。工作区可跨轮保留，完成时以 `final_result.files` 显式选择交付，不自动交付 `target/` 或 `.git`。
