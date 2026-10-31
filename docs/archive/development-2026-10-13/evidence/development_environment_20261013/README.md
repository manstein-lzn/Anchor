# 私有 Python 开发环境验收

本目录保存主 Agent 的独立验收入口。Runtime 开发由 GPT-6.1 Sol / xhigh Agent 完成，主 Agent 负责架构边界、源码 review 和真实 Provider 验收。方案见 [归档修订稿](../../development-environment-freedom-plan-revised.md)，当时验收状态见 [历史台账](../../pilot-development-plan.md#开发环境自由度验收a225)。

## 真实 Provider 场景

`real_provider_acceptance.py` 只编排 Rust Host、固定 Goose ACP 和原生授权 MCP，不替代模型响应。它读取 `.env` 的白名单配置作为数据，创建必须全新的隔离根，暂存冻结 Host 与网络 helper，使用独立 state/workspace/Library/catalog/home/build；已有服务、Session 和实验不参与验收。

实际链路：创建 `src/`/pyproject 项目 → 空私有 venv/pip → 离线 PEP 660 editable → 执行 console script → 精确修改源码并重测 → 默认 ensure 保留现场 → 整理独立候选 → 候选 MCP 业务调用 → 原生确认安装 → AgentNode 正式调用及产物 → 删除开发 Session → 新 Pilot Session 挂载并实际调用 → 新 AgentNode Run 再调用。

开发 fixture 复用 Rust 测试的源码，移除其第三方依赖。正式候选明确声明本地合成的 `formalprobe==1.0` wheel，由现有 Host 求解、来源校验和共享 store/view 接线准备；MCP 实际导入正式包，并断言私有 venv 和 editable 包不存在。它还加载复制到候选的修改后源码，因此随机参数、源码值、正式依赖值、原生结果与模型产物可以相互核对。

```sh
python3 real_provider_acceptance.py \
  --host rust/target/debug/anchor-runner-host \
  --root .local/development-environment-acceptance-20261013/real-<新的唯一标记>
```

脚本只确认当前 fixture Plugin 的现有安装表单。它不批准网络、凭据或其他 Plugin；安装和开发行为均来自这次明确授权的隔离验收。原生 MCP 证据按精确 Run ID 和实际参数匹配，删除后的调用不能复用删除前的结果。配置、解释器、Goose、uv、Host、helper、fixture 和 wheel 摘要分别保存；API key 不进入概要和终端输出。

## 证据与范围

每次隔离根保存 `identity.json`、各阶段 Turn/SSE/调用、候选结果、Run/产物、原生 MCP 证据、Session 删除事实及最终 `evidence.json`。失败尝试保留独立根，不与成功混为同一轮。

2026-10-13 主 Agent 验收通过：真实 `deepseek-flash` 的两个 Pilot Turn、两个正式 Graph Run 全部 completed；删除开发 Session 后新 Session 及新 Run 均实际调用正式依赖与修改后源码，两个 Run 各有一次原生业务调用，产物逐项一致。原生安装表单实际出现并确认，两个临时 Session 删除 HTTP 200；私有选择和原 workspace 消失，正式 Plugin/view/store 保留。隔离 Host exit 0、监听关闭、未见所属进程残留或 Host panic；`.env` 与共享解释器摘要不变。完整根 `.local/development-environment-acceptance-20261013/real-architecture-01/`，概要 [final-real-provider.json](final-real-provider.json)。

最终 Host SHA256 `e6c67c7d5b899eeb61226572a092250b22d5bf38cf8e9e10c651105ab5ad59c5`，固定 Goose 1.53.0 SHA256 `71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7`。最终确定性闭环与真实 Provider 使用同一 Host；较早 standalone build 的 `2663d200…` 不是最终验收身份。开发者的 16 个源码/fixture 字节已独立核对，见 [runtime-source.sha256](runtime-source.sha256)。

实际 Rust 命令、退出码、日志位置和摘要见 [checks.json](checks.json)。定向 Sandbox **5 passed**、平台状态 **2 passed**；三个相关 crate all-targets **41 targets：995 passed / 4 failed / 91 ignored，exit 101**。三项 Graph 删除用例报 catalog 锁冲突，一项旧开发视图用例在释放时遇 writer 竞争；受影响子集独立复跑 **27 + 1 passed，exit 0**，没有修改运行策略或伪造全量通过。全量包含新 selection 丢失断言；最后状态指引和等价 Clippy 条件整理之后，状态定向、all-targets Clippy `-D warnings`、fmt、build 与最终 Goose **1 passed** 均通过。最终确定性证据摘要见 [final-deterministic.json](final-deterministic.json)。ignored 不算已执行，本次无 Web 改动，未运行 Web 检查。

早期确定性 fixture 的三轮失败分别涉及覆盖已有文件缺少编辑基线、无依赖候选用未授权 Python 别名、精确解释器 basename 未获 allowlist 授权；只修正 fixture 调用和隔离授权，未扩大正式候选契约。日志 `/tmp/anchor-development-environment-goose{,-02,-03}.log` 及对应 evidence 根保留。初次 Clippy 的 `collapsible_if` 失败也保留；修正后的检查单独记录。真实 Provider 本轮一次通过，未用确定性模型响应替代真实返回。

确定性回归使用真实 uv/Python/pip/Bubblewrap，以及实际 Host/GraphRunner/Goose/Library；模型传输由本地 fixture 替换。其覆盖范围包括包安装/升级/卸载、项目隔离、文件语义、editable、旧视图、store 不变、网络拒绝、锁等待、取消及重启观察。它与真实 Provider 证据分别记录。

仅剩解包 store、缺少 wheel/源码时，旧只读视图可继续使用，私有空 venv/pip 可以离线重建，私有包不能直接从 store 重新安装。恢复包需可获取的安装来源，工具不会自动回退旧视图。本次没有第三方构建后端/native 包兼容性、reflink/空间收益、大依赖压力或线上部署验收；正式安装仍没有强制成功业务测试/测试依赖锁门禁。

2026-10-13 后续按用户明确授权部署到当前开发 Host，并清空 Pilot、企业微信和所属 Goose 对话历史。构建源码与上述 16 项摘要一致；release SHA256 `50331ca988d1991b4f97052588401f68b4b55b6c78d8fde98fef2fd12b56f104`，实际运行身份与部署文件相同，health/ready 正常，13 张 Session 表全为空。4 个 Graph、5 个正式 Plugin、共享包与配置保留，非对话 RSI Run 保留。本次未重复真实 Provider 业务；部署事实与 A225 验收身份分别记录，见 [deployment.json](deployment.json) 和台账 A226。
