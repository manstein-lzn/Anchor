# Anchor Web 工作台

## 当前入口

- 前端：<http://127.0.0.1:5173>
- 后端：<http://127.0.0.1:8090/docs>
- 项目：`/home/mansteinl/Anchor/apps/web`

首次连接需要本地 API Token。在本机终端查看：

```bash
cat /home/mansteinl/Anchor/.local/api-token
```

将 Token 输入连接页。它只用于本地 Anchor API 的 Bearer 认证，不是模型 API
Key。前端不会读取 Codex 配置或模型密钥，也不会把 Token 加入 URL、源码或构建
产物。Token 存在当前浏览器标签页的 `sessionStorage` 中；断开连接会删除它。
浏览器的会话恢复功能可能恢复 sessionStorage，因此共享电脑上应主动断开连接。

## 使用流程

1. 新建工作流，在右侧设置名称和 Graph ID。已保存的 Graph ID 不可修改。
2. 添加节点：Agent、工具、路由、并行、汇合、验证、审批、等待事件、人工任务、
   产物、循环。Agent、工具和验证节点需要对应引用；循环节点需要退出条件。
3. 拖动节点两侧的连接点建立连线，也可在属性面板选择起点、终点并添加连线。
4. 选中节点或连线配置属性。完整 JSON 编辑器可编辑 schema 引用、metadata、
   连线 input_mapping 等已有 Graph IR 字段，不需要把这些字段重新建模。
5. 保存草稿。草稿允许语义不完整；校验与发布会检查必填字段、入口、可达性、
   终点以及显式 Loop 边界。
6. 发布将固定一个不可变 Graph Version。版本页可以查看旧版本，返回草稿时
   保留尚未保存的本地编辑。相同草稿修订重复发布会返回原有版本。

手机窄屏使用“工作流 / 画布 / 属性”切换面板。画布支持缩放、适应视图，
编辑支持撤销、重做、复制和删除节点。删除节点会同时删除关联连线。

顶栏“运行”进入 Run Console（运行状态只读展示，审批/事件等待可直接批准、拒绝或恢复），展示数据库中的真实 Task/Run 状态、固定
Graph 版本、节点状态、递增事件和工具操作账本。当前以 5 秒间隔增量轮询；
单次查询页大小只限制传输，不限制 Agent 生命周期。事件很多时会逐页追赶，
不会把游标跳到最新位置而漏掉中间审计记录。

可通过工具栏导入 `examples/graphs/research-review.json`，得到一个研究员、
交叉审查、证据验证、报告交付的编排示例。导入不会自动保存或发布；其中的
`agents.researcher` 等是引用占位符，不代表已经注册或可以执行的 Agent。

## 数据与恢复边界

- `definition` 是后端 Graph IR；节点位置写入独立 `layout.positions`，不参与
  发布内容哈希。导入导出保留其余现有配置字段，不做隐式语义转换。
- 草稿保存使用 `expected_revision`。发生 409 冲突时保留本地内容，禁止发布；
  可以先导出，再重新载入服务器草稿进行比较，不自动覆盖远端。
- 保存请求确认丢失时不会自动重试写入。再次保存可能得到修订冲突，这是
  对“服务器可能已经保存”的保护，不表示需要强制覆盖。
- 离开页面或切换工作流会提示未保存修改。未保存内容仅在页面内存中，
  **浏览器崩溃后不能保证恢复**；需要保留的内容应保存或导出。
- HTTP 请求有连接确认超时，不对 Agent 生命周期、Token、步骤或成本设预算。

## 当前执行边界

工作台已经连接真实 canonical 数据库、receiver 和 Agent worker。已发布 Graph 中
的 `agent` 节点可以经过 durable admission、claim/lease、模型调用、artifact/context
checkpoint 和下游传播，最终推进 Run/Task 终态。Run Console 展示真实节点、事件、
operation、memory、artifact 和 lease 状态；可恢复的 stale Agent lease 提供人工恢复
和明确标记失败操作，不生成虚假进度。

顶部“接收器已连接”只表示接收进程近期有心跳；“执行 Worker 未连接”也只表示近期
没有 worker heartbeat，不能单独证明某个 Run 成功、失败或停滞。Run 的业务状态必须
以节点、事件、lease 和 operation 记录共同判断。Agent lease 不会因超时被自动偷取；
Tool 的未知外部结果也不会因断线被盲目重试。

当前模型 worker 只领取 `agent` 节点；独立 control worker 只领取 Router、
Parallel/Join 和 Artifact。control 节点把 resolved selected-input 写成真实 JSON
artifact/context checkpoint，并使用持久化 edge decision 推进条件分支，不伪造模型
结果。未选择路径显示为 `skipped`，join 会等待所有入边决议和全部 selected 前驱。

