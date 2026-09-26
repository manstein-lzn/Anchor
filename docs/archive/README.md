# 历史归档

归档日期：2026-09-25。这里保留旧方案与实验依据，**不定义当前实现，也不构成待办**。现行文档从 [项目 README](../../README.md) 进入，唯一升级方向由 [Plugin 设计](../plugins.md) 维护。

原根目录的 25 份架构、讨论、计划与结果文档整体移入这里，正文保留，只增加归档提示。文中的状态、测试数、路径、命令、读取顺序和实施指令以当时为背景；旧的“下一步”不应继续执行。归档正文中的根目录路径及已删除模块名称是历史记录，不是当前可用入口。

## 已替代的架构与接口

这些材料主要描述删除于 `9a7497f` 的旧运行时。原文中“product intent may still be a backlog”等表述已经失效；不再从中自动恢复产品规划。

- [ARCHITECTURE.md](ARCHITECTURE.md)
- [WORKSPACE.md](WORKSPACE.md)
- [CONTENT_COMMIT_PROTOCOL.md](CONTENT_COMMIT_PROTOCOL.md)
- [RUN_ADMISSION.md](RUN_ADMISSION.md)
- [AGENT_SURFACE.md](AGENT_SURFACE.md)

## 历史决策与讨论

- [DECISIONS.md](DECISIONS.md)
- [OPEN.md](OPEN.md)
- [ANCHOR_CORE_MODEL_DISCUSSION_DRAFT.md](ANCHOR_CORE_MODEL_DISCUSSION_DRAFT.md)

`DECISIONS.md` 同时保留已删除设计和仍影响当前代码的决策，不能把整份文件视为现行规范。当前代码边界已经整理到 [当前架构](../architecture.md)。

`OPEN.md` 不再是活跃待办。核心模型草案中把 Op 泛化为所有能力、外部交互和长期协作的方向已被收窄；当前能力、工具与 OpNode 的边界以能力库设计为准。

## Agent Node 调研、迁移与验收

以下文件保存 PydanticAI/harness 接入期间的计划、实验、故障窗口和主验收。运行内核迁移已经完成，不重新执行阶段计划。历史依赖版本是当时的验证环境，不代替当前项目依赖声明。

- [AGENT_HARNESS_RESEARCH.md](AGENT_HARNESS_RESEARCH.md)
- [AGENT_HARNESS_RESEARCH_REPORT.md](AGENT_HARNESS_RESEARCH_REPORT.md)
- [AGENT_NODE_G2_RESULT.md](AGENT_NODE_G2_RESULT.md)
- [AGENT_NODE_MIGRATION_ACCEPTANCE.md](AGENT_NODE_MIGRATION_ACCEPTANCE.md)
- [AGENT_NODE_PLAN_01.md](AGENT_NODE_PLAN_01.md)
- [AGENT_NODE_PLAN_01_ACCEPTANCE.md](AGENT_NODE_PLAN_01_ACCEPTANCE.md)
- [AGENT_NODE_PLAN_01_RESULT.md](AGENT_NODE_PLAN_01_RESULT.md)
- [AGENT_NODE_PLAN_02.md](AGENT_NODE_PLAN_02.md)
- [AGENT_NODE_PLAN_02_ACCEPTANCE.md](AGENT_NODE_PLAN_02_ACCEPTANCE.md)
- [AGENT_NODE_PLAN_02_RESULT.md](AGENT_NODE_PLAN_02_RESULT.md)
- [AGENT_NODE_PLAN_03.md](AGENT_NODE_PLAN_03.md)
- [AGENT_NODE_PLAN_03_ACCEPTANCE.md](AGENT_NODE_PLAN_03_ACCEPTANCE.md)
- [AGENT_NODE_PLAN_03_RESULT.md](AGENT_NODE_PLAN_03_RESULT.md)
- [AGENT_NODE_PLAN_G2.md](AGENT_NODE_PLAN_G2.md)
- [AGENT_NODE_VALIDATION_PLAN.md](AGENT_NODE_VALIDATION_PLAN.md)
- [AGENT_NODE_VALIDATION_RESULT.md](AGENT_NODE_VALIDATION_RESULT.md)
- [ARCHITECTURE_AGENT_NODE_MIGRATION.md](ARCHITECTURE_AGENT_NODE_MIGRATION.md)

## 历史示例

[旧图示例](../../examples/graphs/previous/README.md) 保留原有位置，不兼容当前运行时。当前可运行图位于 [examples/graphs](../../examples/graphs)，主要研究示例为 [deep-academic-research.json](../../examples/graphs/deep-academic-research.json)。

## 查证与维护

需要理解一个历史故障或决策时，按文件与关联提交查证。正文不随当前功能持续改写；对当前行为的纠正写入现行文档。归档不是功能承诺，也不是重新引入图发布版本、运行数据库或多服务架构的理由。
