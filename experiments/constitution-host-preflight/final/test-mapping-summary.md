# M5 测试函数与迁移登记汇总

测量输入固定为迁移前 fc558e2（fc558e279eea9aaa68f4f26730285c3ee6f18515）和迁移后 e60374f（e60374fd06eda07de8df9c41e64a5e03f6e3246d）。统计脚本读取 Git 对象，不读取后续工作区源码；下列历史登记也核对了 e60374f 中的内容。

## 源码函数数量

| 口径 | fc558e2 | e60374f | 变化 |
| --- | ---: | ---: | ---: |
| Rust workspace 物理测试函数，含 ignored | 1,821 | 2,162 | +341 |
| 其中普通测试函数 | 1,706 | 2,036 | +330 |
| 其中显式 ignored 测试函数 | 115 | 126 | +11 |
| ignored 中需要固定 Goose 的函数 | 111 | 122 | +11 |
| 其他 ignored 函数 | 4 | 4 | 0 |
| Web 单测 callback 定义 | 213（33 文件） | 280（43 文件） | +67 |
| Web 单测中的 it.each 定义 | 20 | 22 | +2 |
| Web 浏览器测试 callback 定义 | 84（37 文件） | 99（44 文件） | +15 |
| 浏览器条件 skip 调用，不算测试定义 | 10 | 12 | +2 |

Rust 按 workspace member 中带 #[test] 或 #[tokio::test] 的物理 fn 统计，内联 #[cfg(test)] 模块包含在内。两侧没有 rstest、async_std::test、cfg_attr 测试生成或 macro_rules 测试生成；脚本支持前两类显式测试属性。普通 helper fn 不计数，确有测试属性的进程 helper 每侧 3 个，保留在总数中并在 JSON 单列。所有源码 cfg 分支均计入，非当前 Linux/feature 组合下执行次数；函数自身 cfg 属性每侧 25 / 30 项，模块继承 cfg 不另展开。doctest 不属于 fn，另由执行报告记录。

Goose 子集按 ignore 原因含 Goose 判定，包含 workspace 其他 crate 的独立分发测试。其余 4 个 ignored 是 HTTPS 子进程 helper、外部 HTTPS MCP、慢命令和 Rustup 部署环境测试。JSON 给出每个 ignored 的文件、行号和原因，不能将 Host 的完整 --ignored 数字直接称为 Goose 函数数。

Web 由 TypeScript AST 识别 it/test 的 callback，排除 hooks、setTimeout、use 和 test.skip(condition, reason)。每个 it.each 只计 1 个 callback；两侧字面参数行合计 89 / 98，describe.each 的套件也会重复实例化内部 callback。因此源码 callback 与 Vitest 实例不同。浏览器同理，含测试定义外的循环以及环境条件 skip，定义数不是本轮已执行数。

复跑：

~~~sh
python experiments/constitution-host-preflight/final/test_inventory.py \
  --before fc558e2 --after e60374f
~~~

需要 Python 3.11+、Node 与项目已安装的 TypeScript；解析器版本来自项目固定依赖。输入源码全部经 git show 读取，不导入或执行任何测试。分 crate / 文件及位置详见 [test-counts.json](test-counts.json)，实现见 [test_inventory.py](test_inventory.py)。本项仅执行统计脚本，未重跑 workspace、Web 或完整 Goose。

## 与已执行结果的关系

M4 已有证据为 workspace 2,200 / 0 / 126、Web 359 和 Host 完整 ignored 123 / 0 / 0；这里只引用 [M4 checks](../m4/checks.json)，不将其冒充 M5 新执行。

- Rust 测试函数物理总数为 2,162。六个普通 Goose 夹具测试被 28 个 integration target 引用，源码只算 6 次，执行多算 6 × 27 = 162 次；再加 Runtime doctest 2 次，2,162 + 162 + 2 = 2,326，等于 M4 workspace 2,200 + 126。迁移前六个相同夹具被 25 个 target 引用，同样会重复执行；旧侧没有为 M5 再运行。
- M4 Host 完整 ignored 123 包含 121 个 Host Goose 函数和 2 个其他条件测试；workspace 另有独立分发 Goose 1 项以及 HTTPS 2 项，故 workspace ignored 为 126、Goose 物理子集为 122。
- Web M4 的 359 是参数与套件展开后的执行实例。源码单测 280 个 callback，22 个 each 合计 98 行增加 76 个实例；PluginSettings.test.tsx 的 describe.each 两个上下文把内部 3 个 callback 各执行两次，再增加 3；280 + 76 + 3 = 359。不要把 359 写为 359 个独立源码函数。
- M2 的助手曾通过独立 Kernel target 重复编译同一 12 个场景，M3 删除重复 target；12 个普通助手场景仍保留。这影响执行次数，未删除那 12 个源码业务函数，见 [M3 Goose 登记](../m3/goose-regressions.md) 和 [M2 完成交回](../m2/completion-verification.md)。

## M1–M4 断言修改

