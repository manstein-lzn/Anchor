# 长历史读取与内存：隔离验证

本切片从上游 `a097b38` 开始，修复长历史被每次状态读取、恢复投影和 Probe 重建重新物化为全部快照的问题。历史文件与恢复、成本、编号语义保持既有契约。没有部署，也没有真实模型或对外发送。

## 实现与独立审查

- `FileStore` 逐条校验冷历史，以轻量 `RecordEntry` 保存原 stamp/submission/verdict；只保存最后 after 的字节范围，不保存每条快照。`load` 的成本从全部 stamp 饱和累加，不信任快照里的 spent。
- 本实例的同步追加增量更新内存摘要；外部修改、同长改写、文件替换、截断、删除/重建使旧摘要失效。解码失败显式失效；I/O 失败不发布摘要。不存在新的持久文件或第二份事实来源。
- 恢复扫描与快照回读使用同一打开文件；只读 HistoryReader 保持不加锁、不写文件。完整历史 API 仍返回原 Record。
- Host recovery 与 Probe 改用轻量条目；`submit` 只读取一次目标，`place` 用完整条目数量而不反序列化全部快照。Oink 样本全部是 Write，不能将 place 当作该样本的热路径。
- 独立 Agent 审查覆盖最后 after、全裁决成本、文件替换、部分写入与只读边界。发现的扫描后换文件、深层 JSON 热冷分歧和解码失败失效边界均已修正，并有对应回归。

该进程内摘要替代 `load` 的 `Vec<Record>` 全量快照物化与重复计费扫描；轻量接口替代 Host recovery/Probe 的完整历史读取。没有新增依赖、全局锁或持久格式。

## 同源前后对照

数据为事故历史的私有临时副本：288,355,398 字节、525 条、当前资源 1,054,468 字节。只保留聚合指标；副本不在 Git 中。Rust 1.99.0，dev opt-level=1、debug=0，同机同一份数据，默认分配器。上游基线仅加入相同的测量代码。

| 测量 | 上游基线 | 修后 |
| --- | ---: | ---: |
| 首次 `/runs` | 2981 ms | 984 ms |
| `/runs`＋`/timeline` 查询对中位数 | 4451 ms | 32.3 ms |
| 查询对最大值（12 次） | 4573 ms | 35.3 ms |
| 12 次查询对读取字节 | 13,866,402,661 | 25,343,555 |
| HTTP 进程峰值 RSS | 1,136,440 KiB | 48,780 KiB |
| 预热后至轮询结束 RSS | 1,135,660→1,136,440 KiB | 47,028→48,780 KiB |
| 单次暖 `load` 中位数 | 932 ms | 2.58 ms |
| 一次合成 Write 提交 | 1941 ms | 27.4 ms |
| 合成 Write 提交读取字节 | 578,828,034 | 1,062,765 |

Write 使用内核公开入口、确定性允许裁决器，保留相同大小的资源状态；不是一次真实 Goose 对话。读取量为 `/proc/self/io` 的 rchar，包含状态与必要小文件，命中页缓存也计入，不等同物理磁盘读取。

HTTP 测量使用真实 Axum router 与应用门面，在单个 Tokio 测试进程里重复访问；12 次为持续轮询的局部回归，不是 32 worker 的长期内存 soak。修后仍首次完整校验约 275 MiB 历史。没有证明生产进程超过 20 GiB 的全部内存都由此造成，也未用剖析器确认 glibc arena 的因果关系。

指标原值与边界见 [measurements.json](measurements.json)。`pair_p95_ms` 在 12 个样本上取最大样本，表中按最大值标示。

## 实际验证

所有构建使用独立 target、`CARGO_BUILD_JOBS=1`、`CARGO_PROFILE_DEV_DEBUG=0`、`CARGO_PROFILE_TEST_DEBUG=0`；启动构建前检查 `/proc/meminfo`。编译通常加 `ulimit -v 6291456`，大历史测量加 `ulimit -v 4194304`。网络沙箱用例不继承该地址空间限额，helper 的虚拟地址预留与此限额不兼容。

实际执行：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-kernel --locked --offline -- --test-threads=2
cargo test --manifest-path rust/Cargo.toml -p anchor-runtime --locked --offline -- --test-threads=2
cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host --no-run --locked --offline
# 对该构建产物执行：
$HOST_TEST_BINARY --test-threads=2
cargo build --manifest-path rust/Cargo.toml -p anchor-sandbox-bwrap --bins --locked --offline
$HOST_TEST_BINARY --exact node_tools::tests::graph_network_intent_is_applied_and_host_authority_still_limits_it --test-threads=1
$HOST_TEST_BINARY --exact pilot_tools::frozen_mounts_tests::a_real_stdio_client_connects_only_with_the_network_capability_and_denial_does_not_break_offline_tools --test-threads=1
cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --test run_access_fence --test layer_fence --test configuration_fence --locked --offline
cargo clippy --manifest-path rust/Cargo.toml -p anchor-kernel --all-targets --locked --offline -- -D warnings
cargo clippy --manifest-path rust/Cargo.toml -p anchor-runner-host --all-targets --no-deps --locked --offline -- -D warnings -A clippy::result_large_err -A clippy::useless_borrows_in_formatting
```

结果：Kernel 60/0，Runtime 159/0（含 doc tests），围栏 5/0。Host 普通 bin 首轮 1089/2/3（通过/失败/忽略）；两项失败首先缺少 `anchor-sandbox-network`，补齐后带地址空间限额仍失败，移除限额后各自单独通过。最终恢复/Probe/冻结语料 35 项及恢复门面 5 项再次通过；列表/详情/时间线四项定向测试此前通过。新增的独立 HTTP 性能回归默认忽略，已显式执行通过，上游对照按预期失败于全历史重读断言。

Kernel 严格 Clippy 通过。组合严格 Clippy 首先被上游 `anchor-rsi` 的 `chunks_exact_to_as_chunks` 拦住；Host `--no-deps` 严格模式有上游既有 `result_large_err`、`useless_borrows_in_formatting` 共 51 条，未涉及本次修改路径。显式只放行这两类之后 Host 全目标 Clippy 通过，不称为原命令全绿。

## 重现测量

```sh
cargo build --manifest-path rust/Cargo.toml -p anchor-kernel --example history_performance --locked --offline
$TARGET/debug/examples/history_performance /absolute/private-copy-root run/example
$HOST_TEST_BINARY --exact api::tests::history_performance::repeated_run_and_timeline_queries_do_not_reread_snapshot_history --ignored --nocapture --test-threads=1
```

Kernel example 会对指定副本追加一条保留状态的合成记录，只能使用私有测试副本。HTTP 回归默认生成 256 条逐渐增长的合成快照，不需要线上数据；显式 `ANCHOR_PERF_HISTORY_COPY` 可指定只含 resource.json/history.jsonl 的私有副本目录。

## 未覆盖

- 本轮没有部署；生产 OOM 是否停止及长时间 RSS 曲线需由 B 部署后观察。
- 没有更改历史快照的磁盘增长方式、并发锁语义、前端轮询周期或分配器。
- 完整 Goose 本地 Provider 端到端未执行：本机现有 Goose 摘要为 `ff0d7025…`，固定回归要求 `17e065a7…`，没有绕过摘要校验或在共享 WSL 重建 Goose。
- 5/20/50 用户的完整 Host→Goose→MCP 压测、真实 Provider 与企业微信业务验收仍待单独进行。当前数值只证明本切片的历史读取和分配大幅减少。
