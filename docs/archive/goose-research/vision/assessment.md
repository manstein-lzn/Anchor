# Goose + ACP 对 Anchor 愿景的判断

> 历史调研：结论限于当时检查的版本和边界，当前实现与验收见开发台账。

日期：2026-10-06。状态：讨论与调研结论，不是运行时切换决定或生产验收。

## 总结

Goose + ACP 可以作为统一 Agent 执行后端的目标方向；目前没有发现它与 Anchor 产品对象和外层执行边界存在根本冲突。但“产品方向兼容”“Goose 原生能力存在”“Anchor 已接入”“完整产品已验收”不是同一结论，不能承诺已经满足所有愿景。

Anchor 应保留产品内核，Goose 承担 Agent loop、模型接入和原生上下文；ACP 是进程交互边界，MCP 是受限工具边界。迁移不应把 Graph、Run、Plugin 或平台会话替换成 Goose 对应对象。现有 Rig 仅为 io-harness 提供 transport，并不是两个 Agent loop；ACP 的收益是让这些实现依赖退出 Anchor 的进程内 Agent 装配，而不是让运行时依赖消失。

## 愿景归属

| 愿景 | 迁移后的事实所有者与判断 |
| --- | --- |
| Graph、AgentNode/OpNode、路由、反馈、局部并行和 Graph 调用 | Anchor 共享 Runner；Goose 只执行 Agent，不成为隐藏 Graph 调度器。所有组合仍需复跑 Goose fixture。 |
| 工作区、只读上游快照、Artifact 和可追溯产物 | Anchor；不得用 Goose cwd 或最后一段回复替代快照、完成和提交契约。 |
| Plugin、渐进披露、资源来源与授权 | Anchor Library/Plugin；Goose MCP 是调用接入，不是 Anchor Plugin 资产模型。 |
| Pilot、Session、Turn、多用户、Responses、WebUI | Anchor 产品与入口；Goose 原生 session 保存 Agent 历史，须明确映射，不能直接替代产品 Session。 |
| 企业微信、Docmost、Scheduler、研究与 RSI | Anchor 宿主、普通 Graph 与 Plugin；不要求 Goose 内置这些领域能力。 |
| Agent loop、Provider、原生会话与 compaction | 可复用 Goose，但各模型、媒体、压缩与长会话组合尚未在 Anchor Goose 路径验收。 |
| 独立 Graph 包与无 Python 标准部署 | 方向兼容；闭包需显式包含兼容 Goose binary、Anchor Runtime 与被选工具，不能把 ACP 当作打包/安装验收。 |

## 必验门槛

1. **未知结果后继续**：外部效果已发生、ToolResponse 尚未落盘时杀进程。重开原记录并由 Agent 查询现场后继续，不盲目重放，也不把底层恢复决定变成用户审批。官方 load 与新 prompt 是候选路径，尚未证明这条语义；不要求 exactly-once。
2. **权限与网络**：只暴露挂载且授权的工具，保持节点文件/网络/凭据边界。当前 spike 的共享网络只是受控测试条件，不是 OS loopback-only 隔离，不能作为生产安全结论。
3. **平台会话与长任务**：人工提问在等待中重启仍可回答；多轮压缩、反馈回访、用户新消息替换取消、观察重连与 history 清理。Goose 已有 compaction 和 form elicitation 接入，但这些产品组合未验收。
4. **模型、usage 与预算**：宿主模型绑定不能静默变化；摘要/重试/续聊都计入预算。标准 context 占用和 Goose 累计 usage 是不同事实，事后通知/取消不能证明下一次超预算模型请求从未发送。
5. **协议与部署可维护性**：固定二进制与协议，明确哪些是标准 ACP、哪些是 Goose 扩展。应优先用公开接口和已有框架能力，不为迁移再写 Agent loop、上下文或恢复引擎；若只能靠长期私有 SQL、模型响应解析或重度 fork 守住产品契约，需要重新评估“干净依赖”的收益。

## 建议路线

继续在候选分支按上述门槛验证，优先未知结果后核查继续，再接一个真实 Plugin 与 compaction/提问组合。常规验证沿用小型 Graph、真实 Goose 和确定性本地 Provider；通过后做少量有界真实 Provider 验收，不跑大型研究来代替执行契约验证。

若门槛通过且适配保持薄，建议 AgentNode 与 Pilot 最终统一 Goose，迁移后移除 Anchor 对 io-harness/Rig 的 Agent 装配依赖，而非长期保留三套后端。已有历史不静默换执行器，数据策略和生产切换须单独验收。当前产品架构仍冻结 io-harness，修改这一技术方向须明确记录，不能把此报告当作已经授权的切换。

## 依据与未覆盖

- 产品边界：`docs/product-architecture.md`、`docs/rust-platform-target.md`。
- 已执行证据与限制：`docs/goose-acp-spike.md`；A112 不证明未知结果安全继续。
- 本轮 Runtime 核查：`research_goose_vision/findings_runtime.md`，含固定 v1.53.0 官方源码来源。
- 官方 ACP 接入说明：`https://raw.githubusercontent.com/aaif-goose/goose/v1.53.0/documentation/docs/gdk/acp/index.md`。
- 官方协议：`https://agentclientprotocol.com/protocol/v1/prompt-turn`。

并行边界子任务没有交付新的 findings，不作为证据；本报告的边界判断来自主轨公开文档、现有产品契约和已经执行的 spike，未宣称新增模型/部署能力通过。没有运行新测试、真实模型或生产操作，没有修改架构冻结契约与验收台账。
