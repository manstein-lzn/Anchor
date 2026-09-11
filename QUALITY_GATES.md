# 质量门禁

> **原则：干净正交不是设计出来的，是被门禁逼出来的。**
> 判不了的规则 = 不存在的规则。本文件列出**可自动判定**的规则与运行方式。

---

## 1. 五道门

| 门 | 位置 | 判定 |
|---|---|---|
| **架构测试** | `tests/test_architecture.py` | 依赖方向、边界所有权、反模式棘轮 |
| **复杂度/体积** | `scripts/quality_gate.py` + `quality-baseline.json` | 指标只减不增 |
| **Lint** | `ruff`（`pyproject.toml`） | 正确性与架构规则 |
| **类型** | `mypy`（棘轮） | 错误数不增加 |
| **交付物门禁** | `runtime/academic.py` + `runtime/evidence.py` | 论文的骨架、行文、数字与引用可核验性 |

```bash
.venv/bin/pytest -q tests/test_architecture.py
.venv/bin/python scripts/quality_gate.py          # 加 --fast 跳过 mypy
```

### 交付物门禁（第五道）

代码的门禁只管代码。学术图的产出是一篇论文，它同样需要能被自动判定，否则
「面向读者」只是一句口号。判据全部是确定性的，失败即退回重做：

| 判据 | 位置 | 失败结果 |
|---|---|---|
| 必需章节、锚点顺序、≥2 个主题节、禁止 catch-all 章节 | `structure_errors` | 退回作者 |
| 正文禁止证据级别标签、过程语言、内容哈希 | `craft_errors` | 退回作者 |
| 段落 ≤1200 字符、Survey Methodology ≤2500 字符、摘要 ≤1800 字符 | 同上 | 退回作者 |
| 每个结果型数字至少 1 篇**读过全文**的来源 | `unsupported_number_claims` | 退回作者 |
| 引用必须可验证（id 在检索结果中、读取为同一篇） | `verify_source` | 退回作者 |
| 来源数 / 全文阅读数下限 | 同上 | 退回**采集** |
| 只报 minor 且确定性检查通过 | `evaluate_review` | **批准**（ADR-043） |
| 同一机械缺陷连续 3 次未改 | 同上 | `blocked`，交人工 |

这套门禁在真实运行中抓出过：章节编号不匹配、中英文标题、JSON 前缀、同篇论文的
两个 URL、引用编号漂移、把摘要数字当全文数字。**单元测试一个都没抓到这些。**

---

## 2. 架构规则（A1–A5）

| # | 规则 | 强制 |
|---|---|---|
| **A1** | 依赖单向：`domain ← state ← runtime ← api` | `test_dependencies_only_point_downward` |
| **A2** | 顶层模块必须属于已知层 | `test_every_module_belongs_to_a_known_layer` |
| **A3** | 内容引用前缀只在边界模块出现 | `test_content_reference_prefixes_stay_in_the_boundary` |
| **A4** | 节点类型分发走注册表，不用字符串比较 | `test_node_type_dispatch_is_ratcheted` |
| **A5** | 禁止静默吞异常（`except: pass`） | `test_no_silently_swallowed_exceptions` |
| **A6** | 模块体积预算 | `test_modules_stay_within_the_size_budget` |

### 边界模块白名单（A3）

`artifact://` / `workspace://` 只允许出现在：

```
runtime/artifacts.py    边界本身
state/storage.py        引用扫描
runtime/integrity.py    证据可读性校验
runtime/resolution.py   前驱产物解析
runtime/verifier.py     已验证产物绑定
api/app.py              产物读取端点
```

新增内容后端时，**只改这里，不在别处解析字符串**。

---

## 3. 实现规则（B1–B6）

