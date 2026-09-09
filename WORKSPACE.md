# Workspace 设计 v2：控制面 + 内容面 = 恢复闭包

> **状态**：设计提案（v2），已纳入外部调研结论与五轮评审。
> **配套**：`CONTENT_COMMIT_PROTOCOL.md`（提交/对账协议）、`WORKSPACE_STORAGE.md`（物理存储与回收）。
> **本文件只定义架构边界与不变量，不含实现计划。**
> **一句话**：Anchor 已有控制面；本设计补上内容面，并用不可变内容引用把两者锁成一个**恢复闭包**。

---

## 1. 问题

### 1.1 现状

```
Canonical State + append-only events   ← 唯一真相来源
内容 = 不可变 blob（artifact://sha256/...）——控制面的一个投影
```

这回答「**发生过什么**」。它不回答「**工作现在长什么样**」——因为今天的内容只有不可变 blob，没有可写工作区、代码执行、共享缓存或历史版本。

### 1.2 为什么之前没有工作区（记录教训）

| 原因 | 说明 |
|---|---|
| 概念被同名词掩盖 | ADR-012 的 "private workspace" 指**每次工具调用的临时沙箱目录**，不是持久工作区 |
| 第一切片不需要 | 研究→评审→报告：输入是文档，输出是 Markdown，节点间无文件编辑 |
| 架构不变量排斥 | "Canonical State 是唯一真相"与"事件日志外的可变文件系统"直接冲突 |
| 顺序是先策略后能力 | ADR-012 明确先做只读工具（无副作用歧义），写/执行留后 |

**教训（已入 ADR）**：安全边界（sandbox）与持久实体（workspace）是两个概念，必须分开建模；合并成一个词会同时掩盖两个缺口。

---

## 2. 核心模型：Recovery Closure

**不是"两个真相来源"。** 准确表述是：

```
Canonical Control State  +  Canonical Content Store  =  Recovery Closure
```

| 谁 | 决定什么 |
|---|---|
| 控制面事件历史 | **哪些 revision 属于这个 run** |
| 内容存储 | **这些 revision 的字节是什么** |

```
没有事件引用的 revision  → 孤儿（对执行语义无意义）
有事件引用却缺字节       → 完整性故障（失败关闭，绝不猜测）
```

**Live workspace、sandbox/VM 状态、cache、memory、vector index、summary 全部不参与 canonical truth。**

### 2.1 与 I2 的关系

`PRODUCT_VISION.md` 的 I2 已修订为 **Canonical Recovery Closure**（见该文件）。本设计的所有决策都服从该表述。

---

## 3. 实体与生命周期

### 3.1 四级关系

```
Project / Repository                    长期，不可变历史
        │ immutable base revision
        ▼
GraphVersion                            不可变模板，只**声明**工作区契约
        │
        ▼
Run                                     拥有 mutable workspace 所有权
  └── node-level revision lineage        ← 见 §6
```

| 实体 | 生命周期 | 作用 |
|---|---|---|
| **Project** | 长期 | 一个仓库（代码 / 数据集 / 文档集） |
| **GraphVersion** | 不可变 | 声明 `workspace_input` / `workspace_policy` / `merge_policy` / `sandbox_policy` |
| **Run** | 单次 | 实例化隔离工作区；**绝不与同 graph 的其它 run 共享工作树** |
| **NodeRun** | 单节点 | 消费输入 revision，产出输出 revision |

### 3.2 硬规则

> **Graph 绝不拥有 mutable workspace。Writable workspace 属于 Run。**

否则同一 graph 的并发 run 会写同一个工作树，立即破坏 I8 与 I9。

### 3.3 Graph 如何声明

```yaml
workspace:
  source: workspace://repo123@a9fe31...   # immutable base revision
  mode: fork_per_run
  merge_policy: require_clean
  sandbox_policy: default
```

每个 Run 各自 `fork(base=a9fe31...)`，互不可见。

---

