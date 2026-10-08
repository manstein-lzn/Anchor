# Rust Host 与 WebUI API 契约

React/TypeScript WebUI 调用 Rust Host 的资源 API。Host 可直接托管编译 Web bundle；应用服务和唯一 GraphRunner 持有执行事实，前端负责投影与控制请求。当前证据与待验收项见 [开发台账](pilot-development-plan.md)。

## Graph、Run 与 Artifact

| API | 契约 |
| --- | --- |
| `GET /graphs`、`GET /graphs/{graph}` | Graph 身份、定义与活动 Run，保留作者 JSON/layout |
| `POST /graphs` | `{name,definition?}` 创建普通 Graph |
| `PUT /graphs/{graph}` | `{definition:{...}}` 编译并保存，通过 Library 绑定资源 |
| `POST /graph-validation` | `{definition:{...}}` 校验而不保存/执行；无效输入与语义诊断区分 |
| `DELETE /graphs/{graph}` | 活动/可继续 Run、定义或未结算调用引用会阻止删除；只清本 Graph 数据 |
| `POST /trigger` | `{graph,input?}` 接纳为 `{run,graph}`，busy 冲突；不接受调用方绝对路径 |
| `GET /runs`、`GET /runs/{run}` | 状态、路径/轮次、trace、节点、调用、Plugin 与触发来源 |
| `POST /runs/{run}/pause|stop|resume` | 控制请求与最终状态分别展示，保留 snapshot/input/身份 |
| `GET /runs/{run}/files/{node}`、`.../{path}` | 不可变 Artifact 文件、预览/下载；拒绝 host-path 越权 |
| `GET /graph-relations`、`GET /channel-sessions` | 真实定义、调用与可信来源投影 |
| `GET /timeline`、`/schedules` | Run 与计划事实；时间戳未知如实披露，不编造历史 |

HTTP DTO 是产品投影，不直接输出执行器私有记录。未知结束时间、exit status 或来源必须保持未知；下游失败不能改变上游已提交事实。文件复制/迁移可能改变基于 mtime 的历史时间精度。

## Session、Library 与渠道

Session API 包括创建/重命名/删除、消息、Turn、SSE、停止、问题与回答。Turn 按 `request_id` 去重，SSE 支持游标回放和重连；同一请求不得再次执行。问题/回答归 Session/Turn，模型历史归 Goose。

Plugin API 提供 catalog、安装/移除与 owner-bound 授权事务。OAuth、模型与远程 MCP 的真实兼容性需要独立验收；本地工具测试不等于外部授权流程已经通过。

可信渠道入口绑定用户、Graph、回复节点与本地冻结附件。普通 Graph 和 Pilot 管理入口分别接纳，不把助手业务交给管理 Agent。相同 Session 串行，不同用户可并发；旧轮迟到回复抑制和持久去重由 Host 保证。

Webhook 与 Responses 共用接纳和身份校验。Responses 只实现冻结子集，不能宣称兼容完整 API；SSE 代理需关闭缓冲。`/health` 是 liveness，`/ready` 是配置 Graph readiness，不检查模型网络。

## 权限、观察与验收

API key、工具范围、路径/网络、原生 Session scope 与精确删除前置条件由 Host 强制执行。提示词、Plugin 说明、前端隐藏按钮和模型输出都不是授权。

trace 展示 ACP 的原生文本/工具内容，按 `tool_call_id` 配对；媒体有独立可接受类型/解码边界。投影用于查看，恢复仍使用持久 Run/工具事实与 Goose 原生 Session。普通 assistant text 不冒充 canonical 完成摘要。

浏览器验收使用实际 Rust Host、固定 Goose 与 Node 本地 Provider，检查 Graph CRUD、真实执行、暂停/停止/继续、历史、文件、Session 和必要提问。UI fixture 只证明展示；未配置而跳过、实际端到端与真实业务验收须分别记录。运行方法见 [小图回归](runtime-contract-tests.md)。
