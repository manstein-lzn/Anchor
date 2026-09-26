# Anchor Core Model Discussion Draft

> **历史归档（2026-09-25）**：保留当时的讨论、计划与验证证据，不代表当前实现或待办。
> 文中的“下一步”“待实施”“必须”及旧阅读顺序仅适用于当时阶段，不作为后续开发指令。
> 当前入口见 [项目 README](../../README.md)，唯一升级方向见 [Plugin 设计](../plugins.md)。

> Status: Discussion Draft  
> Purpose: 供 Anchor 开发 Agent 讨论下一阶段核心抽象与运行时边界  
> 核心目标：让 Anchor 保持极简概念模型，同时具备足够强的表达力，用于构建长期运行、可组合、可嵌套、可与现实世界持续交互的 Agent WorkGraph。

---

## 1. 背景

Anchor 希望提供一种类似 PyTorch 构建模型的方式来构建 Agent WorkGraph：

- 用户可以组合 `AgentNode`、`OpNode` 和 `Edge` 构建 Graph；
- Graph 可以进一步作为 SubGraph 被其他 Graph 嵌套和组合；
- Graph 经过编译后，SubGraph 被内联展开；
- 最终运行时面对的仍然是一张最简单的图：

```text
Graph = AgentNode + OpNode + Edge
```

Anchor 不希望为了每一种外部交互方式不断增加新的 Graph Primitive，例如：

- ToolNode
- HumanNode
- MCPNode
- HTTPNode
- WaitNode
- EventNode
- A2ANode
- MailboxNode
- WebhookNode

这些概念如果都可以通过代码执行表达，就应该尽量被统一到 `Op`。

核心哲学：

```text
Agent = Reasoning
Op    = Capability
Edge  = Coordination
Graph = Composition
```

---

# 2. 核心抽象

## 2.1 Graph

Graph 只由两种 Node 和 Edge 构成：

```text
Graph
 ├── AgentNode
 ├── OpNode
 └── Edge
```

Graph 描述的是：

> 一项工作的结构，以及工作之间的依赖与协调关系。

Graph 不直接描述：

- HTTP
- Webhook
- 企业微信
- Kafka
- MCP
- A2A
- Shell
- Python
- Database
- Human Interaction
- Timer
- External Event

这些都不应该成为新的 Graph Primitive。

---

## 2.2 AgentNode

`AgentNode` 表示需要模型进行理解、推理、判断、规划和自适应执行的部分。

例如：

```text
需求分析
研究
设计
代码实现
文档撰写
问题判断
方案评审
```

定义：

> AgentNode = 使用模型完成无法在设计阶段完全程序化描述的工作单元。

AgentNode 可以拥有若干可调用的 `Op`：

```yaml
agent:
  name: coder
  ops:
    - bash
    - file.read
    - file.write
    - git
    - test.run
    - human.ask
```

Agent 自己决定：

- 是否调用某个 Op；
- 何时调用；
- 调用几次；
- 根据结果如何继续工作。

这些动态调用不应该改变 Graph topology。

---

## 2.3 Op

`Op` 是整个设计中最重要的统一抽象。

定义：

> Op = Agent 之外一切可以由程序明确调用的 Capability。

例如：

```text
bash
python
file.read
file.write
git
pytest
web.fetch
HTTP API
MCP
database.query
send.email
send.wecom
human.ask
agent.ask
agent.delegate
wait.event
wait.until
deploy
compile
simulation.run
```

它们在 Anchor Core 中不需要属于不同的一级概念。

只要一个能力可以通过程序调用，它就是一个 `Op`。

因此：

```text
Tool       ⊂ Op
Capability = Op
Connector  = Op implementation
MCP Tool   = Op implementation
API call   = Op implementation
Human ask  = Op implementation
A2A call   = Op implementation
```

Anchor 不需要同时维护 Tool、Capability、Connector、Action 等多套相似抽象。

---

# 3. Op 的两种使用方式

同一个 Op 应该同时支持两种使用模式。

## 3.1 Op 作为 OpNode

Graph 作者明确规定某一步必须执行某个操作：

```text
ImplementAgent
      |
      v
  RunTests Op
      |
      v
 ReviewAgent
```

这里 `RunTests` 是 Graph 的静态结构。