## 4. 边界规范：`content_ref`

```
content_ref :=
    artifact://sha256:<64-hex>
  | workspace://<workspace-id>@<immutable-revision>/<path>
```

**`<immutable-revision>` 禁止是** `main`、`latest`、branch HEAD、`sandbox_id` 或任何随时间改变的名字。一旦进入事件历史，解析结果必须**永不漂移**。

### 4.1 revision 载体

```
revision.kind = git_commit | cas_tree
```

| 后端 | 用途 | revision.id | digest |
|---|---|---|---|
| **Git-backed** | 代码仓库（第一实现） | git commit OID | Anchor 自算 SHA-256 tree digest |
| **CAS-backed** | 通用文件树（Office/数据集/PDF/notebook） | CAS tree root | 同左 |

**Git 是代码工作区的第一实现，不是 Anchor 的内容模型本身。** Anchor 自算 digest 使语义独立于 git 的 object format（sha1/sha256）。

---

## 5. revision 的内容集与 manifest

### 5.1 内容集定义

```
freeze = git add -A && git commit
       → revision = 该 commit 的 tree
       → ignored 文件天然排除
```

| 类别 | 是否进 revision | 去向 |
|---|---|---|
| tracked 源码 | ✅ | revision |
| untracked 新文件 | ✅ | revision（`add -A` 覆盖） |
| ignored 依赖/构建产物 | ❌ | 跨 run 共享 cache |
| 证据型生成物（测试报告、图表、构建产物） | ❌ | **artifact** |
| 大二进制（模型权重、大 PDF） | ❌（v1 体积上限） | artifact + 工作区内小引用文件 |

**`.gitignore` 是"源码 vs 派生物"的单一真相来源**，不再有第二套规则。

### 5.2 一个节点可以同时产出两类内容引用

```
NodeCommitted(
  output_content_refs = [
    workspace://ws-1@<sha>/src/...,     ← 源码树
    artifact://sha256/<digest>          ← 测试报告
  ]
)
```

### 5.3 manifest

```
revision.manifest:
  entries: [ {path, mode, blob_digest, size} ]   # 按 path 原始字节序
  digest: <tree_digest>
```

---

## 6. `tree_digest` 规范化

```
tree_digest = sha256( canonical_manifest_bytes )

canonical_manifest_bytes =
   for each entry in byte_sorted(path):
       path_utf8  \0  mode_octal  \0  blob_sha256_hex  \n
```

| 项 | 规定 |
|---|---|
| 路径排序 | 原始字节序（非 locale） |
| 路径编码 | UTF-8 字节；**不做 Unicode 规范化** |
| mode | `100644` / `100755` / `120000` / `160000` |
| symlink | mode=120000，blob=链接目标字节；不跟随 |
| submodule | mode=160000，blob=commit id；**不递归** |
| 空目录 | 不表示（与 git 一致） |
| 换行/编码 | **绝不规范化** |
| 大小写 | 原样保留 |

> **digest 只含内容，不含 provenance。** base revision、策略、模型版本存在 revision 记录里，不进 digest——否则同一棵树因来源不同得到不同 digest，去重失效。

---

## 7. 写入与提交协议

**Agent 不能直接写文件系统。** 所有变更经工作区网关。完整协议见 `CONTENT_COMMIT_PROTOCOL.md`，要点：

```
声明输入 revision
   ↓
hydrate 沙箱（CoW：只读 base + 空 delta）
   ↓
执行
   ↓
freeze → revision + manifest + tree_digest
   ↓
read-after-write 校验
   ↓
验证器对冻结 revision 判定（§9）
   ↓
Append NodeCommitted / NodeFailed（含 input_refs、output_refs、operation_ids、verifier、版本）
   ↓
推进图
```

| 崩溃窗口 | 状态 | 处置 |
|---|---|---|
| revision 已冻结、事件未提交 | **orphan prepared revision** | reconciler 回收或重关联 |
| 事件已提交、revision 不可读 | **INCONSISTENT_CONTENT** | 失败关闭 + 人工诊断，**绝不从当前 mutable workspace 猜** |

