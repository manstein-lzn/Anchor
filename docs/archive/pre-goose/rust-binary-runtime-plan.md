# Rust Binary Runtime 产品化计划

> 历史快照：保留当时设计与证据，不代表当前实现或待办。当前入口见 [文档目录](../../README.md)。

> 历史迁移资料：io-harness/Rig 源码、实验原型与回归入口已移除，本文中的旧命令和路径仅用于追溯当时证据。当前执行、开发和部署以 Goose-only 迁移决定（历史路径 `goose-runtime-migration.md`）、[当前架构](architecture.md) 和 开发台账（历史路径 `pilot-development-plan.md`） 为准；旧源码可从 Git 历史回查。

> 第一阶段已于 2026-10-05 收口。本文保留目标、验收标准和实际证据；当前未完成的产品平台迁移边界见 [Rust Runtime 迁移边界与当前收口状态](rust-migration-closure.md)。

## 目标

交付一个核心执行不依赖 Python、Anchor Runtime 源码或 Python 虚拟环境的 Rust Runtime 二进制，并与一个明确声明资源闭包的 Graph 包组合部署。既有 Plugin 保持原样，可使用部署者提供的 Python、Node.js、脚本环境或 MCP；不要求将业务工具改写为 Rust。第一阶段只证明一个真实 Graph 可以被交付和运行，不承担完整 Anchor 平台迁移。

这个目标解决的是交付和部署问题：减少核心源码暴露，消除 Runtime 自身的 Python 依赖链，固定 Runtime 版本，降低部署环境差异。Plugin 外部依赖按实际选用能力配置。Rust 是否改善整体执行效率必须通过实测确认；模型和外部服务延迟占主导的 Graph 不能预先宣称会明显提速。

## 第一阶段范围

第一阶段交付一个 Linux 发行包，包含：

- 一个版本固定、可重复构建的 `anchor-runner-host` release binary；
- 一个 `manifest.json`、展开后的 `graph.json` 和精确 Plugin 资源闭包；
- 不含密钥、凭证、运行状态和工作区；
- 部署配置示例，以及明确的系统前置条件（至少包括 Bubblewrap 和目标 Linux 运行库）。

首个验收 Graph 采用串行 Op → Agent → Op 形态，可带一个本地 fixture Plugin，使用已有 GraphRunner、io-harness、Bubblewrap 和 Artifact 事实。第一阶段暂不把 fanout/join、`Op.call detach`、Session、Scheduler、Channel、Graph 在线编辑和外部业务 MCP 作为交付前置；Graph 声明了未支持能力时必须在启动前拒绝，不能静默降级。

Graph JSON、Plugin manifest、Plugin 脚本和业务资源是否需要进一步编译或加密，属于独立的交付保护问题。二进制 Runtime 只解决 Anchor Runtime 源码和 Python 依赖不随交付暴露，不能自动隐藏所有业务资源。

## 产品要求与验收

| ID | 要求 | 验收证据 | 阶段 |
| --- | --- | --- | --- |
| BR-1 | release binary 可重复构建且版本固定 | 在干净构建环境生成相同版本和校验摘要；核心 Runtime 不加载 Python 包，Plugin 依赖独立提供 | walking skeleton |
| BR-2 | Graph 包资源闭包完整且不含密钥 | manifest、Graph、Plugin 资源逐项校验；包外路径、symlink、credential 文件拒绝 | 发行包 |
| BR-3 | 干净环境可执行真实 Graph | 只安装 binary、系统前置依赖、Graph 包和部署配置；真实 provider 返回完成结果 | 真实端到端 |
| BR-4 | Sandbox 和 Artifact 边界成立 | 节点只能访问授权输入；产物经过 commit/hash 校验；宿主路径和凭证不可由 Graph 扩大 | 真实端到端 |
| BR-5 | 重启与恢复可核验 | 在节点边界和已记录中断点重启；同一 Run 按持久事实继续，未知副作用不自动重放 | 恢复验收 |
| BR-6 | standalone 入口行为可解释 | 不支持的 `Op.call`、并行或权限配置在 admission 阶段明确拒绝；不产生悬空 child 或伪成功 | 负例验收 |
| BR-7 | 交付收益可量化 | 与当前 Python 基线比较安装体积、启动时间、常驻内存、依赖数量和代表任务耗时 | 结果决策 |

