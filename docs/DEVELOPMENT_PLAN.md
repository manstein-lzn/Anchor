# Anchor Development Plan

**日期**：2026-09-12  
**状态**：当前执行计划  
**执行方式**：一次只推进一个可验收切片；完成后由架构验收，不以“代码已写”作为完成标准。

## 1. 当前目标

Anchor 当前的目标不是实现 RSI，也不是一次性完成认知、记忆、归因和自动优化。

当前目标是交付一个能够稳定运行的 Agent Graph Runtime：

```text
定义图 -> 发布不可变版本 -> 创建 Run -> 执行节点
-> 持久化事件/产物/工作区 -> 验证 -> 观察/暂停/恢复/人工介入
```

必须保持的边界：

- 图版本发布后不可变，Run 必须 pin 到具体版本。
- 事件历史和不可变内容引用组成恢复闭包；缓存、memory、summary、model recording 都不是恢复真相。
- 每个副作用都有 operation id、授权、幂等语义和审计记录。
- 完成必须经过 verifier 或确定性检查。
- 节点只能看到声明式输入；不得引入隐式全局共享状态。
- 健康任务不能被猜测的轮数、token 数或 wall-clock 上限静默终止。
- R0/R1 replay 可以成为硬保证；重新调用非确定性模型的 R2/R3 只能记录和比较。

## 2. 分层路线

### 当前产品层：必须可靠

```text
执行、恢复、工作区、验证、审批、观测、回放、成本记录
```

### 连续性层：可选增强

```text
Contract、Checkpoint、投影、受控 recall、takeover evaluation
```

认知层不能成为普通 Run 的单点故障，也不能建立第二个 canonical recovery state。

### RSI 层：未来愿景

```text
Run traces -> metrics/outcomes -> candidate lesson/strategy
-> offline harness -> approval -> new graph/capability version
```

RSI 初期只能提出候选变更，不能自动修改生产图、质量门、Contract 或运行时策略。

## 3. 执行顺序

### P0：运行时硬化

#### P0.1 持久服务与主机重启恢复

目标：主机或服务重启后，运行不会因为 transient service 消失而长期停滞。

范围：

- persistent user systemd units；
- 服务启动前的数据库/配置检查；
- worker、control、verifier、scheduler、supervisor 的启动顺序；
- 已有 lease/recovery 语义的真实重启验证；
- 启动失败必须可诊断，不能假装服务已就绪。

不做：新调度器、新的 lease 类型、自动偷取 lease。

验收：

1. 启动一条包含 Agent、Verifier、等待或工作区的真实 Run。
2. 在节点执行期间停止相关服务或重启 user service。
3. supervisor 发现 stale/recoverable 状态，显式恢复后 Run 完成或失败闭合。
4. 重启不产生重复副作用、不丢失事件、不伪造完成。
5. `systemctl --user` 状态、日志、readiness 和测试输出均可作为证据。

#### P0.2 并行分支失败传播

目标：一个分支失败后，相关 in-flight sibling、pending downstream 和 join 状态有明确、可恢复、可审计的结局。

范围：

- failure fan-out 语义；
- in-flight sibling 的取消或标记策略；
- dispatch supervision；
- join/merge 在失败、取消、未知结果下的行为；
- 事件幂等和重启恢复。

不做：通用并行状态写入、CRDT、后台自动修复。

验收：

- sibling 尚未执行、正在执行、已完成、结果未知四种时序；
- 重复 failure event 和失序 delivery；
- supervisor 重启后状态一致；
- 不允许失败 Run 被 sibling 重新打开 downstream。

#### P0.3 生产边界说明与失败关闭

目标：把当前开发版的限制变成明确行为，而不是隐含假设。

范围：

- SQLite 单进程限制和 PostgreSQL 路径分别验证；
- production identity/authorization 的当前边界；
- approval UI/API 的未完成能力；
- artifact service 的替代接口或明确阻断；
- 所有关键 unsupported path 返回结构化错误。

验收：每个未支持路径都有测试、错误码和文档，不允许 silent fallback。

### P1：回放、观测与成本

#### P1.1 完整 model recording/replay

目标：录制一次真实 Run，并在没有 provider 的情况下重建 R0/R1 路径。

必须完成：

- whole-campaign recording/replay；
- recording retention 与 artifact GC；
- replay 不参与 canonical recovery；
- prompt、instructions、tool-loop sequence 不一致时定位失败；
- replay 不 fall through 到 live model；
- provider-free CI fixture。

验收：

```text
recorded run -> replay-only process -> same event/state path
```

R2/R3 不得被标成 deterministic replay。

#### P1.2 稳定前缀与工作集观测

目标：先测清 prompt 成本和缓存，而不是先实现智能 memory。

每次模型调用至少记录：

