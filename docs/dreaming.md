# Dreaming 退役说明

2026-11-15：已按用户要求完成复杂 Dreaming 专用系统的源码删除及本地整合验收，该事项退出活跃待办。本文不再维护内置自动改进的活跃设计、操作指南或后续交付计划，也不建立替代系统。

## 源码范围与部署边界

本轮删除范围是 Host 的 `application/dreaming`、专用 HTTP API、启动自动安装、周期协调、生产观察，以及 `execution/frozen` 专用实验执行、局部上下文分支实验适配和自身 meta 机制；同时删除 Web Dreaming 页、发行包的 Dreaming 工厂 Graph 和专用 `dreaming` 质量包。Facts/Eval/Hub 中的 Dreaming 专用 meta 及依赖 `kernel-dreaming` 生命周期的实验协议一并删除，不作为当前能力或继续待办。

`Kernel::submit`、Hub 全局分析/窗口/安全视图格式及读侧处理、普通 Graph/Runner/Goose、Artifact，以及独立 ordinary/input-discovery 质量工具保留；不保留原 Host/Eval 安全视图导出链，也不新建导出 API。Goose 自身的会话、上下文和 compaction、普通 Run 的恢复与权限边界不变；删除的是专用实验接入，不是普通 Goose 上下文能力。当前边界见[架构](architecture.md#事实分析与独立评分)与[数据中枢](harness-data-hub.md)。

历史 Run 元数据及原 `dreaming`/`experiment` 身份保留、可读，用于查证与来源隔离；它们不是普通任务的启动或恢复授权，不能按普通 Run 续跑，也不因删掉旧协议而改写为生产样本。原 handovers 事实的只读围栏保留，继续核对未完成交接与既有回复等待，不恢复新替换或接管机制。

当前源码以历史创建回执围栏隔离旧 Dreaming system schedule，不得因专用调度删除而降级为普通 admission。自动 retention 跳过历史 `dreaming`/`experiment` Run，保护其 Artifact 及 `dreaming-*` 专用节点交付；退役不触发历史清理或迁移。

**本地 8077/5173 已按用户授权切换到退役后的 format 5 发行。** 旧 Host 已停止，旧 release 文件、Graph、Run 与实验历史保留；`/dreaming` 返回 404，旧 Dreaming 图无法通过新 Host 的执行接纳。11/15 切换时 03:00 计划保持暂停，未清理或迁移旧数据；11/16 按后续授权通过既有 API 删除该停用计划，并只清理无人使用的构建缓存，业务与实验历史仍保留。没有恢复 meta 或 Dreaming 调度；切换与后续维护分别见上线记录（本机原件，未入库：`experiments/dreaming-removal-20261115/rollout-20261115/README.md`）、维护记录（本机原件，未入库：`experiments/local-maintenance-20261116/README.md`）。

## 本轮删除验收

workspace 全目标检查、Host 及相关 crate 回归、六 crate 严格 Clippy、51 份 Host 改动文件定向格式检查均通过。Web 最终复跑的 50 文件/401 测试及构建通过，仅既有大 chunk 告警；此前 5 项 mock 浏览器回归单列，不等同原生或真实服务 E2E。固定 Goose 配本地确定性 Provider 的 3 项原生测试通过、留下 4 份回执，覆盖工具/事实、未知效果恢复不重放与普通常驻助手自动恢复；0 真实模型请求、0 生产数据。

全仓格式检查仍因 4 份接手前/并行文件的格式差异未通过，未修改它们；早轮失败保留在证据中，最终所列默认全量回归复跑通过，不改写成从无失败。源码删除验收与后续发行切换分开记账：后者补验了完整 format 5 归档、三次 source-free 启动、精确发行 Host 的原生执行/恢复，并核对本地现有 Graph 和历史。10k、DEB/RPM 安装及真实模型业务重跑未执行；静态接纳、渠道认证和历史保留不证明业务端到端成功或 Dreaming 收益。结论见[统一台账](pilot-development-plan.md#当前验收结论)，原始删除回归见[删除验收](../experiments/dreaming-removal-20261115/README.md)。

## 历史证据入口

原始实验材料按[本机证据规则](development.md#文档归属)保留，未选材料不入库；历史归档保留原结论。以下只用于查证当时实现、验证与部署，不是当前能力、待办或运行指令：

| 主题 | 历史证据 |
| --- | --- |
| 基础接入与受限事实窗口 | 基础接入（本机原件，未入库：`experiments/dreaming-foundation/README.md`）、窗口验收（本机原件，未入库：`experiments/dreaming-window/README.md`） |
| 候选、独立评估与确定性闭环 | 候选准备（本机原件，未入库：`experiments/dreaming-candidates/README.md`）、实验闭环（本机原件，未入库：`experiments/dreaming-experiments/README.md`） |
| Plugin 换代与自递归边界 | Plugin 实验（本机原件，未入库：`experiments/dreaming-plugin-evolution/README.md`）、meta 记录（本机原件，未入库：`experiments/dreaming-meta-recursion/README.md`） |
| 共享 Hub 协议及跨周期分析 | 源码收敛（本机原件，未入库：`experiments/dreaming-consolidation/README.md`）、[中枢联合验收](../experiments/harness-hub-v3/README.md) |
| 原发行与真实 Provider 试跑 | 无预算发行（本机原件，未入库：`experiments/dreaming-no-budget/rollout-20261114/README.md`）、原上线试跑（本机原件，未入库：`experiments/dreaming-live/README.md`） |
| 最后一次旧 release 上线与调查 | 2026-11-15 上线及试跑（本机原件，未入库：`experiments/dreaming-context-branch/rollout-20261115/README.md`）、旧周期停止记录（本机原件，未入库：`experiments/dreaming-investigation/nightly-20261115/README.md`） |
| 局部分支与晨报负结果 | [分支退役及历史索引](dreaming-context-branch-plan.md) |

历史确定性通过不等于真实生产收益。原生产分析的三项提议缺上下文范围和适用局部质量包，未创建实验或发布；后续晨报候选也被淘汰。真实正向发布、持续净收益及完整无人值守自递归未获证明；退役不改变这些结论。
