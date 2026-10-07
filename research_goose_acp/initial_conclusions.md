# Goose + ACP 对 Anchor 的初步调研结论

日期：2026-10-06。这是静态研究和架构判断，不是接入方案冻结或运行验收。

## 一句话结论

**Goose 是值得认真验证的 Rust Agent Runtime 候选；ACP 是值得保留的外部 Agent 接入边界。但当前不能得出“换成 Goose 就更合适或更接近生产”的结论。建议保持现有生产路线不变，先用小型确定性 Graph 验证一个可逆的 Goose 节点执行切片，再决定是否替换内核。**

这不是因为已投入 io-harness 就拒绝更好的框架，而是需要比较 Goose 能减少多少剩余工作，以及能否保留 Anchor 的产品不变量。

## 版本基线

- 本次读取官方发布 API 时，最新稳定产品为 `v1.53.0`，发布于 `2026-10-02T18:44:11Z`，tag commit 为 `76da81cb964b21cd096db739302329b40c2998b8`。
- 该发布源码中的 GDK crate 版本为 `0.1.0-alpha.11`。稳定产品发布不等于嵌入式 Rust API 已稳定。
- 对照 main 固定为 `540df77c30e3c1f3b51915cb18ae081931c96851`，提交于 `2026-10-06T04:24:32Z`；它修复 ACP client extension 选择语义。不能把 main 修复算入 v1.53.0。

## 已核实的价值

1. **不需要 Python 才能复用核心。** Goose 具有 Rust Agent loop、Provider、上下文管理和 ACP server。桌面 UI 与跨语言绑定是其他交付面，不是 Anchor 复用核心的必选依赖。
2. **不只有 CLI 黑盒一种方式。** `goose-agent`、`goose-provider-types` 等公开接口可以在同一 Rust 进程中组装 Runtime。默认 `goose-sdk` 主要导出 ACP wire types，不能将它误称完整 Agent 内核。
3. **ACP 的两个方向都存在。** Anchor 可以作为 client 调用 Goose agent；Goose 也可以作为 client 调用外部 ACP agent。对 Anchor 直接调用其他 agent 而言，并非必须再嵌套一个 Goose 中间层。固定版本外部 ACP Provider 的官方文档仍列出 `goose session resume/fork` 不支持；源码内部尝试 `Provider::resume` / `session/load` 不能直接当作这两个 CLI 产品流程已经交付。
4. **低成本回归可行。** Provider 接口可以注入确定性实现；完整 Goose 产品库还公开模型录制/回放 Provider。可以执行实际 Goose loop、工具和工作区路径而不访问真实模型，但目前尚未在 Anchor 中实现或验证。

## 不同接入方式的判断

| 方式 | 收益 | 当前判断 |
| --- | --- | --- |
| Anchor 的节点执行适配器通过 ACP 调用 Goose | 复用完整产品行为，减少对 Goose 私有 Rust API 的直接耦合，可验证外部 Agent 互换 | 适合先做单节点、进程隔离的可逆 spike；协议能力仍须协商和测试，不保证所有 agent 等价 |
| Anchor 进程内嵌入轻量 Goose Agent crates | 工具、工作区、持久事实可继续由 Anchor 掌握，不必引入完整 Goose 配置/会话体系 | 有效的内核候选；alpha API、会话/效果持久化、完成/预算/恢复适配仍要评估 |
| 用 Goose 完整平台取代 Anchor | 利用其现成 Agent 产品 | 尚无理由：它不直接承接 Anchor Graph、Run、Artifact、Plugin 授权和既有平台契约 |
| Anchor 自己对外提供 ACP server | 为编辑器/外部客户端开放 Anchor 的执行能力 | 可独立规划的入口能力，不是切换内核或当前生产迁移的前置条件 |

候选执行器应位于 `NodeExecutionPort` 边界。不要把 Goose 完整 Agent 当成 Rig 式纯模型 transport 塞进现有 io-harness loop，造成双重 Agent 循环、工具所有权和预算事实难以解释。试验期可以比较两个后端，但一次节点执行只由一个 loop 负责。

## 四个关键门槛

### 1. 权限不能只靠 ACP 或提示词

