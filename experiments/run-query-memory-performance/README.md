# Run 查询分配：代码去重与分配区隔离对照

基线为 `6cbfdaee0303aea1d244d6c0a6be48b54ff12bcb`。本切片接续[长历史修复](../kernel-history-performance/README.md)，只减少读取时重复解码、临时复制和列表同时持有的大记录。没有部署、修改线上 allocator 参数、重启服务或触发真实 Provider/业务发送。

## 改动与事实边界

- `record_of` 从 JSON 引用反序列化，不先复制整棵 JSON；一次 `read_run` 共用已解码记录和同一份 frontier。frontier 仍在临时副本上运行原有 `prepare`，不引入另一套执行规则。
- `FileStore::with_record_entries` 借用已缓存历史，替代恢复查询对 `Vec<RecordEntry>` 的深复制；原 `record_entries` 委托同一入口，资源租约、缓存刷新和删除收尾规则只有一份实现。当前回调只做纯投影，不重入同一资源存储。
- HTTP 详情复用本次视图、记录和更新时间，不再重复 `read_run` 或复制完整记录。通用 `project_run` 对调用者传入的旧视图仍刷新现场。
- 列表先枚举 ID，逐条读取并投影，不同时保留所有 Run 的完整记录；仍完整解码和校验每条记录，没有另建字段子集 schema、持久索引或状态缓存。单 Run 基准没有量化多 Run 列表的峰值收益。
- 持久格式、计费、恢复过滤、并发租约、权限及写入前校验不变。独立静态审查见 [review.md](review.md)。

## 对照结果

同机 WSL / glibc 2.39 / rustc 1.99.0 (b940084d7 2026-09-28)，dev/test `opt-level=1`、`debug=0`，构建 `jobs=1`。基线干净源码仅加入同一份测量代码；源码与二进制摘要见 [measurements.json](measurements.json)。

四组分别启动五个独立进程，基线/修复与默认/arena=2 交错运行。每个进程使用一个合成 Run：256 条增长快照、历史 67,662,374 字节、当前资源 525,202 字节，最后 input 是一个 512 KiB 字符串。32 个 Tokio worker、8 路并发、64 对 `/runs`＋`/timeline` 请求，前 16 对预热、后 48 对计时；实际观察到 7 个工作线程，不表示 32 线程都饱和。

表中是五次进程结果的中位数；p95 列是每次 48 个查询对的 p95 再取中位数，不是线上尾延迟保证。

| 版本 / 参数 | 结束 RSS MiB | HWM MiB | 查询对 p50 ms | 查询对 p95 ms | 查询对/秒 |
| --- | ---: | ---: | ---: | ---: | ---: |
| 基线 / 默认 | 51.84 | 51.84 | 2.158 | 2.602 | 2689 |
| 修复 / 默认 | 47.64 | 47.64 | 1.825 | 2.319 | 3064 |
| 基线 / arena=2 | 41.68 | 43.95 | 3.010 | 3.597 | 1990 |
| 修复 / arena=2 | 40.29 | 40.29 | 2.191 | 2.498 | 2751 |

默认分配器下，代码去重的结束 RSS 中位数降低 8.1%，查询对 p50 降低 15.4%。完整范围、每批 RSS、glibc 统计和延迟样本保留在指标文件；单次测量较短，不能把此比例外推到线上 1.3 GiB 的进程。

两版默认参数下，48 对暖查询读取量相同，均为 50,551,531 字节；本轮没有进一步减少必要的当前状态读取。结束时 glibc 已分配账目中位数约 1.35→1.32 MiB，空闲账目约 24.27→19.83 MiB，与削减临时分配峰值及保留空间的方向一致；这不是线上全部增长的因果复现。

修复版再设置 `MALLOC_ARENA_MAX=2`，相对修复版默认参数，结束 RSS 中位数变化 -15.4%，查询对 p50 变化 +20.0%。因此本切片只交付代码去重，不直接部署 arena=2；它有内存/竞争权衡，是否限制及取值要在代表性并发负载下另验。未尝试 arena=4/8 或替换 allocator。

20 个进程均通过进程内响应一致性和预热后读取量断言；跨进程仅归一化 `started`/`updated` 后 HTTP 输出完全一致。指标文件包含归一化响应及摘要。查询耗时包含进程内 Axum 路由、响应解析/断言和每批采样，不含真实 HTTP 网络传输；响应未包含 fixture 的大 input。

`mallinfo2` 的 allocated/free 是分配器账目，包含 tcache/元数据等，不能当作精确 Rust 活对象、驻留空闲页或整个进程物理需求。每批采样本身和保留的测量数组也有少量分配。本短基准不能复现昨天至今的增长过程，也不能排除小规模长期泄漏。

## 验证

所有 Rust 构建均使用独立 target、`CARGO_BUILD_JOBS=1`、`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`，并使用 `--locked --offline`。Kernel/Runtime 通过 scripts/test.sh 使用 tmpfs，Host 普通全量与 helper 复跑显式使用 tmpfs；前置 Host 定向测试使用普通临时目录。无地址空间限额，不操作线上数据。

实际命令与结果（FIXED_TEST_BIN 是保留的测量副本；HELPER_TEST_BIN 是摘要相同、位于原 Cargo debug/deps 的产物）：

