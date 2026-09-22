# Anchor Agent Harness 深度调研报告

> 调研日期：2026-09-22  
> 目标：为 Anchor 的 **Agent Node 内部执行底座**选择尽量少自研、又能可靠支持长任务的方案。  
> 结论对象：mini-swe-agent 2.4.6、PydanticAI 2.46.0 + 第一方 `pydantic-ai-harness` 0.32.0；补充筛选 LangChain Deep Agents 0.7.15。  
> 证据原则：优先官方文档、发布说明和源码；未实际执行的能力明确标为“未验证”。

---

## 1. 结论摘要

### 1.1 推荐结论

**建议进入“受控迁移验证”：把 Anchor 的 Agent Node 内部模型循环从 mini-swe-agent 迁移到 `PydanticAI 2.46.0 + pydantic-ai-harness 0.32.0`，但不要迁移 Anchor 的 Graph、workspace/Git、bubblewrap 沙箱、CLI 能力体系和 op 节点，也不要直接采用 PydanticAI Harness 的 Coder/Shell 全家桶。**

换句话说，推荐的不是“用 PydanticAI 重写 Anchor”，而是：

```text
Anchor Graph / Scheduler / Workspace / Git / Routing / op
                         │
                 Anchor-owned Node API
                         │
              PydanticAI Node Adapter
          ┌──────────────┼────────────────┐
          │              │                │
   context control   event/cancel   step persistence
   pydantic harness   pydantic core  pydantic harness
          │
        bash
          │
Anchor existing bubblewrap sandbox + CLI
```

这次调研中最重要的变化是：**到 2026-09，PydanticAI 已经不再只是“提供 history processor 钩子、压缩算法仍由业务自己写”的框架。** 第一方 `pydantic-ai-harness` 已经提供可直接组合的模型无关上下文压缩、token/window 触发、tool-call/tool-return 配对保护、pin、上下文使用量事件、大工具输出 spill、StepPersistence 等能力。它已经跨过了“能否实质减少 Anchor harness 自研”的门槛。

相较之下，mini-swe-agent 2.4.6 依旧非常适合作为一个**简单、透明、单 bash 的短循环 Agent**，而且与 Anchor 当前集成高度匹配；但它没有把 Anchor 当前最痛的长任务问题——累计上下文预算、自动压缩、压缩持久化、大输出无损外置、崩溃边界记录——吸收到框架本体。因此继续 mini 并不是坏方案，只是意味着 **Anchor 要继续自己成为 harness 框架作者**。

### 1.2 为什么不是“直接全面迁移”

PydanticAI + Harness 仍然有四个不能被包装成“已经解决”的缺口：

1. **不是 exactly-once。** `StepPersistence` 明确记录 `started/completed/failed`，崩溃时可以暴露 `unknown_after_crash`，但官方明确说明它不是完整 graph-state checkpoint，也没有自动执行恢复；外部副作用与本地 SQLite 记录无法原子提交。副作用幂等、对账、是否重放，仍属于 Anchor/orchestrator。
2. **provider 真正返回 context-window overflow 后自动压缩并重试，没有找到官方保证。** Harness 很强的是“请求前主动控制”。如果真实部署窗口与 registry 不一致，provider 仍可能先拒绝。因此建议 Anchor 保留一个很薄的“一次性 overflow recovery guard”。
3. **`ToolOutputLimits(Spill)` 会注册第二个模型可见工具 `read_tool_result`。** 若“模型只能看到 bash”是绝对硬约束，就不能零代码获得 Harness 自带的无损 spill/paging，需要 Anchor 自己保留一个薄的 spool/CLI 适配。
4. **Graph 等待/恢复仍是 Anchor 责任。** PydanticAI 的 deferred tools 能表达“当前 run 结束，外部补结果后继续”，但谁把 Node 标为 WAITING、持久化 resume token、进程重启后何时再调度，仍必须由 Anchor Graph/runtime 定义。

### 1.3 置信度

| 结论 | 置信度 | 原因 |
|---|---:|---|
| PydanticAI Harness 能显著减少上下文治理自研 | 高 | 官方文档与当前 API 明确覆盖 compaction、pin、window、usage、ToolOutputLimits |
| 可无侵入保留 Anchor Graph / workspace / Git / sandbox | 高 | 推荐只替换 Node 内模型循环；自定义单 bash tool 可直接映射现有沙箱 |
| StepPersistence 能提升崩溃可观测与恢复决策 | 高 | 官方明确提供 snapshot、tool-effect ledger、`unknown_after_crash` |
| StepPersistence 可直接替代 Anchor 全部恢复代码 | 低 | 官方明确否定完整 graph-state restore / exactly-once / automatic execution recovery |
| PydanticAI 真实模型下摘要质量满足 Anchor 长任务 | 中低 | 需要 Anchor 的真实 coding/research workload benchmark；文档能力不能证明摘要质量 |
| 最终应正式迁移 | 中高 | 架构匹配明显，但仍必须完成任务书要求的 fault-injection 与真实模型 A/B 后再落 ADR |

### 1.4 最简决策

如果今天必须做一个工程方向选择：

