# 文档目录

| 文档 | 用途 |
| --- | --- |
| [使用指南](usage.md) | 本地启动、配置、Graph、Run、Session 与工具 |
| [开发指南](development.md) | 代码归属、工作节奏与验证命令 |
| [当前架构](architecture.md) | Rust 实现、事实所有者和当前边界 |
| [产品与系统架构](product-architecture.md) | 产品目标、执行不变量和未冻结需求 |
| [开发台账](pilot-development-plan.md) | 实际交付、验收证据和待验收项 |
| [Plugin](plugins.md) | 清单、Skill、资源、MCP 和权限 |
| [小型 Graph 回归](runtime-contract-tests.md) | 确定性运行时检查与证据 |
| [生产候选回归](rust-production-candidate.md) | release Host/Web/Goose 包的隔离执行 |
| [生产部署](rust-production-deployment.md) | source-free 包、环境与 systemd |
| [切换准备](rust-production-cutover.md) | 旧数据盘点、独立根与备份索引 |
| [Goose 接入决定](goose-runtime-migration.md) | ACP/MCP 的边界与取舍 |
| [Graph 作者契约](rust-graph-authoring-contract.md) | 用户 JSON 与执行 IR |
| [Graph 组合](graph-composition-design.md) | 内联子图、独立调用与 fanout/join |
| [Web API 契约](rust-frontend-api-contract.md) | 资源 API 与前端投影 |
| [企业微信](wecom-assistant.md) | 原生网关、用户授权与 Session |
| [RSI](rsi.md) | 证据、公开生态审查和改进提案 |
| [本机工作周报](weekly-work-report.md) | 采集、评审和发布门禁 |

当前依赖以 [Cargo workspace](../rust/Cargo.toml)、[Cargo.lock](../rust/Cargo.lock) 和 [前端配置](../apps/web/package.json) 为准。进度与测试数量只以台账中的具体执行记录为证据。

[历史归档](archive/README.md) 保留早期设计、旧执行器和迁移调研，供追溯使用；其中的旧命令、源码链接和待办不适用于当前代码。删除旧实现不会转换或清理用户的旧工作记录。