含义：

> 不管前面的 Agent 怎么思考，这一步测试都是业务流程的一部分。

可以理解为：

```text
OpNode = Graph 中对某个 Op 的声明式调用
```

---

## 3.2 Op 挂载给 Agent

同一个 `RunTests` Op 也可以作为 Agent capability：

```text
CodingAgent
    |
    +-- file.read
    +-- file.write
    +-- bash
    +-- run_tests
    +-- git
```

Agent 可以在自己的工作过程中：

```text
写代码
  ↓
run_tests()
  ↓
发现失败
  ↓
修改代码
  ↓
run_tests()
```

Graph 上仍然只有：

```text
CodingAgent
     |
     v
ReviewAgent
```

这些 Agent 内部动态产生的 OpCall 不应该进入 Graph topology。

---

## 3.3 统一运行时

两种形式底层应共用同一个执行系统：

```text
                OpExecutor

        +-----------+-----------+
        |                       |
      OpNode              Agent Op Call
        |                       |
        +-----------+-----------+
                    |
                    v
                  OpCall
```

这样可以统一：

- 参数校验；
- 权限控制；
- 日志；
- tracing；
- retry；
- timeout；
- idempotency；
- side-effect audit；
- secret management；
- external connector。

---

# 4. OpCall：最小而关键的异步语义

为了让 Anchor 可以处理从毫秒到数天的外部交互，而不增加新的 Graph Primitive，建议给 `OpCall` 一个极小的统一生命周期。

最小模型：

```text
OpCall
   |
   +--> Completed(result)

or

OpCall
   |
   +--> Pending(handle)
            |
            |  external completion
            v
        Completed(result)
```

即：

```text
invoke(input)
    -> Completed(output)

or

invoke(input)
    -> Pending(handle)
```

---

## 4.1 为什么 Pending 很重要

下面这些操作本质上都可以统一：

```text
bash                     100 ms
HTTP                     500 ms
LLM API                   20 s
remote job                10 min
simulation                 2 h
human reply                5 h
approval                    3 d
external business event    ? 
```

时间尺度不应该改变 Anchor 编程模型。

例如：

```text
human.ask(
    user="wang",
    question="这个模块目标频率是多少？"
)
```

可以返回：

```text
Pending(handle="opcall_8291")
```

数小时后外部系统收到回复：

```text
350 MHz，最好留到 400 MHz
```

Connector 完成：

```text
complete(
    handle="opcall_8291",
    result="350 MHz，最好留到 400 MHz"
)
```

Anchor 恢复对应 Agent 执行。

对 Agent 而言，它只是调用了一个普通 Op。

---

# 5. 与现实世界交互

Anchor 不需要额外引入一个复杂的 Interaction 子系统作为核心抽象。

核心原则：

> Graph 对现实世界的一切主动交互，最终都表现为 Op。

形式：

```text
Agent
  |
  v
 Op
  |
  v
World
```

或者：

```text
Graph
  |
  v
OpNode
  |
  v
World
```

例如：

```text
send_wecom()
send_email()
publish_kafka()
http.post()
update_jira()
create_pr()
human.ask()
agent.ask()
agent.delegate()
```

全部统一为 Op。

---

# 6. Human / Agent / External System 不需要进入核心模型

例如：

```text
Agent A
   |
human.ask()
   |
   v
企业微信
   |
   v
Human
```

从 Anchor Core 看：

```text
Agent A
   |
   v
  Op
```

类似地：

```text
Agent A
   |
agent.ask()
   |
   v
A2A
   |
   v
Agent B
```

Anchor Core 仍然只看到：

```text
Agent A
   |
   v
  Op
```

因此：

```text
Agent ↔ Human
Agent ↔ Agent
Agent ↔ API
Agent ↔ MCP
Agent ↔ Device
Agent ↔ Database
```

在核心抽象中没有区别。

区别存在于具体 Op implementation 中，而不是 Graph IR 中。

---

# 7. 世界主动进入 Anchor

Anchor 仍然需要解决外部世界如何触发一个 Graph。

但这不应该进入 Graph IR。

Graph 本质上只需要：

```text
GraphDefinition
      +
Initial Input
      |
      v
GraphRun
```

至于 `GraphRun` 是如何创建的，可以来自：

