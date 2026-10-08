# CodeMode 评估

> 历史快照：保留当时设计与证据，不代表当前实现或待办。当前入口见 [文档目录](../../README.md)。

2026-10-01。此次评估最初在 `pydantic-ai-harness==0.32.0` 上完成；本轮已升级并回归到 `pydantic-ai-harness==0.36.0`、`pydantic-ai-slim==2.46.0` 和 `pydantic-monty==1.0.0`。真实模型仍为配置的 `deepseek-flash`。数据源是只读的合成 Docmost 类 MCP 服务，页面正文约 24 KB，包含重复的标题、日期和金额字段；没有访问 Docmost、企业微信或生产 Graph。

## 已验证

- `pydantic-monty==1.0.0` 在 Harness 0.36.0 下可以正常运行 CodeMode；此前 Harness 0.32.0 的 Monty 0.0.x 兼容性结果仍作为版本迁移对照保留。
- Tool Search 发现 MCP 工具后，CodeMode 可以在 Monty 沙箱内调用多个已发现工具；MCP 工具内部调用会出现在 `run_code` 的 `ToolReturnPart.metadata`，包含嵌套工具名、调用 ID 和返回值。
- 通过 Anchor 的真实 `run_node`、Bubblewrap 内 stdio MCP、DeepSeek Flash 和结构化节点完成结果的整条链路可以完成。一次成功试验保存在 `.local/codemode-evaluation/anchor-node/`；其中 `codemode-meter.trace.jsonl` 是节点 trace，包含嵌套 `probe_search_reports` 和 `probe_read_report`。
- CodeMode 可以把读取后的完整页面留在沙箱中，只把抽取后的字段返回给模型；这符合 Graph 只关心节点结果和产物的边界。
- 现有已安装 Docmost MCP 实际检查结果：21 个工具均没有 `outputSchema`、`readOnlyHint` 或 `destructiveHint`。因此 CodeMode 目前无法仅凭 MCP 元数据安全地区分只读/写入工具，且生成的 Python 签名缺少返回类型；这正是 DeepSeek 在评估中更容易重试和误编排的直接原因。

## DeepSeek Flash 结果

同一合成任务分别进行了直接 MCP、Tool Search 和 Tool Search + CodeMode 试验。单次结果不能作为性能基准，DeepSeek 的计划和重试会影响数字。

| 模式 | 结果 | 模型请求 | 输入 token | 观察 |
| --- | --- | ---: | ---: | --- |
| 直接 MCP | 成功 | 3 | 20,788 | 搜索和读取各一次，读取正文进入历史 |
| Tool Search | 成功 | 4 | 22,523 | 首轮工具 schema 隐藏，但多一次搜索往返 |
| Tool Search + CodeMode（独立 Agent） | 成功 | 4 | 6,070 | 代码内读取并提取，返回约 307 output token；需提示模型不要返回全文 |
| Anchor `run_node` + CodeMode | 成功 | 4–8 | 约 6k 起 | 结构化完成和 Graph 约束能正常工作，但模型有时会重复 `run_code`、请求 Bash 或检查宿主路径 |

在最干净的一次独立 CodeMode 试验中，模型发现工具后用两次 `run_code` 完成搜索和读取，第二次只返回 86 个字符的字段结果，嵌套 `read_report` 的 32,805 字符正文留在 CodeMode 返回元数据中。另一轮 Anchor 试验还显示，DeepSeek 可能因为 MCP 返回 schema 不够明确而先后重试代码；因此 CodeMode 的收益依赖返回 schema、Plugin 指令和模型行为，不能只安装依赖就保证。

## 发现的限制

1. Harness 0.32.0 的 CodeMode 实现按照 Monty 0.0.x 的资源字段构造运行时限制；在该旧组合中直接安装 Monty 1.0 会失败：`unknown limits key 'max_duration_secs'`。当前固定组合已迁移到 Harness 0.36.0 + `pydantic-monty==1.0.0`，并通过隔离 CodeMode 合成调用和 Anchor 相关回归；这只证明版本兼容，不代表 CodeMode 已进入生产 Graph。
2. CodeMode 本身不负责恢复 REPL 状态。Monty 会话是单次 Agent Run 内的进程状态；重启、恢复或持久执行不能把 Python 变量当作持久事实。工具副作用必须由工具和现有 StepPersistence/效果账本承接。
3. CodeMode 会把嵌套工具调用保留在 `run_code` 元数据里，但 Anchor 当前 trace 投影和流式展示主要围绕顶层 PydanticAI 工具事件。启用后应明确 UI 是否展示嵌套调用，并确保恢复时不会把代码中的已完成副作用静默重放。
4. `run_code` 可以调用有副作用的工具；Monty 的 `restart`、模型重试或模型重新生成代码可能再次调用已经成功的工具。审批、主动发送、财务操作和不可逆 Docmost 操作不应无选择地放进 CodeMode。
5. CodeMode 默认把已发现工具签名放入 `run_code` 描述。它应与 Tool Search 配合；需要稳定 prompt cache 时再评估 `dynamic_catalog=True`，不能把 CodeMode 当成工具 schema 延迟加载的替代品。
6. 资源限制是 Monty 会话级的累计约束，`max_tool_calls` 是每个 `run_code` 调用的嵌套调用上限；它们不等同于 Anchor 的整个 Node 请求预算，仍需由 Anchor `UsageLimits` 和节点取消语义兜底。

