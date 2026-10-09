# 小型 Runtime Graph 回归：Goose 确定性入口

当前标准回归使用现有纯 Rust `anchor-devtools`，实际经过 Host、共享
GraphRunner、Goose ACP、授权 MCP、Bubblewrap 和 Artifact，只替换模型传输及
远程业务 endpoint。官方执行和回归入口均在 Rust workspace；历史验收证据
保留在台账和归档中，不自动成为当前源码的验收。

## 标准命令

从仓库根目录运行，显式指定已经审查的 Anchor 自建 lean Goose ACP 二进制（[构建脚本](../scripts/build-goose-acp.sh)，上游 v1.53.0 源码）：

```sh
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
CARGO_TARGET_DIR=/tmp/anchor-goose-regression-target \
CARGO_BUILD_JOBS=2 \
cargo +stable run --manifest-path rust/Cargo.toml -p anchor-devtools --locked -- \
  regression goose-fixture --evidence-root /tmp/anchor-goose-regression
```

`regression fixture` 是同一 `run_goose_fixture` 的 CLI alias，执行相同 Goose
套件；不是旧框架开关。兼容 `run_fixture` API 也委托该函数，不另写 Runner。

需要固定依赖支持的 stable Rust、Linux、Git、Bubblewrap 和可用的
user/mount/network namespace。入口在测试前校验 Goose 可执行权限和 SHA256
`71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7`，
不下载 binary、不加载 `.env`。缺 binary、digest 不符、构建失败、必要测试
未执行或证据缺失均不能算通过。

| 参数或环境 | 契约 |
| --- | --- |
| `--workspace-root` | 仓库根；默认编译时的仓库根 |
| `--target-dir` | 默认 `CARGO_TARGET_DIR`，未设置时为 `/tmp/anchor-native-regression-target` |
| `--evidence-root` | 新私有证据目录的 parent，默认系统临时目录 |
| `ANCHOR_GOOSE_BINARY` | 已验证的固定 Goose 可执行文件的绝对路径 |
| `CARGO_BUILD_JOBS` | 编译并发，可按机器资源设置；不是模型请求限制 |

不同编译工作流使用独立 target，不同时覆盖正在采证的 Host binary。
每次执行创建新 `anchor-goose-regression-*` 根，不从旧证据目录推断本次通过。

## 套件与证据

入口先构建官方 WeCom、Docmost 原生工具，再逐套运行以下既有 Host 测试，
使用 `--locked`、`--no-default-features` 和
`-- --ignored --test-threads=4 --nocapture` 选择需固定 Goose 的确定性场景。
这是显式运行带 ignored 标记的场景；选中后仍忽略的测试不能通过验收。
普通 helper 的过滤计数与场景通过数分别记录。

| 套件 | 核查边界 |
| --- | --- |
| `goose_acp` | 实际 Agent loop、结构化完成与回执、工具权限、取消、未知效果核查后继续及保留的 spike 反例 |
| `goose_pilot` | 原有授权工具、同原生 Session 续聊/重启、幂等、SSE 重放、变更后的现场核查与取消 |
| `goose_elicitation` | 原生问答、条件删除确认/拒绝、目标变化、停止和重启不重放确认 |
| `goose_media` | MCP PNG/JPEG/WebP、混合内容顺序、大图、模型 wire 图片、媒体拒绝、Artifact 与重启核查 |
| `goose_conversation` | 跨 Run 与循环原生历史、用户/节点隔离、只读前驱、冻结图片、身份拒绝与整图清理 |
| `goose_channel` | 私有 Unix gateway、原生发送身份、相同正文不同调用、越权拒绝与丢 ACK 后不重发 |
| `goose_compaction` | Goose 原生压缩、Plugin/现场重读、取消、重启和同 prompt 超窗恢复 |
| `goose_pilot_compaction` | Pilot 压缩、续聊/重启、显示历史/SSE、摘要取消和变更后核查 |
| `goose_trace` | prompt 活跃期原生文本/工具结果、状态配对、停止与重启后保留显示和 Artifact |
| `goose_session_calls` | wait/detach、child Run、Session 映射、local ACK、重启结算及 yield/前台/续行 |
| `goose_library` | 本地 Library 安装后实际 Goose 小图接线 |
| `native_plugins` | 实际只读 Skill、原生 stdio MCP、loopback 业务结果及 workspace/Artifact |

模型和业务服务均为本地确定性 fixture，不调用真实模型、不发送公网消息、
不发布文档、不读取生产数据。视觉模型名和 fixture usage 仅驱动实际 Goose
的传输/压缩分支，不证明真实图片理解或真实 token 费用。

