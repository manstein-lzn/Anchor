# 历史归档

这里保留旧方案与实验依据，**不定义当前实现，也不构成待办**。现行入口见 [文档目录](../README.md)，产品方向见 [产品架构](../product-architecture.md)，未关闭事项见 [当前验收与待办](../pilot-development-plan.md)。按问题查具体记录，不把归档列为开工必读。

## 2026-10-20 Runtime 迁移与回归收尾

以下任务已结束或被后续迁移取代，正文与原结论保留，只增加归档提示并修正链接。仍有效的契约见[当前架构](../architecture.md)、[Run 访问边界](../run-access-facade.md)和[Runtime 回归](../runtime-contract-tests.md)；当前冻结基点归[开发指南](../development.md)，迁移经过见下节历史说明。

- [全量迁移任务书](runtime-migration-2026-10-20/constitution-migration-task.md)、[合入须知](runtime-migration-2026-10-20/constitution-merge-task.md)与[工作记录](runtime-migration-2026-10-20/constitution-refactor-log.md)：M1–M5 的实施、审查与证据。
- [H2/P 探针](runtime-migration-2026-10-20/constitution-h2-task.md)、[R 读模型探针](runtime-migration-2026-10-20/constitution-r-task.md)与[R2 状态词汇探针](runtime-migration-2026-10-20/constitution-r2-task.md)：保留各自结论，R2 当时仍是停止、未验收，不能视为后续迁移的通过证明。
- [Goose 基线任务](runtime-migration-2026-10-20/goose-baseline-task.md)与[Trace 原子写入任务](runtime-migration-2026-10-20/trace-evidence-atomic-task.md)：已完成的回归修复及其历史门槛。
- [Run 访问门面任务](runtime-migration-2026-10-20/run-access-facade-task.md)：已完成的 F0–F2 拆迁过程，不再作为实施指令。

## 2026-11-04 设计与过程记录收敛

这些文件保留当时的推导、授权范围、实施要求和证据，不再独立维护现状。仍有效的约束已并入当前架构与专项契约，未关闭事项归[统一台账](../pilot-development-plan.md#正在收口)；当时的冻结基点、数量、命令和“下一步”不自动生效。

- [运行时重建最终说明](design-records-2026-11-04/anchor-runtime-migration-overview.md)、[早期 Host 接入计划](design-records-2026-11-04/constitution-host-integration-plan.md)、[预检修订决定](design-records-2026-11-04/constitution-preflight-decision.md)：迁移已结束；Kernel 提交、存储与权限约束归当前架构。
- [Host 收敛任务](design-records-2026-11-04/host-convergence-task.md)：H0–H5 已验收结束；遗留命名欠债以当前围栏为准，后续接线未自动获授权。
- [以提交为细腰](design-records-2026-11-04/submission-narrow-waist.md)、[W 系列收敛方案](design-records-2026-11-04/convergence-development-plan.md)：保留讨论与旧建议；W1 已完成，其他工作包不因列出而获授权，默认预算建议已由后续用户决定取代。
- [Goose 按需激活路线](design-records-2026-11-04/goose-native-capability-reuse-plan.md)：公开接口实验与路线依据；懒连接仍未实现，当前授权 inventory 契约不变。
- [Harness 数据中枢任务书](design-records-2026-11-04/harness-data-hub-tasks.md)：P0–P7 的实验与审查记录；P7 内容验收未过、G4/G5 未通过，不能由前期阶段结果补签。

## 2026-10-13 开发计划与验收历史

以下正文保留，归档时只增加历史提示并调整相对链接。既有 A 编号、失败经过、测试和部署身份不改写；当时的“当前”不代表最新实现。

- [原开发台账](development-2026-10-13/pilot-development-plan.md)：2026-09-26 至 2026-10-13 的阶段、验收矩阵和推进记录。
- [Plugin authoring 计划](development-2026-10-13/pilot-plugin-authoring-development-plan.md) 与 [初始化计划](development-2026-10-13/pilot-initialization-and-development-plan.md)：S1–S4/I1–I2 的实施背景，未关闭范围已转入当前待办。
- [开发环境原方案](development-2026-10-13/development-environment-freedom-plan.md) 与 [修订方案](development-2026-10-13/development-environment-freedom-plan-revised.md)：原稿由修订稿替代，当前工具行为见 [Pilot 开发](../pilot-development.md)。
- [渐进披露实施计划](development-2026-10-13/plugin-progressive-disclosure-plan.md)：A222/A224 的计划及分阶段验收；懒连接仍按 [活跃路线](design-records-2026-11-04/goose-native-capability-reuse-plan.md) 单独判断。
- [2026-10-13 验收证据](development-2026-10-13/evidence/)：开发环境与 Plugin 披露的脚本、概要和研究材料归档于此，不属于运行时或发行包。
- [Goose 迁移计划](development-2026-10-13/goose-runtime-migration.md) 与 [Graph 组合设计](development-2026-10-13/graph-composition-design.md)：迁移阶段、早期原型和执行器背景；有效决定与当前契约分别留在现行文档。

## Rust 与 Goose 收敛前的历史

2026-10-08 清理维护树时，将旧宿主、框架比较、迁移计划和主文档快照收纳到 `pre-goose/`，保留当时证据，不继续维护旧命令。Goose 初始 ACP/vision 调研移入 `goose-research/`。

- [旧主架构快照](pre-goose/architecture.md) 与 [旧产品架构快照](pre-goose/product-architecture.md)
- [旧混合宿主说明](pre-goose/runner-host.md)
- [旧平台目标](pre-goose/rust-platform-target.md)、[旧平台计划](pre-goose/rust-platform-development-plan.md)、[旧 Runtime 计划](pre-goose/rust-rig-migration-plan.md)
- [框架能力核查](pre-goose/rig-harness-capability-audit.md)、[候选调研](pre-goose/rust-harness-candidate-research.md)、[CodeMode 调研](pre-goose/codemode-evaluation.md)
- [Pilot 体验核查](pre-goose/pilot-experience-audit.md)、[P2 验收](pre-goose/pilot-p2-acceptance-review.md)
- [Goose ACP 初步结论](goose-research/acp/initial_conclusions.md)、[初始接入验证](goose-research/acp/spike.md) 与 [vision 评估](goose-research/vision/assessment.md)

已删除实现的路径和命令在历史正文中仍有参考意义；需要源码时查对应 Git 历史，而不是在当前开发环境重新安装旧依赖。旧运行记录不因文档整理被改写成 Goose 证据。

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

[旧图示例](legacy-examples/README.md) 已移入本归档，不兼容当前运行时。当前可运行图位于 [examples/graphs](../../examples/graphs)，主要研究示例为 [deep-academic-research.json](../../examples/graphs/deep-academic-research.json)。

## 查证与维护

需要理解一个历史故障或决策时，按文件与关联提交查证。正文不随当前功能持续改写；对当前行为的纠正写入现行文档。归档不是功能承诺，也不是重新引入图发布版本、运行数据库或多服务架构的理由。
