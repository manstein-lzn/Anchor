# RSI 设计参考

**状态**：未来研究参考，不是当前 Runtime 依赖。  
**更新日期**：2026-09-12

## WikiSkill

**论文**：*WikiSkill: Compiling Agent Experience into Persistent Knowledge for Skill Evolution*  
**来源**：[arXiv 2608.27454v1](https://arxiv.org/html/2608.27454v1)  
**定位**：RSI / 跨 Run 经验演化参考。

WikiSkill 将经验处理拆成三类对象：

```text
Raw Layer   不可变执行轨迹、工具调用、观察、结果
Wiki Layer  从轨迹提炼的模式、失败原因和策略
Skill Layer 当前实际注入 Agent 的可执行能力
```

它提供的主要参考是：

- 原始执行证据不被派生经验覆盖；
- 派生经验可以持续积累、修订和冲突处理；
- skill 修改先作为 candidate，再经过验证集评估；
- 验证失败时回滚 skill，但保留原始经验和谱系；
- 执行 Agent 不应默认获得全部经验库，而应使用有限、选择后的投影；
- 经验的有效性取决于任务分布、模型、工具和前置条件，不能由一次成功推导出普遍真值。

## 对 Anchor 的映射

| WikiSkill | Anchor 的未来对象 | 当前状态 |
|---|---|---|
| Raw Layer | EventLog、model recording、tool operation、artifact、workspace revision | 已有，但不为 WikiSkill 特制 |
| Wiki Layer | candidate experience、failure pattern、attribution hypothesis | 未实现 |
| Skill Layer | Agent capability / prompt / tool policy revision | 版本化能力已有；自动演化未实现 |
| Validation split | offline harness / held-out evaluation | 部分已有；不能据此宣称 RSI 已有 |
| Rollback | 新 capability version、拒绝或退役 | 版本和拒绝边界已有；经验退役协议未实现 |

## 采纳边界

### 参考采用

```text
Raw -> Derived Experience -> Candidate Capability -> Independent Validation -> Approval
```

候选经验至少应记录：

- 来源 Run、NodeRun、attempt 和 artifact；
- graph/runtime/model/tool 版本；
- 适用任务族和前置条件；
- 正面结果、负面结果和验证判据；
- candidate / accepted / rejected / retired 状态；
- 替代关系、回滚路径和版本谱系。

### 明确不采用

- 不把 Wiki Layer 当作 Anchor canonical recovery state；
- 不让 WikiSkill 或其他经验系统绕过 I8 自动注入节点；
- 不因一次成功或 LLM 自评就晋升经验；
- 不让经验维护器直接修改已发布 Graph、Verifier 门槛或 Contract；
- 不把 Wiki 全量放进当前 Agent Context；
- 不把论文中的 benchmark 结果当作 Anchor 的普遍保证。

## 与当前 Context Engine 的关系

WikiSkill 不是 Context Engine 的内核。未来它最多通过一个受约束的候选源接口参与：

```text
ExperienceProvider
    -> candidate references
    -> Anchor scope/version checks
    -> Context Engine selection
    -> bounded materialization
```

Context Engine 仍然负责可见性、容量、来源和投影；WikiSkill 类系统负责未来经验的提炼与候选演化。

## 与 P4 RSI 的关系

WikiSkill 作为 P4 RSI 的设计参考，支持以下实验路线：

```text
Run traces
    -> experience candidates
    -> offline skill/prompt candidates
    -> held-out evaluation
    -> human or explicit policy approval
    -> new immutable capability revision
```

在 P0–P2 运行时硬化和 Context Engine 基础边界完成前，不实现这条在线闭环。

## 证据限制

WikiSkill 的实验结果支持其论文所测试的任务、模型和验证设置下的改善，不证明经验在任意新 Run 中普遍有效。论文本身也显示，开放全部 Wiki 访问可能不如经过选择的经验投影；因此它支持“经验库与执行上下文分离”，不支持“经验越多越好”。
