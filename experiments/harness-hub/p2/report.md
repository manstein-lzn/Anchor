# P2 事实记录：报告

执行：2026-10-21，按 [总体架构](../../../docs/harness-data-hub.md#当前职责与依赖) 第 5.2、5.3 节和 [任务书 §P2](../../../docs/archive/design-records-2026-11-04/harness-data-hub-tasks.md#p2-事实词汇表与记录适配器)，由主 Agent 直接实现。

## 做了什么

- **新 crate `anchor-facts`**：事实词汇表，是写侧和读侧唯一共享的契约。只依赖 `anchor-kernel`、serde 和 sha2，不包含存储和分析。内容包括：
  - 资源命名：`trace/{run}/{node}/{invocation}`，以及按内容寻址的 `config/{digest}`；
  - 两个 Validator：
    - `validator/observation`：只接受宿主代码以 Write 提交、内容合法、并且属于该 trace 的观察；
    - `validator/configuration`：配置清单创建后不可修改；
  - 授权规则 `RecorderOnly`：只允许 `anchor/recorder` 以宿主代码身份写 `trace/…`；
  - 内容格式 `Observation { version, invocation, facts }`，事实分三种：`lifecycle`、`model_turn`、`tool_call`；
  - 配置清单 `AgentConfiguration` 及其摘要；参数形状摘要 `arguments_shape`；失败分类 `classify_failure`。
- **Host 记录器 `fact_recorder.rs`**：
  - 观察事实由执行端口拥有，存在它自己的数据根 `state/kernel-facts/`，有独立的写入租约。用的是同一个 `Kernel::submit`、同一种 Record 格式。
  - 记录只是尽力而为：失败只写进 invocation 的 evidence（`fact_recording`），不改变执行结果；数据根不可用时就不记录，执行照常进行。
- **记录点**：
  - bridge 给每次工具调用加上开始和结束时间，以及 Goose 的 `toolCallId`；
  - usage 样本加上观察时间和缓存 token；
  - `GooseNodePort::execute` 在调用结束时组装配置清单和事实，提交一条记录。

## 设计上的两处调整（相对架构草案）

1. **观察事实存在执行端口自己的数据根，而不是 Run 的数据根。** 这样记录时的写盘不会和 Run 的裁决争用写锁，执行端口也不用去拿 Run 的内核，耦合最小。这正好符合"每个所有者存自己的历史"。数据中枢按"数据根 + 资源 + 位置"读取。
2. **每次调用只提交一条记录，里面是这次调用的全部事实，而不是每个事实各提交一次。** 实测每次提交要同步写盘 3 次，约 20 毫秒。这些事实在调用结束时一起上报，盖的是同一个时间，也没有逐条裁决的需要。逐条提交不会多带任何信息，开销却按事实数成倍增长：在固定 Goose 夹具里，一次调用要 55 到 216 毫秒；改成一条之后，固定约 39 毫秒。每个事实自己携带发生的时间。

## 真实模型验证

用和 P0 相同的夹具和渠道（平台默认 Flash），换成本次编译的 Host（`/tmp/anchor-p2/bin`），在隔离目录里串行跑了 T1 5 次、T2 10 次。15 次全部 completed，没有触发看门狗。原始数据在 `.local/harness-hub-p2/`，不入库。逐 Run 结果见 [runs.jsonl](runs.jsonl)，汇总见 [summary.json](summary.json)，复跑用 [analyze_p2.py](analyze_p2.py)。

| 检查 | 结果 |
| --- | --- |
| 每条记录被接受，并带内核时间和配置摘要 | 全部 |
| 每次调用对应一条 trace 记录、一个配置清单 | 全部 |
| 模型轮次数、工具调用数和 evidence 一致 | 全部 |
| 工具调用有开始和结束时间，以及 Goose 的调用 id | 全部（共 90 次，耗时中位数 66 毫秒） |
| 模型轮次有 `messageId` 和观察时间 | 全部 |
| 失败分类 | 2 次 `not_authorized`，和 P0 看到的是同一类接口摩擦 |
| 事实里出现业务内容 | **0**（用任务输入和答案的关键字扫描全部记录和配置清单） |
| 记录开销 | 每次调用中位数 39 毫秒，最多 40 毫秒；平均占 Run 墙钟时间的 **0.7%**；没有记录错误 |

## P0 的缺口

| 缺口 | 现状 |
| --- | --- |
| 每条内核记录的时间 | P1 补上 |
| 单次工具调用的耗时 | **补上**：由 bridge 在 Host 侧观察开始和结束 |
| AgentNode 的开始和结束时间 | **补上**：`lifecycle` 事实 |
| 每次提交的配置身份 | **补上**（观察事实这一侧）：配置清单包括 Graph 摘要、节点、模型及绑定、Goose 二进制、提示词和 Plugin 的摘要 |
| 模型轮次、工具调用、提交之间的因果 | **补上**：天然标识 `messageId`、`toolCallId`、`model_request`（第几次模型请求），以及 invocation 键 |
| 工具失败的类别 | **补上**：`not_authorized`、`invalid_arguments`、`completion_rejected`、`cancelled`、`not_executed`、`tool_error` |
| Op 节点的耗时 | **未做**，见下 |

## 已知边界

- **事实在调用结束时一次写入。** 如果调用中途 Host 崩溃，这次调用的事实会丢失；恢复后的调用写进同一个 trace，标记 `resumed`。代价是只丢中断那一次的观察，换来的是写入开销小、执行路径上的耦合小。
- **还没记录的**：
  - Op 节点的耗时；
  - Pilot 会话里的模型调用；
  - Run 历史中节点结果那条提交还没有盖配置摘要。中枢可以通过 run、node、invocation 把它和 trace 关联起来。

  这三项需要改 Runtime 或 Pilot，留到确有分析需要时再做。
- **失败分类依赖 Host 拒绝时的措辞。** 只记录类别，不记录文本。措辞改变时需要同步更新 `classify_failure`，现有单元测试固定了当前的措辞。
- **时间不保证单调**：用的是系统时钟。

## 门槛

按任务书的测试分级规则（Host 代码有改动），切片完成时跑一次。原始日志在 `/tmp/anchor-p2/gates/`。

| 门槛 | 结果 |
| --- | --- |
| 定向测试 | `anchor-facts` 6 项；Host 的记录器、evidence 转换、usage、bridge 共 23 项；`goose_acp` 15 项、`goose_completion` 9 项 |
| workspace | 第一次 **1 项失败**，见下面的说明；修复后重跑：135 个目标，**2,223 / 0 / 126**（比 P1 多 11 项，都是新测试） |
| 严格 Clippy、fmt、围栏 | 通过；修复后对 `anchor-facts` 补跑 Clippy，仍然通过 |
| 完整固定 Goose | **41 个目标，123 / 0 / 0** |
| 内核 | `git diff 9a5fa57 -- rust/anchor-kernel` 为空，C1 不变 |

**workspace 抓到的问题**：`arguments_shape` 原本假设 serde_json 的对象会按 key 排序，所以编码是规范的。单独测试 `anchor-facts` 时确实如此；但 workspace 里别的依赖打开了 serde_json 的 `preserve_order` 特性，对象改为保留插入顺序，于是"同一形状、不同 key 顺序"会得到不同的摘要。修复办法是显式排序。完整 Goose 回归是在修复之前跑的；这次修复只改了这一个纯函数，不影响执行路径，所以没有重跑。

另外，上面的真实模型验证用的是修复前的二进制，所以 `.local/harness-hub-p2/` 里的参数形状摘要和 key 的顺序有关；其他结论不受影响。