- **目标是“尽量不再自己造 context/recovery harness”**：选 **PydanticAI + pydantic-ai-harness**，做薄适配。
- **目标是“近期一行主逻辑都不想动，只求继续稳定”**：继续 **mini-swe-agent**，但接受未来上下文治理和恢复机制主要由 Anchor 自己维护。
- **不建议 Deep Agents 作为 Anchor Node 内核**：它确实有成熟的 summarization/offload/persistence/HITL，但其 LangGraph/Filesystem/SubAgent middleware 与 Anchor 已有 Graph/workspace/runtime 重叠较大，相当于在一张 Graph 的 Node 里再塞一张框架 Graph，省下部分 harness 代码的同时增加了状态模型和升级面的复杂度。

---

## 2. 版本与证据范围

### 2.1 Anchor 基线

本报告**没有获得 Anchor 仓库源码访问**，因此没有声称检查 `README.md`、`OPEN.md`、ADR-056 或 `src/anchor/...`。关于 Anchor 当前实现的判断仅以调研任务书为事实基线：

- Python + React；Graph 负责 nodes/deps/routing/loops，op 不依赖模型。
- Node 独立 workspace；跨图循环保留；执行后由 Anchor 创建 Git commit。
- 上游只读挂载 `/in/<node>`，当前节点写 `/workspace`。
- Agent 当前为 mini-swe-agent；模型只看到 bash，真实执行经过 bubblewrap。
- scholarly、done、route 均以 CLI 形式通过 bash 使用。
- 当前累积完整 model/tool 消息，没有自动 token 预算、压缩或 overflow recovery。
- Anchor 自己维护 trace、恢复、沙箱、完成协议；尚无任意崩溃点 exactly-once 保证。

**迁移前项目方仍需补证**：

1. 当前 `TracingAgent` 保存的 trace schema 以及哪些 UI/日志消费者依赖它。
2. `SandboxEnvironment.execute()` 对 timeout、进程组 kill、stdout/stderr 截断的精确语义。
3. `anchor-done` / `anchor-route` 的退出码与机器可解析协议。
4. `_task/_handed/_agent_for` 的恢复调用点及 resume identity。
5. 当前预算是按 Node run、graph run 还是全任务累计。
6. bubblewrap 中 workspace 与 `/in` mount 在 crash/retry 后的稳定性。

### 2.2 候选版本

| 候选 | 调研版本 | 发布/提交 | 状态 |
|---|---|---|---|
| mini-swe-agent | **2.4.6** | 2026-07-23, `a83fcae` | GitHub 当前 latest stable |
| PydanticAI | **2.46.0** | 2026-09-18, `c4898ab` | 当前 latest stable；项目标记 Production/Stable |
| pydantic-ai-harness | **0.32.0** | 2026-09-18, `434649f` | 第一方扩展；0.x，pyproject 标记 Alpha |
| LangChain Deep Agents | **0.7.15** | 2026-09-16, `0f5a2b5` | GitHub releases 当前 latest；main changelog 已出现 0.7.16 (2026-09-21)，说明发布节奏很快 |

许可证：mini-swe-agent、PydanticAI、pydantic-ai-harness 均为 MIT（已核官方 LICENSE/pyproject）。

### 2.3 本次“实际执行”范围

当前研究执行环境中未安装 `minisweagent`、`pydantic_ai`、`pydantic_ai_harness`、`deepagents`，且无可用于联网安装依赖/调用真实 provider 的凭证。因此：

- **已完成**：官方文档、release、关键源码行为核查；接口级设计分析。
- **未完成**：任务书要求的同机 package PoC、真实模型摘要质量、真实 provider context-overflow 注入、实际 SIGKILL fault injection。
- **交付中提供**：精确版本 pin、最小控制流 probe、基线 introspection probe、fault-injection runbook。

所以第 4 节严格把“文档/源码确认”和“运行通过”分开，不用推测填“通过”。

---

## 3. 能力矩阵

能力标签：**内置** / **官方扩展** / **仅有钩子** / **需自研** / **不支持 / 未验证**。

### 3.1 上下文管理（最高优先级）

