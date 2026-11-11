# 开发指南

先读 [开发约定](../AGENTS.md)，从 [文档目录](README.md) 进入任务相关的当前契约；需要判断范围或验收时再读 [当前验收与待办](pilot-development-plan.md)。历史归档只在查证具体故障或决定时读取。

## 项目结构与依赖

`rust/` 是唯一 Cargo workspace，官方 Host、Kernel、工具、发行和开发检查共享 [Cargo.lock](../rust/Cargo.lock)。`apps/web/` 使用 React/TypeScript；依赖见 [package.json](../apps/web/package.json) 和其 lock。官方 Plugin 清单/Skill 位于 `plugins/`，Graph 示例位于 `examples/`。

## 源码启动

这里只适用于开发者；下载原生安装包试用请看[使用指南](usage.md)，目标机不需要下面的构建工具。

需要 Linux x86_64、Git、Bubblewrap、slirp4netns、Rust stable（通过 rustup 安装）和 Node.js 20.19+ 或 22.12+ / npm。构建固定 Goose 还需要 C 编译工具、curl、patch、rsync、binutils、util-linux 和 coreutils。Debian/Ubuntu 可先安装系统工具：

```sh
sudo apt-get install build-essential curl git bubblewrap slirp4netns patch rsync binutils util-linux coreutils
```

Rust/rustup 和符合上述版本的 Node.js/npm 需预先安装。开发脚本先实际探测隔离 namespace 与私有 PID namespace 的 `/proc` 挂载能力，Host 启动时也独立检查；失败时停止并报告原因，不使用共享宿主网络绕过。脚本不会自动使用 sudo 或修改系统策略；模型调用和 Plugin 网络权限分别配置。

```sh
git clone https://github.com/manstein-lzn/Anchor.git
cd Anchor
./scripts/dev.sh start
```

打开 <http://127.0.0.1:5173>，按[使用指南](usage.md)在「设置 → 模型服务」保存 default Flash，再回到 Pilot 对话。5173 统一转发到 <http://127.0.0.1:8077> 的 Rust Host，提供已构建的 WebUI，不加载 Vite 开发依赖。首次 `start` 自动执行锁定的 `npm ci`、增量构建 debug Host/relay、准备 [Goose 本机产物](#固定-goose-与构建缓存)，并构建缺少或已更新的 Web 资产；缺少系统工具时提示具体缺项。

```sh
./scripts/dev.sh setup     # 准备依赖、Plugin 和构建产物，不启动服务
./scripts/dev.sh restart   # 更新代码后增量构建并重启
./scripts/dev.sh status
./scripts/dev.sh stop
```

`status`/`stop` 不下载或构建，只操作脚本持有的 PID。启动前检查端口与当前运行任务；重启会中断活动节点，之后应查看持久记录并继续。不因文档变化或仅验证而重启用户服务。

