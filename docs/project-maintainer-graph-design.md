# 常驻工程 Graph：项目现场与执行边界（设计）

状态：整体设计未完成；第 2 片 Workspace 交接及第 3 片公共 envelope/环境坐标已实现，待真实验收。全部实现后按开发约定改写为当前契约并退出活跃入口。

## 目标

用户在 WebUI 给一个常驻 Graph 授权联网和 git 凭据，在企业微信里告诉它仓库地址和问题，Agent 自己完成 clone、配置环境、改代码、测试和 push。用户可以随时用新消息打断，改错了继续反馈修复。

用户已定（2026-11-01）：

| 决定 | 结论 |
| --- | --- |
| 首个验收仓库 | Anchor 本身（`origin` = `git@github.com:manstein-lzn/Anchor.git`） |
| push 权限 | 允许直接推主分支 |
| 项目现场寿命 | 跟着 Graph 走，Graph 改版本（含指令）后现场仍在 |
| 系统依赖 | 默认用 pixi（conda-forge），锁文件提交进项目 |

## 现状差距

1. **Graph 公共执行边界已实现，待真实验收。** `node_host/envelope.rs` 从宿主底线、ToolEnvironment、已挂载 Plugin 的挂载与网络授权、本次 local inputs 生成公共 envelope；节点准备与 `Op.run` 共用，不改变原挂载、网络、PATH 或 `/proc`。Plugin MCP、Pilot、开发环境及依赖构建仍保留各自边界，本片不迁。
2. **自装工具不可见。** `PATH` 固定为注册工具目录加 `/usr/bin:/bin`（`anchor-sandbox-bwrap` 的 `make_path`）。Agent 装到 `$HOME/.local/bin`、`$HOME/.cargo/bin` 的程序要写绝对路径才能运行。
3. **本机没配 Rust。** 节点准备经运行配置快照，把部署的 Rust 授权同时给 Graph 节点和 Pilot；本机部署此前没有设置 `ANCHOR_PILOT_RUSTUP`。
4. **规则没告诉 Agent。** `anchor_run` 的描述只列命令白名单，没写 `HOME=/workspace` 可写且持久、`/usr` 只读、没有 root、凭据只在 git 工具里。
5. **现场跨版本交接已实现，待真实验收。** 复用现有可信交接链，同一 owner、会话、Graph 来源（`bundle_source`）与同名 work 节点继承 Workspace，指令或结构改变不再切断现场；不新增资源键或 Graph 字段。Plugin 权限仍只在插件/联网改动兼容时继承（[企业微信契约](wecom-assistant.md#停止恢复与退役)）。
6. **环境坐标已实现，待真实验收。** `AgentConfiguration.envelope_sha256` 引用同一 facts 根的不可变规范描述，hub 可展示、读取并按该字段分组；旧记录缺失字段时摘要不变。现场指纹与 shell 首个程序名未做，`ToolCall.program` 对 `sh -c` 仍只记 `sh`。

已实测（2026-11-01）：本机 bwrap 0.4.0 在 bwrap 内再起 bwrap 成功，所以 Anchor 自己的沙箱测试能在项目现场里跑。pixi 本机未安装。

## 设计

### 1. 宿主只定三条边界，一处计算

| 边界 | 内容 | 谁定 |
| --- | --- | --- |
| 文件 | 可写：项目现场 `/workspace`（也是 `HOME`）。只读：系统目录、`/in`、`/plugins`、`/spill`、注册工具目录 | 宿主 |
| 网络 | 开或关，来自 `network` Plugin 授权 | 用户在 Graph 上授权 |
| 凭据 | 只注入对应 Plugin 的 MCP 进程；Agent 的 shell 看不到 | 用户在 Plugin 上授权 |

**公共 envelope 已实现，待真实验收。** `node_host/envelope.rs::invocation_envelope` 一处计算 Graph invocation 的沙箱授权、网络、工具环境与命令超时上限。`preparation.rs` 和 `execution.rs` 删除各自的工具环境、local inputs、Plugin 挂载叠加，NodeTools 从同一 envelope 接收授权。Plugin MCP 的原启动边界保持不变，未迁入命令 envelope。

规范 JSON 的键与集合排序，PATH 保留优先级；描述系统/工具/Rust/Plugin 只读来源与目的路径、可写沙箱路径、local inputs 目的类别、网络、设备、私有 PID namespace `/proc`、固定变量、两条命令路径的超时上限和拿到凭据的 Plugin MCP 进程所属 id。宿主生成的临时 launcher 来源用稳定名称表示；不记凭据值、Run/Workspace 来源路径、输入版本或 spill。Op 的 route helper、`ANCHOR_*` 变量及其 PATH 前缀，`anchor_run` 的 project/开发环境 overlay、命令别名归一化、直接 Rustup 安装命令的条件写授权与 spill 留在原路径，不进入公共摘要。Pilot 和依赖构建尚未迁入。

边界之内不再限制做法：命令白名单对项目现场不起额外作用（`sh` 已在白名单），不新增按命令的规则。

### 2. 边界内自由配置环境

- `PATH` 固定前置 `$HOME/.pixi/bin`、`$HOME/.cargo/bin`、`$HOME/.local/bin`。只有这一组固定目录，不做配置项。
- 宿主以注册工具目录提供固定版本的 pixi 和 Rust 工具链（rustup 代理），与固定版本 Goose 的做法一致。包和工具链缓存放在 `$HOME` 下，随现场保留。
- 项目的系统依赖写进项目里的 `pixi.toml`，`pixi.lock` 提交进仓库。没有 `sudo`、`apt`、`docker`。conda-forge 没有的系统包由 Agent 报告给用户，以后再考虑按项目的 rootfs。
- `anchor_run` 描述写明上面三条边界和这些目录约定。这项改动按 hub 的 A/B 门禁验证后才保留（上一次改 `anchor_edit` 描述的 P5 被 A/B 推翻）。

### 3. 现场跟着 Graph 走

Workspace 交接复用 `conversation.previous_run` 与 Session 的可信交接链，与 Goose 原生历史采用同一身份规则：同一 owner、同一 session、同一 Graph 来源（`bundle_source`）、同名 work 节点。旧实例退役后，Graph 指令或结构改变的新 Run 继续绑定原 Workspace 资源，不复制目录、不重新 clone，也不新建存储、资源键或 Graph 字段。

- work 节点改名、换 Graph 来源、换会话或 owner 是新现场边界；wait/reply 改名不切断同名 work 节点的现场，每环仍按各自实例校验身份、交接证书、退役事实与 digest。
- 单写者不变：沿用执行租约和发布/中断事实，旧执行者和本地工具进程退出且候选已结算后才能转交。连续重启跳过未准备现场的中间实例，不越过身份边界。
- Goose 原生历史规则不变，已支持跨退役与指令改变接续。Agent 开始工作时先核查现场（`git status`、`git log`）；Plugin 权限仍按插件/联网兼容规则继承，不随现场扩大。
- 已共享资源按既有保留与显式释放规则回收，删除来源 Run 不删除共享现场；其他节点仍默认使用独立 `(Run,node)` 现场，已交付 Artifact 保持固定版本。

### 4. 交付就是 git

- 推送走 git Plugin 的 `git_run`，凭据只在它的 MCP 里。Anchor 首个验收用一个有写权限的 deploy key，允许直接推 `master`。
- Agent 不部署：不重启、不替换正在运行的宿主。改动生效仍由用户构建和重启。
- 用户本机仓库和 Agent 现场是两个 clone，都推同一个 `origin/master`。用户推送前先 `pull --rebase`。

### 5. RSI 需要的事实

| 事实 | 位置 | 用途 |
| --- | --- | --- |
| `envelope_sha256`（已实现，待真实验收） | `AgentConfiguration`，空值不序列化，旧记录摘要不变；规范描述复用 `state/kernel-facts` 内 `config/{envelope_sha256}`、`validator/configuration` 的内容寻址与幂等创建 | hub signals/configurations 显示环境坐标；读取单个配置同时得到描述；contrast/gate 可按 `envelope_sha256` 分组 |
| 现场指纹：开始和结束时的 git HEAD、工作树是否有改动、已知锁文件摘要（`pixi.lock`、`Cargo.lock`、`rust-toolchain.toml`、`uv.lock`、`package-lock.json`） | 每次 invocation 的 trace 新增一种 Fact | 不复制文件也能归因到具体一步；环境变化可见 |
| `sh -c` 脚本的首个程序名 | `ToolCall` 新增可选字段，标明是启发式 | 按程序统计失败不退化 |
| 测试/CI 结果、push 是否被接受、后来是否被回滚、用户是否纠正 | `grade/{run}`，由宿主或 CI 记录，不由模型自评 | 真实质量信号 |

现场指纹在同一 envelope 的沙箱里取，不以宿主权限在现场里执行 git（防止仓库配置如 `core.fsmonitor` 借宿主执行代码）。

产品数据只产生信号。选择门禁仍在冻结、独立的评测集上判定：评测 Graph 继续用「固定准备 → 被测节点 → 固定评分」，每次全新现场。

## 切片

按「先跑通一条，再收口」排序。每片结束删除被替代的路径，并跑相关子集测试。

1. **垂直切片（Run 内）。** 常驻助手挂 git 与 `network` Plugin；节点拿到 Rust 工具链和 `PATH` 约定；真实 Provider 经企业微信完成 Anchor 的 clone → 改一处小问题 → 相关 `cargo test` → push `master`。暴露的环境问题在这一片修掉。
2. **现场跟着 Graph 走（已实现，待真实验收）。** 复用可信交接链，按同 owner、会话、Graph 来源与同名 work 节点继承现有资源，不新建资源键；真实验收：改 Graph 指令后发新消息，Agent 在原现场继续，不重新 clone。
3. **边界收口与事实（公共 envelope 已实现，待真实验收）。** 节点准备和 `Op.run` 共用执行边界；`envelope_sha256` 与规范描述进入现有 facts 清单，hub 可展示、读取和切分。**未做**：现场指纹、shell 首个程序名、`anchor_run` 描述调整及 A/B；Plugin MCP/Pilot/依赖构建边界迁移不在本片。
4. **pixi。** 宿主提供固定版本 pixi；验收：在一个需要系统库的小项目上，Agent 用 pixi 装好依赖并跑通测试，锁文件进入提交。

## 未定事项

- 工程维护用现有企业微信助手 Graph 加挂 Plugin，还是新建一个机器人和 Graph（一机器人一 Graph）。
- deploy key 由用户生成并在 git Plugin 上配置；本设计不生成或保存凭据。
