# Constitution Host 接入：预检与回归

本目录保留最初的反例证据，当前可执行程序已按 K0.1 转为回归：三个原反例及停止后的迟到 Code 写入都必须得到修复后的结果，否则程序失败。它使用真实 Kernel 与 GraphValidator、单节点 Op fixture，不运行节点执行器、Goose 或真实 Provider，不代表 H1–H6 的功能验收。

- [原始 evidence.json](evidence.json) 和 [review.md](review.md) 属于 `d0f9b61` 生产源码的历史预检，保留原始内容；其中“不阻塞”的迟到结果建议已被审查决定 D5 否决。
- [审查决定 D1–D5](../../docs/archive/design-records-2026-11-04/constitution-preflight-decision.md) 是本轮授权依据。
- [K0.1 九项检查与验证](k01/README.md)、[当前回归输出](k01/evidence.json) 对应本轮修订。源码 `main.rs` 成功时输出 `k01_regression_passed`，并逐项执行断言。

原反例已移入内核正式测试；本目录另保留跨 Kernel/GraphValidator 的小型公开 API 回归。所有历史损坏、状态文件丢失模拟均在私有 `tempfile` fixture 中进行，无真实进程崩溃或生产文件修改。

从仓库根执行：

```sh
cargo fmt --manifest-path experiments/constitution-host-preflight/Cargo.toml --check
cargo clippy --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml --all-targets -- -D warnings
cargo run --offline --locked --manifest-path experiments/constitution-host-preflight/Cargo.toml > experiments/constitution-host-preflight/k01/evidence.json
```

`--offline` 要求已缓存锁定依赖；此独立 workspace 的 Cargo.lock 不改变主 workspace 或内核依赖。Host 尚未接入，固定 Goose 与真实 Provider 验收没有被本回归替代。