| 能力 | mini-swe-agent 2.4.6 | PydanticAI 2.46 + Harness 0.32 | Deep Agents 0.7.15 | Anchor 仍需承担 | 关键风险/限制 |
|---|---|---|---|---|---|
| 累计上下文预算 | **需自研**。核心 loop 累积 `messages`，有 step/cost/time limit，但没有累计 context window policy | **官方扩展**。Compaction 可按 messages/tokens/fraction 触发；`ReportContextUsage` 可上报 | **内置/框架能力**，具 summarization/context offload | 给自托管/网关模型配置真实 context window | registry 错误可导致触发点过晚 |
| 自动压缩 | **需自研** | **官方扩展**：SlidingWindow、ClearToolResults、DeduplicateFileReads、Summarizing、Tiered、Fallback 等 | **内置** summarization middleware | 选择策略与阈值；对业务约束做 eval | summary 仍可能事实丢失；LLM summarization 有成本 |
| tool-call / tool-return 配对 | mini 自己按简单循环生成；裁剪历史若自研必须自己保证 | **官方扩展**：Harness compaction 明确保证 pairing | LangGraph message machinery 提供结构化历史 | 适配层不得再做无结构字符串裁剪 | 自定义 history mutation 仍可能破坏结构 |
| system/关键约束保留 | **需自研** | system prompt 不作为普通历史随意丢弃；**官方扩展 `pin()`** 允许关键 durable task state 在 shipped strategies 中保持 | middleware 有自己的 system/context 机制 | 将“不可丢任务状态”显式 pin，不要把所有历史 pin | pin 太多会重新制造上下文膨胀 |
| token 估算/模型窗口 | **需自研** | **官方扩展**：provider usage anchor + tokenizer/heuristic；`max_fraction`; `context_window` override | 框架内部预算能力较强 | 对公司私有模型显式配置 `context_window`；不要盲信 registry | 自托管模型 ID、tier-gated window 容易识别错误 |
| 为输出留余量 | **需自研** | 可用 compaction target + UsageLimits/per-request input cap 组合；没有看到一个单独叫“reserve_output_tokens”的统一按钮 | 可配置输入预算 | Anchor profile 定义安全余量，如只用 75–85% 窗口 | 模型 reasoning/output 上限变化需压测 |
| provider 拒绝 context overflow 后自动救回 | LiteLLM canonical path把 `ContextWindowExceededError` 列入 abort exceptions：**不重试，但也不压缩恢复** | **不支持 / 未验证**。官方主要保证请求前 compaction；未找到 parent provider overflow → force compact → retry 的稳定承诺 | changelog 有 bounded compaction recovery，但 Anchor 场景未实测 | 写一个**一次性** overflow guard：识别已知异常 → 更激进 compact → retry once | provider 错误类型不统一；不可无限重试 |
| 大 tool output 控制 | mini 默认 observation 对 >10k chars head/tail 截断，提示模型改命令/落文件；**有损** | **官方扩展** `ToolOutputLimits`: truncate/spill/summarize；默认 >=10k spill，失败再 truncate | 内置 tool result offload | 决定是否接受额外 `read_tool_result`；制定 TTL | Harness Spill 会破坏“只有 bash 一个模型工具”的严格形状 |
| 大输出无损分页找回 | **需自研/靠 bash 手动重定向文件** | **官方扩展** Spill + `read_tool_result(handle, offset, limit, from_end, pattern)` | 有 offload / filesystem 读取 | 如果必须单 bash，自建很薄 spool CLI | spill 文件默认持久保留，清理策略需应用决定 |
| 压缩结果持久化 | mini 没有内建 | compaction 修改会进入 run history；**官方扩展 StepPersistence** 可保存 snapshots；summary 在 durable 场景有 replay 设计 | LangGraph checkpointing | 选 File/SQLite store；定义 retention | snapshot 不等于完整 Graph checkpoint |
| 压缩成本/延迟 | **自研后自负** | Harness 暴露 summary 请求用量；Tiered 允许先零 LLM 策略后总结 | middleware 自己执行总结 | 监控 summary 次数与 cost；用独立便宜模型是否可接受需评估 | history rewrite 会使 provider prompt cache 从修改点失效 |

**上下文结论：**这是 PydanticAI + Harness 相对 mini 的决定性优势。它不只是给 hook，而是已经包含实际策略实现。对本任务“少维护代码”的目标，这一项足以让迁移值得做 PoC。

### 3.2 bash、工具生命周期与能力管理

| 能力 | mini | PydanticAI + Harness | Anchor 仍需承担 | 结论 |
|---|---|---|---|---|
| 只暴露一个 bash | **内置、天然契合**；LiteLLM 模型明确 `tools=[BASH_TOOL]` | **内置可实现**：只注册一个自定义 bash function tool，不使用 Coder/Shell | bash wrapper 映射现有 SandboxEnvironment | 两者都可；Pydantic 不要求工具拆碎 |
| 复用现有 bubblewrap | **已在 Anchor 实现** | **可直接复用**，工具函数内部调用现有 sandbox，无需 Pydantic 自带 sandbox | sandbox enforcement 全部继续归 Anchor | 推荐保留 |
| 工具参数校验 | 简单固定 bash schema | **内置** Pydantic schema validation | command policy 仍属于 sandbox/Anchor | Pydantic 更完整但不是核心收益 |
| 工具 timeout | 主要由 Environment | Pydantic 可限制 async；同步线程无法强杀 | **必须继续由进程级 sandbox 实现 timeout/kill** | 不要把 framework cancel 当 OS kill |
| 工具 cancel | mini 主要依赖外层中断 | Pydantic **内置** `CancellationToken` / `RunContext.cancel()`；async 工具协作取消 | bash 子进程组实际 kill | Pydantic 明显更好 |
| 自动重试副作用 | mini model/query retry相对简单 | Pydantic工具 validation/`ModelRetry` 有 retry budget；可设 `retries=0`；`ToolFailed` 可把失败返回模型而不要求重复调用 | 对 bash 强制 `sequential=True`, `retries=0`; 不在已执行命令后 raise ModelRetry | 防止重复 side effect |
| 一个响应多命令 | mini actions 顺序列表执行 | Pydantic function tools默认可并发；**可 `sequential=True` / whole-run sequential** | 对 bash 全局串行 | 必须显式关闭并发语义 |
| CLI 能力说明组合 | prompt + bash CLI 很自然 | 同样自然，不需要注册每个 CLI | 公共执行规则、role、capability docs分层 | 推荐继续 CLI 模式 |
| 网络/凭证/文件权限 | 由 Anchor sandbox 真正 enforce | Pydantic prompt/tool description不构成权限控制 | **继续由 bubblewrap/mount/env/network policy enforce** | 不迁移安全边界 |

### 3.3 执行、事件、交互与恢复

