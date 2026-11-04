# Goose 能力复用与按需激活路线

> 历史归档（2026-11-04）：保留当时的推导、授权范围、过程与证据，不作为新的实施授权；当前行为见[架构](../../architecture.md)，未关闭事项见[统一台账](../../pilot-development-plan.md)。

2026-10-13。用户目标：遵循 Anchor 哲学，尽量复用 Goose 的执行机制，提高稳定性和质量。本文件记录路线判断及尚未实现的边界；原生 API 实验见 [证据](../development-2026-10-13/evidence/plugin_disclosure_20261013/native-activation/README.md)，进度见开发台账 A223，当前实现仍以 [architecture.md](../../architecture.md) 为准。

## 职责与复用原则

Anchor 持有 Graph/Run/Artifact、Plugin 真身、共享依赖、Session/Turn 和宿主授权。Goose 持有 Agent loop、Provider、原生会话/历史、compaction 与自身扩展运行机制。Kernel 和稳定 Node 契约不依赖 Goose 的私有类型，Pilot 和 AgentNode 共用适配。

Goose Session 中的扩展配置是执行投影，不能成为另一套 Plugin 安装、挂载或权限事实。加载原生历史后仍在模型执行前核对当前授权；运行时新增连接同样必须由宿主允许。解释器、依赖和工作区引用复用已有唯一来源，不交由 Goose 的全局配置重新定义。

框架能力先经固定版公开接口接入；协议通信继续使用成熟 MCP SDK。新增 Host 机制只补充产品事实、授权、隔离和恢复所需的边界。每次原生接入都收敛对应的重复职责，保持一条正式执行路径；不新增通用 Agent loop、记忆/计划系统、连接平台或独立 Plugin 状态库。

## 原生接口实验后的选择

固定 v1.53.0 有两条不同路径：ACP session extension add 可以在活动 prompt 中连接，但本轮工具 schema 不自动刷新；原生 Extension Manager 可以在同 prompt 刷新并调用，却从 Goose configuration 管理扩展，并把所选服务的全部工具 schema 发给模型。实际测试的 1,004 工具全部进入模型请求，manager 也能停用 `tom`。扩展经原生 Session 保留，load 会重新连接。

据此，后续按需激活采用**现有 Anchor 聚合 MCP + 成熟 rmcp 连接适配**作为实施方向。Goose 原生管理能力继续用于原生会话和宿主控制的扩展校准；完整 Plugin 目录、说明/Skill/资源、权限和有界 schema 披露继续从 Anchor Library 与实际授权事实派生。不直接开放原生 Extension Manager，也不为按需激活另写通用 MCP 连接引擎。

这是接入路线判断，不是懒连接已实现的声明。本次没有改动生产 transport、工具装配、授权或状态格式。原生动态扩展未来如提供合适授权及轮内更新契约，可在 Goose adapter 内替换相应机制；当前不通过静默升级、私有 API、伪造工具名字或修改 Agent loop 获得刷新。

## 下一切片需要确定的契约

1. **发现先于连接**：Plugin 元数据、说明、Skill 和公开资源由 Library 离线发现。选中 Plugin/server 后取得真实 MCP inventory/schema。缓存只作可重建的发现观察；作者不重复维护工具 schema。未连接服务的工具目录未知，需要明确返回状态，不能伪称全量实时工具已发现。现有 A222 的 `anchor_tools` 完整授权 inventory 契约目前保持不变，未来调整必须明确记录并验证。
2. **连接属于执行资源**：优先限定在当前 invocation/Turn 内复用已用连接，首次使用按既有 Sandbox/凭据/依赖视图建立，取消/终态沿现有 owner 清理链收束。原生会话重载不把旧连接或曾启用状态当作当前授权，不预建跨用户或跨任务连接池。
3. **单次准备与授权一致**：准备仍从同一 Library 定义派生；延迟连接前核查冻结授权与当前现场，沿原有代码/依赖观察规则记录真实使用，不能借延迟扩大联网、可写路径、宿主命令或凭据访问。
4. **复用实际 adapter**：初始化、tools/list、请求/结果与 transport 使用已有 `anchor-mcp-host` / rmcp；Host 只补充按目标激活、合并同时首次使用、取消及所拥有资源的收束。Pilot 原生 peer、Node final_result、业务工具身份、媒体、错误和 receipt 继续走当前执行端口。

后续先实现一个 Plugin 的发现→选中→首次连接→详情→实际调用→回收垂直切片，Pilot/Node 共用策略；再覆盖未用服务不连接、坏服务隔离、并发首次使用、启动失败/取消、权限变化、会话重载和代码/依赖热更新。直接 schema 兼容模式与延迟连接的关系在切片中明确，不让历史 schema 假冒已连接事实。

## 验收边界

原生接口存在、确定性 probe、Anchor 接入和真实 Provider 端到端是不同状态。A223 只关闭路线实验，不关闭懒连接产品能力。后续仍须实际经过 Host、GraphRunner、NodeExecutionPort、固定 Goose ACP、授权 MCP 和 Sandbox；确定性回归只替换模型传输，随后独立真实 Provider 验实际参数、调用日志、trace/receipt、产物与清理。

按需连接的评价包括冷启动资源与失败范围，渐进披露的评价包括模型初始/查询/schema 字节；分开测量。HTTP 取消不能代替 stdio 子进程清理；单个大 MCP 工具目录也不能代替大量 server 的启动验证。发行、部署和旧数据/配置切换另行记录。
