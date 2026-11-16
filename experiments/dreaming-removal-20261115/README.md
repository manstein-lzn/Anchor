# Dreaming 专用系统删除验收

2026-11-15，按用户要求删除复杂 Dreaming 专用系统，不设计替代控制器或精简实验平台。本记录只证明源码删减后的本地回归，不代表生产停用或新发行上线。

入库边界：仅保留本摘要与原始 `result.json`；原日志、完整 fixture 回执、切换现场清单和备份均按[本机证据规则](../../docs/development.md#文档归属)保留、未入库。JSON 中的路径与摘要只定位当时原件，不是克隆依赖，也不补签本轮候选。

## 范围

- 删除 Host 专用应用/API/启动安装与协调、调查/实验/发布/观察/撤回、自身 meta、frozen 执行、上下文分支适配、Web 专用页、发行工厂 Graph/质量包及依赖旧生命周期的实验协议。
- 保留 `Kernel::submit`、结构化事实记录、普通 Graph/Runner/Goose/Artifact、Hub 全局扫描/分析/窗口消费，以及独立 `record`、普通 `grade-run` 和 input-discovery 质量工具。
- 原 owner 安全窗口生成只由 Dreaming 调用，随该链路撤掉；Hub 保留格式与显式 DTO 消费，不会自动生成这些视图，也没有新建导出 API。原始目录访问仍需显式授权，不能冒充安全窗口。
- 历史 `dreaming`/`experiment` 身份、机器发布授权和 handover 事实仅保留读取/隔离围栏；不得降级为普通任务启动或恢复。旧 system schedule 不派发，自动 retention 不清除旧 Run 及专用 Artifact。
- 平台源码格式为 5，独立 Graph 包仍为 1；新 Host 不能直接替换进旧 format 4 release。没有部署、迁移、生产模型调用、恢复日计划或重跑真实业务 Graph。

删减统计按接手时工作区快照，不按 Git HEAD；许多专用文件接手时尚未跟踪。准确 Host 统计及验证摘要见 [result.json](result.json)。统计包括测试，不将其表述为纯生产实现行数。

## 实际结果

| 验证 | 结果 |
| --- | --- |
| Cargo workspace 全 target 检查 | 通过 |
| Host binaries 单元 | 1,185 通过，3 默认忽略 |
| Host 全部 integration targets | 277 通过，127 默认忽略 |
| Facts/Eval/Hub/Distribution/GraphHost | 277 通过，8 默认忽略 |
| 六个相关 crate 全 target Clippy | `-D warnings` 通过 |
| 本轮 51 份改动 Host Rust 文件格式 | 通过 |
| 全 workspace 格式 | 未通过；4 份非本轮文件已有格式差异，未覆盖其内容 |
| Web 单元和构建 | 50 文件、401 测试通过；构建通过，仅大 chunk 告警 |
| Web mock 浏览器回归 | 子 Agent 实际执行 5 项通过，不是原生服务或真实模型验收 |
| 固定 Goose + 本地确定性 Provider | 显式选中 3 项均通过，产生 4 份原生场景回执 |

三项原生测试覆盖实际工具反馈及事实提交、未知效果的核查恢复/中止且不重放、普通常驻助手保留 Run/工作区/原生 Session 自动恢复。全部回执记录 `real_model_requests: 0`、`production_data_used: false`，不是 Dreaming A/B 或正向收益证明。

## 命令与原始证据

完整命令、计数、二进制身份及原始证据索引见 [result.json](result.json)。本机原始日志和未改写的 fixture 回执保存在 私有验收目录（本机原件，未入库：`.local/dreaming-removal-20261115`）；不把完整合成对话复制进当前文档。

```sh
cargo +stable check --manifest-path rust/Cargo.toml --locked --offline --workspace --all-targets
XDG_RUNTIME_DIR=/run/user/1001 scripts/test.sh --locked --offline -p anchor-runner-host --bins
XDG_RUNTIME_DIR=/tmp TMPDIR=<独立磁盘临时目录> scripts/test.sh --locked --offline -p anchor-runner-host --test '*' -- --test-threads=1
XDG_RUNTIME_DIR=/run/user/1001 scripts/test.sh --locked --offline -p anchor-facts -p anchor-eval -p anchor-hub -p anchor-distribution -p anchor-graph-host
cargo +stable clippy --manifest-path rust/Cargo.toml --locked --offline -p anchor-runner-host -p anchor-distribution -p anchor-graph-host -p anchor-facts -p anchor-eval -p anchor-hub --all-targets -- -D warnings
npm --prefix apps/web test
npm --prefix apps/web run build
```

原生测试使用已核对摘要的固定 Goose、独立复制的 Host/helper binaries 和临时数据根。实际选中 `goose_acp::native_goose_direct_model_transport_requires_observed_tool_receipt`、`goose_kernel_recovery::kernel_observed_native_effect_accepts_durable_completed_or_abort_without_replay`、`goose_persistent_assistant::host_restart_automatically_resumes_the_instance_with_its_workspace_and_session`，均为 `--ignored --exact --test-threads=1 --nocapture`，没有将未运行的 ignored 套件算作通过。

## 失败与未覆盖

- 首轮 Host 全量的晨报 fixture 因 tmpfs 空间不足失败；改用磁盘临时目录，不修改产品行为。随后一项 fixture 启动出现 `ExecutableFileBusy`，单独复跑通过。
- `writer_lease` 的退出后重入断言前两轮收到空 outcome；增加原响应的失败诊断后，单独及最后全部 integration targets 复跑通过。未改租约产品逻辑，也未证明早轮失败的根因，不写成从无失败或已确定环境原因。
- 全 workspace `fmt --check` 仍指出 `anchor-facts/src/tests.rs`、Host 的 `application/schedule_subscriptions/delivery.rs` 和 `node_tools.rs`、`anchor-sandbox-bwrap/src/lib.rs` 的格式差异；这些不属于本轮删减，不做连带修改。
- 没有新增真实 Provider、外部渠道、生产数据或收益验收，没有重跑完整 native ignored 集、10k 性能、完整平台 source-free 启动、DEB/RPM 安装事务或生产部署。旧 release 不会因源码删除自动改变。