| 能力 | mini | PydanticAI + Harness | Anchor 仍需承担 | 风险/限制 |
|---|---|---|---|---|
| 纯文本不能直接判成功 | mini 的 magic completion/Anchor CLI 已契合 | Pydantic 默认文本会结束 run；但 **output validator 可 `ModelRetry`**，直到 bash wrapper 观察到 `anchor-done/route` | 一个很薄的 completion gate | output retry 预算需单独设置 |
| 增量事件 | mini 需自定义 trace/interactive | Pydantic **内置 run events**，可区分 model/tool/final；Harness也有 typed capability events | 转换为 Anchor-owned `NodeEvent` | Graph 不得接触 Pydantic 类型 |
| 模型取消 | 较薄 | **内置** cancellation token / run cancellation，带部分 history | provider取消依赖 provider；业务映射 | cancelled usage 可能不完整 |
| 工具取消 | Environment层 | async tool 可取消；sync tool thread 无法强杀 | bubblewrap process-group kill | OS side effect不能回滚 |
| 持久等待用户输入 | Anchor目前缺协议 | Pydantic **内置 deferred tools** 可把待外部处理调用作为 run output，并用 `DeferredToolResults` 继续 | Anchor定义 WAITING、resume ref、调度；若坚持单 bash，CLI side-channel 转 deferred | 仍不是“框架自动替 Graph 调度” |
| 步骤持久化 | Anchor自研 trace | **官方扩展 StepPersistence**：events、continuable snapshots、tool-effect ledger、lineage；File/SQLite/Mongo | 决定与现有 trace 合并还是替代一部分 | Harness 0.x；retention自理 |
| 崩溃后的副作用不确定性 | Anchor目前没有验证 exactly-once | **官方扩展**明确暴露 `unknown_after_crash` | Anchor根据 command/idempotency/Git/workspace决定 replay/reconcile | 这是可观测，不是自动修复 |
| 自动 crash resume | Anchor 自定义 | **官方明确未实现 automatic execution recovery** | Node runner决定从哪个 safe checkpoint继续 | 不能宣传 exactly-once |
| 恢复 Graph state | Anchor已有 Graph/runtime | StepPersistence **明确不负责** graph-state/workspace snapshot | Anchor继续负责 | 正好符合本次“Graph不动”的边界 |
| 预算跨恢复累计 | mini 当前需 Anchor自存 | Pydantic有 `UsageLimits` 和 run usage；StepPersistence 不恢复 retry counters等业务状态 | Anchor持久化 `BudgetSnapshot`，新 run 注入累计 usage | 不要只依赖 snapshot |
| 预算退出 vs 失败 | mini `LimitsExceeded` 可识别 | Pydantic UsageLimitExceeded等可映射 | NodeResult 显式 `BUDGET_EXHAUSTED` | 避免被 Graph 当一般失败重试 |

### 3.4 集成与长期维护

| 项目 | mini 2.4.6 | PydanticAI + Harness | Deep Agents |
|---|---|---|---|
| Node 代码复杂度 | 极低；当前已集成 | Core 比 mini 大，但能力通过官方 capability 组合；Anchor adapter 可保持薄 | 高；LangGraph + middleware + backend scaffolding较多 |
| Graph–Node 解耦 | 当前已做到 | 完全可做到，只要所有 Pydantic 类型留在 adapter 内 | 可做到，但内部又引入 LangGraph 状态模型 |
| 供应商兼容 | 主要 LiteLLM/OpenRouter/其他 backend | 官方多 provider + vLLM/Ollama/LiteLLM 等 | LangChain provider ecosystem |
| 测试替身 | mini 有 DeterministicModel | **TestModel / FunctionModel** 一等支持 | LangChain 有测试办法但本次未深入 |
| API 稳定性 | 2.x，核心极小 | PydanticAI 2.x stable；Harness **0.x Alpha**，升级风险主要集中在 Harness | Deep Agents 0.x，近期高频发布 |
| 外部服务硬依赖 | 无 | 本方案 **无**；File/SQLite StepStore 足够起步 | LangGraph checkpointing可本地，但整体依赖面更大 |
| 托管服务必须吗 | 否 | 否；Logfire等不是必需 | 否 |
| 与已有 workspace/Git 重叠 | 很小 | 若只用 capabilities 很小；若上 Coder/Shell则明显重叠 | **明显**：filesystem/sandbox/subagent/graph 机制与 Anchor 重叠 |
| 长任务 harness 自研量 | **高** | **中低** | 中，但系统重叠成本高 |

---

## 4. 最小验证场景：调查结果与待执行实验

### 4.1 当前环境状态

**没有把以下任何一项写成“运行通过”。** 当前容器 import 检查结果为四个包均未安装，因此本次结论的运行层证据需要在 Anchor 开发环境补齐。

交付包中的 `experiments/` 提供：

- `mini_baseline_introspection.py`：验证 stable package 的版本、单 bash/tool schema、overflow abort exception、Agent limits。
- `pydantic_controlflow_probe.py`：用 `FunctionModel` 确定性验证“纯文本不能完成 → bash 明确提交 → 文本才可结束”、bash 串行和无工具自动重试的控制流。
- `pydantic_context_probe.py`：检查 Harness capability 能否注册、`ToolOutputLimits` 是否确实额外暴露 `read_tool_result`，并做 deterministic sliding-window history probe。
- `CRASH_RECOVERY_RUNBOOK.md`：定义四个 crash window 的 fault injection 与验收口径。

### 4.2 场景逐项状态