独立 verifier worker 只领取 Verifier 节点：确定性 JMESPath 或严格 JSON 模型裁决，
绑定已校验前驱 artifact 与 context hash，持久化 VerificationRecord，只有 persisted
passed 才能完成节点并打开下游；rejected/error 直接失败关闭。Run Console“验证证据”
区域展示裁决、证据引用与哈希。

监督进程持续写入进展观察（`GET /api/runs/{run_id}/progress`），并在完整循环重复且
没有已验证进展时登记可追溯诊断（`GET /api/runs/{run_id}/diagnostics`）。Run Console
的“诊断”与“进展证据”区域展示这些持久化记录；观察不足不会被显示成失败，重复循环
也不会自动终止 Run。业务循环次数、节点 attempt（含故障重试）和请求重试是三个独立
计数，界面不混用。

Approval、Wait/HumanTask、Loop 和 Tool 仍需要各自的
专用 executor 与持久状态语义。边条件使用 JMESPath，并在发布时检查语法；运行时只接受布尔结果。Run
Console 的“路由决议”区域展示 selected 状态、求值器版本、上下文哈希和证据引用。

后端已有 manual、cron、interval、internal-event 和 webhook trigger/admission 边界，
但 Web 端尚未提供完整的 trigger 管理和审批执行界面。Run-scoped durable memory 已有
provenance、content hash 和 tombstone 删除能力，完整的长期 context policy、向量检索、
retention/GC、MCP/skills/A2A 和生产级多用户权限仍待后续里程碑。

工具 operation ledger 已持久化，并明确区分成功、失败与 `outcome_unknown`；实际
ToolGateway、schema/permission 校验、审批边界和 MCP adapter 尚未接通。因此当前不能
把工具节点用于真实外部副作用。

## 本地开发

Node 22.22.3 已验证。使用项目提供的 lockfile 安装：

```bash
cd /home/mansteinl/Anchor/apps/web
npm ci
npm run dev
```

默认仅绑定 `127.0.0.1:5173`，端口占用时启动失败，不接管现有服务；可加
`-- --port 5174` 改用另一端口。开发代理将 `/api` 和 `/health` 转发到 8090，
不会注入认证凭据，也无需开放后端 CORS。
代理目标可用服务器环境变量 `ANCHOR_WEB_API_URL` 调整，不要将密钥设置为
`VITE_*` 环境变量。重新解析依赖时 npm 10 曾触发可选 peer 依赖解析缺陷，
可使用 `npx --yes npm@11 install`；不要用强制降级绕过安全补丁。

当前前后端均以临时用户服务运行，无需 sudo：

```bash
systemctl --user status anchor-api-dev anchor-web-dev anchor-receiver-dev \
  anchor-supervisor anchor-worker anchor-control-worker anchor-scheduler
journalctl --user -u anchor-web-dev
journalctl --user -u anchor-receiver-dev
systemctl --user stop anchor-web-dev anchor-receiver-dev
```

交接时 API、Web、receiver 和 supervisor 正在运行；Agent worker、control worker
和 scheduler 有意保持停止，避免消费开发库中的遗留 Run。不要仅为消除“未连接”
状态而启动它们。开发服务不是开机自启动服务。前端停止后，可直接使用
`npm run dev`，或重新创建临时用户服务：

```bash
cd /home/mansteinl/Anchor/apps/web
systemd-run --user --unit=anchor-web-dev \
  --property=WorkingDirectory=/home/mansteinl/Anchor/apps/web \
  "$(command -v node)" /home/mansteinl/Anchor/apps/web/node_modules/vite/bin/vite.js \
  --host 127.0.0.1 --port 5173
```

后端启动、迁移和 Token 文件说明见 `API.md`。`npm run build` 生成 `dist/`，
但生产部署还需要同源反向代理、TLS、权限体系和持久执行基础设施；不要将
Vite 开发服务直接暴露到公网。

## 验证

```bash
cd /home/mansteinl/Anchor/apps/web
npm test
npm run build
npx playwright install chromium
npm run test:e2e
npm audit
```

浏览器测试自动在 8091 和 5181 启动专用 API 与前端，使用迁移后的临时 SQLite
数据库及独立测试 Token，不读取本地开发 Token，不写入 `.local/api.sqlite`。
测试完成后关闭服务并清理临时数据库。截图位于 `apps/web/test-results/`，
失败时额外保留 trace。

当前覆盖：Graph 配置往返、鉴权、完整发布流程、版本只读、拖拽持久化、
撤销重做、导入导出、修订冲突、丢失保存确认、窄屏编排和布局边界。
浏览器验证范围为 Chromium；Firefox、WebKit 和实际触屏设备尚未验证。
