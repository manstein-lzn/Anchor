# 随源码提供的 Graph 与 Plugin

执行 `./scripts/dev.sh start` 后，首次启动会把 [starter.json](starter.json) 中的示例导入可编辑 catalog，并把官方 Plugin 安装到 Library。网页中的 Graph 和插件库可直接查看、修改；安装插件本身不会执行工作流、连接外部账号或启用渠道机器人。

## 从哪个 Graph 开始

| Graph | 用途 | 运行前需要 |
| --- | --- | --- |
| `call-report`、`call-worker` | 主工作流生成报告，调用子工作流处理，再读取返回文件。建议先运行 `call-report` | 无需模型；输入可使用 `{"report_code":"DEMO-001"}` |
| `revise-loop` | 写作、评审、反馈返工和最终文件 | 在模型设置保存 default Flash |
| `parallel-audit` | 两个独立审查分支并行，完成后综合报告 | default Flash |
| `plugin-research` | 单节点学术调研，生成有来源的研究笔记 | default Flash；任务联网 |
| `deep-academic-research` | 多节点研究、质疑、评审和报告，可能进行多轮模型调用 | default Flash；任务联网 |
| `wecom-persistent-assistant` | 常驻企微助手模板：这张 Graph 配一个机器人，每个会话一个常驻实例 | default Flash；在 Agent 节点所挂 `wecom` Plugin 详情的「此 Graph 的机器人」中保存成员、设置凭据并启用 |
| `feishu-persistent-assistant` | 飞书常驻助手模板，长连接私聊、实际工具进度、Markdown 和文件 | default Flash；在 Agent 节点所挂 `feishu` Plugin 详情配置成员 `open_id`、App ID / App Secret 并启用，再在飞书后台保存长连接、订阅接收消息并发布；文件需额外应用权限，见[接入说明](../docs/feishu-assistant.md) |

## 已安装的 Plugin

| Plugin | 随附能力 | 配置 |
| --- | --- | --- |
| `network` | 隔离网络中的 HTTP 请求、文件下载与联网说明 | 无需单独账号；受会话和节点网络权限限制 |
| `git` | 本机 Git 克隆、分支、差异、提交与推送，工作副本与节点编辑和测试工具共用 | 本地仓库无需配置；私有远程仓库在插件详情配置 SSH 或 HTTPS 凭据，可选提交者姓名和邮箱 |
| `academic-research` | 学术检索、原文读取和研究 Skill | 无需仓库附带的账号；访问情况取决于实际论文服务 |
| `docmost` | 页面 MCP 与图片附件工具 | 在插件详情填写 Docmost URL 和 API key |
| `wecom` | 企微工具、Gateway 和使用说明 | 机器人按 Graph 配置，见上；默认不连接 |
| `feishu` | 飞书长连接 Gateway、进度卡、Markdown 及私聊附件 | 应用凭据和接收/回复成员按 Graph 配置；默认不连接，仅向可信原私聊发送文件／任务通知，不提供群聊或任意收件人发送 |
| `anchor-steward`（Anchor Dev） | Graph、Plugin、Rust、Python、pixi 与 Git 工作流的按需 Skill | 在 Pilot 插件面板启用；纯说明，无 MCP server |
| `graph-tasks`（Graph Tasks） | AgentNode 按需选择/构建 Graph、异步委派与设置计划 | 在 Graph 编辑器挂载并保存节点可见范围与能力上限；不自动授权或安装候选 Plugin |

插件的本地 Rust 工具随源码首次准备编译和安装，用户不用手动复制二进制。模型、插件连接和渠道配置仍由网页管理，不附带任何凭据。

完整平台发行包也预装 `network`、`git`、Anchor Dev 和 Graph Tasks，不附带业务 Graph。Anchor Dev 保留目录 ID `anchor-steward`，启用后 Pilot 按需读取开发方法，不会自动初始化环境、启用 Git 或授予联网。使用 Git 工具需单独启用 `git`，远端访问仍按 Host 权限与凭据配置执行。关闭 Anchor Dev 只移除后续 Turn 的方法披露，不删除源码、环境和缓存，也不撤销其他已授权工具。Graph Tasks 的节点授权与结果输入规则见[使用契约](../docs/plugins.md#graph-任务)。

## 用户修改与其他示例

首次导入保留已有的同名 Graph/Plugin。导入完成后，源码更新与重启不会覆盖这些副本，也不会恢复用户已删除的条目。可按需在网页修改或另建 Graph，Plugin 的更新和卸载沿用插件库的管理流程。

`setup` 会准备插件及工具；Graph 需要 Host API，在 `start/restart` 中导入。显式配置的 entry bundle、自定义 Host 或外部服务不自动导入。完整预构建平台包的内容沿用其发行清单，本说明描述源码启动。

[graphs/](graphs/) 仍提供其他现有示例。RSI、周报及 Docmost 周报包装图依赖明确授权的本机输入目录和附加工具，按 [RSI](../docs/rsi.md)、[周报](../docs/weekly-work-report.md) 配置后导入；Python 依赖探针用于开发验证，按需从插件库安装并准备 Python 环境。这些内容未列为默认上手项目。

[AI 每日晨报](graphs/ai-morning-briefing.json) 是可选业务图：共用一次冻结的时间窗口与上期基线，四路并行发现 → 聚合去重与选题 → 三路并行核实原文 → 写作、独立评审与定向修补 → PDF 渲染、逐页视觉检查与本地发布。[来源入口](../plugins/ai-morning-briefing/skills/ai-morning-briefing/sources.json) 随 Plugin 分发，覆盖国内、国际、技术社区和独立搜索；入口用于发现线索，最终事实回到一手发布或可靠媒体原始采访调查，不限制公司名单。发布关卡核对六项评审结论和所审稿件身份；返工保留时间窗口和未受影响内容。整期以约 4–5 页为目标，每条通常两三句话、按重要性排序，保持正常字号自然分页，核查过程留在内部记录。安装 [DeepSeek 联网检索](../plugins/deepseek-web/plugin.json) 和 [晨报](../plugins/ai-morning-briefing/plugin.json) 插件，在前者配置官方 DeepSeek API key；研究、写作与评审使用 `models.default`，视觉检查使用支持图片的 `models.backup.flash`。

两个工具分别由 `anchor-deepseek-tools package-plugin <新目录>` 和 `anchor-briefing-tools package-plugin <新目录> <Typst可执行文件>` 打包，沿用 Library 安装入口；渲染还需要系统 Poppler 与 Noto CJK SC、DejaVu 字体。已发布期刊只归档正文、PDF 和检查信息，下一次运行自动读取最近成功版本与上一日期刊以便去重；同日期刊保持身份并增加修订号。检索以实际窗口为主，适当重叠回查未刊的重要漏项，保留真实日期。可用 `anchor-briefing-tools seed-baseline <插件目录> <PDF> <日期> <截止时间> <窗口起点>` 导入首次参考稿，时间须有明确时区；该操作仅接受空档案。

生产图只交付确切版本的日名 PDF、`report.json` 和工具导出的 `publication.json`，不包含渠道、收件人或分发子图。`send_not_before` 默认上海时间 `09:05`；提前完成等待至该时刻，生产晚于该时刻则直接交接，显式 `null` 表示立即交接。让已有助手通过 `graph-tasks` 持有生产计划，或由操作员显式迁移原普通计划；成功、失败和受阻回原 Session，助手按原请求用自己的渠道工具发送。原文件和导读分别以实际 ACK 为准，未知效果不盲重发。计划启动时间仍遵循 Host 本地时区。配置与验收见[企微契约](../docs/wecom-assistant.md)。
