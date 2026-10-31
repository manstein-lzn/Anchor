# Graph 作者模型与 Runtime IR 契约

普通用户继续编辑同一种 Graph JSON；`GraphSnapshot::from_authoring` 负责校验、规范化、模块展开和资源绑定。WebUI 不要求用户手工改写执行 IR 或编辑 `manifest.json`。

```text
画布 / Graph JSON → Authoring Model → Compiler / Admission
                  → GraphSnapshot + 资源 manifest → 共享 Runner
```

## 两层模型

作者模型表达 `agents`、`ops`、内联 `graphs`、`nodes`、`edges`、目标/接口、输入输出、读写/网络声明、Plugin 和 `layout`。角色只复用配置，Plugin 直接挂节点。

Runtime IR 保存稳定展开节点、路由/反馈/并行、输入与权限、绑定 Plugin 和执行所需事实。它不承载编辑器布局，也不能静默忽略模型、读写、调用模式或权限。未知字段与不支持组合必须在编译/接纳时明确诊断，不能伪装为可运行配置。

## 编译与保存

1. 解析作者定义并校验 schema、拓扑、接口、模块和读写声明。
2. 展开内联模块，生成稳定节点 ID，保留原作者定义供重开编辑。
3. 从 canonical Library 解析 Plugin，生成精确无密钥资源闭包。
4. 编译反馈、轮次、fanout/join、Graph Call、输入映射和文件引用。
5. 写出定义与摘要 manifest，bundle loader 再验证资源与绑定。
6. 返回具体位置和原因；保存成功与 provider/业务服务可用分别验收。

| 用户动作 | 应维持的契约 |
| --- | --- |
| 修改 Agent/Op | 编译错误指出角色/节点，定义可重开 |
| 设置 host Op | 保留 operation JSON；宿主能力、入口授权与普通命令执行分别检查 |
| 设置反馈 | 检查出口和轮次，保留同 Run 文件延续与恢复身份 |
| 挂载 Plugin | 已安装资源校验与摘要绑定；未安装/越权明确拒绝 |
| 内联模块 | 节点身份稳定，原模块与 layout 往返保存 |
| fanout/join | 配对、独立分支、收束和失败边界提前校验 |
| 独立 Call | 目标、wait/detach、输入映射与父子事实明确 |
| 修改 layout | 仅保存展示信息，不扩大执行权限 |

作者模型断言、固定展开样例与 HTTP CRUD 测试位于 Rust crate 的测试范围。测试样例不执行另一个语言解析器；具体本轮结果见 [开发台账](pilot-development-plan.md)。历史快照只用于解释先前兼容来源。

## 文件交付

Agent 完成必须调用 `final_result`，显式提供 `files: string[]`；纯文字使用 `[]`。路径相对 `/workspace`，允许选择目录并递归交付；禁止根目录、通配符、路径逃逸和选中链接/特殊文件。普通二进制、可执行位和空目录受支持。节点 `writes` 声明必交项，必须存在且被 `files` 覆盖；漏项在接受完成前返回可修正错误。`Op.run` 直接按 `writes` 交付；没有 `writes` 的 Op 仅交付文字/JSON 输出。

工作现场不会逐 invocation 复制或还原；未交付文件继续留在同一工作区。下游只读 `/in/<node>` 仅含该版本的交付，`.anchor-artifact.json` 提供固定 Artifact 引用、清单摘要及文件摘要。需要编辑上游文件时自行只复制所需文件。项目 Git、依赖和缓存无需为归档搬到临时目录。新 Run 默认独立，跨 Run 共享由 Host 显式绑定资源；Goose 仍负责原生会话续接。

## 模型选择

Agent 的 `model` 可填写 `models.default.flash`、`models.default.pro`、`models.backup.flash` 或 `models.backup.pro`，分别读取设置页中的服务和模型。省略 `model` 或使用 `models.default` 时选择 default Flash。每个新节点 invocation 读取当时保存的配置，同一 Run 的后续节点与常驻助手下一轮也适用；进行中的 invocation 和同 invocation 恢复保留其模型/服务绑定。backup 仅在明确选择时使用，不自动降级。

default 尚未保存时兼容旧托管默认配置；已保存但缺少所选模型或密钥时执行失败，不回退旧配置。上述四个设置引用以外，仅已有托管配置中的显式别名保留旧映射；不读取 `ANCHOR_MODEL_ALIASES` 或其他环境模型服务字段。新 Graph 使用上述设置引用，仓库示例已按此选择模型。旧 invocation 即使用过与新设置引用同名的别名，恢复时仍核对原绑定，不自动改写用户 Graph。模型请求和历史仍由 Goose 原生 Provider/Session 承担。

## Host Op 与常驻助手

