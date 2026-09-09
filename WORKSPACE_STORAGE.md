# 工作区物理存储与回收

> **配套**：`WORKSPACE.md`（架构边界）、`CONTENT_COMMIT_PROTOCOL.md`（提交协议）。
> 本文件解决：**"per-run 工作区"如何在磁盘上不爆炸。**

---

## 1. 四类数据

| 类别 | 生命周期 | canonical | 存储方式 | 回收 |
|---|---|---|---|---|
| **source revision** | 长期 | ✅ | git/CAS 去重（blob 级） | 可达性 GC |
| **dirty delta** | run 生命周期 | ❌ | CoW / overlay / reflink | freeze 后合并进 revision，delta 丢弃 |
| **依赖与缓存** | 跨 run | ❌ | 共享 keyed cache | key + LRU |
| **构建/临时输出** | 短期 | ❌ | scratch + quota | run 结束即删 |

**关键**：`dirty delta` 与 `cache` 都**不进** canonical，所以可以被无痛丢弃或重建。

---

## 2. CoW：为什么不是"每 run 一份 checkout"

朴素做法：

```
20 个并行 agent × 完整 checkout 5 GB = 100 GB
```

真实做法：

```
read-only hydrated base（共享）
        │
        ├── run1 upper delta（只存改动）
        ├── run2 upper delta
        └── ...
```

| 机制 | 适用 | 说明 |
|---|---|---|
| overlayfs | Linux 容器 | lowerdir=只读 base，upperdir=可写 delta |
| reflink（btrfs/XFS） | 本地文件系统 | 写时复制，无需 overlay |
| ZFS/btrfs snapshot | 有对应 FS 时 | 快照 + 克隆 |
| git worktree + hardlink | 纯 git 后端 | 共享 .git objects，工作树按需 |

**`SandboxProvider` 必须支持 `create_from_revision(revision)`**，底层是 CoW 还是全量拷贝属于 capability negotiation。

---

## 3. 容量模型（说明量级，非 benchmark）

假设一个真实研发仓库：

```
git object database      3 GB
checked-out source       5 GB
dependency/build cache   8 GB
每个 agent 实际改动      200 MB
20 个并行 agent
```

| 方案 | 计算 | 合计 |
|---|---|---|
| 朴素（每 run 全量 checkout + 各自依赖） | 3 + 20×5 + 20×8 | **263 GB** |
| CoW + 共享缓存 | 3 + 5 + 20×0.2 + 8 | **~20 GB** |

差距来自两点：**只读 base 共享** + **依赖缓存跨 run 共享**。

> 数字取决于构建系统与仓库；`node_modules`/`target/`/`ccache` 常常比源码大一个数量级。

---

## 4. 逻辑持久 ≠ 物理常驻

```
Run 开始
   ↓ create_from_revision(base)
沙箱执行
   ├── checkpoint → freeze revision（沙箱可继续）
   └── 等待（人/事件/定时）
          ↓ freeze revision
       销毁沙箱
          ↓
       只保留 revision + manifest
          ↓
       恢复时 hydrate（CoW）
```

| 场景 | 磁盘成本 |
|---|---|
| 运行中 | base（共享）+ delta + scratch |
| 等待中 | 已冻结的 canonical objects + manifest |
| 已结束 | 仅被事件引用的 revision |

**长期挂起 run 不再绑定一块永久 VM 盘。**

---

## 5. 配额：复用已有机制

Anchor 已有存储预算与滚动清理（`storage_budgets` + `retention_audit`）。扩展：

| 作用域 | 计量 | 超限动作 |
|---|---|---|
| 全局 | 数据库 + 内容存储 + cache | 滚动淘汰最老已结束 run |
| 每图 | 该图可达 revision 字节 | 同上，按图 |
| 每 run | delta + scratch | 单 run 上限，超限失败关闭 |
| cache | key 空间 | LRU 驱逐 |

**预算仍是监控/清理目标，绝不终止正在运行的节点**（I6）。

---

## 6. 可达性 GC

```
根集合 = 所有事件引用的 content_ref
         ∪ prepared 记录（未完成提交）
         ∪ 验证器产出的 artifact
   ↓ 展开
revision → tree → blobs
   ↓
不可达 blob/revision → 删除
```

| 对象 | 可达条件 | 备注 |
|---|---|---|
| revision | 被 `NodeCommitted`/`NodeFailed` 引用 | **失败节点的 revision 也是证据，不可回收** |
| prepared revision | prepared 记录存在 | 对账完成后转为可达或孤儿 |
| cache entry | key 未被驱逐 | 非 canonical，可随时删 |
| scratch | run 进行中 | run 结束即删 |

**与 artifact GC 统一**：现有 mark-and-sweep 扩展到 revision 与 tree。

---

## 7. 驱逐策略

| 类别 | 策略 |
|---|---|
| 已结束 run 的 revision | 最老优先，受保护集豁免（活跃 lease / 等待人工 / 未知副作用） |
| dirty delta | run 结束 / 失败 / 取消即删 |
| cache | LRU + key 前缀隔离（`(image_digest, lockfile_digest, arch, toolchain)`） |
| scratch | run 结束即删 |

**cache 的正确性边界**：命中缓存**不得改变 canonical 输入**；语义检索缓存必须带模型/embedding/index/schema 版本。

---

## 8. 可观测指标

| 指标 | 用途 |
|---|---|
| 每 run delta 字节 | 容量规划 |
| base 共享率 | CoW 有效性 |
| cache 命中率 / 驱逐率 | 缓存调优 |
| hydrate 延迟 | 冷启动成本 |
| freeze 延迟 | 提交路径开销 |
| 可达 revision 数 / 总字节 | GC 效果 |
| 孤儿字节 | 对账健康度 |

---

## 9. Benchmark 计划（实现前）

| # | 测什么 | 为什么 |
|---|---|---|
| 1 | hydrate 延迟 vs base 大小（CoW vs 全量拷贝） | 决定 `SandboxProvider` 契约 |
| 2 | freeze 延迟 vs delta 大小 | 决定提交路径是否可接受 |
| 3 | 20 个并行 run 的实际磁盘占用 | 验证容量模型 |
| 4 | cache 命中率对冷启动的影响 | 决定缓存分层 |
| 5 | GC 全量扫描耗时 | 决定是否需要增量索引 |
| 6 | overlayfs vs reflink vs worktree 的性能 | 选默认机制 |

**结论应以 benchmark 为准，不以产品文档为准。**
