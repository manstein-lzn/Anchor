# 全量迁移任务书：Host 全部 Run 走内核，删除旧 Runner

> 历史归档：保留当时的任务要求、结论与证据，不作为当前待办；现行入口见[文档目录](../../README.md)。

编制：2026-10-19。决定：用户选择全量迁移（见工作记录"R2 审查结论"）。目标是**统一定律、崩溃与计费正确、删除旧 Runner**，不是让 Host 变小。起点：`d915314`（R2 停止现场）。

**当前状态（2026-10-20）**：M1、M1.5、M2、M3、M4 已验收。§10.20 M5 的测量、探针复核和收尾完成，按本任务书交回，迁移任务结束。固定基点 master `e60374f`，C2 save 0 / mutation 0，Kernel 相对 `0859373` 零 diff；本轮 workspace 2,200 / 0 / 126、fmt 与围栏通过。M4 的 Web 359 及构建、完整固定 Goose 123 / 0 / 0 沿用已验结果，本轮未重跑完整 Goose。最终数据、脚本和未验边界见 [M5 最终测量](../../../experiments/constitution-host-preflight/final/measurement.md)，后续决定见其中“迁移后待决清单”。以下 M1.5 条款为历史口径，后续覆盖以 §10.10–§10.20 为准：

1. 按 §10.2 合并 `master`，冲突逐文件登记；合并后 workspace 与 Web 全绿。
2. 按 §10.3 用 master 的 Goose 锁定版本跑全部 Goose 忽略测试，原样贴出结果。**注意**：master 目前的 Goose 基线本身就是坏的（extensions/add 返回 -32603，由 master 的 G0 任务处理，见 `/home/mansteinl/Anchor/docs/goose-baseline-task.md`）。G0 修复之前，门槛改为"相对 master 基线没有新增失败"。master 侧基线**优先使用 G0 交回的失败名单**；如果 G0 还没交回，就用临时干净 worktree 在本次合并的那个 master 提交上跑，例如 `git worktree add /tmp/master-base <提交>`，用完删除。**不得**在 `/home/mansteinl/Anchor` 上跑基线，因为那里有其他 agent 正在修改。Goose 套件不要和 G0 同时跑：G0 正在跑时，先完成合并和规则表，Goose 对照放到最后。M1 时分支锁定的是旧摘要，本机没有对应二进制，所以不能执行；合并后不再适用这个理由。
3. 补交 §10.1 的规则清单表：分别在 `1d1ba54` 与合并后的代码上统计。M1 交回时缺了这一项。
4. 从 M1.5 起执行 §10.6：原始日志不再入库。

S19–S24、S26 已由用户批准（2026-10-19）。M3 的前置条件见 §10.7：master 完成 F2，并已合入本分支。

本文取代上位计划 H3–H6 的阶段划分；上位计划的约束 C1–C6、§2.1 调度器、§2.2 执行者语义仍然有效。