默认产物位于 `rust/target/debug`（或 `CARGO_TARGET_DIR/debug`），不会自动选取旧 release 产物；显式指定自定义 Host/relay 时，两者都必须已可执行。路径、端口等覆盖只取显式进程设置，是开发/部署输入，不是模型、Plugin 或渠道的产品配置源，见[开发脚本](../scripts/dev.sh)和[操作员进程设置](rust-production-deployment.md#operator-process-settings)。开发脚本与 Web 构建不加载 `.env`，也不执行、覆盖或删除旧环境文件。

开发数据默认独立保存在 `.local/rust/`：`catalog/`、`state/`、`workspaces/` 和 `library/` 分别保存可编辑 Graph、运行与会话事实、工作区和 Plugin，`dev/` 保存脚本 PID 与日志。现有 `.local/demo`、旧环境和运行历史不会被自动迁移或清理。

默认源码启动按 [starter 清单](../examples/starter.json) 一次性准备官方 Plugin、工具和依赖，Plugin 仍经 Library 安装器发布。`setup` 不启动 Host 或导入 Graph；`start`/`restart` 在脚本管理的 Host 就绪后，才经现有 API 导入示例 Graph。不自动运行 Graph、调用模型、连接业务账号或启用渠道。

`.source-starter.json` 只记录源码启动器的初次准备，不是 Graph/Plugin 的事实来源。导入内容成为用户可编辑副本，已有同名内容、用户后续修改和删除不被覆盖或重新引入；显式指定 entry bundle、自定义 Host 或外部服务时跳过源码导入。示例用途与配置条件见[上手示例](../examples/README.md)，不能据此推断原生运行时包也附带全部业务示例。

前端源码热更新单独使用 `npm --prefix apps/web run dev -- --port 5174`，不能替换试用入口，避免浏览器保留的开发依赖缓存因重建失效。

## 固定 Goose 与构建缓存

源码启动通过[构建脚本](../scripts/build-goose-acp.sh)固定 Goose v1.53.0、仓库补丁、上游 Cargo.lock 和 musl 工具链，按 `--locked` 构建静态 x86_64 musl lean ACP-only 二进制并校验静态 ELF；不含完整 CLI、调度器、bundled MCP 与平台扩展。配方自动安装固定工具链的 musl target 和已校验的交叉编译器，首次需要联网，后续复用产物。

默认输出为 `.local/bin/goose`；也可用 `scripts/build-goose-acp.sh --output "$PWD/.local/bin/goose"` 手动重建。日常启动的 `--ensure` 按配方输入标记和本机输出校验和复用缓存；输入更新或文件缺失、损坏则重新构建。`.sha256` 和 `.build-record.json` 是可重建的构建记录，不要求各机器产物摘要相同；源码与交叉编译器的下载仍核验固定摘要。

开发脚本自动把实际产物摘要传给 Host，由 Host 独立核验，并定位 relay 隔离模型控制网络；源码用户无需设置 `ANCHOR_GOOSE_BINARY_SHA256`。可显式覆盖 `ANCHOR_GOOSE_BINARY`、`ANCHOR_GOOSE_RELAY_BINARY` 等路径，脚本会在所选 Goose 输出路径准备本机产物。同 invocation 的身份绑定仍遵守[恢复契约](architecture.md#goose-与恢复)，不会因使用本机产物而放宽。

发行 builder 与固定 Goose 回归另有固定二进制摘要要求，本机源码构建成功不代表该产物可用于发行打包或这些回归。完整平台安装器核验随包工具，不要求最终用户构建 Goose；分别见[发行 builder](../rust/anchor-distribution/README.md)与[标准回归命令](runtime-contract-tests.md#标准命令)。

## 系统 Plugin 维护

原生工具已有候选打包入口，从仓库根目录执行：

```sh
cargo build --manifest-path rust/Cargo.toml -p anchor-network-tools --release --locked
rust/target/release/anchor-network-tools package-plugin /absolute/path/to/candidate/network
```

父目录 `/absolute/path/to/candidate` 必须已存在，目标 `network` 目录必须不存在；设置了 `CARGO_TARGET_DIR` 时，第二条命令改用该 target 下的 `release/anchor-network-tools`。[打包实现](../rust/anchor-network-tools/src/package.rs)输出 `plugin.json`、`skills/network/SKILL.md` 和 `bin/anchor-network-tools`，不覆盖已安装 Plugin。结果只是候选，不自动挂载或授予网络权限。

`network`、`graph-tasks`、`feishu`、`wecom` 受系统身份保护，不能使用普通 `install --replace-existing` 或 Web/Pilot 普通安装路径发布。完整发行首次安装从已验证 inventory 引导；操作员维护复用 [Library](plugins.md#系统插件与普通插件) 的专用命令：

```sh
anchor-library --root /absolute/path/to/library inspect-system --id network
anchor-library --root /absolute/path/to/library install-system \
  --id network --directory /absolute/path/to/candidate/network --digest <reviewed-candidate-digest>
anchor-library --root /absolute/path/to/library upgrade-system \
  --id network --directory /absolute/path/to/candidate/network \
  --digest <reviewed-candidate-digest> --existing-digest <reviewed-installed-digest>
```

`install-system` 只接纳缺失项，`upgrade-system` 同时核对已安装代码和候选摘要，在原 Library 发布锁内执行；旧版本损坏或并发变化不能静默覆盖。系统升级属于操作员/发行维护，不授权 Agent 自行升级 Host、扩大权限或部署。代码更改、确定性回归、真实 Provider/渠道验收和生产切换是分开的事实。

## 工作节奏

先确定归属层、事实所有者和小契约，交付可运行的垂直切片。Agent loop、模型历史与 compaction 复用 Goose；Graph、Run、Artifact、Sandbox、Library 和 Session/Turn 由 Anchor 保持唯一事实。

独立且契约稳定的工作包可在隔离 worktree 中并行，一个路径只保留一个实际编辑者。主轨负责共享 manifest、集成、review 和验收；获得足够证据后直接集成，不反复等待报告。变更应聚焦，避免为形式化拆分建设额外平台。

工作区可能有多条工作流的未提交改动，不 reset、checkout 丢弃或 clean；提交只含本次实际改动。`.local/`、旧环境、密钥、日志和运行历史不是清理对象，不因源码或示例迁移被覆盖。更新示例不会自动迁移用户 Graph。

### Kernel 修改边界

当前冻结基点 C1 为 `9a5fa57`，依据 [P1 最终冻结记录](../experiments/harness-hub/p1/verification.md#冻结为-c12026-10-20)。除非另有经独立审查授权的 Kernel 变更，以下差异必须为空：

```sh
git diff 9a5fa57 -- rust/anchor-kernel
```

## 验证

开发时先跑相关 crate/测试子集；垂直切片或跨模块语义完成后再执行一次必要的整体回归。同一工作流复用 `CARGO_TARGET_DIR`，独立并行构建使用不同 target，不覆盖正在采证的二进制。普通文档与低影响可逆改动检查链接/格式即可，不增加镜像式测试。

```sh
cargo +stable test --manifest-path rust/Cargo.toml -p anchor-devtools --locked
scripts/test.sh --workspace --all-features --all-targets --locked -- --test-threads=8
cargo +stable clippy --manifest-path rust/Cargo.toml --workspace --all-features --all-targets --locked -- -D warnings
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check
npm --prefix apps/web test
npm --prefix apps/web run build
```

`scripts/test.sh` 把测试临时目录放到 `XDG_RUNTIME_DIR` 的 tmpfs 上再调用 `cargo +stable test`，减少 Host 持久写 fsync 的磁盘开销。不用 `/dev/shm`，因为沙箱测试保留了 `/dev`；没有 tmpfs 时脚本提示并退回普通临时目录。

已缓存依赖时可加 `--offline`，缓存缺失要如实报告。Goose 条件集成用例不在普通 `cargo test` 中自动执行，按[标准回归命令](runtime-contract-tests.md#标准命令)准备固定 Goose、relay 和测试 helper，再显式选择套件；缺少 Goose、浏览器或外部配置与通过是不同结果。

浏览器测试使用 Node 本地 Provider 与实际 Rust Host/Goose；执行方式见回归指南。release 包用 [Release 与 Source-Free 候选](runtime-contract-tests.md#release-与-source-free-候选)检查。大型 RSI、深度研究、周报和真实业务服务按低频内容验收单独运行。

测试报告只写实际命令、结果和证据，区分失败、跳过、未配置、未覆盖与真实模型请求。并发超时先独立复跑受影响用例，只有新的失败或未决风险才扩大验证，不重复跑已通过全量。

### 文本措辞检查

宿主使用的模型网关按原始子串拦截分级词，整条 400 拒绝且重试无效。中文没有词间空格，前一个词的末字会和后一个词的首字拼出这些词：「拒」「绝」+「密」（拒绝 + 密钥/密码）、「机」「密」（随机 + 密钥、开机 + 密码、手机 + 密码）、「秘」「密」（神秘 + 密码）。中文总结、文档、提交信息、报告和示例提示词都要避免。

处理方式分两类：跨词拼接改写措辞（「拒绝该密钥」「不接受密钥」「至少32字节、随机生成的密钥」「开机口令」「手机解锁码」），真词用同义说法（「保密的字段」「凭据字段」）；通用做法是在首字和「密」之间插入一个字或分隔符。规则见 [开发约定](../AGENTS.md)。

```sh
scripts/check-sensitive-wording.sh          # 受版本控制的工程文本，覆盖上述三类连写
scripts/check-sensitive-wording.sh --all    # 另含未跟踪的工程文件，仍跳过 .local/ 等运行状态
```

脚本自身用转义拼接模式，检查脚本、规则文本和测试 fixture 都不要写入连写原文；新增词表项时同步扩展脚本里的 term 列表。采集到的外部历史在 [周报](weekly-work-report.md) 的证据投影中按同一规则改写（`anchor-rsi` 的 `redact::text`），因此引用原文须回查原始文件。

## 文档归属

| 信息 | 维护位置 |
| --- | --- |
| 长期约定 | `AGENTS.md` |
| 产品方向、职责和不变量 | [产品架构](product-architecture.md) |
| 当前实现、接口与仍有效的决定 | [当前架构](architecture.md) 及其链接的专项契约；保留必要理由，直接替换过期描述 |
| 安装和操作 | [使用指南](usage.md)、[部署指南](rust-production-deployment.md) |
| 当前结论、未关闭事项和证据入口 | [当前验收与待办](pilot-development-plan.md)；更新已有条目，不另写推进流水 |
| 命令、讨论、修复过程与原始输出 | 提交/PR 或验收产物；当前文档只链接必要证据 |
| 已完成计划、被替代的设计与调研 | [历史归档](archive/README.md)；不再作为当前契约或待办入口 |

同一事实只维护一个权威位置，其他文档链接过去；当前契约、统一验收待办台账和历史归档各司其职，不另开并行状态文档。交付时更新行为、结论和未覆盖边界，移走已完成计划，替换或删除过期描述；不把修正追加在旧结论后面。未通过真实 Provider 的能力不得写成真实端到端完成。

证据需要说明源码/二进制身份、实际命令和结果；`.local/`、`/tmp/` 的原始输出仅本机可用时如实标明，保留可复跑脚本或稳定产物链接，不为此把全部临时文件入库。历史证据不补签新构建或新部署。

文档 review 检查读者能否快速找到当前答案、同一事实是否重复、旧内容是否该替换/移出，以及未完成事项是否仍可定位。按任务控制必读信息量，不用拆文件或追加新规则掩盖重复。
