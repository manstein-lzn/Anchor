# Graph 作者模型与 Runtime IR 契约

普通用户继续编辑同一种 Graph JSON；`GraphSnapshot::from_authoring` 负责校验、规范化、模块展开和资源绑定。WebUI 不要求用户手工改写执行 IR 或编辑 `manifest.json`。

```text
画布 / Graph JSON → Authoring Model → Compiler / Admission
                  → GraphSnapshot + 资源 manifest → 共享 Runner
```

## 两层模型

作者模型表达 `agents`、`ops`、内联 `graphs`、`nodes`、`edges`、目标/接口、输入输出、读写/网络声明、Plugin 和 `layout`。角色只复用配置，Plugin 直接挂节点。

Runtime IR 保存稳定展开节点、路由/反馈/并行、输入与权限、绑定 Plugin 和执行所需事实。它不承载编辑器布局，也不能静默忽略模型、读写、调用模式或权限。未知字段与不支持组合必须在编译/接纳时明确诊断，不能伪装为可运行配置。

## 编译与保存

1. 解析作者定义并校验 schema、拓扑、接口、模块和读写声明。
2. 展开内联模块，生成稳定节点 ID，保留原作者定义供重开编辑。
3. 从 canonical Library 解析 Plugin，生成精确无密钥资源闭包。
4. 编译反馈、轮次、fanout/join、Graph Call、输入映射和文件引用。
5. 写出定义与摘要 manifest，bundle loader 再验证资源与绑定。
6. 返回具体位置和原因；保存成功与 provider/业务服务可用分别验收。

| 用户动作 | 应维持的契约 |
| --- | --- |
| 修改 Agent/Op | 编译错误指出角色/节点，定义可重开 |
| 设置反馈 | 检查出口和轮次，保留同 Run 文件延续与恢复身份 |
| 挂载 Plugin | 已安装资源校验与摘要绑定；未安装/越权明确拒绝 |
| 内联模块 | 节点身份稳定，原模块与 layout 往返保存 |
| fanout/join | 配对、独立分支、收束和失败边界提前校验 |
| 独立 Call | 目标、wait/detach、输入映射与父子事实明确 |
| 修改 layout | 仅保存展示信息，不扩大执行权限 |

作者模型断言、固定展开样例与 HTTP CRUD 测试位于 Rust crate 的测试范围。测试样例不执行另一个语言解析器；具体本轮结果见 [开发台账](pilot-development-plan.md)。历史快照只用于解释先前兼容来源。

## 当前边界

保存校验不验证模型凭据、真实网络、业务内容质量或所有用户 Graph 的执行结果。Plugin 闭包来自宿主 canonical catalog，不从请求接受任意本地路径。精确累计预算、嵌套并行、分支交叉和区域内 Call 等未支持能力应明确拒绝。

现有 Graph 字段可解析不等于已完成运行级兼容；RSI、周报和深度研究需要其工具、输入与门禁分别验收。目标设计中的编辑器扩展也不能自动视为支持。独立包可以隐藏 Runtime 源码，但 Graph、Skill 和资源仍是用户资产。

共享职责与运行不变量见 [产品架构](product-architecture.md)，当前实现见 [当前架构](architecture.md)。
