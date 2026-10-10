use super::{bridge::Bridge, transport::AcpConnection};
use anchor_runtime::Cancellation;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::Instant;

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

pub(super) async fn open(
    connection: &mut AcpConnection,
    bridge: &Bridge,
    // Base URL the sandbox uses for the bridge (its own loopback when isolated).
    endpoint: &str,
    restored: Option<&str>,
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
    // `enabledExtensions` restricts the session to exactly these extensions, so the
    // builtin that injects the per-turn persistent instructions (`tom`, "Top Of
    // Mind") must be listed explicitly; otherwise the host boundary never reaches
    // the model even though `GOOSE_MOIM_MESSAGE_TEXT` is set.
    let mut params = json!({"cwd":"/workspace","mcpServers":[server.clone()],
        "_meta":{"hidden":true,"sessionTitle":"Anchor",
            "enabledExtensions":[{"type":"mcp","server":server},
                {"type":"builtin","name":"tom"}]}});
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
    Ok(OpenedSession {
        id,
        initialize,
        response,
        history,
    })
}
