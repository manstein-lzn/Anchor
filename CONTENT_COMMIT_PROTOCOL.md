# 内容提交与对账协议

> **配套**：`WORKSPACE.md`（架构边界）。本文件只解决一个问题：
> **内容已冻结、控制事件尚未提交时崩溃，如何保证恢复闭包不被破坏。**

---

## 1. 参与者与职责

| 参与者 | 职责 | 是否 canonical |
|---|---|---|
| 控制存储（事件日志） | 记录 `NodeCommitted` / `NodeFailed`，是"哪些 revision 属于该 run"的唯一事实 | ✅ |
| 内容存储（git / CAS） | 保存 revision 的字节 | ✅（仅被引用部分） |
| 沙箱 | 执行、产出可写 delta | ❌ |
| Reconciler | 处理崩溃窗口，使 prepared 状态收敛 | — |

---

## 2. revision 状态机

```
prepared ──freeze+persist──▶ frozen ──verifier──▶ ready ──event──▶ committed
   │                            │                   │                 │
   │                            └── content 不可读 ──┴──▶ inconsistent_content
   └── 未被任何事件引用 ──▶ orphan ──▶ GC
```

| 状态 | 含义 | 谁可见 |
|---|---|---|
| `prepared` | 沙箱已冻结，内容正在持久化 | 仅本节点 + reconciler |
| `frozen` | 内容可读、digest 校验通过 | 本节点 |
| `ready` | 验证器已给出判定并持久化 | reconciler 可据此完成提交 |
| `committed` | 事件已追加 | 全体；进入恢复闭包 |
| `orphan` | 无事件引用 | 仅 GC |
| `inconsistent_content` | 事件已提交但内容不可读 | 运维；**失败关闭** |

---

## 3. 提交序列（幂等）

```
1. 沙箱执行（输入 = 声明的 immutable revision）

2. freeze
   2a. 计算 tree_digest（见 WORKSPACE.md §6）
   2b. 持久化内容 → revision_id（git commit / CAS put）
   2c. read-after-write 校验：重新读取并重算 digest，必须一致
   2d. 写 prepared 记录
       prepared = {
         node_run_id, attempt, revision_id, tree_digest,
         manifest_digest, operation_ids[], sandbox_id,
         created_at
       }

3. verifier 对 revision_id 运行（只读 + 独立 scratch）
   3a. 持久化 verifier_result 到 prepared 记录

4. commit（单次幂等追加）
   NodeCommitted(
     idempotency_key = f"node:{node_run_id}:{attempt}:committed",
     input_refs[], output_refs[]=[revision_id + artifacts],
     operation_ids[], verifier_result, runtime/tool/policy versions,
     prepared_revision_id
   )

5. 清除 prepared 记录（幂等）
```

**关键**：第 3a 步把验证结果**持久化进 prepared 记录**，使 reconciler 能直接完成第 4 步，而不必重跑验证器（重跑可能非确定性）。

---

## 4. 崩溃窗口与对账

| 窗口 | 崩溃点 | 磁盘状态 | 对账动作 |
|---|---|---|---|
| **W1** | 2b 之后、2d 之前 | 内容存在，无 prepared 记录 | 内容成为**孤儿** → GC 回收；节点重试产生新 revision |
| **W2** | 2d 之后、3a 之前 | prepared 记录存在，无验证结果 | 重新运行验证器（此时尚无判定，重跑安全）→ 写回 3a |
| **W3** | 3a 之后、4 之前 | prepared 记录 + 验证结果齐备 | reconciler 直接执行第 4 步（幂等） |
| **W4** | 4 之后、5 之前 | 事件已提交，prepared 记录残留 | 幂等清除 prepared 记录 |
| **W5** | 4 之后，内容不可读 | 事件引用缺失字节 | **`inconsistent_content`**：失败关闭，进人工诊断；**绝不从当前 mutable workspace 猜测内容** |

### 4.1 对账触发

| 触发 | 说明 |
|---|---|
| worker 启动 | 扫描本 worker 遗留的 prepared 记录 |
| supervisor 周期 | 扫描超过阈值仍处 prepared 的记录 |
| 人工 | 运维显式触发对账 |

### 4.2 对账幂等性

```
reconcile(prepared):
  if event(node_run_id, attempt, committed) exists:
      clear prepared            # W4
      return
  if prepared.verifier_result is null:
      result = run_verifier(prepared.revision_id)   # W2
      persist result
  if content_readable(prepared.revision_id):
      append NodeCommitted(...)                     # W3
      clear prepared
  else:
      mark inconsistent_content                     # W5
```

**任意多次执行结果相同。**

---

## 5. 完整性校验

| 校验 | 时机 | 失败动作 |
|---|---|---|
| `tree_digest` 重算一致 | freeze 后、每次读取时 | 失败关闭 |
| `manifest_digest` 一致 | 同上 | 失败关闭 |
| `output_refs` 均可在内容存储解析 | 提交前 | 失败关闭 |
| `verifier_result` 只引用声明的 refs | 提交前 | 失败关闭 |
| 事件引用的 revision 均可读 | 恢复时（`integrity.py`） | `inconsistent_content` |

---

## 6. 幂等键

| 动作 | 键 |
|---|---|
| 内容持久化 | `content:<tree_digest>`（内容寻址天然幂等） |
| 节点提交 | `node:<node_run_id>:<attempt>:committed` |
| 节点失败 | `node:<node_run_id>:<attempt>:failed` |
| 对账 | `reconcile:<node_run_id>:<attempt>:<window>` |

**同一 `tree_digest` 重复持久化返回同一 `revision_id`**（git/CAS 均满足）。

---

## 7. 失败模式

| 模式 | 检测 | 处置 |
|---|---|---|
| 内容存储超时 | freeze 超时 | 节点失败；内容可能已写入 → 孤儿 GC |
| 内容存储部分写 | read-after-write 校验失败 | 失败关闭；重新持久化 |
| 验证器崩溃 | lease 丢失 | 现有 supervision 语义；prepared 记录保留 |
| 并发 reconciler | 幂等键冲突 | 后到者无操作 |
| 控制存储不可用 | 提交失败 | 保留 prepared 记录，稍后对账 |

---

## 8. 故障注入测试矩阵（实现前必须能跑）

| # | 注入点 | 期望 |
|---|---|---|
| 1 | 2b 后 kill | 产生孤儿；GC 可回收；节点可重试 |
| 2 | 2d 后 kill | reconciler 重跑验证器并完成提交 |
| 3 | 3a 后 kill | reconciler 直接提交，不重跑验证器 |
| 4 | 4 后 kill | 幂等清除 prepared |
| 5 | 内容存储返回部分写 | read-after-write 捕获，失败关闭 |
| 6 | 提交后删除内容 | 恢复时 `inconsistent_content`，不猜测 |
| 7 | 两个 reconciler 并发 | 只产生一次提交 |
| 8 | 提交中控制存储超时 | prepared 保留，可重试 |

---

## 9. 待实测的三个方案

| 方案 | 说明 | 需测量 |
|---|---|---|
| A. 双写 + reconciler | 内容与控制各自写，靠对账收敛 | 崩溃窗口、恢复复杂度 |
| B. transactional outbox + prepared revision | 控制库先写 outbox，后台投递 | 吞吐、延迟 |
| C. 元数据与控制同库、字节放对象存储 | 元数据事务 + 内容外置 | 是否真能减少窗口 |

**决策依据**：故障注入下的窗口数量与恢复时间，而非理论优雅度。
