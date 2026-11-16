# 本地 Codex 项目经理常驻助手

本文定义通过常驻助手 Graph（飞书 / 企业微信）担任本地工程项目经理（Engineering Manager）、以本机安装的 Codex CLI（`codex app-server --stdio`）作为多会话并行执行池的当前契约与运维方式。

## 边界与职责

| 层 | 所有者 | 职责与边界 |
| --- | --- | --- |
| 渠道与对话 | Anchor 常驻助手 Graph + `feishu` / `wecom` + `graph-tasks` | 接收可信私聊、短等待后回应、拆解需求、维护工作台账（常驻 `/workspace`）、按需安排周期巡检并向原私聊主动汇报；台账不替代真实工具状态 |
| 协议适配 | `plugins/codex`（普通 Streamable HTTP MCP Plugin） | 保存 owner-bound 的 Bridge 地址、Bearer 凭据与可选项目默认子路径（`X-Codex-Project-Scope`）；向 Manager 暴露非阻塞会话编排工具 |
| 本机执行池 | `anchor-codex-bridge`（本机用户态守护进程） | 仅监听显式指定地址（默认 `127.0.0.1:8091`），强制校验 Bearer 凭据与启动时显式授权的 `--allow-root` 本地目录白名单；管理 `codex app-server --stdio` 连接、实时流式状态投影与受管 Git Worktree |
| 编码引擎 | 本机 `codex` CLI | 完全复用本机用户已有的 `~/.codex` 配置、认证、Skills 与本地模型服务；Anchor 不接管或重写本地 Codex 配置 |

这一拆分遵守以下不变量：

- **不建第二套通用运行时**：Anchor 的 `Kernel`、`GraphRunner`、`GooseNodePort` 与 `BubblewrapSandbox` 均不修改；Codex 作为外部工程执行资源通过受审 HTTP Plugin 接入。
- **不以提示词代替路径校验**：Bridge 工具的项目发现、会话选择和 worktree 操作由 `--allow-root` 约束，每次调用重新校验规范路径；header `X-Codex-Project-Scope` 只能进一步收窄。此规则约束工具路由，不等同于 Codex 全盘读取隔离。
- **长任务不阻塞聊天窗口**：渠道进度卡仅用于短时交互观察。`spawn` 在启动握手/RPC 后有界等待（默认 12 秒，可设 0），未结束返回 `running` 与 `thread_id`；多个任务优先设 0，避免累计等待。Manager 通过 `board` / `inspect` 查询进度、通过 `steer_or_resume` 动态纠偏，并经已授权的 `graph-tasks` 巡检计划与原渠道发送工具汇报。工具等待上限不是整个模型回复的时间承诺。
- **并行写任务使用 Git Worktree 隔离**：同一仓库的多条只读调研会话可共享主工作树（`mode: "read-only"`）；多条并行修改会话通过 `worktree` 参数在受管目录（`<worktree-root>/<repo>-<hash>/<slug>`，分支 `codex/<slug>`）独立推进，避免互相污染索引与构建产物。

### 信任与恢复边界

