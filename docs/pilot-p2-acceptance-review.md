# Anchor Pilot P2 独立验收

日期：2026-09-26。验收对象：当前工作区代码及 [开发汇报](pilot-p2-delivery-report.md)。

## 结论

暂不通过完整交付验收。原生持久记录、带新消息的短对话中断续聊、普通操作免重复审批等路径已经跑通；下面五处接线问题仍需修正。其中三处影响历史恢复或未保存编辑，两处影响产物跳转和压缩阈值。

本轮只验收并记录问题，没有修改产品代码。修复应继续复用现有框架和界面，不增加恢复引擎、记忆系统或审批机制。

## 必须修正

### R1 / P1：“继续上次回复”不加载中断工作记录

位置：`src/anchor/pilot.py:488`（`respond`）；`apps/web/src/Pilot.tsx:164`（`resume`）。

`attempt_history` 只在 `prompt is not None` 时调用；界面的“继续上次回复”提交空 prompt，所以仍使用崩溃前的 conversation store。相比发送新消息，这条入口丢掉了已完成的工具结果和未完成调用事实，模型可能再次发起原来的工作。

独立复现：复用 `tests/test_pilot_turns.py` 的 `_kill_a_pilot_process`，真实杀死工具执行中的子进程；新建 Scheduler 后调用 `pilot_message('killed', None)`，用 FunctionModel 检查实际收到的历史。

结果：HTTP 等价状态为 200，但模型的 `tools_seen=[]`、`recorded_result_seen=False`，只看到原始“先读一下现场”。同样数据用新消息续聊的现有测试能看到 `recorded-result`。

复验要求：两种续聊入口都能读到已经保存的结果及未完成调用，不盲目重发；保留 deferred 回答/确认的原语义。

### R2 / P1：压缩后的新记录被按长度误判为旧记录

位置：`src/anchor/pilot.py:475`（`attempt_history`）。

`is_provider_valid(recorded) and len(recorded) <= len(saved)` 不能判断记录是否更新。压缩会减少消息条数，即便新快照包含最新工具结果，也会被这条判断丢弃，下一轮回退到旧 conversation store。

独立复现：在真实 Scheduler / FileStepStore / compaction 路径上使用 FunctionModel，配置 `max_messages=10, keep_messages=2`，先完成四轮普通对话，再调用三次返回唯一标识的工具，随后注入 provider 异常。通过框架 `continue_run` 读取中断记录，再发送新消息。

结果：`saved_len=9`、`recorded_len=8`、`recorded_valid=True`；新快照包含唯一工具结果，但 `attempt_selected=False`，下一轮模型看不到该结果。

复验要求：历史选择依据执行关联和保存状态，不能以消息数量代表新旧；补“压缩后中断 → 续聊”的回归。修复属于存储接线，不需要另造历史合并引擎。

### R3 / P1：聊天链接切换工作流会丢失未保存编辑

位置：`apps/web/src/App.tsx:163`（`openReference`）。

该入口直接调用 `setName`，绕开同文件已有的 `selectGraph` 未保存检查；随后加载另一张 Graph 并清空撤销历史。

独立浏览器复现：编辑 alpha 的目标为 `UNSAVED_REVIEW_EDIT`，不保存；切到 Pilot，点击 beta 的 Graph 链接，再切回 alpha。使用真实 Vite / Chromium，API 返回隔离的模拟数据，不触碰用户 Graph。

结果：确认对话框出现次数为 0；返回 alpha 后目标恢复为 `saved objective`，未保存内容丢失。

复验要求：聊天引用遵守已有切换保护；取消切换时保留编辑和当前位置，确认后才切换。

### R4 / P2：Artifact 链接丢弃文件路径，只打开节点对话

位置：`apps/web/src/links.ts:40`（`anchorTarget`）；`apps/web/src/RunInspector.tsx`。

解析器读出了 `path`，但导航目标没有传递它。RunInspector 默认显示对话，既未切到文件，也未展开指定产物。

独立浏览器复现：点击 `#anchor/artifact/review-run/write/result.txt`，节点标题变为 write，但“对话”仍为选中状态，“文件”为未选中状态；指定文件未打开。

