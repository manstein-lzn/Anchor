# Graph Kernel 模块边界

共享 Kernel 位于 [anchor-runtime/src/graph](../rust/anchor-runtime/src/graph)，平台与独立 Host 复用同一 Runner、端口和 Run 事实。模块拆分围绕不变量，不增加第二套执行语义。

| 边界 | 内容 |
| --- | --- |
| 作者模型 / 编译 | 定义、模块展开、拓扑、接口、读写与静态诊断 |
| IR / 调度 | 已展开节点、依赖、路由、反馈与显式并行区域 |
| Run 状态 / Store | invocation、完成/中断事实、cursor、控制与持久化 |
| NodeExecutionPort | 结构化请求/结果、取消与单节点能力 |
| ArtifactPort | 提交、谱系和只读输入 |
| GraphCallPort | 独立子 Run 的接纳、等待、detached 与恢复关联 |

每 Run 有唯一协调者。并行只在合法配对区域内发生，节点执行不会自行推进 cursor；乱序返回仍需按确定的 join 契约收束。失败和停止保留已发生事实，不把全部分支重跑作为恢复策略。

HTTP、Session、Scheduler、渠道协议、Goose 进程接入、文件沙箱与业务工具属于 Host/适配器。Kernel 只通过小端口调用它们，不读取其他模块私有状态，不依赖研究或周报业务。

对应 Rust 测试核查编译、路由、feedback、activation、并行故障窗口、身份和持久状态 roundtrip。实际运行结果只记录在 [开发台账](pilot-development-plan.md)，不在设计文档维护过时测试数。作者格式见 [作者契约](rust-graph-authoring-contract.md)。
