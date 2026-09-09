# Workspace 设计：控制面与内容面的边界

> **状态**：设计提案，待外部调研（见 `docs/AGENT_ARCHITECTURE_RESEARCH_BRIEF.md`）验证后转 ADR。
> **本文件只定义架构边界与不变量，不含实现计划。**
> **一句话**：Anchor 现在只有控制面；本设计补上内容面，并用一条显式边界把两者锁在一起。

---

## 1. 问题：一个真相来源 vs 两个

### 1.1 现状

```
Canonical State + append-only events   ← 唯一真相来源
   └─ run / node / lease / decision / verification / operation / approval / wait
内容 = 不可变 blob（artifact://sha256/...）——控制面的一个投影
```

`PRODUCT_VISION.md` 的硬约束原文：

> Canonical State and append-only events are the **recovery source**; context and
> memory are **projections** with provenance and retention rules.

这回答的是「**发生过什么**」。它不回答「**工作现在长什么样**」——因为今天的内容只有不可变 blob。

### 1.2 缺口

| 想要的能力 | 今天的障碍 |
|---|---|
| 可写文件系统 / 代码执行 | 只有不可变 blob + 每次调用的临时沙箱 |
| 共享缓存区 | 节点间只交换不可变值 |
| 工作区版本（回滚 / diff） | 无工作区实体 |
| 历史落盘 / git 管理 | 无仓库概念 |
| 外部只读挂载 + 受控联网 | 沙箱 `--unshare-all`、只读根 |

### 1.3 结论

> 不是功能缺失，是**架构缺一层**。内容需要成为**一等持久状态**，而不是控制面的投影。

---

## 2. 双平面模型

```
┌─────────────────────────────────────────────────────────────┐
│ 控制面（已有，保留）                                          │
│   事件日志 · run/node/lease/decision/verification/operation  │
│   性质：事件溯源、确定性、可重放                               │
│   回答：谁在何时做了什么、为什么、能否恢复                      │
└───────────────────────────┬─────────────────────────────────┘
                            │ 边界：content_ref（内容寻址 + pin）
┌───────────────────────────┴─────────────────────────────────┐
│ 内容面（新增）                                                │
│   Project（长期仓库） · Workspace（run 的工作树）              │
│   Artifact（不可变 blob，已有）                                │
│   性质：版本化可变树（git）、内容寻址                           │
│   回答：工作现在长什么样                                       │
└─────────────────────────────────────────────────────────────┘
```

**关键原则**：两个面都是真相，但回答不同问题。任何一方的变更都必须能被另一方引用与验证。

---

## 3. 实体与生命周期

### 3.1 三个实体

| 实体 | 生命周期 | 作用 |
|---|---|---|
| **Project** | 长期 | 一个仓库（代码库 / 数据集 / 文档集）。有稳定的 `project_id`。 |
| **Workspace** | 单次 run | Project 的一个**工作树**（git worktree / 分支）。隔离、可写、可丢弃。 |
| **Run** | 单次执行 | pin 到 graph 版本；绑定 0 或 1 个 Workspace。 |

```
Project (长期仓库, 不可变历史)
   ├── Workspace@run-1  (branch: run/<run_id>)
   ├── Workspace@run-2  (branch: run/<run_id>)
   └── ...
```

### 3.2 为什么不是"每个 graph 一个工作区"

**graph 是不可变模板，run 是它的实例。** 如果工作区挂在 graph 上：

```
同一个 graph 并发跑 2 次 → 两个 run 共享同一个目录 → 互相踩
```

正确做法：

| 层 | 定义什么 |
|---|---|
| **Graph** | **声明**工作区契约（是否用工作区、绑定哪个 Project、挂载与权限） |
| **Run** | **实例化**一个隔离的 Workspace |
| **Project** | 提供长期内容与历史 |

这样既满足"每个 graph 有自己的工作区约定"，又天然隔离并发运行。

### 3.3 Graph 如何声明工作区

在 graph `metadata` 中声明（示意，最终形态待调研后定）：

```json
{
  "workspace": {
    "mode": "run",
    "project_ref": "projects.my-service",
    "base_revision": "main",
    "expose": {
      "agents.academic.planner": { "read": ["**/*.md"], "write": [] },
      "agents.coder": { "read": ["src/**"], "write": ["src/**"],
                        "exec": ["pytest", "ruff"], "network": ["pypi.org"] }
    }
  }
}
```

