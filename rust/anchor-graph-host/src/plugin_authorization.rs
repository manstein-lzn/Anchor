//! Secret-free Host authority identity, independent of code and credential values.

use crate::dependencies::VIEW_TOKEN;
use crate::mcp::{McpServerDefinition, host_environment_references};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Persist the Host grants a declaration can request, not its code/configuration
/// revision or resolved credential values. Changes to dependencies, local
/// entrypoints and business arguments stay within the same sandbox authority.
pub(crate) fn plugin_authorization_digest(
    servers: &[McpServerDefinition],
    network: bool,
    mutable: &[String],
) -> String {
    let declarations = serde_json::json!({
        "format": 1,
        "mcp_servers": servers.iter().map(|server| {
            let config = &server.config;
            let command = config.get("command").and_then(Value::as_str);
            let entrypoint = command.map(|command| {
                if (command.contains('/') && !Path::new(command).is_absolute())
                    || ["PLUGIN_ROOT", "CODEX_PLUGIN_ROOT", "CLAUDE_PLUGIN_ROOT", VIEW_TOKEN]
                        .iter().any(|key| command.starts_with(&format!("${{{key}}}/")))
                {
                    "<plugin-entrypoint>"
                } else {
                    command
                }
            });
            let references = host_environment_references(config);
            let mut grant = serde_json::json!({
                "name": server.name,
                "transport": config.get("type").and_then(Value::as_str)
                    .unwrap_or(if command.is_some() { "stdio" } else { "http" }),
                "host_entrypoint": entrypoint,
                "environment_references": references,
                "env_vars": config.get("env_vars"),
                "optional_env_vars": config.get("optional_env_vars"),
                "env_http_headers": config.get("env_http_headers"),
                "bearer_token_env_var": config.get("bearer_token_env_var"),
                "url": config.get("url"),
                "auth": config.get("auth"),
                "oauth_resource": config.get("oauth_resource"),
                "oauth_client_id": config.get("oauth_client_id"),
                "oauth_scope": config.get("oauth_scope"),
            });
            // Disclosing the channel sender is a Host grant. Only declarations
            // that request it change; every existing authority stays identical.
            if let Some(headers) = config.get("channel_identity_headers") {
                grant["channel_identity_headers"] = headers.clone();
            }
            grant
        }).collect::<Vec<_>>(),
        "network": network,
        "mutable": mutable,
    });
    let bytes = serde_json::to_vec(&declarations)
        .expect("Plugin authorization declaration serialization is infallible");
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
#[path = "plugin_authorization_tests.rs"]
mod tests;
