# Host 收敛任务书

> 历史归档（2026-11-04）：保留当时的推导、授权范围、过程与证据，不作为新的实施授权；当前行为见[架构](../../architecture.md)，未关闭事项见[统一台账](../../pilot-development-plan.md)。

目标：`anchor-runner-host` 的依赖方向与文档一致，每条规则只剩一份实现。不改用户可见行为、HTTP JSON、磁盘路径与字节、权限边界；行数下降是结果，不是指标。

本页是交给开发 Agent 的任务书。每项完成后由架构负责人验收，验收结论写回本页「状态」列，过程留在提交说明。

## 现状（2026-10-25，master `e3e8384`）

- `runner-host` 生产代码约 58k 行，是单个二进制 crate，约 70 个模块平铺在 `main.rs` 下，`pub(crate)` 在全 crate 可见。唯一有测试固定的边界是 [Run 访问围栏](../../run-access-facade.md)。
- 下层反向依赖上层：非入口模块对 `crate::application` 有 73 处引用（20 个文件），对入口适配模块（`api`、`pilot_tools`、`node_tools`、`channel_tools`）有 34 处（14 个文件）。约 50 处是 `application::metadata` 的读写，它实际是 Run 元数据存储，只是放在了 `application` 下。
- 同一规则存在多份实现，部分已经分叉：
  - Plugin id 有两种规则。Library、graph-host、runtime authoring 和 `api.rs:660` 要求首字节为字母或数字；`pilot_tools::identifier` 不要求，却被凭据和插件配置拿去校验 Plugin id（例如 `credential_store.rs:353`、`plugin_configuration.rs:63`）。
  - 上下文窗口：设置层 `model_configuration.rs:232` 接受 0，运行时 `goose_acp/configuration.rs:390` 才拒绝。
  - 「`required` 只对 Pilot Graph 生效」及挂载默认值写了 3 份：`api.rs:641-652`、`node_host/mounts.rs:400-450`、`node_host/turn_plugins.rs:128-160`。
  - 生产代码中 `FilePluginCatalog::new` 有 46 处、`Library::new` 有 17 处，共分布在 29 个文件（H4 设计实测）。目录租约在每个调用点各自决定。
  - Pilot 与 AgentNode 各写了一条工具组装管线：`pilot_tools.rs:125-310` 与 `node_host/preparation.rs:41-170`，流程都是 prepare_plugins → authorize_oauth → assemble_with_sandbox → attach_tools，只有失败策略不同。
  - `sha256(durable_key)` 文件名主干有 7 份；`goose_acp::pilot::directory`（不跟随符号链接的持久建目录）被 14 个文件借用。
  - `ANCHOR_BWRAP`、`ANCHOR_RUNNER_LIBRARY_ROOT`、`ANCHOR_RUNNER_CATALOG_ROOT`、`ANCHOR_RUNNER_STATE_ROOT` 在 `main.rs` 之外仍被重复读取，与 `main.rs:248` 的注释相违。
- 兼容旧格式的代码约 1–1.5k 行，不是本任务的对象。

## 已定的架构决定

执行中不必再请示，照此执行；遇到与之冲突的事实，停下并在交回里说明。

1. **层次。** 组合根是 `main.rs`、`bin/` 与 `execution.rs`（后者是共享 Runner 的装配，与 Run 围栏的运行时豁免一致）。入口适配是 `api*`、`pilot_tools*`、`node_tools*`、`channel_tools.rs`。用例层是 `application*`。其余都是 Host 服务，不得引用用例层或入口适配；用例层不得引用入口适配。
2. **Run 元数据是存储，不是用例。** `application::metadata` 及其所需的存储助手下沉为顶层 `run_metadata` 模块，使用自己的错误类型；`RunApplication::graph_identity` 改为同模块的自由函数。
3. **Plugin id 的唯一规则**是「首字节为 ASCII 字母或数字，其余为 `[A-Za-z0-9_.-]`」，权威实现放在 `anchor-runtime`（Library、graph-host、Host 已经依赖它），其余副本改为调用它。名称规则（非空、非 `.`/`..`、`[A-Za-z0-9_.-]`）仍用于 Graph 名等，同样只留一份。Library 从不安装首字节不合规的 id，收紧不影响已安装的插件。
4. **配置只在组合根读取。** 上面四个变量只在 `main.rs` 读取，以参数或配置结构向下传递。模型字段只有一个校验函数，保存和运行都调用它，保存时就不接受 0。
5. **Plugin 领域的唯一主人**是一个新的 Host 服务模块（建议名 `plugin_library`）。它负责：Library/目录根、带正确租约的 catalog 与 Library 构造、读定义并重验摘要、按 Graph 声明与 Pilot Graph 求挂载声明。`application/plugins/catalog.rs` 的列表用例建在它之上。Pilot 与 Node 共用一条工具组装管线，失败策略作为参数传入。
6. **不拆 crate。** 本轮只理顺模块依赖；全部任务完成后再评估是否值得拆 crate。

