# Anchor Pilot P2 开发汇报

日期：2026-09-26 ｜ 范围：`docs/pilot-agent-development-command.md` 的「当前目标开发」 ｜ 依据台账：[pilot-development-plan.md](pilot-development-plan.md)

> 本文是一次交付的快照，说明做了什么、怎么验的、哪里还没验。状态若与台账冲突，以台账为准。

## 1. 结论

| # | 指令要求 | 状态 |
| --- | --- | --- |
| 1 | 接入 PydanticAI / Harness 原生持久记录，保留 JSONL、消息快照及相关文件 | 完成 |
| 2 | 重开同一 Session 可发新消息；Agent 加载中断历史、查询现场后继续 | 完成，真实杀进程 + 浏览器验收 |
| 3 | 收敛阻断续聊的旧门禁，取消已授权普通操作的重复审批 | 完成 |
| 4 | 长对话需要压缩时使用 Harness 已有接口 | 接线完成；**真实长会话未验证** |
| 5 | 聊天中的 Graph、Run、Artifact 引用直接跳转已有页面并返回会话 | 完成 |
| 6 | 真实 provider、浏览器、进程中断后的续聊验收 | 完成（三条真实路径） |

自动化回归：后端 300 项、前端 23 项单测 + 9 项 e2e，均通过（见 §4.1）。

## 2. 交付内容

### 2.1 后端

| 文件 | 改动 | 解决的问题 |
| --- | --- | --- |
| [pilot.py](../src/anchor/pilot.py) | `StepPersistence` 后端换成原生 `FileStepStore`（`state/pilot-steps/`），并打开 `capture_frontier=True` | 记录不再是单一 SQLite 文件，而是框架原生 `run.json` + `events.jsonl` + `tool_effects.jsonl` + `snapshots/*.json` + `media/*`；没有 frontier 快照时，工具执行中被杀的进程不留任何可读现场 |
| 同上 | 新增 `attempt_history()` | 只在框架记录比已保存对话「更长」时接手，正常回合仍走 conversation store，不重复历史；只采纳创建时间不早于会话的记录，避免复用 id 继承已删除会话 |
| 同上 | 新增 `_close_unfinished()` | 中断历史末尾的未完成 tool call 会被框架拒绝叠加新 prompt；只把它标成框架自己的 `state='interrupted'`，由框架合成 `outcome='interrupted'` 的 tool-return：模型看到「调用过、结果未知」，不会重放 |
| 同上 | 六个普通工具取消 `requires_approval`，仅 `graph_delete` 保留 | 用户请求即授权，不再逐次确认；破坏性操作仍由框架的 deferred 确认把关 |
| 同上 | 新增 `_compaction()` | 复用框架 `SlidingWindowCompaction`（可选 `SummarizingCompaction`），配置键 `pilot_compaction`，不另建记忆系统 |
| 同上 | 指令更新 | 授权规则、中断语义（先核查现场、不重放、不谎报成功）、对象引用写法 |
| [serve.py](../src/anchor/serve.py) | 删除两处 `unsafe_to_retry` 副作用门禁；允许 `interrupted` 会话接新消息；`/messages` 与 turn API 采用同一条待确认规则 | 旧门禁正是「重开原会话后不能继续」的直接原因 |
| [pilot_turns.py](../src/anchor/pilot_turns.py) | 删除 `unsafe_to_retry` | 同上；同时移除对旧 `pilot-steps.sqlite` 的读取 |

### 2.2 前端

| 文件 | 改动 |
| --- | --- |
| [links.ts](../apps/web/src/links.ts) / [links.test.ts](../apps/web/src/links.test.ts) | 解析 `#anchor/graph|run|artifact/...` 引用并映射到已有视图 |
| [App.tsx](../apps/web/src/App.tsx) | 捕获引用点击切换到已有页面；顶栏「返回会话」回到原 Session；会话选择提升为受控 |
| [Pilot.tsx](../apps/web/src/Pilot.tsx) | 支持受控会话；中断会话可直接发新消息；说明文案改为「删除工作流会再次确认」 |
| [markdown.tsx](../apps/web/src/markdown.tsx) | 不再把 `#anchor/...` 改写成 sanitizer 的 `user-content-` 命名空间（原先会让链接失效） |
| [pilot.spec.ts](../apps/web/e2e/pilot.spec.ts) | 审批用例改为删除场景；新增「引用跳转 + 返回原会话」用例 |
| [real-provider.spec.ts](../apps/web/e2e/real-provider.spec.ts) | 新增真实 provider + 浏览器 + 杀进程 + 续聊验收（需 `ANCHOR_REAL_PROVIDER=1`，默认跳过） |

