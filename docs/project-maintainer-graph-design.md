# 常驻工程 Graph：项目现场与执行边界（设计）

状态：设计，未实现。实现完成后按开发约定改写为当前契约并退出活跃入口。

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

1. **边界没有单一来源。** 沙箱策略在 `main.rs:337`、`pilot_tools.rs:59`、`goose_acp/pilot.rs:290`、`application/development_environment.rs:667`、`dependency_provision/build.rs:41` 分别构造，之后在节点准备、`Op.run`、`anchor_run`、Plugin 视图和开发环境里各自叠加挂载与联网。没有一个值能回答「这次调用拿到了什么」。
2. **自装工具不可见。** `PATH` 固定为注册工具目录加 `/usr/bin:/bin`（`anchor-sandbox-bwrap` 的 `make_path`）。Agent 装到 `$HOME/.local/bin`、`$HOME/.cargo/bin` 的程序要写绝对路径才能运行。
3. **Graph 节点没有 Rust。** 宿主的 Rust 工具链只经 `with_pilot_rust` 给 Pilot。
4. **规则没告诉 Agent。** `anchor_run` 的描述只列命令白名单，没写 `HOME=/workspace` 可写且持久、`/usr` 只读、没有 root、凭据只在 git 工具里。
5. **现场跨版本只在少数情况继承。** 只改插件或联网的 Graph 版本可以交接现场，结构或指令改变就不继承（[企业微信契约](wecom-assistant.md)）。
6. **环境不在事实里。** `AgentConfiguration` 没有边界摘要；`ToolCall.program` 对 `sh -c` 只记 `sh`。

已实测（2026-11-01）：本机 bwrap 0.4.0 在 bwrap 内再起 bwrap 成功，所以 Anchor 自己的沙箱测试能在项目现场里跑。pixi 本机未安装。

## 设计

### 1. 宿主只定三条边界，一处计算

| 边界 | 内容 | 谁定 |
| --- | --- | --- |
| 文件 | 可写：项目现场 `/workspace`（也是 `HOME`）。只读：系统目录、`/in`、`/plugins`、`/spill`、注册工具目录 | 宿主 |
| 网络 | 开或关，来自 `network` Plugin 授权 | 用户在 Graph 上授权 |
| 凭据 | 只注入对应 Plugin 的 MCP 进程；Agent 的 shell 看不到 | 用户在 Plugin 上授权 |

新增一个函数，从（宿主配置、节点声明、已授权 Plugin、现场绑定）算出一次调用的执行边界（envelope），节点的 `anchor_run`、`Op.run` 和 Plugin MCP 都从它派生沙箱，不再各自叠加。envelope 有规范序列化和摘要。

替代并删除的路径：第一期替换 Graph 节点执行的三处叠加（`node_host/preparation.rs`、`node_host/execution.rs`、`node_tools.rs` 中的挂载与联网拼装）。Pilot 和依赖构建后续迁入同一函数，迁完删除它们各自的策略构造。

边界之内不再限制做法：命令白名单对项目现场不起额外作用（`sh` 已在白名单），不新增按命令的规则。

### 2. 边界内自由配置环境

- `PATH` 固定前置 `$HOME/.pixi/bin`、`$HOME/.cargo/bin`、`$HOME/.local/bin`。只有这一组固定目录，不做配置项。
- 宿主以注册工具目录提供固定版本的 pixi 和 Rust 工具链（rustup 代理），与固定版本 Goose 的做法一致。包和工具链缓存放在 `$HOME` 下，随现场保留。
- 项目的系统依赖写进项目里的 `pixi.toml`，`pixi.lock` 提交进仓库。没有 `sudo`、`apt`、`docker`。conda-forge 没有的系统包由 Agent 报告给用户，以后再考虑按项目的 rootfs。
- `anchor_run` 描述写明上面三条边界和这些目录约定。这项改动按 hub 的 A/B 门禁验证后才保留（上一次改 `anchor_edit` 描述的 P5 被 A/B 推翻）。