## 任务

| 任务 | 规模 | 依赖 | 状态 |
| --- | --- | --- | --- |
| H0 依赖方向围栏 | S | — | 已验收（`a0de84b`）；实测基线 58 / 35 / 1，[证据](../../../experiments/host-convergence/h0/verification.md) |
| H1 共享原语与已分叉规则 | M | H0 | 已验收（7dd8204）；依赖计数 58 / 23 / 1；本机已安装 Plugin id 均合规，[证据](../../../experiments/host-convergence/h1/verification.md) |
| H2 Run 元数据下沉 | M | H1 | 已验收（ebc9919）；依赖计数 25 / 23 / 1，服务 → 用例减少 33（按下文确认口径），[证据](../../../experiments/host-convergence/h2/verification.md) |
| H3 配置只在组合根读取 | M | H2 | 已验收（c87ef57）；本机已保存模型设置通过新校验；四变量在入口各读一次，模型保存与执行共用类型化校验；依赖计数 25 / 23 / 1，[证据](../../../experiments/host-convergence/h3/verification.md) |
| H4 Plugin 领域单一主人 | L | H3；调研可提前 | 已验收（3a8096c）；生产 +288 行；构造外部 0 / 0，依赖计数 22 / 23 / 1，[证据](../../../experiments/host-convergence/h4/verification.md) |
| H5 剩余反向依赖清零 | M | H4 | 已验收（f948880）；生产 51,251（净 -5）；未登记项 0/0/0，35 处登记为命名欠债，见 layer-fence.txt，[证据](../../../experiments/host-convergence/h5/verification.md) |

规模：S 约半天，M 约 1–2 天，L 约 3–5 天（按 Agent 工作量估计）。

### H0 依赖方向围栏

- 新增 `tests/layer_fence.rs` 与 `layer-fence.txt` 逐文件上限，仿照 `run_access_fence`：沿用同样的扫描规则（排除测试、注释与字面量），可把词法器抽到 `tests/common/` 共用，`run_access_fence` 的行为不得改变。
- 计数三类引用：Host 服务 → `crate::application`；Host 服务 → 入口适配；用例层 → 入口适配。层的划分按决定 1 写死在测试里。
- 基线取当前实测值（上文的 73 / 34 / 1，以测试实际结果为准）。超过上限或清单外文件出现引用就失败；下降时提示同步降低清单，不自动改写。
- 验收：在临时副本里加一处违例，确认测试失败，再撤回。

### H1 共享原语与已分叉规则

- Plugin id 和名称规则按决定 3 各留一份。所有副本改为调用它，包括 `pilot_tools::identifier`、`api.rs:660`、`local_inputs.rs:241/250`、`application/graphs.rs:231`、graph-host `resources.rs:8`、runtime `authoring.rs:226`、Library `validate_id`。凡校验 Plugin id 的调用点改用 Plugin id 规则，其余仍用名称规则；分界逐个列在提交说明里。
- 新建中立模块（建议名 `host_fs`），放入不跟随符号链接的持久建目录（现为 `goose_acp::pilot::directory`）和 `sha256(durable_key)` 主干函数。7 处主干调用改用该函数，各自的前缀（`nf1-`、`fixture-` 等）保持原样，磁盘路径字节不变。`main.rs:70` 的 `create_durable_directory` 语义不同（跟随符号链接），保留并注明区别。
- 先写失败测试，再修：以 `-x`、`_x` 为 id 的 Plugin 在凭据与配置入口被拒绝。本机已安装插件的 id 是否都合规，由验收者核查；开发 Agent 不读取 `.local/`。