**声明式是刻意的**：默认拒绝，权限按 agent 逐个授予。

---

## 4. 边界规范：`content_ref`

### 4.1 定义

```
content_ref :=
    artifact://sha256/<64-hex>                          # 不可变 blob（已有）
  | workspace://<workspace_id>/<commit_sha>             # 工作树快照（树）
  | workspace://<workspace_id>/<commit_sha>/<path>      # 树中某个文件
```

### 4.2 三条边界不变量

| # | 不变量 |
|---|---|
| **B1** | 控制面必须永远能证明「这个节点看到的是哪个 revision」——节点输入快照包含其全部 `content_ref` |
| **B2** | 内容面的每次变更必须对应控制面的一条 operation（含 before/after 摘要与 actor） |
| **B3** | 恢复 = 重放控制面 + 按 pinned revision 物化内容面；任一侧缺失即**失败关闭**，不静默继续 |

### 4.3 与 I2 的关系（需正式修订）

现状 I2 说「Canonical State 是恢复源」。引入内容面后的精确表述建议：

> **控制面是"发生过什么"的恢复源；内容面是"存在什么"的恢复源。两者通过 pinned
> `content_ref` 关联；任一侧不可解析时，运行失败关闭而非降级继续。**

这条修订是本设计**最重要的产出**，需要调研确认业界是否有更成熟的表述。

---

## 5. 写入协议（唯一写入口）

**Agent 不能直接写文件系统。** 所有变更经工作区网关：

```
agent 请求写 /src/a.py
   ↓
1. 权限检查    该 agent 的 write 范围包含此路径吗？
   ↓
2. 记 operation  operation_id + request_hash + before_sha（幂等、可重放）
   ↓
3. 执行写入    在沙箱内落盘
   ↓
4. git commit  产生新 commit_sha（原子）
   ↓
5. 发事件      workspace.committed（含 before/after、actor、reason）
   ↓
下游节点输入 = workspace://<id>/<new_sha>
```

| 性质 | 保证 |
|---|---|
| 幂等 | 同一 operation_id 重放返回已提交结果，不重复写 |
| 审计 | 每次写入都有 operation + 事件 + commit 三元组 |
| 可回滚 | commit 是原子检查点 |
| 失败关闭 | 权限不足 / 校验失败 → 拒绝，不产生半写状态 |

---

## 6. 版本与复现

| 项 | 设计 |
|---|---|
| 版本载体 | git commit（内容寻址的树） |
| 节点输入 | pin 到 `<commit_sha>`，写入输入快照 |
| 复现 | 按 commit_sha 检出即可得到完全相同的文件树 |
| 差异 | `git diff before..after` 直接可读 |
| 大文件 | 不进 git；走 artifact store，工作区内以引用文件表示（待调研确认） |
| 快照不再内联正文 | 节点输入只放 `content_ref`，正文按需解析 → **同时解决当前快照内联导致的重复存储** |

---

## 7. 并发模型（分阶段）

| 阶段 | 模型 | 冲突处理 | 适用 |
|---|---|---|---|
| **P1** | 只读工作区 | 无 | 先验证边界，零风险 |
| **P2** | 单写者 | 写锁串行 | 大多数场景足够 |
| **P3** | 路径级写锁 | 不相交路径可并行 | 大仓库分工 |
| **P4** | 分支 + 合并 | 冲突升级为审批节点 | 真正的多 agent 并行开发 |

**P2 是默认**：简单、可预测、无死锁。P3/P4 只在有证据需要时引入。

### 7.1 锁的层级（防止死锁）

```
必须先持有 node lease，再申请 workspace 锁；
禁止反向获取。锁顺序固定：node → workspace → 路径。
```

这是必须写死的规则，否则 worker 之间会互相等待。

---

## 8. 恢复语义

| 场景 | 行为 |
|---|---|
| worker 崩溃（已提交） | 工作区一致；重放控制面继续 |
| worker 崩溃（未提交） | 未提交变更**丢弃**（或 stash 到 `refs/recovery/<claim_id>` 供诊断） |
| lease 过期被回收 | 同上；工作区回到最后提交状态 |
| commit_sha 缺失/损坏 | **失败关闭**：节点失败，`error_code=content_missing`，进入人工诊断 |
| Project 不可达 | 节点失败关闭；不静默降级为"无工作区" |

