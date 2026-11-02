//! MCP declaration merge, validation and scoped environment expansion.

use crate::dependencies::{PluginDependencies, VIEW_TOKEN};
use crate::plugin_configuration::{PluginConfiguration, http_url_is_valid};
use crate::resources::{
    inside_file, is_executable, normalize_inside, read_object, reject_symlink_components,
    valid_reference,
};
use crate::{dependencies, invalid};
use anchor_runtime::graph::GraphError;
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};

/// One validated MCP server declaration from `.mcp.json` or `plugin.json`.
/// The config is kept as JSON because the MCP adapter owns transport-specific
/// details and credentials; the catalog owns validation and expansion only.
#[derive(Clone, PartialEq)]
pub struct McpServerDefinition {
    pub name: String,
    pub config: Value,
}

impl std::fmt::Debug for McpServerDefinition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerDefinition")
            .field("name", &self.name)
            .field("config", &"<redacted>")
            .finish()
    }
}

pub(crate) fn servers_from_dir(
    id: &str,
    root: &Path,
    dir: &Path,
    resolve_env: bool,
) -> Result<Vec<McpServerDefinition>, GraphError> {
    let manifest_path = dir.join("plugin.json");
    reject_symlink_components(dir, &manifest_path)?;
    let manifest = read_object(&manifest_path)?;
    // A manifest field this catalog does not act on is refused here as well
    // as in `definition`, so no caller can read a declaration that
    // `definition` would have rejected.
    dependencies::reject_unknown_fields(id, &manifest)?;
    let dependencies = dependencies::parse(id, &manifest)?;
    dependencies::parse_network(id, &manifest)?;
    dependencies::parse_mutable(id, &manifest)?;
    servers_from_manifest(id, root, dir, &manifest, &dependencies, resolve_env)
}

pub(crate) fn servers_from_manifest(
    id: &str,
    root: &Path,
    dir: &Path,
    manifest: &serde_json::Map<String, Value>,
    dependencies: &PluginDependencies,
    resolve_env: bool,
) -> Result<Vec<McpServerDefinition>, GraphError> {
    let mut servers = serde_json::Map::new();
    let dot_mcp = dir.join(".mcp.json");
    if dot_mcp.exists() {
        reject_symlink_components(dir, &dot_mcp)?;
        let config = read_object(&dot_mcp)?;
        collect_server_values(config.get("mcpServers"), &mut servers, id)?;
    }
    match manifest.get("mcpServers") {
        None => {}
        Some(Value::String(relative)) => {
            let config_path = inside_file(dir, relative)
                .map_err(|_| invalid(format!("Plugin {id}: unsafe MCP config path")))?;
            reject_symlink_components(dir, &config_path)?;
            let config = read_object(&config_path)?;
            collect_server_values(config.get("mcpServers"), &mut servers, id)?;
        }
        Some(value) => collect_server_values(Some(value), &mut servers, id)?,
    }
    let mut result = Vec::new();
    for (name, value) in servers {
        // `${ANCHOR_PLUGIN_VIEW}` names the view the Plugin's own declaration
        // produces. A Plugin that uses it without declaring anything has
        // nothing to point at, so the declaration is refused here rather
        // than launched on the deployment's interpreter by accident.
        if dependencies.is_empty() && mentions_view_token(&value) {
            return Err(invalid(format!(
                "Plugin {id}: server {name}: ${{{VIEW_TOKEN}}} needs a `dependencies.python` declaration"
            )));
        }
        let Some(config) = mcp_server(
            id,
            &name,
            value,
            dir,
            false,
            root.parent().unwrap_or(root),
            &|key| std::env::var(key).ok(),
        )?
        else {
            continue;
        };
        result.push(McpServerDefinition { name, config });
    }
    // Validate mappings against the declarations before template expansion
    // removes references, on every catalog read path.
    PluginConfiguration::parse(id, manifest, &result)?;
    if !resolve_env {
        return Ok(result);
    }
    result
        .into_iter()
        .filter_map(|server| {
            match mcp_server(
                id,
                &server.name,
                server.config,
                dir,
                true,
                root.parent().unwrap_or(root),
                &|key| std::env::var(key).ok(),
            ) {
                Ok(Some(config)) => Some(Ok(McpServerDefinition {
                    name: server.name,
                    config,
                })),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            }
        })
        .collect()
}
/// The non-path Host environment keys used by `${NAME}` / `${NAME:-default}`
/// templates anywhere in a MCP configuration. This is the same projection that
/// binds Plugin authorization; callers can reject Host access before expansion.
pub fn host_environment_references(value: &Value) -> BTreeSet<String> {
    let mut references = BTreeSet::new();
    environment_references(value, &mut references);
    references
}

