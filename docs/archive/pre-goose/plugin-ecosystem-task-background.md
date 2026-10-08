# Anchor 社区 Plugin 生态兼容：独立任务背景

> 历史快照：保留当时设计与证据，不代表当前实现或待办。当前入口见 [文档目录](../../README.md)。

2026-09-28。本文用于把一次产品讨论交接给新的 Agent，帮助理解背景、用户意图和工程起点。它是一份任务说明，不是冻结的技术设计或验收契约。实现形态、研究方式、工作拆分与推进节奏均留给接手者结合实际情况探索。

## 为什么提出这个任务

Anchor 已经具备 Graph、AgentNode、OpNode、Plugin、Run 和 Pilot 等基础能力。用户现在关注的是扩展 Plugin 矩阵，让系统能处理更多种类的实际工作。

用户不希望靠自己逐个手写 Plugin 来积累能力。Claude、Codex 及其社区已有大量方法、脚本、工具和能力组合，用户希望 Anchor 能直接利用这些生态，并让这些资源成为在 Anchor 中可使用、可管理、可组合的能力。

这里的“我们自己的 Plugin”强调能力进入 Anchor 的使用体验与运行体系，不表示希望重新编写上游内容或长期维护大量手工改写的副本。

## 用户真正希望达到的目标

用户在讨论中明确表达：

> “理论上来说，我希望的是 plugin 能够完全兼容 openai 或者 anthropic 的 plugin，我们应该没有道理做不到。”

这一方向面向 OpenAI/Codex 与 Anthropic/Claude 两个生态。用户期待的是原生资源能够在 Anchor 中发挥作用，兼容工作主要由 Anchor 承担，用户不必为了迁入一种能力而先学会制作 Anchor 专属 Plugin。

从使用者角度看，理想体验是发现一个有用的社区插件后，能够把它引入 Anchor，用在自己的任务或工作流里。插件携带的方法、资源以及所依赖的执行能力能够相互配合，最终完成工作。

“完全兼容”表达的是产品方向。两个生态具体有哪些插件机制、各机制依赖哪些宿主行为，以及怎样在 Anchor 中实现和证明兼容，仍有待研究。此前讨论没有完成这项技术核查，也没有得出完整兼容已经实现或已被证明可行的结论。

## 此前讨论走过的思路

最初的建议是筛选社区 Skill，保留说明和脚本，再包装成 Anchor 现有 Plugin。曾举过薄适配入口、来源记录、固定上游版本以及先选少量代表资源等例子。

用户随后指出，这种思路收窄了目标：他希望 Anchor 兼容社区插件本身，而不只是挑选其中容易迁移的内容。

讨论因此转向“Anchor 需要提供什么宿主能力，才能运行这些生态的插件”。原生格式加载、工具接入、MCP、hooks、命令、子代理、配置和生命周期等被提及，作为理解问题的线索。

这些线索没有形成既定方案。上文提到的包装目录、样本数量、适配层、组件分类或实施顺序都只是讨论中的设想，没有被用户确定为本任务的要求。接手者可以重新判断问题、比较不同路线，并提出更合适的实现方式。

## Anchor 的工程起点

Anchor 是本地优先的 Agent 工作流系统。Graph 组织任务，AgentNode 负责模型理解与工具执行，OpNode 执行确定性程序；Run 保存执行状态、节点工作区、对话轨迹和产物。节点成果通过 Git commit 对应的只读快照交给下游。Pilot 是用户通过自然语言查看和管理工作流的入口，Session 承载长期对话。

当前模型执行使用 PydanticAI，记录和恢复接入 PydanticAI Harness。节点命令通过 Bubblewrap 沙箱执行。网络、文件和工具挂载在运行时处理。

现有 Plugin 机制比较直接：

- AgentNode 在 Graph 中显式引用 Plugin 标识。
- 历史能力库曾用根 `plugin.json` 提供名称和描述，并用 `instructions.md` 提供完整说明；当前 Skill 布局以清单声明的 `skills/<skill>/SKILL.md` 为说明来源。
- Agent 初始获得简短目录，按需读取完整说明；Plugin 资源只读挂载到节点。
- 工具通过独立登记的执行入口和环境提供，多个 Plugin 可以复用工具。
- Run 保存资源绑定摘要；当前实现会在有关执行和恢复边界核对资源变化。
- WebUI 已有 Plugin 选择、说明查看和运行绑定记录展示。

