# Plugin 渐进式披露：2026-10-13 验收证据

对应 [归档专项计划](../../docs/archive/development-2026-10-13/plugin-progressive-disclosure-plan.md) 和 [历史台账](../../docs/archive/development-2026-10-13/pilot-development-plan.md) A222。以下是当时的实际运行结果和部署边界，不代表当前部署身份。

## 保留文件与运行身份

- [real_provider.py](real_provider.py)：可复用的隔离真实 Provider 验收脚本。
- [final-real-provider.json](final-real-provider.json)：最终二进制真实验收概要、独立 MCP 调用及清理结果，不含模型凭据。
- [checks.json](checks.json)：主 Agent 实际检查命令、退出结果和 all-targets 分 target 明细。

最终真实验收根：`.local/plugin-disclosure-acceptance-20261013/real-da18c817d0174c14/`。原始脱敏证据在该根的 `pilot-events.json`、`pilot-turn.json`、`pilot-messages.json`、`node-native-evidence.json`、`run.json`、`artifact.json`、`evidence.json` 和 `host.log`；实际 MCP 调用 JSONL 在隔离 workspace 中。起始 HEAD 为 `dba4d725bc40afff50b30ccc1018b48b027ad2f0`，验收包含本任务工作树实现。

| 项目 | 最终身份/结果 |
| --- | --- |
| Host SHA256 | `acb782f176db656dc50c0490fe62163cdee1a02423b398132a010bbbd8eb34fd` |
| 固定 Goose SHA256 | `71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7` |
| 真实模型 | `deepseek-flash` |
| Library / 挂载 | 1,000 Plugin；Pilot 10，Node 1,000 |
| 目标 MCP | 一个 Python stdio server，1,000 无关工具 + `produce_report` |
| 实际任务 | 发现 Plugin、读取概览/说明、正确 Skill 和随机 token 资源、取得完整 schema、真实调用；Node 保存产物后原生完成 |
| 结果 | Pilot/Node completed；独立 MCP 日志、随机 token、total=42、JSON 产物、实际目标 trace 与回执一致；无关 Skill 正文未加载 |
| 清理 | Session DELETE 200，Host exit 0，listener closed，无 panic 或存活所属子进程 |
| 既有现场 | `.env` 和共享 Python 解释器字节不变；当前部署、8077/5173 未更新 |

## 实际检查命令

主 Agent 从仓库根执行，均退出 0：

```bash
PATH=/home/mansteinl/.cargo/bin:$PATH cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host -p anchor-graph-host --all-targets --locked --offline -- --test-threads=4
PATH=/home/mansteinl/.cargo/bin:$PATH cargo test --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host --locked --offline disclosure -- --nocapture
PATH=/home/mansteinl/.cargo/bin:$PATH cargo build --manifest-path rust/Cargo.toml -p anchor-runner-host --bin anchor-runner-host --locked --offline
PATH=/home/mansteinl/.cargo/bin:$PATH cargo clippy --manifest-path rust/Cargo.toml -p anchor-runner-host -p anchor-graph-host --all-targets --locked --offline -- -D warnings
PATH=/home/mansteinl/.cargo/bin:$PATH cargo fmt --manifest-path rust/Cargo.toml --all -- --check
python3 real_provider.py
```

all-targets：Host 28 targets，869 passed / 0 failed / 86 ignored；GraphHost 2 targets，41 passed / 0 failed。合计 **910 passed / 0 failed / 86 ignored**，包含不同 target 对共享 fixture 的重复运行。默认 ignored 不因此视为已执行。全量通过后 Clippy 首轮报告 `manual_strip`，仅把 frontmatter 的手动前缀切片改为等价 `strip_prefix`；收尾后 20 项 disclosure、build、最终 Clippy、fmt 和最终真实 Provider 通过。没有声称该等价收尾后又跑一次全量。

主 Agent 日志：`/tmp/anchor-plugin-disclosure-root-{tests,contract,build,clippy-final,real-final}.log`。无 Web 改动，未运行 Web 检查。

实现者从 `rust/` 执行的定向检查：

```bash
cargo test -p anchor-runner-host --bin anchor-runner-host -- --nocapture disclosure plugin_management_metadata_and_reads_share_the_library_publication_guard pilot_tools::frozen_mounts_tests
cargo test -p anchor-runner-host --bin anchor-runner-host goose_acp::bridge::tests:: -- --nocapture
```

分别 **28 passed** 和 **11 passed**；日志 `/tmp/anchor-plugin-disclosure-final-contract.log`、`/tmp/anchor-plugin-disclosure-bridge.log`。

以下 7 次 Cargo 调用实际使用固定 Goose 及确定性本地 Provider，只替换模型传输，均通过。从 `rust/` 执行，每行设置 `ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose`：