/// The same `${NAME}` / `${NAME:-default}` syntax the catalog expands. Names
/// matter for Host access; values and fallback literals do not become Run facts.
fn environment_references(value: &Value, references: &mut BTreeSet<String>) {
    match value {
        Value::String(text) => {
            for part in text.split("${").skip(1) {
                let Some((expression, _)) = part.split_once('}') else {
                    continue;
                };
                let key = expression
                    .split_once(":-")
                    .map_or(expression, |(key, _)| key);
                if valid_environment_key(key) && !host_path_environment_key(key) {
                    references.insert(key.to_owned());
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                environment_references(value, references);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                environment_references(value, references);
            }
        }
        _ => {}
    }
}

fn collect_server_values(
    value: Option<&Value>,
    servers: &mut serde_json::Map<String, Value>,
    id: &str,
) -> Result<(), GraphError> {
    let Some(value) = value else { return Ok(()) };
    let object = value
        .as_object()
        .ok_or_else(|| invalid(format!("Plugin {id}: mcpServers must be an object")))?;
    for (name, server) in object {
        if !valid_reference(name) || !server.is_object() {
            return Err(invalid(format!(
                "Plugin {id}: invalid MCP server declaration"
            )));
        }
        servers.insert(name.clone(), server.clone());
    }
    Ok(())
}

/// These names are path facts supplied by the Host, never user credentials or
/// configurable environment aliases. Keep reference discovery and configuration
/// binding validation on the same reserved-name rule.
pub(crate) fn host_path_environment_key(value: &str) -> bool {
    [
        "PLUGIN_ROOT",
        "CODEX_PLUGIN_ROOT",
        "CLAUDE_PLUGIN_ROOT",
        VIEW_TOKEN,
    ]
    .contains(&value)
}

pub(crate) fn valid_environment_key(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn scoped_environment(
    plugin: &str,
    key: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, GraphError> {
    if !valid_environment_key(key) {
        return Err(invalid(format!(
            "Plugin {plugin}: invalid MCP environment variable name"
        )));
    }
    lookup(key).ok_or_else(|| {
        invalid(format!(
            "Plugin {plugin}: MCP environment variable {key} is not set"
        ))
    })
}

fn http_url_declaration_is_valid(url: &str) -> bool {
    if !url.contains("${") {
        return http_url_is_valid(url);
    }
    let mut projected = String::new();
    let mut remaining = url;
    while let Some((before, rest)) = remaining.split_once("${") {
        projected.push_str(before);
        let Some((expression, after)) = rest.split_once('}') else {
            return false;
        };
        let key = expression
            .split_once(":-")
            .map_or(expression, |(key, _)| key);
        if !valid_environment_key(key) {
            return false;
        }
        // Placeholders can denote a full URL, scheme, host or port. Pick
        // syntactically valid representatives only for declaration validation;
        // the resolved endpoint is independently validated before transport.
        let in_authority = projected
            .split_once("://")
            .is_some_and(|(_, rest)| !rest.contains(['/', '?', '#']));
        let placeholder = if projected.is_empty() {
            if after.starts_with("://") {
                "https"
            } else {
                "https://configuration.invalid"
            }
        } else if in_authority && projected.ends_with('[') {
            "::1"
        } else if in_authority && projected.ends_with(':') && !projected.ends_with("://") {
            "443"
        } else {
            "configuration.invalid"
        };
        projected.push_str(placeholder);
        remaining = after;
    }
    projected.push_str(remaining);
    http_url_is_valid(&projected)
}

fn http_headers_are_valid(headers: &serde_json::Map<String, Value>, selectors: bool) -> bool {
    headers.iter().all(|(name, value)| {
        http::HeaderName::from_bytes(name.as_bytes()).is_ok()
            && (selectors
                || http::HeaderValue::from_str(value.as_str().unwrap_or_default()).is_ok())
    })
}

fn validate_mcp(
    id: &str,
    name: &str,
    server: &serde_json::Map<String, Value>,
) -> Result<(), GraphError> {
    let stdio = server.contains_key("command");
    let allowed: &[&str] = if stdio {
        &[
            "type",
            "enabled",
            "startup_timeout_sec",
            "tool_timeout_sec",
            "command",
            "args",
            "env",
            "env_vars",
            "optional_env_vars",
            "cwd",
            // Anchor's own run-time declaration for this server. It is not part of
            // the MCP protocol and never reaches the transport; it lives here so
            // the binding sits next to the server it applies to and is covered by
            // the Plugin digest like every other declared field.
            "anchor",
        ]
    } else {
        &[
            "type",
            "enabled",
            "startup_timeout_sec",
            "tool_timeout_sec",
            "url",
            "headers",
            "http_headers",
            "env_http_headers",
            "bearer_token_env_var",
            "auth",
            "oauth_resource",
            "oauth_client_id",
            "oauth_scope",
            "channel_identity_headers",
        ]
    };
    if let Some(key) = server.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(invalid(format!(
            "Plugin {id}, server {name}: unsupported MCP field: {key}"
        )));
    }
    if let Some(enabled) = server.get("enabled")
        && !enabled.is_boolean()
    {
        return Err(invalid(format!(
            "Plugin {id}, server {name}: enabled must be boolean"
        )));
    }
    if let Some(anchor) = server.get("anchor") {
        let anchor = anchor.as_object().ok_or_else(|| {
            invalid(format!(
                "Plugin {id}, server {name}: anchor must be an object"
            ))
        })?;
        if anchor.contains_key("environment") {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: `anchor.environment` no longer selects a managed environment; declare the package in this Plugin's `plugin.json` under `dependencies` instead"
            )));
        }
        if let Some(key) = anchor.keys().next() {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: unsupported anchor field: {key}"
            )));
        }
    }
    let transport = server
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(if stdio { "stdio" } else { "http" });
    if (stdio && transport != "stdio") || (!stdio && !matches!(transport, "http" | "sse")) {
        return Err(invalid(format!(
            "Plugin {id}, server {name}: invalid MCP transport type"
        )));
    }
    for key in [
        "command",
        "cwd",
        "url",
        "bearer_token_env_var",
        "oauth_resource",
        "oauth_client_id",
        "oauth_scope",
    ] {
        if let Some(value) = server.get(key)
            && value
                .as_str()
                .is_none_or(|value| value.is_empty() || value.contains('\0'))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: {key} must be a nonempty string"
            )));
        }
    }
    for key in ["args", "env_vars", "optional_env_vars"] {
        if let Some(value) = server.get(key) {
            let Some(values) = value.as_array() else {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must be a list of strings"
                )));
            };
            if values
                .iter()
                .any(|value| value.as_str().is_none_or(|value| value.contains('\0')))
            {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must be a list of strings"
                )));
            }
        }
    }
    for key in ["env", "headers", "http_headers", "env_http_headers"] {
        if let Some(value) = server.get(key) {
            let Some(values) = value.as_object() else {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must contain string keys and values"
                )));
            };
            if values.iter().any(|(key, value)| {
                key.is_empty()
                    || key.contains('\0')
                    || value.as_str().is_none_or(|value| value.contains('\0'))
            }) {
                return Err(invalid(format!(
                    "Plugin {id}, server {name}: {key} must contain string keys and values"
                )));
            }
        }
    }
    for key in ["env_vars", "optional_env_vars"] {
        if server
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .any(|name| !valid_environment_key(name))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: {key} contains an invalid environment variable name"
            )));
        }
    }
    if server
        .get("env")
        .and_then(Value::as_object)
        .is_some_and(|values| values.keys().any(|key| !valid_environment_key(key)))
        || server
            .get("bearer_token_env_var")
            .and_then(Value::as_str)
            .is_some_and(|key| !valid_environment_key(key))
        || server
            .get("env_http_headers")
            .and_then(Value::as_object)
            .is_some_and(|values| {
                values
                    .values()
                    .filter_map(Value::as_str)
                    .any(|key| !valid_environment_key(key))
            })
    {
        return Err(invalid(format!(
            "Plugin {id}, server {name}: invalid MCP environment variable name"
        )));
    }
    for key in ["headers", "http_headers", "env_http_headers"] {
        if server
            .get(key)
            .and_then(Value::as_object)
            .is_some_and(|values| !http_headers_are_valid(values, key == "env_http_headers"))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: invalid MCP HTTP header"
            )));
        }
    }
    for key in ["startup_timeout_sec", "tool_timeout_sec"] {
        if let Some(value) = server.get(key)
            && !value
                .as_f64()
                .is_some_and(|value| value > 0.0 && value <= 86_400.0)
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: {key} must be positive seconds, at most 86400"
            )));
        }
    }
    if !stdio {
        let url = server
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !http_url_declaration_is_valid(url) {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: url must be HTTP(S), without embedded credentials or fragment"
            )));
        }
        if server
            .get("auth")
            .is_some_and(|value| value.as_str() != Some("oauth"))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: auth supports only oauth; use headers or bearer_token_env_var for tokens"
            )));
        }
        let oauth = server.get("auth").and_then(Value::as_str) == Some("oauth")
            || server.get("oauth_resource").is_some();
        if !oauth && (server.contains_key("oauth_client_id") || server.contains_key("oauth_scope"))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: oauth_client_id and oauth_scope require OAuth auth"
            )));
        }
        if oauth
            && (server.contains_key("bearer_token_env_var")
                || ["headers", "http_headers", "env_http_headers"]
                    .iter()
                    .any(|key| {
                        server
                            .get(*key)
                            .and_then(Value::as_object)
                            .is_some_and(|values| {
                                values
                                    .keys()
                                    .any(|name| name.eq_ignore_ascii_case("authorization"))
                            })
                    }))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: OAuth cannot be combined with static Authorization"
            )));
        }
        if server.contains_key("headers") && server.contains_key("http_headers") {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: choose headers or http_headers, not both"
            )));
        }
        if server.contains_key("channel_identity_headers") {
            validate_channel_identity_headers(server).map_err(|reason| {
                invalid(format!(
                    "Plugin {id}, server {name}: channel_identity_headers {reason}"
                ))
            })?;
        }
    }
    Ok(())
}