## 上游版本核查

PyPI 的发布元数据显示这不是一个模糊的“最新包”问题，而是 Harness 的明确迁移点：

| Harness | 发布时间 | CodeMode 的 Monty 约束 | PydanticAI 约束 |
| --- | --- | --- | --- |
| 0.32.0 | 2026-09-19 | `>=0.0.23`，但代码仍传旧的 `max_feed_duration_secs` 字段 | `>=2.44.0` |
| 0.35.0 | 2026-09-25 | `>=0.0.23,<1` | `>=2.44.0` |
| 0.36.0 | 2026-09-25 | `>=1.0.0,<2`，已适配 Monty 1.x | `>=2.44.0` |
| 0.52.0 | 2026-09-30 | `>=1.0.0,<2` | 固定 `pydantic-ai-slim==2.52.0` |

在隔离环境中验证了 `pydantic-ai-slim==2.46.0 + pydantic-ai-harness==0.36.0 + pydantic-monty==1.0.0`：CodeMode 可以导入并完成 Tool Search + `run_code` 的合成工具调用。Anchor 工作区随后完成相关子集和全量回归（479 项通过）。这个结果说明 Anchor 不必为了 Monty 1.x 立即跳到 Harness 0.52；Harness 0.52 应与 PydanticAI 2.52 成对评估，不能只替换一个包。

官方资料给出的合理用法与 Anchor 设计一致：

- [Pydantic AI Harness CodeMode 文档](https://github.com/pydantic/pydantic-ai-harness/blob/main/pydantic_ai_harness/code_mode/README.md)支持 `tools='all'`、名单、谓词和 metadata；Anchor 采用默认全体普通工具路径，并让 Harness 自己保留控制工具、延迟未发现工具和代码执行工具。
- 同一文档说明 Tool Search 与 CodeMode 的组合：未发现的 deferred 工具不会进入 `run_code`，发现后才成为代码函数；`dynamic_catalog=True` 可保持工具定义块稳定，但工具签名会转移到动态指令中。
- CodeMode 的 Observability 章节定义了 `run_code` 返回的 `tool_calls` / `tool_returns` 元数据；Anchor 应据此投影嵌套轨迹，不应丢掉它们。
- Monty 默认没有宿主文件、环境变量和时钟访问。Anchor 应继续用已有 NodeSandbox 和 MCP 传递文件，避免给 CodeMode 配置 `MountDir` 或 `os_access` 去绕过 Graph 的路径边界。
- [Anthropic Tool Search cookbook](https://github.com/anthropics/anthropic-cookbook/blob/main/tool_use/tool_search_with_embeddings.ipynb)展示了把工具当作可发现资源的模式，并报告其示例初始上下文可下降约 90%；这个数字是其示例的结果，不是 Anchor 的测量。

## 当前 Anchor 接线

CodeMode 现在是 AgentNode 的内部自动优化，不出现在 Graph schema 或 Web 编排表单中。安装 `.[codemode]` 后，真实 provider 的 AgentNode 自动创建 Harness `CodeMode(tools='all', dynamic_catalog=True)`；Tool Search、完成控制和其他框架工具由 Harness 保持原生，Anchor 的 Bash 工作区工具也保持原生。没有 Monty 时自动回退到普通工具调用。FunctionModel 测试路径仍保持原有直接工具行为，以便无 provider 回归保持确定性。使用当前 `.env` 的 DeepSeek Flash 做最小真实 provider smoke test 已通过：自动 capability 生效，模型在同一节点内正确使用原生 Bash 与 `run_code`，2 次模型请求后完成结构化结果；未挂载业务 Plugin，也未执行外部副作用。

因此 Plugin 和 Agent 都不需要理解工具全名或 Monty 资源参数。Plugin 继续提供普通 MCP/PydanticAI 工具；CodeMode 自动获得当前回合已经发现的可用工具，并把嵌套调用放入既有 trace 元数据。NodeSandbox、Graph 的网络和文件边界不向 Monty 扩大。

## 结论和建议

CodeMode 适合 Anchor 的 AgentNode 内部批量查询、筛选和汇总。它能减少完整 MCP 结果进入模型上下文，Graph 仍只接收结构化完成结果和文件产物。它不会替工具判断业务授权或外部副作用结果；这些仍由现有 Plugin 权限、Graph 调度、发送幂等和恢复语义负责。

后续重点不应是让每个 Graph 配置 CodeMode，而是补强通用工具事件的嵌套轨迹投影、取消和恢复观测，并用真实 Docmost/企业微信对话比较请求数、输入 token、延迟、结果正确率和副作用结果记录。CodeMode 本身保持基础设施默认开启；单个部署只需在运行环境级别关闭 Monty 依赖即可回退。

工具返回 schema 仍然值得逐步补齐，因为它能改善 Monty 代码的类型提示和模型结果质量；它不再是用户启用 CodeMode 的前置条件。副作用工具仍需遵循各自的授权、幂等和恢复契约，不能把 CodeMode 当成外部系统 exactly-once 保证。
