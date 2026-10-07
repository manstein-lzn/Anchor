# Goose ACP 验证分支

日期：2026-10-06。分支：`codex/goose-acp-spike`。这是候选 Runtime 验证，不是生产切换或替换 io-harness 的架构决定。

同日后续用户已决定统一迁移 Goose，并暂不实施预算控制，见 [Goose-only Runtime 迁移决定](goose-runtime-migration.md)。本文保留 A112 的实际验证与限制，不将方向决定回填为 Goose 产品已完整验收；当前实现仍是显式 spike。

## 本轮结论

真实 Goose v1.53.0 已在小型受控 Graph 中经过 Anchor Host、共享 GraphRunner、AgentNode 执行端口、HTTP MCP、现有 ToolPort/Bubblewrap、结构化完成与 Artifact 提交。没有将 Goose 当作模型 transport 嵌套到 io-harness loop，也没有另建 Graph Runner。

这证明 ACP 单节点接入和零真实模型的执行回归可行；不证明 Goose 已对齐所有 Anchor 能力、可以替换生产内核，或未知工具副作用已经能安全继续。本轮没有真实外部 Provider、业务系统、发布、发送、生产数据或密钥。

## 实现与事实归属

- `rust/anchor-runner-host/src/goose_acp.rs`：显式 opt-in 节点适配器，绑定 invocation、二进制 SHA256、Goose session 与完成事实；未决 invocation 保留事实并拒绝自动重放。
- `rust/anchor-runner-host/src/goose_acp/transport.rs`：有界 ACP stdio JSON-RPC，请求 deadline/取消、权限请求拒绝、未知 fs/terminal 请求拒绝、进程组清理；stderr 不作为协议或回显凭据。
- `rust/anchor-runner-host/src/goose_acp/bridge.rs`：将已有 Host ToolPort 暴露为唯一 `anchor` MCP，额外提供 `final_result`；本地模型 proxy 计数并机械拒绝同响应中混合完成/业务调用及完成后的业务工具。工具 handler 取消后等待实际结果，有界收敛失败或进程清理失败不能发布 Completed。
- Graph、Run、route、Artifact、输入快照和业务工作区仍由 Anchor 持有；Goose 原生会话 SQLite 位于 `work/.goose-process/<invocation hash>/data/sessions/`，不是 ACP 展示流自造的恢复日志。
- 外层调用事实/evidence 位于 `state/goose-acp-spike/`；业务文件仍在原节点工作区，不把 Goose 的私有数据混入业务 Artifact。

默认后端仍为 io-harness。选择不同后端须使用不同 state root；不能静默把既有 io-harness/Goose invocation 当成另一后端的新节点执行。

## 权限与当前限制

Goose Runtime 为访问本地模型 proxy/MCP 使用共享网络，必须由宿主显式设置 `ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1`。**Bubblewrap 的 `--share-net` 不是仅允许 loopback 的 OS 防火墙。** 本片以假凭据、本地服务、禁用 developer、精确 extension 选择及无 fs/terminal 回调限制执行面；业务 `anchor_run` 仍按 Graph 的 network 与宿主权限执行。对任意 Goose 程序的进程级 outbound 隔离尚未验收，不能据此部署不可信 Runtime。

模型上游只接受无凭据、无路径的显式 HTTP loopback IP，禁代理和重定向。loopback URL 本身不能证明真实模型调用为零：Runtime evidence 的 `real_model_calls` 为 null；本轮零调用结论来自测试拥有的 scripted Provider、真实采集请求和脚本消费校验。

本片只接固定版本、固定 fixture model、普通无 Plugin/媒体/跨轮会话的 AgentNode。未知 model、精确累计请求预算、Plugin、图片与会话续接显式拒绝，不降级到旧后端。模型响应以最多 16 MiB 有界缓冲供完成协议校验；实时摘要/SSE 行为不在本轮证据内。取消/工具收敛清理最多额外等待三秒，不承诺已发生的副作用回滚。

尚未验证：Goose 未完成节点的现场核查/安全继续、compaction、feedback/fanout/join/Graph call 的 Goose 组合、提问续答、媒体、Plugin/OAuth、部署分发、保留/删除清理、多用户长会话、精确累计预算、真实 Provider、生产迁移和切换。原 io-harness 路径的回归通过不能替这些 Goose 边界计账。

## 二进制基线

- 官方 release：`v1.53.0`，2026-10-02 发布。
- Linux musl release archive SHA256：`4124f3b56dcebf1f396ddddaa66d68cf710318dcf947d8c81de6eb5866af11d7`。
- 解包的 Goose binary SHA256：`bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340`。
- 本轮文件：`/tmp/anchor-goose-acp-spike/goose`；官方 release digest、版本、startup probe 与源文件身份保留在 `/tmp/anchor-goose-acp-spike/evidence/`。
- 最终集成测试实际 Host binary SHA256：`abac8efff5df1b82a70feb01a2b8007b39dea0e6bd89a52e79d5b6373b5a3960`。证据按每个实际执行副本记录 hash，不靠后来构建的默认路径判断身份。

## 重跑

