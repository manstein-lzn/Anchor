# Rust-native WebUI API 出口

> 历史设计与首片验收，当前契约见同名现行文档。

本文冻结“前端体验基本不变”的产品级 API 目标，并记录首个已验证的 Rust Host → React 垂直切片。Rust `serve` 可直接托管同一 React bundle；Graph、Run、Artifact 与真实 Run 时间线路由已接通。浏览器验收已从 Rust Host 页面加载工作台，在非 loopback + Bearer key 配置下运行 Graph 并通过界面打开产物。Session/Pilot、Scheduler、Plugin 管理、relations/channel 和完整平台迁移仍未完成。路线目标仍是复用同一 React bundle，不因 Runner 语言变化重做画布与 Run Inspector。

## 第一批：Graph 画布与 Run 体验

| 当前前端请求 | Rust API 需提供的产品投影 |
| --- | --- |
| `GET /graphs` | `{graphs:[{graph,running,active_runs}]}` |
| `GET /graphs/{graph}` | `{graph,definition}`；definition 保持 JSON Graph 用户资产格式 |
| `POST /graphs`、`PUT /graphs/{graph}`、`DELETE /graphs/{graph}` | 已接入无 Plugin Graph catalog 首片：创建 Graph、admission 后保存；删除当前有 Graph 定义或未结束 Run snapshot 调用的目标会返回 409，目标自身有未结束 Run 也返回 409。终态历史调用不阻止删除；成功后仅清目标 Graph 自身及其 Run 数据。配置 bundle 是普通 Graph；Plugin Graph 写入继续 fail closed |
| `POST /trigger` | 输入 `{graph,input?}`，接受后 `{run,graph}`；busy 返回 409。Rust 读取服务端配置 Graph/bundle，不接受调用方绝对路径 |
| `GET /runs` | `{runs:[OurRun]}`，至少包含 `run,graph,status,running,started,updated,executed,objective,trigger` |
| `GET /runs/{run}` | `{graph,run,state,traces,nodes,calls?,plugins?}`；state 保持前端依赖的产品语义，trace 提供可读消息，不要求复制 Harness 原事件 |
| `POST /runs/{run}/pause|stop|resume` | A62已接入：暂停后真正重启同一Runner续跑、原snapshot/input与Plugin身份校验、停止/未知副作用保护；返回 `{run,asked}` 或清晰错误，请求确认与最终Run状态分开 |
| `GET /timeline?days=&before=` | 已接入 Rust Run 历史投影；时间范围与状态来自持久 Run/来源 metadata。计划项为空且 `capabilities.scheduling=false`，React 会禁用计划管理；Rust Scheduler 和 `/schedules` 尚未实现 |
| `GET /runs/{run}/files/{node}`、`GET /runs/{run}/files/{node}/{path}` | 文件目录、文本预览、下载；现按 Rust-owned Run 的节点持久结果读取不可变文件快照，未知节点或尚无commit返回404；下载流式，preview限定1MiB，不能接受任意 host path |
| `GET /graph-relations`、`GET /channel-sessions` | 当前画布的组合/来源投影；若首期能力不可用，前端需做显式能力降级，不返回伪造空事实 |

接口可以在 Rust 中用 Axum 等成熟框架实现。HTTP DTO 是产品 projection；不要把 `GraphRunRecord` 或 Agent 执行器内部记录直接序列化成对外协议。Graph JSON 继续作为可编辑资产，Rust loader 负责 schema/admission 和 bundle manifest 校验。

## 后续产品面

Pilot 页面另依赖 `/sessions`、`/sessions/{id}/messages`、`/turns`、turn SSE、会话 rename/delete/stop/confirm/reject；Plugin 页面依赖 `/plugins`、`/plugins/{id}`、安装和授权 API；这些属于同一个 Rust 产品服务的后续迁移切片。前端 bundle 与其 API base URL 应保持可复用，不要求 Pilot/Plugin 后端和 Graph Runner 一次性切换。

## 所有权与验收

Rust API 调用 Rust 应用服务和唯一 GraphRunner。Rust-owned Run、checkpoint、events、artifact 都由 Rust 一方写入；前端只读投影，不拥有事实。Python legacy 可以并行服务旧 Run，但同一 Run ID 不得双写。验收需要真实 HTTP 浏览器运行、Graph JSON 创建/编辑/启动、Run timeline/detail、pause/stop/resume、文件查看，以及 UI 流程在前端未改动时通过。业务接口缺失不能用空数组或假状态掩盖。

A62补充：Graph来源和创建时间来自接纳时持久metadata，Graph同摘要不等于同身份；当前 `updated` 未知返回空、节点 `exit_status` 未知返回null，已提交节点不因下游失败改判失败。无metadata旧Run列表以unknown来源披露，详情/控制不猜身份。

当前浏览器验收覆盖 Rust Host 自己托管的 bundle、非 loopback API key 输入、无 Plugin 单节点 Graph 运行、节点文件面板和运行时间线。没有结束时间的 Run 显示时长未知；宿主重启后状态仍为 `running`、但当前无活动任务时显示等待接续。API 层已有 Run detail 与 Harness trace fixture 投影测试，但尚未用真实模型 Run 验收 trace 面板。完整产品验收仍需要 Plugin、Session/Pilot、Scheduler、relations/channel、pause/stop/resume 及跨平台宿主路径；缺失项按能力明确降级，不以此首片宣称完整 Rust 平台替代。
