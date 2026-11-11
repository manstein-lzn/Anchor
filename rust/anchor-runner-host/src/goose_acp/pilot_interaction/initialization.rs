use super::*;
use crate::plugin_library::Catalog;

pub(super) async fn policy(
    tools: &PilotTools,
    name: &str,
    arguments: &serde_json::Map<String, Value>,
    peer: Peer<RoleServer>,
    cancellation: &Cancellation,
) -> Result<Vec<ToolResultContent>, ToolError> {
    let (plugin, on) = if name == "plugin_mount" {
        if arguments.len() != 2 {
            return Err(failure("plugin_mount accepts plugin and on only"));
        }
        let plugin = arguments
            .get("plugin")
            .and_then(Value::as_str)
            .filter(|plugin| anchor_runtime::identifier::is_plugin_id(plugin))
            .ok_or_else(|| failure("plugin must be an installed Plugin identifier"))?;
        let on = arguments
            .get("on")
            .and_then(Value::as_bool)
            .ok_or_else(|| failure("on must be a boolean"))?;
        (plugin, on)
    } else {
        if arguments.len() != 1 {
            return Err(failure("session_network accepts allowed only"));
        }
        let allowed = arguments
            .get("allowed")
            .and_then(Value::as_bool)
            .ok_or_else(|| failure("allowed must be a boolean"))?;
        (crate::plugin_network::NETWORK_PLUGIN, allowed)
    };
    let definition = Catalog::new(&tools.library_root)
        .definition(plugin)
        .map_err(|_| failure("the Plugin is not installed; mounting cannot install it"))?;
    if name == "session_network" && on && !definition.network {
        return Err(failure("the installed network Plugin is invalid"));
    }
    let expected = definition.binding();
    let message = if name == "session_network" {
        format!(
            "{}当前会话的任务联网？此轮权限保持冻结，确认后 Anchor 将在本轮结束时自动继续同一会话。",
            if on { "允许" } else { "禁止" }
        )
    } else {
        format!(
            "在当前会话{} Plugin「{plugin}」？本轮工具保持冻结，确认后 Anchor 将在本轮结束时自动继续同一会话。",
            if on { "启用" } else { "停用" }
        )
    };
    let requested_schema = native_schema(
        json!({"type":"object","properties":{"confirm":{"type":"boolean","title":"确认此会话授权变更","default":false}},"required":["confirm"],"additionalProperties":false}),
    )?;
    let result = tokio::select! {
        result=peer.create_elicitation(ElicitRequestParams::FormElicitationParams{meta:None,message,requested_schema})=>result.map_err(|_|failure("native authorization did not complete"))?,
        _=async { while !cancellation.load(Ordering::Acquire) {tokio::time::sleep(Duration::from_millis(10)).await;} }=>return Err(failure("Pilot was cancelled while waiting for authorization")),
    };
    active(tools, cancellation)?;
    if result.action != ElicitationAction::Accept
        || result
            .content
            .as_ref()
            .and_then(|content| content.get("confirm"))
            != Some(&Value::Bool(true))
    {
        return Ok(vec![ToolResultContent::json(
            json!({"changed":false,"action":result.action}),
        )]);
    }
    let _catalog = tools.application.graph_catalog_mutation_guard().await;
    let _lease = tools
        .application
        .plugin_reference_lease()
        .await
        .map_err(|error| failure(application_reason(error)))?;
    active(tools, cancellation)?;
    Catalog::new(&tools.library_root)
        .checked_definition(
            plugin,
            crate::plugin_library::ExpectedDefinition::ExactBinding(&expected),
        )
        .map_err(|error| match error {
            crate::plugin_library::DefinitionError::Read(_) => {
                failure("Plugin changed while authorization was pending")
            }
            crate::plugin_library::DefinitionError::Mismatch => failure(
                "Plugin changed while authorization was pending; request a fresh confirmation",
            ),
        })?;
    if on {
        crate::plugin_view::prepare_for_plugin(&tools.library_root, plugin)
            .map_err(|error| failure(error.to_string()))?;
    }
    let session = tools
        .sessions
        .get(&tools.owner, &tools.session)
        .map_err(|error| failure(error.to_string()))?;
    let network = plugin == crate::plugin_network::NETWORK_PLUGIN;
    let unchanged = session.plugin_mounts.get(plugin) == Some(&on)
        && (!network || session.network_allowed == on);
    if unchanged {
        return Ok(vec![ToolResultContent::json(
            json!({"changed":false,"plugin":plugin,"on":on,"effective_next_turn":false}),
        )]);
    }
    if network {
        tools
            .sessions
            .set_network_policy(&tools.owner, &tools.session, on, on, &tools.owner)
            .map_err(|error| failure(error.to_string()))?;
    } else {
        tools
            .sessions
            .set_plugin_mount(
                &tools.owner,
                &tools.session,
                plugin,
                on,
                &tools.owner,
                "Pilot native authorization",
            )
            .map_err(|error| failure(error.to_string()))?;
    }
    tools.request_continuation("Session authorization changed")?;
    Ok(vec![ToolResultContent::json(
        json!({"changed":true,"plugin":plugin,"on":on,"effective_next_turn":true,"continuation_requested":true,"guidance":"Finish this Turn now; Anchor will continue this Session with the newly authorized tools."}),
    )])
}
