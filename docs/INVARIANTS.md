# 不变量与其验收测试

`docs/AGENT_ARCHITECTURE_RESEARCH_BRIEF.md` 定义 I1–I9。**只写在文档里的不变量不是不变量**，
所以每一个都绑定至少一个可运行的测试，并由 `tests/test_invariants.py` 强制——
该测试会断言这张表里的每个测试名**真实存在**。

改测试名会让本文件失效，而那个测试会立刻告诉你。

| # | 不变量 | 绑定的验收测试 |
| --- | --- | --- |
| **I1** | 图不可变：发布后不可修改，run pin 到具体版本 | `test_admission.py::test_new_graph_version_does_not_change_existing_run` |
| **I2** | Canonical State 是恢复源：事件日志是唯一真相，上下文/记忆是投影 | `test_recovery_process.py::test_interrupted_worker_lease_requires_explicit_recovery` |
| **I3** | 副作用有账本：operation id、幂等、授权、审计 | `test_tool_gateway.py::test_transport_failure_is_outcome_unknown_not_failed` |
| **I4** | 完成需验证：由确定性或领域验证器判定，不是模型的声明 | `test_verifier.py::test_deterministic_verifier_passes_and_opens_completion_gate` |
| **I5** | 长等待不占 worker：等待从持久状态恢复 | `test_approval.py::test_event_wait_parks_and_resumes_on_matching_type` |
| **I6** | 不因猜测的数值预算中断健康 agent | `test_execution_reliability.py::test_total_budget_expires_even_without_an_active_worker` |
| **I7** | 供应商中立：不依赖特定模型厂商 / 引擎 / 向量库 | `test_architecture.py::test_dependencies_only_point_downward` |
| **I8** | 声明式输入：节点只看被声明的输入 | `test_worker_service_context.py::test_resolver_without_artifacts_keeps_reference` |
| **I9** | 每个 run 可复现：给定图版本 + 输入 + 运行期版本可重放同一决策路径 | `test_model_recording.py::test_a_whole_run_replays_by_call_order` |

## 几处需要说明的绑定

**I6 的测试选的是「预算耗尽」而不是「看门狗」**，因为这一条说的是**不得因猜测的数值中断**。
该测试证明恰恰相反的情形：预算是**显式设置**的（`run_timeout_seconds`），所以耗尽它有明确后果；
而 `test_execution_reliability.py` 里其余测试覆盖「无预算时健康任务不被终止」。
两半都需要，一条里放不下。

**I7 绑的是分层测试**，因为"供应商中立"的机制就是分层：`domain` 不许 import `runtime`，
`runtime` 不许 import `api`。一个特定厂商的客户端只能出现在 `runtime` 的适配器里，
所以分层一旦被违反，中立性就没了——而分层是可机械判定的，中立性本身不是。

**I8 绑的是 resolver 测试**，它证明未声明的输入**不会被当成内容**：没有工件可读时，
引用保持为引用，而不是被来自别处的文本顶替。

**I9 绑的是整轮回放的顺序测试**，它证明"给定模型答案，引擎路径是确定的"。
这正是 P1.1 建立的性质：没有它，"行为变了"就无法归因到 prompt 或策略。

## 不在这张表里的

`I2` 还有一半是"上下文/记忆是带溯源与保留规则的投影"。溯源由 `test_integrity.py` 与
`test_memory.py` 覆盖，保留规则由 `test_retention.py` 覆盖——但**"投影永不参与恢复"这条
在两者里都没有断言**，它由 `test_architecture.py::test_replay_and_memory_never_become_canonical_recovery_state`
从模块图层面强制（`state` 层不得 import 投影模块）。三条合起来才是完整的 I2。
