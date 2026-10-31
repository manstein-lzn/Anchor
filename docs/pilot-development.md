# Pilot 初始化与 Plugin 开发

本文说明当前实现的工具行为与边界。Plugin 清单、执行披露和安装契约见 [Plugin](plugins.md)，共享依赖不变量见 [依赖标准](plugin-dependencies.md)，实际通过范围见 [当前验收与待办](pilot-development-plan.md)。

## 基础环境与工作区

完整平台 Pilot 使用内置管理指令，空 catalog 可以启动，不需要占位业务 Graph。普通 Turn 与 Responses 共用接纳路径；`previous_response_id` 继续原 Session，独立请求使用独立工作区。

浏览器“设置”可以保存 `default/backup × flash/pro` 两组服务信息、测试连接以及初始化 Python/Rust，不要求先启动 Pilot。新 Pilot Turn 与 Graph 节点调用读取已保存的默认模型或显式模型引用，当前调用保持冻结；backup 仅供显式选择，不自动降级。旧执行配置与绑定保留，模型保存、连接测试与真实任务验收分别判断。密钥不进入模型上下文。选择契约见 [Graph 作者契约](rust-graph-authoring-contract.md)，配置边界见 [使用指南](usage.md#模型与-goose)。

`RuntimeConfiguration` 是 Python、Rust、uv 和来源的唯一解析入口。`runtime_prepare` 按冻结的任务联网授权初始化，探测成功后才发布 `state/managed/runtime-config.json`（format 1）和内存快照。完整平台默认使用公共 PyPI；显式部署字段归部署者管理，Pilot 不覆盖，也不双写 `.env`。配置解释器路径不等于启用依赖准备。

没有完整发行包的开发部署，已有解释器可与显式登记的 `ANCHOR_DEPENDENCIES_UV`、`ANCHOR_DEPENDENCIES_UV_SHA256`、`ANCHOR_DEPENDENCIES_SOURCES` 配套使用，可选进程设置见 [使用指南](usage.md#host-根目录与权限)，无需环境文件。Host 校验固定工具摘要并复用原依赖准备器；不会自行从 PATH 寻找工具或扩大来源授权。设置页按实际准备条件开放 Python 初始化，缺少工具时仍允许使用已有解释器，并说明缺失项。

Python 执行还需启用授权：显式部署由 `ANCHOR_PILOT_PYTHON_ENABLED` 控制；未显式指定时，`runtime_prepare` 成功后可以发布 Host 管理的启用事实。显式禁止不能被初始化覆盖。

Python 同版本复用已有解释器。GNU Linux 的 Rust 初始化选择 `x86_64-unknown-linux-gnu`，保存完整工具链名；已有但不可执行的工具链拒绝使用，Rustc/Cargo 实际探针全部成功才发布配置。失败保留此前配置。完整平台的系统安装要求见 [发行说明](../rust/anchor-distribution/README.md#fresh-platform-installation)。

Pilot 在 `<workspace_root>/pilot/<hash(owner,session)>` 工作，跨 Turn 保留现场。`anchor_read`、`anchor_edit`、`anchor_run` 与候选工具都使用这个 Session 根；候选目录相对 `/workspace`，不使用部署 workspace 根或 Goose 内部目录。

文件工具通过目录 FD 和 no-follow 相对操作防止路径逃逸，允许解析到工作区内的符号链接。编辑采用临时文件及原子 rename。`anchor_read` 每页文本预算 256 KiB；首行超预算会报错，使用 `anchor_run` 检查有界字节范围。完整文件摘要和编辑仍读取整个文件，内存随文件大小增长。

执行启动时冻结 `RuntimeSnapshot`，包括尚未配置的状态；当前 Turn 不因其他请求更新配置而获得解释器、工具或权限。后续 Turn 和 Graph 节点重新读取 Host 当前配置。网络与挂载行为见 [任务联网](network-plugin.md)。

## Python 项目

每个 Session 项目拥有独立、可写的私有 venv，使用共享只读解释器。项目文件描述重建所需依赖，实际试装包允许偏离文件；私有包由标准 pip 安装，不与 Library store hardlink，也不回填正式存储。

| 操作 | 行为 |
| --- | --- |
| `development_prepare(project=...)` / `action='ensure'` | 新项目创建空 venv 与 pip；已有环境保留现场，只核对归属、布局和解释器绑定，不证明包健康 |
| 内联 `requirements` / `development_requirements` | 显式标准 pip 安装请求；不做精确同步，不删除未列出的包，不另存长期依赖声明 |
| `anchor_run(project=...)` | Python 别名、裸 pip、模块、shell 和 console script 使用同一私有环境/PATH |
| `action='rebuild'` | 显式重建空环境，保留源码；拒绝同时请求非空依赖数组，依赖另行安装 |
| `action='discard'` | 只释放私有环境；不删除项目源码 |

典型命令均通过 `anchor_run(project=...)` 执行：

```sh
pip install -r requirements.txt
pip install -e '.[dev]'
python -m pip check
pytest
```

requirements、constraints、extras、marker、递归文件和 editable 由 pip 正常解析，修改文件不会隐式同步环境。失败可能留下部分安装结果，没有 pip 事务或自动回滚承诺。

私有现场在 Host scope 的 `development/projects/<project-hash>/`：`selection.json` 保存归属、解释器绑定及最小操作观察，`.lock` 协调项目操作；仅 `env/` 精确可写挂载到 `/development/env`，不进入候选源码或 Library 视图扫描。固定 uv 在此稳定沙箱路径创建 venv，共享解释器必须携带可用的 `ensurepip` 才能引导私有 pip；成功后才发布选择事实。Host 不在沙箱外运行该可写环境的 Python。

命令只获得自己的环境、工作区和只读解释器，不挂 store 或兄弟环境。私有 pip 服从冻结网络和文件权限；正式取包仍遵守 Host 来源、哈希和共享存储规则。

### 旧项目兼容

旧 `current.json` 与只读视图继续可用，默认 ensure 返回 `legacy_read_only`、`pip_available=false`。首次显式 rebuild 成功才切换私有环境，失败保留旧选择；旧引用直到 Session 释放。损坏私有选择不会自动回退或清空。

只剩共享 store 的解包字节时，旧视图仍可使用，私有空 venv/pip 也可离线重建；私有包恢复需要可获取的 wheel 或源码，pip 不能从解包 store 重新安装包。共享正式视图复用与私有开发包安装各自遵循 [依赖标准](plugin-dependencies.md#私有开发环境)。

## Rust 开发

使用 Host 管理的共享工具链，或部署显式设置的 `ANCHOR_PILOT_RUSTUP`、`ANCHOR_PILOT_RUSTUP_HOME`、`ANCHOR_PILOT_CARGO_HOME`。Host 只读挂载 proxy、工具链和 registry/git 缓存；可信 Cargo 入口在 Session 内建立可写 `CARGO_HOME`，首次通过锁和临时目录复制缓存，不复制编译器或挂宿主 Cargo 凭据。

默认 Cargo、命名工具链、shell 和原 Cargo 绝对路径经过同一入口，Rustc 绑定冻结工具链。沙箱 `/proc` 只显示自己的 PID namespace；只有联网允许下精确直接执行 `rustup toolchain install` 才授予工具链写权限，并禁止 self-update、不改共享 default。`Cargo.toml`/`Cargo.lock` 管理 crate，正式交付为普通 Plugin 二进制。Pilot 命令上限 600 秒，AgentNode 上限由部署配置，默认 30 秒。

## AgentNode 项目开发边界

常驻 AgentNode 的稳定工作区支持跨 Turn 编辑、Git 仓库及构建测试，沿用相同的共享解释器和宿主工具登记；Pilot 的 `development_prepare` 和 `anchor_run(project=...)` 私有环境管理并未自动开放给 AgentNode。项目还需要匹配版本的编译器、可达的依赖来源和已有任务联网授权。Git 身份不等于工具链或系统服务授权。

每次命令独立创建沙箱，`/tmp` 和网络 namespace 不跨命令保留；环境变量和工作目录需在命令中重新设置。服务与集成测试可在同一条命令内协同运行，后台服务不会因此获得跨命令生命周期。宿主 loopback、Docker socket、数据库及 GUI 也不自动可用。AgentNode 命令默认上限 30 秒，部署可用 `ANCHOR_NODE_COMMAND_TIMEOUT_SECONDS` 提高，仍受节点的时间预算限制。

AgentNode 完成时通过 `final_result.files` 显式交付文件或目录，`writes` 是必交子集；未选中的依赖、构建缓存、链接及项目 `.git` 留在稳定工作区，不扫描或复制。交付只接受普通文件和目录，二进制、可执行位和空目录受支持。完成与中断不生成整树备份；后续 invocation 继续原现场，原生会话仍由 Goose 续接。工作区机制不提供后台服务、系统包或额外网络权限；所需工具链继续经宿主授权。验收边界见 [当前台账](pilot-development-plan.md)。

## 候选准备、测试与交付

1. 在 Session 工作区实际获取或开发候选，记录来源 commit/摘要。审查、准备与导入只暂存代码，排除 `mutable` 数据，候选数据位置初始为空。
2. `plugin_prepare` 按正式 `dependencies.python` 在所属 Goose scope 的 `candidates/` 准备代码摘要、依赖锁和临时只读视图；不借用项目私有 venv，不发布正式视图。
3. `plugin_test` 实际启动 MCP、读取工具表，并可按 `server`/`tool`/`arguments` 做业务调用；结果区分协议启动与 `business_tested`。源码改变后按新摘要重新测试。
4. `plugin_review` 检查内容指纹、文件、入口与配置需求；编译产物携带源码、锁文件和构建说明，入口相对正式 Plugin，避免引用开发目录。
5. `plugin_install` 走 [正式安装确认](plugins.md#安装与更新)，正式依赖视图独立于开发现场。安装不自动挂载；挂载后的生效与接续见 [Session 契约](architecture.md#session渠道与入口)。删除开发目录或 Session 后正式 Plugin 仍独立运行。

准备结果按代码摘要与冻结解释器复用；解释器变化可对同源码重新准备，成功后才原子切换 `current-preparation.json`，失败不覆盖此前结果。旧 format 1 `preparation.json` 兼容读取，不可变准备事实保留。测试核对所选事实和解释器/包集合，不借另一个 Turn 的结果。安装当前没有强制绑定此前成功业务测试或依赖锁的门禁。

候选网络声明不授予 Turn 权限；候选配置拒绝宿主环境模板，只允许获准路径模板，不继承同 ID 正式 Plugin 的私密凭据或 OAuth。HTTP MCP 取消请求等待不保证远端进程退出；Host 启动的命令及 stdio MCP 必须收束回收。工具格式见 [作者 Skill](../plugins/anchor-steward/skills/plugin-authoring/SKILL.md)。

## 并发、失败与清理

同项目的准备、全部命令、重建和释放共用可取消排他锁，持有至 Sandbox 子进程结束；scope 共享 lease 保护 Session 删除，普通命令不持全局 Library 写锁。

Host 在命令前保存 `operation_in_progress`，收束后保存 `last_status`。失败、超时、取消或重启遗留操作提示 `inspection_required`，要求核查包、源码与测试；它不增加审批、不阻断检查、不自动重放 pip。后续命令成功不证明整个现场健康，显式重建成功清除旧观察。

Session 删除先核对 owner/Session/创建时间绑定，取得原生 execution lease 和开发 scope 排他 lease，释放候选/旧视图引用并保存 released 事实，再清理所属私有环境、workspace、Goose 历史及精确 mount 快照，成功后删除 Session 行。Running Turn、活执行 lease 或归属不明拒绝删除，部分失败保留归属供重试；其他 Session、未知快照和正式 Plugin 保留。

开发凭据输入、OAuth、渠道和计划继续使用 Host 现有用例；接口存在与真实外部业务通过分开判断，见 [当前验收与待办](pilot-development-plan.md#保留的验收边界)。
