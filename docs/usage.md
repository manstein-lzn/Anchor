# 使用指南

普通用户按本页完成安装、配置模型和使用 Pilot；不需要先阅读架构文档。要从源码启动，请看[开发指南](development.md)。

## 支持环境

source-free 完整平台归档面向 Linux x86_64，目标机需要 Bubblewrap、slirp4netns、Git、shell、CA 证书、系统链接器及发行 inventory 所列的动态库。Rust 与 Node 是构建机依赖，不是目标运行环境要求；系统依赖由操作员准备，归档不会安装它们。

系统必须允许 Bubblewrap 创建隔离 namespace，并在私有 PID namespace 挂载新的 `/proc`。不满足时 Host 报告原因并非零退出，不初始化 Host 数据、恢复 Run 或监听 HTTP，也不自动修改宿主策略或降低隔离要求。通过这项基线检查也不代表全部网络或工具已可用。

模型尚未配置、Python/Rust 尚未准备是正常的首次配置状态，不阻止在沙箱基线可用时打开 WebUI。

## 安装与打开 WebUI

取得经审查的完整平台归档及可信构建报告中的 SHA256。manifest 只是文件 inventory，不是签名；不要只凭归档自己声明的摘要建立信任。操作员使用仓库的安装脚本安装到全新目录，再运行随包 launcher：

```sh
scripts/install-platform.sh --archive /reviewed/platform.tar.gz \
  --sha256 TRUSTED_BUILD_REPORT_SHA256 --install-root /existing/parent/anchor
/existing/parent/anchor/release/anchor-runtime/bin/anchor-platform start
```

安装器不启动服务或注册开机自启，也不覆盖已有目录；不提供在位升级或旧 Python 部署数据的迁移工具。模型、运行记录、工作区和环境放在安装根的私有 `data/` 下，不写入只读 release。不要用全新安装器覆盖已有现场。

