# A224 持续能力摘要与会话挂载更新验收

2026-10-13：主 Agent 驱动两个实现子 Agent 完成开发、review 和隔离验收。Plugin 摘要从本次冻结授权目录及实际工具装配派生，通过 MCP `initialize.instructions` 进入 Goose v1.53.0 的扩展系统提示；详细内容继续按需读取。运行中网页挂载保存下一次执行事实，当前 Turn 正常收束后合并为一次接续，停止和新消息可以终止旧来源的接续。没有新增目录存储、Agent loop、记忆或恢复引擎。

## 已执行检查

实际命令、日志、退出码与执行阶段保存在 [checks.json](checks.json)。计数按 Cargo target 或场景报告，不把重复执行相加为唯一测试数。

| 检查 | 结果 | 范围 |
| --- | --- | --- |
| Host all-targets | 30 targets，893 passed / 0 failed / 90 ignored，exit 0 | 早于最后的回执文案修正；ignored 不等于已执行 |
| 披露定向 | 24 passed；MCP initialize 1 passed | 预算、空目录、Unicode/多行描述、冻结目录与可发现状态 |
| 固定 Goose Session load | 2 passed | 直接/渐进 schema，关闭 tom，同 Session 挂载/停用及 Host 重启加载 |
| 固定 Goose compaction | entry/reactive 各 1 passed | 各 8/5 个普通请求保留系统摘要；框架专用摘要请求不计入这一承诺；重读与副作用不重放 |
| Web unit / browser / build | 120 / 12 passed；独立目录 build exit 0 | browser 包含实际 Host+Goose 的冻结挂载、合并接续、停止，也包含 mock 竞态用例；不是 12 次真实 Provider 运行 |
| 最终回执定向 | Bridge 12 passed；native guessed receipt 1 passed | 猜测/过期回执拒绝，最新只读发现回执可完成，业务调用一次；其他原生用例只同步错误文案断言，未复跑 |
| 最终 build / Clippy / fmt | 全部 exit 0 | 回执文案修正后；没有重复宣称全量回归 |

确定性模型只替换传输，实际经过 Host、GraphRunner/NodeExecutionPort、固定 Goose ACP 和授权 MCP；原生框架行为不是 mock。摘要和压缩的原始证据分别为 `/tmp/anchor-goose-acp-spike/evidence/integration/657957/goose-plugin-summary-load-{0,1}/evidence.json`、`662145/goose-compaction-plugin-workspace/evidence.json`、`663028/goose-compaction-reactive-context-error/evidence.json`。可携带概要见 [deterministic.json](deterministic.json)。

浏览器实际路径证据为 `/tmp/anchor-plugin-mount-browser-lObztU/evidence.json`；构建身份为 `.local/plugin-capability-context-20261013/browser-host/identity.json`。测试 Host 从 `59d501f2…` 构建复制后仅 strip 为 `029dae8b…`；没有把这一产物冒充最后的 Node Host。

## 真实 Provider 分路径验收

真实模型 `deepseek-flash`；透明记录代理原样转发请求、响应及 usage，没有用固定回答或修改 token 用量替代真实模型。用户任务不点名 Plugin 或发现工具。可复用脚本为 [real_provider.py](real_provider.py)，概要为 [final-real-provider.json](final-real-provider.json)。

**Pilot** 原始根：`.local/plugin-capability-context-20261013/real-d0c019b9c7262926/`，Host SHA256 `59d501f2bd020405d3a5c8fd5143a874e0dd07f002d94f0012aeb6b0cd787226`。Library 1,000 Plugin，Pilot 挂 10 个，目标 stdio MCP 1,001 工具。模型自行发现、读取并调用 `zz-lens`，实际 MCP 日志和产物核对通过。活动 Turn 暂停一次模型传输后，真实调用挂载/停用 API：暂停的当前请求仍是原摘要，多次点击合并为一个同原生 Session 的 continuation；能力核查的 continuation 没有重复出报告，后续明确的新报告任务调用 `zz-prism`。新正常请求包含 prism 摘要，旧 lens 不在当前系统摘要中。

在该隔离实例把模型上下文配置为 32,000，压力消息 60,202 字节，保留真实 usage，确实触发两个 Goose 原生 summary 请求。原生消息从 64 到 73，其中 68 条隐藏且有可见压缩标记；压缩后的普通模型请求仍有当前 Plugin 摘要，没有重做报告。模型从压缩历史回答，因此这一真实路径不证明压缩后发生新的工具重读；重读由确定性 compaction 场景独立证明。