```bash
ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose cargo test -p anchor-runner-host --test goose_acp native_goose_disclosed_tools_round_trip_keeps_the_completion_contract -- --ignored --nocapture
ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose cargo test -p anchor-runner-host --test goose_plugin_disclosure -- --ignored --nocapture
ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose cargo test -p anchor-runner-host --test goose_media native_goose_mixed_media_preserves_blocks_vision_wire_receipt_and_artifacts -- --ignored --nocapture
ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose cargo test -p anchor-runner-host --test goose_elicitation goose_ask_user_validates_answer_and_preserves_one_turn_and_sse_history -- --ignored --nocapture
ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose cargo test -p anchor-runner-host --test goose_elicitation -- --ignored --nocapture goose_graph_delete_requires_native_true_confirmation goose_graph_delete_native_
ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose cargo test -p anchor-runner-host --test goose_compaction native_goose_entry_compaction_rereads_readonly_plugin_and_workspace_before_graph_completion -- --ignored --nocapture
ANCHOR_GOOSE_BINARY=/home/mansteinl/Anchor/.local/rust/bin/goose cargo test -p anchor-runner-host --test native_plugins native_goose_reaches_a_hidden_plugin_tool_through_disclosure -- --ignored --nocapture
```

删除确认一行包含 true/false/decline/cancel 四项，合计 **10 个实际 Goose 场景**。原始证据根 `/tmp/anchor-goose-acp-spike/evidence/integration/`：

| 场景 | evidence.json 相对路径 | 日志后缀 |
| --- | --- | --- |
| Node 披露/完成 | `528792/goose-native-disclosure/evidence.json` | `goose-node` |
| Pilot Plugin 披露 | `534113/goose-pilot-plugin-disclosure/evidence.json` | `goose-pilot` |
| 混合媒体 | `537767/goose-media-mixed-vision/evidence.json` | `goose-media` |
| 原生提问 | `538412/goose-ask-user/evidence.json` | `goose-ask` |
| 删除 true/false/decline/cancel | `541218/goose-delete-{true,false,decline,cancel}/evidence.json` | `goose-confirm` |
| compaction 后重读 | `542438/goose-compaction-plugin-workspace/evidence.json` | `goose-compaction` |
| 本地 WeCom mock | `546288/goose-native-wecom-disclosure/evidence.json` | `native-wecom` |

日志路径 `/tmp/anchor-plugin-disclosure-<后缀>.log`。部分 native 场景对应格式整理前编译的测试二进制；最终 WeCom 与 28 项定向在格式整理后运行，主 Agent 最终真实验收绑定上表明确的 Host 身份，不混作单一二进制全量验收。

## 规模预算与失败收敛

定向测试的合成 ToolDefinition JSON 序列化测量：

| 业务工具数量 | 全部定义 | 初始披露定义 |
| --- | ---: | ---: |
| 10 | 4,881 B | 3,223 B |
| 100 | 49,071 B | 3,223 B |
| 1,000 | 493,671 B | 3,223 B |

这是字节证据，不是 tokenizer 计数或整个 Pilot prompt 成本。目录条数/字节、精确冷门项、长描述尾部、多词中文、Skill/资源选择、UTF-8 digest 分页、配置保密、权限变更及发布 guard 有定向回归。

早期 native 失败如实保留：89 MB debug MCP fixture 复制件触发客户端既有 5 秒 mount 超时，仅 strip 测试复制件，未改原 binary/全局超时；旧媒体/压缩/WeCom fixture bundle 携带 Plugin 快照被现有 id-only 契约拒绝，改为独立 checkout 后经正式 Library 安装；试图删除 Pilot 自己 Graph 被正确拒绝，改独立目标；本地 mock 使用 127.0.0.1 被 `disable-host-loopback` 阻断，复用现有 routable local-network fixture。以上修正后场景通过，没有扩大管理 loopback 或修改生产配置。

## 重用前提与未覆盖范围

真实验收脚本需要已构建 Host、上表固定 Goose、已有 relay/network helper，以及 `.env` 中可用模型和共享 Python 解释器配置。脚本只按白名单解析配置数据，不 source 配置；创建私有隔离根和随机端口，预置 fixture Library，显式 `ANCHOR_NODE_TOOL_DISCLOSURE=1`，复制并记录 Host identity，调用真实模型，清理 Session 和 Host。它会产生真实模型请求和隔离本地文件，不发送外部业务消息，不重启当前部署。概要脱敏保留凭据外的证据。

真实场景只有一个目标 MCP；其他 999 个 Plugin 只提供目录/说明。不据此宣称 1,000 MCP server 启动或并发连接通过。没有复验公共市场获取、Plugin 安装/authoring、真实外部业务、MCP 懒连接或 Code Mode。当前部署仍是 PID 184037、Host SHA256 `984578f85d9bc460a8868f53236f8e99c04bc61d69f616993adcb67cb42d2f15`，配置 `ANCHOR_NODE_TOOL_DISCLOSURE=0`，本次没有更新。