| 场景 | 源码/文档能确认什么 | 当前实际运行 | 迁移 PoC 验收标准 |
|---|---|---|---|
| 单 bash 接入 | mini固定 bash；Pydantic 可注册单一自定义 tool 且设 sequential | **未执行** | Graph 零修改；读/写 workspace；`anchor-done` 明确完成；纯文本无法提交 |
| 大输出 | mini默认 >10k head/tail 有损；Harness ToolOutputLimits默认 >=10k spill + bounded fallback | **未执行** | 生成 >100k 输出，尾部 marker 能通过 handle/paging 找回；记录完整结果去向和 TTL |
| 长对话 | Harness有多种 compaction、pairing保证、pin | **未执行** | 低窗口强制触发；早期 pinned constraint 仍逐字存在；tool pairing provider-valid；记录压缩前后 token |
| 窗口超限 | mini LiteLLM canonical path overflow直接 abort；Harness主要 proactive | **未执行** | mock provider overflow一次；Pydantic adapter只做一次 aggressive compaction + retry，不死循环 |
| 崩溃恢复 | StepPersistence有 started/completed/failed、unknown_after_crash；不自动 dedupe | **未执行** | 四个 kill window 全部记录计数文件、ledger、snapshot；不允许“猜测没执行所以自动重放” |
| 用户交互 | Deferred tools能结束 run并由外部补结果 | **未执行** | Node进入 WAITING后进程可退出；重启后加入用户结果继续；Graph只看 Anchor resume token |
| 完成与预算 | output validator可 retry文本；UsageLimits区分 request/tool/token/cost | **未执行** | 文本“完成”≠完成；CLI commit=完成；预算耗尽映射独立状态；resume后累计预算不清零 |

### 4.3 真实模型实验必须单独做

Deterministic model 只能证明控制流；它不能证明：

- SummarizingCompaction 会不会漏掉早期设计约束；
- coding task 多次 compact 后是否发生“认知漂移”；
- prompt cache 被改写后的真实延迟/成本；
- 某个私有 OpenAI-compatible/vLLM endpoint 的 window metadata 是否准确；
- provider overflow 的异常分类是否与预期一致。

建议用 Anchor 的真实任务构造 20–30 个 benchmark，每个至少重复 3 次，记录：任务成功、关键约束违反次数、context token 轨迹、compaction 次数、summary 调用 token/费用、TTFT、总时延、工具重做次数、崩溃恢复是否产生重复副作用。不要以“一次长任务跑通”替代稳定性评估。

---

## 5. 集成草图

### 5.1 最小 Graph–Node API：类型归 Anchor，不归框架

```python
@dataclass(frozen=True)
class NodeRunRequest:
    graph_run_id: str
    node_run_id: str
    node_id: str
    task: str
    workspace: Path
    upstream_mounts: Mapping[str, Path]
    role_instructions: str
    capability_notes: tuple[str, ...]
    budget: BudgetSpec
    resume: ResumeRef | None = None


class NodeEventKind(Enum):
    MODEL_DELTA = "model_delta"
    TOOL_STARTED = "tool_started"
    TOOL_FINISHED = "tool_finished"
    CONTEXT_COMPACTED = "context_compacted"
    CONTEXT_WARNING = "context_warning"
    CHECKPOINTED = "checkpointed"
    WAITING = "waiting"
    WARNING = "warning"
    FAILED = "failed"
    COMPLETED = "completed"


@dataclass(frozen=True)
class NodeResult:
    status: Literal[
        "completed", "waiting", "budget_exhausted", "failed", "uncertain"
    ]
    route_intent: str | None
    resume_ref: ResumeRef | None
    usage: UsageSnapshot
    trace_ref: str
    uncertainty: ToolEffectUncertainty | None = None


class AgentNodeRunner(Protocol):
    async def run(self, request: NodeRunRequest) -> AsyncIterator[NodeEvent]: ...
```

**硬规则：** Graph 不能拿到 `ModelMessage`、`DeferredToolRequests`、`StepEvent`、LangGraph state 等框架类型。adapter 内部翻译成 Anchor 类型。

### 5.2 Pydantic Adapter 的建议组成

```text
AgentNodeRunner(Pydantic)
  ├─ PromptAssembler
  │    ├─ common execution rules
  │    ├─ role instructions
  │    ├─ node instructions
  │    └─ optional capability notes
  │
  ├─ custom bash tool
  │    └─ existing Anchor SandboxEnvironment / bubblewrap
  │
  ├─ completion gate
  │    └─ observe anchor-done / anchor-route protocol
  │
  ├─ capabilities
  │    ├─ TieredCompaction
  │    ├─ ClampOversizedMessages
  │    ├─ ReportContextUsage
  │    ├─ StepPersistence
  │    └─ [optional] ToolOutputLimits
  │
  ├─ UsageLimits
  ├─ cancellation/event bridge
  └─ one-shot provider-overflow guard (Anchor thin layer)
```

### 5.3 bash wrapper

建议语义，而非最终 API 代码：

```python
@agent.tool(retries=0, sequential=True)
async def bash(ctx: RunContext[NodeDeps], command: str) -> BashObservation:
    # 真正权限、网络、文件和超时仍由 Anchor sandbox enforce
    obs = await deps.sandbox.execute(command)

    # parse_done_route 只解析现有显式协议，不允许普通文本提交
    commit = parse_done_route(obs)
    if commit is not None:
        deps.completion = commit

    # tool 已执行过以后，不通过 ModelRetry 触发“框架自动重做”
    return render_bounded_observation(obs)
```

