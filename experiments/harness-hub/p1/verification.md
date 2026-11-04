# P1 内核变更：验证与审查请求

执行：2026-10-20，按 [任务书 §P1](../../../docs/archive/design-records-2026-11-04/harness-data-hub-tasks.md#p1-内核变更时间因果配置只读读取)，由主 Agent 直接实现。字段的实际形状和选择理由见 [总体架构 5.1](../../../docs/harness-data-hub.md#当前职责与依赖)。

## 改动范围

| 文件 | 增 / 删 |
| --- | ---: |
| `rust/anchor-kernel/src/kernel.rs` | +46 / −12 |
| `rust/anchor-kernel/src/model.rs` | +68 / −4 |
| `rust/anchor-kernel/src/store.rs` | +74 / −13 |
| `rust/anchor-kernel/src/lib.rs` | +2 / −2 |
| `rust/anchor-kernel/tests/constitution.rs` | +8 / −3 |
| `rust/anchor-kernel/tests/recorded_facts.rs`（新增） | +374 |
| `rust/anchor-runner-host/src/application/run_access/kernel_probe.rs` | +2 / −1 |

- 改动前，`git diff 0859373 -- rust/anchor-kernel` 等于本次的工作区改动，说明 P1 之前内核确实零改动。
- Runtime、GraphHost、Web 都没有改动。Host 只改了探针重建时的字段映射。

## 不变量核对

- **裁决规则零改动**：`adjudicate`、`resolve_pending`、`charge`、`task_control` 和三个宪法 Validator 的判断分支都没有变。新字段只在构造 Stamp、Point、Creation 和人工批准记录时写入，没有任何地方读取它们来做判断。
  - `recorded_facts.rs::verdicts_do_not_depend_on_time_cause_or_configuration` 用两组不同的时钟、配置和因果引用，跑同一组提交序列，其中包括预算越界结算。结果是裁决序列和最终资源完全相同。
- **仍然只有一个写入口**：`submit` 的签名没有变；只新增了 `with_clock` 这一个构造选项。
- **没有新增 Reason、Phase、Waiter、Action 或 Store 种类。**
- **旧数据**：新字段一律可选。没有新字段、并且带有旧 `knobs` 的记录可以正常读取，读出来是"未记录"。现有的 `storage_regressions.rs` 和 `file_store_deletion.rs` 夹具仍然带着 `knobs`，并且通过。
- **允许改动的断言**：只有 `tests/constitution.rs` 的 `every_verdict_is_recorded_and_measured_including_refusals`，从全局 knobs 改为 Principal 携带配置。原来"第二个点的 knobs"这条断言改为"第一个点没有配置，第二个点是 `config-v2`"，覆盖面没有缩小。其他断言都没有改。

## 新增测试（`tests/recorded_facts.rs`，9 项）

1. 内核时钟给每条记录盖时间，包括被拒绝的记录；`seq` 保持顺序。
2. 因果引用和配置原样进入历史和测量点，包括未授权被拒的提交。
3. 创建时间由内核盖章，创建者自带的时间会被覆盖；配置摘要在修改、结算和重新打开存储之后都保留。
4. 内核为提交生成的 effect 资源，沿用该提交的时间和配置。
5. 人工批准后写入目标资源的那条记录，因果引用指向 pending 资源上的决定记录。
6. 旧格式的记录和资源读出来是"未记录"。
7. 不同时钟、配置和因果引用下，裁决完全相同。
8. `HistoryReader` 的结果和 `FileStore` 一致：
   - 读取前后，数据目录的完整文件树（路径和大小）完全不变；
   - 写入者正在追加的半行不返回；
   - 资源被删除后读出来是不存在。
9. `HistoryReader` 不会创建不存在的根目录。

## 门槛

在标准构建目录 `/tmp/anchor-goose-regression-target` 下执行，原始日志在 `/tmp/anchor-p1/`，不入库。

| 门槛 | 命令 | 结果 |
| --- | --- | --- |
| 内核 | `cargo +stable test -p anchor-kernel --locked --offline` | 原有 53 项加新增 9 项，全部通过 |
| workspace | `cargo +stable test --workspace --locked --no-fail-fast` | 133 个目标，**2,209 通过 / 0 失败 / 126 忽略**。M5 是 2,200 / 0 / 126，多出的 9 项正是新增测试 |
| 严格 Clippy | `cargo +stable clippy --workspace --all-targets --locked -- -D warnings` | 通过 |
| fmt | `cargo +stable fmt --all -- --check` | 通过 |
| 围栏 | `cargo +stable test -p anchor-runner-host --locked --test run_access_fence` | 1 / 0 / 0 |
| 标准 Goose 套件 | `anchor-devtools regression goose-fixture`（见 [确定性回归](../../../docs/runtime-contract-tests.md)） | 20 套，107 / 0 / 0，没有真实模型请求 |
| 完整固定 Goose | `cargo +stable test -p anchor-runner-host --locked --no-fail-fast -- --ignored --test-threads=1`，环境同 M4 的 `goose_full` | **41 个目标，123 / 0 / 0**，和 M4 一致 |
| Web | 未重跑 | API 投影没有变化：Point 不对外暴露，Principal 的新字段可选，并且为空时不序列化 |
| 措辞检查 | `scripts/check-sensitive-wording.sh` | 通过 |

Goose 二进制的 SHA-256 是 `17e065a7…`，入口会校验。

## 未覆盖的边界

- Host 还没有填写配置摘要和因果引用，现在一律为空，留到 P2。
- `reads.rs` 的只读磁盘路径仍然用 `FileStore::new`，它会创建目录和锁文件。改为 `HistoryReader` 不属于 P1 的范围，作为后续整理项登记。
- 时间不保证单调。系统时钟回拨时，`at` 可能倒退，顺序以 `seq` 为准。
- 没有做并发读写压力测试；只验证了"写到一半的行"这种情况。

## 请求独立审查

P1 改动了内核。按宪法"提议和裁决分离"的原则，应由另一个 Agent 或用户审查，通过后以本提交作为新的冻结基点（C1）。建议审查时：

1. 读 `git show <本提交> -- rust/anchor-kernel/src`，确认新字段没有出现在任何判断条件里：
   `grep -n "\.at\b\|config\|cause\|created" rust/anchor-kernel/src/kernel.rs`
   每一处都应该是写入，而不是条件判断。
2. 复跑 `cargo +stable test -p anchor-kernel --locked --offline`，确认 `recorded_facts` 的 9 项都通过。
3. 判断"配置随 Principal 走"和"创建事实作为资源上的字段"这两个选择是否可以接受。理由见总体架构 5.1。
4. 如果审查要求修改，把问题记在这份文件末尾；修改后重新冻结。

## 独立审查（2026-10-20，审查 a9366c5）

结论：**暂不冻结为 C1**。裁决零改动这一点成立；但第 1 条问题会影响 `cause` 的含义，冻结前需要先处理。

已核对：

- 请求 1：`kernel.rs` 中 `at`、`config`、`cause`、`created` 的每一处都是写入，没有出现在任何判断条件里。Host 的 `RunAuthority` 和 `GraphValidator` 只读取 `principal.id` 和 `kind`，也不读新字段。
- 请求 2：`anchor-kernel` 共 53 项通过，其中 `recorded_facts` 9 项；本地复跑结果一致。
- 请求 3：两个设计选择都可以接受，附带的条件见下面第 6 条。

### 问题

1. **（必须处理）`seq` 在内核重启后从 1 重新计数。** `writer: Mutex::new(0)`（`kernel.rs:81`），而且启动时不会从存储里恢复。我在仓库外写了一个测试，在同一个 FileStore 上重启内核，`run/1` 的历史 seq 是 `[1, 1]`。后果有三点：
   - `Cause { resource, seq }` 在跨重启的历史里会有歧义；
   - 本文"顺序以 `seq` 为准"这句话跨重启不成立；
   - 根据代码推断（没有实际运行验证）：`effect/{target}/{seq}` 和 `pending/{target}/{seq}` 重启后可能撞名。`store.create` 返回 `Exists` 时，目标记录已经提交了。生产 Host 目前不用 Execute 和 Pending，所以还没有触发。
   - 建议：`Kernel::new` 时从存储恢复 seq 的最大值，或者把计数器持久化。记录格式不需要改。
2. **（应当处理）`with_clock` 在 `new` 之后才生效。** 所以三个宪法 Validator 资源，以及在 `with_clock` 之前 `install` 的资源，`created.at` 总是取自 SystemClock。实测：注入固定时钟 42 后，`validator/constitution` 的 `created.at` 仍然是真实的墙钟时间。`recorded_facts.rs:46-49` 的测试辅助函数本身就是这个顺序。建议让时钟能在构造时传入。
3. **（小）`HistoryReader::load` 和删除并发时会违反文档。** 如果它读到 `resource.json` 之后，资源目录被改名删除，接着读历史会得到 NotFound，被当成空历史处理，结果返回 `Some(resource)`，而且 `spent = 0`。文档承诺的是"被删除的资源读出来就是不存在"。建议在组装完成后再检查一次 `resource.json` 是否还存在，不存在就返回 `None`。
4. **（小）文件名超长时，`HistoryReader::load` 和 `FileStore::load` 行为不一致。** 资源 id 编码后超过 NAME_MAX 时，`fs::read` 返回 InvalidFilename，`HistoryReader::load` 把它当错误返回；`FileStore::load` 用的是 `exists()`，会返回 `None`。这条根据代码推断。
5. **（小）人工批准记录的 `cause` 指向一条稍后才写入的记录。** `resolve_pending` 先提交目标记录，之后 `submit` 才提交 pending 资源上的决定记录。如果在两次提交之间崩溃，`cause` 就悬空了。建议在 Cause 的文档里写明：引用可能悬空，读取方必须容忍。
6. **（设计附带条件）`config` 放在 Principal 上之后，`Principal` 的 `PartialEq/Eq` 也会比较 config。** 以后如果有 Authority 或 Validator 用整个 Principal 做比较或者当作键，config 就会悄悄变成身份的一部分，进而影响裁决。建议在 `Principal.config` 的文档里写明：授权只能按 `id`、`kind` 判断。
7. **（测试缺口）** `verdicts_do_not_depend_on_time_cause_or_configuration` 的两次运行用的是同一组 `cause("x", n)`，因此"不同因果引用"实际上没有变化。另外，这个差分测试没有覆盖 Pending/批准路径和 Execute/effect 路径。建议让两次运行的 cause 不同，并补上这两条路径。

第 1 条和第 2 条修改后，请重跑内核测试和 workspace 门槛，再申请冻结。

## 对第一轮审查的处理（2026-10-20）

| # | 处理 | 验证 |
| --- | --- | --- |
| 1 | 构造内核时，从存储的所有历史里恢复最大的 `seq`，作为写入计数器的起点（`kernel.rs` 的 `last_seq`）。记录格式不变。启动代价是线性读一遍历史 | 新测试 `seq_continues_across_a_restart_so_names_and_causes_stay_unique`：在同一个 FileStore 上重启内核，`run/1` 的 seq 是 `[1, 3, 4]`；重启前后两次 Execute 生成的 effect 资源名不同，共 2 个，没有 `Exists` 错误 |
| 2 | 去掉 `with_clock` 构造器方法，改为构造函数 `Kernel::with_clock(store, authority, clock)`；`Kernel::new` 等价于传入 `SystemClock`。测试辅助函数同步改为构造时注入 | 新测试 `the_clock_given_at_construction_stamps_the_constitutional_validators_too`：注入从 42 开始的时钟，三个宪法 Validator 和测试用 Validator 的创建时间依次是 42 到 45 |
| 3 | `HistoryReader::load` 组装完成后再检查一次 `resource.json`，不存在就返回 `None` | 这个时间窗口没法在测试里确定性地复现，属于代码层面的修复；删除后读取的路径由已有测试覆盖 |
| 4 | 读取侧把 `NotFound` 和 `InvalidFilename` 都当作"不存在"，和 `FileStore` 用 `exists()` 判断的结果一致 | 新测试 `an_id_too_long_for_the_filesystem_reads_as_absent_in_both_views`：编码后约 600 字节的 id，两个读取接口都返回"不存在" |
| 5 | 在 `Cause` 的文档里写明：`seq` 在同一个数据根下跨重启唯一；引用可能悬空，读取方必须能容忍 | 文档 |
| 6 | 在 `Authority` trait 和 `Principal.config` 的文档里写明：只能按 `id` 和 `kind` 判断，不能拿整个 `Principal` 做比较或者当作键 | 文档；复核了 Host 的 `RunAuthority` 和 `GraphValidator`，也只按 `id` 和 `kind` 判断 |
| 7 | 差分测试的两次运行，时钟、配置、因果引用都不同，并且覆盖了普通工作、Execute 和 effect 生命周期、Pending 和人工批准、过期提交、预算耗尽 | 8 个裁决和所有资源（去掉创建戳之后）完全相同；裁决原因依次是 `Ok ×4, AwaitingHuman, Ok, StaleVersion, BudgetExhausted` |

### 重跑的门槛

原始日志在 `/tmp/anchor-p1b/`。

| 门槛 | 结果 |
| --- | --- |
| 内核 | `recorded_facts` 12 项，内核测试全部通过 |
| workspace | 133 个目标，**2,212 / 0 / 126**（比第一轮多 3 项，正是新测试） |
| 严格 Clippy、fmt、围栏 | 通过；围栏 1 / 0 / 0 |
| 完整固定 Goose | **41 个目标，123 / 0 / 0** |
| 标准 Goose 套件 | 第一次在 `goose_completion` 有 1 项失败，原因是测试 Host 启动时端口被占用（`AddrInUse`），属于环境冲突；同一项在随后的完整 Goose 里通过。单独重跑整套：**20 套，107 / 0 / 0**。两次的证据都保留在 `/tmp/anchor-p1b/` |

### 对已有开发数据的影响

8077 现在运行的部署版本还是旧内核，不受影响。以后部署新内核时，旧开发数据里重启前留下的重复 seq 会保留在历史里不变，新的提交从最大值往后编号。旧记录本来就没有 `cause`，所以不会产生有歧义的引用。

请复审第 1、2 条，确认后冻结。

## 复审（2026-10-20，审查 aa90da4）

结论：**第 1、2 条修复正确，可以把 aa90da4 冻结为 C1**，前提是接受下面两条记录在案的边界。这两条都是第 1 条的修复带来的，不涉及裁决逻辑。

已核对：

- **第 1 条**：`last_seq` 遍历 `list("")` 下所有资源的历史，effect 和 pending 资源自己的历史也包括在内，取其中最大的 seq 作为计数器起点。没有写入记录的 seq（目标资源不存在、被拒绝）不出现在任何持久化的名字里，重复使用不会造成冲突。新增的重启测试覆盖了 effect 资源自己的历史（seq 2），然后重启。
- **第 2 条**：`Kernel::with_clock` 改成了构造函数，`new` 委托给它；`install` 用的是注入的时钟。生产代码只在 `kernel_runs.rs:94` 使用 `Kernel::new`，行为不变。
- 本地复跑内核测试，全部通过，其中 `recorded_facts` 12 项。第 3 到第 7 条的修改也看过，没有问题。

### 由第 1 条带来的两个边界（在仓库外实测）

1. **任何一条历史损坏，内核都无法启动。** 我在一个资源的历史末尾追加 `{broken}\n` 之后重启，`Kernel::new` 返回 `Store(Corrupt(...))`。
   - 改动前，损坏只影响那一个 Run；而 Host 的探针重建（`kernel_probe.rs:22`）是有意设计成失败不影响启动的。现在整个 Host 都起不来。
   - 这是"不能确定最大 seq 就拒绝写入"的失败即关闭，和"未观察的就是未知，并阻断下一步"的精神一致。写入方会截断不完整的末尾，所以损坏只可能来自外部篡改或磁盘故障。
   - **建议接受**，作为已知边界登记在这里；以后改动内核时补一个测试，固定这个行为。如果不接受，就需要持久化一个 seq 高水位，这又是一次内核改动。
2. **删除资源后重启，seq 可能和已删除的记录重号。** 实测：`run/b` 持有 seq 2，删除它之后重启，`run/a` 的下一条记录也是 seq 2。
   - 因为两个资源的 id 不同，`(resource, seq)` 形式的引用不会混淆；只有单独拿 seq 当全局键的读取方才会受影响。
   - 所以 `Cause` 文档里"seq 在同一个数据根下跨重启唯一"写得过强，准确的说法是"在仍然保存着的记录中唯一"。
   - 建议冻结时以本文为准，下次改动内核时顺带修正这条注释；P2 的中枢不要把 seq 单独当作全局键。

另外，启动时会完整反序列化所有历史。Host 的探针重建已经对 `run/` 做过一遍同样的读取，所以启动代价大约增加一倍，目前可以接受。

## 冻结（2026-10-20）

接受复审的结论和两个已知边界，同时做了两处只涉及注释和测试的调整，不改变任何代码行为：

- 改正 `Kernel.writer` 和 `Cause` 的注释：`seq` 在同一个资源内递增；保留的记录里，"资源 id + seq"不会重复；被删除资源用过的编号不会被记住，所以 `seq` 单独不是跨资源的键。原来"跨重启唯一"的说法过强。
- 两个边界都补了测试来固定：
  - `an_unreadable_history_stops_the_kernel_from_starting_and_names_the_file`：历史末尾加一行坏数据后，`Kernel::new` 返回 `Corrupt`，报错信息里带 `history.jsonl: line 2`；
  - `after_a_removal_seq_still_increases_within_each_retained_resource`：删除 `run/b` 再重启，`run/a` 的 seq 是 `[1, 2]`。
- 内核测试全部通过，`recorded_facts` 14 项；`anchor-kernel` 严格 Clippy 和 fmt 通过。只改了注释和测试，workspace 和 Goose 沿用 `aa90da4` 的结果。
- 架构文档 5.1 写入了两个边界；第 6 节加上了数据中枢的键规则："数据根 + 资源 id + seq"，跨资源按时间排序，悬空的因果引用保留并标记。

**新的冻结基点 C1 就是包含本节的这个提交。** 从此以后，`git diff <C1> -- rust/anchor-kernel` 必须为空，除非另有经过审查的内核变更。

## 按第一性原理简化（2026-10-20，冻结 `e172e05` 之后）

用户追问"为什么需要序号"。逐条核对之后的结论是：**记录不需要编号，因果引用也不该放进内核。** 所以在 `e172e05` 的基础上做了减法，代码净删 116 行。

### 理由

- **全局 `seq`**：原内核就有这个字段，是跨资源的全局计数器。生产代码里没有任何地方读取它，内核里只用来排列先后，以及给内核生成的资源起名。
  - 历史按资源分区、只追加，同一资源内的先后就是记录在历史里的位置；跨资源的先后看 `Stamp.at`。
  - 给全局计数器加上"跨重启唯一"的要求，就是在分区存储上硬造一个不存在的全局顺序。复审提的两个边界，以及启动开销翻倍，都源于这一点。
- **`Submission.cause`**：内核"原样记录，不校验，不解读"的字段，按定义就是内容。因果改为用天然标识写进 `content`，例如 Goose 的 `messageId`、`toolCallId`；人工批准记录本来就带有 `{"approved": pending.id}`。

### 改动

- 删除 `Stamp.seq`、`Kernel` 的写入计数器（写锁保留，改为 `Mutex<()>`）、`last_seq` 启动扫描，以及 `Submission.cause` 和 `Cause`。旧记录里的 `seq`、`knobs` 字段读取时被忽略。
- 内核生成的资源改为 `effect/{target}/{位置}`、`pending/{target}/{位置}`，位置是"这条记录在目标资源历史里是第几条"（`Kernel::place`）。只有 Execute 和 Pending 路径需要多读一次目标资源的历史长度。
- 保留：时间（构造时注入的 Clock）、`Principal.config`、`Resource.created`、Point 的 `at` 和 `config`、`HistoryReader`，以及审查第 3、4、6 条的修复。

### 和审查问题的对应

| 问题 | 现状 |
| --- | --- |
| 第一轮 1：重启后 `seq` 重新计数 | 没有序号了。原内核"重启后 effect、pending 撞名"的潜在缺陷也由按位置命名一并修复 |
| 第一轮 2、3、4 | 已修，保留 |
| 第一轮 5：因果引用可能悬空 | 内核里没有因果引用了。规则搬到数据中枢：内容里的标识可能悬空，中枢保留并标记 |
| 第一轮 6：`Principal` 的相等比较包含 config | 保留文档规则：授权只能按 `id`、`kind` 判断 |
| 第一轮 7：差分测试 | 保留：两次运行的时钟和配置不同，覆盖 Execute 和 effect 生命周期、Pending 和人工批准、过期提交、预算耗尽 |
| 复审 1：一条历史坏了整个 Host 起不来 | 消失：没有启动扫描了，损坏只影响它自己的资源（新测试 `an_unreadable_history_affects_only_its_own_resource`） |
| 复审 2：删除后编号重复 | 消失：没有全局编号 |
| 启动开销翻倍 | 消失 |

### 新的已知边界（请审查方判断）

- 一个资源被删除后，如果用同一个 id 重新创建，它的历史会从第 1 条重新算；如果它旧的 `effect/…`、`pending/…` 资源没有跟着删除，就会撞名。在这种情况下，记录已经提交，创建 effect 资源时会返回 `Exists`。
  - 现在不会触发：生产 Host 不走 Execute 和 Pending 路径，Run id 也带时间戳，不会重复使用。
  - 建议等 Host 真正用上这两条路径时，在删除 Run 的同时删除它的 effect 和 pending 资源。

### 测试与门槛

- `recorded_facts` 12 项：删除了依赖 seq 和 cause 的测试；新增了"按位置命名，重启后仍然唯一"（实际依次生成 `effect/…/1`、`pending/…/2`、`effect/…/3`），以及"一条历史损坏只影响它自己的资源"。
- 按任务书新的测试分级规则，执行一次切片门槛，用时 31 分钟，原始日志在 `/tmp/anchor-p1c/`：

| 门槛 | 结果 |
| --- | --- |
| `anchor-kernel` | 全部通过 |
| workspace | 133 个目标，**2,212 / 0 / 126** |
| 严格 Clippy、fmt、围栏 | 通过；围栏 1 / 0 / 0 |
| 完整固定 Goose | **41 个目标，123 / 0 / 0** |

标准 Goose 套件完全包含在完整回归里，按新规则不再另跑。

请审查方审查这一版。通过后，以这一版的提交作为新的 C1，替换 `e172e05`。

## 审查"按第一性原理简化"（2026-10-20，审查 9a5fa57）

结论：**通过，可以用 9a5fa57 替换 `e172e05` 作为 C1。**

### 1. 按位置命名：成立

- `place` 的值是目标资源的历史长度加 1，在持有写锁时计算。从计算到 `submit` 提交目标记录之间，目标历史不会再增加任何记录。
  - 进入 `resolve_pending` 的 HumanDecision 路径会提前返回，不会调用 `place`；
  - `submit` 对已存在的目标一定会提交。
  
  所以名字就是这条记录实际所在的位置。
- 同一个目标下，名字唯一：历史只追加，`trim_incomplete_tail` 只截掉从来没成为记录的末尾，重启后历史长度不变。
- 不同目标之间也不会撞名：后缀固定是 12 位数字，`effect/T/N` 只能对应同一个 T。
- 旧数据：本机找不到任何 `effect%2F*`、`pending%2F*` 目录，生产 Host 也不走这两条路径，所以新的 12 位位置编号不会和旧的 seq 编号撞名。

### 2. 删除 seq 和 cause 不影响裁决：确认

- 内核里 seq 只有两个用途：给 effect、pending 资源起名，以及写进 Stamp。
  - 裁决只通过 `has_unknown_effect` 用到 effect 的名字，而它只看前缀 `effect/{endpoint}/`，不看后缀，所以命名方式变化不影响结果。
  - pending 资源的名字不参与任何判断。
- cause 从来没有被读取过。
- 全工作区 `cargo check --all-targets` 通过，说明内核之外没有任何地方读取 `seq()` 或 `cause`。仓库里其余的 `"seq"` 都是 Session 和事件流自己的字段，和内核无关。
- 旧记录里的 `seq`、`cause`、`knobs` 字段读取时被忽略，没有任何类型设置 `deny_unknown_fields`。
- 本地复跑内核测试，全部通过，其中 `recorded_facts` 12 项。

### 3. 新边界：可以接受，但需要把表述写全

- 影响不止撞名。同一个 id 重建之后，旧的 `effect/{id}/…` 只要还在，就会被 `has_unknown_effect` 按前缀算到新资源头上：旧 effect 是 `unknown` 状态时，新资源的 Execute 会被拒绝，原因是 `EffectUnknown`。也就是说，残留的 effect 会影响裁决。这个问题在按 seq 命名时就已经存在，不是这次引入的。
- 现在确实走不到，但原因应该写成"生产 Host 不创建 effect 和 pending 资源"。"Run id 带时间戳"并不完全准确：`admission.rs:52` 用的是 `rust-{纳秒}`，但还有 `turn-{turn_id}` 形式的 id。
- 建议登记的规则：**删除资源时，必须连同它的 `effect/{id}/` 和 `pending/{id}/` 一起删除；或者不允许复用资源 id。** 等 Host 真正用上这两条路径时实现。
- 另一个已有问题，不在本次范围内：前缀匹配会把嵌套 id 的 effect 也算进来。例如目标 `run/1/x` 的 `unknown` effect，也会阻断 `run/1` 的 Execute。也等 Host 用上 Execute 时一并处理。

## 冻结为 C1（2026-10-20）

接受审查结论：**`9a5fa57` 替换 `e172e05`，成为新的冻结基点 C1。** 从此 `git diff 9a5fa57 -- rust/anchor-kernel` 必须为空，除非另有经过审查的内核变更。

已知边界按审查意见补全，登记在 [总体架构 5.1](../../../docs/harness-data-hub.md#当前职责与依赖) 的"已知边界"中：

1. **同一个 id 删除后重建**：如果它旧的 effect 资源还在，后果不只是新 effect 撞名（记录已经提交、创建时返回 `Exists`）。旧 effect 处在 `unknown` 状态时，按前缀查找未知 effect 的判断会把它算到新资源头上，新资源的 Execute 会被拒绝，这会影响裁决。这个问题按 seq 命名时就已存在，不是本次引入的。
   - 现在走不到的原因：**生产 Host 不创建 effect 和 pending 资源**。原文"Run id 带时间戳"不准确，Run id 还有 `turn-{turn_id}` 这样的形式。
   - 规则：Host 用上这两条路径之前，必须二选一：删除资源时连同它的 effect、pending 资源一起删除，或者禁止复用资源 id。
2. **嵌套 id 的前缀匹配（已有问题，不在本次范围）**：`effect/run/1/` 这个前缀也会匹配到 `run/1/x` 的 effect，所以 `run/1/x` 下的未知 effect 会阻断 `run/1` 的 Execute。等 Host 用上 Execute 时一起处理。