---

## 9. 权限模型（声明式，默认拒绝）

```
agent × {
  read:    [glob]      # 可读路径
  write:   [glob]      # 可写路径（默认空）
  exec:    [command]   # 可执行命令白名单（默认空）
  network: [domain]    # 可访问域名（默认空）
  secrets: [secret_ref]# 由沙箱注入为环境变量，绝不进入 prompt 或工作区
}
```

| 原则 | 说明 |
|---|---|
| 默认拒绝 | 未声明的能力一律不可用 |
| 最小可见 | 与现有 I8（声明式输入）一致：共享物理工作区 ≠ 每个 agent 看到全部 |
| 密钥不进模型 | 沙箱注入；模型只能调用工具，不能读取密钥 |
| 可审计 | 每次越权尝试都记 event |

---

## 10. 执行沙箱

| 维度 | 设计 |
|---|---|
| 隔离 | 容器 / microVM（具体选型待调研：冷启动 vs 隔离强度 vs 成本） |
| 挂载 | 工作区可写；外部目录**只读**且必须声明 |
| 网络 | 默认无网络；按 agent 白名单开放域名，记录出站审计 |
| 资源 | CPU / 内存 / 进程数 / 磁盘 / 墙钟上限 |
| 密钥 | 沙箱注入环境变量，作用域限定单次命令 |
| 产物 | 执行输出作为 operation 结果落入账本（有上限） |

---

## 11. 保留与回收（复用已有机制）

| 对象 | 策略 |
|---|---|
| Workspace 工作树 | 随 run 保留策略回收（已有 `retention` 机制） |
| git 对象 | 按 Project 做 unreachable 对象 GC |
| 依赖缓存 | 内容寻址、可驱逐、跨 run 共享（只读或单写者） |
| Artifact | 已有 mark-sweep GC |

---

## 12. 不变量合规表

| 不变量 | 双平面下如何保持 |
|---|---|
| I1 图不可变 | 不变；工作区契约写在图 metadata，仍是不可变版本的一部分 |
| I2 恢复源 | **修订**为双恢复源 + 失败关闭（§4.3） |
| I3 副作用账本 | 写入即 operation；执行命令亦为 operation |
| I4 完成需验证 | 不变；验证器可读 `content_ref` 做确定性检查 |
| I5 长等待不占 worker | 不变；等待期间工作区状态由 commit 保证 |
| I6 不因预算中断 | 不变；工作区资源上限是**单次命令**限制，不是任务寿命预算 |
| I7 供应商中立 | git + 容器/microVM 均为可替换成熟件 |
| I8 声明式输入 | **强化**：路径级读写范围声明 |
| I9 可复现 | 由 pinned commit_sha 保证；输入快照包含全部 content_ref |

---

## 13. 迁移路径（非破坏式）

| 阶段 | 内容 | 风险 |
|---|---|---|
| **W0** | 定义 `content_ref` + 只读工作区（挂载、读文件、只读执行） | 极低 |
| **W1** | 单写者 + 写入协议（operation/commit/event） | 中 |
| **W2** | exec 沙箱（网络白名单、资源上限、密钥注入） | 中高 |
| **W3** | 并发扩展（路径锁 → 分支合并） | 高 |
| **W4** | Project 长期仓库 + 跨 run 缓存 | 中 |

**每一阶段都不影响现有知识工作图**：它们只用 artifact，不绑定工作区。

---

## 14. 待调研回答的开放问题

1. 容器 vs microVM：我们的延迟/成本/隔离要求下哪个更合适？
2. git 是否足以承载工作区（大文件、二进制、超大仓库）？
3. 是否存在更好的内容面载体（内容寻址文件系统、事务文件系统）？
4. 分支合并（P4）在 agent 场景下的实际成功率如何？
5. I2 的修订措辞，业界是否有更成熟的表达？
6. 网络白名单在依赖安装场景下的可行性与安全代价？
7. 共享缓存的正确性边界（语义缓存是否可用于证据链）？
8. 是否有法规要求影响审计设计？

---

## 15. 非目标

- 多用户 / 多租户 / 远程 MCP 传输
- 让 agent 自行注册能力（能力仍是文件配置、只读暴露）
- 允许 agent 绕过审批门或直接改 Canonical State
- 在边界与调研结论确认前编写 Workspace 实现代码