### 2.3 验收脚本与文档

- [scripts/verify_pilot_provider.py](../scripts/verify_pilot_provider.py)（重写）：真实 DeepSeek 的 HTTP/SSE 验收。
- [scripts/verify_pilot_resume.py](../scripts/verify_pilot_resume.py)（新增）：真实 `kill -9` + 重启 + 续聊验收。
- 文档：[architecture.md](architecture.md)、[product-architecture.md](product-architecture.md)、[usage.md](usage.md) 已按实际实现更新；[AGENTS.md](../AGENTS.md) 收敛为只写长期原则（状态类内容移出台账之外的文件）。

## 3. 关键实现决策

先核对本地固定版本框架（`pydantic-ai-slim==2.46.0` / `pydantic-ai-harness==0.32.0`）再接线，四个结论决定了实现方式：

1. **必须打开 `capture_frontier`**。只开 `StepPersistence` 时，工具执行中被 `kill -9` 只留下 `events.jsonl` 与 `tool_effects.jsonl`，`continue_run` 直接 `LookupError`；打开后才有 `snapshots/*.json` 可供下一个进程读取。
2. **框架会拒绝在未处理调用上叠新 prompt**（`UserError: Cannot provide a new user prompt when the message history contains unprocessed tool calls`）。这是好事——它宁报错也不静默重放——所以只需把末尾响应标成 `state='interrupted'`，剩下交给框架。
3. **持久化 run ID ≠ turn ID**：配置 `agent_name` 后 run ID 是 `5:pilot<turn_id>` 的 base64，因此按 `conversation_id` 查记录，而不是拼 ID。
4. **压缩会改写继续对话所用的历史**（并留下 receipt）；被丢弃的消息只存在于该 run 更早的快照里。这一点已在 `architecture.md` 写明，不是「只影响单次请求」。

**一处需要说明的边界**：第 2 条的处理方式（标注框架自有的 `state` 字段）是我自行判断的范围。它是用框架公开语义接线、消息修复全部由框架完成，不是自研恢复引擎；如果你认为这类判断应当先取得同意，替代方案是不携带中断记录、只从最后一条完整消息继续，代价是丢掉「它试过什么」。

## 4. 验证

### 4.1 自动化

| 命令 | 结果 |
| --- | --- |
| `./.venv/bin/python -m pytest -n 8 --dist worksteal` | **300 passed in 98.57s，退出码 0** |
| `./.venv/bin/python -m pytest tests/test_pilot_turns.py tests/test_pilot_tools.py tests/test_session.py -q` | 33 passed |
| `./.venv/bin/python -m ruff check src tests scripts` | All checks passed |
| `./.venv/bin/python -m compileall -q src scripts` | 通过 |
| `npm --prefix apps/web test` | 23 passed |
| `npm --prefix apps/web run test:e2e` | 9 passed, 1 skipped（真实 provider 用例按需跳过） |
| `npm --prefix apps/web run build` | 通过 |

新增/重写的后端用例包括：真实子进程 `SIGKILL` 后重启续聊两例、`FileStepStore` 文件记录、普通操作直接执行并记账、删除仍需确认、未知结果不重放、压缩生效留下 receipt、复用 Session id 不继承旧记录、坏记录不阻断对话、`/messages` 不绕过待确认。

### 4.2 真实验收

**（a）真实 DeepSeek HTTP/SSE** — `./.venv/bin/python scripts/verify_pilot_provider.py`

直接建图与启动 Run（无二次审批）→ 沙箱产出 `result.txt` → 回复携带 `#anchor/run/...`、`#anchor/graph/...` → 删除确认与拒绝 → 过期确认被拒 → `session_ask` 问答 → 每轮提交去重 → 文件记录存在。

- Session：`work`（Run `20260926T131742`）、`reject`、`stale`、`ask`
- 证据：`.local/pilot-provider-qxmyajmk/`

**（b）真实进程中断 + 重启续聊** — `./.venv/bin/python scripts/verify_pilot_resume.py`

`SIGKILL` 两次（工具已开始无终态 / 工具结果已记录），同一数据根重启后在同一 Session 发新消息。

- 结果缺失例：模型报告结果不可用（回复含 `INTERRUPTED`），未重放
- 结果已记录例：模型原样读出 Run 标识 `20260926T131236`，且恢复轮没有再次调用 `run_list`
- Session / turn：`missing`（`086cad6b-9970-4905-bc30-bd9aae2a1c9d` interrupted → `696c42b2-3586-4a44-b467-c7cf8151cb3f` completed）、`recorded`（`209da1cf-ef88-4c07-8367-15d5e48817ba` interrupted → `9f8eef87-1865-4c22-9ef6-40dc24f27585` completed）
- 证据：`.local/pilot-resume-u3pvap_a/`