本次讨论时，仓库 `plugins/` 下只有 `academic-research` 这一份随代码维护的 Plugin。当前机制已提供能力挂载的基础，但尚未实现这里讨论的两个社区生态的完整兼容。

这些是接手时的定位信息，不是未来设计必须保持的形态。工程仍在演进，实际状态以接手时的代码和开发记录为准。

## 可供探索的问题

怎样让用户真正使用社区能力，比预先选择某种目录结构更接近这个任务的核心。研究中可能涉及以下问题；它们用于帮助打开思路，不是完整清单或预设工作包：

- 两个生态分别怎样定义 Plugin、Skill、工具和其他组件？哪些是共享规范，哪些是宿主扩展？
- 一个真实插件运行时会依赖哪些工具名称、上下文、事件、目录、会话或交互行为？
- Anchor 已有能力和固定版本框架能够承接多少？有哪些公开接口或现成实现值得复用？
- 社区插件与 Graph、AgentNode、Pilot 的关系怎样组织，才能让用户自然地发现、选择和使用能力？
- 插件中的脚本、工具环境、外部服务和配置怎样进入实际执行链？
- 上游资源怎样保持来源可追溯，并处理许可证、变化和更新？
- 怎样通过实际使用判断兼容程度，识别“内容被读到”和“能力真正可用”的差别？

这些问题的答案可能影响架构，也可能发现现有机制已经足够。本文不预判结果，不限定技术栈、格式、版本、插件范围、交付批次或兼容实现方式。

## 资料与代码导航

仓库背景：

- [开发约定](../../../AGENTS.md)
- [开发台账](../../pilot-development-plan.md)：既有阶段、验收状态和推进记录。
- [当前架构](architecture.md)：当前系统结构。
- [产品与系统架构](product-architecture.md)：产品方向与对象关系。
- [Plugin 说明](../../plugins.md)：现有格式、挂载、工具环境及运行记录。
- [使用指南](usage.md)

本文记录的是用户新提出的独立任务。旧文档中的“本轮范围”描述此前工作，不表示本任务需要排在那些阶段之后；旧 Plugin 文档的首期范围也不是本次兼容目标的上限。

便于进入代码的几个位置：

| 位置 | 当前相关职责 |
| --- | --- |
| `src/anchor/library.py` | Plugin 与工具解析、资源挂载和摘要 |
| `src/anchor/simple/graph.py` | Graph 定义、节点引用和子图展开 |
| `src/anchor/simple/run.py` | 调度、Plugin 注入、输入快照和运行记录 |
| `src/anchor/node/` | Agent/Op 执行、框架接线、上下文与恢复 |
| `src/anchor/runtime/` | 沙箱、命令环境与工具执行 |
| `src/anchor/pilot.py`、`src/anchor/serve.py` | 对话控制与服务入口 |
| `apps/web/src/Plugins.tsx` | 现有 Plugin 查看与选择界面 |
| `tests/test_plugins.py` | 现有 Plugin 行为的测试 |
| `plugins/academic-research/` | 当前随仓库维护的实例 |
| `pyproject.toml` | 实际依赖与固定框架版本 |

外部资料起点：

- [Agent Skills 规范](https://agentskills.io/specification)
- [Claude Skills 文档](https://code.claude.com/docs/en/skills)
- [Claude Plugin 参考](https://code.claude.com/docs/en/plugins-reference)
- [Anthropic Skills 仓库](https://github.com/anthropics/skills)
- [Codex 文档入口](https://developers.openai.com/codex/)

讨论期间已读取 Agent Skills 规范、Claude 的相关文档和 Anthropic Skills 仓库说明。Codex Skills 官方页面访问返回 403，其最新插件机制尚未核实。这些链接是研究起点，现有调查不构成覆盖两个生态的兼容性分析。Anthropic 仓库说明还区分了开源 Skill 与仅公开源码的部分文档 Skill，具体资源的使用条件需要结合实际来源理解。

## 给接手 Agent 的交接说明

用户希望你把这个方向作为独立任务推进，有空间自行研究、判断和提出方案。这份文档传递的是问题背景与产品意图，具体怎么做留给你结合仓库、上游生态和用户反馈发展。

本次交接只生成背景文档并记录入口，没有导入社区插件、实现兼容机制或开展兼容验收。用户关心的最终价值是：Anchor 能持续吸收社区已有能力，扩展可完成的工作，而用户不必逐个手搓 Plugin。
