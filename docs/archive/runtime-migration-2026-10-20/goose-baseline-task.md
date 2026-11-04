# G0 任务书：恢复固定 Goose 回归基线（master）

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

任务已完成：`250435d` 经 `3b7d1d9` 合入 master，原 worktree 与分支按用户要求清理。固定 Goose 不变；三项残留在 F1 完成后由 `cc99091` 单独修正。当前标准命令、比较基线、断言修正理由与验证边界见 [Runtime 回归](../../runtime-contract-tests.md)。下文保留原任务要求，不作为当前待办。

编制：2026-10-19。适用分支：`master`（`/home/mansteinl/Anchor`）。**优先级高**：master 的门面任务（F1/F2）和 `constitution-core` 的迁移任务，都以这组测试作为验收门槛。

## 现象

F0 验收时，用 master 锁定的 Goose（`/var/tmp/anchor-goose-build/artifacts/goose-acp.first`，sha256 `17e065…`，与源码锁定值一致）运行忽略测试，几乎全部失败：

| 套件 | 通过 / 失败 |
| --- | --- |
| goose_acp | 1 / 14 |
| goose_channel | 0 / 4 |
| goose_compaction | 0 / 4 |
| goose_completion | 0 / 9 |
| goose_conversation | 0 / 13 |
| goose_development_environment | 0 / 1 |

`goose_distribution` 之后的套件尚未运行。

共同错误：`ACP _goose/unstable/session/extensions/add JSON-RPC error code -32603`。宿主的诊断显示 Goose 的 stderr 有 39 字节，原文被隐藏（`goose_acp/transport.rs` 的 `StderrDiagnostics`）。现场保留在 `/tmp/.tmpy1OL3s`。

已知线索：

- 锁定值在 `586916d`（2026-10-18，"close assistant rebuild and provider error handling"）中由 `dc2a43…` 改为 `17e065…`。当前二进制文件的构建时间是 2026-10-18 19:26。
- 发出该请求的代码：`goose_acp/session.rs` 约 124 行。
- 证据目录：`.local/run-access-facade-20261019/README.md`、`goose-ignored-configured.log`、`goose-independent-retry.log`。
- F0 只新增了测试和文档，没有改生产代码，所以这不是 F0 引入的问题。

## 要做的事

1. **拿到 stderr 原文。** 例如本地临时打印、直接用现场生成的配置运行 Goose，或抓取扩展启动命令的输出。临时诊断代码**不提交**。
2. **定性**，结论必须有证据支撑：
   - **环境问题**：例如扩展依赖的二进制未构建、Bubblewrap 权限、测试需要的环境变量（F0 已发现 `ANCHOR_TEST_RUST_TOOLCHAIN` 需要设置）；
   - **新 Goose 构建与宿主不兼容**：例如 `586916d` 的补丁或构建方式改变了 extensions/add 的行为；
   - **宿主回归**。

   能找到旧版本二进制或旧提交的，用它做对照。
3. **修复。** 环境问题：写进 `docs/runtime-contract-tests.md` 的标准命令，必要时让测试给出明确提示，不能只是失败。构建或宿主问题：按正常开发流程修复，并附回归证据。**不得**用改断言、加 `#[ignore]`、放宽超时来掩盖。
4. **重跑并记录基线。** 完整运行 `cargo test -p anchor-runner-host -- --ignored`（使用正确的环境变量），结果写入 `docs/runtime-contract-tests.md` 或 `.local` 下的证据，包括：各套件的通过、失败、忽略数，以及仍然失败的测试名单和原因。这个名单就是之后各任务使用的 **Goose 基线**。

## 停止条件

- 修复需要升级或更换 Goose 版本：给出方案，交回用户决定。
- 根因在外部服务或凭据：写明后交回。

## 交回

```
根因：<一句话 + 证据位置>
修复：<commit 或"仅环境/文档"及内容>
Goose 基线：<各套件 通过/失败/忽略>；仍失败：<测试名单及原因>
标准命令：<最终命令>
```
