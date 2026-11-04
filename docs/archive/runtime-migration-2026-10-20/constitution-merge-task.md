# constitution-core 合入 master：master 侧须知

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

编制：2026-10-20。用户已批准把 `constitution-core` 合入 master。合并后，Run 的运行时整体换成新内核，旧的 `GraphRunner`、`RunStore`、`RunStatus` 全部删除。业务代码继续通过 `application/run_access` 门面访问 Run，门面接口不变。

## 你现在要做的

1. **完成当前的 debug 并按正常流程提交。** 门槛和平时一样，在当前 master 上通过即可。
   - 如果问题出在 Run 的执行、恢复或调度上（`execution.rs`、`node_host/`、`anchor-runtime/src/graph/`、`run_access` 内部），动手修之前先确认：这个问题在 `constitution-core` 上是否还存在。路径是 `/home/mansteinl/code/anchor-constitution-core`，只读，不要在那里改文件。那部分旧实现马上要被删除，在旧代码上修可能白做。
   - 如果问题在 Session、渠道、API、Web 这些层，照常修。
2. **提交后进入合并窗口：暂停新的提交**，直到合并完成。告诉用户"debug 已提交"。
3. 分支会先把你的提交合进去，重跑全部门槛，再交给审查方验收。审查通过后，由用户通知你执行：

   ```bash
   git -C /home/mansteinl/Anchor merge --ff-only constitution-core
   ```

   执行前确认工作区没有未提交的已跟踪改动。快进合并不会产生合并提交；如果命令失败，原样贴出输出并停下，不要改用别的合并方式。
4. **合并后**：
   - 后续 M4、M5 在 master 上进行；
   - Goose 标准命令照旧使用 `/tmp/anchor-goose-regression-target`；
   - 不推送，是否推送由用户决定。

## 补充（10-20 11:15）：合并前工作区必须干净

审查方核对时，master HEAD 是 `08db12f`，但工作区还有 42 个已跟踪文件没有提交（+2,157 / −232，最后修改时间 10:58）。改动主要在 Session、渠道、定时任务、凭据和 API 层。这批改动不处理，合并就无法进行：

- 其中 13 个文件分支上也改过，`merge --ff-only` 会因为要覆盖本地改动而直接拒绝。
- `application/run_access/projection.rs` 的改动用到了 `run.control.pause` / `cancellation` 和旧的 `active` 执行器表。分支上这些已经删除，合并后编译不过。

处理方式：

1. **如果这批改动是你的：** 做完后按正常门槛提交。
   - `projection.rs` 里缩小锁范围的那处修复不用移植到新架构。分支上对应的代码已经不存在，Run 的活跃状态只记在 `KernelRuns.active` 里。
   - 但请在提交说明里写清它修的是什么现象，分支 agent 合并时要据此确认新实现没有同样的问题。
   - `tests/goose_persistent_assistant.rs` 新增的场景照常提交。合并后它们会走 Kernel 路径，由分支的门槛覆盖。
2. **如果不是你的：** 不要动这些文件，告诉用户。
3. 提交完成后，确认 `git status --porcelain --untracked-files=no` 输出为空，然后告诉用户："master 已提交，工作区干净，HEAD = <hash>"。从这时起冻结提交。

## 补充（10-20 13:40）：快进被未跟踪文件挡住的处理

第一次快进失败，原因是 `experiments/constitution-poc/` 下有 14 个未跟踪文件，分支在 `9e9ef98` 已经提交了同名文件。审查方已逐个比对，14 个文件全部和 `constitution-core` 上的版本**字节一致**；其他未跟踪文件都不冲突。`target/` 目录不受影响，分支的 `.gitignore` 会忽略它。

处理方法：先把这 14 个文件移到备份目录，不删除，然后快进，最后比对内容。按下面的命令原样执行：

```bash
cd /home/mansteinl/Anchor
B=/tmp/constitution-poc-untracked-backup
mkdir -p "$B"
git ls-tree -r --name-only constitution-core experiments/constitution-poc | while read -r f; do
  mkdir -p "$B/$(dirname "$f")" && mv "$f" "$B/$f"
done
git merge --ff-only constitution-core
diff -r --exclude=target "$B/experiments/constitution-poc" experiments/constitution-poc && echo IDENTICAL
git log -1 --oneline                          # 应为 204d126
git status --porcelain --untracked-files=no   # 应只有 " M docs/constitution-merge-task.md"
```

- **本文件本身的未提交改动是预期的**：这一节是审查方在冻结期间写进来的，分支上没有改过这个文件，所以不影响快进。**不要在快进前提交它**，因为那样 master 就不再是 `204d126` 的祖先，快进会失败。
- 三项都符合（输出 `IDENTICAL`、HEAD 是 `204d126`、已跟踪改动只有本文件）时：
  1. 删除 `$B`；
  2. 只提交本文件，提交说明写 `docs: record constitution-core fast-forward procedure`；
  3. 告诉用户"已快进到 204d126，并提交了本文件，HEAD = <hash>"。
- 如果 `merge` 再次失败，原样贴出输出并停下。备份保留，不要删。要恢复的话，把 `$B/experiments/constitution-poc/` 下的文件移回原位即可。
- 如果 `diff` 有输出，同样停下，贴出输出，备份保留。
- 不推送。