```sh
scripts/test.sh -p anchor-kernel -p anchor-runtime --locked --offline -- --test-threads=2
$FIXED_TEST_BIN application::run_access:: --test-threads=2
$FIXED_TEST_BIN api::tests::run_access_json:: --test-threads=2
$FIXED_TEST_BIN api::tests::kernel_probe:: --test-threads=2
TMPDIR=/mnt/wslg/runtime-dir $FIXED_TEST_BIN --test-threads=2
# 五项 helper 路径失败分别从原 Cargo 路径复跑，每项使用 --exact 与 --test-threads=1：
TMPDIR=/mnt/wslg/runtime-dir $HELPER_TEST_BIN --exact "$HELPER_TEST_NAME" --test-threads=1
# 最终源码重建后，从原 Cargo 路径执行普通全量：
TMPDIR=/mnt/wslg/runtime-dir $HELPER_TEST_BIN --test-threads=2
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host --test run_access_fence --test layer_fence --test configuration_fence --locked --offline
cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-kernel -p anchor-runtime --all-targets --locked --offline -- -D warnings
cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-runner-host --all-targets --no-deps --locked --offline -- -D warnings
cargo +stable clippy --manifest-path rust/Cargo.toml -p anchor-runner-host --all-targets --no-deps --locked --offline -- -D warnings -A clippy::result_large_err -A clippy::useless_borrows_in_formatting
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check
scripts/check-sensitive-wording.sh --all
git diff --check
```

结果：Kernel **61/0**、Runtime **159/0**（含集成与 doc tests）；前置 Run 门面 90 项、HTTP JSON 1 项、API Probe 11 项通过。最终源码的 Host 普通 bin 全量 **1091/0/3**（通过/失败/默认忽略），五项架构围栏通过。ignored 性能用例另以 20 个独立进程显式执行，均通过。Kernel/Runtime 全目标严格 Clippy 通过，fmt、措辞、diff 检查通过。

Host 全目标 `--no-deps` 严格 Clippy 仍被上游既有 `result_large_err`、`useless_borrows_in_formatting` 两类阻断；只放行这两类之后通过，不能称为原严格命令全绿。本轮没有放行 dead_code：详情重复读取删除后，旧 `run_updated` 包装器只剩测试使用，已删除，测试改用既有视图接口。

初次普通全量在搬移后的测量二进制上运行，结果为 1086/5/3；五项失败因 helper 按可执行文件位置推导路径，搬移目录不符合 Cargo profile 布局。从原 `debug/deps` 路径单独复跑五项均通过；最终普通全量也从原路径通过。没有修改这些测试断言、扩大网络权限或跳过失败用例。移动测试产物造成的本机中间构建文件保留在 `.local/`，不进入提交。

一次 Host 初始编译误用了本机旧 Session 产物，报告缺失接口；源码接口实际存在。刷新隔离工作树源时间戳后重新编译当前依赖通过，未为绕过构建问题改动业务代码。该次失败不计为测试通过，也不与性能样本混用。删除旧包装器时先遗漏的两处测试调用在重建中报错，随后一并迁移到现有视图；最终源码重建及验证通过。

## 重现四组测量

在干净基线和修复源码中使用同一份 [ignored 基准](../../rust/anchor-runner-host/src/api/tests/history_performance.rs)，分别构建并保留独立测试二进制，不能让后续构建覆盖正在采样的产物：

```sh
CARGO_TARGET_DIR="$BASELINE_TARGET" cargo +stable test --manifest-path "$BASELINE_ROOT/rust/Cargo.toml" -p anchor-runner-host --bin anchor-runner-host --no-run --locked --offline
CARGO_TARGET_DIR="$FIXED_TARGET" cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host --no-run --locked --offline
BENCH=api::tests::history_performance::repeated_run_and_timeline_queries_do_not_reread_snapshot_history
# 对两个稳定二进制分别运行默认和 arena=2；每次调用是新进程。
env -u GLIBC_TUNABLES -u MALLOC_ARENA_MAX -u MALLOC_TRIM_THRESHOLD_ -u MALLOC_MMAP_THRESHOLD_ -u ANCHOR_PERF_HISTORY_COPY "$TEST_BIN" --exact "$BENCH" --ignored --nocapture --test-threads=1
env -u GLIBC_TUNABLES -u MALLOC_ARENA_MAX -u MALLOC_TRIM_THRESHOLD_ -u MALLOC_MMAP_THRESHOLD_ -u ANCHOR_PERF_HISTORY_COPY MALLOC_ARENA_MAX=2 "$TEST_BIN" --exact "$BENCH" --ignored --nocapture --test-threads=1
```

每组五次并交错顺序；准确的本轮执行顺序记录在 JSON 的 `samples[].order`。JSON 输出可能在 libtest 的 `test ...` 前缀之后，从该行第一个左花括号解析。本机原始输出位于 `.local/run-query-memory/balanced-*.log`，不在 Git 中；公开指标仅来自合成数据，不含真实会话副本。

## 部署与后续观察

本轮未部署。由总控协调 B 将经过集成验证的代码部署到实际运行版本，默认先保留原 allocator 参数，不能把源码推送当成线上已生效。不要为了立刻降低 RSS 对旧进程注入 trim 或重启业务。

上线后用相同的 Run 数、历史规模和并发条件观察 RSS、高水位、接口延迟及 allocator 活跃/空闲账目。区分业务事实增加、缓存条目增加与分配器保留；只看 RSS 不能完成归因。保留空闲空间不会因别的进程需要内存就保证由 glibc 自动全部归还，操作系统可能换出匿名页，WSL 回收也不是同一层机制。

前端仍每三秒轮询，完整历史的磁盘快照增长方式未改，多 Run/32 线程持续负载、Host→Goose→MCP 本地 Provider 完整链、真实 Provider 与真实渠道均未在本轮扩验。
