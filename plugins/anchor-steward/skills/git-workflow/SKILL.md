---
name: git-workflow
description: 维护当前工作区的 Git 现场，按需读取独立 Git Plugin 的真实工具与身份指导。
---

# Git 工作流

Anchor Dev 只提供指导，不自带 Git MCP、身份或联网权限，也不暗中挂载 `git`。先查 `anchor_plugins({"plugin":"git"})`；已挂载时通过 `anchor_plugin_read` 读其 overview、Skill 目录，再选 `skills/git/SKILL.md`。工具用 `anchor_tools` 搜索并以 `anchor_tool_detail` 取得当前完整 schema/实际 server，按披露经 `anchor_tools_call` 调用，不在此复制全套参数或凭据规则。

Pilot 若找不到执行能力，可用 `plugin_list` / `plugin_read` 管理查询发现已安装 Git，向用户建议启用或按既有 `plugin_mount({"plugin":"git","on":true})` 原生确认流程办理；新 Turn 才生效。AgentNode 不拥有这些 Pilot 管理工具，缺能力交用户配置。Git 的网络访问仍须单独已有任务联网授权。

1. 先查当前分支、`status`、差异与 remote，辨明用户已有改动；不要重建另一份仓库或覆盖现场。
2. 克隆、编辑、构建和测试在同一 `/workspace` 项目目录进行；跨轮继续原现场。宿主已授权的本地 Git 可经 `anchor_run` 使用；需配置身份的远程操作使用 Git Plugin 实际披露的匹配 server，不把 Plugin 凭据复制到 shell、URL 或文件。
3. 只修改任务范围，先运行实际项目测试再汇总差异。未获提交/推送授权时不执行；获准后仅暂存本次实际改动的路径，不 `add .` 混入其他工作。提交成功不等于测试通过。
4. 不擅自 reset/clean、覆盖已有改动、强推或删除分支。并发工具不得同时修改同一仓库。提交或推送中断时，先核查本地/远端事实，不盲目重试。

凭据与已授权宿主 SSH 路径只按 Git Plugin 的当前指导和 Host 配置处理，不读取或回显内容；挂载指导、Git 身份和网络授权是三件事。
