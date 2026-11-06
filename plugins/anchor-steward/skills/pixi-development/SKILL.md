---
name: pixi-development
description: 使用发行包已授权的固定 pixi 管理项目依赖、锁文件与工作区内环境，并实际测试。
---

# pixi 项目环境

pixi 是宿主登记、摘要校验后授权的固定命令，不是 Plugin 安装脚本，也不是 `runtime_prepare` 的 kind（该工具只接受 `python` / `rust`）。先用当前 `anchor_run` 运行 `pixi --version` / `pixi --help`，再按项目需要查子命令帮助；不下载替代二进制、不自行扩大命令白名单。未披露时交用户/宿主配置，不因挂载 Anchor Dev 就假定可用。

- 优先沿用已有 `pixi.toml` 或带 pixi 配置的 `pyproject.toml`，没有时才按实际帮助初始化。依赖、channel、platform 与 tasks 写入项目 TOML，`pixi.lock` 与源码一起纳入交付；不要手改求解结果或无故刷新锁。
- `.pixi/` 是项目环境，缓存是可重建现场，不是依赖事实。项目放 `/workspace/<项目>`；缓存也留本工作区（例如在 shell 中设置 `PIXI_CACHE_DIR=/workspace/project/.cache/pixi`），不得改用未授权的宿主路径。复用当前 HOME/已有缓存时先检查实际环境，不宣称 Host 自动共享 pixi 包缓存。
- 不把 pixi 环境与 Pilot `development_prepare` 的 venv 混为一谈；选择项目约定的一套环境运行测试，避免解释器/PATH 串用。AgentNode 只能用自己实际登记的 pixi 和沙箱权限，没有 Pilot 初始化工具。
- 取包须已有任务联网授权，缓存存在不保证离线依赖完整；缺包如实报告。不要为装系统依赖使用 sudo/apt/docker 或启动后台服务。

在已有项目且核对本地帮助支持后，按项目约定执行锁定安装和任务。以下是 Pilot 示例：长安装按实际 schema 显式设置 `timeout_seconds`，最高 600 秒；未设置会沿用命令默认值。AgentNode 的预算由部署决定（默认 30 秒），不得照抄 Pilot 的 600 秒：

```json
{"command":["sh","-c","cd project && export PIXI_CACHE_DIR=/workspace/project/.cache/pixi && pixi install --locked && pixi run --locked test"],"timeout_seconds":600}
```

`test` 必须是项目实际定义的 task/命令，不存在就选实际测试入口；先核查 `pixi run --help`，不编造参数。记录 pixi 版本、使用的锁文件和真实退出结果；`--version`、安装成功或列出 task 均不证明业务测试通过。缓存/环境损坏先核查现场，未经授权不删锁或清空环境。交付选 TOML、lock、源码与必要产物，不交付 `.pixi/`、缓存或项目 `.git`。