---

## 8. 并发模型

### 8.1 节点级 revision 血缘

```
base_revision
   ↓ node A:  input=base       → output=rev_A
   ↓ node B:  input=rev_A      → output=rev_B
   ↓ parallel: input=rev_B     → rev_P1, rev_P2
   ↓ join:    input={rev_P1,rev_P2} → output=rev_merged
```

**每个节点消费声明的输入 revision，产出自己的输出 revision。** run 的工作区是这些 revision 构成的血缘图，不是一条可变分支。

### 8.2 关键推论

> **并发安全由「不可变输入 + 显式合并」保证，不需要工作区写锁。**

节点各自从同一 base 派生 CoW delta，互不可见（输入是已提交 revision）。依赖图本身提供串行化；唯一冲突点是 merge。

因此"单写者锁"从**正确性机制**降级为**资源控制机制**（防磁盘/CPU 爆炸）。

### 8.3 合并策略

```yaml
merge_policy: require_clean    # v1 默认且唯一
```

| 情况 | 行为 |
|---|---|
| 三方合并无冲突 | 接受 → 新 revision → **仍需验证器** |
| 有冲突 | **失败关闭** → 结构化错误 → 升级为 `human_task`，两个分支 revision 作为证据附上 |

**v1 明确不做**：prefer-source/target、自动 union、agent 自动解冲突。合并本身是 `operation + commit`，可审计。

### 8.4 进度信号

`tree_digest` 长时间不变 → 无进展 → watchdog 报警。**不违反"不用轮次预算杀健康 agent"**（I6）。

---

## 9. 验证器与冻结 revision

```
沙箱内执行（CoW delta，可写）
   ↓ freeze
revision_R（不可变）
   ↓
验证器对 revision_R 运行（工作区只读 + 独立 scratch）
   ↓
verdict
   ├─ pass → NodeCommitted(input_refs, output_refs=[R], verifier_result)
   └─ fail → NodeFailed (input_refs, output_refs=[R], verifier_result)
```

| 规则 | 说明 |
|---|---|
| 验证器**不能改工作区** | 否则提交内容 ≠ 被验证内容 |
| 验证器可有**独立 scratch** | 跑测试需要临时目录 |
| 验证器产出走 **artifact** | 不污染 revision |
| 验证器在沙箱内运行 | 同一 `SandboxProvider`，只读挂载 |
| **被拒 revision 仍是证据** | 由失败事件引用，**不是孤儿**（影响 GC） |
| 重新验证 | 确定性验证器可对历史 revision 重跑；模型验证器重跑产生**新**记录（R2/R3） |

---

## 10. 权限模型（声明式，默认拒绝）

```
agent × {
  read:    [glob]
  write:   [glob]      # 默认空
  exec:    [command]   # 默认空
  network: [domain]    # 默认空
  secrets: [secret_ref]# 沙箱注入，绝不进 prompt 或工作区
}
```

| 原则 | 说明 |
|---|---|
| 默认拒绝 | 未声明一律不可用 |
| 最小可见 | 与 I8 一致：共享物理工作区 ≠ 每个 agent 看到全部 |
| 密钥只传引用 | capability token / broker；不持久化 secret value |
| 越权可审计 | 每次越权尝试记 event |

---

## 11. 执行沙箱

| 维度 | 设计 |
|---|---|
| 隔离分级 | 进程/bubblewrap（可信低风险）→ OCI 容器（普通）→ gVisor（高风险）→ microVM/Firecracker（互联网可访问、执行未知代码） |
| 挂载 | 工作区 CoW（只读 base + 可写 delta）；外部目录只读且必须声明 |
| 网络 | 默认无网络；按 agent 白名单开放域名，经代理审计 |
| 资源 | CPU / 内存 / 进程数 / 磁盘 / 墙钟上限 |
| 密钥 | 沙箱注入，作用域限定单次命令 |
| 输出 | 作为 operation 结果落入账本（有上限） |
| 抽象 | `SandboxProvider` 最小契约：`create_from_revision / exec / read_write / freeze_revision / network_policy / secret_ref / terminate` |