- 这是同一可信本机用户的开发执行池，不是多租户隔离服务。持有 Bearer 的客户端有 Bridge 白名单内的管理权限；可选 scope header 不是独立凭据。同一 owner 的 Plugin 配置跨 Graph 共用，Graph 指令中的项目名不是权限边界。
- Codex 的模型、认证、Skills 与本地 MCP 仍由用户管理；本地 MCP/工具可能拥有独立权限。Bridge 为任务显式设置只读/工作区写入模式、联网和用户审批，但不重写用户配置文件，不声称限制全部读取或全部 MCP 副作用。
- worktree 写任务允许写其目录和仓库共用 `.git`，因此能够操作 Git 元数据，并非恶意代码隔离。并行修改应使用不同 worktree；Bridge 拒绝已知活跃写会话的重叠目录，不检测另一个终端或外部进程的写入。新 worktree 基于已提交的 HEAD/显式 base_ref，不复制主目录未提交修改。
- 受管 worktree 的 bucket、分支与 Git 双向指针必须匹配；符号链接、缺失路径、独立 Git 元数据目录及意外分支会失败关闭。`force` 只跳过工作树干净检查，不跳过归属或活跃会话检查；移除保留分支，不自动合并、推送或部署。
- 常驻 Manager 管理项目，不绑定单个 Codex thread。Bridge 新建自己的 app-server 进程，不接管用户当前 TUI；可读取已有持久历史并续写，但续写前应确认旧 CLI 不在同时运行该 thread。
- Bridge 内存只是 Codex 事件投影，不新增持久任务数据库。HTTP 客户端断开不会停止后台工作；Bridge/app-server 退出则会中断其运行环境，重启后读取 Codex 历史，核对现场再继续，不重放不确定命令。不能承诺跨重启无缝保持运行中的 shell。
- 进度通知不是 Bridge 自带的后台渠道发送器。用户授权后 Manager 必须实际保存巡检计划；每次 worker 只观察一次，结果回原 Session，由 Manager 检查状态变化和真实发送回执。没有计划/渠道权限时明确提示，不许承诺自动通知。

## MCP 工具契约

`plugins/codex` 挂载服务器 `codex`，向节点暴露 14 个工具（模型侧名称前缀为 `codex-codex_`）：

| 工具 | 用途与语义 |
| --- | --- |
| `projects` | 列出当前作用域内可见的本地 Git 仓库（最大深度 3），返回相对路径、当前分支、工作树是否干净、受管 worktree 数量及已知活跃会话数 |
| `spawn` | 在指定项目（或新建/复用受管 `worktree`）启动新的 Codex 会话（`thread/start` + `thread/name/set` + `turn/start`）。支持 `mode: "read-only" \| "workspace-write"`、`network_access` 与有界 `wait_seconds`（0–30 秒）；超时未结束仍保持后台运行并返回 `status: "running"` |
| `btw` | 对作用域内的主会话提问，运行中或未加载的持久历史均可；调用原生 `thread/fork(ephemeral: true)` 后只在临时会话开始一轮，不加载、不 steer/interrupt 主会话。返回独立 `thread_id`、`parent_thread_id` 与已有回答；追问使用临时会话 ID，结束用 `control(action: "close")` |
| `board` | 返回作用域内活跃及近期会话投影：状态、计划步骤、最近活动、活跃轮次耗时、Token 与待决请求。持久历史每页 100 条，使用 `history_next_cursor` 继续；缓存输出上限 200 条，返回 `truncated`。未加载历史不是实时订阅 |
| `inspect` | 按 `thread_id` 读取单个会话详情：最新助手输出正文、受管 worktree 的 `git status --short` 与 `git diff --stat`、最近命令输出尾部，以及待处理的 `ServerRequest`（提问或审批）详情；若会话来自磁盘历史且尚未加载进当前 Bridge 内存，自动回退读取 `thread/read` 与 `thread/turns/list` |
| `steer_or_resume` | 对运行中会话调用原生 `turn/steer` 动态注入补充指令（不打断重来）；对待决提问/审批提交明确答复或决定；对空闲/历史会话调用 `thread/resume`（如需）+ `turn/start` 延续同一上下文推进下一轮 |
| `control` | 对指定会话执行 `interrupt`（调用 `turn/interrupt` 停止当前轮）、`archive`（调用 `thread/archive` 归档会话）或 `remove_worktree`（仅在没有运行中会话且工作树干净或显式 `force: true` 时移除受管 Git worktree） |
| `models` | 分页读取本地 `model/list` 的模型与推理档位目录；目录可见不代表 provider 实际调用成功，不修改本机配置 |
| `history` | 按 `thread_id` 分页读取原生 turns/items；`view` 默认为 turns，items 可指定 `turn_id`，返回原生 `data` / `nextCursor`，不加载或启动工作 |
| `session` | 显式生命周期：`load` 只恢复并订阅、不发送任务；`unload` 对空闲会话请求退订；`fork` 创建持久分支，可指定模型/档位和受管 worktree、但不开始工作；`rename` 改名；`configure` 修改后续轮次模型/档位；`unarchive` 取消归档；`compact` 在空闲会话发起原生异步压缩 |

