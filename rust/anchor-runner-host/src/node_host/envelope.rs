//! Common Graph invocation authority. Command-specific overlays stay with their
//! callers; Plugin MCP launches keep their existing (non-command) sandbox.

use crate::tool_environment::ToolEnvironment;
use anchor_runtime::{NetworkPolicy, ReadOnlyInput, SandboxRequest};
use anchor_sandbox_bwrap::BubblewrapSandbox;
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, sync::Arc, time::Duration};

/// Historical default for one `anchor_run` command.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

fn command_timeout_ceiling(wall_time_limit_seconds: Option<f64>) -> Duration {
    let configured = std::env::var("ANCHOR_NODE_COMMAND_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    resolve_command_ceiling(configured, wall_time_limit_seconds)
}

// Keep the existing deployment/node ceiling semantics, including the historical
// thirty-second floor. Moving the owner must not change a command's authority.
fn resolve_command_ceiling(
    configured: Option<u64>,
    wall_time_limit_seconds: Option<f64>,
) -> Duration {
    let configured = configured
        .filter(|value| *value > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_TIMEOUT);
    let node = wall_time_limit_seconds
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .unwrap_or(DEFAULT_TIMEOUT);
    configured.min(node).max(DEFAULT_TIMEOUT)
}

pub(crate) struct InvocationEnvelope {
    pub(crate) sandbox: Arc<BubblewrapSandbox>,
    pub(crate) network: NetworkPolicy,
    pub(crate) environment: ToolEnvironment,
    pub(crate) timeout_ceiling: Duration,
    pub(crate) op_timeout: Duration,
    description: Value,
}

impl InvocationEnvelope {
    /// Commands on both paths have a private /proc. Do not apply this command
    /// grant to Plugin MCP processes, whose launch policy is unchanged.
    pub(crate) fn command_sandbox(&self) -> BubblewrapSandbox {
        self.sandbox.with_pid_namespace_proc()
    }

    /// Keys and set-like collections are sorted; PATH itself retains precedence.
    pub(crate) fn describe(&self) -> Value {
        self.description.clone()
    }

    pub(crate) fn digest(&self) -> String {
        anchor_facts::text_digest(&self.description.to_string())
    }
}

/// Resolve host grants once from the invocation's already frozen Plugin set.
/// Artifact versions, workspace bindings and route/development/spill overlays
/// are deliberately not coordinates of this common environment.
pub(super) fn invocation_envelope(
    floor: &BubblewrapSandbox,
    environment: ToolEnvironment,
    plugin_mounts: &[ReadOnlyInput],
    network: bool,
    local_inputs: &[ReadOnlyInput],
    credential_plugins: Vec<String>,
    wall_time_limit_seconds: Option<f64>,
) -> Result<InvocationEnvelope, String> {
    let mut grants = local_inputs.to_vec();
    grants.extend_from_slice(plugin_mounts);
    let sandbox = Arc::new(
        environment
            .grant_sandbox(floor)?
            .with_readonly_grants(&grants)
            .map_err(|error| error.to_string())?,
    );
    let network = if network {
        NetworkPolicy::Enabled
    } else {
        NetworkPolicy::Disabled
    };
    let timeout_ceiling = command_timeout_ceiling(wall_time_limit_seconds);
    let op_timeout = Duration::from_secs_f64(wall_time_limit_seconds.unwrap_or(3600.0));
    // Reuse the actual tool-environment projection, not a second PATH/env rule.
    // This request never executes and contains no invocation identity or paths.
    let mut request = SandboxRequest::new("/workspace", ["sh"]);
    request.network = network;
    environment.apply_to_workspace_command(&mut request);
    let mut description = sandbox.with_pid_namespace_proc().describe_boundary();
    let readonly = description["readonly"]
        .as_array_mut()
        .expect("boundary mounts");
    readonly.retain(|mount| {
        !local_inputs
            .iter()
            .any(|input| mount["destination"].as_str() == input.destination.to_str())
    });
    for mount in readonly.iter_mut() {
        if let Some(source) = mount["source"].as_str()
            && let Some(name) = environment.generated_mount_source(Path::new(source))
        {
            mount["source"] = json!(name);
        }
    }
    if let Some(view) = anchor_sandbox_bwrap::device_sys_view(&request.devices) {
        readonly.push(json!({"destination": view, "source": view}));
    }
    description["local_inputs"] = json!(
        local_inputs
            .iter()
            .map(|input| {
                // Input IDs and source trees do not change the capability boundary.
                [
                    "/local-inputs",
                    "/in/channel",
                    "/in/channel-pending",
                    "/in/notifications",
                    "/in/tasks",
                ]
                .into_iter()
                .find(|category| input.destination.starts_with(category))
                .unwrap_or("/in")
            })
            .collect::<BTreeSet<_>>()
    );
    description["network"] = json!(network == NetworkPolicy::Enabled);
    description["devices"] = json!({"minimal": "/dev", "grants": request.devices});
    description["tool_directories"] = json!(request.tool_dirs);
    let mut variables = json!({});
    for (key, value) in anchor_sandbox_bwrap::FIXED_ENVIRONMENT {
        variables[key] = json!(value);
    }
    for variable in request.environment {
        // ToolEnvironment injects only host-owned paths and fixed switches.
        // Plugin configuration/credential environments never enter this request.
        variables[&variable.key] = json!(variable.value());
    }
    description["environment"] = variables;
    description["command_timeout_ceiling_ms"] = json!({
        "anchor_run": timeout_ceiling.as_millis(), "op_run": op_timeout.as_millis()
    });
    description["credential_plugins"] = json!(credential_plugins);
    for key in [
        "readonly",
        "writable",
        "tool_directories",
        "credential_plugins",
    ] {
        sort_set(&mut description[key]);
    }
    sort_set(&mut description["devices"]["grants"]);
    Ok(InvocationEnvelope {
        sandbox,
        network,
        environment,
        timeout_ceiling,
        op_timeout,
        description: anchor_facts::canonical(&description),
    })
}

fn sort_set(value: &mut Value) {
    let items = value.as_array_mut().expect("envelope set");
    items.sort_by_cached_key(Value::to_string);
    items.dedup();
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_sandbox_bwrap::BubblewrapPolicy;

    #[test]
    fn command_ceiling_stays_thirty_seconds_unless_the_deployment_raises_it() {
        use std::time::Duration;
        // Historical default: the deployment grants nothing, so a node budget does
        // not widen the command ceiling on its own.
        assert_eq!(
            super::resolve_command_ceiling(None, None),
            Duration::from_secs(30)
        );
        assert_eq!(
            super::resolve_command_ceiling(None, Some(3600.0)),
            Duration::from_secs(30)
        );
        // An explicit deployment grant widens it, never beyond the node budget.
        assert_eq!(
            super::resolve_command_ceiling(Some(86400), Some(3600.0)),
            Duration::from_secs(3600)
        );
        assert_eq!(
            super::resolve_command_ceiling(Some(86400), Some(1.0e9)),
            Duration::from_secs(86400)
        );
        // Zero or garbage configuration never removes the default.
        assert_eq!(
            super::resolve_command_ceiling(Some(0), Some(3600.0)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn envelope_identity_ignores_invocation_paths_but_tracks_network_and_tools() {
        let root = tempfile::tempdir().unwrap();
        let floor = BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["sh"])
                .authorize_workspace_root(root.path())
                .allow_network(),
        )
        .unwrap();
        let bin = root.path().join("rust/bin");
        let home = root.path().join("rust/home");
        let cargo = root.path().join("rust/cargo");
        for directory in [&bin, &home, &cargo] {
            std::fs::create_dir_all(directory).unwrap();
        }
        for name in ["rustup", "cargo", "rustc"] {
            std::fs::copy("/usr/bin/true", bin.join(name)).unwrap();
        }
        let rust = || {
            let mut environment = ToolEnvironment::default();
            environment
                .configure_pilot_rust(&bin.join("rustup"), &home, &cargo)
                .unwrap();
            environment
        };
        let library = root.path().join("library");
        let tool = library.join("tools/demo");
        std::fs::create_dir_all(&tool).unwrap();
        std::fs::write(tool.join("tool.json"), r#"{"entrypoint":"/bin/sh"}"#).unwrap();
        let resolve = |run: &str, network, environment| {
            let source = root.path().join(run).join("workspace");
            std::fs::create_dir_all(&source).unwrap();
            invocation_envelope(
                &floor,
                environment,
                &[],
                network,
                &[ReadOnlyInput::new(
                    &source,
                    format!("/in/channel-pending/{run}"),
                )],
                vec![],
                None,
            )
            .unwrap()
        };
        let first = resolve("run-one", false, ToolEnvironment::default());
        let second = resolve("run-two", false, ToolEnvironment::default());
        assert_eq!(first.digest(), second.digest());
        assert_eq!(first.describe(), second.describe());
        assert_eq!(
            resolve("run-one", false, rust()).digest(),
            resolve("run-two", false, rust()).digest()
        );
        let description = first.describe().to_string();
        assert!(!description.contains("run-one") && !description.contains("run-two"));
        assert!(!description.contains(&root.path().display().to_string()));
        assert_eq!(
            anchor_facts::canonical(&first.describe()).to_string(),
            description
        );
        assert_eq!(anchor_facts::text_digest(&description), first.digest());
        assert_ne!(
            first.digest(),
            resolve("run-one", true, ToolEnvironment::default()).digest()
        );
        let with_tools = resolve("run-one", false, ToolEnvironment::load(&library).unwrap());
        assert_ne!(first.digest(), with_tools.digest());
        let other = root.path().join("other-library");
        std::fs::create_dir_all(other.join("tools/demo")).unwrap();
        std::fs::copy(tool.join("tool.json"), other.join("tools/demo/tool.json")).unwrap();
        assert_ne!(
            with_tools.digest(),
            resolve("run-one", false, ToolEnvironment::load(&other).unwrap()).digest()
        );
    }
}
