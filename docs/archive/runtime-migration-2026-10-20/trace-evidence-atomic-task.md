# 小任务：Goose trace 证据改为原子写入（master）

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

已完成：共享 durable writer 的六处调用均使用同目录原子替换，已有权限位和 JSON 字节保持不变；新增确定性交错及持续并发回归，旧实现在第一次交错发布即失败。workspace 1,999/0/123、Goose 完整 ignored 120/0/0、标准入口 105/0/0 及 105 份有效证据通过，严格 Clippy/格式通过。调用点逐项判断见 [Runtime 回归契约](../../architecture.md#宿主持久写入与清理)，原始输出见[本机证据](../../../.local/trace-evidence-atomic-20261019/README.md)。未新增真实 Provider 或部署验收；下文保留原任务要求，不作为当前待办。

编制：2026-10-19。来源：`constitution-core` 分支合并验证时，发现了一个 master 上同样存在的读写竞态。

## 现象

分支的 Goose 回归中，`/runs/{id}` 读取 trace 时偶发返回 500，错误为 `Goose trace resource is malformed`。事后保留的现场 JSON 能正常解析，说明读取方读到的是一个正在写入、尚未写完的文件。

## master 上的位置

- 写入：`rust/anchor-runner-host/src/goose_acp.rs` 约 654 行，用 `write_durable` 写 `{stem}.evidence.json`。
- `write_durable`（`rust/anchor-runner-host/src/main.rs:78`）的实现是先 `std::fs::write` 再 fsync。这是原地截断后重写，不是原子替换，并发读取可能读到空文件或半个文件。
- 读取：`rust/anchor-runner-host/src/goose_acp/trace.rs` 约 43 行，解析失败时报上面那条错误。

## 要做的事

1. 证据文件改为原子替换：先写同目录下的临时文件并 fsync，再 rename 覆盖目标文件，最后 fsync 父目录。分支已在 `goose_acp/trace.rs` 的 `store_evidence` 中修过，可以参考：`git -C /home/mansteinl/code/anchor-constitution-core show 490c000 -- rust/anchor-runner-host/src/goose_acp/trace.rs`。
2. 加一个回归测试：一边反复重写证据，一边并发读取，读取方永远只能看到完整的旧版本或完整的新版本。确认这个测试在修复前能失败。
3. 检查 `write_durable` 的其他调用点。凡是会被另一个进程或线程并发读取的文件，同样改为原子替换。不会被并发读取的可以不动，在交回中逐个列出调用点和判断理由。
4. 不修改既有断言，不放宽超时。

## 验收

按 `docs/runtime-contract-tests.md` 的标准命令跑 workspace 和 Goose 忽略测试，失败名单为空。只有 `anchor-wecom-gateway` 的已知时序用例在全量并发运行中失败、单独复跑能通过时，可以记为已知抖动。

## 交回

```
commit：<hash 标题>
修改的调用点：<列表；未改的及理由>
回归测试：<名称；修复前是否失败>
测试：workspace <…>；Goose <…>
```