`spawn` 与 `session` 的 fork/configure 接受独立的 `model` / `reasoning_effort`，例如 `model: "gpt-6.1-sol"`、`reasoning_effort: "max"`。新建时省略则继承本机配置；fork 优先沿用可知的父会话设置，configure 省略项保持不变。原样转发原生字段，不改写配置文件或映射模型提供方。返回值来自原生响应、历史元数据或设置通知，未知时为 null，不能从上下文窗口猜测。

`loaded` 表示当前 app-server 的内存驻留状态，`subscribed` 表示 Bridge 是否订阅该线程事件，两者不是运行状态。原生退订后可有空闲宽限期，所以 `unload` 返回 `unsubscribe_requested`，不能声称内存立即释放。`load` 与开始下一轮由同一个原生加载实现衔接；Manager 无需请用户处理 loaded，也不得发送空任务来唤醒。`models` / `history` 每页默认 20、最大 100；cursor 只用于原查询。

生命周期沿用同一项目授权校验；原生入口只调用当前已审查的项目接口，不提供无限制 RPC、全局配置或权限透传。持久 fork 默认只读，开启实际开发工作仍由 `steer_or_resume` 显式选择模式；新 worktree 来自已提交基点，不复制脏文件。`compact` 仅表示已请求压缩，需 inspect 核实完成；未知提交结果不得自动重放。统一 fork/load 实现替代原 `/btw` 专用 fork 和 continue 内联 resume，不另建状态库或恢复引擎。

飞书/企微的 `/btw <问题>` 由 Manager 按 Skill 调用同名工具，不增设渠道命令路由。临时会话继承分支时刻的原生历史，不是 Manager 代读日志，也不是主会话实时同步的第二输入流。其执行模式固定只读、不联网、不可审批升级，并指示不调用工具、不继续旧任务；本地 MCP 的独立权限边界仍适用。Bridge 重启会丢失临时会话，不能把它当成可持久恢复的开发任务。

### 原生协议入口

日常快捷工具保持不变；不再为每个原生功能单独增加包装命令：

| 工具 | 当前契约 |
| --- | --- |
| `native_discover` | 从本机 Codex 导出的实验协议发现 request / notification / server_request；按 method 查询原生参数与答复 JSON Schema，按 search/offset/limit 查目录。每个请求标明 callable 和权限限制；Bridge 重启后重新生成 |
| `native_call` | 用原生 method + params 调用已审查的项目接口，原样保留模型、档位、结构化输入、目标、队列、历史、设置、评审等参数；返回原生结果。新建需显式 cwd，任务返回不等于完成 |
| `native_events` | 按 thread_id / after 读取带游标的原生通知及服务端请求；每线程最多 256 条 / 1 MiB，单条超过 64 KiB 返回明确截断标记。stream_epoch 改变或 gap 为真时用历史核对，不重放任务 |
| `native_respond` | 按准确 thread_id / request_key 和原生 response_schema 回复动态工具或待决请求；审批仍需用户明确授权，不改变现有审批策略，不接受旧连接的请求键 |

原生参数仍受目录、并行写入、用户审批、旁路只读及不确定结果校验。模型参数以安装版本 schema 为准，不把推理档位拼进模型名。认证、全局配置、宿主无沙箱 shell、无项目归属的进程/文件接口和未审查的原生执行入口只可发现，不可调用；这不是把本机所有管理权限交给 bot。删除/回退持久历史还需显式 confirm。额外 config 只允许模型推理档位、摘要和回答详略，不修改本机配置文件。

原生 `queue/add` 在空闲会话可能立即启动工作，不是纯草稿写入。活动目标与非空队列保留写目录占用，恢复时查询原生目标/队列；未知权限变更保守保留写权限占用。此投影不新增数据库、任务引擎或自动恢复器。进度仍由已有 Graph Tasks 巡检回流，原生事件读取不是渠道自动推送承诺。