### H2 Run 元数据下沉

- 按决定 2 移动 `application::metadata`（339 行）。`execution.rs`、`assistant.rs`、`assistant_plugins.rs`、`node_host/*`、`conversation_resources.rs` 等处改为引用 `run_metadata`。`graph_identity` 的 8 处调用改为调用自由函数。
- 只移动，不改字段、不改文件格式、不改 save/load 语义。错误映射在 `application` 边界处做一次。
- 验收：`layer-fence.txt` 中 Host 服务 → 用例层的计数至少下降 40。实测纯元数据迁移降 33（58 → 25），经用户确认按此验收；剩余 25 处指向 Run 门面、RunApplication、用例错误和 Plugin 发布租约，留给 H3–H5。

2026-10-25 开发实测与范围确认：H1 基线为 58 / 23 / 1；纯元数据下沉后为 25 / 23 / 1，服务 → 用例实际减少 33，与原预估的 40 不一致。余下引用指向 Run 门面、Plugin 租约及其它用例接口。用户已确认保持 H2 范围、按减少 33 交回验收；剩余依赖按 H3–H5 处理，不为满足预估计数扩大本阶段。

### H3 配置只在组合根读取

- 按决定 4，四个变量在 `main.rs` 之外的读取清零（`tool_host.rs:307-308`、`tool_environment.rs:796`、`api.rs:166/195/259`、`pilot_tools.rs:91`、`goose_acp/pilot.rs:311`、`execution.rs:50/94`、`application.rs:341`）。测试里改环境变量的用例同步改为传参，不再依赖全局环境锁。
- 模型字段：抽出唯一的校验函数，设置保存和 Goose 配置都调用它；补一个「保存上下文窗口 0 被拒绝」的测试。开发 Agent 不读取 `.local/` 下的任何设置或凭据文件；本机已保存的设置能否被新校验接受，由验收者核查。
- 其余 `ANCHOR_*` 变量本轮不动，只在交回里列出 `main.rs` 之外仍有读取的清单。

验收者遗留（2026-10-26，不阻塞）：`RunApplication` 同时保存 `data_root`/`catalog_root` 与一份 `HostConfiguration`，`check_store` 的 state 根比较因此恒等、已无检查作用，H4/H5 中改为只保存 Library 覆盖与 bwrap 并删去该比较；`node_host.rs`、`execution.rs`、`durable_write.rs` 三处以默认配置构造 `RunApplication`，并入 H5 由组合根注入。 H5 已删除配置副本与恒等比较，execution reader 和 durable cleanup 已由入口注入；`node_host` 的默认 reader 随 G2 保留为命名架构欠债。

### H4 Plugin 领域单一主人

- 第一步是设计说明，只读即可，可以和 H1–H3 并行：列出全部 catalog 与 Library 构造各自的用途和租约，对照决定 5 给出新模块的接口（函数签名级别），以及 Pilot 与 Node 共用管线的形状。设计说明交验收者确认后再动代码。
- 实现：
  - 挂载声明（含 `required && pilot` 与默认值）只在新模块求一次，`api.rs`、`node_host/mounts.rs`、`node_host/turn_plugins.rs` 改为调用它；
  - 读定义加摘要重验只留一份；
  - Pilot 与 Node 共用工具组装管线，Pilot 的「逐项软失败并记录原因」与 Node 的「硬失败」作为策略参数传入。
- 验收：
  - `FilePluginCatalog::new` 与 `Library::new` 在新模块之外的生产代码中为 0，或者每处保留的调用在设计说明里有理由；
  - 现有 Plugin、Pilot、Node 相关测试全部不改断言即通过。只能因为调用路径变化改测试的构造代码。

验收者对设计的确认（2026-10-25）：租约分类、构造清单、`checked_definition` 保留四种比较、挂载声明复用 `plugin_mounts::resolve`、失败策略矩阵均照设计执行。另有三项修改：

