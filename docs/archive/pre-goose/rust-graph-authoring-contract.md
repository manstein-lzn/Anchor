# Rust Graph 作者模型与 Runtime IR 契约

> 历史设计与首片验收，当前契约见同名现行文档。

## 结论

普通用户编辑的 Graph，必须继续保持 Python Graph 已经提供的作者语义。Anchor 已经为 `run`、Plugin、反馈边、Graph 模块等字段定义了明确含义；Rust 迁移的目标是实现这些含义，而不是要求用户换一种 Graph 格式。

作者 Graph JSON 应继续作为跨实现的规范输入。Rust 内部可以把它规范化为 `GraphSnapshot` 或其他执行结构，但这种转换应当是实现细节；对于普通 AgentNode、OpNode 和 Edge 图，应能使用同一份 `graph.json` 运行。

因此 Rust 内部可以采用如下路径，但不改变用户看到的 Graph：

```text
用户画布 / Graph JSON
        ↓
Authoring Model（与现有 Python graph.json 兼容）
        ↓  Graph Compiler / Admission
Runtime IR（Rust GraphSnapshot + manifest）
        ↓
Rust Runtime
```

Runtime IR 可以比作者模型更严格、更窄，但不能把它的限制伪装成用户编排限制。作者模型的语义由现有 Python Graph 解析器、文档和测试共同定义；Rust 必须逐项实现或在加载时明确报告尚未实现的能力。这里的 Compiler 可以只是无损规范化和资源绑定层，不意味着要建立第二套用户语言。

## 两层模型的职责

### Authoring Model

作者模型负责表达用户意图和编辑器事实，包括：

- Agent、Op、Graph 模块、节点和边；
- `with`、`input`、`input_map`、文件输入输出、`session` 和 `max_rounds`；
- Agent/Op 的 `reads`、`writes`、网络和时间限制；
- AgentNode 的 Plugin 挂载；
- 反馈边、fanout/join 和独立 Graph 调用；
- 编辑器 `layout`、标签和其他不会改变运行语义的视图字段。

`layout` 等视图字段必须能随定义保存和重开，但不得传给 Runtime IR 的执行校验器。作者模型的保存校验应与实际可编译能力在保存时一起检查，不能等到运行时才发现不支持。

### Runtime IR

Runtime IR 只保存一次运行或一次部署真正需要的事实，包括：

- 已展开、身份稳定的 AgentNode/OpNode 和边；
- 已解析的 Graph 模块和静态 Graph call 目标；
- 已绑定且经过资源闭包校验的 Plugin；
- 已解析的输入输出、读写快照和权限；
- Runtime 可执行的操作、恢复边界和版本信息。

IR 可以拒绝当前 Runtime 尚未实现的能力，但拒绝必须指出作者模型中的位置和原因。不得静默忽略 `model`、`reads/writes`、Plugin、调用模式或权限字段。

## 编译入口的最低契约

编译器必须提供一个稳定入口，至少能完成：

1. 读取与 Python 兼容的作者 Graph（保留未知视图字段，不把它们误当运行字段）；
2. 使用与 Python Graph 相同的拓扑、接口、模块、读写和权限规则进行校验；
3. 展开 Graph 模块并生成稳定节点 ID；
4. 解析 Plugin，生成精确资源闭包和无密钥 manifest；
5. 将输入映射、文件引用、调用模式和 `max_rounds` 转成 Runtime IR；
6. 对尚未支持的能力返回可读、带路径的编译错误；
7. 输出可重复的 IR 和诊断摘要，供保存预览、部署包和 Run 快照复用。

首个垂直切片不要求一次实现全部 Runtime 能力，但必须证明同一份 Python 作者 Graph 可以经过“编辑 → 保存 → Rust 加载 → 运行”，而不是手工改写成另一份 Rust bundle。建议先覆盖：Agent、Op.run、普通边、反馈边、`with`、Plugin 挂载、读写声明和恢复；再加入 fanout/join、Graph 模块和 Graph call。

## 面向普通用户的硬验收

下面的动作必须在作者入口完成，用户不需要编辑 `manifest.json` 或了解 `GraphSnapshot`：

| 动作 | 保存时应发生的事 |
| --- | --- |
| 新增 Agent、修改指令或模型 | 画布可保存；编译诊断指出具体节点 |
| 新增 Op 并连接节点 | 拓扑和输入接口立即校验 |
| 增加反馈边并设置轮次 | 检查反馈出口和 `max_rounds`，运行时可恢复 |
| 挂载 Plugin | 解析资源闭包；无绑定或权限不足时保存失败并说明原因 |
| 使用 Graph 模块 | 展开后节点身份稳定，用户仍可折叠编辑 |
| 使用 fanout/join | 检查分支、收束和失败语义，不能运行时才拒绝 |
| 修改布局或标签 | 保存作者定义，不污染 Runtime IR |
| 关闭后重开 | 作者定义与画布状态一致，编译结果可重复 |

首个兼容切片已将 `GraphSnapshot::from_authoring` 接入 Rust bundle loader 与 Graph CRUD。Python conformance fixture 的 8 个作者定义（普通图、反馈、fanout/join、模块、Plugin 引用、Graph call 和编辑器 `layout`）均与 Python 展开快照相符；HTTP GET/PUT 可保存并取回原始模块和 `layout`。Graph PUT 从 canonical Plugin catalog 校验引用，将资源清单列出的非密钥文件复制进 bundle，并写入 digest/resources/MCP server manifest；loader 再对落盘闭包做完整摘要校验。测试覆盖 Plugin Graph 保存、闭包加载和移除旧引用时清除旧 bundle resources。这个结果证明作者 JSON 的首个可运行子集已落地，不证明完整闭环：Python 全部校验/coercion 规则、未知编辑器扩展字段、所有既有生产 Graph 的 Run 行为和真实业务 Provider/MCP 仍未验收。Python Graph 仍是生产作者路径。

## 当前明确不承诺的事情

- Rust 当前 HTTP Graph API 不是完整作者 API；它保留作者 JSON并编译已支持的子集，Plugin binding 只支持从宿主已安装的 canonical catalog 生成 bundle 闭包。
- 已有 Python Graph JSON 不能因为字段能被 JSON 解析，就宣称可由 Rust 原样编辑和运行。
- 二进制 Runtime 交付隐藏的是 Anchor Runtime 源码和 Python 依赖，不自动隐藏 Graph JSON、Plugin 脚本或业务代码。
- Rust Runtime 的执行速度不构成作者模型兼容性的替代验收项。

## 决策

在 Compiler/Admission 层落地前，不继续扩张 `GraphSnapshot` 以承载所有编辑器字段，也不把深度学术调研、RSI、周报或企业微信 Graph 标记为 Rust 可替代。迁移顺序是先完成一个普通用户可编辑 Graph 的作者闭环，再按能力矩阵逐个编译现有 Graph；平台 Channel、Scheduler 和 Session 仍是独立宿主能力。