### 审批与未知副作用处理

- `spawn` 默认使用 `approvalPolicy: "on-request"`（也可显式选 `"never"` 让超界操作直接报错返回给 Codex 自行调整）。
- 默认 `mode: "read-only"`、`network_access: false`，写代码需明确选择 `workspace-write`。审批 reviewer 固定为用户，Bridge 不提供自动审批代理。历史导入默认只读；`steer` 不改变正在运行的权限。
- 当 Codex 发出 `ServerRequest`（`item/tool/requestUserInput`、命令/文件/权限审批或 MCP elicitation）时，Bridge 不静默自动批准越权请求，而是将该请求挂起保存在该 `thread_id` 的内存待决表中，将会话状态标为 `waiting_approval` 或 `waiting_input`。
- Manager 在巡检或用户追问时通过 `board` / `inspect` 看到具体问题或审批内容，向飞书用户确认后，再通过 `steer_or_resume` 提交 `approval_decision` 或 `answers`。若 Bridge 重启，未完成的运行轮次按 Codex 本身持久历史恢复为静态状态，不自动重放未决命令。
- 回答需携带准确 `thread_id` 和 `request_key`。唯一请求键区分 wire ID 类型和旧连接请求，回填只发送到原活跃连接；写入并 flush 后才返回。该确认代表响应已写入，不代表命令已完成。`answers` 是问题 ID 到字符串数组的映射，必须覆盖全部问题；权限请求的授权仅限当前 turn。
- `turn/start` 结果不确定且尚无新轮次事件时标为 `unknown`，保留写目录占用，不自动重试。`inspect` 核对原生运行状态后才能继续；旧查询不能覆盖后续轮次的准入状态。

## 部署与配置步骤

### 1. 构建并启动本机 Bridge

在宿主机用户态启动 `anchor-codex-bridge`（确保当前用户终端本身已能正常执行 `codex`）：

```sh
cargo +stable build --manifest-path rust/Cargo.toml -p anchor-codex-bridge --locked

# 生成至少 32 字节、随机生成的访问凭据并保存在本机私有文件
umask 077
mkdir -p ~/.local/state/anchor-codex-bridge
openssl rand -hex 32 > ~/.local/state/anchor-codex-bridge/token

ANCHOR_CODEX_BRIDGE_TOKEN="$(cat ~/.local/state/anchor-codex-bridge/token)" \
  rust/target/debug/anchor-codex-bridge serve \
  --listen 127.0.0.1:8091 \
  --allow-root /absolute/path/to/projects
```

可选参数：
- `--allow-root <ABS_PATH>`：可重复指定多个允许管理的本地目录根；至少提供一个，且不能为 `/`、`/etc`、`/proc`、`/dev`。
- `--worktree-root <ABS_PATH>`：受管 Git worktree 存放根目录，默认 `<首个 allow-root>/.anchor-codex-worktrees`（若不可写可显式指定到用户缓存目录）。
- `--codex-bin <PATH>`：默认从 `PATH` 查找 `codex`。

保持 Bridge 进程运行；生产可由本机用户的进程管理器托管。不要把明文 HTTP 监听地址暴露到公共网络；跨机器接入需自行提供受控隧道/TLS。`/healthz` 也要求 Bearer，只说明 HTTP 服务就绪，不证明模型可用。安装 Plugin 不自动启动此进程。

本机用户态服务现使用 `Restart=always`：非操作员停服导致的正常退出也由 systemd 拉起，显式 `systemctl --user stop` 不触发 systemd 自动重启。若另行启用了下述 Manager 拉起授权，其后续请求仍可启动该服务。两者都只恢复 Bridge 进程，不自动重放 Codex 工作或网关已拒收的消息；原请求须按真实执行和投递事实分别核查。

### Manager 的本地项目直连

项目 Manager 不必为每次读文件或运行检查创建 Codex 会话。Host 可通过入口配置 `ANCHOR_PROJECT_WORKSPACES`，把一个实际目录单独授权给指定 Graph 和 owner：