```text
REST API
Webhook
Cron
Kafka
GitHub
GitLab
企业微信
另一个 Graph
CLI
UI
```

这些属于 deployment / trigger adapter。

统一后：

```text
External Event
      |
 Trigger Adapter
      |
      v
 create_run(graph, input)
```

因此：

> Event 是启动 GraphRun 的方式，而不是 Graph 的 Primitive。

---

# 8. Graph 运行过程中收到外部事件

如果一个正在运行的 Agent 正在等待一个外部事件，那么这个事件可以被看作某个 `Pending OpCall` 的完成。

例如：

### 等待人类回答

```text
human.ask()
   |
Pending
```

收到回答：

```text
complete(handle, answer)
```

### 等待审批

```text
approval.wait()
   |
Pending
```

收到审批：

```text
complete(handle, approval_result)
```

### 等待远程 Agent

```text
agent.delegate()
   |
Pending
```

远程任务完成：

```text
complete(handle, result)
```

### 等待文件

```text
wait.file()
   |
Pending
```

文件出现：

```text
complete(handle, file_ref)
```

### 等待时间

```text
wait.until(timestamp)
   |
Pending
```

时间到：

```text
complete(handle, timestamp)
```

因此不必为：

```text
message
signal
reply
approval
timer
webhook
event
```

分别设计不同的 Graph Primitive。

运行时只需要：

```text
Pending OpCall
      |
   completion
      |
      v
resume
```

---

# 9. Graph Input / Output 仍然有价值

虽然 Graph 内部可以通过 Op 随时与现实世界交互，但 Graph 仍然应该保留正式 Input / Output。

原因：

> Input / Output 不是为了限制 Graph 与世界交互，而是为了定义 Graph 作为模块时的稳定接口。

例如：

```text
TechnicalReview(
    document,
    standard
)
    |
    v
{
    report,
    decision,
    issues
}
```

这样 Graph 才能：

- 被测试；
- 被复用；
- 被嵌套；
- 被版本化；
- 成为另一个 Graph 的模块。

因此：

```text
Input / Output
    = module interface

Op
    = runtime capability
```

两者并不冲突。

---

# 10. SubGraph

Anchor 应支持 Graph 嵌套：

```text
ResearchGraph
    |
    +-- SearchAgent
    +-- AnalysisGraph
    |      |
    |      +-- ...
    |
    +-- WriterAgent
```

但是 SubGraph 只是一种开发期和组合期抽象。

经过编译：

```text
High-Level Graph
      |
      | inline / compile
      v
Flat Graph
```

最终 runtime 仍然只面对：

```text
AgentNode
OpNode
Edge
```

因此不需要额外设计复杂的 SubGraph Runtime。

核心原则：

> Graph 可以递归组合，但 Runtime IR 应尽可能简单。

---

# 11. Anchor 推荐的最终核心模型

```text
                 Anchor

                   Graph
                     |
           +---------+---------+
           |                   |
       AgentNode             OpNode
           |                   |
           | mounted Ops       | invokes
           v                   v
                    Op
                     |
                +----+----+
                |         |
           Completed    Pending
                          |
                       resume
```

Graph 之外：

```text
External World
      |
 Trigger Adapter
      |
      v
   GraphRun
      |
      +---- Agent -> Op -> World
      |
      +---- OpNode -> Op -> World
```

---

# 12. 核心哲学

Anchor 可以尽量坚持以下几个原则。

## 12.1 极少 Primitive

最终 Core IR：

```text
Graph
AgentNode
OpNode
Edge
```

如果一个能力可以通过代码调用表达，就优先表达成 Op，而不是新增 Primitive。

---

## 12.2 Agent 负责不确定性

Agent 用于：

```text
reasoning
planning
judgement
adaptation
research
coding
writing
decision under uncertainty
```

---

## 12.3 Op 负责确定的 Capability

Op 用于：

```text
programmatic action
tool execution
external integration
waiting
messaging
side effect
deterministic computation
```

注意：

这里的“确定”不是说 Op 一定是纯函数。

例如：

```text
human.ask()
```

结果显然不确定。

这里强调的是：

> 调用机制本身可以被程序明确表达和管理。

---

## 12.4 Edge 负责 WorkGraph 的结构

