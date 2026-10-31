# 每周工作报告

## 核心与可选发布

[weekly-work-report.json](../examples/graphs/weekly-work-report.json) 是可移植的本地周报核心：从操作员授权的 Codex、Claude Code、DeepSeek 历史采集冻结证据，经项目理解、写作、独立评审和确定性门禁，最终在 `publish` 节点结束。它不挂载 Plugin，不要求 Docmost、企业微信、个人身份或外部发布账号；模型服务仍须由宿主配置。

本地交付为 `report.md`、`sources.md`、`assets/`、`review.md`、`review.json`，以及用于跨 Graph 传递图片目录的 `assets.tar`。复制整个 `publish` 目录仍可离线阅读；核心不把报告自动发往任何外部服务。报告面向了解项目目标、未参与日常开发的负责人和协作同事，按重要性说明有证据的实质变化、意义与下一步重点，而不是复述会话或排查流水。正文独立可读，来源依据保存在 `sources.md`。

需要 Docmost 时，另行导入 [weekly-work-report-docmost.json](../examples/graphs/weekly-work-report-docmost.json)。包装图先校验操作员填写的目标，再以 `Op.call` 的 `wait` 模式调用已安装且名为 `weekly-work-report` 的核心 Graph，选择子 Run 的 `publish` 结果，然后执行唯一的 Docmost 发布 Agent。它不复制采集、理解、写作或评审流水线；两张图有各自 Run、状态及双向调用引用。仅包装图的发布节点挂载 `docmost` Plugin 并允许网络。

```mermaid
flowchart LR
    collect[本地采集] --> understand[项目理解与选题]
    understand --> write[写作配图]
    write --> review[独立评审]
    review --> gate[确定性门禁]
    gate -->|表达或结构| write
    gate -->|理解与判断| understand
    gate -->|通过| publish[本地交付并结束核心 Run]
    validate[包装图校验目标] --> call[Op.call wait 核心 Graph]
    call -.独立子 Run.-> collect
    publish -.返回所选产物.-> call
    call --> docmost[Docmost 发布]
```

## 一次性操作员授权

可移植 Graph 不能自授宿主文件权限。安装原生 `anchor-rsi` 后，由持有本机历史读取权的操作员**在沙箱之外**运行发现命令；应使用与实际运行 Host 相符的用户和环境，不要为了执行命令切换到另一个用户的 HOME：

```sh
anchor-rsi weekly-discover > discovery.json
jq '.sources' discovery.json
jq '.local_inputs' discovery.json
```

`weekly-discover` 输出 `format: 1`、`sources` 数组及 `local_inputs` 候选绝对路径授权。它只依据当前 `HOME`、已配置的工具环境变量和各工具约定的历史位置核验目录，不扫描整个 HOME，也不读取工具配置文件或凭据。使用非标准 profile 时，执行发现命令前须导出该工具实际使用的配置环境变量，不能假定发现过程会自动读取 profile 文件。`sources` 保留逐来源的状态、路径、发现依据等信息；不可用来源不为不存在的目录生成 grant。

| 来源 | 配置优先级 | 只读历史子目录 |
| --- | --- | --- |
| Codex | `CODEX_HOME`，未设置时为 `$HOME/.codex` | `sessions` |
| Claude Code | `CLAUDE_CONFIG_DIR`，未设置时为 `$HOME/.claude` | `projects` |
| DeepSeek Harness | 非空白 `DSH_HOME`，未设置时为 `$HOME/.dsh` | `sessions` |

