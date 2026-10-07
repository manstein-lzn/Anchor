# Goose 与 Anchor 产品愿景对齐调研

日期：2026-10-06。仅调研，不切换生产后端或修改执行语义。

## 核心问题

若 Anchor 将 AgentNode 和 Pilot 的底层执行统一迁移到 Goose，并以 ACP 隔离依赖，能否保留产品愿景；哪些能力应由 Goose 承接，哪些仍由 Anchor 拥有，哪些是必须先验收的缺口？

## 工作划分

1. 主 Agent：核对本仓库产品目标、当前实现与 A112 spike 证据，整理产品能力归属及迁移判断；这是当前关键路径。
2. 子 Agent（runtime）：核查官方 Goose 的会话、上下文压缩、恢复、取消、提问、usage/预算及 ACP 可用接口。区分公开支持、代码行为和未经验证的故障语义，保存 `findings_runtime.md`。
3. 子 Agent（boundary）：核查官方 Goose/ACP 的扩展与 MCP、权限、工具隔离、进程部署、模型/媒体与独立运行边界，保存 `findings_boundary.md`。不重复调研恢复和压缩。

## 信息与预算

- 主事实来自 `docs/product-architecture.md`、`docs/architecture.md`、`docs/pilot-development-plan.md` 和 `docs/goose-acp-spike.md`。
- 外部技术事实只使用官方文档、协议与源代码；优先核对实际测试的 Goose v1.53.0，若参考其他版本须明确区分。
- 每个子任务最多 4 次搜索，可直接打开必要的官方页面；不运行真实模型、不安装依赖、不改业务代码。
- 每个子 Agent 只写自己的一份 findings 文件，使用 apply_patch，注明来源 URL、版本、局限和建议验收项。

## 综合方式

按“产品要求 / 事实所有者 / Goose 原生或接入支持 / 未验收与阻塞项”对齐，回答技术可行性与生产可替代性，不将 ACP 接入通过等同完整产品验收，也不将进程依赖隔离说成没有依赖。