| # | 规则 | 反例 | 强制方式 |
|---|---|---|---|
| **B1** | 状态用枚举 + 转移表 | 布尔标志累积 | 非法转移抛错 + 转移测试 |
| **B2** | 禁止字符串分发 | `node.type.value == "tool"` | A4 |
| **B3** | 禁止静默吞异常 | `except Exception: pass` | A5 + ruff `BLE001` |
| **B4** | 禁止隐式 fallback | 内容缺失就退回当前工作区 | 评审 + 专项测试（失败关闭） |
| **B5** | 复杂度预算 | 单函数分支爆炸 | `ruff C901`（max 15）+ 棘轮 |
| **B6** | 单一入口 | 两份 mark-sweep / 两套提交 | 架构测试断言唯一实现 |

---

## 4. 棘轮政策（ratchet）

> **存量不强制立刻改，但数量只减不增。**

- 基线存于 `quality-baseline.json`（ruff 规则计数、mypy 错误数、最大模块行数）。
- 任何指标上升 → 门禁失败。
- `--update` 可以改写基线，但**必须是一次评审过的、有意的改动**，且提交时能看到 diff。
- 白名单（如 `NODE_TYPE_DISPATCH_ALLOWED`）同理：可以删除，不能新增。

**为什么用棘轮**：存量 238 个 mypy 错误不可能一次清零；但如果不设门禁，半年后会变成 400 个。

---

## 5. Definition of Done

每个 W 阶段（含新功能与重构）必须同时交付：

```
[ ] 边界测试：content_ref parse/serialize 往返、digest 确定性
[ ] 不变量测试：对应 I1–I9 至少一条，破坏即红
[ ] 故障注入：涉及提交/恢复时必须覆盖 CONTENT_COMMIT_PROTOCOL.md 的崩溃窗口
[ ] 架构测试：新增模块/依赖后 test_architecture.py 仍绿
[ ] ADR 或 ADR 修订：含"它不做什么"
[ ] quality_gate 通过
```

**缺任何一项，不算完成。**

### 重构优先规则

> **一个概念需要改动 3 个以上模块才能落地 → 先抽概念，再改功能。**

---

## 6. 内容面专项反模式清单

这是最可能变成屎山的地方，提前定探测器：

| 反模式 | 探测器 |
|---|---|
| 工作区逻辑渗进 worker/control | A1 依赖方向 |
| 沙箱后端 `if isinstance(...)` | 必须 `SandboxProvider` 协议；评审 |
| `content_ref` 字符串散落 | A3 |
| 两套 GC / 两套提交路径 | B6 |
| 缺失内容静默降级 | B4 + 故障注入 W5 |
| 控制/内容两套恢复路径漂移 | 恢复测试同时覆盖两侧 |

---

## 7. 当前基线（2026-09-09）

| 指标 | 基线 | 说明 |
|---|---|---|
| ruff | `C901: 13` | 全部为存量复杂函数（`create_app` 123、`cli.dispatch` 49 最大） |
| mypy | `238` | 存量类型错误，新代码不得增加 |
| 最大模块 | `api/app.py 708` | 组合根，单独预算 750；其余 600 |
| 依赖方向违规 | `0` | 已修复 `state/operations.py → runtime` 违规 |
| 静默吞异常 | `0` | 已收窄或标注理由 |

**已修复的存量缺陷**（本轮门禁直接发现）：

- `state/operations.py` 导入 `runtime.artifacts`/`runtime.settings`（分层违规）→ 改为注入 `read_artifact`
- `runtime/agent_tools.py` 闭包捕获循环变量 `capability`（潜在错配 bug）→ 绑定为默认参数
- `state/checkpoints.py` / `state/protocols.py` 注解缺少 `datetime` 导入；`state/operations.py` 缺 `NodeRun`
- 3 处死赋值、14 处未使用导入

---

## 8. 与 CI 的接入

```bash
.venv/bin/pytest -q tests/test_architecture.py   # 架构门
.venv/bin/python scripts/quality_gate.py         # 棘轮门
.venv/bin/pytest                                  # 全量测试
```

三者都绿才算通过。**架构门和棘轮门必须在全量测试之前跑**——它们最快、最能拦住结构性错误。
