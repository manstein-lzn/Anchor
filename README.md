# Anchor

Anchor 是一个以文件、Git 和沙箱为基础的 Agent 工作流系统。用一个 JSON 文件定义图，让节点在独立工作区完成任务，通过带 commit 标识的输入交接成果，在 WebUI 中编排、观察和管理运行。

当前产品方向是：**以 Anchor Pilot 作为唯一系统级控制面，让用户通过可恢复对话使用 Graph；以 Plugin 作为 AgentNode 的可复用能力资产。** Plugin 是一套做事方法及其配套知识和工具；Agent 决定如何运用它，Graph 组织各节点之间的协作。产品边界、Session、恢复和分阶段建设见[产品与系统架构](docs/product-architecture.md)。

## 当前产品

- 创建、编辑、校验和保存工作流；支持 Agent Node、命令型 OpNode、条件路由、反馈循环和子图展开。
- 在统一的纵向画布中编排和观察运行，查看节点多轮执行、对话、工具调用和产物。
- 暂停、继续、停止运行，删除运行记录及文件，删除工作流及其全部运行历史。
- 使用深度学术调研图进行研究、质疑、写作和评审，按证据与反馈回流，产出学术综述论文。

Agent Node 使用 **PydanticAI + pydantic-ai-harness**，模型可通过 Bash 操作沙箱内的工作区，并以结构化结果结束节点；完成不依赖 Bash。每次新运行拥有独立的节点工作区，同一次运行的多轮执行保留文件与 Git 历史。图就是 `graph.json`，没有图版本发布流程；运行记录和 commit 用于追溯执行事实。

旧学术调研图继续按原有方式运行。Plugin 基础链路已接入：AgentNode 引用 Plugin、按需读取说明、调用共享环境中的工具，WebUI 可选择和查看，运行记录保存资源摘要。代表性示例为 [plugin-research.json](examples/graphs/plugin-research.json)。Anchor Pilot、持久 Session 和对话控制面已接入服务，Pilot 可查询和操作 Graph、Run、Plugin，并在修改 Graph 前请求用户确认。知识库编译、自动环境安装和真实模型长上下文验收暂未完成。

## 能力资产：Plugin

Anchor 的核心概念是 **Graph、AgentNode、OpNode、Plugin**。Plugin 由名称、描述、说明，以及按需关联的知识库和工具构成，直接挂载给 AgentNode，不设角色继承规则。

- **唯一事实来源**：Anchor 维护资源，Agent 配置只保存引用；多个能力可以引用同一知识库或工具。
- **渐进式披露**：系统提示词提供能力名称、简述和入口；Agent 按需阅读说明、查询知识、调用工具。
- **知识库暂缓**：先使用 Plugin 说明和补充资料，后续再接入编译知识与统一查询。
- **集中管理工具环境**：工具入口与实现有明确来源；按 Python 和依赖要求隔离环境，跨节点、跨运行复用，不在每个工作区重复安装。
- **图中可见**：编排和运行界面展示 Agent 配置的能力，并能查看说明、知识和工具入口。配置的能力不等于已成功调用或结果可靠。

Plugin 不等同于 Op。OpNode 继续表示图中明确安排的程序执行；Agent 和 OpNode 可以复用同一工具实现。能力通过文件维护，UI 负责选择与只读查看。边界、格式和验收依据只在 [Plugin 设计](docs/plugins.md) 中维护；[当前架构](docs/architecture.md) 说明其文件系统结构。

## 开始使用

首次安装、模型配置和示例工作流导入见 [使用指南](docs/usage.md)。完成准备后，从仓库根目录启动：

```bash
./scripts/dev.sh start
./scripts/dev.sh status
```

访问 **http://127.0.0.1:5173**。后续开发也统一使用此脚本管理后台服务，关闭终端或对话框不会关闭 Anchor。脚本不提供开机自启或崩溃自动拉起；日志、停止和重启方式见使用指南。

## 文档入口

| 文档 | 负责回答的问题 |
| --- | --- |
| [使用指南](docs/usage.md) | 当前怎样安装、启动、编排、运行、恢复和管理文件？ |
| [当前架构](docs/architecture.md) | 当前代码怎样组织，节点、工作区和工具怎样连接，有哪些已知边界？ |
| [产品与系统架构](docs/product-architecture.md) | Anchor Pilot、Session、Graph、Run、Plugin 的产品真相、边界和演进顺序是什么？ |
| [Plugin 设计](docs/plugins.md) | 唯一升级方向是什么，什么已确定，什么尚未实现，怎样验收？ |
| [开发约定](docs/development.md) | 后续开发先读什么，怎样验证和同步文档？ |
| [历史归档](docs/archive/README.md) | 旧架构、讨论和内核迁移当时依据什么，有什么验证记录？ |

现行使用行为由使用指南和当前架构说明；产品边界与演进顺序以产品与系统架构为准，Plugin 的能力格式以 Plugin 设计为准。历史归档中的待办、里程碑和提案不再构成开发计划。运行事实以当前源码与可复现验证为依据，发现文档不符时修正文档，不用旧方案覆盖现状。