### 3. 现场跟着 Graph 走

常驻工程节点的 Workspace 资源归属从（Run、Graph 摘要、节点）改为（Library 中 Graph 的稳定身份、节点）。Graph 改版本、Run 退役或重建后，新 Run 的同名节点绑定同一个资源。

- 单写者不变：同一资源同时只允许一个活动 Run 绑定，沿用现有执行租约和发布/中断事实。
- Goose 原生历史仍按现有规则：指令或结构改变不继承历史。Agent 开始工作时先核查现场（`git status`、`git log`），这本来就是恢复路径。
- 资源只在用户删除 Graph 或显式释放时回收。
- 只对声明了项目现场的节点生效；其他节点保持 `(Run,node)` 现场，已交付 Artifact 不受影响。

### 4. 交付就是 git

- 推送走 git Plugin 的 `git_run`，凭据只在它的 MCP 里。Anchor 首个验收用一个有写权限的 deploy key，允许直接推 `master`。
- Agent 不部署：不重启、不替换正在运行的宿主。改动生效仍由用户构建和重启。
- 用户本机仓库和 Agent 现场是两个 clone，都推同一个 `origin/master`。用户推送前先 `pull --rebase`。

### 5. RSI 需要的事实

| 事实 | 位置 | 用途 |
| --- | --- | --- |
| `envelope_sha256` | `AgentConfiguration`，与 `host_sha256` 一样空值不序列化，旧记录摘要不变 | 环境成为可比较的实验坐标和旋钮 |
| 现场指纹：开始和结束时的 git HEAD、工作树是否有改动、已知锁文件摘要（`pixi.lock`、`Cargo.lock`、`rust-toolchain.toml`、`uv.lock`、`package-lock.json`） | 每次 invocation 的 trace 新增一种 Fact | 不复制文件也能归因到具体一步；环境变化可见 |
| `sh -c` 脚本的首个程序名 | `ToolCall` 新增可选字段，标明是启发式 | 按程序统计失败不退化 |
| 测试/CI 结果、push 是否被接受、后来是否被回滚、用户是否纠正 | `grade/{run}`，由宿主或 CI 记录，不由模型自评 | 真实质量信号 |

现场指纹在同一 envelope 的沙箱里取，不以宿主权限在现场里执行 git（防止仓库配置如 `core.fsmonitor` 借宿主执行代码）。

产品数据只产生信号。选择门禁仍在冻结、独立的评测集上判定：评测 Graph 继续用「固定准备 → 被测节点 → 固定评分」，每次全新现场。

## 切片

按「先跑通一条，再收口」排序。每片结束删除被替代的路径，并跑相关子集测试。

1. **垂直切片（Run 内）。** 常驻助手挂 git 与 `network` Plugin；节点拿到 Rust 工具链和 `PATH` 约定；真实 Provider 经企业微信完成 Anchor 的 clone → 改一处小问题 → 相关 `cargo test` → push `master`。暴露的环境问题在这一片修掉。
2. **现场跟着 Graph 走。** 改资源归属；验收：改 Graph 指令后发新消息，Agent 在原现场继续，不重新 clone。
3. **边界收口与事实。** envelope 函数替换节点执行的三处叠加；记录 `envelope_sha256`、现场指纹和 shell 首个程序名；hub 能按这些字段切分。描述写实做 A/B。
4. **pixi。** 宿主提供固定版本 pixi；验收：在一个需要系统库的小项目上，Agent 用 pixi 装好依赖并跑通测试，锁文件进入提交。

## 未定事项

- 工程维护用现有企业微信助手 Graph 加挂 Plugin，还是新建一个机器人和 Graph（一机器人一 Graph）。
- deploy key 由用户生成并在 git Plugin 上配置；本设计不生成或保存凭据。
