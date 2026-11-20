# 固定 Goose 的兼容补丁

`goose-1.53.0-responses-reasoning.patch` 只修改 Goose 的原生会话类型和 Responses Provider。基线为 Goose v1.53.0，源码归档 SHA-256：

```text
85cc5e76a12e364032df4bec0ed0738a910c3e12ec12731b4405d1fdb18d96ae
```

修复保留 Responses reasoning item 的完整 JSON，包括 `id`、`status`、`content`、`summary`、`encrypted_content` 和 Provider 扩展字段。原始 item 存入 `ThinkingContentBlock.provider_metadata` 可选字段，随 Goose 原生 Session 序列化，在下一次 Responses `input` 中按原顺序回传。旧记录缺少该字段时仍可加载；旧记录已经丢失的 reasoning 不能由此修复重建。

`response.output_item.done` 和 `response.completed` 的重复 item，以及 Goose 将一次模型输出拆成多个工具调用消息时克隆的 reasoning，按原始 response ID 与 reasoning item 身份去重。不同 response 中相同的无 ID item 保持独立。Responses 请求投影将同一原始 response 中的工具调用放在其结果之前，避免原生会话中的 `call/result/call/result` 被服务端解释为第二个缺少 reasoning 的助手响应；原生会话事实不重排，真实用户消息与不同 response 保持分界。`store=false` 时请求 `reasoning.encrypted_content`，以保留无服务端状态会话的续用数据。空 `response.output_text.delta` 作为无输出帧忽略；缺少必填字段的非空 delta 仍报错。

`thinking` 继续提供展示文本，opaque 元数据不转成助手正文或工具输出。Anchor 的 Bridge、会话事实和协议边界不保存或重写这些 reasoning 数据。

同一次 Responses 输出被 Goose 保存为多组 call/result 时，请求投影按原始 response ID 把这些 call 放在对应 result 之前。完整 reasoning 仍只回传一次，真实用户消息和不同 response 是分组边界；原生 Session 文件不改写。此修复针对服务把交错的第二个 call 误判成缺少 reasoning 的新回复这一已复现问题。

本补丁面向同一 Provider 与模型的原生历史续用。它不声明 opaque 或加密 reasoning 在不同服务、模型、账号之间可移植，也不引入跨 Provider 的兼容策略。更换模型配置后历史中的 Provider 私有数据兼容性需要独立验收。

在上述精确版本的解包源码根目录应用和验证：

```sh
patch -p1 < /path/to/goose-1.53.0-responses-reasoning.patch
cargo test -p goose-provider-types --lib reasoning_roundtrip --locked
cargo test -p goose-provider-types --lib --locked
cargo clippy -p goose-provider-types --all-targets --locked -- -D warnings
```

更新 Goose 时应优先核对上游 Responses reasoning item 的解析、原生消息持久化与回传能力，避免继续叠加相同补丁。构建和二进制校验仍由仓库的固定 Goose 构建流程负责。

## 外部完成工具

`goose-1.53.0-external-completion.patch` 在上述 reasoning 补丁之后应用。仅显式设置 `GOOSE_EXTERNAL_COMPLETION_TOOL` 的 AgentNode 启用；Anchor 指定 `anchor__final_result`，Pilot 不启用。

同次模型响应的工具在流结束后统一分批，正文与 thinking 继续流式显示，原生消息、推理元数据和截断标记保留。批次 ID/大小由执行器生成，经私有调用上下文传入 MCP，覆盖同名模型元数据；Host 不依赖代理计数或通知到达顺序。混批中的完成工具被拒绝，其余工具仍经过原授权路径。

唯一完成工具成功后，Goose 先保存原生请求/结果，再结束本轮，不追加模型请求、不用取消冒充完成；原生保存错误继续通过 ACP 报错，用户取消仍返回取消。Host 仍独立校验结果和取消，并等待清理成功才持久化完成。未启用时保留原执行方式；实验性 state machine 与此选项组合明确拒绝。

模型仅以普通正文收尾时，沿用原生 `final_output` 的接续方式：保存草稿并追加仅模型可见的提醒，同一个 prompt 最多两次。不会重启工具执行、换 Session 或将提醒计入新的用户请求；调用仍受原有轮数及取消边界约束。Provider 错误、模型输出截断和缺失工具响应不走该分支。提醒耗尽后，原生历史保存成功才由 ACP 响应的 `_meta.anchorExternalCompletion=not_submitted` 报告；Host 另核查持久工具观察和清理结果后结算，不匹配助手正文。

该补丁不改变原生会话 schema。新 invocation 可沿明确审查的单向 digest 对加载旧历史；已有 invocation 的执行器绑定仍严格保持。更新上游时应核查其外部完成工具、批次上下文与历史保存边界，避免重复维护同一机制。

## Provider 错误

`goose-1.53.0-provider-errors.patch` 在前述两份补丁后应用，复用 Goose 已有的 `MessageContent::Error` 和 ACP JSON-RPC 错误。普通请求错误、网络错误、模型拒答、上下文压缩后仍超窗及压缩摘要失败先保存原生、仅用户可见的 Error 消息，再交给 ACP 报错；成功恢复的超窗压缩仍沿正常执行路径继续。认证和额度不足保留原生专用错误代码或原因。助手普通文本不参与错误识别。

ACP 为通用结构化 Error 返回 `data.reason=provider_error`。Anchor 只在当前 `session/prompt` 的响应 ID、错误代码与消息形状通过协议校验后读取固定错误类别，不回显服务端错误正文；Pilot 把明确的模型请求失败保存为 Turn `failed`，断连、进程与清理失败保留 `interrupted`，用户取消继续优先结算为 `stopped`。新的原生历史沿已审查的单向摘要对接续，当前 invocation 的二进制绑定不放宽。

该补丁沿用现有原生消息 schema，不增加 Anchor Agent loop 或恢复游标。固定二进制构建与单元检查已有记录；实际 Host/Goose 错误场景及真实 Provider 的新增验收仍须以[本轮验证边界](../../.local/closeout-20261018/README.md)为准。