**（c）真实浏览器全链路** — `cd apps/web && ANCHOR_REAL_PROVIDER=1 npx playwright test e2e/real-provider.spec.ts`

真实流式回答 → 输出中途 `SIGKILL` → 页面显示服务未连接 → 重启 → 刷新 → 会话显示「回复已中断」且输入可用 → 新消息得到真实回答并带 Graph 引用 → 点击进入图编排页面 → 「返回会话」回到同一 Session。

- Session：`eceb861a-eb31-415e-b624-625739f66e9a`（turn `1b60cd1b-12ca-4d4d-8e0b-246f3cf7b8e9` interrupted、`aab81f96-9d7d-4315-b131-914c5b0ad3e8` completed）
- 证据：`.local/pilot-browser-acceptance/`（含 `real-provider-interrupted.png`、`real-provider-continued.png`）

### 4.3 验收矩阵

A07 / A08 / A09 / A12 → pass；A10（压缩）→ 已接线、真实长会话未验证；A11 / A13 / A14 属后续产品需求，本轮不验收。明细见[台账](pilot-development-plan.md)。

## 5. 自查发现的逻辑问题与修复

交付后按「保证代码逻辑」复查，发现并修掉三处，均补了回归用例：

1. **复用 Session id 会继承已删除会话的历史**。框架文件记录没有删除接口，删会话后记录仍在盘上，而续聊只按 `conversation_id` 查。修复：只采纳 `run.started_at >= session.created_at` 的记录。对照验证：去掉守卫时模型确实看到旧会话的问题，加回后看不到。
2. **一条坏记录会让所有会话发不出消息**。`list_runs` / 快照解析遇到半截 JSON 会抛异常并冒到接口层。修复：读记录改为 best-effort，读不出就打印 `pilot_record_unreadable` 并回退到已保存对话；Session 仍是 `interrupted`，界面照常显示中断，不假装记录不存在。
3. **旧 `/messages` 入口能绕过待确认操作**。它会先把会话改成 active 再执行，而待确认的调用会被当作中断收尾，之后再确认就会失败。修复：与 turn API 采用同一条规则。

## 6. 未验证 / 未完成

- **真实长会话触发压缩**：只在刻意调小阈值的会话上验证过触发路径与 receipt。且压缩会改写后续对话所依据的历史，边界见 §3。
- **「提问暂停期间杀进程，重启后回答」**：`session_ask` 问答本身已用真实 provider 验过；与杀进程组合未验。
- **真实 provider 下的停止 / 断线重连**：仅 mock / FunctionModel 级回归。
- 多进程、多租户、服务级鉴权不在本轮；turn 去重与 SSE 游标仍依赖单进程内锁。
- 系统 Pilot、可见计划、研究合同、附件、编辑分支、导出：指令明确本轮不开发，未动。

## 7. 复现与证据

```bash
# 自动化
./.venv/bin/python -m pytest -n 8 --dist worksteal
npm --prefix apps/web test && npm --prefix apps/web run test:e2e && npm --prefix apps/web run build

# 真实验收（会产生模型调用费用，使用独立数据目录，不影响正在运行的服务）
./.venv/bin/python scripts/verify_pilot_provider.py
./.venv/bin/python scripts/verify_pilot_resume.py
cd apps/web && ANCHOR_REAL_PROVIDER=1 npx playwright test e2e/real-provider.spec.ts
```

| 证据目录 | 内容 |
| --- | --- |
| `.local/pilot-provider-qxmyajmk/` | Session、Harness 对话、`state/pilot-steps/` 文件记录、turn/SSE、真实沙箱 Run 产物 |
| `.local/pilot-resume-u3pvap_a/` | 两次 SIGKILL 前后同一数据根的会话、记录与 turn 状态、服务日志 |
| `.local/pilot-browser-acceptance/` | 浏览器验收的数据根与两张截图 |

## 8. 注意事项

- 本轮改动**全部留在工作区未提交**（`src/`、`apps/web/`、`docs/`、`AGENTS.md`）；未触碰无关改动，也未清理历史。
- 三个真实验收脚本都会调用真实模型，请在需要时显式运行；`real-provider.spec.ts` 默认跳过。
- 报告中的证据目录是本地数据，不在版本控制内；如需长期保留请自行归档。
