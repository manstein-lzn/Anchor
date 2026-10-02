# Rust Graph Kernel 模块组织设计

状态：设计冻结前评审稿。本文只冻结代码边界和迁移顺序，不表示当前实现已经完成拆分或完成 R6。

## 目的

此前的 `rust/anchor-runtime/src/graph.rs` 同时包含 Graph 快照校验、Run 事实、FileRunStore、节点端口、串行 Runner、fanout/join 调度和全部测试。R5 时这种集中实现便于验证语义，但 R6 之后继续把新能力堆在同一个文件会让不变量、持久化和调度边界互相污染；当前已迁移到下述 `graph/` 模块目录。

拆分必须保持一个 Kernel、一个 Graph Runner 和一个 Run 事实格式。模块拆分不改变公开类型、JSON 字段、调度语义或 Python 宿主的适配边界。

## 冻结的模块边界

```text
graph/
├── mod.rs          公共导出、模块组合和 GraphError
├── model.rs        GraphSnapshot、GraphNode、GraphEdge、AgentDefinition
├── admission.rs    snapshot 校验、digest、fanout/join region 推导
├── state.rs        InvocationKey、RunResult、ParallelActivation、GraphRunRecord
├── store.rs        RunStore、FileRunStore、lease 和原子文件保存
├── ports.rs        NodeExecutionPort、ArtifactPort、RunControl 及请求/结果类型
├── logic.rs        纯路由、输入、跳过传播、SCC 和请求构造辅助函数
├── runner.rs       唯一 Coordinator、串行推进、路由和恢复入口
├── parallel.rs     当前 activation 的 wave、分支 cursor、join manifest
└── tests/
    ├── support.rs  fake ports、Graph fixture、测试 store
    ├── serial.rs   普通路由、ceiling、module activation
    ├── parallel.rs fanout/join、乱序、停止/预算/失败
    └── recovery.rs FileRunStore、迁移和故障窗口
```

`mod.rs` 只负责 `pub use` 和组合，不重新实现调度逻辑。现有调用方继续使用 `anchor_runtime::graph::{...}`；需要保持兼容的类型通过 `pub use` 暴露，内部辅助函数默认保持 `pub(crate)`。

职责归属如下：

| 不变量或事实 | 唯一所有者 | 允许依赖 |
| --- | --- | --- |
| Graph schema、节点/边合法性、配对区域 | `model` + `admission` | `state` 读取已准入快照 |
| Run identity、cursor、result、activation、format migration | `state` | `runner`/`store` |
| 文件原子保存、Run lease、load/save 完整性检查 | `store` | `state`、`runner` |
| 节点/Artifact/Control 宿主能力 | `ports` | `runner`、宿主适配器 |
| 普通节点推进、路由、停止和恢复入口 | `runner` | 所有运行时端口、`parallel` |
| 纯路由、输入、跳过传播和请求构造 | `logic` | `runner`、`state`、`admission` |
| fanout/join 局部并发与收束 | `runner` 当前的 parallel 区域 | `runner` 提供 Coordinator 上下文和保存能力 |

`parallel` 不是第二个 Runner，也不直接取得 `RunStore` 的独立写权限。所有 Run 变更仍由 `runner` 的唯一 Coordinator 顺序提交；`parallel` 只返回待提交的状态变更或通过 Runner 的受控内部方法完成一次 settle。

## 调度边界

串行 Runner 保留两个共同入口：

1. `prepare_invocation`：依据普通 Graph 规则处理 stop/pause、module activation、node max_rounds、输入 commit 和 cursor 持久化。
2. `settle_invocation`：依据 completion fact、Artifact freeze、route 和 result/edge sequence 提交完成事实。

并行分支必须复用这两个不变量，而不是复制一份“只选择节点并执行”的快捷路径。`parallel` 可以要求分支拓扑为静态单出口，但不得因此跳过：

- 节点 `max_rounds`；
- module activation ceiling 和本轮 scope 重置；
- Agent/Op capability admission；
- 精确 selected input commit；
- completion fact、Artifact freeze 和 result sequence。

分支之间允许并发，分支内部仍串行。分支 cursor 在 dispatch 前写入；完成按到达顺序由 Coordinator settle。join 仍是 Coordinator 合成控制事实，不调用 `NodeExecutionPort`；下游普通 AgentNode 读取 join commit。

## 迁移顺序

1. 已建立 `graph/` 目录和 `mod.rs`，通过 re-export 保持 `anchor_runtime::graph::{...}` API 不变。
2. 已移动纯数据类型和 Graph admission 到 `model.rs`/`admission.rs`，保留 format 3 JSON roundtrip 快照测试。
3. 已移动 GraphRunRecord、FileRunStore、ports 和纯逻辑辅助函数到对应模块，恢复窗口测试仍使用同一公共 API。
4. 已将测试从生产模块移到 `graph/tests.rs`，fixture 路径随模块位置调整，测试语义未改写。
5. 下一步从 `runner.rs` 抽取独立 `parallel.rs`，先把 branch prepare/settle 变成 Runner 受控的共享边界，再继续 R6 崩溃恢复。

## 当前验收结论与未决缺口

R6 当前切片已通过 75 个 Rust runtime 测试（其中 Graph 测试 46 项）、Clippy、fmt、diff 检查，以及 Python 相关子集 70 项。已验证真实分支重叠、乱序收束、join 不调用 NodeExecutionPort、失败不放行 join、BudgetStopped/Cancelled reload、FileRunStore activation roundtrip、并行分支的 node `max_rounds` 和 module activation ceiling，以及三个 provider-free 并行 wave 进程崩溃窗口。

这仍是“核心切片通过”，不是完整 R6：

- 并行 wave 现在已经复用 node `max_rounds` 和 module activation ceiling；共享 prepare/settle 入口仍需在后续拆分中继续收紧，避免再次出现旁路。
- 并行 wave 崩溃窗口目前只由 durable test ports 覆盖，尚未接入真实 Node/Artifact host。
- 尚未接入真实 Node/Artifact host、provider、平台宿主或独立 Graph 宿主。
- Cancellation 只保证 fail-closed、drain 已启动 future 和可恢复 cursor，不宣称 sibling exactly-cancel。

这些缺口必须先进入验收矩阵，再继续新增并行能力。