登记的计量单位并不一致：M1/M1.5 是表达式或目标位置，后续含参数矩阵、迁移类别和夹具准备。下表保留原单位，不相加成一个虚假的“断言总数”，也不将表格行数当测试函数数。

| 阶段 | 登记数量与口径 | 证据 |
| --- | --- | --- |
| M1 | Rust 26 处（20 断言期望、6 轮询目标），Web 4 处；fixture schema 增补另列 | [Rust 逐项](../m1/rust-assertions.md)、[Web 逐项](../m1/web-assertions.md) |
| M1.5 | Rust 21 处（M15R01–21），Web fixture 1 处（M15W01）；范围编号按实际序号展开，不按表格行数 | [M1.5 登记](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md) |
| M2 并行模块、Host 多槽位、删除、Probe、Call 切片 | 既有业务断言变更 0；新测试/差分组不混算旧断言变更 | [M2 分阶段登记](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)、[M2 验证](../m2/verification.md) |
| M2 §10.11 合并 | MRG01–11，共 11 组状态/投影登记；相对合并前分支新增 2 组，其余为继承适配 | [合并登记](../m2/merge-verification.md) |
| M2 §10.12 与收尾同步 | 原助手 12 项业务断言原样复用；旧业务放宽 0；测试兼容投影 1 组，收尾同步权限夹具 1 组；新覆盖数量不计旧断言修改 | [§10.12 登记与收尾](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)、[完成交回](../m2/completion-verification.md) |
| M3 §10.13 与 §10.15 | A1–A10 共 10 类，A11–A13 追加 3 类，共 13 类；其中人工恢复为用户裁定的 M2 行为更正 | [M3 登记](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)、[§10.15 追加](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md) |
| M3 结束同步与 §10.18 | 结束同步登记 3 个适配文件；§10.18 交回为 3 类机械适配，范围有继承关系，不再次累加到 A1–A13 | [同步登记](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)、[§10.18 交回](../m3/merge-verification.md) |
| M4 §10.19 | M4A1–M4A9，共 9 类；新增 Web 3 项，业务放宽 0 | [M4 逐项](../m4/assertion-mapping.md) |

## 删除、改名与等价去向

| 阶段 | 数量与边界 | 证据 |
| --- | --- | --- |
| M1、M1.5 | 删除测试均为 0；未对既有测试新增 ignore | [M1 Rust](../m1/rust-assertions.md)、[M1 Web](../m1/web-assertions.md)、[M1.5](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md) |
| M2 前置各切片 / §10.12 | 删除既有测试 0；未对既有测试新增 ignore | [M2 登记](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)、[§10.12](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md) |
| M2 §10.11 合并 | 5 行去向登记，覆盖 1 个退出的旧 Deleted 参数分支、6 个原位改名、2 个既有等价映射、1 个 M1.5 改名、1 个函数拆分为 2 场景；整项业务覆盖删除 0 | [合并映射](../m2/merge-verification.md) |
| M3 Runtime | 73 个原测试逐项映射并保留名称，包含测试 helper；随后 M4 的改名另登记 | [73 项](../m3/runtime-test-mapping.md) |
| M3 Host / Graph host | 39 个消失函数名逐项给出改名、合并、等价覆盖或旧机制退出理由；39 不等于删除 39 个业务测试 | [39 项](../m3/host-test-mapping.md) |
| M3 Goose / §10.15 | 删除重复 target 1 个，重复执行 12 项由默认 target 保留；人工恢复函数改名 1 项；未删除这些业务场景 | [Goose 映射](../m3/goose-regressions.md)、[§10.15 准备与改名](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md) |
| M3 §10.18 | 本轮删除 0、既有测试新增 ignore 0；master 新增 45 个 Rust 测试名全保留 | [合并交回](../m3/merge-verification.md)、[45 项](../m3/merge-test-inventory.json) |
| M4 | 34 个函数（含 helper）改名/等价替换，删除 0、既有测试新增 ignore 0、冻结快照修改 0 | [34 项机器映射](../m4/test-mapping.json)、[断言登记](../m4/assertion-mapping.md) |

这些是不同阶段、不同基线的登记，不将 73 + 39 + 34 解释为 146 个互不重叠函数。旧机制确实退出的条目仍按原映射提供理由，不用总测试增长代替逐项去向。

## S 候选

M1–M4 当前有效并已批准的编号共 8 个：M1 的 S19–S24、S26 共 7 个，M3 的 S27 共 1 个。M1.5、M2 各切片与 §10.11/§10.12、M3 §10.15/§10.18、M4 新增均为 0。S25 作废，不计有效候选；S27 承接并结清历史 S4，不再重复计 S4。W4 尚待决定，不计已批准 S。

证据：[M1 与 M1.5](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)、[M2 各切片](../../../docs/archive/runtime-migration-2026-10-20/constitution-refactor-log.md)、[M2 完成](../m2/completion-verification.md)、[M3 §10.15](../m3/completion-verification.md)、[M3 §10.18](../m3/merge-verification.md)、[M4](../m4/verification.md)。