BR-1 至 BR-6 全部通过，且 BR-7 显示至少一项明确的交付或运行收益，才进入第二个 Graph。只有多个真实 Graph 通过，才重新讨论平台级 Rust 替代。

## 当前进度

2026-10-05，A90 已验证既有 Plugin 的外部环境接线：最终 release binary 原样执行 `plugin-research.json` 和 academic-research Plugin，通过现有 Python scholarly 工具查询真实 Crossref，保存原始检索 JSON 与 research.md Artifact；真实 Provider 为 DeepSeek Flash，证据 `.local/rust-plugin-reuse-gx8k_ms9/evidence.json`。外部环境由 `ANCHOR_RUNNER_LIBRARY_ROOT` 的既有 `tools/<id>/tool.json` 授权提供，未复制新的 Plugin 格式或业务实现。该证据覆盖真实检索接线，不等于完整学术调研、其他业务 Graph 或外部业务 MCP 验收；环境内容尚未冻结进 Run，部署者需保持运行和恢复期间的依赖稳定。

2026-10-04，walking skeleton 已通过：`cargo build --release --bin anchor-runner-host` 生成约 40 MB 的 Linux release binary；`cargo test -p anchor-runner-host --test routing` 的 2 项测试验证默认 Graph input 与触发 input 的合并结果通过 `ANCHOR_INPUT` 到达串行 Op；`cargo clippy -p anchor-runner-host --all-targets -- -D warnings` 和 `cargo fmt --all -- --check` 通过。另在不含 Python 运行时环境变量、只提供 `/usr/bin:/bin` 的临时根目录中，使用 binary、format-1 Graph bundle、Bubblewrap 和 `sh` 完成一次 provider-free Run，响应与持久 Run 均为 `completed`。

同日已补齐第一版发行包工具 `scripts/package_rust_runtime.py`：它接收 release ELF binary 和 format-1 Graph bundle，生成确定性 `tar.gz`，仅包含 binary、bundle、部署说明和无密钥运行清单；会拒绝 symlink、未知顶层资源、Plugin 目录漂移、非 ELF 文件和非法 manifest。`tests/test_package_rust_runtime.py` 的 3 项边界测试、Ruff 和 `py_compile` 通过；同一输入连续生成的归档摘要一致。将该归档解压到不含仓库源码和 Python 路径的临时目录后，实际启动二进制执行 provider-free Graph，Run 为 `completed`，Artifact 中存在 `result.txt`。

发行包验收已固化为 `scripts/rust_runtime_package_smoke.py`。它自动打包、检查归档中没有 Python/源码路径、解压到独立 deployment root，并用最小环境启动 binary，保存 response、Run 路径和 Artifact 路径。当前 provider-free smoke 已通过，最近证据保存在 `.local/rust-runtime-package-4okpp2ld/evidence.json`；该脚本是后续真实 Provider 与基线测试的共同入口。

真实 Provider 验收已接入同一发行包路径：`ANCHOR_RUST_PACKAGE_RUNTIME=1 ./.venv/bin/python scripts/rust_multinode_smoke.py` 会先打包 release binary，再从解压后的 `anchor-runtime` 运行 Graph。最近一次通过证据为 `.local/rust-multinode-t2_n_qn4/evidence.json`：5 次 Provider 请求、`deepseek-flash`、本机 Rust HTTP MCP fixture、Plugin 工具调用顺序正确，最终 Artifact 校验通过。这里的 MCP 仍是本机 fixture，不是业务 MCP。

发行包恢复验收也已通过：`scripts/rust_runtime_recovery_smoke.py` 启动打包后的 HTTP Host，暂停 Run，停止并重启同一个 binary，再续行原 Run。最近一次证据 `.local/rust-runtime-recovery-cs0irsvt/evidence.json` 显示 Run 从 `paused` 继续到 `completed`，并生成 `second.txt` Artifact。该测试覆盖的是已持久化暂停点的恢复，未知外部副作用和跨存储故障窗口仍需单独验收。

首轮 Python/Rust provider-free 基线已由 `scripts/rust_python_baseline.py` 固化。相同的两节点 Op Graph 重复 5 次，最近证据 `.local/rust-python-baseline-tqkga_ov/evidence.json`：Python 中位执行时间约 0.245 秒、峰值 RSS 约 25,256 KiB；Rust packaged binary 约 0.737 秒、峰值 RSS 约 13,748 KiB。Rust 发行归档约 14.3 MB，本机 Python 虚拟环境磁盘占用约 1.03 GB。这个结果是冷进程、Sandbox 和持久化路径的端到端测量；Python 仍使用 Git 记录，Rust 使用 fs2 Artifact/事实记录，不能直接解释为语言级 microbenchmark，也没有覆盖长驻 HTTP Host 的 warm-run。它表明当前简单 Op 场景没有 Rust 速度优势，但有明显的交付体积、依赖和常驻内存优势；模型网络场景仍需单独测量。