当前进度（2026-10-20）：M5 已完成并交回，M1–M5 迁移任务结束；未推送、未删除旧 worktree/分支。S19–S24、S26、S27 已批准，S25 作废，S4 由 S27 结清。W4、完整真实模型 Provider 业务、外部渠道实网、部署联合验收及保留策略等留在 [迁移后待决清单](../../../experiments/constitution-host-preflight/final/measurement.md#迁移后待决清单)，不提供旧 Run 迁移工具；本轮未清理开发状态。

## 0. 工作方式（先读）

- 迁移分 5 个里程碑（M1–M5）。**每个里程碑结束时交回一次**，中途只在遇到 §8 的"必须停下"时交回。其他问题自行判断、登记后继续，审查方在里程碑结束时统一处理。
- 每个里程碑内部按需要拆多个 commit，**每个 commit 编译通过、全部测试通过**。不允许留红的中间提交，WIP 除外：WIP 只在触发"必须停下"时使用。
- 必读：`AGENTS.md`；`docs/anchor-constitution.md`；上位计划第 0、2 节；工作记录中的语义表 S1–S26，以及 P、R、R2 三节；`experiments/constitution-host-preflight/p/measurement.md` 的"未决"。
- 不要用 `git checkout` / `reset` / `stash` 丢弃改动；只 stage 自己改的路径。

**测试修改规则**（全文适用）：

- **状态词汇类断言**（旧 status 字符串、needs_attention、quiescent、旧 phase 名）：可以改为新词汇，逐条登记。
- **旧 Runner 内部机制断言**（cursor、RunStore 保存次数、`stop_requested` 轮询、旧 record 字段）：随旧代码删除时，每个被删测试必须在登记表中给出"新路径的等价测试名"，或"该行为已不存在"及理由。
- **业务行为断言**（产物、成本、路由、授权拒绝、不重放、Session 投递）：**不得放宽**。行为确实改变的，登记为 S 候选；不能用改测试的方式掩盖。

## 1. M1：完成 R2（对外状态词汇）

从 `d915314` 继续。

**审查决定，修正 R2 任务书的错误**：pause 不是 stop 的别名。

- pause 是控制动作，不是状态：Executing 且有 worker 时可用。宿主记录这个意图，调度器在**当前节点结果提交之后、下一次派发之前**提交人的 `stop`，reason 为 `paused`。
- 结果状态是 `waiting / human / reason=paused`。
- `actions` 增加 `pause`。对外状态仍然只有三种相位，词汇不变。
- 旧 Run 的 pause 维持旧 Runner 原行为。
- S25 作废；按这个定义，R2 停下来的那些 pause 测试应能原样通过。

完成 R2 任务书中未完成的项：

- 旧 Run 与内核 Run 在三种处境下的同一 HTTP 对照；
- Pilot 的 pause / stop 独立测试；
- effect 等待的 Web 控件测试；
- 六 crate 全量回归。

其余按 R2 任务书执行。S19–S24、S26 保持"待批"，审查方在 M1 交回时统一处理。

**M1 完成标准**：Rust 六 crate 与 Web 全绿；对外 API 和 Web 只出现新词汇；断言修改登记完整。

## 2. M2：内核路径能力补齐

目标：P 探针中所有返回 `Unsupported` 的能力，都在内核路径上实现。完成后，任何 Graph 都能走内核路径。旧路径仍保留，作为默认入口，同时作为对照。

逐项实现，每项单独 commit。每一项把旧路径中覆盖该能力的测试复制一份，指向内核路径：

1. **并行**：调度器支持多个开放槽位并发执行，没有波次屏障。`StaleVersion` 重提、迟到结果被拒，按 S6、S17、S18 处理。并行模块作用域扩展已由用户批准（2026-10-19）：入口、激活、成员 passes 与嵌套计数清零由纯函数 parallel_frontier 裁决；同一并行激活内只首次从外部进入时计数，后续激活重入递增；上限遵循 S17/S18。
2. **Op.call**：
   - 宿主代码提交 `CallSubmission::Bound` 后，Run 进入 `Waiting::Child`；
   - 子 Run 结算后，宿主代码提交 `resume` 和 `Completed` / `Failed` / `Uncertain`；
   - detach 模式按校验器已有语义处理；
   - 子 Run 同样走内核路径。

   谁观察子 Run 结算、谁发出释放，由你设计，但必须是宿主代码，不能是校验器。
3. **宿主操作与常驻助手**（上位计划 H4）：
   - `Yielded` / 打断，用 Code 提交带 `interruption` 的 NodeSubmission（D4、S14）；
   - 执行者冻结 `ArtifactKind::Interruption` 产物；
   - suppressed 与投递账本语义保持不变；
   - 必须让 `goose_persistent_assistant` 相关确定性测试在内核路径上通过。环境不具备时如实写"未执行"。
4. **执行结果补全**：
   - `BudgetExhausted`：Code 带 Observation 提交 stop，进入 `waiting / human / reason=budget`，人可以 resume。**不得**直接结算为失败，那是待决的 W4。
   - `Aborted`：人放弃。
   - `Interrupted` / `Suspended`：按 P 的取消路径处理。
5. **未知效果的人工决定**：现有 attempt 恢复决定 API（`record_recovery_decision`）接到内核路径。人核查后：
   - 选"已完成"：执行端口写入完成事实，然后 resume；
   - 选"放弃"：abandon。

   **不得自动重放。**
6. **删除 Run / Graph**：`deletion.rs` 对内核 Run 的支持。前置条件改为读相位与 worker。

**对照**（原 H2.2 中取消的那部分，现在值得做了）：并行组与调用组的随机生成器各取 5,000 个 seed，用同一个 fixture 端口分别交给旧 Runner 和新调度器执行：

- 比较派发多重集与最终 Graph 事实；
- 失败结局用 H2 任务书 §1 的三条不变量判定；
- 串行组已在 P 中做过 1,000 seed，扩到 10,000。

**M2 完成标准**：内核路径上没有 `Unsupported`；三组对照全绿；旧路径测试照旧全绿。

## 3. M3：默认切换到内核路径

1. 所有新 Run 的准入都走内核路径：删除 `execution=kernel_probe` 开关，删除"按存储归属分派"的双路径逻辑。
2. 磁盘上的旧 RunStore 记录在分支上不读取：按 C6，读到时具名拒绝，并提示需要离线迁移。生产数据迁移不在本里程碑。
3. **测试夹具迁移**：现有测试中用 `store.save` 直接构造旧 Run 的夹具，改为通过内核创建，必要时用 Code 提交把 Run 推进到所需相位。不得为了构造夹具而直接写内核存储文件。
4. 所有控制入口（control、abandon、recovery、session_calls、assistants、dispatch、admission、wecom 等）改为通过内核提交与读取相位。
   - 每个原先手写的前置条件：能由内核定律推出的，删除；属于 Session 层的，保留在 Session 层。
   - 每个文件写清删了哪些判断、保留哪些及理由。
   - S4（退役之后能否 abandon）按内核定律处理：未结算就可以 abandon，登记为 S 候选。
5. **真实 Goose 确定性回归**按 `docs/runtime-contract-tests.md` 执行一次。环境不具备时写"未执行"，这一项会成为合并前的门槛。

**M3 完成标准**：生产代码中没有任何路径构造 `GraphRunner`；全部测试全绿；C2 棘轮（H1 基线 16 处 save、36 处 mutation）降为 0，逐项列出每处的去向。

## 4. M4：删除旧实现

删除 `runner.rs`、`parallel.rs`、`RunStore` / `FileRunStore`、`RunControl` 的轮询接口、`RunStatus`、`RunCursor`、`CompletionFact` 中只为旧 Runner 存在的部分、R/R2 的临时适配与 `run_json` 兼容桥，以及其他只为它们存在的类型（以实际可删为准）。

- `GraphRunRecord` 中不再由它拥有的字段一并删除，例如 `cursor`、`status`、`error`、`recovery`、`recovery_submissions`。
- 差分测试失去了对照组：先用 M2 的对照生成 20,000 例随机 Graph 的派发轨迹与最终事实快照，提交为回归基线；然后差分测试改为对快照比对。
- 每个被删测试按 §0 的规则登记。

**M4 完成标准**：`grep -rn "GraphRunner\|RunStatus\|FileRunStore\|stop_requested\|pause_requested" rust --include=*.rs` 为零；全部测试全绿。

## 5. M5：测量与收尾

1. 接入测量探针（上位计划 H6）：Host 给内核注册 `Probe`，把每个测量点写入只读 JSONL 投影，不新增对外 API。
2. 写 `experiments/constitution-host-preflight/final/measurement.md`：
   - 按 crate 列出删除与新增行数（相对 `fc558e2`）；
   - 每条生命周期规则的独立判断点，迁移前后对比；
   - 直接写 Run 的点：前后对比；
   - 测试数量变化及登记表汇总；
   - 仍然手写的判断，按 Session 层、授权、执行端口归类。
3. 更新工作记录、上位计划状态行，以及合并前需要的事项清单：生产数据离线迁移、W4、真实 Provider 验收。

## 6. 约束

- C1：`anchor-kernel` 不改生命周期或业务规则。用户已明确批准唯一存储例外：FileStore::remove、删除恢复与单资源互斥；Store trait 不变，MemoryStore 不增加删除。其余内核改动仍属于"必须停下"。
- C2：Run 的状态只经 `Kernel::submit` 改变；M3 结束时验收为零。
- C3：校验器纯函数。
- C4：零停止 / 暂停轮询。pause 是宿主意图，由调度器在边界提交 stop，**不是**在执行循环中轮询标志。
- C5：身份与成本由宿主盖章，Agent 不得提交 Settle、fanout、join、interruption。
- 宪法第二条：未知效果不自动重放。
- 单一权威：任一时刻每个 Run 只在一个存储里。

## 7. 验收命令（每个里程碑交回前跑）

```bash
cd /home/mansteinl/Anchor/rust
cargo build -p anchor-sandbox-bwrap --bins
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
git -C .. diff 0859373 --stat -- rust/anchor-kernel    # M4 要求零 diff，已批准的存储切片包含于该基点
cd ../apps/web && npm test && npm run build
```

M1 起使用 `--workspace`，替代之前的六 crate 清单。

## 8. 两级停止条件

**必须停下交回：**

- 需要改 `anchor-kernel`，包括新增 Reason、Waiter、Phase 变体，或给 Stamp、Store 加接口。
- 需要改校验器的裁决规则；新增 Code 提交种类除外，需要的话也要先登记理由。
- 业务行为断言只能放宽才能通过（见 §0）。
- 违反宪法：结算可逆、自动重放未知效果、从提交内容读身份或成本、双权威。
- 某项能力在内核路径上做不到，且不属于 Session 层可以承担的事。

**自行判断、登记后继续：**

- 状态词汇和旧 Runner 内部机制断言的修改。
- 旧行为向内核定律对齐造成的差异：登记为 S 候选，附现场。
- 并发调度中由提交顺序决定的差异，前提是满足 H2 任务书 §1 的不变量。
- 接口形状、文件组织、Web 文案。

登记位置：工作记录中每个里程碑一节，下设"S 候选"、"断言修改"、"删除测试映射"三张表。

## 9. 里程碑交回模板

```
里程碑：M<n>
commit 列表：<hash 标题>（每个一行）
测试：workspace <通过/失败/忽略>；Web test <…>；Web build <…>；与上次交回差异 <…>
对照（M2）：serial/parallel/calls 各 <seed 数，差异>
C2 棘轮（M3 起）：save <n>，mutation <n>
删除行数 / 新增行数（M4 起）：<…>
S 候选：<条数与一句话列表>
断言修改：<条数>；删除测试：<条数>，均已映射
真实 Goose：<执行并通过 / 未执行及原因>
clippy / fmt：<…>
C1：<…>
停止条件触发：<无 / 哪条及现场>
需要审查方决定的问题：<…>
```

## 10. 审查方补充（2026-10-19；M1 交回时开始生效）

本节依据 P、R、R2 的数据修正本任务书的验收口径与流程，**与前文冲突时以本节为准**。M1 进行中的工作不需要回头改，交回时按 10.1 补齐即可。

### 10.1 验收口径：按宪法，不按行数

- 每个里程碑验收时，逐条回答 `docs/anchor-constitution.md` 中"完美"的三个条件：
  - 新增或改动的组件能否从宪法推出，或者属于可整块替换的边缘；
  - 核心（`anchor-kernel`）是否需要改动；
  - 核心里有没有可以删的东西。

  同时过一遍第 7 节的四个极限。行数只作辅助信号：新增显著多于删除时说明原因，不作为通过与否的条件。
- **度量口径改为宪法第 10 节的"同一规则的独立判断点"**，不再统计消费者函数包络。R、R2 统计的是包络，把 Session、授权、投递规则和 Run 规则混在一起，这和宪法的标准不是一回事。
- 每个里程碑交回时，附一张"规则清单"表，至少覆盖以下规则：
  - 是否终态
  - 可否 stop、resume、abandon、pause
  - 是否静止
  - 是否需要关注
  - 迟到结果如何处理
  - 未知效果如何处理
  - 预算耗尽如何处理
  - 子 Run 结算后如何释放父 Run

  每条规则列出：Host 中独立决定它的位置（`文件:行`），改前与改后各几处。凡是"读同一个视图或派生函数得出结论"的，不算独立判断点。
- **M1 交回时补测**：用同一张规则表，对 `1d1ba54`（R 之前）与 M1 完成后的代码各测一次。这一项替代 R2 的包络结论，作为读取侧是否统一的证据。

### 10.2 与 master 同步

- M1 交回后、M2 开始前，把 `master` 合并进 `constitution-core`，用 merge，不用 rebase。冲突解决逐文件登记在工作记录中。
- 之后每个里程碑结束时都再同步一次。同步后全量测试必须全绿，才算这个里程碑交回。

### 10.3 Goose 门槛

- 合并 master 之后，分支使用 master 的 Goose 锁定版本（`17e065…`）。本机 `/var/tmp/anchor-goose-build/artifacts/goose-acp.first` 与它一致；使用前用 `sha256sum` 核对。
- 从 M1 交回开始，每个里程碑都要跑依赖 Goose 的忽略测试，例如：
  ```bash
  ANCHOR_GOOSE_BINARY=/var/tmp/anchor-goose-build/artifacts/goose-acp.first \
    cargo test -p anchor-runner-host -- --ignored
  ```
  原样贴出各 target 的结果汇总。失败的必须定性：是 master 上原有的问题，还是迁移引入的。
- "未执行"只在二进制确实不可得时允许，并写明你查找过的位置。只查 PATH 不算查过。

### 10.4 测量探针提前到 M2

- 原 M5 的第 1 项（Host 为内核注册 `Probe`，测量点写入只读 JSONL 投影）移到 M2，最早在并行与 Op.call 落地之后接入。
- M2 交回时附一段真实运行的测量点样本，以及一个简短统计：按校验器、身份、动作、裁决、原因码汇总。M2 结束时由用户决定是否继续 M3。

### 10.5 Session 层"一个事实一个写入者"检验（M3）

M3 迁移控制入口时，列出 Session 层所有"读取 Run 状态来做业务决定"的位置，例如"Run 结束了，就投递回复"。逐条说明它在迁移后属于哪一种：

- 读取 Run 的结算事实（允许）；
- 应当成为 Session 自己拥有的事实；
- 两个写入者在写同一个事实（需要修正）。

最后一种按"必须停下"之外的规则登记并给出修正方案。只有修正需要改内核时，才停下交回。

### 10.6 证据规范

从 M1 交回起，不再提交原始 `.log` 文件。只提交摘要（`verification.md`、`measurement.md`）和机器可读的 `checks.json` / `counts.json`。原始输出放在 `.local/` 或 `/tmp` 下，路径写进摘要。已提交的 64 个原始日志暂不处理，合并到 master 之前再决定是否清理。

### 10.7 与 master 门面配合（2026-10-19，用户批准）

master 将按 `/home/mansteinl/Anchor/docs/run-access-facade-task.md` 建立 Run 访问门面，分三步：F0 围栏、F1 读取门面、F2 写入门面。业务代码只通过门面读写 Run，master 上行为零变化。本分支据此调整：

- **门面是契约。** M1 交回后合并 master，把门面带进来。之后本分支实现**同一套门面接口**，背后换成内核：
  - R2 的 `RunView` 与新对外词汇，改为门面在内核下的实现和序列化；
  - P 的 `kernel_runs` 并入门面实现；
  - 门面的每个规则谓词，在内核下按宪法定律给出新语义。与 master 语义不同的，逐条登记为 S 候选。
- **围栏随合并生效。** 本分支同样必须让 `run_access_fence` 保持全绿。分支新增的内核访问代码只能放在门面模块或运行时侧。
- **提前删除旧类型，"切换即删除"。** M3 只有在 master 的 F2 合并进本分支之后才能开始。M3 在同一个里程碑内完成两件事：
  1. 默认切换到内核；
  2. 删除门面的旧实现，以及 `GraphRunner`、`parallel.rs`、`RunStore`/`FileRunStore`、`RunStatus`、`RunCursor`、`RunControl` 中的轮询接口。

  此后 master 上若再有新代码直接使用这些类型，合并时会**编译失败**，冲突一律显式暴露。原 M3 中"所有控制入口改为内核提交"一项，因为业务代码已经只调用门面，范围缩小为门面实现本身。
- **M4 缩小为收尾**：删除 `GraphRunRecord` 中不再拥有的字段、`CompletionFact` 中只服务旧 Runner 的部分；差分测试改为对快照比对（做法同第 4 节）。
- 如果 F2 晚于本分支的 M2 完成，M2 结束后先做 M5 中的测量部分和 10.5 的检验准备，**不得**在没有门面的情况下提前开始 M3。

### 10.8 合并顺序（2026-10-19 补充）

master 的读取门面正在分片推进：F0 是 `e3a5778`，F1 的第一个切片是 `06f7b8a`，F1 还没做完。现在两边各有一套读模型，分别是 master 的 `application/run_access` 和本分支的 `run_view.rs` 等文件。为避免同一批读侧文件反复冲突，按下面的顺序合并：

1. 当前对 `2b3b7a2` 的合并照常完成并提交。
2. 接着**只合并 `e3a5778`（F0）**。合并后围栏会因为本分支新增的文件失败。处理方法：把 `application/kernel_runs.rs`、`kernel_runs/`、`run_view.rs`、`legacy_run_view.rs`、`run_json.rs`、`run_observations.rs` 移到 `application/run_access/` 下面。围栏已经把这个目录认定为门面，移过去就能通过。**不得**扩大 `runtime_file` 的豁免范围，也不得调高清单计数。这样 M2 期间新写的内核访问代码会受到围栏约束。
3. **master 的 F1 完成之前，不合并 `06f7b8a` 及之后的提交。** F1 完成后一次性对齐：
   - 以 master 的 `run_access` 接口为准，包括谓词名称和构造方式，本分支的内核实现放在这套接口后面；
   - 谓词在内核下的语义，按已批准的 S19–S24、S26 执行；
   - 预计 `application/run_access.rs` 会出现 add/add 冲突，合并时以 master 的接口为准。
4. 步骤 2 完成后即可开始 M2，不需要等 F1。

### 10.9 Goose 基线（G0 已交回并验收，2026-10-19）

- **根因**：Goose 测试的 fixture 没有配置 relay。任务联网用 slirp 隔离网络命名空间，访问不到宿主 loopback 上的 MCP 控制面，于是 ACP 在 `extensions/add` 处返回 `-32603`。生产与开发启动路径（`scripts/dev.sh`）本来就配置了 relay，不受影响。Goose 二进制不需要更换。修复提交为 `250435d`（分支 `fix/goose-baseline`），由 master agent 择机合入 master。
- **标准命令**：以 `docs/runtime-contract-tests.md` 为准（G0 合入 master 之后），关键是必须设置 `ANCHOR_GOOSE_LOCAL_NETWORK=1`，并让 `ANCHOR_GOOSE_RELAY_BINARY` 指向**同一次构建**产出的 `anchor-net-relay`。不设置这两项跑出来的 Goose 结果一律作废。
- **master 基线**（基点 `e3a5778`）：完整忽略测试 116 通过 / 3 失败 / 0 忽略。三项残留失败：
  1. `goose_conversation::next_conversation_invocation_changes_model_and_endpoint_without_losing_history`
  2. `goose_distribution::extracted_goose_runtime_resumes_without_sources_or_replaying_workspace_effects`
  3. `goose_pilot_authoring::candidate_declaration_cannot_enable_network_or_credentials_and_command_cancel_can_continue`
- **本分支的门槛**：在合并后的分支上用标准命令跑一次，失败名单不得超出上述三项。超出的每一项必须单独复跑确认，再定性为迁移引入还是环境原因。**不再需要**在 master-base worktree 上另跑 Goose 基线；已经跑过并且配置了 relay 的结果可以保留作为对照，没有配置 relay 的结果作废。

### 10.10 M1.5 审查结论与 M2 开工（2026-10-19）

- **M1.5 通过。** 审查方核对了以下几项：
  - 围栏测试与 master F0 逐字节一致，未扩大豁免，上限从 224 降到 192；
  - 内核零 diff；
  - 定向重跑全部通过：围栏、run_access / JSON 字节 / kernel_runs 17 项、Kernel 37 项。

  全量 workspace 和 Goose 这次没有重跑，因为与 master 的 F1 构建同时进行；M2 交回时一并重跑。
- **Goose 门槛已经起作用**：第一次带 relay 跑全量时，发现了 5 个 M1 词汇迁移遗漏，其中助手回执 2 个、Session Call 轮询 3 个，均已修复。这 5 个问题的测试此前从未在本分支上执行过。
- G0 的原始证据现在位于 `/home/mansteinl/Anchor/.local/goose-baseline-20261019/`。
- `goose_distribution` 是否通过，取决于构建目录里的沙箱二进制是不是硬链接，属于随环境变化的 fixture 缺陷。它在本分支通过不代表已经修复，基线名单仍保留三项。
- **双路径并存期间，规则计数持平甚至上升是正常的。** 旧路径与内核路径并存时，每条规则暂时各有一份实现，收敛要到 M3（切换并删除）才兑现。M3 交回的目标是：
  - 每条 Run 规则在 Host 中最多只保留"门面中的一处派生"，外加逐条列明的 Session 或授权收窄；
  - 写入门禁全部委托给内核；
  - 未知效果与预算的处理，收敛为门面中的恢复操作，加上执行端口提供的事实。

  达不到的，逐条说明原因。
- **可以开始 M2**，范围是 §2 加上 §10.4 的测量探针。M2 结束时是用户决定继续还是停止的检查点。

### 10.11 master 门面完成后的调整（2026-10-19，与前文冲突时以本节为准）

master 已完成 F1（`0116074`）、F2（`d968ba5`）、G0（`3b7d1d9`）和三项 Goose 残留修复（`cc99091`）。业务侧内部类型引用从 224 降到 0，13 条规则的独立判断体从 36 降到 13。§10.7 要求的 M3 前置条件已经具备。

1. **立即合并，不等 M2 结束。** 当前进行中的这一项提交后，马上合并 master 的最新提交，按 §10.8 第 3 点对齐 `run_access`，以 master 的接口为准。M2 剩下的几项（Op.call 如果还没完成、常驻助手打断、预算、未知效果的人工决定）**直接在门面接口背后实现**，不要再按 control、recovery、session_calls、abandon 的旧形状去写。原因：F2 已经重写了这些写入点，按旧形状写会在合并时整体返工。冲突逐文件登记。
2. **内核改动追认，并登记为 K0.2。** `0859373` 除了新增 `FileStore::remove`，还给 `anchor-kernel` 加了两个依赖（`anchor-file-lease`、`sha2`），并在 FileStore 内部引入了按资源的排他阻塞租约。审查方追认这次改动：依赖属于基础设施，C1 的字面要求仍然成立。但它改变了内核的并发语义，需要在工作记录中新增一节"K0.2：FileStore 删除与资源租约"，写明以下几点：
   - 哪些操作取租约；
   - 与 Host 侧 `kernel_runs` 租约的加锁顺序，以及为什么不会死锁；
   - 为什么用阻塞等待而不是非阻塞；
   - `Store` trait 是否未变；
   - Host 侧原有的那把锁能不能因此删掉。能删的，在 M3 中删除。
3. **C1 基线更新**：从现在起，C1 检查改为 `git diff 0859373 -- rust/anchor-kernel` 为空。§6、§7、§8 中出现的 `6ee2b0e` 一律读作 `0859373`。之后的任何内核改动仍然属于必须停下的情况。
4. **Goose 门槛收紧**：合并 `cc99091` 之后，Goose 失败名单必须为空，§10.9 中的三项豁免作废。
5. **已知时序脆弱用例**：`anchor-wecom-gateway` 的 `slow_callback_does_not_block_control_and_older_reply_is_suppressed` 在全量并发运行时已失败三次，单独复跑都能通过。全量运行中如果只有它失败，单独复跑通过即可记为已知抖动，不阻塞交回。master 那边会另行修复。
6. `da708cb`（经门面删除）和 `5293e65`（测量探针）在 M2 交回时一并审查。

### 10.12 §10.11 审查结论与 M2 后续（2026-10-19）

**§10.11 通过。** 审查方核对了以下几项：

- 合并提交 `490c000` 的两个父提交分别是 `2582528` 和 master F2 `d968ba5`；`cc99091` 已包含在内。
- 审查方在机器空闲时重跑 workspace：128 个 target，2113 通过、0 失败、123 忽略。
- 围栏测试与计数清单都和 master F2 逐字节一致。
- 内核从 `0859373` 起零改动。
- 工作记录已补 K0.2 一节。
- Goose 120 / 0 / 0 采信交回证据，审查方没有重跑。

**1. 并行分支里的 Op.call：不扩展校验器。** master 从 `82a726d`（2026-10-06）起，就在 Host 准入的 `reject_snapshot` 中拒绝"fanout/join 区域内的 Op.call"，所有准入路径都会经过它（`execution.rs`、`graphs.rs`、`main.rs`）。旧系统本来就不支持这种组合，所以内核路径在同一个准入点、用同一条具名错误拒绝即可。M2 验收时把它算作产品层的准入规则，不算内核路径上的 `Unsupported`。

**2. Goose trace 读取返回 500：请确认根因。** 首轮 `native_goose_compacted_effect_survives_host_kill_and_is_inspected_without_replay` 在读取 `/runs/{id}` 时返回 `Goose trace resource is malformed`，但事后保留的现场 JSON 能正常解析。`490c000` 改了 `goose_acp/trace.rs` 的 `store_evidence`，并新增了回归测试 `trace_reader_retains_complete_snapshot_during_evidence_publication`。请在交回中写明：

- 根因是不是"读到了正在写的证据文件"；
- 修复是不是原子替换：先写临时文件并 fsync，再 rename，然后 fsync 父目录；
- 这个回归测试在修复前能否稳定失败。

**3. 继续 M2 剩余项**：常驻助手打断、预算、未知效果的人工决定、大规模对照（§2），全部在门面接口背后实现。M2 结束时按 §9 模板交回，由用户决定是否继续 M3。

### 10.13 M2 审查结论与 M3 开工（2026-10-19，与前文冲突时以本节为准）

**M2 通过。** 审查方核对了以下几项：

- 最终提交 `4d3d19d` 已同步 master `cc919d1`，其中包含 trace 原子写入修复 `d042ebe`。
- 内核从 `0859373` 起零改动。
- 围栏测试与计数清单都和 master 逐字节一致。
- 内核路径上只剩一处 `Unsupported`，是缺少 GraphCallPort 时的配置错误。
- 新增的 Code 提交种类 `AdmissionFailed` 已按 §8 登记；S 候选 0 条。
- 全量测试审查方没有重跑，因为当时 master 正在跑 Goose。采信交回数字：workspace 2139 / 0 / 136，Goose 加本地 Provider 133 / 0，Web 311。

**用户决定（2026-10-19）：Anchor 尚未投入生产，不需要生产切换方案。** 因此：

- 不写离线迁移工具，也不为旧数据保留读取路径。按 C6，读到旧 RunStore 记录时具名拒绝。
- 本地开发状态里的旧 Run 会被拒绝。请在交回中写明清理办法：要清哪些目录，或者给出一条命令。
- 合并前清单中的"生产数据离线迁移"一项取消。

**M3 开工。** 按 §3 和 §10.7 执行，另有以下调整：

1. **先冻结对照快照，再删除旧 Runner。** 这一条更正 §4 与 §10.7 之间的时序错误：§10.7 把删除 `GraphRunner` 提前到了 M3，但 §4 把生成 20,000 例快照放在 M4，到 M4 时对照组已经不存在了。所以 M3 的**第一个提交**必须是：
   - 用现有的旧 Runner 生成各组随机 Graph 的派发轨迹与最终事实快照，规模不低于 M2 的对照（串行 10,000，并行有失败与无失败各 5,000，模块 5,000，调用 5,000）；
   - 快照头写明生成器版本和 seed 范围；
   - 快照保存的是旧侧事实，新侧对照时仍按原差分规则比较，包括 H2 任务书 §1 的三条不变量，**不要求逐字节相等**；
   - 差分测试先改为同时对旧 Runner 和快照比对，两边都通过之后，才能删除旧 Runner。
2. **切换即删除**：默认切换到内核与删除旧类型在 M3 内一起完成，删除范围见 §10.7。M4 只剩 `GraphRunRecord` 中不再拥有的字段和 `CompletionFact` 的收尾。§4 中 M4 的 grep 标准（`GraphRunner|RunStatus|FileRunStore|stop_requested|pause_requested` 为零）提前到 M3 验收。
3. **原子写入只保留一份实现。** master 的 `write_durable`（`main.rs:78`）和分支的 `store_evidence`（`goose_acp/trace.rs:12`）都做了原子发布。如果 `store_evidence` 还保留着自己的"临时文件加 rename"逻辑，改为调用 `write_durable`，删掉重复实现。
4. **命名收尾。** 内核路径成为唯一路径之后，`kernel-probe/executor`、`kernel-probe/host`、`kernel-probe/operator` 这三个 principal id，以及 `validate_probe` 等名字里的 probe 都不再准确，改为正式名称。principal id 会写进历史，因为还没投入生产，直接改，不做兼容。
5. **忽略数核对。** 交回报告里的忽略数从 123 变成了 136，但相对 master，`#[ignore]` 字面量只多了 1 个（124 对 123）。请在 M3 交回中说明多出的 12 个从哪来，例如 `cfg_attr` 条件忽略，或者同一个测试模块被编进了多个 target，并列出名单。
6. **验收重点**，§10.1、§10.5、§10.10 的要求不变：
   - C2 棘轮：直接写 Run 的点降为 0，逐项列出每一处的去向；
   - 规则清单：每条 Run 规则在 Host 中只剩门面里的一处派生，加上逐条列明的 Session 或授权收窄；
   - §10.5 的 Session 层"一个事实一个写入者"检验表；
   - 逐条回答宪法"完美"的三个条件与四个极限；
   - Goose 0 失败。
7. **M3 交回之后**，由用户决定是否立即把 `constitution-core` 合入 master，之后 M4、M5 直接在 master 上继续，不再维持长期分支。

### 10.14 与 master 并行期间的规则（2026-10-19）

master 上另有 agent 在改 Web、`api.rs`、`main.rs`、`goose_acp/pilot.rs`、`run_access/reads.rs` 等文件。M3 照常推进，另外遵守以下几条：

1. **使用独立的构建目录。** 两份 `runtime-contract-tests.md` 的标准命令都把 `CARGO_TARGET_DIR` 设成了 `/tmp/anchor-goose-regression-target`。两个 worktree 共用这个目录时，一方的构建会覆盖另一方正在使用的 Host 二进制、测试二进制和 relay，导致 Goose 结果不可信。本分支一律改用 `CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target-core`，`ANCHOR_GOOSE_RELAY_BINARY` 也指向这个目录下的 relay。交回报告中写明实际使用的目录。
2. **M3 期间不合并 master**，到 M3 结束时再按 §10.2 合并一次。预计冲突集中在 `run_access/reads.rs` 和 `main.rs`。解决原则：业务行为以 master 为准，运行时实现以本分支为准。master 新写的代码如果用到了被删除的旧类型，在合并时改为调用门面，并逐条登记。
3. **负载抖动**：在并发负载下出现的失败，必须单独复跑确认后才能定性。只有单独复跑也失败的，才算真失败。

### 10.15 M3 停止交回的裁定（2026-10-20，与前文冲突时以本节为准）

**结构核对通过（审查方只做了 git 层面的检查，没有重跑测试）：**

- 内核从 `0859373` 起零改动；
- 旧类型 `GraphRunner|RunStatus|FileRunStore|stop_requested|pause_requested` 在 `rust/` 下零命中；
- 快照提交 `0d029eb` 在删除提交 `6e91e5c` 之前。

**1. 普通 resume 的恢复契约：选项 1，由 Agent 核查后继续。**

- 理由：
  - 这是 master 现行的产品行为，也是 `AGENTS.md` 的长期原则：先保存事实，再由 Agent 核查现场后继续，不把内部 attempt 包装成用户审批。
  - 宪法第二条禁止的是**自动重放**，不禁止 Agent 核查。人的显式 resume 本身就是人的决定。Agent 在同一个原生 Session 中核查现场，而不是重新执行，这符合"未知不重放"。
  - "第一次 resume 只停驻"这个行为从来没有在 master 上出现过。它是 M2 时把任务书里"未知效果的人工决定"理解成了"普通 resume 必须先停驻"造成的，属于审查方任务书措辞的问题，不是产品决定。
- 做法：
  - 人工 Completed/Abort 决定 API 保留，作为另一条路径：人已经知道结果时直接裁定。它的不重放、幂等、冲突、Abort 断言全部保留。
  - M2 的 `kernel_unknown_native_effect_requires_a_durable_human_decision_before_continuing_or_abandoning` 改为按选项 1 的准备方式进入已登记的 pending 状态。原有的人工 API 断言不得删减。测试名如果不再符合实际行为，就改名。
  - 在工作记录中登记为"M2 行为更正：恢复回到 master 契约"，并逐条列出测试的准备方式改了什么、断言保留了哪些。这一项不算新的 S 候选，因为结果是对齐 master，用户看不到语义变化。

**2. 其余 Goose 失败：按 `m3/goose-regressions.md` 中各组的"建议边界"修复。** 约束如下：

- 不全局改变 `Uncertain` 的映射；
- spike 只在它自己的 `completion_fact` 边缘映射到已有的 Failed，保持 fail-closed，不能写成可以安全恢复；
- 不重放未知效果，不放宽业务断言，不改内核。

如果某一组只有改业务断言或改内核才能修好，就停下交回，只交那一组，其余各组继续修。

**3. 门槛**：固定 Goose 的完整忽略测试必须 0 失败。本轮失败过的 12 项，每一项都要单独复跑一次并贴出结果。

**4. S27** 在 M3 交回时一并审查。它实际上回答了待决的 S4（退役之后还能不能 abandon），需要用户同时裁定。

**5.** M3 通过之后，由用户决定是否把分支合入 master。

### 10.16 M3 审查结论（2026-10-20）

**M3 通过。** 审查方在机器空闲时独立复验了 `41e320d`：

- workspace：129 个 target，2153 通过、0 失败、125 忽略，与交回数字一致；
- 固定 Goose 完整忽略套件，按标准命令执行，target 用 `-core` 目录：122 通过、0 失败、0 忽略，与交回数字一致；
- 内核从 `0859373` 起零改动；旧类型 `GraphRunner|RunStatus|FileRunStore|stop_requested|pause_requested` 在 `rust/` 下零命中；
- 快照提交 `0d029eb` 在删除提交 `6e91e5c` 之前；
- 本分支已包含 master 最新提交 `0aa5197`，master 没有未提交的已跟踪改动。

**待用户决定：**

1. S27（承接 S4）：退役只移动 Session 的 current 指针，未结算的 Run 仍然可以被 Human abandon，退役记录和 Run 的进度都保留。
2. 是否把 `constitution-core` 合入 master。合入之后，M4、M5 在 master 上继续。

用户决定之前，不要开始 M4。

### 10.17 用户决定与合并步骤（2026-10-20，与前文冲突时以本节为准）

**用户决定：**

- **S27 批准。** 退役只移动 Session 的 current 指针，未结算的 Run 仍可由 Human abandon，退役记录与 Run 的进度都保留。这一条同时结清 S4。请在工作记录中把 S27 标为已批准，S4 标为由 S27 结清。
- **批准把 `constitution-core` 合入 master**，之后 M4、M5 在 master 上继续。

**合并步骤：**

master 目前有 debug 工作在进行，工作区里有 28 个已跟踪文件尚未提交。其中 4 个本分支也改过：`api.rs`、`api/tests/sessions.rs`、`application/assistants.rs`、`pilot_tools.rs`。所以不能马上快进合并。

1. **现在要做的**：提交本任务书和工作记录的更新，然后等待。不开始 M4。
2. **master 的 debug 提交之后**，由用户通知你。届时把 master 最新提交合入本分支：
   - 冲突按 §10.14 的原则处理：业务行为以 master 为准，运行时实现以本分支为准；
   - master 新写的代码如果用到了已删除的旧类型，改为调用门面；
   - 每个冲突逐条登记。
3. **合并后重跑全部门槛**：
   - workspace；
   - 固定 Goose 完整套件，使用 `-core` 目录；
   - Web；
   - clippy、fmt；
   - 围栏。

   用 §9 模板交回，交回中写明最终提交，并确认这个提交包含 master 最新提交。
4. **审查通过后**，由 master 一侧执行 `git -C /home/mansteinl/Anchor merge --ff-only constitution-core`，本分支不要去动 master 的工作区。
5. **合并完成后**：
   - M4、M5 改在 master 上进行；
   - `-core` 构建目录不再需要；
   - 本 worktree 和分支保留到 M5 结束，何时删除由用户决定；
   - 是否推送另行决定。

### 10.18 执行合并（2026-10-20，与 §10.17 冲突时以本节为准）

master 已提交，工作区干净，tip 是 `dfd3427`。比本分支多 3 个提交：`08db12f`（Web 页面状态）、`87c46a3`（助手身份规范化、创建重试幂等、Session v9 迁移）、`dfd3427`（文档）。和本分支重叠的文件有 18 个。

1. **先提交待提交的文档**：本任务书 §10.16–§10.18、工作记录里的 S27/S4 状态，以及 `docs/anchor-runtime-migration-overview.md`（如果还没入库）。只提交这些路径。
2. **合并 `dfd3427`**：用 merge，不用 rebase。冲突原则同 §10.17 第 2 步。下面几项要专门处理：
   - **`run_access/projection.rs`**：保留本分支的实现，**不要**移植 master 的 pause/cancellation 执行器表。`87c46a3` 修的现象是：旧 `project_run` 读 Session/渠道部署资格时一直持有全局 active 锁，并发的 `/graphs`、`/runs` 只能排队等重复的整份插件哈希，用户看到的是好几秒的登录失败。需要确认两点，并逐条登记结论：
     - (a) 本分支没有任何路径在持有 `KernelRuns.active` 时做 Session、部署或插件读取，也不跨 `.await` 持有它；
     - (b) 本分支的 projection 是否也在读取时做整份插件哈希。如果是，按 master 的方式改：先拒绝历史绑定，再查部署；观测改用 live `PluginMetadata`；执行前的完整检查保留。
   - **`87c46a3` 为这个现象加的测试**：如果有，保留它，让它在 Kernel 路径上跑并且通过。
   - **`tests/goose_persistent_assistant.rs`**：master 新增的 canonical-account 重启等场景要全部保留，走 Kernel 路径。所以固定 Goose 的总数会超过 122，交回时列出新增了哪几项。
   - **Session v9 迁移、渠道账号别名、创建幂等键**：这些是业务层，以 master 为准。本分支不改 Session schema。
   - master 新代码如果用到了已删除的旧类型，改为调用门面。C1 照旧：`rust/anchor-kernel` 相对 `0859373` 的 diff 必须为空。
3. **重跑 §10.17 第 3 步的全部门槛**，交回格式同那一步。另外两项要求：
   - 交回中附上 merge 提交的冲突登记；
   - 写明最终提交包含 `dfd3427`，确保 master 能快进。

### 10.19 M4：在 master 上收尾（2026-10-20，与前文冲突时以本节为准）

**现状**：master 已快进到 `204d126`，HEAD 是 `ceac4ae`。从现在起只在 `/home/mansteinl/Anchor` 上工作，分支 worktree 只读保留。原 §4 中这两项已在 M3 完成，本节不再要求：删除旧类型，以及冻结快照（30,000 例）。

**由谁做**：由原分支 agent 在 master 工作目录里做。master agent 如果同时做业务开发，不得修改以下路径：

- `application/run_access/`
- `anchor-runtime/src/graph/`
- `node_host/`
- `goose_acp.rs`
- Web 的 `Timeline.tsx` 和 `RunDetailPage*`

两边都只 stage 自己改的路径。

**范围一：`GraphRunRecord` 逐字段处置。** 每个字段登记一行：删除，或保留并写明所有者和理由。审查方粗查到的线索如下，数字是全仓 / 生产代码的命中数，以你核实的为准：

| 字段 | 线索 | 预期方向 |
| --- | --- | --- |
| `recovery`、`recovery_submissions` | `reads.rs:151` 读出后立刻用 `recovery_facts` 的结果覆盖，记录里的这两个字段只是个搬运容器。生产代码中读取它们的只有 `run_access.rs:309`（`has_pending_recovery`）和 `state.rs:497`。 | 移到 RunView，或者由 Kernel history 投影出来，然后从记录中删除。 |
| `error` | Web `Timeline.tsx:315` 会显示 `state.error`。Kernel 世界里，失败原因保存在 Settle 的 reason 中。 | 失败原因改由 Kernel 投影提供，记录中的字段删除。**Web 仍然必须显示失败原因**。HTTP 键名可以保留，也可以改用 `projection.reason`，选哪种都要登记。 |
| `updated`、`touch()` | 生产代码 0 处。更新时间已经由宿主在提交时记录。 | 删除。 |
| `sequence` | 全仓 72 处，校验器和 `logic.rs` 会递增它。 | 能由 Kernel 的 version 或 history 推出来就删除；推不出来就保留，并写明它承载的是哪条图事实。 |
| `format`、`RunRecordFormat`、`GraphRunRecordWire` | 当前用来对旧格式做具名拒绝（C6）。 | 记录现在存放在 Kernel resource 里。由你判断格式版本该放在记录上还是 resource 上，具名拒绝必须保留。 |
| `plugin_bindings_initialized`、`plugin_bindings`、`graph_calls` 等 | 都是业务事实，生产代码在用。 | 保留，只登记。 |

**范围二：`CompletionFact`。** 四个变体 `NotStarted`、`Resumable`、`Uncertain`、`Yielded` 在生产代码中都有使用。逐个说明它是否只为旧 Runner 存在，并给出证据。如果都不是，就写"无可删"。不要为了删而删。

**范围三：旧机制残留测试。** 检查 `anchor-runtime/src/graph/tests/migrated_*.rs`，例如 `completion_fact_closes_run_store_crash_window…`，看测试名或断言里是否还在描述已经删除的 RunStore 机制。按 §0 规则处理：要么改名指向新的等价机制，要么在映射表中登记后删除。

**会受影响的测试**（审查方的规格检查，请补全）：

- `run_access/http.rs` 中的 `/runs` 详情字节基线，它直接拼了 `"error"` 和 `"recovery"`；
- Web 的 `RunDetailPage.test.tsx`；
- Web 的 `ChannelSessions.test.tsx`。

改动对外 JSON 时，按"状态词汇类断言"登记。显示失败原因、待恢复提示属于业务断言，不得放宽。

**门槛**（一次交回，用 §9 模板）：

- workspace；Web test 和 build；clippy、fmt；围栏。
- **完整固定 Goose 套件，使用 master 的标准命令和 `/tmp/anchor-goose-regression-target`**。这一轮同时覆盖合并时审查方没有重跑的 40 个 target。
- C1：`rust/anchor-kernel` 相对 `0859373` 的 diff 为空。
- 交回中附上字段处置表、`CompletionFact` 结论和删除测试映射。
- §8 的停止条件照旧有效。M5 不自动开工。不推送。

### 10.20 M5：测量与收尾（2026-10-20，与前文冲突时以本节为准）

**现状**：M4 已验收。审查方定向重跑了 Graph 140 / 0 / 0、run_access 81 / 0、围栏 1 / 0 / 0，以及 Web 的三个改动文件 26 / 0。用户已推送 master，`origin/master = ce90b5b`，并批准开始 M5。工作目录仍是 `/home/mansteinl/Anchor`。

**由谁做**：原分支 agent。M5 以测量和文档为主，§10.19 的路径限制解除。M5 要改以下四份文档，master agent 不要动：

- `docs/constitution-migration-task.md`
- `docs/constitution-refactor-log.md`
- `docs/constitution-host-integration-plan.md`
- `docs/anchor-runtime-migration-overview.md`

两边都只 stage 自己改的路径。第一步：单独提交本文件。

**测量基点**：开工时选一个 master 提交作为"迁移后"的基点，记为 `<基点>`，所有测量都在这个提交上做，交回时写明。

- 之后 master agent 的提交不纳入测量。
- 测量期间如果有新提交改到 Run 相关路径，在交回里列出即可，不用重测。

**范围一：探针复核。** Probe 在 M2 已经接入（`kernel_probe.rs`，`kernel_runs.rs:99`），M5 不新增探针，只做两项确认：

- `points.jsonl` 能从 history 重建。取一个数据目录，比对在线写入的点和重建出的点，差异要登记。
- 附一段真实运行的样本和汇总，口径同 §10.4：按校验器、身份、动作、裁决、原因码汇总。样本取自现有的某个确定性测试，或单个 Goose target。**不要为了采样重跑完整 Goose。**

**范围二：写 `experiments/constitution-host-preflight/final/measurement.md`。** 内容按原 §5 第 2 项，口径补充如下：

1. **行数**：按 crate 列出删除和新增行数，区间从 `fc558e2` 到 `<基点>`。
   - 生产代码和测试分开统计，内联的 `#[cfg(test)]` 模块算测试，分不开的写明原因。
   - `anchor-kernel` 单独一行。
2. **规则清单**：用 §10.1 的八条规则，列 `1d1ba54` 和 `<基点>` 两列，每处写 `文件:行`。
   - 读同一视图或同一派生函数得出结论的，不算独立判断点。
   - 可以引用 M1.5、M2、M3 的表，但最终数字要在基点上重新核实。
3. **直接写 Run 的点**：H1 基线是 save 16、mutation 36，到基点为 0。引用 M3 的 52 项去向，确认 M4 之后仍然成立。
4. **测试**：
   - 迁移前后的测试函数数量，workspace、Web、Goose 忽略测试分开统计；
   - M1–M4 的断言修改、删除测试映射和 S 候选，只汇总条数和链接，不重复正文。
5. **仍然手写的判断**：按 Session 层、授权、执行端口三类列出。每条写明位置，以及为什么不能由内核定律推出。
6. **宪法复核**：
   - 按 §10.1 回答"完美"的三个条件，并过一遍宪法第 7 节的四个极限；
   - 只写有证据的结论，规模压测、长期运行、真实模型这些没做的，写"未验"。

**范围三：收尾文档。**

- 更新以下几处：
  - 本文顶部的两处状态行；
  - 上位计划的状态行；
  - 概览文档的"现在在哪里"；
  - 工作记录里的 M5 一节。
- 写一张"迁移后待决清单"，替代原 §5 第 3 项的"合并前事项"。每项写现状和该由谁决定，不要替用户做决定。至少包括：
  - 生产数据离线迁移：用户已决定不提供（Anchor 尚未上生产），登记即可；
  - W4：预算耗尽是否结算为失败；
  - 真实模型 Provider、外部渠道实网和部署的验收；
  - 是否把时间作为内核事实；
  - history 和 `points.jsonl` 的保留策略；
  - 何时删除 `constitution-core` 的 worktree 和分支。

**门槛**：

- **只改了 `docs/` 和 `experiments/` 时**：
  - 跑 fmt、围栏和 C1；
  - 列出 `ce90b5b` 到 `<基点>` 之间的 Rust 源码差异。没有差异就直接引用 M4 的 workspace 结果，有差异就重跑 workspace；
  - **不需要重跑完整 Goose。**
- **改了任何源码时**（例如探针重建有问题需要修）：跑 §7 全套，加完整固定 Goose，并在交回里写明改了什么、为什么改。
- **C1**：`rust/anchor-kernel` 相对 `0859373` 的 diff 为空。
- 测量脚本放在 `final/` 下并提交；原始日志不入库，按 §10.6。
- 一次交回，用 §9 的模板。§8 的停止条件照旧有效。不推送。
- M5 交回后迁移任务结束，删不删分支由用户决定。