```json
{
  "feishu-anchor-developer": {
    "owner": "local",
    "path": "/home/mansteinl/Anchor",
    "exclude": [".local"]
  }
}
```

- 默认无授权；Graph、Plugin 或消息内容不能自行增授。目录须为现有的 canonical 绝对路径，排除项须为现有、非链接的相对目录。Host 状态和 Library 在项目内部时必须排除。
- 当前获准助手工作 Turn 可发现 `project_read`、`project_edit`、`project_run`。它们复用 NodeTools 的读取、乐观校验编辑和 Bubblewrap 命令执行，不调用 Bridge；这三个工具中的 `/workspace` 是真实项目，普通 `anchor_*` 的 `/workspace` 仍是原 Manager 工作笔记目录。
- 每次操作重新检查 Graph、owner、当前助手/Turn 和目录身份。项目命令沿用已有命令、联网和超时上限，排除目录在命令中为空且只读，文件工具也不接受排除路径。不增加任意宿主 shell、服务控制或 Codex 配置权限。
- 直接操作与 Bridge 的受管写任务共用目录租约；忙时不执行，不覆盖其他写入者。取消调用不提前释放仍在运行的命令租约。独立 worktree 可以并行；锁是协作机制，不约束未参与的本机终端，也不等于跨 worktree 的共享 Git 元数据事务。
- Bridge 观察到 Codex 内部委派或归属不明的子执行时，保守保留对应写锁，不把父轮结束当作全部子任务结束。有写锁的连接丢失也不自动释放或重连执行；需要操作员确认现场后受控重启服务，生产服务须使用 `KillMode=control-group` 收束其子进程。Bot 的幂等 start 不是 restart，也不会解除这种占用。暂不宣称内部委派完成后的锁自动回收已实现。
- 原 Session、工作笔记与对话历史继续由既有机制维护，项目目录不替换这些持久资源。Codex 是可选执行池，适合长任务与独立并行工作；Bridge 上线不等于历史任务恢复。

实现与部署分开验收；实际部署状态、测试及未覆盖边界见本文「验证边界」和统一台账。

### Manager 的受限拉起授权

操作员可在 Host 部署环境设置 `ANCHOR_CODEX_BRIDGE_MANAGER_GRAPHS='["feishu-anchor-developer"]'`，重启 Host 后生效。未配置时任何 Manager 都不获得此权限；Graph 或 Plugin 自己的指令不能授予它。Host 仅为名单内当前有效的常驻助手、正确工作节点和运行中 Turn 暴露 `codex_bridge_status {}` / `codex_bridge_start {}`；该 Turn 还须冻结授权 `codex` 与 network，且连接是固定的 `http://127.0.0.1:8091/mcp`。

权限只针对本机当前用户的 `anchor-codex-bridge.service`：查看状态、幂等启动；不提供任意命令、服务名、安装、停止、重启、配置或额外项目目录权限。它取代人工执行固定服务的状态/启动命令，不另建进程监管器或任务恢复库。已运行服务保持原 PID。

Host 在该授权助手连接 MCP 前尝试启动依赖；若连接仍不可用，仅该服务的连接降级，Manager 仍能解释状况并使用受限服务工具。连接失败可能涉及网络、认证或握手，不能推断进程停止；配置及已知协议/清单错误不被改写成成功。若本轮工具清单没有 Codex，手动启动后由下一次用户输入重新连接，不承诺轮内刷新。服务启动不等于原生任务恢复、模型可用或消息送达，不自动重放任务、审批和已拒收消息。

### 2. 安装 `codex` Plugin 并配置连接

通过插件库界面导入 `plugins/codex` 目录，或使用 `anchor-library` 安装（纯 HTTP MCP 清单与 Skill，不含运行期 Python 依赖）：

```sh
rust/target/debug/anchor-library --root /path/to/library \
  install --directory plugins/codex --id codex
```