对于 PydanticAI，工具默认存在并发能力，因此 **bash 要明确 `sequential=True`**，或整个 run 使用 sequential tool execution mode。Anchor 的 shell 命令有副作用，不应该让同一模型响应中的多个 bash 并发后再试图猜顺序。

### 5.4 显式完成 gate

PydanticAI 默认收到普通文本就可以结束 run，这和 Anchor 当前语义不同。建议保留 CLI 协议，用 output validator 纠正：

```python
@agent.output_validator
async def require_anchor_commit(ctx, output):
    if ctx.deps.completion is None:
        raise ModelRetry(
            "Node is not committed. Use anchor-done / anchor-route via bash."
        )
    return output
```

这层是必要适配，但很薄；它比新增一个“finish function tool”更符合当前“单 bash + CLI”体系。

### 5.5 上下文策略建议

不要一上来只用 LLM summary。建议 tiered：

1. `ToolOutputLimits` 或 Anchor spooler：**输出生成时**就避免大块 tool result 长期驻留。
2. `DeduplicateFileReads` / `ClearToolResults`：先清理可重读、重复内容。
3. `ClampOversizedMessages`：处理单个异常超大 response/args。
4. `SummarizingCompaction`：仅在便宜策略不够时总结更老历史。
5. 为“任务目标、验收条件、不可违反约束、当前计划/已确定决策、关键 artifact refs”建立短小 pinned state。
6. `ReportContextUsage` 把 context 百分比上报 Anchor UI/trace。

对自托管模型，**显式配置 `context_window`**，不要依赖 `genai-prices` 根据名字猜企业部署的真实窗口。

### 5.6 完整 trace 与模型 context 分离

建议继续维护两层：

```text
Execution Record (完整、审计、不可因 compaction 消失)
  - raw model responses / usage
  - raw tool commands + bounded/full output refs
  - tool lifecycle + exit status
  - compaction receipts/events
  - user wait/resume
  - budget state
  - Git/workspace refs

Model Context (有限、可改写)
  - system/role/task
  - pinned durable state
  - compacted history / summary
  - recent turns
  - bounded tool results / handles
```

`StepPersistence` 可以承担前者中的“agent step history / snapshots / effect ledger”的一部分，但不应该取代 Anchor 自己的 graph-run、Git commit、workspace 身份。

### 5.7 恢复原则

对 crash 后的 tool effect，推荐 NodeResult 增加 `uncertain` 状态，而不是“恢复 = 再执行”：

```text
started, no terminal record
        │
        ▼
unknown_after_crash
        │
  ┌─────┼─────────────────────┐
  │     │                     │
read-only/idempotent       side-effectful
  │                           │
replay allowed        reconcile first / user policy
```

Git workspace 是 Anchor 的优势：对“写 workspace”的 shell，可以通过 Git/index/file state 做局部对账；对外部系统（网络 API、issue、数据库）则必须设计 idempotency key 或人工/业务 reconciliation。Harness 无法替代这一层。

---

## 6. 迁移与维护成本

### 6.1 推荐迁移后可以删除/缩小什么

**有机会删除或显著缩小：**

- 自研 message-history compaction 算法；
- 自研 token/window 估算主体；
- 自研 tool-call/return pairing-aware history trim；
- 自研 context usage telemetry；
- 大部分 agent-level cancellation/history repair；
- 一部分 step snapshot / tool-effect lifecycle 记录代码；
- 如果接受第二工具：大输出 spill/paging 实现。

**必须保留：**

- Anchor Graph 调度、routing、loop；
- workspace + Git commit 边界；
- bubblewrap sandbox 与进程管理；
- `anchor-scholarly` 等 CLI；
- `anchor-done` / `anchor-route` 显式完成语义；
- Graph-level resume identity；
- crash 后外部副作用 reconcile；
- op 节点完全不经过模型框架。

**需要新增的薄层：**

- Pydantic ↔ Anchor `NodeEvent/NodeResult` 转换；
- completion gate；
- provider context-overflow one-shot recovery；
- persisted cumulative budget metadata；
- WAITING/resume protocol；
- 若坚持单 bash：输出 spool CLI。

### 6.2 粗粒度工程量（用于比较，不是承诺）

假设现有 sandbox/Graph/CLI 均可复用：

| 工作项 | Pydantic 迁移估计 | 继续 mini 后补齐同等级能力 |
|---|---:|---:|
| Node adapter + bash + completion | 150–300 LOC | 现有基础上小改 |
| event/cancel bridge | 100–200 LOC | 200–400+ LOC，自行设计生命周期 |
| context policy 配置/封装 | 100–200 LOC | 500–1000+ LOC + 算法维护/测试 |
| overflow guard + budget resume | 100–200 LOC | 150–300 LOC |
| persistence/recovery glue | 100–200 LOC | 400–800+ LOC 才能达到 ledger/snapshot 可观测性 |
| fault/long-run tests | 300–600 LOC | 同等级测试仍必须写 |
| 单 bash spooler（可选） | +100–250 LOC | +100–250 LOC |

上述只是“适配代码量级”而非完整产品工期。综合判断：**Pydantic 路径大约 1–2 个工程周可完成一轮严谨 PoC + fault test；继续 mini 要做的不是一次性代码更多而已，更大的成本是 Anchor 长期拥有 compaction/recovery 的正确性责任。**

### 6.3 额外依赖与运维