高级能力（pause-memory、fork-memory）只作 capability negotiation，**不得进入核心恢复假设**。

---

## 12. 物理存储与回收

完整模型见 `WORKSPACE_STORAGE.md`。四类数据：

| 数据 | 生命周期 | canonical | 存储 |
|---|---|---|---|
| source revision | 长期 | ✅ | git/CAS 去重 |
| workspace dirty delta | run 生命周期 | 临时 | CoW / overlay / reflink |
| 依赖与缓存 | 跨 run | ❌ | shared keyed cache |
| 构建/临时输出 | 短期 | ❌ | scratch + quota |

核心原则：

> **workspace 逻辑持久，不必物理常驻。**

```
等待 → freeze → 销毁沙箱 → 只留 revision + manifest
恢复 → hydrate（CoW）→ 继续
```

长期挂起 run 的磁盘成本 ≈ 变更的 canonical objects + manifest，而不是永久 VM 盘。

---

## 13. 不变量合规表

| 不变量 | 双平面下如何保持 |
|---|---|
| I1 图不可变 | 工作区契约是图版本的一部分 |
| **I2 恢复闭包** | **修订**：控制事件 + 被引用且校验通过的内容 revision 构成闭包 |
| I3 副作用账本 | 写入即 operation；执行命令亦为 operation |
| I4 完成需验证 | 验证器读冻结 revision，确定性判定 |
| I5 长等待不占 worker | freeze 后沙箱可销毁，恢复时 hydrate |
| I6 不因预算中断 | 资源上限是单次命令限制；进度靠 tree_digest 变化检测 |
| I7 供应商中立 | git + 可插拔 `SandboxProvider` |
| I8 声明式输入 | 强化为路径级读写范围声明 |
| **I9 法证重放** | 分级 R0–R3；pinned revision + 记录非确定性调用结果 |

---

## 14. 迁移路径（非破坏式）

| 阶段 | 内容 | 风险 |
|---|---|---|
| **W0.1** | `content_ref` 边界类型 + 失败关闭解析（ADR-027） | ✅ 已完成 |
| **W0.2** | 只读工作区（挂载、读、只读执行） | 极低 |
| **W1** | 单写者 + 冻结/提交协议（operation/commit/event） | 中 |
| **W2** | exec 沙箱（网络白名单、资源上限、密钥注入） | 中高 |
| **W3** | 并行 fork/merge（`require_clean`） | 高 |
| **W4** | Project 长期仓库 + 跨 run 缓存 | 中 |

现有知识工作图不受影响（它们只用 artifact）。

---

## 15. 剩余开放问题

| # | 问题 | 归属 |
|---|---|---|
| 1 | 跨存储 commit/reconcile 的实测（三种方案对比故障窗口） | `CONTENT_COMMIT_PROTOCOL.md` |
| 2 | 工作区物理存储与 eviction 的 benchmark | `WORKSPACE_STORAGE.md` |
| 3 | 合并语义扩展（schema-aware / 文档级） | W3 之后 |
| 4 | `SandboxProvider` 的可移植性与冷启动成本 | W2 |
| 5 | 法规适用性矩阵（EU AI Act / 中国标识办法） | 产品化前 |

**已收敛、不再讨论**：Git vs Merkle（→ Git-first + CAS-backed）；per-graph vs per-run（→ graph 声明、run 实例化、node 级血缘）；单写者锁（→ 由不可变性替代，锁降为资源控制）。

---

## 16. 非目标

- 多用户 / 多租户 / 远程 MCP 传输
- 让 agent 自行注册能力
- 允许 agent 绕过审批门或直接改 Canonical State
- 在边界与协议确认前编写 Workspace 实现代码
