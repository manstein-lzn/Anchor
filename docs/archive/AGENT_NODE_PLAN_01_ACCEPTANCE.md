# 第一包主验收记录

> **历史归档（2026-09-25）**：保留当时的讨论、计划与验证证据，不代表当前实现或待办。
> 文中的“下一步”“待实施”“必须”及旧阅读顺序仅适用于当时阶段，不作为后续开发指令。
> 当前入口见 [项目 README](../../README.md)，唯一升级方向见 [Plugin 设计](../plugins.md)。

最新验收版本：`b9aae105d2ab0beb24afa93272a9d0186e4aa70f`。结论：**第一包通过，G1 冻结为后续验证基线**。这不是正式迁移或恢复能力验收。

## 最新复验结果

- 全量后端 145 项测试通过，无失败、无 skip；ruff 与 mypy（23 个源文件）通过。
- R1 已解决：读取未执行的 ModelRequestNode.request，保存完整末批标准工具结果与调用 ID；25,000 字符末尾标记保留，skipped 可关联，无额外模型请求或副作用。
- R2 已解决：真实执行器挂载 ClearToolResults，实际清理普通工具输出；执行钩子观察到成功、非零退出、提交和 skipped；run-level 钩子的取消行为已明确验证。
- 后续使用 `run_node(request, *, model, capabilities=())` 组合接缝。共享文件归属与已知限制见总计划 G1。

**已知限制转交第三包**：提前结束时 Anchor 返回 completed，框架 wrap_run 的 handler 抛 CancelledError。第一包只证明此边界可观察，不证明 StepPersistence 已记录完成。第三包必须验证 terminal record/最后快照如何可靠落地，并区分主动取消与正常提交；不能假造 AgentRunResult 或直接把所有 cancelled 改成 completed。需要共享适配器改动时由主集成收敛。

**转交第二包**：压缩后的框架历史不等于完整原始记录；第二包负责分离二者，并解决沙箱截断之前的大输出保留。本次仅验收原型无 capability 默认路径的末批完整性与 capability 兼容接缝。

以下保留上一轮发现作为修订证据，状态均已关闭。

## 历史验收版本

`014b528`：当时 G1 未通过，阻塞项如下。

## 已独立复验

- 全量后端 141 项测试通过，无 skip。
- `ruff check src/ tests/` 通过。
- `mypy src/anchor/` 通过，覆盖 23 个源文件。
- 新实现采用普通 bash 函数工具、提交状态守卫、公开迭代边界退出。此前请求计数、失败消息、沙箱异常与真实分支问题已有修复。

## R1：最后一批记录不完整（阻塞）

独立实验：同一模型响应依次调用 bash 输出 25,000 个字符及末尾标记，再调用真实 anchor-done。Node 返回 completed，模型请求数为 1；trace 的框架部分仅有 user-prompt、tool-call、tool-call，没有 tool-return。exit.commands 中的备用 output 被截为 20,000 字符，末尾标记不存在。

这不是第二包要解决的超大输出存储问题：这里输出小于现有沙箱限额，却被第一包新增的截断再次丢弃。备用命令记录也没有 tool_call_id，不能直接用于可靠关联或后续框架历史恢复。

修订要求：在不增加模型请求、不重新执行工具的前提下，记录最后一批已形成的标准工具结果（包括 skipped），或提供等价的完整、可关联记录。优先核查公开 ModelRequestNode.request 等已形成结果的读取方式；不要执行该模型请求节点来补记录。移除新增静默截断，或保留完整输出引用及明确截断元数据。无须提前实现第二包的大输出存储系统。

新增回归：同批普通命令 + done + skipped，输出超过 20,000 字符；检查末尾内容、调用 ID 对应、skipped 状态、无后续副作用与无额外模型请求。

## R2：真实 Harness 生命周期兼容性仍未验证（阻塞）

现有兼容性测试构造 ToolReturnPart/RetryPromptPart 后调用内部 iter_tool_pairs，再单独检查一轮运行的消息类型。这证明了消息形态改正，但没有在真实执行器上启用 ClearToolResults 或 StepPersistence，也没有验证提前离开 agent.iter 时最后一批的记录与生命周期行为。

修订要求：补一个最小可复现实验，在实际 run_node 的组合接缝挂载真实 Harness capability。至少观察普通命令成功、非零命令结果、提交与 skipped 的执行钩子/记录，并确认最后一批何时对框架可见。实际运行一次工具结果清理，证明它处理的是该执行器产生的历史。若使用 StepPersistence 做观察，只需临时本地 store，不要求本包实现恢复或 SIGKILL。

明确记录提前结束是否触发 run-level 完成钩子、是否产生可供后续包使用的末批结果。观察到框架生命周期与 Node completed 不一致时，应说明最小适配边界，不能仅以 Node 状态代替框架证据。

## 报告整理与后续

将 RESULT 文档开头改为最终实现摘要，把 ToolOutput/ModelRetry、旧测试计数和已否定判断收进历史部分，避免下一包按旧入口理解。上下文属于第二包，恢复属于第三包。

上述两项修订验收后，再在总计划中填写实际基线 SHA、capability 接缝、共享文件所有权和结果记录契约。当前没有宣布正式迁移，也未修改执行代码。