Edge 表达：

```text
dependency
control relationship
data/workspace relationship
```

不要把 Agent 每次动态调用 Op 产生的关系都画成 Edge。

否则 WorkGraph 会被运行时细节污染。

---

## 12.5 Runtime complexity should not leak into programming model

内部可以非常复杂：

```text
webhook routing
message correlation
worker suspend
sandbox snapshot
event persistence
retry
network
A2A
MCP
enterprise messaging
scheduler
```

但 Anchor 用户看到的仍然应该只是：

```text
Agent
Op
Edge
Graph
```

---

# 13. 一个完整示例

目标：

> 一个长期运行的软件 Issue 自动处理流程。

Graph：

```text
UnderstandIssue
      |
      v
Implementation
      |
      v
 RunTests
      |
      v
   Review
```

定义：

```yaml
nodes:

  understand:
    type: agent
    agent: issue_analyst
    ops:
      - repo.read
      - issue.read
      - web.fetch
      - human.ask

  implement:
    type: agent
    agent: coder
    ops:
      - file.read
      - file.write
      - bash
      - git
      - human.ask
      - agent.ask

  test:
    type: op
    op: test.run

  review:
    type: agent
    agent: reviewer
    ops:
      - repo.read
      - git.diff
```

运行过程中：

```text
Implementation Agent
      |
      | human.ask("API兼容性要求是什么？")
      v
    Pending
```

此时 Graph 不需要增加 HumanNode。

外部两小时以后回复：

```text
必须保持 backward compatibility
```

对应 OpCall 完成。

Agent 恢复：

```text
Pending
   |
Completed(result)
   |
   v
继续 Implementation
```

然后：

```text
Implementation
      |
      v
 RunTests OpNode
```

这里测试是业务流程强制步骤，因此被显式放入 Graph。

同样的 `test.run` 也完全可以同时挂载给 Implementation Agent。

---

# 14. 与 PyTorch 类比

Anchor 的长期目标可以类比：

```text
PyTorch
Tensor
Module
forward()
Module composition
Backend
```

对应：

```text
Anchor
Workspace / Artifact
Graph
run()
Graph composition
Execution backend
```

其中：

```text
Graph
  = composable work module

Agent
  = adaptive computation

Op
  = capability

Edge
  = composition structure
```

Graph 可以像 `nn.Module` 一样被组合。

SubGraph 可以嵌套。

编译阶段可以 inline。

最终执行 IR 保持极简。

---

# 15. 与 AX / Runtime Backend 的边界

Anchor 不应该继续向底层 execution infrastructure 无限扩张。

建议边界：

Anchor owns:

```text
Graph semantics
AgentNode / OpNode
Edge semantics
Graph compilation
Workspace / Artifact semantics
GraphRun state
OpCall lifecycle
Agent runtime contract
Capability permission
Execution trace
```

Execution Backend owns:

```text
sandbox
worker
process
container
resource scheduling
snapshot
hibernate
resume
cluster
network isolation
```

AX / Agent Substrate 可以未来成为一种 Execution Backend。

Anchor 不应把 AX 作为核心依赖。

---

# 16. 当前最值得讨论的设计问题

以下问题建议开发阶段优先讨论，而不是马上增加更多 Feature。

## Q1. Op 到底是什么？

建议讨论是否接受最强定义：

> Op = Agent 之外所有可以通过程序调用的 Capability。

如果接受，应删除或弱化独立的：

```text
Tool
Capability
Connector
Action
```

等重复概念。

---

## Q2. OpNode 是否只是 Bound OpCall？

是否可以定义：

```text
OpNode = Graph 中声明式、静态存在的 Op invocation
```

而 Agent dynamic tool call 与 OpNode 最终统一进入：

```text
OpExecutor -> OpCall
```

---

## Q3. OpCall 是否应该原生支持 Pending？

建议将下面模型作为一等运行时语义：

```text
Completed(result)

Pending(handle)
```

这是 Anchor 支持长期工作流、Human-in-the-loop、A2A、异步业务系统的关键。

---

## Q4. Pending 时 AgentNode 如何恢复？

需要明确：

```text
Agent 调用 Op
   |
Pending
   |
Agent execution suspension
   |
external completion
   |
resume
```

