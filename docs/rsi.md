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

`anchor-rsi collect` 使用既有 Rust evidence 模块，动态发现授权源码、部署 Graph、公开 Plugin/Skill 资源和 Run 元数据；同时冻结源码 Git 变更及默认 `state/schedules.json`。索引记录原始与脱敏快照哈希、采集时间和缺失范围。凭证、环境、缓存、私有运行记录、符号链接、二进制和超过 8 MiB 的文件被排除并记录限制。自定义宿主路径不会从 Graph 中自动获得授权；自定义计划存储未落在默认路径时明确记为未覆盖。

`run-index.json` 索引发现的历史 Run，`runs.json` 保存机械投影及七天窗口。索引区分本周与历史记录，缺少时间戳不算本周成功样本。每个虚拟证据路径对应冻结文件，可回查状态、错误及原始记录中存在的字段；`source_field_presence` 和 `projection_omitted` 区分不存在与未采集。完整聊天、trace、checkpoint 和隐藏推理不进入证据。冻结绑定只证明资源身份，不能恢复未保存的历史 Plugin 实现字节。

每个领域先读 `domains/<领域>.json`，按失败、本周变更、历史提案选择重点，不要求把全部源码/历史读进上下文。专项输出 `findings.json/md`，区分 observed/inferred/unknown，列出证据位置、实际已读范围、未覆盖范围和局限。综合先读取这些发现及 `join.json`，再按冲突和因果关系回查原始证据。索引覆盖、模型实际审查和结论正确性分别报告。

`anchor-rsi research` 从冻结依赖声明查询公开注册表与 GitHub 元数据，复用 Rust ecosystem 工具，不加载部署凭证。当前覆盖动态发现的直接依赖；Rust、npm 及第三方项目声明解析不要求安装这些语言的解释器。HTTPS 来源限定为 crates.io、PyPI、npm 和 GitHub；不使用环境代理或自动重定向。超时、响应大小和限流失败都保留为证据，不能写为查询成功。

公开版本与仓库元数据只是生态信号，不证明功能收益、升级兼容性或实际安装状态。社区讨论、私有信息、传递依赖与不支持的声明格式不属于完整覆盖；报告须保留相应局限。

分析节点写出：

- `rsi-report.md`：面向维护者的本周判断、限制和优先建议；
- `evolution.json`：带 ID、证据、风险、验收和回滚条件的提案台账；
- `sources.md`：本地文件字段/路径、外部 URL、抓取时间和限制。

`previous.json` 保存已验证的成功发布历史及操作员明确授予的历史报告。原生 Run 历史按 publish Artifact 身份和 manifest 文件哈希核验；无法验证时记录错误，不拿未提交工作区替代历史。`previous-index.json` 提供冻结历史路径，模型按需读取完整提案。每个历史 ID 都必须在新一期 proposals 或 carry_forward 中处理，不可静默消失；resolved 需要新的实施及验证证据，证据不足用 continue/hold。

两个评审节点不能修改稿件。`review` 是普通 Op：校验两份评审绑定当前 analyze commit、各自匹配 review-join commit，再合并独立检查与意见。任一 open/拒绝不能被另一方通过覆盖；发布保留 review-manifest.json。`gate` 除四项评审及 reviewed_commit 外，还验证五个领域发现的格式/证据文件、本次 join 的分支 commit、时间窗口、提案 ID 唯一性及历史连续性。普通问题返回 analyze；专项产物不完整或评审明确要求补查时返回 audit-context，复用本次采集窗口并重新审查。audit-context 将 gate.txt 保存为 audit-feedback.txt，再进入 fanout，保证各分支收到跨反馈边的具体修正要求；首次审查标记 FIRST_AUDIT。无效引用反馈包括原路径，专项引用使用实际文件，目录清单引用索引。证据存在和 schema 合法不能机械证明语义正确，内容仍由独立评审核查。不使用固定轮数假装任务已经收敛。