推荐初期组合：

```text
pydantic-ai==2.46.0
pydantic-ai-harness==0.32.0
StepStore = FileStepStore 或 SqliteStepStore
```

**不建议初期引入 Temporal / DBOS / Prefect / Restate。** PydanticAI 的 durable execution integrations 是真实存在的，但 Anchor 已有 Graph、workspace、Git 和节点调度。现在引入 workflow engine 会把“Node harness 迁移”扩大成“runtime 平台迁移”，与本任务的最小改动目标相反。

何时再考虑外部 durable workflow engine：

- Node 要跨小时/天等待外部事件；
- 多 worker 分布式抢占/租约成为硬需求；
- 需要跨服务 durable timer / signal；
- Anchor 自己的 scheduler 已经成为主要维护负担。

### 6.4 PydanticAI Harness 版本风险

这是推荐方案最大的非功能性风险：

- PydanticAI core 是 2.x stable；
- `pydantic-ai-harness` 是 **0.x / Alpha**；官方明确表示 minor release 可能改 API，但承诺 deprecation warning / migration note；
- 0.25 → 0.32 在不到一个月内连续发布，能力演进非常快。

因此生产策略应当是：**严格 pin 版本 + adapter 隔离 + contract tests**。不要在 Anchor 各处直接 import Harness 类型；只允许 `anchor/.../pydantic_adapter/` 依赖它。这样未来 0.33/0.40 的变化只改一个边界。

### 6.5 Deep Agents 为什么没有胜出

Deep Agents 是本次唯一值得进入详细筛选的额外候选，因为它确实解决了长期 agent 的一批实际问题：context summarization/offload、filesystem、subagent、persistence/HITL，且 0.7.14 刚加入 bounded compaction recovery/input budget validation。

但对 Anchor 来说，它“太像一个已经带 Graph/runtime 的完整 agent harness”：

- 底层构建在 LangGraph 上；
- Python profiles 可以排除部分 tools/middleware，但官方明确 `FilesystemMiddleware`、某些内部 permission scaffolding 等属于 required scaffolding（SubAgent middleware是否附加取决于是否启用 subagent）；
- durable wait/resume 又落到 LangGraph checkpointer/node semantics；
- 这与 Anchor 自己已有 Graph、workspace/Git、sandbox 的边界重叠。

如果 Anchor 未来决定“连 Graph runtime 也一起换”，Deep Agents 应重新评估；但在当前任务“只换 Agent Node 内执行底座”的边界下，它不是最小维护总量的方案。

---

## 7. 建议与待决问题

### 7.1 近期最小可交付方案

建议按 4 个短阶段推进，任何阶段失败都可以停在 mini，不需要大爆炸迁移。

**阶段 A：控制流替换 PoC**

- 新建 `PydanticAgentNodeRunner`，与 mini runner 并存，通过配置切换。
- 只注册现有 bash；`sequential=True`，tool retry=0。
- 复用 SandboxEnvironment。
- 加 completion output validator。
- 用 FunctionModel 跑现有真实执行测试，确认 Graph 无需知道框架。

**阶段 B：先解决最痛的 context**

- 启用 `TieredCompaction` + `ClampOversizedMessages` + `ReportContextUsage`。
- 对每个公司部署 endpoint 明确 `context_window`。
- 定义 pinned durable state schema。
- 做 20–30 个真实 long-horizon tasks 的 mini vs Pydantic A/B。

**阶段 C：恢复与 fault injection**

- 接入 `StepPersistence(File/SQLite, capture_frontier=True)`。
- 跑任务书四个 crash window。
- Anchor resume policy 必须识别 `unknown_after_crash`，绝不盲目 replay。
- 确认哪些现有 trace 可删，哪些仍是审计记录必须保留。

**阶段 D：大输出与用户等待**

- 决定是否接受 `read_tool_result` 作为第二工具。
- 若不接受，实现一个极薄 `anchor-output` spool/read CLI。
- 定义 Anchor `WAITING` / `ResumeRef`，再把 Pydantic deferred-tool 语义映射进去。

完成 B+C 后再作正式迁移 ADR；在此之前保持 mini 可回滚。

### 7.2 建议的默认配置方向

不是最终数值，但建议起点：

```text
context trigger:        0.75–0.85 of verified real model window
hard per-request guard: lower than provider rejection threshold
compaction:             cheap deterministic passes -> summarization
pinned state:           only task contract / decisions / artifact refs / current plan
bash concurrency:       sequential
bash retry:             0 framework retries after execution starts
provider overflow:      one forced compact + one retry maximum
StepPersistence:        capture_frontier=True during evaluation
store:                  local SQLite/File first; no distributed workflow service
```

阈值必须通过真实 workload 调，不应把上述比例写死成架构标准。

### 7.3 最值得项目方确认的 6 个问题

1. **“只允许一个 bash 工具”是偏好还是绝对约束？** 这直接决定能否零自研使用 `ToolOutputLimits(Spill)`。
2. **当前 trace 有哪些外部消费者？** 如果 UI/恢复/API 已绑定其 schema，StepPersistence 只能先并存，不能直接替换。
3. **节点 bash 的副作用是否基本都限制在 workspace？** 如果大量会操作外部系统，recovery policy 必须比当前更保守。
4. **公司模型 endpoint 的真实 context window 是否有统一 registry？** 如果有，应由 Anchor 明确传给 harness，而不是依赖模型名推断。
5. **恢复后的 budget 是按“逻辑 Node run”累计还是每次 process invocation 重置？** 建议前者，但需产品语义确认。
6. **未来“等待用户输入”只是少量人工确认，还是会变成长时间事件驱动工作流？** 前者用 Anchor WAITING + deferred tool 足够；后者才值得评估 Temporal/DBOS 类 durable runtime。

