use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::PathBuf;

use anchor_runtime::SandboxEnvironment;
use reqwest::Url;
use sha2::{Digest, Sha256};

pub(super) fn binary() -> Result<(std::path::PathBuf, String), String> {
    let binary = std::env::var_os("ANCHOR_GOOSE_BINARY")
        .map(std::path::PathBuf::from)
        .ok_or("ANCHOR_GOOSE_BINARY is required")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    // An isolated sandbox reaches its control plane through the relay, so the
    // shared-network opt-in is only required when the sandbox shares the network.
    if !RelaySettings::isolated(&binary)
        && std::env::var("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK").as_deref() != Ok("1")
    {
        return Err(
            "Goose needs ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1 to share the host network, \
             or an anchor-net-relay beside the Goose binary (ANCHOR_GOOSE_LOCAL_NETWORK=1) to isolate it"
                .into(),
        );
    }
    let digest = super::file_sha256(&binary)?;
    if std::env::var("ANCHOR_GOOSE_BINARY_SHA256").ok().as_deref() != Some(&digest) {
        return Err("ANCHOR_GOOSE_BINARY_SHA256 must match the configured binary".into());
    }
    Ok((binary, digest))
}

pub(super) fn command(
    sandbox: &anchor_sandbox_bwrap::BubblewrapSandbox,
    directory: &std::path::Path,
    binary: &std::path::Path,
    environment: Vec<SandboxEnvironment>,
    cancellation: anchor_runtime::Cancellation,
    transport: &BridgeTransport,
) -> Result<tokio::process::Command, String> {
    let mut grants = vec![anchor_runtime::ReadOnlyInput {
        source: binary.to_path_buf(),
        destination: "/tools/goose".into(),
    }];
    // The packaged Goose is the Anchor-built lean ACP server (`goose-acp`). It speaks ACP
    // on stdio directly and takes no subcommand: the upstream `goose acp` subcommand form
    // belongs to the full CLI and is intentionally not used, and the lean binary rejects it
    // with `unknown argument: acp`.
    let (argv, network) = match transport {
        BridgeTransport::Shared => (
            vec!["/tools/goose".to_owned()],
            anchor_runtime::NetworkPolicy::Enabled,
        ),
        BridgeTransport::Isolated { relay, socket } => {
            grants.push(anchor_runtime::ReadOnlyInput {
                source: relay.clone(),
                destination: RELAY_MOUNT.into(),
            });
            grants.push(anchor_runtime::ReadOnlyInput {
                source: socket.clone(),
                destination: SOCKET_MOUNT.into(),
            });
            (
                vec![
                    RELAY_MOUNT.to_owned(),
                    "--socket".to_owned(),
                    SOCKET_MOUNT.to_owned(),
                    "--listen".to_owned(),
                    RELAY_LISTEN.to_owned(),
                    "--".to_owned(),
                    "/tools/goose".to_owned(),
                ],
                anchor_runtime::NetworkPolicy::Disabled,
            )
        }
    };
    let sandbox = sandbox
        .with_readonly_grants(&grants)
        .map_err(|error| error.to_string())?;
    let mut process = anchor_runtime::SandboxRequest::new(directory, argv);
    process.readonly_inputs.extend(grants);
    process.network = network;
    process.cancellation = cancellation;
    process.environment = [
        ("GOOSE_MODE", "auto"),
        ("GOOSE_PATH_ROOT", "/workspace"),
        ("GOOSE_TELEMETRY_OFF", "1"),
        ("NO_COLOR", "1"),
        ("RUST_LOG", "warn"),
    ]
    .into_iter()
    .map(|(name, value)| SandboxEnvironment::new(name, value))
    .collect();
    process.environment.extend(environment);
    sandbox
        .isolated_command(process)
        .map_err(|error| error.to_string())
}

/// Sandbox-visible path of the in-sandbox network relay.
const RELAY_MOUNT: &str = "/tools/anchor-net-relay";
/// Sandbox-visible path of the bridge UNIX socket.
const SOCKET_MOUNT: &str = "/tools/anchor-bridge.sock";
/// Loopback address the relay serves inside an isolated sandbox.
pub(super) const RELAY_LISTEN: &str = "127.0.0.1:9080";

/// Goose builtins a session may load, alongside Anchor's own MCP server.
///
/// `tom` is required for the per-turn boundary text; other names are opt-in so a
/// disclosure layer (for example `code_execution`) can be enabled deliberately
/// instead of by default.
pub(super) fn enabled_builtins() -> Result<Vec<String>, String> {
    match std::env::var("ANCHOR_GOOSE_ENABLED_EXTENSIONS") {
        Ok(configured) => parse_enabled_builtins(&configured),
        Err(_) => Ok(default_enabled_builtins()),
    }
}

/// The per-turn boundary text depends on `tom`, so it is always the default.
fn default_enabled_builtins() -> Vec<String> {
    vec!["tom".to_owned()]
}

fn parse_enabled_builtins(configured: &str) -> Result<Vec<String>, String> {
    let names = serde_json::from_str::<Vec<String>>(configured)
        .map_err(|_| "ANCHOR_GOOSE_ENABLED_EXTENSIONS must be a JSON array of names".to_owned())?;
    for name in &names {
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(
                "ANCHOR_GOOSE_ENABLED_EXTENSIONS names must be simple identifiers".to_owned(),
            );
        }
    }
    Ok(names)
}

