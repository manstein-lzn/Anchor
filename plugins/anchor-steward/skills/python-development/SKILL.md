---
name: python-development
description: 使用 Pilot 私有 Python 项目环境，区分试装依赖、正式 Plugin 依赖与 AgentNode 沙箱。
---

# Python 开发

Pilot 先 `platform_status({})`，核查 Python 执行、依赖准备和私有项目能力。共享解释器尚未准备且已授权联网时，`runtime_prepare({"kind":"python"})`（可带 `version`）；显式部署禁止不能被覆盖。配置变化时结束当前 Turn，由 Host 自动续接，新 Turn 才用新冻结能力。

在已有或新项目 `project` 中：

```json
{"project":"project"}
```

传给 `development_prepare` 默认 `ensure`：建立空的私有 venv/pip；已有环境保留包与源码，不代表包健康。随后始终通过同一项目选择运行：

```json
{"project":"project","command":["python","-m","pip","install","-e",".[dev]"]}
{"project":"project","command":["python","-m","pip","check"]}
{"project":"project","command":["python","-m","pytest"]}
```

项目根可用 `project: "."`。pip 也可读 requirements/constraints、extras 和 marker；修改依赖文件不自动同步。`requirements` / `development_requirements` 数组是显式追加安装请求，不是精确同步。安装仍须当前冻结网络/文件授权；不修改共享解释器或正式只读视图。只在用户授权清空私有包时用 `action: "rebuild"`，不同时传非空依赖数组；`discard` 仅释放环境、保留源码。旧项目返回 `legacy_read_only` 时没有 pip，首次显式 rebuild 成功才切换。

中断可能留下部分安装：先核查 `pip list`、`pip check` 与项目测试，不以旧准备回执证明健康，也不盲目重装。Python 私有试装包不会自动进入 Plugin；正式依赖写 `plugin.json` 顶层 `dependencies.python`，再 `plugin_prepare` / `plugin_test`。

AgentNode 没有 Pilot 的上述初始化和私有项目管理工具，`anchor_run` 也不承诺 `project` 参数。它可在稳定工作区运行已授权解释器/项目自管环境（如另一个 Skill 的 pixi），但缺解释器、依赖来源或网络时应报告，不绕过宿主。完成通过 `final_result.files` 显式交付源码/锁等，不选环境目录。
