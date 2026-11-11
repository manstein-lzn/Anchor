use super::{bridge::Bridge, configuration, transport::AcpConnection};
use anchor_runtime::Cancellation;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

const SYSTEM_PROMPT: &str = r#"Use Markdown formatting for responses.
The latest <turn-context> block contains current operational facts, not a user request. Earlier blocks are historical.
{% if include_extensions and not code_execution_mode %}
{% for extension in extensions %}
{% if extension.instructions %}
{{ extension.instructions }}
{% endif %}
{% endfor %}
{% endif %}"#;

pub(super) struct OpenedSession {
    pub(super) id: String,
    pub(super) initialize: Value,
    pub(super) response: Value,
    pub(super) history: Vec<Value>,
}

/// Optional ACP client capabilities this Host declares to the agent.
pub(super) struct ClientCapabilities {
    pub custom_notifications: bool,
    pub form_elicitation: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn open(
    connection: &mut AcpConnection,
    bridge: &Bridge,
    // Base URL the sandbox uses for the bridge (its own loopback when isolated).
    endpoint: &str,
    restored: Option<&str>,
    instructions: &str,
    binding: &configuration::ModelBinding,
    cancellation: &Cancellation,
    deadline: Option<Instant>,
    capabilities: ClientCapabilities,
) -> Result<OpenedSession, String> {
    let handshake_deadline = deadline.unwrap_or_else(|| Instant::now() + Duration::from_secs(30));
    let mut declared =
        json!({"_meta":{"goose":{"customNotifications":capabilities.custom_notifications}}});
    if capabilities.form_elicitation {
        declared["elicitation"] = json!({"form":{}});
    }
    let (initialize, _) = connection
        .request(
            "initialize",
            json!({"protocolVersion":1,
            "clientCapabilities":declared,
            "clientInfo":{"name":"anchor","version":"0.1.0"}}),
            cancellation,
            handshake_deadline,
        )
        .await?;
    if initialize["protocolVersion"] != 1
        || initialize["agentCapabilities"]["mcpCapabilities"]["http"] != true
        || initialize["agentInfo"]["name"] != "goose"
        || initialize["agentInfo"]["version"] != "1.53.0"
    {
        return Err("Goose did not negotiate ACP v1 and HTTP MCP".into());
    }
    let server = json!({"type":"http","name":"anchor", "url":format!("{endpoint}/mcp"),
        "headers":[{"name":"Authorization","value":format!("Bearer {}", bridge.token)}]});
    // Goose 1.53 adds its default builtins when creating a session and retains
    // extensions when loading one; enabledExtensions alone is not an allowlist.
    // Reconcile through its native extension API before any model prompt. `tom`
    // supplies the persistent boundary text; other builtins are deployment opt-ins.
    let mut enabled = vec![json!({"type":"mcp","server":server})];
    for name in configuration::enabled_builtins()? {
        enabled.push(json!({"type":"builtin","name":name}));
    }
    let mut params = json!({"cwd":"/workspace","mcpServers":[server.clone()],
        "_meta":{"hidden":true,"sessionTitle":"Anchor","enabledExtensions":enabled.clone()}});
    if let Some(id) = restored {
        params["sessionId"] = json!(id);
    }
    let (response, history) = connection
        .request_with_deadline(
            if restored.is_some() {
                "session/load"
            } else {
                "session/new"
            },
            params,
            cancellation,
            Some(handshake_deadline),
        )
        .await?;
    let id = restored
        .or_else(|| response["sessionId"].as_str())
        .filter(|id| !id.is_empty())
        .ok_or("Goose returned no sessionId")?
        .to_owned();
    let (listed, _) = connection
        .request(
            "_goose/unstable/session/extensions/list",
            json!({"sessionId":id}),
            cancellation,
            handshake_deadline,
        )
        .await?;
    let extensions = listed["extensions"]
        .as_array()
        .ok_or("Goose returned no session extension list")?;
    let mut retained = vec![false; enabled.len()];
    for entry in extensions {
        if let Some(index) = enabled
            .iter()
            .position(|requested| matches_extension(requested, &entry["extension"]))
        {
            retained[index] = true;
            continue;
        }
        let key = entry["extensionKey"]
            .as_str()
            .filter(|key| !key.is_empty())
            .ok_or("Goose returned an extension without a key")?;
        connection
            .request(
                "_goose/unstable/session/extensions/remove",
                json!({"sessionId":id,"extensionKey":key}),
                cancellation,
                handshake_deadline,
            )
            .await?;
    }
    for (requested, retained) in enabled.iter().zip(retained) {
        if !retained {
            connection
                .request(
                    "_goose/unstable/session/extensions/add",
                    json!({"sessionId":id,"extension":requested}),
                    cancellation,
                    handshake_deadline,
                )
                .await?;
        }
    }
    // Loading restores the native session's saved model, independently of the
    // process defaults. Select this invocation's frozen model through ACP before
    // prompting; Goose persists the selection without replacing its history.
    connection
        .request(
            "session/set_config_option",
            json!({"sessionId":id,"configId":"model","value":binding.model}),
            cancellation,
            handshake_deadline,
        )
        .await?;
    // Replace the runtime's default identity on both new and loaded sessions.
    // Append the role as literal text, never as part of the rendered template.
    for update in [
        json!({"sessionId":id,"mode":"set","text":SYSTEM_PROMPT}),
        json!({"sessionId":id,"mode":"append","key":"anchor_role","text":instructions}),
    ] {
        connection
            .request(
                "_goose/unstable/session/system-prompt/set",
                update,
                cancellation,
                handshake_deadline,
            )
            .await?;
    }
    Ok(OpenedSession {
        id,
        initialize,
        response,
        history,
    })
}

fn matches_extension(requested: &Value, actual: &Value) -> bool {
    match requested["type"].as_str() {
        Some("builtin") => {
            matches!(actual["type"].as_str(), Some("builtin" | "platform"))
                && actual["name"] == requested["name"]
        }
        Some("mcp") => actual["type"] == "mcp" && actual["server"] == requested["server"],
        _ => false,
    }
}