- **`Catalog` 只包装实际用到的方法**，不镜像 `FilePluginCatalog` 的全部 API。错误优先沿用现有类型（`GraphError`、`InstallError`）；只有调用方确实需要区分取消等情形时才加新枚举。目的是避免薄包装层本身变成新增行数。
- **工具管线不用回调，也不用策略枚举。** 共用的是步骤实现，不是调用顺序：拆成几个普通函数，例如「准备一组」「授权 OAuth」「连接并挂接披露」。Pilot 和 Node 各自按现有顺序调用，Node 自己的配置保留与观察记录写在调用之间。分组（逐项还是整批）和错误处理（记原因还是 `?`）留在各自调用方。设计里的时序约束照旧，接口形状以此为准。
- **验收基线以设计实测为准**：生产代码中 `FilePluginCatalog::new` 46 处、`Library::new` 17 处，在 `plugin_library` 之外降为 0。

验收者结论（2026-10-26）：挂载声明、Pilot 授权判断、Pilot 连接错误文本与披露挂接时机逐处对照旧实现，语义等价。H0–H4 生产行数累计约 +300，本轮至今没有瘦身；H4 的增量主要是 `Catalog`/`PluginLibrary` 的约 20 个转发方法，以及准备步骤迁入新模块。转发层的价值在于所有权围栏，不在于减少行数。H5 不得再净增生产行数。

### H5 剩余反向依赖清零

架构验收者裁决（2026-10-26）：第一层按 G13/G14/G6/G7/G11/G12/G4（含 A22、B20）执行，Host 生产物理行数必须低于 H4 基线 51,256；第二层接受下表命名架构欠债。原盘点的全量端口方案预算净增 +186 行未实施，清单保留为历史估算。

- 已完成的归属迁移：工具名归中立 `tool_names`，已保存回复图片读取归 `channel_replies`，Session workspace 与开发作用域释放归 `pilot_resources`；原路径与转发均删除。G6/G7/G11/G12 对应引用全部消除。
- G13/G4：`RunApplication` 删除整份配置副本和恒等 state 根比较；Pilot/工具装配接收 Library 覆盖与 bwrap 参数，execution reader 和 durable cleanup 惰性回调由入口注入。保留原清理扫描顺序及失败计数。
- G14 复核偏离：`runtime_configuration::install_release_network` 有生产调用，`PluginLibrary::install_directory` 不能加 `cfg(test)`；保留原入口与安装锁语义。其余转发也都有调用，没有可直接删除的闲置方法。此前“只有测试调用”的盘点结论已纠正。
- 实际依赖 22 / 23 / 1 → 20 / 14 / 1，消除 11 处；剩余 35 处均在下表，未获准计数为 0 / 0 / 0。围栏继续检查原分类和逐文件上限，未提高数字。Host 生产物理行数 51,256 → 51,251（净减 5），使用既有 `measure_lines.py::test_partition`。