/// The facts a Goose sandbox should state to the model up front, so the agent
/// learns its boundary instead of discovering it by failing.
pub(super) struct BoundaryFacts {
    /// Whether the sandbox has its own network namespace.
    pub isolated: bool,
    /// Wall-clock budget for this node, when the graph sets one.
    pub wall_clock: Option<std::time::Duration>,
    /// Whether tools are disclosed on demand rather than listed up front.
    pub disclosure: bool,
}

/// Host-authored boundary statement, injected every turn through Goose's
/// persistent instructions (`GOOSE_MOIM_MESSAGE_TEXT`).
///
/// Only facts the model cannot read anywhere else belong here: mounts, network,
/// budget and the host authorization rule. Command limits, timeouts and output
/// retention already live in the `anchor_run` description, so they are only
/// pointed at to avoid drift and duplicated tokens.
pub(super) fn boundary_text(facts: &BoundaryFacts) -> String {
    let mut text = String::from(
        "节点边界（宿主声明，始终有效）：\n\
         - 文件：/workspace 可写；/in 与 /plugins 只读。\n\
         - 命令：只能通过 anchor_run 使用其说明中列出的授权命令。\n",
    );
    text.push_str(if facts.isolated {
        "- 网络：沙箱没有外网访问（无 DNS、无出口）；模型请求由宿主代理，凭据不在沙箱内。\n"
    } else {
        "- 网络：沙箱共享宿主网络。\n"
    });
    if let Some(limit) = facts.wall_clock {
        text.push_str(&format!(
            "- 预算：本节点墙钟上限约 {} 秒，到点会被强制中断；优先交付可用结果。\n",
            limit.as_secs()
        ));
    }
    if facts.disclosure {
        // Reference the constants so the prompt cannot drift from the port.
        text.push_str(&format!(
            "- 能力：可用工具按需披露——先用 `{}` 检索，再用 `{}` 调用；\
             可用集合由宿主授权决定，列表之外的工具无法调用。\n",
            crate::tool_disclosure::SEARCH_TOOL,
            crate::tool_disclosure::CALL_TOOL,
        ));
    }
    text.push_str("- 授权：对外发送消息、提交业务操作或改动宿主配置，都需要用户明确授权。");
    text
}

/// Isolated-sandbox settings shared by every Goose sandbox (nodes and Pilot).
///
/// The opt-in is read once per port; the relay binary defaults to the one shipped
/// beside the Goose binary.
pub(super) struct RelaySettings {
    relay: Option<PathBuf>,
}

impl RelaySettings {
    /// The sandbox relay for a pinned Goose binary, if one can be named.
    fn relay_binary(binary: &std::path::Path) -> Option<PathBuf> {
        std::env::var_os("ANCHOR_GOOSE_RELAY_BINARY")
            .map(PathBuf::from)
            .or_else(|| {
                binary
                    .parent()
                    .map(|parent| parent.join("anchor-net-relay"))
            })
    }

    /// Whether the sandbox runs isolated.
    ///
    /// Isolation is the default: a deployment that ships `anchor-net-relay` beside
    /// its Goose binary gets its own network namespace without asking. Setting
    /// `ANCHOR_GOOSE_LOCAL_NETWORK=0` returns to sharing the host network, which is
    /// also what happens when no relay binary is available.
    pub(super) fn isolated(binary: &std::path::Path) -> bool {
        Self::isolated_for(
            std::env::var("ANCHOR_GOOSE_LOCAL_NETWORK").ok().as_deref(),
            Self::relay_binary(binary).is_some_and(|path| path.is_file()),
        )
    }

    /// Pure form of the decision so it is testable without the process environment.
    fn isolated_for(flag: Option<&str>, relay_available: bool) -> bool {
        match flag {
            Some("0") => false,
            Some("1") => true,
            _ => relay_available,
        }
    }

    pub(super) fn from_env(binary: &std::path::Path) -> Result<Self, String> {
        if !Self::isolated(binary) {
            return Ok(Self { relay: None });
        }
        let relay = Self::relay_binary(binary)
            .ok_or("ANCHOR_GOOSE_BINARY has no directory to look for the sandbox relay")?;
        Ok(Self { relay: Some(relay) })
    }

    pub(super) fn is_isolated(&self) -> bool {
        self.relay.is_some()
    }

    /// The transport one sandbox uses to reach the bridge and the model proxy.
    pub(super) fn transport(&self, socket: PathBuf) -> BridgeTransport {
        match &self.relay {
            Some(relay) => BridgeTransport::Isolated {
                relay: relay.clone(),
                socket,
            },
            None => BridgeTransport::Shared,
        }
    }
}

/// How a Goose sandbox reaches Anchor's bridge and model proxy.
pub(super) enum BridgeTransport {
    /// The sandbox shares the host network and dials the bridge over loopback.
    Shared,
    /// The sandbox runs in its own network namespace. An in-sandbox relay listens
    /// on that namespace's loopback and forwards to the bridge over a mounted
    /// UNIX socket, so the bridge is the only host service it can reach.
    Isolated {
        relay: std::path::PathBuf,
        socket: std::path::PathBuf,
    },
}

impl BridgeTransport {
    /// Base URL the sandbox uses for the model proxy and the MCP server.
    pub(super) fn endpoint(&self, bridge_url: &str) -> String {
        match self {
            Self::Shared => bridge_url.to_owned(),
            Self::Isolated { .. } => format!("http://{RELAY_LISTEN}"),
        }
    }
}