Op 在 `run`、`call`、`fanout`、`join`、`host` 中恰好选择一种。`host` 必须为 JSON object，`operation` 必须为非空字符串；其余参数由 Kernel 原样保留并通过 `NodeExecutionPort` 交给宿主，不解析为 Session/WeCom 类型，也不授予隐式 Plugin、网络或文件权限。`NodeExecutionCapabilities.host_operations` 默认关闭；支持普通 `Op.run` 不等于支持 host operation。当前 Host 只接入获准助手的 `session.wait_input` 和 `session.reply`，未知 operation 明确拒绝；Graph 能保存不代表手动或 standalone 入口获得 Session 授权。

当前助手的两项 Op 定义为：

```json
{
  "ops": {
    "wait_input": { "host": { "operation": "session.wait_input" } },
    "reply": { "host": { "operation": "session.reply" } }
  }
}
```

完整 opt-in 样例见 [wecom-persistent-assistant.json](../examples/graphs/wecom-persistent-assistant.json)。这是普通 `wait_input (Op) → assistant (Agent) → reply (Op) → wait_input`，不是新节点类型或专用 Runner；渠道只接纳这一形状。当前渠道 Host 接纳要求恰好一个等待 Op、一个 Agent 和一个回复 Op，入口是等待节点，三条普通边构成上述循环，不设置节点 `max_rounds` 或模块轮次上限。渠道配置的结果节点仍为 Agent `assistant`，与投递 Op `reply` 分别绑定；Kernel 的通用 host port 本身不限定这套业务形状。host Op 当前不能放入并行区域。

- `session.wait_input` 只在可信助手 binding 的 Session 内领取输入，并持久绑定等待 invocation 与 Turn；恢复/重试仍读取同一 Turn。等待不调用模型，不继承 `Op.run` 的默认命令超时，支持暂停/取消。本轮内容位于等待节点 commit 的 `output`（`message`、`channel`、`attachments`、`session`、`turn`、`interrupted_messages`），Agent 读取 `committed_inputs`，不得把冻结的 Run 启动 `input` 当作下一轮消息。
- `session.reply` 只读取本 Turn 经普通边交接的确切 Agent commit，校验 `source_commit`、`reply_for` 与本轮图片集合，然后结算 Turn 并路由回等待；不要求整个 Run Completed，不读取一个长期 Run 的“最新产物”替代本轮身份。平台 ACK/unknown 属于既有投递账本，不由节点完成状态代替。
- 新输入中断旧 Agent 后，执行端口须等执行者真正退出，保留可写现场并持久保存 `CompletionFact::Yielded`，再返回 `NodeExecutionOutcome::Yielded`。Runner 记录 `interruption` 并冻结 `ArtifactKind::Interruption` 控制 Artifact，沿普通合法 route 到回复节点；旧轮 `suppressed` 后返回等待，不冒充 Agent 成功或业务回复。控制 Artifact 不冻结未完成 workspace 为成功文件；不支持中断控制证据的 Artifact 适配器须 fail-closed。普通 `Cancelled`/`Interrupted` 停止语义不因这一扩展被改写。
- 所有节点的 workspace 默认按 `(Run, node)` 稳定复用；逐轮 invocation 和显式交付的 Artifact 身份独立不可变。输入和文件授权来自可信 Turn/binding：当前 `/in/channel`，最多最近 8 条尚未越过 confirmed 边界的中断输入快照 `/in/channel-pending/<turn>`，不接受作者 JSON 或模型输出自行指定全历史路径。

首次 wait binding 查询最近 9 条、实际只授权最近 8 条并持久保存 `pending_truncated`；等待产物同时输出 `interrupted_messages_truncated`。当 `output.interrupted_messages_truncated=true`，Agent 必须如实说明更早中断输入未自动纳入/未完整阅读，不能声称信息全保留或借此请求全历史挂载。该标记无需额外数据库 schema 迁移，旧 binding facts 缺省 `false`。

契约检查分别位于 `rust/anchor-runtime/src/graph/tests/host_operations.rs`（编译/通用端口、路由和控制 Artifact）、`rust/anchor-runner-host/tests/goose_persistent_assistant.rs`（实际 Host/Goose ACP、确定性模型传输、跨轮文件与恢复）及 Session/Host 的助手退役测试。真实 provider 隔离证据、实际企微验收与生产切换分别记录，见 [助手验证方式](wecom-assistant.md#docmost-和验收)；这些测试位置本身不是本轮执行通过的声明。

## 当前边界

保存校验不验证模型凭据、真实网络、业务内容质量或所有用户 Graph 的执行结果。Plugin 闭包来自宿主 canonical catalog，不从请求接受任意本地路径。精确累计预算、嵌套并行、分支交叉和区域内 Call 等未支持能力应明确拒绝。

现有 Graph 字段可解析不等于已完成运行级兼容；RSI、周报和深度研究需要其工具、输入与门禁分别验收。目标设计中的编辑器扩展也不能自动视为支持。独立包可以隐藏 Runtime 源码，但 Graph、Skill 和资源仍是用户资产。

共享职责与运行不变量见 [产品架构](product-architecture.md)，当前实现见 [当前架构](architecture.md)。
