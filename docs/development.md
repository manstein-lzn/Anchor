# 开发指南

先读 [开发约定](../AGENTS.md)，从 [文档目录](README.md) 进入任务相关的当前契约；需要判断范围或验收时再读 [当前验收与待办](pilot-development-plan.md)。历史归档只在查证具体故障或决定时读取。

## 项目结构与依赖

`rust/` 是唯一 Cargo workspace，官方 Host、Kernel、工具、发行和开发检查共享 [Cargo.lock](../rust/Cargo.lock)。`apps/web/` 使用 React/TypeScript；依赖见 [package.json](../apps/web/package.json) 和其 lock。官方 Plugin 清单/Skill 位于 `plugins/`，Graph 示例位于 `examples/`。

运行与构建需要 Linux x86_64、Git、Bubblewrap、slirp4netns、Rust/rustup、Node.js 和 C 构建工具；系统工具清单见[使用指南](usage.md#安装与本地启动)。执行 `./scripts/dev.sh start` 自动准备锁定的前端依赖，增量构建 debug Host/relay、准备固定版本及补丁的 Goose，并更新 Web 资产；`setup` 只准备不启动，Goose 配方自动安装所需 musl target。默认平台源码启动按 [上手清单](../examples/starter.json) 一次性准备官方 Plugin 及工具，并经 Host API 导入示例 Graph；内容成为用户可编辑副本，已有同名内容与后续修改/删除不覆盖。不需要 `.env`，开发脚本与 Web 构建忽略旧环境文件。`scripts/dev.sh` 管理独立 Rust 开发根和本次服务 PID；拉取更新或修改后端后执行 `./scripts/dev.sh restart` 自动增量构建并加载，不因文档变化或仅验证而重启用户服务。

## 工作节奏

`dev.sh` 的 5173 入口统一转发到 Host，提供已构建的 WebUI；前端源码热更新单独使用 `npm --prefix apps/web run dev -- --port 5174`，不能替换试用入口，避免浏览器保留的开发依赖缓存因重建失效。

先确定归属层、事实所有者和小契约，交付可运行的垂直切片。Agent loop、模型历史与 compaction 复用 Goose；Graph、Run、Artifact、Sandbox、Library 和 Session/Turn 由 Anchor 保持唯一事实。

独立且契约稳定的工作包可在隔离 worktree 中并行，一个路径只保留一个实际编辑者。主轨负责共享 manifest、集成、review 和验收；获得足够证据后直接集成，不反复等待报告。变更应聚焦，避免为形式化拆分建设额外平台。

工作区可能有多条工作流的未提交改动，不 reset、checkout 丢弃或 clean；提交只含本次实际改动。`.local/`、旧环境、密钥、日志和运行历史不是清理对象，不因源码或示例迁移被覆盖。更新示例不会自动迁移用户 Graph。

## 验证

开发时先跑相关 crate/测试子集；垂直切片或跨模块语义完成后再执行一次必要的整体回归。复用同一 `CARGO_TARGET_DIR`，减少重复构建。普通文档与低影响可逆改动检查链接/格式即可，不增加镜像式测试。

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-devtools --locked
scripts/test.sh --workspace --all-features --all-targets --locked -- --test-threads=8
cargo +stable clippy --manifest-path rust/Cargo.toml --workspace --all-features --all-targets --locked -- -D warnings
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check
npm --prefix apps/web test
npm --prefix apps/web run build
```

`scripts/test.sh` 把测试临时目录放到 `XDG_RUNTIME_DIR` 的 tmpfs 上再调用 `cargo +stable test`：Host 的存储每次持久写都 fsync，磁盘上这部分占了 Host 套件大半时间（217 秒对 56 秒）。不用 `/dev/shm`，因为沙箱测试保留了 `/dev`；没有 tmpfs 时脚本提示并退回普通临时目录。

已缓存依赖时可加 `--offline`，缓存缺失要如实报告。Goose 条件集成用例不在普通 `cargo test` 中自动执行，必须通过 [确定性 Graph 回归](runtime-contract-tests.md) 显式选择；缺少 Goose、浏览器或外部配置与通过是不同结果。

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
  cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools --locked -- \
  regression fixture
```

`ANCHOR_GOOSE_BINARY` 指向 [构建脚本](../scripts/build-goose-acp.sh) 产出的 Anchor 自建 lean Goose ACP 二进制；该脚本固定上游源码、补丁及 musl 工具链，按 lock 编译并校验静态 ELF；源码启动自动使用本机产物摘要，无跨机器二进制 pin。

浏览器测试使用 Node 本地 Provider 与实际 Rust Host/Goose；执行方式见回归指南。release 包用 [原生候选回归](rust-production-candidate.md) 检查。大型 RSI、深度研究、周报和真实业务服务按低频内容验收单独运行。

测试报告只写实际命令、结果和证据，区分失败、跳过、未配置、未覆盖与真实模型请求。并发超时先独立复跑受影响用例，只有新的失败或未决风险才扩大验证，不重复跑已通过全量。

### 文本措辞检查

宿主使用的模型网关按原始子串拦截分级词，整条 400 拒绝且重试无效。中文没有词间空格，前一个词的末字会和后一个词的首字拼出这些词：「拒」「绝」+「密」（拒绝 + 密钥/密码）、「机」「密」（随机 + 密钥、开机 + 密码、手机 + 密码）、「秘」「密」（神秘 + 密码）。中文总结、文档、提交信息、报告和示例提示词都要避免。

处理方式分两类：跨词拼接改写措辞（「拒绝该密钥」「不接受密钥」「至少32字节、随机生成的密钥」「开机口令」「手机解锁码」），真词用同义说法（「保密的字段」「凭据字段」）；通用做法是在首字和「密」之间插入一个字或分隔符。规则见 [开发约定](../AGENTS.md)。

```sh
scripts/check-sensitive-wording.sh          # 受版本控制的工程文本，覆盖上述三类连写
scripts/check-sensitive-wording.sh --all    # 另含未跟踪的工程文件，仍跳过 .local/ 等运行状态
```

脚本自身用转义拼接模式，检查脚本、规则文本和测试 fixture 都不要写入连写原文；新增词表项时同步扩展脚本里的 term 列表。采集到的外部历史在 [周报](weekly-work-report.md) 的证据投影中按同一规则改写（`anchor-rsi` 的 `redact::text`），因此引用原文须回查原始文件。网关侧的匹配规则建议见 `.local/sensitive-word-rule/rule.md`（分词判定，跨词拼接放行、真词仍拦）。

## 文档归属

| 信息 | 维护位置 |
| --- | --- |
| 长期约定 | `AGENTS.md` |
| 产品方向、职责和不变量 | [产品架构](product-architecture.md) |
| 当前实现与接口 | [当前架构](architecture.md) 及其链接的专项契约；直接替换过期描述 |
| 安装和操作 | [使用指南](usage.md)、[部署指南](rust-production-deployment.md) |
| 当前结论、未关闭事项和证据入口 | [当前验收与待办](pilot-development-plan.md)；更新已有条目，不另写推进流水 |
| 重要架构决定的原因 | 短决策记录；明确有效、废弃或被替代的状态 |
| 命令、讨论、修复过程与原始输出 | 提交/PR、任务记录或验收产物；文档只链接必要证据 |
| 已完成计划、旧设计与调研 | [历史归档](archive/README.md)；退出开工入口 |

同一事实只维护一个权威位置，其他文档链接过去。交付时更新行为、结论和未覆盖边界，移走已完成计划，替换或删除过期描述；不把修正追加在旧结论后面。未通过真实 Provider 的能力不得写成真实端到端完成。

证据需要说明源码/二进制身份、实际命令和结果；`.local/`、`/tmp/` 的原始输出仅本机可用时如实标明，保留可复跑脚本或稳定产物链接，不为此把全部临时文件入库。历史证据不补签新构建或新部署。

文档 review 检查读者能否快速找到当前答案、同一事实是否重复、旧内容是否该替换/移出，以及未完成事项是否仍可定位。按任务控制必读信息量，不用拆文件或追加新规则掩盖重复。
