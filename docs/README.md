# 文档目录

按角色和当前任务找入口，不用通读目录。一个主题只维护一个当前契约；验收与待办归统一台账，过程和原始方案归历史。

## 使用 Anchor

只需从[使用指南](usage.md)开始：Ubuntu 原生包安装 → 配置模型 → 把 Graph/Plugin 文件交给 Pilot → 日常运行与排错。无需先读开发或架构文档。

## 编写 Graph 和 Plugin

| 要做什么 | 入口 |
| --- | --- |
| 编写、校验、交付及组合 Graph JSON | [Graph 作者契约](rust-graph-authoring-contract.md) |
| Plugin 清单、安装、权限、凭据与依赖 | [Plugin](plugins.md)、[系统插件接入](plugins.md#开发系统插件)、[依赖标准](plugin-dependencies.md) |
| Agent 按需调用/创建 Graph、设置计划与处理完成结果 | [Graph 任务](plugins.md#graph-任务)、[结果输入契约](architecture.md#按需-graph-任务与结果输入) |
| 让 Pilot 开发、准备环境、自测和交付 | [Pilot 开发](pilot-development.md)；联网规则归 [Plugin](plugins.md#pilot-任务联网) |
| 找示例或配置业务能力 | [上手示例](../examples/README.md)、[企业微信](wecom-assistant.md)、[飞书](feishu-assistant.md)、[RSI](rsi.md)、[周报](weekly-work-report.md) |

## 开发和运维

| 任务 | 当前文档 |
| --- | --- |
| 源码启动、修改代码、运行检查、维护文档 | [开发指南](development.md)、[开发约定](../AGENTS.md) |
| 确定职责、事实所有者与执行边界 | [当前架构](architecture.md)，沿其链接查专项契约 |
| HTTP API 与 Web 投影 | [API 契约](rust-frontend-api-contract.md) |
| 最高原则及其设计推导 | [宪法](anchor-constitution.md)，实现与存储边界仍以当前架构为准 |
| Run 访问门面与依赖围栏 | [Run 访问边界](run-access-facade.md)、[Host 依赖方向](architecture.md#执行链与所有权) |
| Runtime 或发行候选验证 | [统一回归指南](runtime-contract-tests.md) |
| 原生包、systemd、运行预检与操作员参数 | [部署指南](rust-production-deployment.md)、[发行 builder](../rust/anchor-distribution/README.md) |
| 判断什么验过、还有什么没关闭 | [当前验收与待办](pilot-development-plan.md) |

依赖版本以 [Cargo workspace](../rust/Cargo.toml)、[Cargo.lock](../rust/Cargo.lock) 和[前端配置](../apps/web/package.json)为准。当前文档直接替换过期描述；过程和原始输出不塞进使用指南，记录归属见[开发指南](development.md#文档归属)。

## 方向与历史查证

方向不等于已实现，历史任务书也不是待办。仅在相关任务需要时读取：

| 主题 | 入口与状态 |
| --- | --- |
| 产品方向与不变量 | [产品架构](product-architecture.md) |
| Harness 事实记录、分析与优化 | [数据中枢](harness-data-hub.md)：已实现契约与未实施方向分开标注；状态仍归验收台账 |
| 内置 Dreaming Graph 与自动 Harness 改进 | [Dreaming 开发文档](dreaming.md)：后续开发的统一依据，含数据/接口、实验、自动应用/撤回、自递归、分片交付与验收；待实施，不代表当前能力 |
| 常驻工程 Graph | [项目现场与执行边界](project-maintainer-graph-design.md)，分片实现中 |
| MCP 按需连接、RSI 闭环及其他未关闭事项 | [统一待办](pilot-development-plan.md#正在收口)，列出不等于已授权实施 |
| Kernel 冻结与开发检查 | [开发指南](development.md)，不再读取已结束的迁移任务书开工 |

[历史归档](archive/README.md)收纳已完成或被取代的任务、旧执行器和验收过程，保留原结论与证据；旧命令、部署状态和“下一步”只代表当时背景，不作为开工必读。