/// Channel facts the Host may attest to an HTTP MCP server for the message it
/// is answering. The values come only from the Host's admitted channel input.
const CHANNEL_IDENTITY_FIELDS: &[&str] = &[
    "source",
    "account",
    "conversation_kind",
    "conversation_id",
    "sender_id",
];

/// The declared `(header, channel field)` pairs of one MCP server.
pub fn channel_identity_headers(config: &Value) -> Vec<(String, String)> {
    config
        .get("channel_identity_headers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(header, field)| Some((header.clone(), field.as_str()?.to_owned())))
        .collect()
}

fn validate_channel_identity_headers(
    server: &serde_json::Map<String, Value>,
) -> Result<(), &'static str> {
    let declared = server
        .get("channel_identity_headers")
        .and_then(Value::as_object)
        .filter(|declared| !declared.is_empty())
        .ok_or("must map at least one header name to a channel identity field")?;
    // Static configuration can never supply or shadow a Host-attested value.
    let configured = ["headers", "http_headers", "env_http_headers"]
        .iter()
        .filter_map(|key| server.get(*key).and_then(Value::as_object))
        .flat_map(|values| values.keys())
        .map(|name| name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    for (header, field) in declared {
        let lower = header.to_ascii_lowercase();
        if http::HeaderName::from_bytes(header.as_bytes()).is_err()
            || lower == "authorization"
            || !seen.insert(lower.clone())
        {
            return Err("contains an invalid, duplicate or authorization header name");
        }
        if configured.contains(&lower) {
            return Err("cannot reuse a header that configuration already supplies");
        }
        if field
            .as_str()
            .is_none_or(|field| !CHANNEL_IDENTITY_FIELDS.contains(&field))
        {
            return Err(
                "values must be channel identity fields: source, account, conversation_kind, conversation_id or sender_id",
            );
        }
    }
    Ok(())
}

/// Whether a server declaration names its own dependency view.
pub(crate) fn mentions_view_token(value: &Value) -> bool {
    match value {
        Value::String(text) => text.contains(&format!("${{{VIEW_TOKEN}}}")),
        Value::Array(items) => items.iter().any(mentions_view_token),
        Value::Object(items) => items.values().any(mentions_view_token),
        _ => false,
    }
}

fn expand_env(
    value: Value,
    plugin_dir: &Path,
    plugin_id: &str,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<Value, GraphError> {
    match value {
        Value::String(text) => {
            let mut output = String::with_capacity(text.len());
            let bytes = text.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                if bytes[index] == b'$' && bytes.get(index + 1) == Some(&b'{') {
                    let Some(end) = text[index + 2..].find('}') else {
                        output.push(bytes[index] as char);
                        index += 1;
                        continue;
                    };
                    let end = index + 2 + end;
                    let token = &text[index + 2..end];
                    let (key, default) = token
                        .split_once(":-")
                        .map(|(key, default)| (key, Some(default)))
                        .unwrap_or((token, None));
                    if !valid_environment_key(key) {
                        output.push_str(&text[index..=end]);
                        index = end + 1;
                        continue;
                    }
                    // The dependency view is a deployment path, not a path inside
                    // the Plugin: only the host that grants the view's read-only
                    // authority may name it, so the declaration is kept as
                    // written and substituted at launch.
                    if key == VIEW_TOKEN {
                        output.push_str(&text[index..=end]);
                        index = end + 1;
                        continue;
                    }
                    let replacement = match key {
                        "CODEX_PLUGIN_ROOT" | "CLAUDE_PLUGIN_ROOT" | "PLUGIN_ROOT" => {
                            plugin_dir.to_string_lossy().into_owned()
                        }
                        _ => match environment(key) {
                            Some(value) if !value.is_empty() => value,
                            Some(value) => default.unwrap_or(&value).to_owned(),
                            None => default.map(str::to_owned).ok_or_else(|| {
                                invalid(format!(
                                    "Plugin {plugin_id}: MCP environment variable {key} is not set"
                                ))
                            })?,
                        },
                    };
                    output.push_str(&replacement);
                    index = end + 1;
                } else {
                    let character = text[index..].chars().next().expect("index in string");
                    output.push(character);
                    index += character.len_utf8();
                }
            }
            Ok(Value::String(output))
        }
        Value::Array(items) => items
            .into_iter()
            .map(|item| expand_env(item, plugin_dir, plugin_id, environment))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        Value::Object(items) => items
            .into_iter()
            .map(|(key, item)| Ok((key, expand_env(item, plugin_dir, plugin_id, environment)?)))
            .collect::<Result<serde_json::Map<_, _>, GraphError>>()
            .map(Value::Object),
        other => Ok(other),
    }
}

pub(crate) fn mcp_server(
    id: &str,
    name: &str,
    value: Value,
    plugin_dir: &Path,
    resolve_env: bool,
    data_root: &Path,
    environment_lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<Value>, GraphError> {
    let mut server = value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(format!("Plugin {id}: MCP server {name} must be an object")))?;
    validate_mcp(id, name, &server)?;
    if server.get("enabled") == Some(&Value::Bool(false)) {
        return Ok(None);
    }

    if resolve_env {
        server = expand_env(Value::Object(server), plugin_dir, id, environment_lookup)?
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("expanded MCP server is not an object".into()))?;

        // Endpoint templates are only declarations until this boundary. Check
        // the actual destination before handing any credentials to transport.
        if !server.contains_key("command")
            && !server
                .get("url")
                .and_then(Value::as_str)
                .is_some_and(http_url_is_valid)
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: resolved url must be HTTP(S), without embedded credentials or fragment"
            )));
        }

        // Configuration can introduce invalid process arguments or header
        // values even when the original declaration is valid. Validate again
        // without including expanded data in any diagnostic.
        validate_mcp(id, name, &server)?;

        if let Some(command) = server.get("command").and_then(Value::as_str) {
            let executable = Path::new(command);
            // External interpreters are deployment resources. Preserve their
            // declaration here; the host sandbox must authorize the exact
            // configured tool environment before launching a process.
            if executable.is_absolute()
                && executable.starts_with(plugin_dir)
                && (!executable.is_file() || !is_executable(executable))
            {
                return Err(invalid(format!(
                    "Plugin {id}: MCP command is not an executable Plugin file"
                )));
            }
            if let Some(Value::Array(values)) = server.get("optional_env_vars")
                && values.iter().filter_map(Value::as_str).any(|name| {
                    environment_lookup(name).is_none_or(|value| value.trim().is_empty())
                })
            {
                return Ok(None);
            }
        }
    }

    if let Some(Value::String(cwd)) = server.get("cwd").cloned()
        && !Path::new(&cwd).is_absolute()
    {
        let path = normalize_inside(plugin_dir, &cwd)
            .ok_or_else(|| invalid(format!("Plugin {id}: MCP cwd escapes the Plugin bundle")))?;
        let path = if path.exists() {
            let resolved = path.canonicalize()?;
            if !resolved.starts_with(plugin_dir) {
                return Err(invalid(format!(
                    "Plugin {id}: MCP cwd escapes the Plugin bundle"
                )));
            }
            resolved
        } else {
            path
        };
        if resolve_env {
            server.insert(
                "cwd".into(),
                Value::String(path.to_string_lossy().into_owned()),
            );
        }
    }

    if resolve_env {
        let stdio = server.contains_key("command");
        if stdio {
            let mut env = server
                .get("env")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let names = server
                .get("env_vars")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .chain(
                    server
                        .get("optional_env_vars")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten(),
                )
                .filter_map(Value::as_str);
            for key in names {
                let value = scoped_environment(id, key, environment_lookup)?;
                env.insert(key.into(), Value::String(value));
            }
            server.insert("env".into(), Value::Object(env));
        } else {
            let mut headers = server
                .get("http_headers")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if let Some(explicit) = server.get("headers").and_then(Value::as_object) {
                headers.extend(explicit.clone());
            }
            if let Some(values) = server.get("env_http_headers").and_then(Value::as_object) {
                for (header, variable) in values {
                    let variable = variable.as_str().ok_or_else(|| {
                        invalid(format!(
                            "Plugin {id}: invalid MCP environment variable name"
                        ))
                    })?;
                    headers.insert(
                        header.clone(),
                        Value::String(scoped_environment(id, variable, environment_lookup)?),
                    );
                }
            }
            if let Some(variable) = server.get("bearer_token_env_var").and_then(Value::as_str) {
                if headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case("authorization"))
                {
                    return Err(invalid(format!(
                        "Plugin {id}: duplicate MCP authorization configuration"
                    )));
                }
                let token = scoped_environment(id, variable, environment_lookup)?;
                headers.insert(
                    "Authorization".into(),
                    Value::String(format!("Bearer {token}")),
                );
            }
            server.insert("headers".into(), Value::Object(headers));
        }
        // Selectors have now become actual values. Validate those credentials
        // before passing them to transport; diagnostics never contain values.
        if server
            .get("headers")
            .and_then(Value::as_object)
            .is_some_and(|headers| !http_headers_are_valid(headers, false))
        {
            return Err(invalid(format!(
                "Plugin {id}, server {name}: invalid MCP HTTP header"
            )));
        }
        server.insert("_anchor_plugin_id".into(), Value::String(id.into()));
        server.insert(
            "_anchor_plugin_dir".into(),
            Value::String(plugin_dir.to_string_lossy().into_owned()),
        );
        server.insert("_anchor_server_name".into(), Value::String(name.into()));
        server.insert(
            "_anchor_auth_dir".into(),
            Value::String(
                data_root
                    .join("state")
                    .join("mcp-auth")
                    .join(id)
                    .join(name)
                    .to_string_lossy()
                    .into_owned(),
            ),
        );
    }
    Ok(Some(Value::Object(server)))
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
