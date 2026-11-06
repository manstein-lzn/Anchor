---
name: graph-authoring
description: 按当前作者模型编写 Graph，验证模型引用、节点边界与显式 Artifact 文件交付。
---

# Graph 编写

先 `graph_read` 检查已有定义，再读取 `graph_validate` / `graph_create` / `graph_update` 的实际 schema。作者 JSON 不是 Runtime IR 或 bundle manifest；不手写内部资源清单。先做一个可运行的小图，验证后按需求扩展。

- `ops`、`agents` 是按名称索引的对象；`nodes` 通过 `{"id":"n","op":"step"}` 或 `agent` 引用；`entry` 是节点 ID，`edges` 用 `from` / `to`。Op 的 `run` 是 shell 字符串，不是 argv。`reads`、`writes`、`network` 写在 Op/Agent 定义；安装后的 Plugin ID 列表 `plugins` 写在 AgentNode，不写角色。声明不扩大宿主授权。
- Agent `model` 可省略，或用 `models.default`（默认 Flash）、`models.default.flash`、`models.default.pro`、`models.backup.flash`、`models.backup.pro`。不要把 provider 模型名或 `default` 当设置引用；backup 只显式选择。每个新 invocation 读取配置，当前调用和同 invocation 恢复保留冻结绑定。
- Agent 必须调用 `final_result`，显式给 `files: string[]`；纯文字用 `files: []`，其余参数按实际 schema。路径相对 `/workspace`；可交付目录及普通二进制，不能用根目录、通配符、路径逃逸、链接或特殊文件。`writes` 是必交子集，必须存在且由 `files` 覆盖；Op 直接按 `writes` 交付。
- 每个节点有独立、稳定的 `/workspace`，同 Run 不共享工作树。下游仅从边关联 commit 的只读 `/in/<源节点 ID>` 获得显式交付的 Artifact；`reads` 不是自动复制或共享。需要编辑上游文件时，只复制所需文件到当前节点工作区。未交付的 Git、依赖、缓存留原节点；不要打包整树。

最小文件交付示例（先校验，不隐式启动）：

```json
{"definition":{"entry":"hello","ops":{"hello":{"run":"printf 'hello\n' > hello.txt","writes":["hello.txt"]}},"nodes":[{"id":"hello","op":"hello"}],"edges":[]}}
```

将此对象传 `graph_validate`，按诊断修正；保存用 `graph_create({"name":"hello","definition":...})` 或 `graph_update({"graph":"已有ID","definition":...})`。保存与启动分开，只有用户要求才 `graph_run({"graph":"hello"})`；接纳 Run ID 不等于完成，通过 `run_status` 检查。停止仍可恢复；永久收束走 `run_abandon` 的原生确认，保留历史与现场。错误恢复先读事实，不把旧执行记录当 Goose 恢复游标。