该 aggregate 脚本随后在 Node 的终态采样/产物断言阶段退出 1。通过的 Pilot 单独摘录为 [pilot-real-provider.json](pilot-real-provider.json)，保留 aggregate 退出码和失败阶段，没有把完整脚本标为通过。

**最终 AgentNode** 命令：`python3 real_provider.py --node-only`，exit 0；日志 `/tmp/anchor-plugin-capability-context-real-node-final.log`。原始根：`.local/plugin-capability-context-20261013/real-88a393df05d4dd6c/`，Host SHA256 `38fde27e85119cbbbe5a0cc1eb326bd13bb750768bf983abc084c7a851e0d8b9`。本次挂 999 个 Plugin，摘要显示 87、明确省略 912，16 KiB 预算仅约束 Plugin 部分，完整 Goose system 为 16,985 字节。目标位于省略部分，模型仍按任务关键词发现它，选读说明、正确 Skill、随机凭据资源与完整 schema；13 次真实模型请求均返回 HTTP 200。独立 MCP 日志只有一次报告业务调用，随机 marker/token 和 17+25 的结果、`proof.json`、原生完成工具及最新真实回执一致。没有读取无关 Skill 正文。概要为 [node-real-provider.json](node-real-provider.json)。

Pilot 与最终 Node 绑定不同 Host 构建。最后生产变化是回执指导和错误文案：回执属于最近授权非完成工具，包含发现与只读工具；实际校验条件没有改变。该修正后只重复受影响的 Bridge/native completion 及 Node 真实路径，没有把旧 Pilot 或浏览器证据改署为最终 Host。

两条真实路径 Session DELETE 200、Host exit 0、监听关闭，无 panic 或存活所属子进程；`.env` 和共享 Python 解释器字节未变。固定 Goose SHA256 `71e76c412597b2ecd96ed20d0706e7666f31c018216e7cb5d65c5ca5c44824a7`。

## 失败、修正及范围

| 原始根尾缀 | 观察与处理 |
| --- | --- |
| `2edbe95e556c51e1` | 旧 continuation 指导重复了已完成报告；改为沿最新用户请求只处理未完成且已授权工作，能力变化不授权重做已完成效果 |
| `30fddcabad27c92a` | 模型主动读了无关 Skill；只读工具只返回实际请求的正文，没有自动加载所有 Skill。保留模型选择波动，不承诺每次都只读相关内容 |
| `c62d388a9f0c4b23` | Pilot 已通过，但 fixture 压力消息超出公开 API 64 KiB 上限；缩为 60,202 字节，未提高 API 限额 |
| `d0c019b9c7262926` | Pilot 与真实压缩通过；Node 先观测 active=false，未等 durable completed，且自然 Markdown 报告与严格 JSON 断言不一致；fixture 改为等持久终态并明确保存原始服务 JSON |
| `e75e2343a857237c` | Node 完成正确产物，但拿错回执后受错误提示误导，又调用业务工具取回执；修正真实错误和指导，明确可从只读核查取得回执，保留校验条件 |
| `88a393df05d4dd6c` | 修正后的 Node-only 退出 0，实际业务调用一次、产物与最新回执通过 |

这是预置隔离 Library 的能力披露与执行验收，不是 Plugin 安装、外部业务、1,000 个 MCP server 并发连接或部署验收。MCP 懒连接、Goose Code Mode 仍未实施。能力摘要使 Agent 能主动选择，但不能保证任意模型每轮都选择正确或从不多读。

Web 子 Agent 最初误把 build 写入正在使用的 `apps/web/dist`；主 Agent 已从前次部署产物恢复全部原文件并核对 manifest，未重启 Host。后续所有 build 使用隔离 outDir。修正记录 `.local/plugin-capability-context-20261013/web-build-correction.json`；8077 当前静态入口仍为原摘要 `d9fa3264…`。5173 是 Vite dev server，HTML 含开发注入，不能拿其响应与 dist HTML 直接比字节。收尾身份核对见 [deployment-identity.json](deployment-identity.json)：原 Host PID 184037/摘要保持一致。

代码与验收证据随本次提交固化，未部署；没有改模型配置或共享依赖。
