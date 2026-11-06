# Anchor Dev

这是开发指导目录，稳定 Plugin ID 为 `anchor-steward`。只读与当前任务相关的一项 Skill，不一次读取全部正文；需要时再补读。

| 任务 | Skill 路径 |
| --- | --- |
| 适配、审查、自测、交付 Plugin | `skills/plugin-authoring/SKILL.md` |
| 编写 Graph、模型引用与文件交接 | `skills/graph-authoring/SKILL.md` |
| Rust 工具链、构建与测试 | `skills/rust-development/SKILL.md` |
| Python 私有环境与依赖 | `skills/python-development/SKILL.md` |
| pixi 项目环境与锁文件 | `skills/pixi-development/SKILL.md` |
| Git 仓库协作与独立 Git Plugin | `skills/git-workflow/SKILL.md` |

读取示例：`anchor_plugin_read({"plugin":"anchor-steward","section":"skill","path":"skills/graph-authoring/SKILL.md"})`。先查实际工具 schema；Skill 不替代宿主工具定义。文本分页按 `next_offset` 继续，并用返回的摘要作为 `digest`。

本 Plugin 无 MCP、依赖安装或初始化副作用，也不自动启用联网或 Git。开关只改变后续 Turn 的指导披露，不撤销已授权命令、清空 workspace 或改变 runtime 事实。Pilot 管理工具与 AgentNode 沙箱能力不同，以当前调用的实际披露为准。