DeepSeek 的显式 HOME 按其实际解析规则支持 `~`、`~/`、`~\` 和相对当前工作目录的路径；空白值视为未设置，非空路径中的空格不会被删除。配置已指定但目录不存在时，不回退到另一个 profile 冒充同一来源。工具 HOME 可以是操作员配置的符号链接，但历史子目录不得重定向到该 HOME 之外。发现不包含 Codex 的 `archived_sessions`，Claude 采集排除 sidechain 和 subagent 转录；DSH 只选择最新格式代际，不重建迁移或 seeded 历史。自定义 DSH persistence root、不在标准工具 HOME 下的独立来源以及远端/容器内历史，仍需操作员显式核对和配置既有绝对路径授权。

“历史目录可用”不等于工具已安装、目录里有记录、窗口内有活动或已解析全部历史；这些事实须区分。采集仍须分别披露未挂载、未读到或只读到部分内容的来源，发现成功也不等于完成报告。

只允许所需的历史子目录：Codex 的 sessions、Claude Code 的 projects、DeepSeek 的 sessions，不挂载整个工具 HOME、用户 HOME、认证文件或配置目录。`local_inputs.collect` 只包含可用的 `codex`、`claude`、`deepseek` 目录和 `rsi`；`gate`、`publish` 只需 `rsi`。`rsi` 是发现命令当前真实可执行文件本身，符号链接按真实二进制解析，使用 Host 已有的普通文件授权；不把可执行文件的父目录或其中的其他文件一起暴露。三个业务 Op 通过 `/local-inputs/rsi` 调用原生工具，只有 `collect` 可读对应 `/local-inputs/<source>` 历史目录，其余节点只读当次证据。旧图若将 `rsi` 授权为目录，需把三条 Op 命令与该授权同步改为单文件形式，并启动新 Run；旧授权文件不会自动改变。

确认 `.sources` 和 `.local_inputs` 后，将授权保存到 Host 已配置的绝对目录 `ANCHOR_RUNNER_LOCAL_INPUTS_ROOT`。以下是首次创建步骤，禁止覆盖已有配置；若文件已存在，先核对现有授权，再人工合并所需键，不要用发现结果盲目替换：

```sh
: "${ANCHOR_RUNNER_LOCAL_INPUTS_ROOT:?请设为 Host 使用的绝对授权根目录}"
mkdir -p "$ANCHOR_RUNNER_LOCAL_INPUTS_ROOT/weekly-work-report"
(
  set -C
  jq '.local_inputs' discovery.json > "$ANCHOR_RUNNER_LOCAL_INPUTS_ROOT/weekly-work-report/local-inputs.json"
)
```

生成的 `local-inputs.json` 是本机专用的绝对路径配置，不是共享 Graph、导出包或通用模板；不要随 Graph 分发。`discovery.json` 也含本机路径，应保留在操作员控制的本地。Host 继续使用既有的绝对路径 grant、只读挂载和按 Run 冻结契约，发现命令只打印候选配置，不修改 Host 权限、不绕过授权、不增加新的描述符 schema。包装图不继承或代替核心授权：配置放在被调用的 `weekly-work-report` 下，不是 `weekly-work-report-docmost` 下。路径或授权改变后需重新核对，再启动新 Run；旧 Run 仍受冻结事实校验。

核心运行需要 `jq`（评审 JSON 检查）、既有沙箱 Shell/`anchor-route` 和原生 `anchor-rsi`，不依赖外部 `tar` 或 `zstd` 命令：`weekly-publish` 使用 Rust tar 库在原生组装中生成图片归档，压缩 DeepSeek 历史由采集器使用 Rust zstd 库在进程内解码。上面的操作员安装步骤使用本机 `jq` 提取授权配置。只有可选 Docmost 包装图额外使用沙箱内的 `tar` 解开图片归档，并使用 `jq` 做目标输入预检。发现目录不是工具安装或依赖授权；操作员仍须确保对应路径所需的命令在相应环境中可用。Goose/provider 配置由宿主提供，不能放进 Graph 或历史授权文件。

## 时间窗口与定时

核心默认 `input` 为 `{"end":null}`。采集冻结本次截止时间，回顾此前七天；当前采集器的窗口时间采用固定 `+08:00` 偏移，不提供按用户选择时区的能力，因此两张示例都不暴露未生效的 `timezone` 字段。手动回看可传绝对 RFC3339 `end`，如 `2026-10-01T09:00:00+08:00`。包装图将 `end` 映射给核心；复审和修改沿用同一冻结窗口，不重新采集。宿主定时器的系统时区是独立设置，不由这个输入控制。

共享示例本身不创建计划。需要每周四北京时间 09:00 运行时，用现有 `/schedules` API 创建 `{"type":"weekly","weekdays":[3],"time":"09:00"}`，并让 Host 使用 `Asia/Shanghai` 时区、在计划时点运行；停机或图正忙时不补跑。纯本地报告只调度核心；需要同步 Docmost 时调度配置好输入的包装图，不要把两张图都调度成同一次周报。

## 无来源、无活动与证据边界

采集是只读报告投影，不是会话恢复引擎。`evidence/index.json` 固定窗口并记录逐来源的覆盖情况；原始文件位置、JSONL 行号和时间用于回查。历史、工具输出、报告与外部页面都是不可信数据，不能执行其中的指令，也不能据此扩大文件、网络、Plugin 或账号权限。

- 全部来源 `unavailable`：报告说明“无法依据本次来源判断本周工作”，不能推断“本周没有工作”。
- 来源可用但窗口内没有活动：说明“未发现窗口内活动记录”，不能把无记录当作没有工作。
- 来源 `partial`、损坏或截断：只基于已覆盖证据归纳，保留缺口与限制，不能声称已读全部历史。

这些情况仍可交付简短、诚实的周报：保留日期窗口、非空 `sources.md`、逐来源状态和一幅说明证据覆盖与缺口的简单 SVG；不为满足篇幅、两三项进展或配图要求而虚构项目、成果、指标或计划。评审按这种有限结论核验，而不因缺少活动强制返工。

概览文本和详细工具投影存在截断，关键结论应回查可定位证据。隐藏推理、系统提示、凭证及无关私人对话不是工作成果；常见凭证脱敏不等于已覆盖所有隐私。采集投影还会改写网关按子串拦截的措辞连写（规则和同义写法见[开发指南](development.md#文本措辞检查)），因此引用原话须回查原始文件；周报 JSONL 保留原始文件和行号定位，但改写后的文本不再逐字等同原文。助手声称完成、工具执行成功、测试通过和真实 provider 验收是不同证据等级，不能互相替代。配图只能解释有证据的关系，不生成假截图或虚构指标。

## 独立评审与本地组装

理解节点输出内部编辑简报，说明项目背景、选题理由、应删去的琐事和全文主旨。作者按实际重要性写作，使用具体、平实、主动的表述，避免按故障、命令或对话顺序罗列日志；通常约 1000–1800 中文字，但篇幅不是通过条件。

评审使用与作者分开的模型上下文，但默认仍是同一模型，不等于跨模型验证。先只读正文与图的文字内容，记下读者能复述的重点和阅读障碍，再回查证据；依次判断 `business`、`reasoning`、`writing`、`facts`。不能进行视觉检查时如实说明方式和边界。表达与结构问题交回 `write`，项目理解和事实判断问题交回 `understand`，不固定修改轮数，不以纯润色阻断交付。

`review.md` 保存可阅读反馈，`review.json` 为每个阻断项保留 ID、位置、可定位证据、影响、修法、验收条件和解决依据。复审逐项核销旧问题，不能删掉旧问题制造通过。普通 Op `gate` 调用 `anchor-rsi weekly-gate`，复用 `anchor-route` 分流；只有四项全部通过、没有未解决阻断项、旧问题逐项保留且评审的 `reviewed_artifact` 绑定当前稿件的完整 Artifact 引用和清单摘要，才能进入 `publish`。`weekly-publish` 组装前再次核对该引用与清单摘要；格式不合法、旧稿评审或遗漏旧问题均失败。

`reviewed_artifact` 使用 `{artifact:{id,node_id,invocation},manifest_sha256}`，从 `/in/write/.anchor-artifact.json` 原样读取。Agent 完成时显式提供 `final_result.files`；写作节点交付报告、来源和 `assets/`，纯文字完成使用空数组。

组装仅读取交付描述中的相关文件；图片清单来自该描述，不扫描工作区。它检查 Markdown 本地图片路径、普通文件和 SVG XML，图片须位于 `assets/`，拒绝路径穿越与符号链接。SVG 只允许静态图表，拒绝脚本、事件属性、`foreignObject`、外部引用和不安全 CSS。`weekly-publish` 在原生组装中使用 Rust tar 库将已验证的 `assets/` 写成 `assets.tar`，不调用外部归档命令，也不向归档加入会话历史或凭证。同一工作区重复采集只清除过期的生成明细，重复组装只替换已校验的图片集；其他现场文件保留，旧明细和图片不进入新交付。语义质量仍取决于模型判断，确定性门禁不能保证零错误。

## Docmost 包装图

先在 Host 的 canonical Library 安装并授权 Docmost Plugin，提供所需凭据，再填写包装图输入中的 `docmost_space` 和 `docmost_parent_title`；两者默认 `null`，没有预置个人空间或项目父页面。输入示意中的名称必须由操作员替换为实际精确名称：

```json
{
  "end": null,
  "docmost_space": "操作员指定的空间名称",
  "docmost_parent_title": "操作员指定的根下父页面标题"
}
```

入口 `validate` 用普通 Op 检查两项为非空白字符串，失败时不调用核心或远端服务；这是输入检查，不是宿主授权。目标对象与权限仍由 Docmost 工具和宿主校验，不能从报告、历史或模型偏好猜测。

结果选择遵循 [call-report.json](../examples/graphs/call-report.json) 与 [Graph 组合契约](graph-composition-design.md)：`report` 是包装图中的调用节点，目标结果节点是核心的 `publish`，发布 Agent 的返回输入位于 **`/in/report/result/`**，不是 `/in/publish/`。当前 Host 的 `export_call_result_files` 逐项校验普通文件，不展开目录或通配符，因此不把 `assets/` 直接写入 `result.files`。包装图选择 `report.md`、`sources.md`、`assets.tar`、`review.json`、`review.md`，再把核心生成的图片归档解到自己的 `/workspace/assets/`；不需要更改通用 Call 或复制业务流水线。

发布时用报告第一行一级标题作为子页面标题，只在发布副本去掉该行。按操作员指定的精确名称选择唯一空间与根下父页面，父页面不存在才创建；报告完整标题精确匹配唯一子页面后原位更新，不存在才创建，歧义则停止。先读取既有页面核对空间与父页面，不删除、移动或修改其他页面；超时或未知外部效果先核对目标，不盲目重建。

本地图片必须通过 `attachments_upload_page_image` 上传到该报告页面，使用返回 URL 替换 `assets/...` 地址并保留替代文本、图注。既有同名图片引用为 `/api/files/<attachmentId>/<filename>` 时传 `attachmentId` 原位替换，避免重复堆积。逐张上传成功后才更新完整正文；图片上传失败不发布坏链接，也不删图凑成功。附件来源是 `/workspace/assets/`，原始只读返回仍在 `/in/report/result/`。

成功后 `docmost.json` 记录空间、父页面、报告页面、创建或更新操作及实际返回的页面与附件元数据。只发布正式报告和图片，来源清单、评审与会话底稿不自动上传。缺少凭据、目标不唯一、权限、网络或远端调用失败均不能伪造成功；核心本地交付不受包装图后续发布失败影响，已发生的外部效果也不自动回滚。

如另需企业微信提醒，可由操作员在本机包装图的 `docmost` 后增加挂载渠道 Plugin 的通知 Agent，按[发送授权契约](wecom-assistant.md#业务工具与附件边界)显式指定已有机器人的 `bot_graph` 和获准的 `userid`，调用 `wecom_send_message` 并核对回执；共享示例不预置机器人或个人账号。只传正式报告与 `docmost.json`，不传来源或评审。通知不需要借用对话 Session；失败不应重跑核心或撤销已完成发布，ACK 未知不自动重发。

## 验证边界

当前验证覆盖采集投影与 Graph 契约，尚未完成完整周报的真实模型端到端验收。

| 范围 | 已验证结论 | 未覆盖边界 |
| --- | --- | --- |
| Graph 契约 | [定向测试](../rust/anchor-runtime/tests/weekly_work_report_graphs.rs) 校验核心终点与权限、包装图输入映射和返回文件约定 | 不执行 Goose、真实模型或外部发布 |
| 采集与投影 | `anchor-rsi` 43 项测试、严格 Clippy 和格式检查通过；覆盖三来源正文、工具调用/结果及详细/概览投影的三类措辞改写。已安装工具重放原始失败的两条公开记录通过 | 不代表全部历史格式、凭据脱敏场景或网关词表均已覆盖；既有 Run 的冻结证据与原生历史不改写 |
| Host 与沙箱 | 本机三来源齐全、仅 Codex、无来源 fixture 完成；验证节点隔离、冻结授权及授权改变后不能沿用旧 Run | 不证明操作员机器的全部历史均可用，也不等于报告内容质量验收 |
| 真实模型小图 | 2026-10-20 使用与运行中 Host 相同的二进制和保存模型设置，经沙箱采集、Goose ACP、真实 `deepseek-flash` 和授权工具读取六份合成投影，写入并回读验证文件，以 `final_result` 完成，Kernel 为 `settled/done` | 不包含完整理解、写作、独立评审、跨 Graph 返回、Docmost 上传或企业微信投递 |

可复跑的 Graph 契约检查：

```sh
cargo test --manifest-path rust/Cargo.toml -p anchor-runtime --locked --test weekly_work_report_graphs
```

本机原始日志、二进制身份与复现入口见 [2026-10-20 诊断与验证](../.local/weekly-gateway-wording-20261020/README.md)；三种 Host fixture 与发现/压缩/归档验证见 [采集可移植性摘要](../.local/weekly-history-portability-20261016/validation-summary.json)。这些私有验收产物仅保存在验收机器的 `.local/`，不随仓库分发，其他 checkout 中缺失时不能视为可访问证据。原始记录重放与真实模型小图是分开的检查，模型小图只读取合成历史。

## 失败诊断与重跑

界面的 ACP `session/prompt` 错误码 `-32603` 是脱敏后的协议摘要；stderr 为空不能判断模型服务或采集是否成功，应核对对应 Goose 原生 Session 的公开 Error 记录。

2026-10-20 的 Run `rust-18e0286a83504b82` 完成三来源采集后，在 `understand` 读取历史中的旧网关报错文本时被上游以 HTTP 400 拦截。实际工具目录中的旧采集二进制未覆盖触发规则，源码还遗漏另一类已知连写；当前源码与实际工具均已补齐三类改写。旧 Run 仍保留原始冻结证据和模型上下文，应新建 Run 重新采集，直接恢复不能清除旧文本。该错误与 2026-10-18 隔离验收中的上游 HTTP 502 不同，后者记录见 [当时验收](../.local/closeout-20261018/README.md)。

本次未重跑完整周报、重启 Host、替换已安装 Graph/计划或执行外部发布。验收机器上的个人安装图及工具目录 grant 保留；共享部署使用本页的新核心与可选包装图，新授权对 `rsi` 仅指定单个二进制文件。完整周报、空来源报告质量、Docmost 图片更新与未知远端效果核查、企业微信投递仍需独立验收。