- graph/runtime/model/policy version；
- prompt prefix hash；
- declared input hash；
- working-set hash；
- active-window hash；
- input/output/cached/billed tokens；
- projection version。

验收：Run usage API 和测试报告能区分：

- 稳定前缀；
- 本轮新增内容；
- 工具循环重发内容；
- cache hit/miss；
- gross 与 billed input。

#### P1.3 跨 Run content cache

目标：对不可变、可验证的研究内容避免重复抓取和重复物化。

约束：

- cache 是 projection，不是 canonical state；
- content ref 必须带 hash、来源和版本；
- cache miss、损坏、过期必须显式失败或重新获取；
- 不得让 cache 绕过 I8 的 declared input；
- 不做自动经验注入。

验收：冷缓存、热缓存、损坏缓存、不同 graph version、不同任务 scope 均有测试。

### P2：质量基础设施与产品完成度

#### P2.1 provider-free 端到端 CI

将录制响应接入：

- workspace validation；
- workspace lineage；
- parallel merge；
- MCP author/run/observe/gate；
- Agent -> Verifier -> completion gate。

验收：CI 不依赖真实 provider，也不修改 canonical fixtures；差异能定位到节点/调用序号。

#### P2.2 operator policy 与 approval surface

- revision ceiling 作为显式 operator policy，而不是隐藏执行预算；
- approval UI/API 行为一致；
- human wait、pause/resume、unknown outcome 的 Run Console 状态清楚；
- policy 改变不会影响已 pin 的 Run。

#### P2.3 文档与协议稳定化

将文档中的 `Completed`、`Target`、`Not yet implemented` 对齐；为公开 protocol 增加版本策略；每个不变量绑定至少一个 runnable acceptance test。

### P3：可选认知连续性实验

P3 不是当前 Runtime 的阻塞项。

只在 P0–P2 稳定后推进：

1. 冻结 Contract、输入快照、模型版本和工具能力。
2. 比较原始材料、普通摘要、认知状态、认知状态加受控 selector。
3. 使用外部行为标准：约束遵守、失败路径避免、用户修正、下一动作、必要召回。
4. 记录 retrieval 是否先选择再读取，以及读取结果是否累积进工具循环。
5. 如果没有独立行为收益，停止扩展认知 schema。

认知层的最小验收不是“能回答七个问题”，而是“在相同任务条件下减少错误决策或重复失败”。

### P4：RSI 实验层

只做离线候选生成和比较：

- State / Decision / Action / Observation / Outcome 数据导出；
- candidate attribution；
- candidate prompt/strategy version；
- held-out evaluation；
- 人工或显式阈值批准；
- 退役、回滚和版本谱系。

禁止：自动修改已发布 Graph、自动降低 verifier bar、自动将跨 Run 经验注入所有节点、并行 State writers。

## 4. 执行 Agent 的工作规则

每个开发切片必须先提交短计划，包含：

1. 要改变的协议/模块；
2. 不改变的边界；
3. 持久化和恢复语义；
4. 测试与故障注入；
5. 文档更新点；
6. 完成证据和已知限制。

执行过程中：

- 先读相关 `AGENTS.md` 和现有测试；
- 使用现有 domain/store/gateway/ledger 边界；
- 不新增抽象来掩盖未决定的语义；
- 不修改用户未要求的文件；
- 不用 `git reset --hard` 或覆盖已有用户变更；
- 不以“测试通过”替代恢复/审计验收；
- 发生范围变化时停止并记录，而不是顺手扩张。

## 5. 交付格式

每个切片完成时，执行 Agent 必须返回：

```text
Changed:
  files and protocol changes

Verified:
  commands, test counts, E2E evidence

Recovery:
  crash/retry/restart behavior

Not verified:
  explicit gaps and environment assumptions

Follow-up:
  only the next smallest required slice
```

## 6. 架构验收标准

我会按以下顺序验收，而不是只看 diff：

1. 目标是否属于当前阶段；
2. 是否保持 I1–I9；
3. 是否改变 canonical recovery semantics；
4. 是否有正常路径、失败路径、重启路径和重复事件测试；
5. 是否有可复现的命令和证据；
6. 是否引入了没有必要的新状态、服务或权限；
7. 文档中的当前状态是否和实现一致。

只有这些都通过，切片才算完成。

## 7. 立即执行顺序

交给执行 Agent 的第一批任务应严格按以下顺序：

```text
1. P0.1 persistent service + reboot recovery
2. P0.2 failure fan-out/cancellation
3. P1.1 whole-campaign model replay + retention
4. P1.2 stable-prefix/working-set telemetry
5. P2.1 provider-free E2E CI
```

A2A、Memanto、向量 memory、自动 cognition projection、归因引擎和 RSI 暂不进入当前开发批次。
