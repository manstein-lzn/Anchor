# RSI Graph

`rsi` 是一个每周运行的普通 Graph，用来检查 Anchor 最近一周的真实运行记录、当前 Graph/Plugin/架构代码和公开生态变化，并产出下一轮可以验证的改进建议。它使用现有定时器和 Graph 反馈边，不新增调度器、记忆系统或发布系统。

## 做什么

```text
collect（动态发现、证据快照、领域索引）
  ↓
audit-context（保存本轮审查反馈）
  ↓
audit-fanout
  ├─ run-audit
  ├─ code-audit
  ├─ graph-audit
  ├─ plugin-audit
  └─ research（公开联网取证）→ dependency-audit
  ↓
audit-join → analyze → review-fanout
              ↑          ├─ fact-review（事实与公开研究）
              │          └─ proposal-review（方案、验收与回滚）
              │                    ↓
              │             review-join → review（确定性合并）
              │                              ↓
              └──────── 修改报告 ─────────── gate → publish
audit-context ←────── 重新专项审查 ────────────┘
```

整个流程属于同一个 Graph、一次 Run。五个专项拥有各自工作区和模型上下文；research 与其他四个审查同时进行，依赖审查等联网取证完成后开始。join 等全部分支成功，再让综合节点工作。综合之后，第二组配对 fanout/join 并行进行事实评审与提案评审，避免数量/引用核对挤占风险与回滚审查。

`scripts/rsi/collect.py` 动态发现 Git 跟踪和未忽略的新文件、全部部署 Graph 定义、已安装 Plugin/工具及 Skills/MCP/通道资源。源码不再使用固定文件清单，也不按字符数截断；清单明确记录排除、二进制、缺失、外部授权和读取错误。凭证/环境/运行缓存目录排除，敏感字段脱敏；源文件哈希和脱敏快照哈希分别记录。只读授权覆盖的 Plugin 符号链接可跟随，宿主路径通过操作员授予的路径映射回既有沙箱挂载，不因此扩大授权。

`run-index.json` 索引所有历史 Run；`runs.json` 包含与本周窗口相交的 Run 的机械记录，`run_snapshot/` 可按需查看历史状态、失败原因、节点 commit/输入关系和冻结 Graph/Plugin 绑定。不会读取完整聊天、trace 或隐藏推理，任务正文和模型提交正文仅留长度/摘要指纹。冻结绑定只证明当时的资源身份，不能恢复未保存的历史 Plugin 实现字节。

每个领域先读 `domains/<领域>.json`，按失败、本周变更、历史提案选择重点，不要求把全部源码/历史读进上下文。专项输出 `findings.json/md`，区分 observed/inferred/unknown，列出证据位置、实际已读范围、未覆盖范围和局限。综合先读取这些发现及 `join.json`，再按冲突和因果关系回查原始证据。索引覆盖、模型实际审查和结论正确性分别报告。

`scripts/rsi/research.py` 从本次冻结源码发现 Python/npm 声明（包含可选、开发、构建依赖和嵌套 manifest），根据包注册表的项目链接和仓库 remote 发现上游；`github_repos`、`python_packages`、`npm_packages` 输入只追加目标，不替换发现结果。它只访问 `api.github.com`、`pypi.org`、`registry.npmjs.org`，抓取版本、发布说明和公开 issue 信号，重定向也校验主机。请求超时/响应大小边界属于传输约束，失败有记录；不限制模型请求或图轮次。GitHub 匿名限流时不能保证取得每个仓库的动态。

社区覆盖仅限这些公开源，GitHub Discussions、非 GitHub 社区、私有信息和独立 CHANGELOG 页面未采集。上游新版本不证明兼容性，Python 版本清单来自采集 Op 的解释器而非已验证的服务环境，npm 锁文件不证明实际安装状态；不支持的声明格式也列入覆盖说明。

分析节点写出：

- `rsi-report.md`：面向维护者的本周判断、限制和优先建议；
- `evolution.json`：带 ID、证据、风险、验收和回滚条件的提案台账；
- `sources.md`：本地文件字段/路径、外部 URL、抓取时间和限制。

`previous.json` 保存成功发布的历史报告和完整提案，内容从 Run 记录的 publish commit 读取；无法读取时记录错误，不拿未提交工作区替代历史。`previous-index.json` 给出按 ID 的简洁索引，模型按需查询完整历史。每个历史 ID 都必须在新一期 proposals 或 carry_forward 中处理，不可静默消失；resolved 需要新的实施及验证证据，证据不足用 continue/hold。

