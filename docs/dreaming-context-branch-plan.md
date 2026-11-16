# Dreaming 上下文分支退役记录

2026-11-15：用户已要求删除复杂 Dreaming 专用系统，Host 专用 frozen 实验执行、局部分支选源/冻结/上下文适配、自身 meta，以及依赖旧生命周期的实验协议一并删除，不作为当前产品路径或活跃计划维护。普通 Graph/Runner、Goose 自身会话/上下文/compaction、Artifact、Hub 全局分析/窗口/安全视图和独立 ordinary/input-discovery 质量工具保留，不建立替代系统；范围见[Dreaming 退役说明](dreaming.md)。

历史 Run 元数据及 `dreaming`/`experiment` 身份保留、可读，但不能按普通任务启动、resume 或自动接续；普通恢复与权限边界不变。本地服务已按用户授权切换到退役后的完整 format 5 发行，旧 release 文件和历史保留，未迁移旧实验、恢复 meta 或重跑业务图。源码删除与后续切换分别见[删除验收](../experiments/dreaming-removal-20261115/README.md)、上线记录（本机原件，未入库：`experiments/dreaming-removal-20261115/rollout-20261115/README.md`）及[统一台账](pilot-development-plan.md#当前验收结论)，不补签历史分支收益。

## 历史证据

以下保留当时的结果与限制，不作为当前接口、部署说明或继续实施的授权：

| 阶段 | 原结论与证据 |
| --- | --- |
| 真实模型可行性 | 六对分支、两个负对照及从头参照，共 32 请求、0 Graph Run；两臂均 5/6，负对照 0/2，未证明净收益，见原实验（本机原件，未入库：`experiments/dreaming-context-branch/real-provider/README.md`） |
| 产品链路接入 | 真实 Provider 四个分支及独立回执，共 26 请求；候选后缀 9 对基线 7，质量/改善 inconclusive、未发布；测试错误预设断言导致后段真实恢复未覆盖，见原接入验收（本机原件，未入库：`experiments/dreaming-context-branch/product-acceptance-20261114/README.md`） |
| 范围内自动选源 | 固定 Goose 配本地确定性 Provider 验证四个分支、独立淘汰及准确回执恢复；不证明真实生产覆盖或收益，见原自动选源验收（本机原件，未入库：`experiments/dreaming-context-branch/automatic-selection-20261115/README.md`） |
| 旧 release 上线及生产分析 | 239 次真实 Provider 请求、3 条提议；缺原生上下文范围和适用局部质量包，跳过实验，零整图回退、零发布，见原上线记录（本机原件，未入库：`experiments/dreaming-context-branch/rollout-20261115/README.md`） |
| 晨报隔离 A/B | 两个真实保留源、四对八臂；候选 30 请求对基线 38，但两臂各仅 1/4 通过局部计划契约，候选淘汰、未发布，不证明业务质量或生产收益，见原晨报验收（本机原件，未入库：`experiments/dreaming-context-branch/morning-briefing-ab-20261115/README.md`） |

同日 D0 新窗口 119 请求不是同输入开销对照，也不是新发行上线。原始实验材料按[本机证据规则](development.md#文档归属)保留、不改写，不整体入库；历史命令、未关闭方案和“下一步”不转为当前待办。
