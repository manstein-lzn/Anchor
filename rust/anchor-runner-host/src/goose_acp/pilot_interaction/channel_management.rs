//! Native user approval for channel binding and transport lifecycle. Secret
//! values use credential requests; this form contains only explicit grants.
use super::*;

pub(super) async fn policy(
    tools: &PilotTools,
    name: &str,
    arguments: &serde_json::Map<String, Value>,
    peer: Peer<RoleServer>,
    cancellation: &Cancellation,
) -> Result<Vec<ToolResultContent>, ToolError> {
    active(tools, cancellation)?;
    let arguments = Value::Object(arguments.clone());
    let (message, configure, enable) = match name {
        "channel_configure" => {
            let (graph, members) =
                crate::pilot_tools::channel_management::configuration_arguments(&arguments)?;
            // Resolve the bot before the question; the application use case
            // checks the Graph and its channel Plugin again after approval.
            let plan = tools
                .application
                .channel_bot_plan(&graph)
                .map_err(|error| failure(application_reason(error)))?;
            let message = format!(
                "配置 Graph {graph} 的机器人：回复节点 {}，渠道 Plugin {}。允许接收用户 [{}]，允许发送用户 [{}]，{}。配置不会启用联网或挂载 Plugin；凭据通过专用输入控件提供。",
                plan.work_node,
                plan.plugin,
                members.users.join(", "),
                members.send_users.join(", "),
                if members.private_only {
                    "只接收私聊"
                } else {
                    "接收私聊和群聊"
                }
            );
            (message, Some((graph, members)), None)
        }
        "channel_enable" => {
            let (graph, enabled) =
                crate::pilot_tools::channel_management::enable_arguments(&arguments)?;
            let state = tools
                .application
                .channel_snapshot(&graph)
                .map_err(|error| failure(application_reason(error)))?
                .ok_or_else(|| failure("this Graph's bot is not configured"))?;
            if state.owner != tools.owner {
                return Err(failure("this Graph's bot belongs to another owner"));
            }
            let message = format!(
                "{} Graph {graph} 的机器人接入，回复节点 {}。允许接收用户 [{}]，允许发送用户 [{}]。这只控制 Gateway 接入，现有 Run 的权限保持其准入授权。",
                if enabled { "启动" } else { "停止" },
                state
                    .reply_node
                    .as_deref()
                    .unwrap_or("（Graph 当前不可用）"),
                state.binding.users.join(", "),
                state.binding.send_users.join(", ")
            );
            (message, None, Some((graph, enabled, state.binding)))
        }
        _ => return Err(failure("unknown channel confirmation tool")),
    };
    let schema = native_schema(
        json!({"type":"object","properties":{"confirm":{"type":"boolean","title":"确认此渠道配置或接入操作","default":false}},"required":["confirm"],"additionalProperties":false}),
    )?;
    let request = peer.create_elicitation(ElicitRequestParams::FormElicitationParams {
        meta: None,
        message,
        requested_schema: schema,
    });
    tokio::pin!(request);
    let result = loop {
        tokio::select! {
            result = &mut request => break result.map_err(|_| failure("native channel confirmation unavailable"))?,
            _ = tokio::time::sleep(Duration::from_millis(50)) => active(tools, cancellation)?,
        }
    };
    active(tools, cancellation)?;
    if result.action != ElicitationAction::Accept
        || result
            .content
            .as_ref()
            .and_then(|content| content.get("confirm"))
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(vec![ToolResultContent::text(
            json!({"changed":false,"approved":false}).to_string(),
        )]);
    }
    let result = if let Some((graph, members)) = configure {
        tools.configure_channel_approved(&graph, members).await?
    } else if let Some((graph, enabled, expected)) = enable {
        if tools
            .application
            .channel_snapshot(&graph)
            .map_err(|error| failure(application_reason(error)))?
            .is_none_or(|state| state.binding != expected)
        {
            return Err(failure(
                "channel binding changed while confirmation was pending; request a fresh form",
            ));
        }
        tools
            .enable_channel_approved(&graph, enabled, &expected)
            .await?
    } else {
        return Err(failure("channel confirmation has no operation"));
    };
    Ok(vec![ToolResultContent::text(result.to_string())])
}