两个评审节点不能修改稿件。`review` 是普通 Op：校验两份评审绑定当前 analyze commit、各自匹配 review-join commit，再合并独立检查与意见。任一 open/拒绝不能被另一方通过覆盖；发布保留 review-manifest.json。`gate` 除四项评审及 reviewed_commit 外，还验证五个领域发现的格式/证据文件、本次 join 的分支 commit、时间窗口、提案 ID 唯一性及历史连续性。普通问题返回 analyze；专项产物不完整或评审明确要求补查时返回 audit-context，复用本次采集窗口并重新审查。audit-context 将 gate.txt 保存为 audit-feedback.txt，再进入 fanout，保证各分支收到跨反馈边的具体修正要求；首次审查标记 FIRST_AUDIT。无效引用反馈包括原路径，专项引用使用实际文件，目录清单引用索引。证据存在和 schema 合法不能机械证明语义正确，内容仍由独立评审核查。不使用固定轮数假装任务已经收敛。

修订轮优先读取反馈、当前稿件和相关证据；后续评审优先比较分析 commit 差异与旧问题。正式报告不重复叙述评审轮数/提交ID/逐字修改流水账，过程留在 review.json、gate.txt 和 Git 中。这样避免新增过程性矛盾导致无效回路；重要新事实错误仍必须修正。

limited 仅用于已明确限定的证据缺口或纯文字偏好；已查证的事实错误、矛盾、无依据因果和改动无法满足验收均须退回修订。未采集不等于不存在，文档记载的验收与本轮可独立复核的原始产物分别说明。流程成功也不等于内容通过，显式验收脚本只报告 workflow_passed，仍需源证据抽查。

review Op 还会对提案和报告做确定性安全检查：出现关闭脱敏、取消权限保护、绕过证据校验或回退到更宽暴露面的回滚措辞时直接返回 revise；模型评审不能覆盖该拒绝。

它也拒绝无范围的历史首次或完备因果断言，例如把 invocation=1 写成历史首次、把隔离验收 Run 写成生产完成样本，或把时间相关性写成唯一根因；必须区分当前采集窗口、当前部署定义、隔离验收和未排除的构建差异。

## 安装到本机数据根

从仓库根目录执行：

```bash
./.venv/bin/python scripts/setup_rsi.py --root .local/demo
```

脚本只在不存在时复制 `.local/demo/workspaces/rsi/graph.json`，写入 `local-inputs.json` 的只读授权，并在 `state/schedules.json` 增加一个每周四 09:00（机器本地时间）的计划。collect 挂载 anchor/source/code 和该授权文件本身（grants），用于识别已授权宿主路径的沙箱别名；research、review、gate 挂载 code，依赖来源读取 collect 的冻结快照。已经存在的同图同规则计划不会重复创建。停机或 Graph 忙时沿用现有计划语义：该次错过，不补跑。

已有 RSI 的升级需通过 Graph 保存 API 更新完整定义，并在无活动 RSI Run 时同步上述只读授权；安装脚本不会覆盖已有 Graph。原计划引用同一 Graph ID，无需增加第二个计划。

如果 `anchor-serve` 已经运行，优先通过现有 `POST /schedules` API 创建计划，或在脚本执行后重启服务；服务进程会按内存中的计划定期写回 `state/schedules.json`。不要在服务运行时直接编辑该文件。

如果 Anchor 数据根或仓库位置不同，显式传入 `--root` 和 `--source`。定时任务只保存路径和规则，不保存凭证；服务使用的模型仍来自根目录 `.env`。

可在同一模型服务上给综合/评审单独配置模型名别名：`.env` 中设置 `ANCHOR_MODEL_ALIASES={"models.rsi-quality":"服务支持的模型名"}`，新安装传 `--analysis-model models.rsi-quality`；已有图通过保存 API 只调整 analyst/fact-review/proposal-review 的 model。显式真实验收脚本接受同名选项。其他专项及工作流继续用默认模型。具体模型的语义质量须用失败稿反例与完整流程分别验收，不能以“更强型号”代替证据。

## 边界和验收

RSI Graph 不自动提交、合并或部署代码，不自动改 Graph/Plugin，不重放历史外部副作用，也不把模型声称完成当成事实。`evolution.json` 的状态为 proposed/continue/hold/resolved；resolved 是有依据的复查判断，不代表本次 RSI 执行过改动。实际修改由维护者在隔离工作区完成，再用保留案例和相关测试验证。

`tests/test_rsi_parallel.py` 以原生沙箱/模型夹具跑完整图，验证重新专项审查、仅修改报告两种反馈和最终发布；采集/研究/门禁另有覆盖与失败边界测试。显式真实验收运行 `./.venv/bin/python scripts/verify_rsi_parallel.py`，在隔离数据根读取本机授权证据，保存活动节点采样、报告和验收记录。当前实际证据、部署及未验证项统一见开发台账 A30；单次成功报告不代表长期自进化收益已验证。