| 命名例外 | 文件 | 实际计数（服务→用例 / 服务→适配 / 用例→适配） | 保留理由与关闭条件 |
| --- | --- | --- | --- |
| G1-assistant-run | `assistant.rs` | 1 / 0 / 0 | 随 G2 保留交接 Run 查询；待 Node 端口迁移一起关闭，避免拆开在线 owner 语义 |
| G1-assistant-plugins | `assistant_plugins.rs` | 2 / 0 / 0 | 随 G2 保留 RunProgress、授权投影与租约调用；随 Run 查询端口迁移关闭 |
| G1-conversation-resources | `conversation_resources.rs` | 2 / 0 / 0 | 随 G2 保留 Run 删除协调及引用检查；待删除用例与 Node 端口迁移关闭 |
| G1-local-inputs | `local_inputs.rs` | 1 / 0 / 0 | 随 G2 保留 RunProgress 的输入校验、挂载契约；随共享 Run 快照契约迁移关闭 |
| G2-node-host | `node_host.rs`（基线 :100、189、192） | 3 / 0 / 0 | NodeHost 继续持 RunApplication 与默认 reader；端口接线预算 +70 行，暂不实施，保留为架构欠债 |
| G2-node-conversation | `node_host/conversation.rs`（基线 :228） | 1 / 0 / 0 | 随 G2 保留历史投影的 Run 查询；待 Node 端口迁移关闭 |
| G3-channel-management | `channel_supervisor/management.rs` | 2 / 0 / 0 | 随 G8/G9 保留渠道用例契约及错误映射；待渠道契约迁移和交互端口接线关闭 |
| G5-node-preparation | `node_host/preparation.rs`（基线 :110–151） | 0 / 4 / 0 | 随 G2/G8 保留 sandbox 工具与渠道包装的装配；3 处工具名引用已消除，剩余待装配端口迁移关闭 |
| G8-goose-pilot | `goose_acp/pilot.rs`（基线 :5、640、641） | 0 / 3 / 0 | Goose 仍使用 Pilot 适配器与权限表；端口接线预算 +35 行，暂不实施，保留为架构欠债（B01–B03） |
| G8-pilot-interaction | `goose_acp/pilot_interaction.rs` | 8 / 2 / 0 | 随 G8 保留原生交互的用例前置条件、错误及工具权限映射（A09–A16、B04–B05）；待交互端口迁移关闭 |
| G8-channel-interaction | `goose_acp/pilot_interaction/channel_management.rs` | 0 / 2 / 0 | 随 G8 保留渠道参数解析调用（B06–B07），继续共用唯一实现；待交互端口迁移关闭 |
| G9-pilot-host | `pilot_host.rs`（基线 :10、855、875） | 0 / 3 / 0 | 保留 Pilot Turn 具体执行端口；端口接线预算 +35 行，暂不实施，保留为架构欠债 |
| G10-candidate-context | `application/plugin_candidates.rs`（基线 :357，C01） | 0 / 0 / 1 | 随 G9 保留候选自测的 PilotTurnContext；待 Turn 执行端口及所需参数拆分关闭 |

这些是仍存在的反向依赖，不表示实际三类引用已经清零。命名标识固定在 `tests/layer_fence.rs`，文件路径、上限和保留理由写在 `layer-fence.txt` 对应行注释中。验证命令、删除路径、计数与偏离见 [H5 交回](../../../experiments/host-convergence/h5/verification.md)。

架构验收结论（2026-10-26）：H5 通过。生产 51,256 → 51,251（净 -5），未登记项 0/0/0，35 处命名欠债登记在 `layer-fence.txt`。H0–H5 Host 生产行数累计 +295，本轮以依赖方向理顺为主，瘦身发生在 H1（-80）和 H5（-5）。G2/G8/G9 三组端口接线（合计预算 +140 行）未实施，作为后续单独立项的架构欠债。本收敛任务到此结束。

## 共同要求

- 每项单独一个分支或工作树，从最新 master 开始，按依赖顺序合入。同一时间同一文件只有一个编辑者；H4 设计说明只读，不受此限。
- 不改：HTTP 路由与 JSON、磁盘路径与字节、Kernel（`git diff 9a5fa57 -- rust/anchor-kernel` 必须为空）、`run-access-fence.txt` 的上限（只许下降）。
- 测试只因模块路径或构造方式变化而修改，不改断言；如果确需改断言，说明这是行为变化，停下来交回。
- 每项交回时提供：
  - `scripts/test.sh -p anchor-runner-host`（H1 另加 `-p anchor-runtime -p anchor-graph-host -p anchor-library`）、clippy 与 fmt 的实际结果；
  - `layer-fence.txt` 前后的计数；
  - 生产代码行数前后的变化；
  - 偏离本页的地方及理由。
- 不读取 `.local/` 下的设置、凭据和运行状态；不部署、不推送。
- 遵守 [开发约定](../../../AGENTS.md)，包括措辞规则。

## 并行方式

H0 → H1 → H2 → H3 → H4 实现 → H5 依次进行：它们都要改 `api.rs`、`main.rs`、`node_host/`、`pilot_tools.rs` 等同一批文件，并行只会把时间花在合并上。

可以并行的只有两件：H4 的设计说明（只读），以及 H0 完成后由另一个 Agent 预先盘点 H5 的清单（只读）。总量约 8–12 个 Agent 工作日。每项交回后由验收者在当天核查。
