# 文档目录

按当前任务选择入口，沿链接读取需要的契约；不用通读整张目录。实现行为、产品方向、验收状态分别维护，历史材料只用于查证。

## 从这里开始

| 要做什么 | 入口 |
| --- | --- |
| 安装、启动或使用 Anchor | [使用指南](usage.md)、[上手示例](../examples/README.md) |
| 修改代码、运行检查、维护文档 | [开发指南](development.md) 与 [开发约定](../AGENTS.md) |
| 确定职责、事实所有者与执行边界 | [当前架构](architecture.md)；产品目标另见 [产品架构](product-architecture.md) |
| 收敛跨入口重复规则与交互 | [共同事实与实现收敛](shared-facts-convergence.md) |
| 判断什么验过、还有什么没关闭 | [当前验收与待办](pilot-development-plan.md) |

## 按任务查契约

| 任务 | 当前文档 |
| --- | --- |
| Pilot 初始化、Python/Rust 开发、候选自测与交付 | [Pilot 开发](pilot-development.md) |
| Plugin 清单、安装、权限与按需披露 | [Plugin](plugins.md) |
| 共享包、只读视图、私有开发环境与回收 | [依赖标准](plugin-dependencies.md) |
| 联网默认、主动/依赖启用与禁止 | [任务联网](network-plugin.md) |
| Graph 编译、模块与组合 | [作者契约](rust-graph-authoring-contract.md)、[组合契约](graph-composition-design.md)、[Kernel 模块](rust-graph-module-design.md) |
| HTTP API 与 Web 投影 | [API 契约](rust-frontend-api-contract.md) |
| Host 的 Run 访问边界与迁移 | [Run 访问门面与围栏](run-access-facade.md) |
| Host 依赖方向与重复规则收敛（进行中） | [任务书](host-convergence-task.md) |
| Runtime 或发行验证 | [确定性回归](runtime-contract-tests.md)、[候选回归](rust-production-candidate.md) |
| 部署与运行预检 | [部署](rust-production-deployment.md) |
| 渠道与业务 Graph | [企业微信](wecom-assistant.md)、[RSI](rsi.md)、[周报](weekly-work-report.md) |
| RSI 反馈轮与 Agent 会话延续 | [当前契约与验收边界](rsi-agent-session-continuity.md) |
| 理解 Goose 接入取舍 | [接入决定](goose-runtime-migration.md) |
| 宪法迁移最终说明与后续决定 | [最终说明](anchor-runtime-migration-overview.md)、[M5 最终测量与待决清单](../experiments/constitution-host-preflight/final/measurement.md)、[规则审计](../experiments/constitution-host-preflight/final/rules-audit.md)、[测试登记](../experiments/constitution-host-preflight/final/test-mapping-summary.md)；历史实施与门槛见 [任务书](constitution-migration-task.md)、[工作记录](constitution-refactor-log.md)、[M4 交回](../experiments/constitution-host-preflight/m4/verification.md)、[Run 门面](run-access-facade.md) |
| 后续 MCP 按需连接 | [未实施路线](goose-native-capability-reuse-plan.md) |
| Harness 自身的分析与优化（后续主方向） | [数据中枢方向](harness-data-hub.md)、[总体架构](harness-data-hub-architecture.md)、[任务书](harness-data-hub-tasks.md) |

依赖版本以 [Cargo workspace](../rust/Cargo.toml)、[Cargo.lock](../rust/Cargo.lock) 和 [前端配置](../apps/web/package.json) 为准。当前文档直接更新，完成计划退出活跃入口；结论链接到相应提交或验收产物，记录方法见开发指南。

[历史归档](archive/README.md) 保存已完成计划、旧执行器与验收过程。旧命令、部署状态和“下一步”仅代表当时背景；归档不改变用户数据，也不要求新任务读取历史全文。