长期运行时，由操作员按[systemd 部署步骤](rust-production-deployment.md#run-under-systemd)准备服务账号、数据归属并安装[服务模板](../deploy/systemd/anchor.service)，然后执行 `sudo systemctl enable --now anchor.service`。模板使用同一个 launcher，提供开机自启和失败重启（`Restart=on-failure`）；这不是卡死检测，也不等于所有业务已经过长期稳定性验收。

本机打开 <http://127.0.0.1:8077>。**Anchor 没有内置登录，只监听 loopback。** 在服务器上运行时，从自己的电脑建立隧道：

```sh
ssh -N -L 8077:127.0.0.1:8077 user@server
```

随后在自己电脑打开同一地址。也可由操作员配置带认证的反向代理，不要直接暴露管理接口；长连接代理要求见[部署指南](rust-production-deployment.md#channels-and-remote-access)。

## 配置模型

打开「设置 → 模型服务」，先填好 `default` 服务的地址、协议（`chat` 或 `responses`）、API key 和 `flash` 模型名并保存，即可使用 Pilot。地址接受 HTTPS，或用于本地测试的 HTTP loopback IP 地址；不能包含 URL 用户凭据、query 或 fragment。

`pro` 与 `backup` 可按需配置。两组服务独立保存，密钥不回显、不保存在浏览器；不要把凭据写进 Graph、Plugin 包或聊天。更换服务地址或协议需要重新填密钥，清除后不会退回旧环境值。无需编辑 `.env`。

「测试 Flash / Pro」会发送小请求，可能产生费用；测试不保存草稿，保存也不会自动测试。若显示「已连接，但没有收到完整模型结果」，只证明收到响应，尚未验证模型输出。认证失败、权限错误、限流等按页面的实际错误处理。

**保存后下一 Turn 使用新配置，当前执行中的一轮保持原配置，无需重启。** Pilot 默认使用 default Flash；Graph 未指定模型时同样如此。backup 仅供显式选择，**不会自动接管失败任务**。

未配置模型时发送失败，页面会在确认缺项后提供「前往模型设置」，保留文字与附件，保存后可返回重试。旧配置和恢复凭据的边界见[部署指南](rust-production-deployment.md#health-failures-and-existing-data)。

## 交给 Pilot 搭建 Graph 和 Plugin

Pilot 可以用自然语言查询、创建和管理 Graph、Run、Plugin 与产物。把 Graph JSON、Plugin 工程文件或它们的压缩包作为附件交给 Pilot，让它先检查内容、依赖、权限和当前环境，再协助配置；这不是任意压缩包都能自动安装的保证。

完整平台从空业务 Graph catalog 开始，不预装全部业务示例。Plugin 先安装到 Library，再按需要挂载；安装和启用是两回事，挂载变更从后续 Turn 生效，说明文件本身不安装环境或授予权限。源码启动的示例用途见[上手示例](../examples/README.md)。

需要 Python/Rust 时，在「设置 → 开发环境」初始化，明确允许联网下载后再执行；可查看进度、停止、失败后重试。关闭页面不会取消初始化，重新打开可继续查看状态。已有 Python 可点「刷新状态」检测；已有解释器不代表全部依赖工具齐备，缺项以页面说明为准。

安装第三方 Plugin 前会审阅来源、工具与权限，需要时由用户确认；服务账号、凭据及外部依赖授权仍由用户提供。凭据在插件详情中填写和测试，不要直接发给 Pilot。「已保存」只表示配置已保存，「工具发现成功」也不证明具体业务操作成功。显示「由部署管理」的字段由操作员维护。

Graph 是可传播的 JSON 定义，Plugin 是包含 `plugin.json`、Skill 和资源的工程，可声明 stdio/HTTP MCP 与运行依赖；分发资产不应包含用户凭据。编写或分发自己的资产时，再查[Graph 作者契约](rust-graph-authoring-contract.md)、[Plugin 契约](plugins.md)和[Pilot 开发](pilot-development.md)。

## Graph、运行与常驻机器人

- 在 WebUI 创建或打开 Graph，编辑节点、连线、布局和所挂 Plugin，保存后校验。「运行工作流」填写 JSON 对象输入，运行会冻结当时的定义和输入，不随后续编辑改变。Graph/Run 页面可把对象引用带入 Pilot 草稿，发送后请它协助。
- 「插件库」支持从本机目录或 GitHub 目录链接预检、安装与更新；展示来源、权限与替换影响后确认。操作中离开或刷新页面不等于取消，可返回查看进度。卸载前也会检查运行/会话引用与影响。
- 「运行 → 定时计划」支持一次性、固定间隔、每天、每周和每月，按服务所在时区执行。重新启用只安排未来时点；暂停或删除计划不删除既有 Run。
- 企业微信按**一张常驻助手 Graph 一个机器人**配置，各 Graph 分别启停，不是整个 Host 只有一个入口。同一个 Bot 账号不能同时被两张已启用的 Graph 使用。「运行看板 → 消息会话」管理各对话实例；连接、成员和通知配置见[企业微信指南](wecom-assistant.md)。Host 监管网关，不要重复启动独立网关。

## 对话、历史与恢复

Pilot Session 保存长期对话与运行关联；刷新会接回同一 Turn，**关闭页面不等于停止执行**。必要提问在当前 Turn 内回答，删除确认绑定具体目标；服务重启不会重放旧确认，回答已保存也不代表外部操作已经完成。

Run 页面可查看路径、状态、节点对话、工具请求/结果、workspace 与 Artifact。暂停/停止请求先保存，节点收束后才显示最终状态；删除受活动执行、会话与 Graph 调用引用限制。

服务或进程中断后，先看已保存状态和外部现场，再让 Agent 核查并继续原 Run。**对结果未知的发送、写入等操作，不盲目重放。** 重启服务、恢复 Run 和外部动作完成是三件不同的事。

已结束 Run 和未受保护的历史交付默认按 30 天或 20 GB 容量限制回收；当前执行/恢复所需内容受保护，Harness 事实与评分不随之回收。需要长期保留的文件应另行备份，详细规则与调整入口见[保留策略](design-cards/003-data-retention.md#8-批准与实现2026-10-23)。

## 日常操作与常见失败

按模板配置 systemd 服务后，使用系统原有命令；手动启动的 launcher 不会因执行这些命令而自动纳入 systemd：

| 操作 | 命令 |
| --- | --- |
| 查看服务状态 | `systemctl status anchor.service` |
| 最近日志 / 持续查看 | `journalctl -u anchor.service -n 100` / `journalctl -u anchor.service -f` |
| 启动 / 停止 / 重启 | `sudo systemctl start anchor.service` / `sudo systemctl stop anchor.service` / `sudo systemctl restart anchor.service` |
| 检查开机自启 | `systemctl is-enabled anchor.service` |

私有数据保存在 `<install-root>/data`；标准服务模板使用 `/var/lib/anchor/platform/data`，不是只读运行时文件的一部分。停止 writer 后再做一致性备份；不要让手动 launcher 与 systemd 服务同时写同一份数据。

| 现象 | 处理 |
| --- | --- |
| `Can't mount proc ... Operation not permitted` 等沙箱基线错误 | 当前系统不满足运行条件；查看日志定位系统策略，不改成共享 namespace 或宿主 `/proc` 绕过 |
| 服务未启动或没有开机自启 | 核对服务模板、账号与数据权限，按部署步骤启用；完成归档安装不代表服务已启动 |
| Pilot 提示默认模型未配置 | 在「设置 → 模型服务」保存 default Flash，返回原草稿重试 |
| Plugin 缺依赖、凭据或权限 | 在开发环境/插件详情补齐所需配置或授权，不能用安装成功替代运行验证 |
| 运行中断或动作结果未知 | 查看原 Run 和外部效果，让 Agent 核查后继续，不覆盖安装或重复发送 |

归档构建和确定性回归不代表目标机、真实模型、第三方服务或 Pilot 盲装已端到端通过。验收边界见[当前结论](pilot-development-plan.md)；API 集成、打包和运维参数分别见[完整文档目录](README.md)。
