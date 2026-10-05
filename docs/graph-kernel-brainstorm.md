# Graph Kernel 设计头脑风暴

状态：提案，未冻结。本文用于整理语义，不代表已经开始重写 Runtime。

## 核心判断

Anchor 现在的核心方向是正确的：AgentNode、OpNode、Edge 表达执行结构，Plugin 表达能力，Graph 表达可组合的流程。当前的裂痕主要来自 Edge 还同时承担路由、广播和汇合语义，Graph 调用又同时涉及节点执行和 Run 生命周期。

目标不是继续增加专用节点，而是形成一个最小、正交、组合闭包的 Graph Kernel：

```text
Node       = 一个有输入、输出和完成事实的执行单元
Edge       = 激活、路由和输入依赖关系
Plugin     = 节点可使用的能力集合
Graph      = 可以作为 Node 使用的复合执行单元
Run        = Graph 的一次执行实例
```

## 候选表达

### 并行

当前的 fanout/join 是一种正确但偏底层的表达。它把“同时激活分支”和“等待分支汇合”显式写成控制 Op，便于恢复和观察，但普通作者不一定需要直接看到这两个节点。

可选方案：

1. **保留显式 fanout/join**：兼容性最好，Runtime 直接理解，作者心智负担较高。
2. **增加并行组语法**：作者声明“这些边全部激活，之后等待全部完成”，编译时降级为 fanout/join。推荐作为 UI/作者层的便捷表达，同时保留旧 JSON。
3. **把所有普通 Edge 改为广播**：拓扑最简单，但会破坏现有路由选择语义，不建议直接采用。

推荐方案 2。并行的本质是 Edge 的激活策略和汇合策略，fanout/join 可以成为 Runtime 的规范化结果，而不是作者必须管理的对象。

### Graph 调用

Graph 可以满足和 AgentNode、OpNode 相同的输入、输出、完成和产物契约，因此从语义上应当是一个复合 Node，而不是第二套 Graph 系统。

调用方式仍然需要明确生命周期策略：

- `inline`：展开到当前 Run；
- `child + wait`：启动独立 Run，等待完成并读取选择的结果；
- `child + detach`：持久接纳独立 Run，父 Run 不等待业务完成。

这不是新的用户对象，而是 Graph Node 的执行策略。现有 `Op.call` 可以继续作为兼容的 JSON 表达，Runtime 内部把它规范化为复合 Node 调用。

## 推荐的语义分层

用户继续编辑现有 Python-compatible `graph.json`。Rust 读取同一份文件，内部只做规范化：

```text
graph.json
  → Author Graph（Agent / Op / Node / Edge / Graph）
  → Edge policy + Invocation policy 规范化
  → NodeInvocation / Run state
  → Rust Runtime
```

`layout` 等视图字段保留在作者文件中；模块展开、Plugin 资源解析、并行控制记录和子 Run 引用属于内部执行事实。不能把规范化结果再暴露成用户必须编辑的第二种 Graph。

## 需要冻结的语义不变量

1. Agent、Op、复合 Graph 都有统一的输入、输出、完成、失败和恢复契约。
2. Plugin 只授予能力，不改变 Graph 的调度语义。
3. Edge 的“选择一个”“激活全部”“等待全部/任一”必须显式可判定，不能靠节点类型猜测。
4. Graph 作为 Node 组合后仍然是 Graph，嵌套不产生第二套身份和权限模型。
5. 每个 NodeInvocation 都有稳定身份、输入快照、输出产物和恢复事实；并行只是同时存在多个 invocation。
6. Python 与 Rust 使用同一份作者 Graph JSON；Rust 不能要求用户先手工改写成 format-1 bundle。

## 推荐推进顺序

1. 用四个 golden Graph 固化 Python 的 admission、展开、路由、并行和调用事实。
2. 让 Rust 解析同一份作者 JSON，先覆盖普通 Agent/Op/Edge、反馈边、`with`、Plugin 和恢复。
3. 在 Rust 内部引入统一的 Edge policy、Invocation 和 Graph call 状态，不改变用户 JSON。
4. UI 增加“并行组”和“子图”高层操作，旧 fanout/join 与 `Op.call` 继续兼容。
5. Python/Rust 做差分验收；所有维护中的 Graph 通过后，再切换生产执行路径。

暂不建议立即重写现有 Graph JSON，也不建议在语义未冻结前继续增加新的专用 Op 类型。