需要 Rust、Bubblewrap 与 sqlite3 CLI。Goose 不自动下载、编译或回退；真 Goose 用例默认 ignored，显式执行时缺少/错配 binary 会失败，不能把跳过写成通过。环境及配置/数据根由 fixture 隔离，不加载项目 dotenv。

```bash
export CARGO_TARGET_DIR=/tmp/anchor-production-parallel-FE8ZQu/host-target
export CARGO_BUILD_JOBS=2
export ANCHOR_GOOSE_BINARY=/tmp/anchor-goose-acp-spike/goose
export ANCHOR_GOOSE_BINARY_SHA256=bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340
export ANCHOR_TEST_EVIDENCE_ROOT="$(mktemp -d /tmp/anchor-goose-evidence-XXXXXX)"
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --test goose_acp -- --include-ignored --nocapture
```

fixture 给子 Host 显式设置 `ANCHOR_RUNNER_AGENT_RUNTIME=goose-acp-spike`、共享网络授权、固定模型、本地 Provider endpoint 和二进制 hash。不建议在普通生产启动环境设置这些参数。

本轮还实际执行：

```bash
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --bin anchor-runner-host goose_acp -- --nocapture
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-runner-host \
  --test goose_acp --test runtime_contract --test native_plugins \
  -- --include-ignored --nocapture
cargo +stable clippy --manifest-path rust/Cargo.toml \
  -p anchor-runner-host --all-targets -- -D warnings
cargo +1.92.0 fmt --manifest-path rust/Cargo.toml --all -- --check
```

构建/测试均使用上面的 `/tmp` target 和两并发 jobs。没有执行整套 workspace tests、Python/Web 或 Host unit 全量；既有五项 Python-tool/环境失败没有本轮关闭证据。

## 实际验收

最终集成命令：Goose 9 passed（7 场景与 2 fixture 自检）、原 `runtime_contract` 22 passed、`native_plugins` 2 passed，均 0 failed/ignored/filtered。Goose 场景中六个启动实际 Goose loop，一个验证缺少宿主网络授权时拒绝且零模型请求；七个场景合计 20 次本地 scripted Provider 请求，真实模型请求 0。Goose suite 运行 19.49 秒；这是本机并行受控测试时间，不是模型性能 benchmark。

| 场景 | 核查事实 |
| --- | --- |
| 工具 → 完成 → route → Op → Artifact → 重启 | 真实工具结果进入下一模型请求；两个节点记录、workspace bytes、Artifact hash 与上游只读读取；已完成重启没有新 Provider 请求或副作用 |
| 非法 route 修正 | completion schema 和真实错误反馈后修正；错误分支不提交 |
| 命令/绝对路径越权 | 未授权命令 `not_executed`，路径写入被真实 Bubblewrap 拒绝，外部文件不存在；实际模型工具列表严格只有两项 Anchor 工具 |
| 取消与重启 | 控制请求、执行收束与终态；native history 保留且不新增 Provider 请求 |
| 完成后调用业务工具 | MCP 拒绝，workspace 无非法结果，不发布节点 Artifact |
| 外部效果发生后杀 Host | 独立持久外部计数器为一次；native ToolRequest/ToolResponse 已保存；重启保留原 invocation/会话/文件，拒绝自动重放，Provider 请求不增加。**只证明 Anchor spike fail-closed，不证明 Goose 原生安全恢复** |
| 无共享网络授权 | 在 Host admission 拒绝，无 Provider 请求 |

测试读取 Goose 原生 SQLite conversation，将真实 ToolRequest/ToolResponse ID、工具名和结果配对，不只检查数据库文件存在或 ACP 文本。边界定向 unit suite 最终 23 passed/0 failed/0 ignored、247 filtered，运行 3.00 秒，不是 Host unit 全量；包括 frame/notification 限额、RPC 错配、权限拒绝、deadline/取消、后代进程清理、完成 schema、流式工具批次解析以及真实 MCP handler 的关闭等待/未收敛反例。Host `clippy --all-targets -D warnings` 与 workspace `fmt --check` 最终通过。

最终场景证据：`/tmp/anchor-goose-acp-spike/evidence/final-integrated/187081/*/evidence.json`。原默认 fixture 同一 evidence root 有 25 份场景 JSON。完整集成日志：`/tmp/anchor-goose-acp-spike/evidence/final-integration.log`；边界 unit 日志：`/tmp/anchor-goose-acp-spike/evidence/final-unit.log`。

首次集成的越权用例因断言词汇不匹配失败：实际已返回 `not_executed/not authorized`，不是权限放行；修正后定向与最终回归通过。新增关闭测试曾因 reqwest 版本不匹配而编译失败，改用固定 rmcp 的原生构造器；随后因重复 Bearer 前缀得到 401，修正测试配置后通过。上述失败不计作功能通过，最终记录以实际终轮结果为准。

## 下一判断

继续保留现有生产路线。若继续评估，优先验证未完成节点的现场核查/安全继续，以及一个 Plugin 或 compaction/反馈组合；这些结果比再跑大型业务图更能决定 Goose 是否减少剩余工作。ACP 接入可行与 Goose 比 io-harness 更适合，是不同结论。
