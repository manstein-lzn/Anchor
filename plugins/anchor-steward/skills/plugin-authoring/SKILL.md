---
name: plugin-authoring
description: 适配、准备、自测和审查 Anchor Plugin；安装与挂载保留原生确认。
---

# Plugin 开发

先用 Pilot `plugin_list` / `plugin_read` 检查已安装能力，再决定复用或开发。管理读取不授予执行权限。候选放本 Session `/workspace`；`anchor_read` / `anchor_edit` 的 `path` 和候选工具的 `directory` 均为工作区相对路径。覆盖已有文件前先读取并遵守实际编辑 schema 的摘要检查。

## 清单与边界

- 必需 `plugin.json`，至少 `name`；ID 由安装目录/安装参数决定，不添加根字段 `id`。纯 Skill 可只写 `name`、`description`、`skills: "skills/"`，并提供 `instructions.md` 与 `skills/<名称>/SKILL.md`，不需要 MCP。
- 当前根字段白名单：`name`、`description`、`version`、`author`、`homepage`、`repository`、`license`、`keywords`、`interface`、`skills`、`mcpServers`、`configuration`、`dependencies`、`network`、`mutable`、`hooks`、`commands`、`agents`、`apps`。最后四项可被识别但不是额外执行入口；不要把根白名单误用于 server 内部字段。未知字段会报错，以 `plugin_review` 的实际诊断修正，不猜参数。
- MCP 可在 `mcpServers` 或外置 `.mcp.json` 声明；stdio 用 `command` / `args`，HTTP 用 `url`，入口优先插件内相对路径。Python 正式依赖只写顶层 `"dependencies": {"python": ["idna==3.6"]}`，视图解释器为 `${ANCHOR_PLUGIN_VIEW}/bin/python`；不再使用旧 `environment` / `anchor.environment`。
- 可变数据以根字段 `mutable` 显式声明相对路径；不能覆盖清单或 MCP 配置。不要借此修改代码。包只交付普通文件/目录，不依赖链接、`.git`、开发 venv 或宿主绝对路径。Rust 二进制同时带源码、锁文件和构建说明。
- `configuration.fields` / `configuration.environment` 声明所需连接字段；凭据通过 Host 专用表单或 OAuth 配置，不进入源码、命令、URL、聊天和包。候选测试不继承同 ID 正式 Plugin 的凭据或 OAuth。不要复制凭据或自行读取宿主环境。
- 联网需求写根字段 `network: true`（HTTP MCP 自动产生需求）；这只是声明，不给当前 Turn 授权。依赖获取、构建、候选 MCP 均服从已冻结网络与沙箱。

## 候选到交付

1. 仅在用户已授权的范围内实际获取获准来源并构建，记录固定 commit 或内容摘要；未获授权或只讨论方案时，只说明所需步骤，不执行。按需读取 Python/Rust Skill，开发依赖不自动变成正式依赖。
2. `plugin_prepare({"directory":"candidate"})` 准备候选摘要与正式 Python 依赖的临时视图；不安装、不挂载。
3. `plugin_test({"directory":"candidate"})` 真实启动 MCP、列工具。需要业务验证时传实际 `server`、`tool`、`arguments`；仅发现工具或 `business_tested:false` 不能声称业务通过。纯 Skill 没有 MCP，不凭空添加业务调用。内容改变后重做准备与测试。
4. `plugin_review({"directory":"candidate"})` 核对文件、指纹、入口、依赖与配置告警；交付来源、改动、测试摘要及未测边界。
5. 用户要求安装时调用 `plugin_install({"directory":"candidate","id":"example"})`，由原生表单确认；模型不能传确认值，取消不安装。安装不等于挂载，启用仍走 `plugin_mount` 的原生确认或用户 HTTP/页面开关。当前 Turn 冻结；变化生效后结束当前 Turn，由 Host 自动续接，不重做已完成副作用。

不能安装替换正在为自己提供指令的 Plugin，不能修改/删除定义自己的 Graph；交操作员维护。安装目前不强制绑定此前成功业务测试，不能把确认当作测试门禁。中断先核查候选和已保存结果，不盲目重放安装。