/// Host-side model endpoint used by the bridge proxy.
#[derive(Clone)]
pub(super) struct ModelUpstream {
    pub url: Url,
    pub api_key: String,
}

pub(super) struct ModelRegistry {
    fixture: bool,
    /// Sandbox-dials-the-provider mode. Kept only as a rollback lever for the
    /// bridge proxy; the fixture transport always proxies.
    direct: bool,
    /// Context window advertised to Goose, overriding its model-name heuristic.
    context_limit: Option<u64>,
    endpoint: Url,
    wire: &'static str,
    model: String,
    api_key: String,
    aliases: BTreeMap<String, String>,
}

pub(super) struct ModelBinding {
    pub(super) model: String,
    pub(super) identity: String,
}

impl ModelRegistry {
    pub(super) fn from_env(fixture: bool) -> Result<Self, String> {
        let mut invalid_unicode = None;
        let registry = Self::from_values(fixture, |name| match std::env::var(name) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                invalid_unicode = Some(name.to_owned());
                None
            }
        });
        if let Some(name) = invalid_unicode {
            return Err(format!("{name} must be valid Unicode"));
        }
        registry
    }

    fn from_values(
        fixture: bool,
        mut value: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        // The sandbox-dials-the-provider lever exists only for native mode.
        let direct = !fixture && value("ANCHOR_MODEL_DIRECT").as_deref() == Some("1");
        let (endpoint, wire, model, api_key, aliases) = if fixture {
            let host =
                value("ANCHOR_GOOSE_OPENAI_HOST").ok_or("ANCHOR_GOOSE_OPENAI_HOST is required")?;
            let mut endpoint = configured_url(&host, true)?;
            endpoint.set_path("/v1/chat/completions");
            let model = model_name(
                value("ANCHOR_GOOSE_MODEL").unwrap_or_else(|| "fixture-goose".into()),
                "ANCHOR_GOOSE_MODEL",
            )?;
            (endpoint, "chat", model, String::new(), BTreeMap::new())
        } else {
            let host = value("ANCHOR_MODEL_URL").ok_or("ANCHOR_MODEL_URL is required")?;
            let mut endpoint = configured_url(&host, false)?;
            let api_key =
                value("ANCHOR_MODEL_API_KEY").ok_or("ANCHOR_MODEL_API_KEY is required")?;
            validate_text(&api_key, "ANCHOR_MODEL_API_KEY")?;
            let model = model_name(
                value("ANCHOR_MODEL_NAME").unwrap_or_else(|| "default".into()),
                "ANCHOR_MODEL_NAME",
            )?;
            let wire = value("ANCHOR_MODEL_WIRE_API").unwrap_or_else(|| "responses".into());
            let (wire, suffix) = match wire.as_str() {
                "chat" => ("chat", "chat/completions"),
                "responses" => ("responses", "responses"),
                _ => return Err("ANCHOR_MODEL_WIRE_API must be chat or responses".into()),
            };
            let path = format!("{}/{suffix}", endpoint.path().trim_end_matches('/'));
            endpoint.set_path(&path);
            let aliases = model_aliases(value("ANCHOR_MODEL_ALIASES").as_deref())?;
            (endpoint, wire, model, api_key, aliases)
        };
        // Only a real provider needs an explicit window: the scripted fixture
        // transport reports its own limits.
        let context_limit = if fixture {
            None
        } else {
            match value("ANCHOR_MODEL_CONTEXT_WINDOW") {
                None => None,
                Some(window) => {
                    let parsed = window.trim().parse::<u64>().map_err(|_| {
                        "ANCHOR_MODEL_CONTEXT_WINDOW must be a positive integer".to_owned()
                    })?;
                    if parsed == 0 {
                        return Err(
                            "ANCHOR_MODEL_CONTEXT_WINDOW must be a positive integer".to_owned()
                        );
                    }
                    Some(parsed)
                }
            }
        };
        Ok(Self {
            fixture,
            direct,
            context_limit,
            endpoint,
            wire,
            model,
            api_key,
            aliases,
        })
    }

    pub(super) fn resolve(&self, reference: Option<&str>) -> Result<ModelBinding, String> {
        let model = match reference {
            None | Some("models.default") => &self.model,
            Some(reference) if reference == self.model => &self.model,
            Some(reference) => self
                .aliases
                .get(reference)
                .ok_or("model reference is not explicitly configured for Goose")?,
        };
        let identity = serde_json::to_vec(&(self.endpoint.as_str(), self.wire, model))
            .expect("model binding strings are serializable");
        Ok(ModelBinding {
            model: model.clone(),
            identity: format!("{:x}", Sha256::digest(identity)),
        })
    }

    pub(super) fn fixture(&self) -> bool {
        self.fixture
    }

    /// The host-side model endpoint the bridge proxies to.
    ///
    /// The sandbox receives only the bridge URL and a per-invocation token, so a
    /// real provider credential never enters it. `None` means the sandbox dials
    /// the provider itself, which only happens with the explicit
    /// `ANCHOR_MODEL_DIRECT=1` rollback lever.
    pub(super) fn model_upstream(&self) -> Option<ModelUpstream> {
        if self.direct {
            return None;
        }
        Some(ModelUpstream {
            url: self.endpoint.clone(),
            // A fixture upstream ignores credentials; a real provider does not.
            api_key: if self.fixture {
                "fixture-only-not-a-secret".to_owned()
            } else {
                self.api_key.clone()
            },
        })
    }

    pub(super) fn environment(
        &self,
        binding: &ModelBinding,
        bridge_url: &str,
        bridge_token: &str,
    ) -> Vec<SandboxEnvironment> {
        let (host, base_path, api_key) = if self.direct {
            (
                self.endpoint.origin().ascii_serialization(),
                format!(".{}", self.endpoint.path()),
                self.api_key.clone(),
            )
        } else {
            // The sandbox speaks only to the host bridge.
            (
                bridge_url.to_owned(),
                "v1/chat/completions".to_owned(),
                bridge_token.to_owned(),
            )
        };
        let mut environment = vec![
            SandboxEnvironment::new("GOOSE_PROVIDER", "openai"),
            SandboxEnvironment::new("GOOSE_MODEL", binding.model.clone()),
            SandboxEnvironment::new("OPENAI_HOST", host),
            SandboxEnvironment::new("OPENAI_BASE_PATH", base_path),
            SandboxEnvironment::new("OPENAI_API_KEY", api_key),
        ];
        if let Some(limit) = self.context_limit {
            // Goose otherwise infers the window from the model name, which is wrong
            // for the aliases and OpenAI-compatible endpoints deployed here.
            environment.push(SandboxEnvironment::new(
                "GOOSE_CONTEXT_LIMIT",
                limit.to_string(),
            ));
        }
        environment
    }
}