cwd 是上下文，不是 OS 沙箱；权限请求也不保证每个工具都必经 Anchor。Goose v1.53.0 初始 extension 选择先加入 builtin，再合并 client 选择，因此空选择也可能仍保留 developer。main 对精确初始选择的修复，不证明旧 session 已撤权。必须验证默认工具、配置、recipe、load、MCP 和文件/进程路径，保留 Anchor Sandbox 与宿主授权边界。

### 2. 可加载会话不等于未知副作用安全恢复

Goose 提供会话存储与加载，但工具副作用与工具结果持久化之间仍有需要验证的崩溃窗口。轻量 Agent crate 的工具执行先于结果 effect 落盘，恢复同一 kickoff 可能再次遇到未回答请求。这是静态代码支持的风险推断，不是实测重放结论。不能以 SQLite、session/load 或事件流证明 exactly-once，也不能把未知结果强制变成用户审批流程。

### 3. 通用 Agent 完成不等于 Anchor 节点完成

当前 Anchor 要求明确的结构化 completion、合法 route、工具结果已核查、正确的 invocation/Artifact 绑定及取消语义。ACP prompt 完成或最后一段文字不能自动等价替代。node history、模型调用预算、工作区快照与产物恢复也需要独立接线；ACP 展示流不能充当完整运行事实。

### 4. 原生 Agent Runtime 不等于完整平台替代

Anchor 当前已具有 io-harness loop、compaction、恢复和确定性 Host/Graph 回归路径。生产缺口仍包括会话/提问与渠道闭环、Library 安装授权、官方业务工具、预算、业务内容验收和数据迁移/切换。Goose 可以借用部分组件，不能把这些工作自动记为完成。判断标准应是剩余工作减少和产品行为保持，而不只是框架 feature 数量。

## 建议的下一步：只验证，不迁移

1. **单节点切片**：固定版本、隔离配置/数据目录和进程，接一个受限 AgentNode，只暴露假 MCP 与受控工作区。先验初始化、工具调用、结构化结果、历史与产物；不要接真实发布、发送或生产凭据。
2. **两层低成本测试**：fake ACP peer 只验证适配器；真正 Goose loop + 确定性本地 Provider 才验证 Goose 集成。通过后在同一套小型 Graph 中加入反馈、调用、取消、重启、越权和未知副作用窗口，检查 Graph 结果、逐节点 history、tool result 和 workspace。
3. **比较后再选择**：与 io-harness 路径比较适配代码量、事实所有权、恢复/权限/预算能力、构建部署与回归耗时。ACP 本身不降低模型 token 用量；真实 provider 仅在确定性切片通过后做少量有界验收，不跑数百论文或完整大型业务图来验证框架特性。

若 ACP 切片暴露的核心限制只能由运行时内部解决，再做轻量 `goose-agent` 嵌入对比；若两种方式都不能减少剩余产品工作，则保留 io-harness，仅将 ACP 作为可选外部接入方向。

## 依据和边界

- 详细 Runtime 证据与官方源码链接：`findings_runtime.md`。
- 详细 ACP、权限和恢复证据与官方协议链接：`findings_acp.md`。
- Anchor 事实与目标：`../docs/architecture.md`、`../docs/product-architecture.md`、`../docs/pilot-development-plan.md`、`../docs/rust-platform-development-plan.md`、`../docs/runtime-contract-tests.md`；节点契约为 `../rust/anchor-runtime/src/graph/ports.rs`。
- 官方发布记录：<https://github.com/aaif-goose/goose/releases/tag/v1.53.0>。
- 固定版本 ACP 接入说明：<https://github.com/aaif-goose/goose/blob/v1.53.0/documentation/docs/gdk/acp/index.md>。
- 固定版本 Agent kernel：<https://github.com/aaif-goose/goose/tree/v1.53.0/crates/goose-agent>。
- 固定版本 SDK：<https://github.com/aaif-goose/goose/tree/v1.53.0/crates/goose-sdk>。
- 对照权限选择修复：<https://github.com/aaif-goose/goose/commit/540df77c30e3c1f3b51915cb18ae081931c96851>。

本轮以官方 API、文档、固定源码静态阅读为证据。子任务网页搜索未返回可确认的引用 ID，已用 curl 核查官方来源并保存源 URL；主 Agent 随后通过网页读取补充核对了发布记录、Agent kernel、协议接入说明、工具/效果持久化和测试 Provider 源码。未编译 Goose、未实现适配器、未跑回归或真实模型、未改变 Anchor Runtime/平台路线、未修改生产配置。