这关闭了 BR-1 的初步构建、BR-2 的 provider-free 资源准入、BR-3 的一次真实 Provider 发行包路径、BR-5 的持久化暂停点恢复，以及 BR-7 的首轮交付/资源基线。2026-10-05 的收口回归补齐了 standalone `Op.call detach`、`Op.call session` 和并行分支内 `Op.call` 的启动前拒绝；HTTP 宿主对 wait/detach/Session 的既有路径仍由 workspace 测试覆盖。bundle loader 的负例覆盖 symlink、资源漂移、包外路径和 Plugin 凭证，Sandbox/Artifact 的授权输入与提交边界也由 workspace 测试覆盖。收口验证另通过 provider-free 发行包 smoke 和打包 HTTP Host 的暂停、进程重启、同一 Run 续行 smoke；证据位于 `.local/rust-runtime-package-ip7s0veu/evidence.json` 与 `.local/rust-runtime-recovery-qzor4wv0/evidence.json`。

这些证据不等于完整 BR-4/BR-5 的生产部署安全或跨存储故障窗口验收，也没有验证不同 Linux 发行版。尚未证明真实业务 Plugin/MCP 资源闭包或四个业务 Graph 的逐一替代；不同 Linux 发行版兼容性暂不属于本阶段范围。

## 必须先收紧的现有边界

这些问题会直接影响二进制交付的可信度，应在第一阶段相关路径中解决，或由 admission 明确禁止：

- standalone 的 detach 和 Session handoff 在入口 admission 阶段拒绝；持久 HTTP 宿主才提供这些生命周期能力。
- 可恢复 Harness 错误不能被无条件投影为 Failed；若第一阶段不支持恢复，必须在能力声明中拒绝该 Graph。
- Agent `model`、`network` 等字段不能静默忽略；没有 alias 或权限映射时应明确拒绝或固定声明部署级语义。

bundle loader 已拒绝 Plugin 根 symlink 逃逸，Rust HTTP 的 `ANCHOR_API_KEYS` 也已采用 JSON 数组、唯一和至少 32 字节校验；通用 Library catalog 的兼容 symlink 行为不属于可分发 bundle。

Graph 编辑 DTO、完整平台 trace、Session/Turn 和 Scheduler 不属于第一阶段；它们只有在二进制 Graph 运行价值被证实后才重新评估。

## 推进顺序

1. **Walking skeleton**：从现有 `anchor-runner-host` release binary 和 format-1 bundle 开始，在无 Python 的临时根目录运行一个 provider-free 串行 Graph。
2. **干净部署环境**：在本机创建不含仓库源码的独立 deployment root，只放 binary、Graph 包和部署配置，使用最小环境变量与 PATH，验证路径、权限、状态根和工作区边界。当前阶段固定在已验证的 Linux 部署环境，不扩展发行版兼容矩阵。
3. **真实端到端**：接入真实 provider，执行 Bubblewrap、Plugin fixture、Artifact commit，并保存证据。
4. **恢复与负例**：覆盖重启、未知副作用、资源漂移、包外路径、凭证文件、未支持能力和重复 Run；修复或明确拒绝所有悬空状态。
5. **发行包与比较**：生成可复制的压缩发行包，记录系统前置条件，并与 Python 基线做体积、启动、内存和任务耗时比较。
6. **阶段决策**：通过 BR-1 至 BR-7 后再选择第二个真实 Graph；任一核心门槛失败就停在实验 Runtime，不开启平台迁移。

## 非目标

- 不在第一阶段迁移 Python `serve.py`、Session/Pilot、Scheduler、Channel 或现有 WebUI。
- 不复刻 Python 内部文件格式、Harness 内部格式或旧 API。
- 不建立第二个 Graph Runner，不为兼容性提前引入通用安装器、市场或远程控制面。
- 不把二进制交付等同于业务源码保护、完整多租户安全或性能必然提升。

当前实现证据和未完成平台边界见 [Rust Runtime 迁移边界与当前收口状态](rust-migration-closure.md)。