现有 `pilot.spec.ts` 只断言节点标题，没有断言文件内容，因而不能证明“文件直接跳转”完成。

复验要求：点击产物引用后直接定位指定文件，使用现有文件界面即可；返回时仍是原会话。测试应断言目标文件或内容，而不只是节点名。

### R5 / P2：配置中的模型窗口没有传给框架压缩

位置：`src/anchor/pilot.py:345` 及 `:360`、`:364`（`_compaction`）。

`profile.context_window` 只被用于判断是否启用 `max_fraction`，实际窗口数值没有传给 SummarizingCompaction 或 SlidingWindowCompaction。框架因此按注册信息或默认窗口计算阈值，而非用户配置的窗口。

独立接口检查：配置窗口 4096 后，创建出的 capability 的 `context_window=None`；对框架不认识的模型，实际触发阈值为 120000 tokens，而配置窗口的 60% 应是 2457。少于 200 条但文本很长的对话可能先超出窗口。

复验要求：正确传递模型窗口，并核对摘要和滑窗两种接法；真实长会话压缩尚未通过，不能以调小消息条数的测试代替完整验收。

## 已独立核对的通过项

- 全量后端独立复跑：`./.venv/bin/python -m pytest -o addopts='' -q -n 8 --dist worksteal --junitxml=.local/pilot-p2-review-pytest.xml`，300 passed in 99.78s，退出码 0。[JUnit 结果](../.local/pilot-p2-review-pytest.xml)保留为本轮证据；覆盖了前述 33 项相关用例。
- 前端 23 项单测、Ruff、compileall、前端 build、`git diff --check` 通过。
- 前端 E2E：9 passed，1 skipped，退出码 0。跳过的是 opt-in 真实 provider 浏览器用例，本轮未独立重跑该用例；浏览器问题复现另用真实 Chromium、模拟 API 完成。
- `scripts/verify_pilot_resume.py` 独立复跑通过：真实 DeepSeek、两次 SIGKILL、相同数据根重启、同一 Session 带新输入继续，覆盖结果缺失和结果已经记录。证据：[pilot-resume-ga78jp5d](../.local/pilot-resume-ga78jp5d/)，准备 Run `20260926T135924`。这不覆盖 R1 的空 prompt 恢复或 R2 的压缩后恢复。
- `scripts/verify_pilot_provider.py` 独立复跑通过，退出码 0：普通建图与沙箱 Run、删除拒绝、过期删除确认、必要提问和提交去重。证据：[pilot-provider-hntv2f1d](../.local/pilot-provider-hntv2f1d/)，Run `20260926T140327`。
- 本地原交付证据目录存在；原交付的浏览器截图和记录只能证明当时的相应路径，不能覆盖本轮复现出的遗漏。

## 文档与验收口径

- 台账阶段总表将 A07–A10、A12 整体写为完成，但 A10 自己仍注明真实长会话未验证。应按路径分别记录。
- 产品架构同时保留“续聊已有实现并验收”和“仍未完成真实验收”，需在修复交付时统一。
- `_close_unfinished` 使用的是框架已有的 `state='interrupted'` 语义；已对照本地框架代码确认。该适配本身不是要求重建恢复引擎的理由。
- 提问暂停期间重启、真实 provider 停止/断线重连仍按原报告列为未验证；本轮不扩大为多进程或多租户任务。

先修 R1–R5、补对应回归和缺失的真实验收，再提交独立复验；不要扩展后续产品需求。

## 修复记录（2026-09-26）

R1–R5 已完成代码修复：恢复入口统一读取原生快照；快照选择按 Harness 时间戳；中断调用由 PydanticAI 生成 interrupted tool return；压缩能力接收配置的 context window；对象跳转保留未保存编辑保护并直接打开目标文件。后端相关回归 34 项、前端 23 项单测、build、compileall 与 diff 检查通过。真实 provider 的空 prompt 续聊、真实长会话压缩和完整浏览器复验仍需单独运行，未将其写成已通过。