fn validate_text(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(format!(
            "{name} must be nonempty and contain no control characters"
        ));
    }
    Ok(())
}

fn model_name(value: String, name: &str) -> Result<String, String> {
    validate_text(&value, name)?;
    Ok(value.trim().to_owned())
}

fn configured_url(value: &str, fixture: bool) -> Result<Url, String> {
    let error = if fixture {
        "ANCHOR_GOOSE_OPENAI_HOST must be an HTTP loopback IP origin without credentials, path, query or fragment"
    } else {
        "ANCHOR_MODEL_URL must be an HTTPS endpoint or HTTP loopback IP endpoint without credentials, query or fragment"
    };
    let (_, remainder) = value.split_once("://").ok_or(error)?;
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control() || character == '\\')
        || authority.is_empty()
        || authority.contains('@')
        || (fixture && !matches!(&remainder[authority.len()..], "" | "/"))
    {
        return Err(error.into());
    }
    let url = Url::parse(value).map_err(|_| error.to_owned())?;
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    });
    let permitted_scheme = if fixture {
        url.scheme() == "http" && loopback
    } else {
        url.scheme() == "https" || (url.scheme() == "http" && loopback)
    };
    if !permitted_scheme
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(error.into());
    }
    Ok(url)
}

fn model_aliases(value: Option<&str>) -> Result<BTreeMap<String, String>, String> {
    let value = value.filter(|value| !value.is_empty()).unwrap_or("{}");
    let aliases: serde_json::Value = serde_json::from_str(value)
        .map_err(|_| "ANCHOR_MODEL_ALIASES must be a JSON object".to_owned())?;
    let aliases = aliases
        .as_object()
        .ok_or("ANCHOR_MODEL_ALIASES must be a JSON object")?;
    let mut models = BTreeMap::new();
    for (reference, model) in aliases {
        if !reference.starts_with("models.")
            || reference.len() == "models.".len()
            || reference == "models.default"
            || reference.chars().any(char::is_whitespace)
            || reference.chars().any(char::is_control)
        {
            return Err("model aliases require a non-default models.* reference".into());
        }
        let model = model
            .as_str()
            .ok_or("model aliases require a nonempty model name without control characters")?;
        let model = model_name(model.to_owned(), "model alias name")?;
        models.insert(reference.clone(), model);
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_values() -> BTreeMap<String, String> {
        BTreeMap::from([(
            "ANCHOR_GOOSE_OPENAI_HOST".into(),
            "http://127.0.0.1:43210".into(),
        )])
    }

    fn native_values() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "ANCHOR_MODEL_URL".into(),
                "https://provider.example/gateway/v1".into(),
            ),
            ("ANCHOR_MODEL_API_KEY".into(), "operator-secret".into()),
            ("ANCHOR_MODEL_NAME".into(), "primary-model".into()),
        ])
    }

    fn registry(fixture: bool, values: &BTreeMap<String, String>) -> ModelRegistry {
        ModelRegistry::from_values(fixture, |name| values.get(name).cloned()).unwrap()
    }

    fn environment(models: &ModelRegistry, reference: Option<&str>) -> BTreeMap<String, String> {
        let binding = models.resolve(reference).unwrap();
        let entries = models.environment(&binding, "http://127.0.0.1:54321", "bridge-secret");
        // Five transport entries, plus an optional context window.
        assert!((5..=6).contains(&entries.len()), "{entries:?}");
        assert!(!format!("{entries:?}").contains("secret"));
        entries
            .into_iter()
            .map(|entry| (entry.key.clone(), entry.value().to_owned()))
            .collect()
    }

    fn error(fixture: bool, values: &BTreeMap<String, String>) -> String {
        ModelRegistry::from_values(fixture, |name| values.get(name).cloned())
            .err()
            .expect("configuration must be rejected")
    }

    #[test]
    fn environment_constructor_has_the_host_api() {
        let _constructor: fn(bool) -> Result<ModelRegistry, String> = ModelRegistry::from_env;
    }

    #[test]
    fn fixture_defaults_and_environment_use_only_the_bridge() {
        let mut values = fixture_values();
        values.extend(native_values());
        values.insert("ANCHOR_MODEL_ALIASES".into(), "invalid JSON".into());
        let models = registry(true, &values);
        assert_eq!(
            models.model_upstream().unwrap().url.as_str(),
            "http://127.0.0.1:43210/v1/chat/completions"
        );
        assert_eq!(
            environment(&models, None),
            BTreeMap::from([
                ("GOOSE_PROVIDER".into(), "openai".into()),
                ("GOOSE_MODEL".into(), "fixture-goose".into()),
                ("OPENAI_HOST".into(), "http://127.0.0.1:54321".into()),
                ("OPENAI_BASE_PATH".into(), "v1/chat/completions".into()),
                ("OPENAI_API_KEY".into(), "bridge-secret".into()),
            ])
        );
    }

    #[test]
    fn fixture_accepts_only_loopback_ip_origins() {
        let mut values = fixture_values();
        for (host, expected) in [
            ("http://127.0.0.1", "http://127.0.0.1/v1/chat/completions"),
            (
                "http://127.5.6.7:8080/",
                "http://127.5.6.7:8080/v1/chat/completions",
            ),
            ("http://[::1]:8080", "http://[::1]:8080/v1/chat/completions"),
        ] {
            values.insert("ANCHOR_GOOSE_OPENAI_HOST".into(), host.into());
            assert_eq!(
                registry(true, &values)
                    .model_upstream()
                    .unwrap()
                    .url
                    .as_str(),
                expected
            );
        }
        for host in [
            "https://127.0.0.1",
            "http://localhost",
            "http://provider.example",
            "http://192.0.2.1",
            "http://[::2]",
            "http://127.0.0.1/v1",
            "http://127.0.0.1/a/..",
            "http://127.0.0.1//",
            "http://secret@127.0.0.1",
            "http://user:secret@127.0.0.1",
            "http://@127.0.0.1",
            "http://127.0.0.1?secret",
            "http://127.0.0.1?",
            "http://127.0.0.1#secret",
            "http://127.0.0.1#",
            "http:127.0.0.1",
            "http:///127.0.0.1",
            "http://127.0.0.1/\\..",
            " http://127.0.0.1",
            "http://127.0.0.1\n",
            "",
            "secret",
        ] {
            values.insert("ANCHOR_GOOSE_OPENAI_HOST".into(), host.into());
            let failure = error(true, &values);
            assert!(failure.contains("ANCHOR_GOOSE_OPENAI_HOST"));
            assert!(!failure.contains("secret"));
        }
        values.remove("ANCHOR_GOOSE_OPENAI_HOST");
        assert_eq!(error(true, &values), "ANCHOR_GOOSE_OPENAI_HOST is required");
    }

    #[test]
    fn fixture_resolves_only_default_or_the_configured_model() {
        let mut values = fixture_values();
        values.insert("ANCHOR_GOOSE_MODEL".into(), "configured-model".into());
        values.insert(
            "ANCHOR_MODEL_ALIASES".into(),
            r#"{"models.alias":"configured-model"}"#.into(),
        );
        let models = registry(true, &values);
        for reference in [None, Some("models.default"), Some("configured-model")] {
            assert_eq!(models.resolve(reference).unwrap().model, "configured-model");
        }
        for reference in [
            "models.alias",
            "models.unknown",
            "fixture-goose",
            "default",
            "",
        ] {
            assert!(models.resolve(Some(reference)).is_err());
        }
    }

    #[test]
    fn fixture_rejects_empty_or_control_character_model_names() {
        let mut values = fixture_values();
        for model in [
            "",
            "  ",
            "secret\n",
            "secret\r",
            "secret\0",
            "secret\t",
            "secret\u{7f}",
            "secret\u{85}",
        ] {
            values.insert("ANCHOR_GOOSE_MODEL".into(), model.into());
            let failure = error(true, &values);
            assert!(failure.contains("ANCHOR_GOOSE_MODEL"));
            assert!(!failure.contains("secret"));
        }
    }

    #[test]
    fn native_chat_and_responses_keep_the_configured_prefix() {
        let mut values = native_values();
        for (url, host, prefix) in [
            ("https://provider.example", "https://provider.example", ""),
            ("https://provider.example/", "https://provider.example", ""),
            (
                "https://provider.example/v1",
                "https://provider.example",
                "v1/",
            ),
            (
                "https://provider.example/gateway/openai/v1/",
                "https://provider.example",
                "gateway/openai/v1/",
            ),
            (
                "https://provider.example:8443/custom/api",
                "https://provider.example:8443",
                "custom/api/",
            ),
            (
                "https://provider.example/team%2Fname/v1",
                "https://provider.example",
                "team%2Fname/v1/",
            ),
            (
                "http://127.0.0.1:8080/custom/v1/",
                "http://127.0.0.1:8080",
                "custom/v1/",
            ),
            (
                "http://[::1]:8080/custom/v1",
                "http://[::1]:8080",
                "custom/v1/",
            ),
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), url.into());
            for (wire, suffix) in [("chat", "chat/completions"), ("responses", "responses")] {
                values.insert("ANCHOR_MODEL_WIRE_API".into(), wire.into());
                let models = registry(false, &values);
                // The host dials the configured endpoint...
                assert_eq!(models.model_upstream().unwrap().url, models.endpoint);
                // ...while the sandbox only ever speaks to the bridge.
                let entries = environment(&models, None);
                assert_eq!(entries["OPENAI_HOST"], "http://127.0.0.1:54321");
                assert_eq!(entries["OPENAI_BASE_PATH"], "v1/chat/completions");
                // The explicit rollback lever restores sandbox-direct dialing with
                // the configured prefix preserved.
                values.insert("ANCHOR_MODEL_DIRECT".into(), "1".into());
                let direct = registry(false, &values);
                assert!(direct.model_upstream().is_none());
                let entries = environment(&direct, None);
                assert_eq!(entries["OPENAI_HOST"], host);
                assert_eq!(entries["OPENAI_BASE_PATH"], format!("./{prefix}{suffix}"));
                assert_eq!(
                    Url::parse(&entries["OPENAI_HOST"])
                        .unwrap()
                        .join(&entries["OPENAI_BASE_PATH"])
                        .unwrap(),
                    direct.endpoint
                );
                values.remove("ANCHOR_MODEL_DIRECT");
            }
        }
    }

    #[test]
    fn native_environment_calls_the_bridge_without_provider_credentials() {
        let mut values = native_values();
        values.extend(fixture_values());
        let models = registry(false, &values);
        assert_eq!(
            environment(&models, None),
            BTreeMap::from([
                ("GOOSE_PROVIDER".into(), "openai".into()),
                ("GOOSE_MODEL".into(), "primary-model".into()),
                ("OPENAI_HOST".into(), "http://127.0.0.1:54321".into()),
                ("OPENAI_BASE_PATH".into(), "v1/chat/completions".into()),
                ("OPENAI_API_KEY".into(), "bridge-secret".into()),
            ])
        );
        // The provider endpoint and credential stay host-side.
        let upstream = models.model_upstream().unwrap();
        assert_eq!(
            upstream.url.as_str(),
            "https://provider.example/gateway/v1/responses"
        );
        assert_eq!(upstream.api_key, "operator-secret");
    }

    #[test]
    fn native_base_path_cannot_change_origin_through_url_join() {
        let mut values = native_values();
        for prefix in [
            "/https://other.example/v1",
            "/https://user:secret@other.example/v1",
            "//other.example/v1",
            "///other.example/v1",
            "/tenant:name/v1",
        ] {
            values.insert(
                "ANCHOR_MODEL_URL".into(),
                format!("https://provider.example{prefix}"),
            );
            for wire in ["chat", "responses"] {
                values.insert("ANCHOR_MODEL_WIRE_API".into(), wire.into());
                let models = registry(false, &values);
                let upstream = models.model_upstream().unwrap().url;
                assert_eq!(
                    upstream.origin().ascii_serialization(),
                    "https://provider.example"
                );
                assert!(upstream.username().is_empty());
                assert!(upstream.password().is_none());
                values.insert("ANCHOR_MODEL_DIRECT".into(), "1".into());
                let direct = registry(false, &values);
                let entries = environment(&direct, None);
                let joined = Url::parse(&entries["OPENAI_HOST"])
                    .unwrap()
                    .join(&entries["OPENAI_BASE_PATH"])
                    .unwrap();
                assert_eq!(joined, direct.endpoint);
                assert_eq!(
                    joined.origin().ascii_serialization(),
                    "https://provider.example"
                );
                assert!(joined.username().is_empty());
                assert!(joined.password().is_none());
                values.remove("ANCHOR_MODEL_DIRECT");
            }
        }
    }

    #[test]
    fn native_allows_https_or_explicit_http_loopback_only() {
        let mut values = native_values();
        for url in [
            "https://provider.example/v1",
            "https://192.0.2.1/v1",
            "https://localhost/v1",
            "http://127.0.0.1/v1",
            "http://127.255.255.254/v1",
            "http://[::1]/v1",
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), url.into());
            assert!(ModelRegistry::from_values(false, |name| values.get(name).cloned()).is_ok());
        }
        for url in [
            "http://provider.example/v1",
            "http://192.0.2.1/v1",
            "http://localhost/v1",
            "http://[::2]/v1",
            "ftp://provider.example/v1",
            "https://user:secret@provider.example/v1",
            "https://secret@provider.example/v1",
            "https://@provider.example/v1",
            "https://provider.example/v1?token=secret",
            "https://provider.example/v1?",
            "https://provider.example/v1#secret",
            "https://provider.example/v1#",
            "https:provider.example/v1",
            "https:///provider.example/v1",
            "https://",
            "https://provider.example:secret/v1",
            "https://provider.example/\nsecret",
            "https://provider.example/\\secret",
            "",
            "secret",
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), url.into());
            let failure = error(false, &values);
            assert!(failure.contains("ANCHOR_MODEL_URL"));
            assert!(!failure.contains("secret"));
        }
    }

    #[test]
    fn native_requires_endpoint_and_key_and_preserves_existing_name_default() {
        for missing in ["ANCHOR_MODEL_URL", "ANCHOR_MODEL_API_KEY"] {
            let mut values = native_values();
            values.remove(missing);
            assert_eq!(error(false, &values), format!("{missing} is required"));
        }
        let mut values = native_values();
        values.remove("ANCHOR_MODEL_NAME");
        let models = registry(false, &values);
        assert_eq!(models.resolve(None).unwrap().model, "default");
    }

    #[test]
    fn native_rejects_empty_control_character_or_unsupported_values_without_echoing() {
        for name in [
            "ANCHOR_MODEL_API_KEY",
            "ANCHOR_MODEL_NAME",
            "ANCHOR_MODEL_WIRE_API",
        ] {
            for invalid in [
                "",
                "  ",
                "secret\n",
                "secret\r",
                "secret\0",
                "secret\t",
                "secret\u{7f}",
                "secret\u{85}",
            ] {
                let mut values = native_values();
                values.insert(name.into(), invalid.into());
                let failure = error(false, &values);
                assert!(failure.contains(name));
                assert!(!failure.contains("secret"));
            }
        }
        let mut values = native_values();
        values.insert("ANCHOR_MODEL_WIRE_API".into(), "secret-wire".into());
        assert_eq!(
            error(false, &values),
            "ANCHOR_MODEL_WIRE_API must be chat or responses"
        );
    }

    #[test]
    fn aliases_are_explicit_and_unknown_references_never_fall_back() {
        let mut values = native_values();
        values.insert("ANCHOR_MODEL_ALIASES".into(), r#"{"models.research":" research-model ","models.review":"review-model","models.same":"primary-model"}"#.into());
        let models = registry(false, &values);
        for reference in [None, Some("models.default"), Some("primary-model")] {
            assert_eq!(models.resolve(reference).unwrap().model, "primary-model");
        }
        let research = models.resolve(Some("models.research")).unwrap();
        assert_eq!(research.model, "research-model");
        assert_eq!(
            environment(&models, Some("models.research"))["GOOSE_MODEL"],
            "research-model"
        );
        assert_eq!(
            models.resolve(Some("models.review")).unwrap().model,
            "review-model"
        );
        assert_eq!(
            models.resolve(Some("models.same")).unwrap().identity,
            models.resolve(None).unwrap().identity
        );
        for reference in [
            "models.unknown",
            "research-model",
            "review-model",
            "default",
            "",
            "secret-reference",
        ] {
            let failure = models.resolve(Some(reference)).err().unwrap();
            assert!(!failure.contains("secret"));
        }
    }

    #[test]
    fn aliases_reject_invalid_json_references_and_model_names_without_echoing() {
        let mut values = native_values();
        for aliases in [
            "secret-json",
            "[]",
            "null",
            r#""secret""#,
            r#"{"secret":"model"}"#,
            r#"{"models.":"model"}"#,
            r#"{"models.default":"secret"}"#,
            r#"{"models. secret":"model"}"#,
            r#"{"models.secret\n":"model"}"#,
            r#"{"models.secret":null}"#,
            r#"{"models.secret":3}"#,
            r#"{"models.secret":""}"#,
            r#"{"models.secret":"   "}"#,
            r#"{"models.secret":"secret\n"}"#,
            r#"{"models.secret":"secret\t"}"#,
            r#"{"models.secret":"secret\u0000"}"#,
            r#"{"models.secret":"secret\u007f"}"#,
            r#"{"models.secret":"secret\u0085"}"#,
        ] {
            values.insert("ANCHOR_MODEL_ALIASES".into(), aliases.into());
            assert!(!error(false, &values).contains("secret"));
        }
        for aliases in ["", "{}"] {
            values.insert("ANCHOR_MODEL_ALIASES".into(), aliases.into());
            assert!(
                registry(false, &values)
                    .resolve(Some("models.unknown"))
                    .is_err()
            );
        }
    }

    #[test]
    fn identity_is_a_normalized_endpoint_wire_and_model_hash_without_the_key() {
        let mut values = native_values();
        let original = registry(false, &values).resolve(None).unwrap();
        assert_eq!(
            original.identity,
            "e2c3f48fcf29379264d15917b7ae9fc1f0d2a157d324107522925ba1aa3f4972"
        );
        assert_eq!(original.identity.len(), 64);
        assert!(
            original
                .identity
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert!(!original.identity.contains("operator-secret"));
        for endpoint in [
            "https://PROVIDER.example:443/gateway/v1/",
            "https://provider.example/gateway/ignored/../v1",
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), endpoint.into());
            assert_eq!(
                registry(false, &values).resolve(None).unwrap().identity,
                original.identity
            );
        }
        values.insert("ANCHOR_MODEL_API_KEY".into(), "rotated-secret".into());
        let rotated = registry(false, &values);
        assert_eq!(rotated.resolve(None).unwrap().identity, original.identity);
        // A rotated provider credential reaches the host-side upstream only; the
        // sandbox keeps the per-invocation bridge token.
        assert_eq!(rotated.model_upstream().unwrap().api_key, "rotated-secret");
        assert_eq!(
            environment(&rotated, None)["OPENAI_API_KEY"],
            "bridge-secret"
        );
        for (name, replacement) in [
            ("ANCHOR_MODEL_URL", "https://other.example/gateway/v1"),
            ("ANCHOR_MODEL_URL", "https://provider.example/other/v1"),
            ("ANCHOR_MODEL_WIRE_API", "chat"),
            ("ANCHOR_MODEL_NAME", "other-model"),
        ] {
            let mut changed = native_values();
            changed.insert(name.into(), replacement.into());
            assert_ne!(
                registry(false, &changed).resolve(None).unwrap().identity,
                original.identity
            );
        }
    }

    #[test]
    fn fixture_identity_tracks_upstream_and_model_but_not_the_bridge() {
        let values = fixture_values();
        let models = registry(true, &values);
        let binding = models.resolve(None).unwrap();
        models.environment(&binding, "http://127.0.0.1:54321", "first-secret");
        models.environment(&binding, "http://127.0.0.1:54322", "rotated-secret");
        assert_eq!(models.resolve(None).unwrap().identity, binding.identity);
        assert_eq!(
            models.resolve(Some("models.default")).unwrap().identity,
            binding.identity
        );
        for (name, replacement) in [
            ("ANCHOR_GOOSE_OPENAI_HOST", "http://127.0.0.1:43211"),
            ("ANCHOR_GOOSE_MODEL", "other-model"),
        ] {
            let mut changed = values.clone();
            changed.insert(name.into(), replacement.into());
            assert_ne!(
                registry(true, &changed).resolve(None).unwrap().identity,
                binding.identity
            );
        }
    }

    #[test]
    fn boundary_text_states_network_budget_and_authorization() {
        let isolated = boundary_text(&BoundaryFacts {
            isolated: true,
            wall_clock: Some(std::time::Duration::from_secs(240)),
            disclosure: false,
        });
        assert!(isolated.contains("/workspace 可写"), "{isolated}");
        assert!(isolated.contains("没有外网访问"), "{isolated}");
        assert!(isolated.contains("240 秒"), "{isolated}");
        assert!(isolated.contains("需要用户明确授权"), "{isolated}");
        assert!(isolated.contains("anchor_run"), "{isolated}");

        let shared = boundary_text(&BoundaryFacts {
            isolated: false,
            wall_clock: None,
            disclosure: false,
        });
        assert!(shared.contains("共享宿主网络"), "{shared}");
        let disclosed = boundary_text(&BoundaryFacts {
            isolated: true,
            wall_clock: None,
            disclosure: true,
        });
        assert!(disclosed.contains("anchor_tools"), "{disclosed}");
        assert!(!shared.contains("anchor_tools"), "{shared}");
        assert!(!shared.contains("秒"), "{shared}");
        assert!(!shared.contains("secret"), "{shared}");
    }

    #[test]
    fn enabled_builtins_default_to_the_boundary_provider_only() {
        assert_eq!(default_enabled_builtins(), vec!["tom".to_owned()]);
        assert_eq!(
            parse_enabled_builtins(r#"["tom","todo","skills"]"#).unwrap(),
            vec!["tom".to_owned(), "todo".into(), "skills".into()]
        );
        for invalid in ["tom", "{}", r#"[1]"#, r#"["bad name"]"#, r#"[""]"#] {
            assert!(
                parse_enabled_builtins(invalid).is_err(),
                "{invalid} must be rejected"
            );
        }
    }

    #[test]
    fn isolation_is_the_default_when_a_relay_is_available() {
        assert!(RelaySettings::isolated_for(None, true));
        assert!(!RelaySettings::isolated_for(None, false));
        assert!(RelaySettings::isolated_for(Some("1"), false));
        assert!(!RelaySettings::isolated_for(Some("0"), true));
    }

    #[test]
    fn native_context_window_is_advertised_to_goose() {
        let mut values = native_values();
        values.insert("ANCHOR_MODEL_CONTEXT_WINDOW".into(), "1000000".into());
        let models = registry(false, &values);
        let entries = environment(&models, None);
        assert_eq!(entries["GOOSE_CONTEXT_LIMIT"], "1000000");
        // Without it, Goose keeps its own model-name heuristic.
        let plain = environment(&registry(false, &native_values()), None);
        assert!(!plain.contains_key("GOOSE_CONTEXT_LIMIT"));
    }

    #[test]
    fn context_window_must_be_a_positive_integer() {
        for window in ["0", "abc", "-5", ""] {
            let mut values = native_values();
            values.insert("ANCHOR_MODEL_CONTEXT_WINDOW".into(), window.into());
            let failure = error(false, &values);
            assert!(failure.contains("ANCHOR_MODEL_CONTEXT_WINDOW"), "{failure}");
        }
    }

    #[test]
    fn configuration_reads_only_its_explicit_mode_specific_variables() {
        for (fixture, expected) in [
            (true, vec!["ANCHOR_GOOSE_OPENAI_HOST", "ANCHOR_GOOSE_MODEL"]),
            (
                false,
                vec![
                    "ANCHOR_MODEL_DIRECT",
                    "ANCHOR_MODEL_URL",
                    "ANCHOR_MODEL_API_KEY",
                    "ANCHOR_MODEL_NAME",
                    "ANCHOR_MODEL_WIRE_API",
                    "ANCHOR_MODEL_ALIASES",
                    "ANCHOR_MODEL_CONTEXT_WINDOW",
                ],
            ),
        ] {
            let values = if fixture {
                fixture_values()
            } else {
                native_values()
            };
            let mut read = Vec::new();
            ModelRegistry::from_values(fixture, |name| {
                read.push(name.to_owned());
                values.get(name).cloned()
            })
            .unwrap();
            assert_eq!(read, expected);
        }
    }
}
