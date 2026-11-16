//! Native user approval for channel binding and transport lifecycle. Secret
//! values use credential requests; this form contains only explicit grants.
use super::*;
use crate::channel_platform::ChannelPlatform;

fn conversation_scope(platform: ChannelPlatform, private_only: bool, groups: &[String]) -> String {
    let scope = if private_only {
        "只接收私聊，不接纳群消息进入对话"
    } else {
        match platform {
            ChannelPlatform::Feishu if groups.is_empty() => {
                "群名单为空，不放行任何飞书群；仍按成员授权接收私聊"
            }
            ChannelPlatform::Feishu => {
                "除获准私聊外，仅接收所列群内获准成员真正 @ 本机器人的消息；群内共享对话历史和上下文，回复对群成员可见。群授权不替代成员接收和发送授权；不支持机器人 @ 机器人"
            }
            ChannelPlatform::Wecom => {
                "未限制为私聊，沿用企业微信现有成员授权与会话规则；groups 名单仅适用于飞书"
            }
        }
    };
    format!("群授权 groups [{}]。{scope}。", groups.join(", "))
}

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
            let mut message = format!(
                "配置 Graph {graph} 的机器人：回复节点 {}，渠道 Plugin {}。允许接收用户 [{}]，允许发送用户 [{}]。{}本次配置完整替换成员与群名单。配置不会启用联网或挂载 Plugin；凭据通过专用输入控件提供。",
                plan.work_node,
                plan.plugin,
                members.users.join(", "),
                members.send_users.join(", "),
                conversation_scope(plan.platform, members.private_only, &members.groups)
            );
            if members.cli_enabled {
                message.push_str(" 启用飞书通用应用工具：Agent 可使用此应用已发布的 Bot 权限及资源授权，包括云文档、群管理和任意获准资源的读写；这些业务 API 不受私聊回复名单限制。不会继承个人登录。");
            }
            if !members.peer_bots.is_empty() {
                message.push_str(&format!(" 允许协作 Bot [{}] 在获准群中回复已接收的人类消息并 @ 当前 Bot；同一根消息下每个协作 Bot 对当前 Bot 最多进入一次，不把群成员资格当作授权。", members.peer_bots.join(", ")));
            }
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
                "{} Graph {graph} 的机器人接入，回复节点 {}。允许接收用户 [{}]，允许发送用户 [{}]。{}这只控制 Gateway 接入，现有 Run 的权限保持其准入授权。",
                if enabled { "启动" } else { "停止" },
                state
                    .reply_node
                    .as_deref()
                    .unwrap_or("（Graph 当前不可用）"),
                state.binding.users.join(", "),
                state.binding.send_users.join(", "),
                conversation_scope(
                    state.platform,
                    state.binding.private_only,
                    &state.binding.groups
                )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_scope_discloses_exact_groups_and_shared_history() {
        let message = conversation_scope(
            ChannelPlatform::Feishu,
            false,
            &["oc_team".into(), "oc_other".into()],
        );
        assert!(message.contains("groups [oc_team, oc_other]"));
        assert!(message.contains("获准成员真正 @ 本机器人"));
        assert!(message.contains("共享对话历史和上下文"));
        assert!(message.contains("回复对群成员可见"));
        assert!(message.contains("不替代成员接收和发送授权"));
    }

    #[test]
    fn empty_groups_do_not_claim_feishu_group_access_or_change_wecom_semantics() {
        let empty = conversation_scope(ChannelPlatform::Feishu, false, &[]);
        assert!(empty.contains("groups []"));
        assert!(empty.contains("不放行任何飞书群"));
        let private = conversation_scope(ChannelPlatform::Feishu, true, &[]);
        assert!(private.contains("groups []"));
        assert!(private.contains("只接收私聊"));
        let wecom = conversation_scope(ChannelPlatform::Wecom, false, &[]);
        assert!(wecom.contains("沿用企业微信现有成员授权"));
        assert!(wecom.contains("groups 名单仅适用于飞书"));
    }
}
