# 全 Rust 平台开发与验收计划

目标由 [Rust 平台交付决定](rust-platform-target.md) 固定。当前生产默认仍为 Python；已有 Rust Runtime 和低成本回归是起点，不等于平台替代已完成。

2026-10-06 后续用户决定将 Agent 层目标收敛为 Goose-only，停止框架比较并移除预算控制门槛。当前先执行 [Goose 迁移 G0–G2](goose-runtime-migration.md#最短实施顺序)，随后复用本计划的平台能力完成 G3；旧 F2b2 io-harness 提问组件保留已有证据，不再为旧框架扩展新产品路径。

## 开发顺序

| 阶段 | 工作 | 出口 |
| --- | --- | --- |
| F0 基线与契约 | 保存既有未提交工作；梳理入口、事实所有者、工具 schema 与权限；同一基线创建独立 worktree | 写范围和调用契约清楚，不改生产数据或资源 |
| F1 可复用原生首片 | Rust Session 元数据/生命周期事务存储与 API；Rust WeCom 应用 API MCP；Rust Docmost 上传 MCP，输出独立原生 Plugin 包 | 组件与真实 Host/Plugin 组合可验证；不冒称完整 Pilot、网关或生产发布 |
| F2 平台会话闭环 | Turn 幂等、同 Session 串行/替换取消、Pilot、Graph 会话、提问、SSE 及重连；共用 Goose ACP | 现有 WebUI 聊天/续聊、历史、对象跳转、停止和重启闭环 |
| F3 Library/授权与官方工具 | Rust 安装/资源冻结/环境授权、MCP OAuth 存储与刷新、学术检索/提取 CLI；提供 Rust 原生构建/回归入口 | 标准工具闭包无 Python；授权与只读/路径边界有反例 |
| F4 渠道与 Runtime 对齐 | Rust WeCom 长连接、附件、ACK/去重/未知结果；Webhook/relations；嵌套/并行 call、摘要流与调度故障边界 | 已支持的 master 产品行为逐项对齐，缺口不以 501 或假状态掩盖；预算控制按用户决定暂不纳入 |
| F5 候选版本 | 固定 release 和证据；一次必要全量 Rust/Web/兼容检查；代表业务有界真实验收 | RSI/深度研究失败原因收敛，原 Graph/工具契约与用户结果通过 |
| F6 生产切换 | 旧历史导入/只读策略、未完成 Run 收束、单写 backend、Scheduler/网关接管和回滚 | 在无 Python 隔离部署验收后，经独立授权执行生产切换 |

F2/F3/F4 在共享契约稳定后并行。累计请求/token/金额预算暂不实现，不建设计量代理或日志；权限、用户取消和必要稳定性约束保留。

### F2 垂直切片顺序

- F2a：普通 Pilot 的原生聊天、只读 Graph/Plugin/Run/Artifact 工具、Turn 幂等/串行、停止、启动中断和 SSE 重连。系统 Pilot 接入与 AgentNode 相同的 Goose ACP/MCP 层，不变成隐藏 Graph。
- F2b：接通 Graph 创建/更新/删除确认、Run 启动与控制及 `session_ask`/回答。先固定 Turn → 原生会话/Run 的事实关联、未知结果核查和跨存储 admission/删除契约，再并行拆工作包；不能以另一个 Agent loop 或审批引擎补缺口。
  F2b1 交付创建/更新、Run 启动/暂停/恢复/停止及显式关联；F2b2 接原生提问和删除确认的暂停/回答/拒绝、目标变化校验与重启窗口。分片不取消任何原定能力，也不把 F2b1 当作 F2b 完成。
  普通 Goose Pilot 的 F2b2 已按 G2c/A117 接通：同 Turn 原生 form 回答、精确删除确认、目标变化拒绝、停止/重启废弃旧问题、SSE/刷新和定向浏览器/真实 Provider 验收。后续 G2d 的 MCP 图片结果可经真实 Goose 与确定性 Provider 验证原生 Image、模型 wire、大小/解码边界和同 Session 恢复；这不是完整媒体或真实视觉模型验收。旧 io-harness 提问组件不扩展；复杂 form、Graph/channel Session、完整 Responses/媒体输入/渠道/图片 UI/压缩组合仍不在此完成范围。
- F2c：Graph/通道 Session、替换取消、Responses 与完整用户流程组合，核对源权限和 retained Run/会话清理；与 F4 渠道端口复用。
- 每片都有确定性回归；真实 Provider 只验证该片新增路径。F2a 只读成功不能关闭 F2b/F2c，候选版统一 binary 的完整验收保留在 F5。

## 首批独立工作包

| 包 | 写范围 | 契约与验收 |
| --- | --- | --- |
| Session worker | `rust/anchor-platform-session/` | 独立事务事实存储；owner 隔离、创建/列表/读取/改名/状态/事件、重启和并发；不碰 Harness store 和 Python数据 |
| WeCom worker | `rust/anchor-wecom-tools/` | 保持三个应用 API MCP 工具 schema；本地 HTTP fixture、token 缓存、无自动重发 mutating 请求、stdio MCP、独立原生包；长连接网关是后续包 |
| Docmost worker | `rust/anchor-docmost-tools/` | 保持上传工具 schema；固定只读输入根、UUID/MIME/大小/路径边界、multipart 和错误事实、stdio MCP、独立原生包；不连接生产服务 |
| 主轨 | 共享 Cargo/Host 装配、Session HTTP adapter、协议组合回归、架构和台账 | 保存基线、集成 worker 补丁、审查权限/持久化、固定 binary 并独占最终验收 |

每个 worker 在自己的 detached worktree 编辑，只保留一名编辑者；不提交或创建新分支。基线包含当前 HEAD 与原有工作区补丁，worker index 固定基线，交付 diff 只含其新增工作。每个 target 独立且位于 `/tmp`，不得覆盖另一工作流的 binary。主轨先审查再集成，不等待重复的形式化报告。

## 验收节奏

### 生产替代前的并行工作边界

- Graph 删除应用层：单一 worker 维护 `application/graphs`，提供目标快照和条件删除；复用现有 catalog/准入/清理，不把普通 HTTP 删除变成新审批流程。Pilot 确认仍由后续框架接线负责。
- 原生提问接入：单一 worker 维护 Goose 公开提问接口与 Host 的 Turn/owner/回答契约，保持普通 Pilot API 和既有工具语义。旧 io-harness 组件只保留历史验证，不继续扩展；新接线未闭环前不对普通 Pilot 开放。
- 官方学术 CLI：独立 Rust crate，检索、批量、阅读和引用链分别验收，保留原 Python/Plugin 与活跃资源。已实现的 CLI 尚须接到官方 Plugin/工具部署并验公共 API，不将本地 parser/transport 测试当作官方工具闭包完成。
- 主轨：共享 Cargo 装配、Rust 原生 fixture 回归入口、补丁 review 和统一验收。fixture 复用原测试和证据，不另建 Runner 或运行日志；真实模型与业务验收继续独立。

以上工作包使用同一工作区快照的 detached worktree，写范围不重叠。组件接口稳定后再接完整平台会话与安装/授权；没有完成接线的能力不计入生产支持面。

先组件定向测试，再 Host → 共享 GraphRunner → Goose ACP → 授权 MCP/ToolPort → Sandbox 的小型组合 Graph；真实模型只做固定短任务，本地外部服务 fixture，不发业务消息或发布。组件不涉及模型时只声明组件/API 证据，不能扩大为 Agent 产品闭环。A112 的九项 Goose 验证是接入起点，不代替完整迁移回归。

跨模块切片完成后执行一次必要的广域验证。历史 Rust Python-tool 环境失败按 A120 的实际修复及后续台账判断，不笼统宣称一直失败或已经全部关闭；legacy Python 依赖与官方原生闭包分别验收，不删除测试来伪造绿色。新功能同步进入回归矩阵，未覆盖边界仍明确记录。

完成状态及实际命令只写入 [开发台账](pilot-development-plan.md)，本计划不把实现中或未运行的事项写成通过。