修订轮优先读取反馈、当前稿件和相关证据；后续评审优先比较分析 commit 差异与旧问题。正式报告不重复叙述评审轮数/提交ID/逐字修改流水账，过程留在 review.json、gate.txt 和 Git 中。这样避免新增过程性矛盾导致无效回路；重要新事实错误仍必须修正。

limited 仅用于已明确限定的证据缺口或纯文字偏好；已查证的事实错误、矛盾、无依据因果和改动无法满足验收均须退回修订。未采集不等于不存在，文档记载的验收与本轮可独立复核的原始产物分别说明。流程成功也不等于内容通过，仍需源证据抽查。

review Op 还会对提案和报告做确定性安全检查：出现关闭脱敏、取消权限保护、绕过证据校验或回退到更宽暴露面的回滚措辞时直接返回 revise；模型评审不能覆盖该拒绝。

它也拒绝无范围的历史首次或完备因果断言，例如把 invocation=1 写成历史首次、把隔离验收 Run 写成生产完成样本，或把时间相关性写成唯一根因；必须区分当前采集窗口、当前部署定义、隔离验收和未排除的构建差异。

## 安装到本机数据根

构建业务工具后，通过当前 Rust Host 的 Graph 保存和计划 API 安装 [rsi.json](../examples/graphs/rsi.json)。不在服务运行时直接改计划状态文件：

```bash
cargo build --manifest-path rust/Cargo.toml -p anchor-rsi --locked
```

操作员在 `ANCHOR_RUNNER_LOCAL_INPUTS_ROOT` 指定的根下为 `rsi/local-inputs.json` 配置授权。以下路径须替换为实际部署目录；`rsi` 是含业务二进制的目录：

```json
{
  "collect": {"source": "/srv/Anchor", "anchor": "/srv/anchor-data", "runtime-state": "/srv/anchor-state", "rsi": "/opt/anchor/bin"},
  "research": {"rsi": "/opt/anchor/bin"},
  "review": {"rsi": "/opt/anchor/bin"},
  "gate": {"rsi": "/opt/anchor/bin"},
  "publish": {"rsi": "/opt/anchor/bin"}
}
```

业务 Op 使用 `/local-inputs/rsi/anchor-rsi`，Goose 负责 AgentNode 模型循环；Anchor 的现有 Runner 负责反馈、产物及权限。配置 Host 的命令授权和只读挂载后，通过 `POST /schedules` 添加 `{"type":"weekly","weekdays":[3],"time":"09:00"}`，机器时区为 `Asia/Shanghai`。停机或图忙时沿用现有错过语义，不补跑；已有同图同规则计划不重复创建。

升级已有 Graph 时通过保存 API 更新完整定义，并在没有活动 Run 时修改操作员授权。历史提案可由原生 state 中成功发布的结果发现；独立报告根也可通过 `collect --previous DIR` 明确授予。计划和共享示例不保存凭证。

综合与评审可通过现有 `ANCHOR_MODEL_ALIASES` 和角色的 model 字段选择模型。具体模型的语义质量须用失败稿反例与完整流程分别验收，不能以型号代替证据。[原生 RSI bundle](../examples/rust-rsi.md) 使用同一业务模块，并额外将引用绑定到冻结证据哈希和实际成功的 MCP 读取记录。

## 边界和验收

RSI Graph 不自动提交、合并或部署代码，不自动改 Graph/Plugin，不重放历史外部副作用，也不把模型声称完成当成事实。`evolution.json` 的状态为 proposed/continue/hold/resolved；resolved 是有依据的复查判断，不代表本次 RSI 执行过改动。实际修改由维护者在隔离工作区完成，再用保留案例和相关测试验证。

定向验证为 `cargo test --manifest-path rust/Cargo.toml -p anchor-rsi --locked --test business --test evidence`；Graph JSON 静态编译由 `anchor-runtime` 的 shipped-example 作者契约测试覆盖，不启动模型。它们验证正确发布、独立评审拒绝、commit/hash/window mismatch、安全提案门禁、冻结读取和历史 ID 连续性。真实 Goose/provider 的大型业务 Graph、公开业务服务与报告质量需另行验收；历史证据不算本轮新验收，单次报告也不证明长期改进收益。