顶层 `evidence.json` 保存实际命令、退出码、构建结果、测试计数、耗时、
固定 Goose 身份和失败原因。日志含 `fixture.log` 与每套独立日志，场景根在
`fixture/<suite>/`。每个通过测试必须有一份有效场景证据，匹配本次 Host
binary、固定 Goose digest、已完成且无错误的确定性 Provider transcript，
并声明零真实模型调用及未使用生产数据。启动前权限拒绝的反例按原有独立
契约核查，不能冒充实际 Goose Graph 执行。

场景核查实际 Run/节点历史、工具请求与结果、workspace、Artifact manifest
及文件 bytes/hash。Goose 原生历史由 Goose 持有；ACP 显示投影不是另一套
恢复日志。Runtime 通过公开 ACP 恢复。缺 usage 是未知，不写成零成本；
模型调用、完成声明和业务内容质量
分别记录。

## 浏览器与真实验收边界

浏览器定向入口保留 `apps/web/e2e/goose-native-trace.spec.ts` 与
`pilot-goose-elicitation.spec.ts`，显式配置 `ANCHOR_TEST_GOOSE_HOST_BINARY`
和 `ANCHOR_GOOSE_BINARY` 后运行实际 Rust Host/Goose、本地 Provider。
`goose-trace-media.spec.ts` 只验证 UI 投影。需先构建 WebUI；缺配置而跳过
不算通过，UI 图片解码也不等于真实 vision 端到端验收。

```sh
ANCHOR_TEST_GOOSE_HOST_BINARY=/absolute/path/to/anchor-runner-host \
ANCHOR_GOOSE_BINARY=/absolute/path/to/goose \
ANCHOR_BROWSER_BINARY=/absolute/path/to/chrome \
ANCHOR_REAL_PROVIDER=0 \
npm --prefix apps/web run test:e2e -- \
  goose-native-trace.spec.ts pilot-native.spec.ts pilot-goose-elicitation.spec.ts
```

`ANCHOR_BROWSER_BINARY` 可省略以使用已安装的 Playwright 默认 Chromium。
其他原生 spec 使用同一隔离 Host/Provider helper；`real-provider.spec.ts`
只有显式 `ANCHOR_REAL_PROVIDER=1` 才进入真实模型验收。

固定 Goose 未暴露 `final_result.summary` 参数的逐 token 增量，普通 assistant
text 不能冒充 canonical 完成摘要。Pilot 手动 `/compact` 的公开接入和任意
长上下文、多次压缩、摘要错误及媒体/问答组合应按当次证据单独判断。

真实 Provider 的自主工具选择、完成/反馈、当次 model/wire 兼容性须另作
小型端到端验收；确定性回归不能关闭这类验收。完整 RSI、深度研究、周报，
真实 OAuth、公网 WeCom/Docmost/学术服务和完整渠道媒体组合仍是低频业务
验收，本轮暂缓。不为维护每个旧 smoke 另建 Runner 或扩大常规回归 Graph。
目标机发行依赖、旧数据策略、单写切换/回滚和生产授权也分别验收。

当前本地 source-free 候选包检查见
[Rust Production Candidate](rust-production-candidate.md)；
部署与数据切换边界见 [部署指南](rust-production-deployment.md) 和
[切换准备](rust-production-cutover.md)。运维预检与旧数据盘点也通过原生
`anchor-devtools preflight` / `cutover` 执行。

## 历史证据

旧 Runtime contract、原生 Pilot/Plugin live 驱动、三份 live fixture Graph 和
依赖 Harness 私有记录的 Rust 业务 smoke 已移除；其命令不能用于当前源码。
旧指南与代码可在 commit `45d4bbfa189aafb5a41bd8a8b05295e98749ca6b` 查看，
例如 `git show 45d4bbfa189aafb5a41bd8a8b05295e98749ca6b:docs/runtime-contract-tests.md`。
[开发台账](pilot-development-plan.md) 和归档中的旧 io-harness/Rig 记录保留，
不改写为 Goose 已通过。

已有 Goose 历史验收也保留其原范围：A119 七套 44 场景；A121 九套 51 场景，
含七项压缩组合；A122 十一套 57 场景；A124 十二套 59 场景、零真实模型、
345.323 秒。A124 证据根为
`/tmp/anchor-g3-parallel-iXLuGH/anchor-goose-regression-mKrmns/evidence.json`。
A119 的真实 Chat 两轮/重启、A121 的真实短文本压缩/重启证据只证明当时的
配置和短会话，不表示清理后的源码重跑通过或所有 Provider/vision/生产验收
已完成。实际推进和新验收结果继续维护在开发台账。

## 维护约定

新增或修改 Runtime 产品能力时，复用现有小图、确定性 Provider 和故障控制
用例，按变化更新覆盖范围。涉及真实模型行为时另保留定向真实证据。
失败、跳过、未配置 Provider 和未覆盖边界如实区分；不凭绿色测试总数声称
全产品通过，不重建通用 Agent loop、上下文或恢复引擎。