这里需要进一步讨论：

- 保存什么状态；
- model session 是否保留；
- 是否从 canonical state 重建；
- 如何恢复 tool call；
- worker 是否释放；
- workspace 如何保持一致。

但这些应该是 runtime 问题，而不是新增 Graph Primitive。

---

## Q5. 一个 Agent 是否允许多个并发 Pending OpCall？

例如 Agent：

```text
ask human A
ask agent B
start simulation C

继续处理其他事情

之后等待其中一个或多个结果
```

这是很有价值的能力，但可能显著增加 Agent execution semantics 复杂度。

需要判断：

- v1 是否只支持 blocking async Op；
- 后续是否支持 future/task handle；
- 是否需要 `await_any / await_all`。

建议不要过早引入。

---

## Q6. Graph 的 Input / Output 和 Workspace 如何结合？

需要定义：

- Graph input 是否映射为 workspace；
- structured scalar input 如何表示；
- artifact 是否统一通过 URI/reference；
- output 是否必须显式声明；
- SubGraph compile 后如何维护 input/output mapping。

---

## Q7. Edge 到底表达什么？

需要尽量保持 Edge 语义稳定。

建议明确：

```text
control dependency?
data dependency?
workspace dependency?
selection/routing?
```

避免 Edge 随着需求增长承担过多语义。

---

## Q8. 什么情况下必须使用 OpNode？

一个简单判断标准可能是：

> 如果某个 Capability invocation 是业务流程本身的一部分，就使用 OpNode。

> 如果它只是 Agent 为完成自身任务而自主选择的手段，就作为 mounted Op。

例如：

```text
Run compliance check
```

如果法律审查流程规定必须执行：

```text
OpNode
```

如果 Coding Agent 只是想临时跑一下 grep：

```text
mounted Op
```

这个规则值得固化。

---

# 17. 暂时明确不做的事情

为了保护 Anchor 的核心简洁性，当前建议不要增加：

```text
HumanNode
ToolNode
EventNode
WaitNode
WebhookNode
MCPNode
A2ANode
InteractionNode
MailboxNode
TimerNode
```

除非未来发现：

> 某个概念无法被 Agent / Op / Edge 的组合自然表达，并且这种缺陷是结构性的，而不是实现不方便。

否则优先保持：

```text
Agent
Op
Edge
Graph
```

---

# 18. 建议下一阶段工作

第一阶段建议不是继续堆功能，而是验证这个极简模型是否足够。

建议实现 4 个代表性案例：

### Case A — Coding Workflow

```text
Analyze -> Implement -> Test -> Review
```

验证：

- mounted filesystem/bash/git Ops；
- explicit test OpNode；
- workspace dataflow。

### Case B — Human Collaboration

Agent 执行到一半：

```text
human.ask()
```

Op Pending 数小时后恢复。

验证：

- durable OpCall；
- suspend/resume；
- external completion。

### Case C — Agent-to-Agent

Agent：

```text
agent.delegate()
```

远程 Agent 工作完成后返回结果。

验证：

- 长期 Pending；
- external agent connector；
- correlation。

### Case D — Nested Graph

```text
Graph A
  -> Graph B
       -> Agent
       -> Op
  -> Agent
```

编译后：

```text
Flat Graph
```

验证：

- SubGraph interface；
- compile/inline；
- namespace；
- edge rewiring；
- run trace。

如果这四个案例都能够在不增加新 Primitive 的情况下自然表达，说明核心模型基本成立。

---

# 19. 最终判断

Anchor 不需要努力建模世界上的每一种 Agent Interaction。

更好的方向是：

```text
Agent = 能思考的人

Op = 他能够做的一切事情

Edge = 工作的组织结构

Graph = 一整套工作程序
```

Agent 可以在执行过程中自主使用任何被授权的 Op。

Op 可以立即完成，也可以等待数分钟、数小时甚至数天后完成。

Graph 可以递归组合，但在编译以后重新变成：

```text
Agent + Op + Edge
```

因此 Anchor 的复杂度应该主要存在于实现层，而不是概念层。

最终希望达到：

> **简单的 Programming Model，强大的 Runtime。**

或者更简洁：

> **Keep the graph simple. Put capability in Op. Put intelligence in Agent.**
