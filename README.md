# Anchor

**让 Agent 在一张图里完成长期工作，并让每一步都能回看。**

Anchor 是本地优先的 Agent 工作流系统。用 Graph 拆分任务、检查、返工和交付；AgentNode 理解与执行，OpNode 完成确定性步骤，Plugin 提供知识和工具。每次 Run 保留独立工作区、节点历史、工具轨迹和不可变产物。

后端、官方工具和开发检查使用 Rust；AgentNode 与 Pilot 通过 ACP 接入固定版本 Goose，授权工具通过 MCP 提供。WebUI 使用 React/TypeScript。安装与开发入口见 [使用指南](docs/usage.md) 和 [开发指南](docs/development.md)。

<p align="center">
  <img src="docs/images/anchor-graph-editor.png" alt="Anchor Graph 编辑器：带反馈回路的工作流" width="100%">
</p>

## 产品能力

- Graph 创建、编辑、校验、条件路由、反馈、内联子图和配对并行。
- AgentNode 与沙箱 OpNode 共用 Runner、只读输入和取消契约。
- Plugin 的 Skill、资源、stdio/HTTP MCP、Library 管理和授权。
- Run 暂停、停止、继续、历史、文件、Artifact 和删除。
- Pilot Session、SSE 重连、必要提问与回答、原生会话续聊。
- Graph 独立调用、会话交接、定时任务、Webhook 和 Responses 子集。
- 原生企业微信文本网关、Docmost 与学术工具；真实业务验收范围见 [开发台账](docs/pilot-development-plan.md)。

服务中断后，保存的事实和 Goose 原生会话支持 Agent 核查现场后继续。外部副作用的未知结果需要核查；具体研究结论和发布结果由业务 Graph 与使用者验收。

<p align="center">
  <img src="docs/images/anchor-run-evidence.png" alt="Anchor 运行记录：执行路径、节点对话和生成文件" width="100%">
</p>

## 本地启动

需要 Linux x86_64、Rust stable、Git、Bubblewrap、Node.js 20.19+ 或 22.12+，以及 [固定 Goose 二进制](docs/usage.md#模型与-goose)。

```sh
npm --prefix apps/web ci
cp .env.example .env
# 填写 Goose 路径和模型 URL、API key、模型名
./scripts/dev.sh start
./scripts/dev.sh status
```

打开 <http://127.0.0.1:8077>。脚本使用独立的 Rust 数据目录，并管理本次启动的服务；详细配置与首次 Graph 准备见 [使用指南](docs/usage.md)。生产发行包与 systemd 部署见 [部署指南](docs/rust-production-deployment.md)。

## 代码结构

```text
rust/             唯一 Cargo workspace：Kernel、Host、官方工具、打包与开发检查
apps/web/         React/TypeScript WebUI 与浏览器测试
plugins/          官方 Plugin 清单、Skill 和资源
examples/         Graph 与独立 bundle 示例
deploy/           systemd unit 和环境模板
scripts/dev.sh    本地服务管理
docs/             当前文档、验收台账与历史归档
```

## 验证

```sh
cargo +stable test --manifest-path rust/Cargo.toml --workspace --all-features --all-targets --locked
cargo +stable clippy --manifest-path rust/Cargo.toml --workspace --all-features --all-targets --locked -- -D warnings
cargo +stable fmt --manifest-path rust/Cargo.toml --all -- --check
npm --prefix apps/web test
npm --prefix apps/web run build
```

日常运行时回归采用 [确定性小型 Graph](docs/runtime-contract-tests.md)，实际执行 Host、Runner、Goose、MCP 与 Sandbox，并验收历史、workspace 和 Artifact。发行候选通过 [原生候选回归](docs/rust-production-candidate.md) 验证；真实模型和外部业务服务单独验收。

完整导航见 [文档目录](docs/README.md)。Anchor 使用 MIT License，见 [LICENSE](LICENSE)。