### 7.4 最终判断

**PydanticAI + 第一方 Harness 是本次调研里唯一一个在不侵入 Anchor Graph 的前提下，已经能实质接管“上下文治理 + agent 生命周期记录”大量脏活的方案。**

它并没有神奇地解决所有长期 agent 问题：摘要仍需 eval；provider overflow 仍需最后一道防线；副作用 exactly-once 根本不能靠 agent framework 自动保证；Graph-level waiting/resume 仍属于 Anchor。但是这些剩余工作，已经从“自己实现一套 harness”收缩为“在清晰的系统边界做少数 adapter/reconciliation”。

因此本报告建议：

> **不再继续扩建 mini-swe-agent 上的自研通用 context harness；保留 mini 作为回滚基线，立刻做 PydanticAI 2.46 + pydantic-ai-harness 0.32 的并行 Node runner PoC。通过长上下文 A/B 和 crash fault-injection 后，再决定正式迁移。**

---

## 8. 官方证据索引

### mini-swe-agent

- Releases / v2.4.6: https://github.com/SWE-agent/mini-swe-agent/releases
- Core agent loop (`agents/default.py`): https://github.com/SWE-agent/mini-swe-agent/blob/main/src/minisweagent/agents/default.py
- LiteLLM model / `ContextWindowExceededError` / `BASH_TOOL`: https://github.com/SWE-agent/mini-swe-agent/blob/main/src/minisweagent/models/litellm_model.py
- Default mini config / output truncation: https://github.com/SWE-agent/mini-swe-agent/blob/main/src/minisweagent/config/mini.yaml
- Control flow / explicit submit: https://github.com/SWE-agent/mini-swe-agent/blob/main/docs/advanced/control_flow.md
- License: https://github.com/SWE-agent/mini-swe-agent/blob/main/LICENSE.md

### PydanticAI core

- Releases / 2.46.0: https://github.com/pydantic/pydantic-ai/releases
- Agent / cancellation: https://pydantic.dev/docs/ai/core-concepts/agent/
- Output / output validators: https://pydantic.dev/docs/ai/core-concepts/output/
- Retries: https://pydantic.dev/docs/ai/core-concepts/retries/
- Advanced tools / sequential execution: https://pydantic.dev/docs/ai/tools-toolsets/tools-advanced/
- Deferred tools: https://pydantic.dev/docs/ai/tools-toolsets/deferred-tools/
- UsageLimits API: https://pydantic.dev/docs/ai/api/pydantic-ai/usage/
- Testing / TestModel / FunctionModel: https://pydantic.dev/docs/ai/guides/testing/
- Providers: https://pydantic.dev/docs/ai/models/overview/

### pydantic-ai-harness

- Releases / 0.32.0: https://github.com/pydantic/pydantic-ai-harness/releases
- Repository: https://github.com/pydantic/pydantic-ai-harness
- Compaction: https://pydantic.dev/docs/ai/harness/compaction/
- Tool Output Limits: https://pydantic.dev/docs/ai/harness/tool-output-limits/
- Step Persistence: https://pydantic.dev/docs/ai/harness/step-persistence/
- License: https://github.com/pydantic/pydantic-ai-harness/blob/main/LICENSE

### Deep Agents

- Releases / 0.7.15: https://github.com/langchain-ai/deepagents/releases
- Changelog: https://github.com/langchain-ai/deepagents/blob/main/libs/deepagents/CHANGELOG.md
- Python Profiles: https://docs.langchain.com/oss/python/deepagents/profiles

---

## 9. 证据到结论的关键映射

为了便于后续主开发 Agent 审核，这里列出最可能被误读的几条：

1. **“Pydantic 有压缩”不是指自定义 hook。** Harness `Compaction` 文档明确列出 shipped strategies，并说明所有策略保持 tool-call/tool-return pairing；`pin()` 是 shipped strategy 都必须保留的 durable content。
2. **`max_fraction` 不等于绝对可靠的窗口识别。** 官方文档直接举例 registry 记录错误会造成 provider 在压缩前拒绝，因此私有/受限部署应传 `context_window` override。
3. **Spill 是 lossless，但不是单 bash。** `ToolOutputLimits` 明确注册 `read_tool_result`，默认阈值 10,000 字符。
4. **StepPersistence 不是 exactly-once。** 官方明写“not a full graph-state checkpoint”、“Automatic execution recovery is not implemented”，并定义 `unknown_after_crash`；还指出没有 Harness event 能把外部副作用与本地 SQLite write 做成原子事务。
5. **Pydantic 默认文本确实会结束 run。** 所以 Anchor 必须做 completion validator；这是适配要求，而不是可选美化。
6. **取消不是进程强杀。** Pydantic 对 async tool 可协作取消，但 sync worker 线程不能安全终止；Anchor 的 bubblewrap/process group 仍是工具执行层的硬边界。
7. **mini 2.4.6 对 canonical LiteLLM context overflow 是 abort，不是恢复。** 源码把 `ContextWindowExceededError` 放进 `abort_exceptions`，避免无意义 retry，但没有压缩并继续。