在 Anchor WebUI「插件库 → Codex 项目执行池」填写：
- `bridge_url`：`http://127.0.0.1:8091`（Host 的 HTTP MCP 客户端直接在宿主机侧建立连接，不受节点命令沙箱的 loopback 隔离限制，但仍要求节点具备有效 `network` 授权）
- `api_key`：上一步生成的凭据内容
- `project_scope`（可选）：留空则可管理 `--allow-root` 下全部项目；若填写相对路径（如 `Anchor` 或 `marketdata/simai`），则收窄到该子目录。

### 3. 导入常驻项目经理 Graph 并绑定飞书 / 企微

1. 导入示例：
   - 飞书常驻图：[feishu-codex-project-manager.json](../examples/graphs/feishu-codex-project-manager.json)
   - 企微常驻图：[wecom-codex-project-manager.json](../examples/graphs/wecom-codex-project-manager.json)；机器人按[企微契约](wecom-assistant.md)配置，不混用两种渠道历史
   - 巡检子图：[codex-watch-worker.json](../examples/graphs/codex-watch-worker.json)（按原样使用 Manager 示例时，须先导入此图，再保存 Manager 的任务授权；不需要自动巡检时可移除该授权引用）
2. 在 `feishu-codex-project-manager` 的 `manager` 节点确认已挂载 `feishu`、`codex`、`network`（`auto`）与 `graph-tasks`（在节点面板勾选允许调用 `codex-watch-worker` 并保存以生成操作员授权收据）。
3. 按[飞书常驻助手](feishu-assistant.md#配置与首次单聊)为这张 Graph 配置独立飞书自建应用机器人的 `FEISHU_APP_ID`、`FEISHU_APP_SECRET` 与你的 `open_id` 并启用。
   - 若希望“一个飞书机器人管一个大项目”，复制该 Graph 为 `manager-<project>`，分别绑定各自机器人，或在各自 Graph 指令中指定目标项目名。
   - 若希望“一个总控机器人管本机全部开发项目”，保持 `project_scope` 为空即可通过对话按需切换或跨项目并行调度。

## 验证边界

- 2026-11-13 项目直连已仅部署到 `feishu-anchor-developer`：真实飞书请求发现并成功执行 `project_read/edit/run`，读取 AGENTS.md、运行 Git 命令、创建/回读/删除唯一临时文件，没有调用 Codex。原群 Session 与笔记目录 inode 保留，原生会话恢复 156 条历史；角色指令通过现有停止/退役/交接路径更新，没有修改总 Manager 权限。Host 1,321 单测通过、18 默认忽略；项目授权/取消 6 项包含其中，Sandbox/lease 29 项、Bridge 68 项及真实 Goose/确定性模型的项目直连 1 项、飞书回归 6 项通过，严格定向 Clippy 通过。已部署 release 的直连回归另行复验通过；新版 Bridge 的真实本机 provider 只读与临时 worktree 写入两项均通过。真实回复平台回读确认，不代表人已读；本轮 Bot 仍额外通过 CLI 回复提及发送者，未把「只回复一次、不再提及」的交互要求标为通过。保守子执行锁回收、小时级故障恢复与持续项目管理不在完成声明内。见部署及验收证据（本机原件，未入库：`.local/project-direct-20261113/README.md`）。

- 2026-11-13 已仅向 `feishu-anchor-developer` 上线固定服务的状态/启动授权，并更新 Codex Skill。真实飞书 Bot 协作请求触发原 Manager 调用两个宿主工具成功，启动返回 `already_active`；随后只读 `projects` 成功，Bridge PID 保持 520345，Manager 回到 `wait_input`，飞书回复 API 回读确认。1314 项 Host 单元测试、6 项真实 Goose/模拟模型与飞书传输回归、Clippy 及局部格式检查通过。停服后拉起与连接降级已有确定性测试，未主动中断生产 Bridge 做故障注入，不宣称无人值守全链路故障恢复已验收。见上线证据（本机原件，未入库：`.local/codex-bridge-manager-grant-20261113/README.md`）。

- 2026-11-13 AnchorDevManager 的真实群请求因 Bridge 不在线而在 MCP 初始化时停止，未执行模型工具。已恢复原 Bridge 服务并原地续跑同一 Run；真实 Manager 完成历史只读查询，未开新 Codex 工作会话，现回到 `wait_input`。原入站已被网关标为 rejected，未重置去重账本；另以同 Bot 回复原消息，API 成功且回读确认。用户服务已从 `Restart=on-failure` 改为 `Restart=always` 并热加载，未重启 Host 或改变项目授权。Bridge 此前退出码为 0，但退出发起者未知；本次未做故障注入或验证无人值守全链路恢复，不能视为小时级稳定性验收。见恢复证据（本机原件，未入库：`.local/codex-bridge-recovery-20261113-1626/README.md`）。

- 协议与单元测试使用受控本地假 `codex app-server` 进程与临时 Git 仓库，确定性验证初始化握手、路径白名单与越界拒绝、Git worktree 创建/状态/清理、`spawn` 短等待与异步转后台、`board`/`inspect` 实时流式投影、`turn/steer` 动态纠偏、挂起审批/提问回填、`turn/interrupt` 中断，以及通过 `anchor-mcp-host` 与 `anchor-graph-host` 的完整 HTTP MCP 清单及工具调用闭环。
- 已通过本机 `codex-cli 0.167.1` 的真实只读与 worktree 写入测试：经 HTTP MCP、stdio app-server 和原本机 provider 读取/创建临时标记，校验实际文件和主目录隔离；未修改 Codex 配置或产品项目。确定性回归、独立模型调用及检查记录见[验收证据](../experiments/codex-bridge/README.md)。
- Plugin 清单与三张示例通过 `FilePluginCatalog` / `GraphSnapshot` 验证。隔离环境已跑通真实 Host/Goose/HTTP MCP/Codex/调度器/Gateway 的巡检回流：任务执行期间回复与继续对话，运行中及完成通知取得网关 confirmed，完成后删除计划；Codex 使用真实本机 provider，Manager/观察者使用确定性模型响应，飞书平台使用本地模拟接口。单文件任务附件的 `anchor_read` 读取缺陷已修复，仍使用原有无符号链接读取校验。
- 2026-11-12 本机用户态 Bridge 与 `codex` Plugin 已部署，独立 `feishu-anchor-developer` Manager 已建立。新增 `btw` 与模型/档位参数已更新在线 Bridge/Plugin；原生真实调用确认 `gpt-6.1-sol` + `max`、继承上下文回答期间主轮仍运行。Bridge 更新前没有活跃执行任务，未重启 Host、未改 Codex 全局配置。Host 单文件输入修复仍未部署；飞书客户端 `/btw` 消息、审批回传和小时级稳定性仍待验收。证据及失败记录见[验收记录](../experiments/codex-bridge/README.md)。
- 同日项目级生命周期已部署为 10 个工具：新增 models/history/session，合并原有 fork/load 路径，移除 `/btw` 对 loaded 的人为前置条件。46 项 Bridge 回归通过；本机真实 provider 验证已卸载历史旁路上下文、主轮不变、仅加载不发任务、模型/档位设置、持久 worktree fork、历史/模型目录及取消归档；运行中旁路不中断再次通过。compact 已有确定性契约验证，未做真实 provider 压缩验收；本次仍未代验飞书客户端投递。在线连接发现 10 工具，授权摘要和配置 revision 保持不变。

- 同日原生项目客户端已上线 14 工具：全量发现本机 180 请求 / 86 通知 / 11 服务端请求，52 个已审查项目及目录查询接口可调用；并非无限制全协议执行。60 项回归通过，真实 provider 跑通原生任务、结构化输入、动态工具答复、原始事件、队列管理与历史，并复验历史/运行中旁路。在线服务、工具发现和模型目录核验通过，未重启 Host；飞书端验收由用户进行。真实 compact、活动目标自动续跑和小时级稳定性未覆盖，具体证据见验收记录。
