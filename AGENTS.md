# Anchor 开发约定

## 每次开工先读

- `docs/pilot-development-plan.md` — Pilot 开发唯一台账：阶段、冻结契约、A01–A14 验收矩阵、推进记录。
- `docs/architecture.md` — 当前实现真相。
- `docs/product-architecture.md` — 目标设计；标注为目标设计的部分尚未实现。

## 工作区状态

这个工作区长期存在大量未提交改动，横跨多条并行工作流（Pilot、AgentNode 迁移、Plugin、文档重构）。
2026-09-26 的状态快照在分支 `checkpoint/pilot-handoff`，`master` 停在 `520c181`。

- 不 `git reset`、不 `git checkout --`、不 `git clean`。
- 不把无关改动自动纳入提交；一次提交只包含你实际改动的路径。
- 撤销快照：`git branch -D checkpoint/pilot-handoff`，`master` 不受影响。

## 硬约束

- `src/anchor/serve.py` 不得在模块级 import pydantic_ai，`tests/test_node_controlflow.py` 会拦。
- 未经真实 provider 端到端跑通的事，不写进文档说已完成。
- 每完成一步，往 `docs/pilot-development-plan.md` 的推进记录追加一条，并同步更新验收矩阵对应项。

## 验证

- 全量：`./.venv/bin/python -m pytest -q -n 8 --dist worksteal`（约 1 分 40 秒）
- 相关子集：`./.venv/bin/python -m pytest tests/test_pilot_turns.py -q`
- 前端：`npm --prefix apps/web test`、`npm --prefix apps/web run test:e2e`、`npm --prefix apps/web run build`
- `--dist worksteal` 必须带：默认的 `load` 会把 `tests/test_examples.py` 的四个慢用例派到同一个 worker，退化到 3 分钟。
- 开发时只跑相关子集，一个阶段做完再跑一次全量，不要每改一点跑一次。

## 已知未验证项

P2 审批改造（Pydantic deferred tools）之后没有用真实 DeepSeek 端到端跑过一次；浏览器 E2E 用的是 mock SSE 流。
继续往上叠功能之前先补这一项。
