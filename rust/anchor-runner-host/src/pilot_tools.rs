use std::{
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

#[cfg(test)]
use anchor_graph_host::PluginCatalog;
use anchor_platform_session::SessionStore;
#[cfg(test)]
use anchor_runtime::graph::GraphSnapshot;
use anchor_runtime::{ToolDefinition, ToolError, ToolPort, ToolResultContent};
use anchor_sandbox_bwrap::{BubblewrapPolicy, BubblewrapSandbox};
use serde_json::{Value, json};
#[cfg(test)]
use sha2::Digest as _;

pub(crate) mod channel_management;
mod graphs;
mod initialization;
mod plugins;
mod runs;
mod scheduling;
mod sessions;

use crate::{
    application::{AdmissionOptions, RunApplication},
    run_metadata::{PilotRunSource, RunTrigger},
    tool_host,
};

/// The Plugin ids this deployment enabled as its in-process fake fixture.
///
/// The same configuration the node host reads: a fixture binding is enabled by
/// the deployment, not by a Library directory.
fn fake_plugin_ids() -> Vec<String> {
    std::env::var("ANCHOR_RUNNER_FAKE_PLUGINS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect()
}

fn pilot_plugin_sandbox_with_environment(
    bwrap: &Path,
    workspace_root: &Path,
    owner: &str,
    session: &str,
    environment: &crate::tool_environment::ToolEnvironment,
) -> Result<(BubblewrapSandbox, PathBuf), String> {
    let commands = std::env::var("ANCHOR_RUNNER_ALLOWED_COMMANDS").unwrap_or_default();
    let workspace = crate::pilot_resources::session_workspace(workspace_root, owner, session);
    std::fs::create_dir_all(&workspace)
        .map_err(|error| format!("Pilot Plugin workspace is unavailable: {error}"))?;
    let policy = BubblewrapPolicy::new(
        bwrap.to_path_buf(),
        commands
            .split(',')
            .filter(|command| !command.is_empty())
            .map(str::to_owned),
    )
    .authorize_workspace_root(&workspace)
    .authorize_readonly_destination_root("/plugins")
    .authorize_readonly_destination_root("/in/pilot")
    .allow_network();
    let sandbox = BubblewrapSandbox::new(environment.authorize(policy))
        .map_err(|error| format!("Pilot Plugin sandbox is unavailable: {error}"))?;
    Ok((sandbox, workspace))
}

/// The Plugin tool ports one Pilot Turn froze, and why a mounted Plugin is absent.
///
/// The Turn's Session switches are frozen here, once, against the new Turn id: a
/// click made *during* this Turn cannot change what this Turn may use, and the
/// next Turn sees it. `required`/`default` do not apply — the Pilot's own Turn
/// has no Graph snapshot of its own, so a Plugin reaches it through the Session's
/// initial Host policy or a later human mount.
///
/// A Plugin that is not installed, whose view is not usable, or whose MCP server
/// cannot bind is absent and named in `reasons`. Nothing here installs a Plugin,
/// builds or repairs a view, or fetches a package.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn frozen_turn_tools(
    bwrap: &Path,
    tool_library: Option<&Path>,
    data_root: &Path,
    library_root: &Path,
    workspace_root: &Path,
    owner: &str,
    session: &str,
    turn: &str,
    cancellation: anchor_runtime::Cancellation,
    runtime: Option<&crate::runtime_configuration::RuntimeSnapshot>,
    input_mounts: &[anchor_runtime::ReadOnlyInput],
) -> Result<PreparedPilotPlugins, String> {
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        return Err("Pilot tool startup was cancelled".into());
    }
    let source = crate::plugin_mounts::SessionMounts::open(data_root)?;
    let frozen = crate::node_host::freeze_session_switches(data_root, session, turn, &source)?;
    let fixture = tool_host::PluginToolHost::new(fake_plugin_ids());
    let selection_access = crate::dependency_provision::DependencyAccess {
        network: false,
        cancellation: cancellation.clone(),
    };
    let selection_lease = if frozen.switches.is_empty() {
        None
    } else {
        Some(
            crate::plugin_library::publication_lease(
                library_root,
                &selection_access,
                "Pilot mount selection",
            )
            .await
            .map_err(|error| format!("Pilot Plugin mount selection is unavailable: {error:?}"))?,
        )
    };
    let (mut bindings, mut reasons) =
        crate::node_host::session_plugin_mounts(library_root, &fixture, &frozen.switches)?;
    let network = crate::plugin_network::apply(
        library_root,
        &mut bindings,
        frozen.snapshot.network_allowed,
        &mut reasons,
    )?;
    drop(selection_lease);
    if frozen.snapshot_unreadable {
        reasons.push(
            "no Plugin mount snapshot could be read; the Session's current switches were used"
                .to_owned(),
        );
    }
    let environment = crate::tool_environment::ToolEnvironment::for_library(tool_library)?;
    let environment = match runtime {
        Some(snapshot) => environment.with_runtime_snapshot(snapshot)?,
        None => environment,
    };
    // Only a stdio server needs the deployment sandbox: an HTTP server connects
    // from the host, and the in-process fixture needs no process at all.
    let mut stdio = None;
    let mut stdio_checked = false;
    let access = crate::dependency_provision::DependencyAccess {
        network,
        cancellation: cancellation.clone(),
    };
    let mut ports = Vec::new();
    let mut disclosure = crate::plugin_disclosure::PluginDisclosureCatalog::default();
    for binding in &bindings {
        if cancellation.load(std::sync::atomic::Ordering::Acquire) {
            break;
        }
        let prepared = match crate::plugin_library::tools::prepare_plugins_for_owner_async(
            vec![binding.clone()],
            Some(library_root.to_path_buf()),
            fixture.clone(),
            cancellation.clone(),
            Some((
                data_root.to_path_buf(),
                crate::credential_store::binding_owner(owner),
            )),
        )
        .await
        {
            Ok(prepared) => prepared,
            Err(error) => {
                reasons.push(error.to_string());
                continue;
            }
        };
        let binding = &prepared.bindings[0];
        disclosure.extend(prepared.disclosure);
        let single = std::slice::from_ref(binding);
        let mut mcp = prepared.mcp;
        mcp.environment = environment.clone();
        mcp.startup_cancellation = Some(cancellation.clone());
        let declares_stdio = mcp.declares_stdio(binding);
        if declares_stdio && !stdio_checked {
            stdio_checked = true;
            match pilot_plugin_sandbox_with_environment(
                bwrap,
                workspace_root,
                owner,
                session,
                &environment,
            ) {
                Ok(pair) => stdio = Some(pair),
                Err(reason) => reasons.push(reason),
            }
        }
        let oauth = tokio::select! {
            result = mcp.authorize_oauth(Some(crate::credential_store::binding_owner(owner)), network) => result,
            _ = access.cancelled() => break,
        };
        if let Err(reason) = oauth {
            reasons.push(reason);
            continue;
        }
        let unavailable = |reason: String| {
            format!(
                "Plugin `{}` MCP tools are unavailable: {reason}",
                binding.id
            )
        };
        let assembled = if crate::node_host::is_fake_fixture(binding) {
            crate::plugin_library::tools::connect(
                &fixture,
                &mcp,
                single,
                network,
                None,
                &mut disclosure,
            )
            .await
        } else if declares_stdio {
            let Some((sandbox, workspace)) = &stdio else {
                // The sandbox reason is already recorded; a stdio Plugin never
                // runs outside the deployment's sandbox.
                continue;
            };
            let mut readonly = prepared.mounts.clone();
            readonly.extend_from_slice(input_mounts);
            let sandbox = sandbox
                .with_readonly_grants(input_mounts)
                .map_err(|error| error.to_string())?;
            crate::plugin_library::tools::connect(
                &fixture,
                &mcp,
                single,
                network,
                Some(tool_host::SandboxLaunch {
                    sandbox: &sandbox,
                    workspace,
                    readonly_inputs: &readonly,
                    plugin_views: &prepared.views,
                }),
                &mut disclosure,
            )
            .await
        } else {
            crate::plugin_library::tools::connect(
                &fixture,
                &mcp,
                single,
                network,
                None,
                &mut disclosure,
            )
            .await
        };
        match assembled {
            Ok(port) => {
                ports.push(port);
            }
            Err(reason) => reasons.push(unavailable(reason.to_string())),
        }
    }
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        let mut reason = "Pilot tool startup was cancelled".to_owned();
        for port in ports {
            match Arc::try_unwrap(port) {
                Ok(port) => {
                    if let Err(cleanup) = port.shutdown().await {
                        reason.push_str(&format!("; cleanup failed: {cleanup}"));
                    }
                }
                Err(_) => reason.push_str("; MCP port still has an active reference"),
            }
        }
        return Err(reason);
    }
    Ok(PreparedPilotPlugins {
        ports,
        reasons,
        network,
        disclosure,
    })
}

pub(crate) struct PreparedPilotPlugins {
    pub(crate) ports: Vec<Arc<tool_host::CombinedPluginTools>>,
    pub(crate) reasons: Vec<String>,
    pub(crate) network: bool,
    pub(crate) disclosure: crate::plugin_disclosure::PluginDisclosureCatalog,
}

#[cfg(test)]
async fn frozen_plugin_tools(
    data_root: &Path,
    library_root: &Path,
    workspace_root: &Path,
    owner: &str,
    session: &str,
    turn: &str,
) -> Result<(Vec<Arc<dyn ToolPort>>, Vec<String>), String> {
    frozen_turn_tools(
        Path::new("bwrap"),
        Some(library_root),
        data_root,
        library_root,
        workspace_root,
        owner,
        session,
        turn,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
        None,
        &[],
    )
    .await
    .map(|prepared| {
        (
            prepared
                .ports
                .into_iter()
                .map(|port| port as Arc<dyn ToolPort>)
                .collect(),
            prepared.reasons,
        )
    })
}

/// Host assembly for a single frozen Turn. It owns no model history or planning.
pub(crate) struct PilotTurnContext {
    tool_library: Option<PathBuf>,
    pub(crate) workspace: PathBuf,
    pub(crate) sandbox: Arc<BubblewrapSandbox>,
    pub(crate) network: bool,
    pub(crate) cancellation: anchor_runtime::Cancellation,
    pub(crate) commands: Arc<dyn ToolPort>,
    environment: crate::tool_environment::ToolEnvironment,
    readonly_inputs: Vec<anchor_runtime::ReadOnlyInput>,
}

impl PilotTurnContext {
    pub(crate) fn rust_status(&self) -> Value {
        self.environment.pilot_rust_status(self.network)
    }
    pub(crate) fn python_status(&self) -> Value {
        self.environment.pilot_python_status()
    }
    fn platform_status(&self, application: &RunApplication) -> Value {
        self.environment
            .pilot_platform_status(self.network, application.pilot_dependency_status())
    }
    pub(crate) fn new(
        bwrap: &Path,
        tool_library: Option<&Path>,
        workspace_root: &Path,
        owner: &str,
        session: &str,
        network: bool,
        cancellation: anchor_runtime::Cancellation,
    ) -> Result<Self, String> {
        let (sandbox, workspace) = pilot_plugin_sandbox_with_environment(
            bwrap,
            workspace_root,
            owner,
            session,
            &crate::tool_environment::ToolEnvironment::for_library(tool_library)?,
        )?;
        let sandbox = Arc::new(sandbox);
        // This empty Plugin port refuses unknown names; it never points back at
        // PilotPort, so dispatch cannot recurse into the observing adapter.
        let empty = tool_host::PluginToolHost::new(Vec::<String>::new())
            .for_bindings(&[])
            .map_err(|error| error.to_string())?;
        let environment = crate::tool_environment::ToolEnvironment::for_library(tool_library)?
            .with_pilot_rust()?
            .with_pilot_python()?;
        let commands = crate::node_tools::NodeTools::new(
            Arc::new(empty),
            sandbox.clone(),
            workspace.clone(),
            Vec::new(),
            cancellation.clone(),
        )
        .with_network(network)
        .with_timeout_ceiling(std::time::Duration::from_secs(600))
        .with_environment(environment.clone());
        Ok(Self {
            tool_library: tool_library.map(Path::to_path_buf),
            workspace,
            sandbox,
            network,
            cancellation,
            commands: Arc::new(commands),
            environment,
            readonly_inputs: Vec::new(),
        })
    }

    pub(crate) fn set_readonly_inputs(
        &mut self,
        inputs: Vec<anchor_runtime::ReadOnlyInput>,
    ) -> Result<(), String> {
        self.sandbox = Arc::new(
            self.sandbox
                .with_readonly_grants(&inputs)
                .map_err(|error| error.to_string())?,
        );
        self.readonly_inputs = inputs;
        Ok(())
    }

    pub(crate) fn freeze_network(&mut self, network: bool) {
        self.network = network;
        let empty = tool_host::PluginToolHost::new(Vec::<String>::new())
            .for_bindings(&[])
            .expect("empty Plugin tool port");
        self.commands = Arc::new(
            crate::node_tools::NodeTools::new(
                Arc::new(empty),
                self.sandbox.clone(),
                self.workspace.clone(),
                self.readonly_inputs.clone(),
                self.cancellation.clone(),
            )
            .with_network(network)
            .with_timeout_ceiling(std::time::Duration::from_secs(600))
            .with_environment(self.environment.clone()),
        );
    }

    pub(crate) fn configure(
        &mut self,
        snapshot: &crate::runtime_configuration::RuntimeSnapshot,
        library: &Path,
        scope: &Path,
    ) -> Result<(), String> {
        self.environment =
            crate::tool_environment::ToolEnvironment::for_library(self.tool_library.as_deref())?
                .with_runtime_snapshot(snapshot)?;
        let empty = tool_host::PluginToolHost::new(Vec::<String>::new())
            .for_bindings(&[])
            .map_err(|error| error.to_string())?;
        self.commands = Arc::new(
            crate::node_tools::NodeTools::new(
                Arc::new(empty),
                self.sandbox.clone(),
                self.workspace.clone(),
                self.readonly_inputs.clone(),
                self.cancellation.clone(),
            )
            .with_network(self.network)
            .with_timeout_ceiling(std::time::Duration::from_secs(600))
            .with_environment(self.environment.clone())
            .with_development_projects(library.to_path_buf(), scope.to_path_buf()),
        );
        Ok(())
    }

    pub(crate) fn dependency_access(&self) -> crate::dependency_provision::DependencyAccess {
        crate::dependency_provision::DependencyAccess {
            network: self.network,
            cancellation: self.cancellation.clone(),
        }
    }
}

/// A tool that takes no arguments.
fn object() -> Value {
    json!({"type":"object","properties":{},"additionalProperties":false})
}

/// A tool that takes exactly one required string argument.
fn single(field: &str) -> Value {
    json!({"type":"object","properties":{field:{"type":"string"}},"required":[field],"additionalProperties":false})
}

/// How a Pilot tool may be used.
///
/// `Inspect` tools only read. `Mutate` tools change something and are dispatched
/// to `PilotTools` directly — today without a native form, which is recorded
/// debt for `graph_create`/`graph_update`/`graph_run`/`run_pause`/`run_resume`/
/// `run_stop`. `Confirm` tools are owned by the interaction layer, which shows a
/// native form the model cannot fill in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ToolAuthority {
    Inspect,
    Mutate,
    Confirm,
}

/// What calling a tool can touch.
///
/// `ToolAuthority` says *who may call it*; this says *what it can affect*, so
/// "only assets need a human click" is a property of the table rather than a
/// sentence in a document.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ToolEffect {
    /// Reads only.
    Read,
    /// Writes a host-side fact a human consumes (a request, a question).
    HostFact,
    /// Changes a saved asset: a Graph or a Plugin.
    Asset,
    /// Creates an execution.
    Execution,
    /// Controls something already running.
    Control,
}

pub(crate) struct PilotToolSpec {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) parameters: fn() -> Value,
    pub(crate) authority: ToolAuthority,
    pub(crate) effect: ToolEffect,
}

/// The single source of truth for the Pilot tool surface: definitions, the
/// read-only flag and which layer dispatches a call all derive from this table,
/// so a new tool cannot be added to one list and forgotten in another.
pub(crate) const PILOT_TOOLS: &[PilotToolSpec] = &[
    PilotToolSpec {
        name: "publish_file",
        description: "Publish an immutable delivery snapshot of an existing regular file in the Pilot workspace. path is workspace-relative; optional name changes the download filename. Returns an owner-authenticated attachment reference and displays it in this Turn. Use this for generated images and ordinary files; never invent download links.",
        parameters: || json!({"type":"object","properties":{"path":{"type":"string"},"name":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "channel_status",
        description: "Read the bot of one persistent assistant Graph, or every bot when graph is omitted: members, missing credentials and actual Gateway readiness, without secret values.",
        parameters: || json!({"type":"object","properties":{"graph":{"type":"string"}},"additionalProperties":false}),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "channel_configure",
        description: "Configure the bot of a persistent assistant Graph whose Agent node mounts the channel Plugin: its authorized incoming/outgoing users, after native user confirmation. Each Graph is its own bot. Credentials use credential_request with server channel_<platform> and the same graph.",
        parameters: || json!({"type":"object","properties":{"graph":{"type":"string"},"users":{"type":"array","items":{"type":"string"}},"send_users":{"type":"array","items":{"type":"string"}}},"required":["graph","users","send_users"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "channel_enable",
        description: "Start or stop one Graph's bot Gateway through the Host supervisor after native user confirmation. Enabling an already enabled bot repairs missing or unavailable supervision; healthy, connecting and reconnecting bots keep their existing connection. Connecting/authenticating/reconnecting mean work is in progress; authenticated confirms readiness. restart_required means an enable request is needed after checking resources. Missing credentials must be configured first; acceptance is not external connection proof.",
        parameters: || json!({"type":"object","properties":{"graph":{"type":"string"},"enabled":{"type":"boolean"}},"required":["graph","enabled"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "schedule_list",
        description: "List saved Graph schedules and their actual enabled/next occurrence facts.",
        parameters: object,
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "schedule_create",
        description: "Create the Graph schedule the user requested. Uses the same Host Scheduler and supported once/interval/daily/weekly/monthly rules as the platform. The Graph must already exist; acceptance is not proof of a future execution.",
        parameters: || json!({"type":"object","properties":{"graph":{"type":"string"},"rule":{"type":"object"},"input":{"type":"object"},"request_id":{"type":"string","minLength":1,"maxLength":128}},"required":["graph","rule"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "schedule_update",
        description: "Edit, pause or re-enable an existing Graph schedule. Preserves its ID and previous Run facts. Changing timing or re-enabling schedules only future occurrences; inputs-only edits keep the next occurrence.",
        parameters: || json!({"type":"object","properties":{"schedule":{"type":"string"},"graph":{"type":"string"},"rule":{"type":"object"},"input":{"type":"object"},"enabled":{"type":"boolean"}},"required":["schedule","graph","rule","enabled"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "schedule_delete",
        description: "Delete a saved Graph schedule the user requested to remove; preserves prior Run facts.",
        parameters: || single("schedule"),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "credential_request",
        description: "Request a dedicated secure credential form for an installed Plugin's declared MCP environment field, or for a bot's channel credential (server channel_<platform> with the bot's graph). The user enters the value directly into Anchor; the model never sees it. Waits for configuration or cancellation, returns status only, then safely continues this Session to use the credential. Candidate authorization and OAuth use their own explicit flows; never ask for secrets via ask_user.",
        parameters: || json!({"type":"object","properties":{"plugin":{"type":"string"},"server":{"type":"string"},"graph":{"type":"string"},"field":{"type":"string"},"purpose":{"type":"string"}},"required":["plugin","server","field"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "oauth_status",
        description: "Read this owner's installed Plugin OAuth authorization metadata without token values.",
        parameters: || json!({"type":"object","properties":{"plugin":{"type":"string"},"server":{"type":"string"}},"required":["plugin","server"],"additionalProperties":false}),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "oauth_authorize",
        description: "Prepare an owner-bound browser OAuth authorization for an installed Plugin. Show the returned authorization URL to the user; the Host continues this Session after a successful callback.",
        parameters: || json!({"type":"object","properties":{"plugin":{"type":"string"},"server":{"type":"string"}},"required":["plugin","server"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "runtime_prepare",
        description: "Prepare shared Python or Rust with the installed Anchor package's verified native uv/rustup tools. Requires this Turn's task network permission. Explicit deployment-managed fields are read-only. Publishes a probed runtime and safely continues this Session in a new Turn; the current Turn's tools remain frozen.",
        parameters: || json!({"type":"object","properties":{"kind":{"type":"string","enum":["python","rust"]},"version":{"type":"string"}},"required":["kind"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Execution,
    },
    PilotToolSpec {
        name: "development_prepare",
        description: "Ensure a private mutable Python venv with pip for a workspace-relative project (use '.' for workspace root). Default ensure preserves installed trial packages and diagnoses damaged environments. rebuild explicitly clears private packages and creates an empty env; discard releases only this private env, preserving source and legacy/candidate views. Legacy projects keep their read-only view until explicit rebuild succeeds; pip is unavailable there. Optional inline requirements are explicit pip installs, never exact synchronization; installation result is separate from environment readiness. Use anchor_run with the same project for pip, requirements/constraints, editable pyproject/extras, Python, shell and console scripts. Network follows the frozen Turn. Interrupted installs may leave partial changes: inspect with pip list/check and tests, never infer health from old receipts. Formal Plugin dependencies remain separate in plugin.json.",
        parameters: || json!({"type":"object","properties":{"project":{"type":"string"},"action":{"type":"string","enum":["ensure","rebuild","discard"]},"requirements":{"type":"array","items":{"type":"string"}},"development_requirements":{"type":"array","items":{"type":"string"}}},"required":["project"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Execution,
    },
    PilotToolSpec {
        name: "session_network",
        description: "Ask for task network authorization in this Session through a native user confirmation. Enables the installed network Plugin when allowed. The current Turn remains frozen; Anchor safely continues the same Session after it ends. Approval is never a model argument.",
        parameters: || json!({"type":"object","properties":{"allowed":{"type":"boolean"}},"required":["allowed"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "plugin_mount",
        description: "Enable or disable an installed Plugin in this Session after native user confirmation. Installation remains separate. This Turn's tools stay frozen; Anchor safely continues the same Session after it ends.",
        parameters: || json!({"type":"object","properties":{"plugin":{"type":"string"},"on":{"type":"boolean"}},"required":["plugin","on"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::HostFact,
    },
    PilotToolSpec {
        name: "platform_status",
        description: "Inspect the deployment-authorized Python/Rust development and dependency preparation capabilities available to this Turn. Read-only; reports no host paths, package index URLs or secrets, and performs no preparation or mutation.",
        parameters: || object(),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "graph_list",
        description: "List saved Graphs",
        parameters: || object(),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "graph_read",
        description: "Read one saved Graph definition",
        parameters: || single("graph"),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "graph_validate",
        description: "Validate a Graph definition without saving or running it",
        parameters: || json!({"type":"object","properties":{"definition":graphs::definition_parameter()},"required":["definition"],"additionalProperties":false}),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "graph_create",
        description: "Create a saved Graph the user explicitly requested; this does not run it",
        parameters: || json!({"type":"object","properties":{"name":{"type":"string"},"definition":graphs::definition_parameter()},"required":["name","definition"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Asset,
    },
    PilotToolSpec {
        name: "graph_update",
        description: "Update a saved Graph the user explicitly requested; frozen Run resources are preserved",
        parameters: || json!({"type":"object","properties":{"graph":{"type":"string"},"definition":graphs::definition_parameter()},"required":["graph","definition"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Asset,
    },
    PilotToolSpec {
        name: "graph_run",
        description: "Start the Graph the user explicitly requested; returns an accepted Run ID, not completion",
        parameters: || json!({"type":"object","properties":{"graph":{"type":"string"},"objective":{"type":"string"},"input":{}},"required":["graph"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Execution,
    },
    PilotToolSpec {
        name: "plugin_list",
        description: "Search installed Library Plugins with bounded paging or exact plugin ID. Management only: does not mount or grant execution. Use plugin_read for selected overview, instructions, Skills or resources; anchor_plugins lists this Turn execution capabilities.",
        parameters: crate::plugin_disclosure::list_parameters,
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "plugin_read",
        description: "Read one installed Library Plugin section; default overview. Select instructions, skills, skill/path, resources, resource/path or servers. Only selected text is returned, with UTF-8 byte offset/limit, sha256 and next_offset; stop at next_offset=null. Reads do not grant execution. Configuration manifests are private. Legacy resource parameter selects one resource.",
        parameters: || {
            let mut schema = crate::plugin_disclosure::read_parameters();
            schema["properties"]["resource"] = json!({"type":"string"});
            schema
        },
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "plugin_review",
        description: "Review a Plugin candidate directory inside your workspace before any activation: returns its digest, the MCP servers it would run, the environment variable names it needs, and warnings. It installs nothing.",
        parameters: || json!({"type":"object","properties":{"directory":{"type":"string"},"id":{"type":"string"}},"required":["directory"],"additionalProperties":false}),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "plugin_prepare",
        description: "Prepare a Session-workspace Plugin candidate and its Python dependencies before installation. Returns the exact candidate digest, actual locked package set, and whether this deployment granted Pilot a Python interpreter for authoring commands. Use python/python3 with anchor_run when configured; third-party packages still go through dependencies.python. Publishes and mounts nothing. Task network permission is frozen for this Turn.",
        parameters: || json!({"type":"object","properties":{"directory":{"type":"string"},"id":{"type":"string"}},"required":["directory"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Execution,
    },
    PilotToolSpec {
        name: "plugin_test",
        description: "Actually launch a prepared Plugin candidate in the sandbox and list its MCP tools. Optionally pass server, tool and arguments to call one tool; no call means protocol discovery only. Bounded to 120s, cancellable, never installs or adds candidate tools to this Turn. Candidate declarations cannot grant host credentials or network.",
        parameters: || json!({"type":"object","properties":{"directory":{"type":"string"},"id":{"type":"string"},"server":{"type":"string"},"tool":{"type":"string"},"arguments":{"type":"object"}},"required":["directory"],"additionalProperties":false}),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Execution,
    },
    PilotToolSpec {
        name: "run_list",
        description: "List durable Graph Runs",
        parameters: || object(),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "run_status",
        description: "Read one durable Run state",
        parameters: || single("run"),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "run_pause",
        description: "Pause this Run after the current node result is committed, before another node starts; inspect run_status afterwards",
        parameters: || single("run"),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Control,
    },
    PilotToolSpec {
        name: "run_resume",
        description: "Resume an existing Run through the same Runner and frozen resources",
        parameters: || single("run"),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Control,
    },
    PilotToolSpec {
        name: "run_stop",
        description: "Request cancellation of an existing Run; inspect run_status afterwards",
        parameters: || single("run"),
        authority: ToolAuthority::Mutate,
        effect: ToolEffect::Control,
    },
    PilotToolSpec {
        name: "artifact_read",
        description: "Read a committed UTF-8 artifact (at most 1 MiB), not a live workspace",
        parameters: || json!({"type":"object","properties":{"run":{"type":"string"},"node":{"type":"string"},"path":{"type":"string"}},"required":["run","node","path"],"additionalProperties":false}),
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "session_wait",
        description: "Read this Session and its associated Runs without waiting",
        parameters: object,
        authority: ToolAuthority::Inspect,
        effect: ToolEffect::Read,
    },
    PilotToolSpec {
        name: "plugin_install",
        description: "Install a Plugin candidate you prepared inside your own workspace into the Anchor Library. The user approves the exact reviewed content in a native form; approval is never a tool argument.",
        parameters: || json!({"type":"object","properties":{"directory":{"type":"string"},"id":{"type":"string"}},"required":["directory"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::Asset,
    },
    PilotToolSpec {
        name: "plugin_uninstall",
        description: "Uninstall a Plugin from the Anchor Library after a fresh native user confirmation. The user approves deleting the Plugin and its only copy of its data in a native form; approval is never a tool argument. Refused while any Session mounts it or an unfinished Run binds it.",
        parameters: || json!({"type":"object","properties":{"plugin":{"type":"string"}},"required":["plugin"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::Asset,
    },
    PilotToolSpec {
        name: "graph_delete",
        description: "Delete a saved Graph only after a fresh native user confirmation of its exact current contents.",
        parameters: || json!({"type":"object","properties":{"graph":{"type":"string"}},"required":["graph"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::Asset,
    },
    PilotToolSpec {
        name: "ask_user",
        description: "Ask the user for necessary information using a native form; the current Turn waits for their answer.",
        parameters: || json!({"type":"object","properties":{"message":{"type":"string"},"requested_schema":{"type":"object"}},"required":["message","requested_schema"],"additionalProperties":false}),
        authority: ToolAuthority::Confirm,
        effect: ToolEffect::HostFact,
    },
];

/// The authority recorded for a tool name, or `None` for an unknown tool.
pub(crate) fn authority(name: &str) -> Option<ToolAuthority> {
    PILOT_TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .map(|tool| tool.authority)
}

/// What calling this tool can affect, or `None` for an unknown tool.
pub(crate) fn effect(name: &str) -> Option<ToolEffect> {
    PILOT_TOOLS
        .iter()
        .find(|tool| tool.name == name)
        .map(|tool| tool.effect)
}

#[derive(Clone)]
pub(crate) struct PilotTools {
    pub(crate) application: RunApplication,
    pub(crate) graph_name: String,
    pub(crate) library_root: PathBuf,
    pub(crate) data_root: PathBuf,
    pub(crate) workspace_root: PathBuf,
    pub(crate) sessions: SessionStore,
    pub(crate) owner: String,
    pub(crate) session: String,
    pub(crate) turn: String,
    pub(crate) context: Option<Arc<PilotTurnContext>>,
}

fn parameter(arguments: &Value, name: &str) -> Result<String, ToolError> {
    arguments
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ToolError::Failed(format!("{name} must be a string")))
}

fn failed() -> ToolError {
    ToolError::Failed("resource unavailable or malformed".into())
}

/// A refusal whose reason the assistant is allowed to read and act on.
fn rejected(reason: impl Into<String>) -> ToolError {
    ToolError::Failed(reason.into())
}

/// Resolve a candidate directory *inside* the assistant's own workspace.
///
/// The model never names a host path: it names a workspace-relative directory,
/// and this resolves and confines it. Symlinks that leave the workspace, `..`,
/// and absolute paths are refused before any review work happens.
pub(crate) fn workspace_directory(workspace: &Path, relative: &str) -> Result<PathBuf, ToolError> {
    let path = Path::new(relative);
    if relative.is_empty()
        || relative.contains('\0')
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(rejected(
            "directory must be a workspace-relative path without `.` or `..` components",
        ));
    }
    let root = workspace
        .canonicalize()
        .map_err(|_| rejected("the assistant workspace is unavailable"))?;
    let candidate = root
        .join(path)
        .canonicalize()
        .map_err(|_| rejected("the candidate directory does not exist"))?;
    if !candidate.starts_with(&root) || !candidate.is_dir() {
        return Err(rejected("the candidate directory escapes the workspace"));
    }
    Ok(candidate)
}

fn text_resource(file: impl Read) -> Result<String, ToolError> {
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failed())?;
    if bytes.len() > 1024 * 1024 {
        return Err(ToolError::Failed(
            "text resource exceeds 1 MiB preview limit".into(),
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| ToolError::Failed("binary artifact cannot be decoded as text".into()))
}

impl PilotTools {
    pub(crate) fn task_workspace(&self) -> PathBuf {
        self.context
            .as_ref()
            .map(|context| context.workspace.clone())
            .unwrap_or_else(|| {
                crate::pilot_resources::session_workspace(
                    &self.workspace_root,
                    &self.owner,
                    &self.session,
                )
            })
    }

    pub(crate) fn preparation_root(&self) -> Result<PathBuf, ToolError> {
        let session = self
            .sessions
            .get(&self.owner, &self.session)
            .map_err(|_| failed())?;
        Ok(crate::pilot_host::scope(&self.data_root, &self.owner, &session).join("candidates"))
    }

    pub(crate) fn for_turn(mut self, turn: &str) -> Self {
        self.turn = turn.into();
        self
    }

    fn require_active_turn(&self) -> Result<(), ToolError> {
        let turn = self
            .sessions
            .get_turn(&self.owner, &self.session, &self.turn)
            .map_err(|_| failed())?;
        if turn.status != anchor_platform_session::TurnStatus::Running {
            return Err(ToolError::Failed("Pilot Turn is not active".into()));
        }
        Ok(())
    }

    /// Refuse to rewrite the Graph that defines this assistant.
    ///
    /// The Graph carries the assistant's role, instructions, plugin bindings and
    /// therefore its own capabilities. Letting the assistant edit it would be a
    /// self-escalation path, so the operator interface stays the only way to
    /// change it.
    pub(crate) fn refuse_self_graph(&self, graph: &str) -> Result<(), ToolError> {
        if graph == self.graph_name {
            return Err(rejected(format!(
                "`{graph}` defines this assistant; change it through the operator interface, not from inside the assistant"
            )));
        }
        Ok(())
    }

    async fn execute(&self, name: &str, arguments: Value) -> Result<Value, ToolError> {
        match name {
            "publish_file" => {
                self.require_active_turn()?;
                if self.context.as_ref().is_some_and(|context| {
                    context
                        .cancellation
                        .load(std::sync::atomic::Ordering::Acquire)
                }) {
                    return Err(rejected("Pilot was cancelled"));
                }
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Publish {
                    path: String,
                    name: Option<String>,
                }
                let args: Publish = serde_json::from_value(arguments)
                    .map_err(|_| rejected("publish_file requires path and optional name"))?;
                let attachment = crate::pilot_attachments::publish_workspace_file(
                    &self.data_root,
                    &self.sessions,
                    &self.owner,
                    &self.session,
                    &self.turn,
                    &self.task_workspace(),
                    &args.path,
                    args.name.as_deref(),
                )
                .map_err(rejected)?;
                Ok(json!(attachment))
            }
            "runtime_prepare" => self.runtime_prepare(&arguments).await,
            "development_prepare" => self.development_prepare(&arguments).await,
            "credential_request" => self.credential_request(&arguments).await,
            "oauth_status" => self.oauth_status(&arguments),
            "oauth_authorize" => self.oauth_authorize(&arguments).await,
            "channel_status" => self.channel_status(&arguments).await,
            "schedule_list" => self.schedule_list(&arguments).await,
            "schedule_create" => self.schedule_create(&arguments).await,
            "schedule_update" => self.schedule_update(&arguments).await,
            "schedule_delete" => self.schedule_delete(&arguments).await,
            "platform_status" => {
                let mut value = self
                    .context
                    .as_ref()
                    .map(|context| context.platform_status(&self.application))
                    .unwrap_or_else(|| {
                        crate::tool_environment::ToolEnvironment::default().pilot_platform_status(
                            false,
                            self.application.pilot_dependency_status(),
                        )
                    });
                if let Some(configuration) = self.application.runtime_configuration() {
                    value["initialization"] = configuration.status();
                }
                Ok(value)
            }
            "graph_create" => self.graph_create(&arguments).await,
            "graph_update" => self.graph_update(&arguments).await,
            "graph_run" => self.graph_run(&arguments).await,
            "run_pause" | "run_resume" | "run_stop" => self.run_control(name, &arguments).await,
            "graph_list" => self.graph_list(&arguments).await,
            "graph_read" => self.graph_read(&arguments).await,
            "graph_validate" => self.graph_validate(&arguments).await,
            "plugin_list" => self.plugin_list(&arguments).await,
            "plugin_read" => self.plugin_read(&arguments).await,
            "plugin_review" => self.plugin_review(&arguments).await,
            "plugin_prepare" | "plugin_test" => self.plugin_candidate(name, &arguments).await,
            "run_list" => self.run_list(&arguments).await,
            "run_status" => self.run_status(&arguments).await,
            "artifact_read" => self.artifact_read(&arguments).await,
            "session_wait" => self.session_wait(&arguments).await,
            _ => Err(ToolError::Unknown(name.into())),
        }
    }
}

impl ToolPort for PilotTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        // Confirm tools are owned by the interaction layer, which advertises them
        // with its own definitions; this layer dispatches only Inspect and Mutate.
        let mut definitions: Vec<_> = PILOT_TOOLS
            .iter()
            .filter(|tool| tool.authority != ToolAuthority::Confirm)
            .map(|tool| ToolDefinition {
                name: tool.name.into(),
                description: tool.description.into(),
                parameters: (tool.parameters)(),
            })
            .collect();
        if let Some(context) = &self.context {
            definitions.extend(context.commands.definitions());
        }
        definitions
    }

    fn is_read_only(&self, name: &str) -> bool {
        effect(name) == Some(ToolEffect::Read) || name == crate::tool_names::READ_TOOL_NAME
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(context) = &self.context
                && context
                    .commands
                    .definitions()
                    .iter()
                    .any(|tool| tool.name == name)
            {
                self.require_active_turn()?;
                if context
                    .cancellation
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    return Err(rejected("Pilot was cancelled"));
                }
                return context.commands.call(name, arguments).await;
            }
            let result = self.execute(name, arguments).await?;
            Ok(vec![ToolResultContent::text(result.to_string())])
        })
    }
}

#[cfg(test)]
mod tests;

/// The Session-mount freezing the Pilot's own Turn goes through.
///
/// These exercise the entry point `PilotService::admit` calls, then the same two
/// functions `PilotPort` uses to build its tool list and route a call, so a
/// Plugin that appears here appears in the Pilot's tools.
#[cfg(test)]
mod frozen_mounts_tests {
    use super::*;
    use crate::goose_acp::pilot::{plugin_definitions, plugin_port};
    use anchor_platform_session::CreateSession;
    use serde_json::json;

    use crate::test_support::EnvVar;

    struct Fixture {
        _root: tempfile::TempDir,
        root: PathBuf,
        state: PathBuf,
        /// The deployment's workspace root: what a node call authorizes, and what
        /// a Pilot Turn's Plugin sandbox is built from.
        workspace_root: PathBuf,
        sessions: SessionStore,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let state = root.path().join("state");
            std::fs::create_dir_all(state.join("platform")).unwrap();
            let sessions = SessionStore::open(state.join("platform/sessions.sqlite")).unwrap();
            sessions
                .create(
                    "local",
                    CreateSession {
                        id: Some("sess".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
            let path = root.path().to_path_buf();
            std::fs::create_dir_all(path.join("plugins/network")).unwrap();
            std::fs::write(
                path.join("plugins/network/plugin.json"),
                json!({"name":"联网","network":true}).to_string(),
            )
            .unwrap();
            let workspace_root = path.join("workspaces");
            std::fs::create_dir_all(&workspace_root).unwrap();
            Self {
                _root: root,
                root: path,
                state,
                workspace_root,
                sessions,
            }
        }

        fn switch(&self, plugin: &str, on: bool) {
            self.sessions
                .set_plugin_mount("local", "sess", plugin, on, "operator", "the Session view")
                .unwrap();
        }

        /// The built-in Pilot tool list, exactly as `PilotTools` builds it.
        fn base_definitions(&self) -> Vec<ToolDefinition> {
            PilotTools {
                application: RunApplication::new(self.state.clone(), self.root.clone()),
                graph_name: "pilot".into(),
                library_root: self.root.clone(),
                data_root: self.state.clone(),
                workspace_root: self.root.join("workspaces"),
                sessions: self.sessions.clone(),
                owner: "local".into(),
                session: "sess".into(),
                turn: String::new(),
                context: None,
            }
            .definitions()
        }

        async fn freeze(&self, turn: &str) -> (Vec<std::sync::Arc<dyn ToolPort>>, Vec<String>) {
            frozen_plugin_tools(
                &self.state,
                &self.root,
                &self.workspace_root,
                "local",
                "sess",
                turn,
            )
            .await
            .unwrap()
        }
    }

    fn names(definitions: &[ToolDefinition]) -> Vec<String> {
        definitions
            .iter()
            .map(|definition| definition.name.to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_session_mounted_plugin_reaches_the_pilots_tool_list() {
        let _env = crate::PROCESS_ENV_LOCK.lock().await;
        let _fake = EnvVar::set("ANCHOR_RUNNER_FAKE_PLUGINS", "fake-tools");
        let fixture = Fixture::new();
        // Nothing is mounted yet: the Pilot's list is the built-in tools only.
        let (ports, reasons) = fixture.freeze("turn-1").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        let before = names(&plugin_definitions(fixture.base_definitions(), &ports));
        assert!(before.iter().any(|name| name == "graph_run"));
        assert!(!before.iter().any(|name| name == tool_host::FAKE_ECHO_TOOL));

        // The human mounts an installed Plugin before the next Turn.
        fixture.switch("fake-tools", true);
        let (ports, reasons) = fixture.freeze("turn-2").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        let after = names(&plugin_definitions(fixture.base_definitions(), &ports));
        assert!(
            after.iter().any(|name| name == tool_host::FAKE_ECHO_TOOL),
            "{after:?}"
        );
        // The built-in control tools are not a switchable capability.
        assert!(after.iter().any(|name| name == "graph_run"));
        assert!(!after.iter().any(|name| name == "plugin_install"));
        // And the mounted Plugin's tool really is callable through the frozen port.
        let port = plugin_port(&ports, tool_host::FAKE_ECHO_TOOL).unwrap();
        let result = port
            .call(tool_host::FAKE_ECHO_TOOL, json!({"value": {"answer": 42}}))
            .await
            .unwrap();
        assert_eq!(result[0].as_json(), Some(&json!({"answer": 42})));
    }

    #[tokio::test]
    async fn a_click_mid_turn_does_not_change_that_turns_pilot_tools_or_a_frozen_retry() {
        let _env = crate::PROCESS_ENV_LOCK.lock().await;
        let _fake = EnvVar::set("ANCHOR_RUNNER_FAKE_PLUGINS", "fake-tools");
        let fixture = Fixture::new();
        fixture.switch("fake-tools", true);
        let (running, _) = fixture.freeze("turn-1").await;
        // The human switches it off while turn-1 is still running.
        fixture.switch("fake-tools", false);
        // The Turn that already started keeps what it froze.
        let running = names(&plugin_definitions(Vec::new(), &running));
        assert!(
            running.iter().any(|name| name == tool_host::FAKE_ECHO_TOOL),
            "{running:?}"
        );
        // A retry of the *same* Turn reads that Turn's snapshot, not the Session.
        let (retry, _) = fixture.freeze("turn-1").await;
        let retry = names(&plugin_definitions(Vec::new(), &retry));
        assert!(
            retry.iter().any(|name| name == tool_host::FAKE_ECHO_TOOL),
            "{retry:?}"
        );
        // The next Turn sees the click.
        let (next, _) = fixture.freeze("turn-2").await;
        let next = names(&plugin_definitions(Vec::new(), &next));
        assert!(
            !next.iter().any(|name| name == tool_host::FAKE_ECHO_TOOL),
            "{next:?}"
        );
    }

    #[tokio::test]
    async fn an_absent_or_unusable_plugin_is_absent_with_a_reason() {
        let _env = crate::PROCESS_ENV_LOCK.lock().await;
        let fixture = Fixture::new();
        // Installed, but its own declaration's view is not there.
        let plugin = fixture.root.join("plugins/needsview");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({"name":"Needs view","dependencies":{"python":["idna==3.7"]}}).to_string(),
        )
        .unwrap();
        fixture.switch("ghost", true);
        fixture.switch("needsview", true);

        let (ports, reasons) = fixture.freeze("turn-1").await;
        assert!(ports.is_empty());
        assert!(
            reasons.iter().any(|reason| reason.contains("`ghost`")
                && reason.contains("not available in this Library")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("`needsview`") && reason.contains("view")),
            "{reasons:?}"
        );
    }
    /// A loopback MCP server, so the decisive test can mount a Plugin that is
    /// really installed in the Library rather than the in-process fixture.
    #[derive(Clone)]
    struct HttpFixtureServer;

    impl rmcp::ServerHandler for HttpFixtureServer {
        fn get_info(&self) -> rmcp::model::ServerInfo {
            rmcp::model::ServerInfo::new(
                rmcp::model::ServerCapabilities::builder()
                    .enable_tools()
                    .build(),
            )
            .with_server_info(rmcp::model::Implementation::new("pilot-mount-fixture", "1"))
        }

        async fn list_tools(
            &self,
            _: Option<rmcp::model::PaginatedRequestParams>,
            _: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::ListToolsResult, rmcp::model::ErrorData> {
            let schema: std::sync::Arc<serde_json::Map<String, Value>> = std::sync::Arc::new(
                serde_json::from_value(json!({
                    "type":"object","properties":{"value":{"type":"string"}},
                    "required":["value"],"additionalProperties":false
                }))
                .unwrap(),
            );
            Ok(rmcp::model::ListToolsResult {
                tools: vec![rmcp::model::Tool::new("echo", "Echo the value.", schema)],
                next_cursor: None,
                meta: None,
            })
        }

        async fn call_tool(
            &self,
            request: rmcp::model::CallToolRequestParams,
            _: rmcp::service::RequestContext<rmcp::RoleServer>,
        ) -> Result<rmcp::model::CallToolResult, rmcp::model::ErrorData> {
            let value = request.arguments.unwrap().get("value").cloned().unwrap();
            let mut result =
                rmcp::model::CallToolResult::success(vec![rmcp::model::ContentBlock::text(
                    value.as_str().unwrap_or_default(),
                )]);
            result.structured_content = Some(value);
            Ok(result)
        }
    }

    struct HttpFixture {
        endpoint: String,
        cancellation: tokio_util::sync::CancellationToken,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.cancellation.cancel();
            self.task.abort();
        }
    }

    async fn http_mcp_fixture() -> HttpFixture {
        let cancellation = tokio_util::sync::CancellationToken::new();
        let service: rmcp::transport::StreamableHttpService<
            HttpFixtureServer,
            rmcp::transport::streamable_http_server::session::local::LocalSessionManager,
        > = rmcp::transport::StreamableHttpService::new(
            || Ok(HttpFixtureServer),
            Default::default(),
            rmcp::transport::StreamableHttpServerConfig::default()
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancellation.child_token()),
        );
        let app = axum::Router::new().nest_service("/mcp", service);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        HttpFixture {
            endpoint,
            cancellation,
            task,
        }
    }

    #[tokio::test]
    async fn an_installed_plugin_with_an_http_server_reaches_the_pilots_tool_list() {
        let _env = crate::PROCESS_ENV_LOCK.lock().await;
        let fixture = Fixture::new();
        let server = http_mcp_fixture().await;
        let plugin = fixture.root.join("plugins/demo");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({"name":"Demo","mcpServers":{"remote":{"type":"http","url":server.endpoint}}})
                .to_string(),
        )
        .unwrap();
        fixture.switch("demo", true);

        let (ports, reasons) = fixture.freeze("turn-1").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        let names = names(&plugin_definitions(fixture.base_definitions(), &ports));
        assert!(
            names.iter().any(|name| name == "demo-remote_echo"),
            "{names:?}"
        );
        let port = plugin_port(&ports, "demo-remote_echo").unwrap();
        let result = port
            .call("demo-remote_echo", json!({"value":"mounted"}))
            .await
            .unwrap();
        assert_eq!(result[0].as_json(), Some(&json!("mounted")));
    }

    #[tokio::test]
    async fn network_denial_excludes_host_http_mcp_and_is_frozen_for_the_turn() {
        let _env = crate::PROCESS_ENV_LOCK.lock().await;
        let fixture = Fixture::new();
        let server = http_mcp_fixture().await;
        let plugin = fixture.root.join("plugins/remote");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::write(
            plugin.join("plugin.json"),
            json!({"name":"Remote",
            "mcpServers":{"api":{"type":"http","url":server.endpoint}}})
            .to_string(),
        )
        .unwrap();
        fixture.switch("remote", true);
        let (ports, reasons) = fixture.freeze("allowed-turn").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        assert!(plugin_port(&ports, "remote-api_echo").is_some());
        fixture
            .sessions
            .set_network_allowed("local", "sess", false, "operator")
            .unwrap();
        let (retry, _) = fixture.freeze("allowed-turn").await;
        assert!(plugin_port(&retry, "remote-api_echo").is_some());
        let (denied, reasons) = fixture.freeze("denied-turn").await;
        assert!(plugin_port(&denied, "remote-api_echo").is_none());
        assert!(
            reasons.iter().any(|reason| reason.contains("禁止联网")),
            "{reasons:?}"
        );
        fixture
            .sessions
            .set_network_allowed("local", "sess", true, "operator")
            .unwrap();
        let (retry, _) = fixture.freeze("denied-turn").await;
        assert!(plugin_port(&retry, "remote-api_echo").is_none());
        let (allowed, reasons) = fixture.freeze("next-turn").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        assert!(plugin_port(&allowed, "remote-api_echo").is_some());
    }

    fn network_binary() -> PathBuf {
        let executable = std::env::current_exe().unwrap();
        let profile = executable.parent().unwrap().parent().unwrap();
        let target = profile.parent().unwrap();
        let mut build = std::process::Command::new("cargo");
        build
            .args([
                "build",
                "--offline",
                "-p",
                "anchor-network-tools",
                "--manifest-path",
            ])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .unwrap()
                    .join("Cargo.toml"),
            )
            .env("CARGO_TARGET_DIR", target);
        if profile.file_name().is_some_and(|name| name == "release") {
            build.arg("--release");
        }
        let output = build.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        profile.join("anchor-network-tools")
    }

    #[tokio::test]
    async fn a_real_stdio_client_connects_only_with_the_network_capability_and_denial_does_not_break_offline_tools()
     {
        let _env = crate::PROCESS_ENV_LOCK.lock().await;
        let _commands = EnvVar::set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,cat,printf,true");

        let fixture = Fixture::new();
        let executable = network_binary();
        // The same network client installed as an offline Plugin: it must run,
        // but a connection to the Host's listener must be denied by the kernel.
        for id in ["network", "probe"] {
            let directory = fixture.root.join("plugins").join(id);
            std::fs::create_dir_all(directory.join("bin")).unwrap();
            std::fs::copy(&executable, directory.join("bin/anchor-network-tools")).unwrap();
            std::fs::write(directory.join("plugin.json"), json!({"name":id,"network":id == "network",
                "mcpServers":{"http":{"command":"bin/anchor-network-tools","env":{"ANCHOR_NETWORK_WORKSPACE":"/workspace"}}}}).to_string()).unwrap();
        }
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = hits.clone();
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        socket.connect("192.0.2.1:80").unwrap();
        let local = socket.local_addr().unwrap().ip();
        let listener = tokio::net::TcpListener::bind((local, 0)).await.unwrap();
        let url = format!("http://{}/probe", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/probe",
                    axum::routing::get(move || {
                        let seen = seen.clone();
                        async move {
                            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            "connected"
                        }
                    }),
                ),
            )
            .await
            .unwrap();
        });
        fixture.switch("probe", true);
        let (offline, reasons) = fixture.freeze("offline").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        let port = plugin_port(&offline, "probe-http_request").unwrap();
        assert!(
            port.call("probe-http_request", json!({"url":url}))
                .await
                .is_err()
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        fixture.switch("network", true);
        let (online, reasons) = fixture.freeze("online").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        let port = plugin_port(&online, "network-http_request").unwrap();
        let value = port
            .call("network-http_request", json!({"url":url}))
            .await
            .unwrap();
        assert!(
            value.iter().any(|content| content
                .as_json()
                .is_some_and(|value| value["body"] == "connected")),
            "{value:?}"
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        fixture
            .sessions
            .set_network_allowed("local", "sess", false, "operator")
            .unwrap();
        let (denied, reasons) = fixture.freeze("denied").await;
        assert!(plugin_port(&denied, "network-http_request").is_none());
        assert!(reasons.iter().any(|reason| reason.contains("禁止联网")));
        let port = plugin_port(&denied, "probe-http_request").unwrap();
        assert!(
            port.call("probe-http_request", json!({"url":url}))
                .await
                .is_err()
        );
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        server.abort();
    }
    /// The repository's stdio MCP fixture binary (the same one the node-host
    /// tests use), built once per test process.
    fn stdio_fixture_binary() -> PathBuf {
        static BINARY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        BINARY
            .get_or_init(|| {
                let executable = std::env::current_exe().unwrap();
                let profile = executable.parent().unwrap().parent().unwrap();
                let target = profile.parent().unwrap();
                let mut build = std::process::Command::new("cargo");
                build
                    .args([
                        "build",
                        "--offline",
                        "-p",
                        "anchor-mcp-host",
                        "--example",
                        "http_fixture",
                    ])
                    .arg("--manifest-path")
                    .arg(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .parent()
                            .unwrap()
                            .join("Cargo.toml"),
                    )
                    .env("CARGO_TARGET_DIR", target);
                if profile.file_name().is_some_and(|name| name == "release") {
                    build.arg("--release");
                }
                let output = build.output().unwrap();
                assert!(
                    output.status.success(),
                    "the stdio fixture did not build: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                profile.join("examples/http_fixture")
            })
            .clone()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_installed_stdio_plugin_reaches_the_pilots_tool_list_and_runs() {
        use std::os::unix::fs::PermissionsExt;

        let _env = crate::PROCESS_ENV_LOCK.lock().await;
        // The deployment's own node-call authority: the command allow-list and the
        // workspace root a Plugin's stdio server is launched under.
        let _commands = EnvVar::set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh");
        let fixture = Fixture::new();
        let _workspace = EnvVar::set("ANCHOR_RUNNER_WORKSPACE_ROOT", &fixture.workspace_root);

        let plugin = fixture.root.join("plugins/probe");
        std::fs::create_dir_all(&plugin).unwrap();
        std::fs::copy(stdio_fixture_binary(), plugin.join("mcp-fixture")).unwrap();
        std::fs::write(
            plugin.join("server.sh"),
            "#!/usr/bin/bash\nexec ./mcp-fixture --stdio \"$@\"\n",
        )
        .unwrap();
        for file in ["server.sh", "mcp-fixture"] {
            std::fs::set_permissions(plugin.join(file), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        std::fs::write(
            plugin.join("plugin.json"),
            json!({
                "name":"Stdio probe",
                "mcpServers":{"probe":{"command":"server.sh","args":["echo","--prefix","mounted:"]}}
            })
            .to_string(),
        )
        .unwrap();
        fixture.switch("probe", true);

        let (ports, reasons) = fixture.freeze("turn-1").await;
        assert!(reasons.is_empty(), "{reasons:?}");
        let names = names(&plugin_definitions(fixture.base_definitions(), &ports));
        assert!(
            names.iter().any(|name| name == "probe-probe_echo"),
            "{names:?}"
        );
        // The tool is not just advertised: the sandboxed server really answers.
        let port = plugin_port(&ports, "probe-probe_echo").unwrap();
        let result = port
            .call("probe-probe_echo", json!({"value":"ok"}))
            .await
            .unwrap();
        assert_eq!(result[0].as_text(), Some("mounted:ok"));
        // The server ran inside its session workspace, not somewhere else.
        let log = std::fs::read_to_string(
            fixture
                .workspace_root
                .join("pilot")
                .join(format!(
                    "{:x}",
                    sha2::Sha256::digest(serde_json::to_vec(&("local", "sess")).unwrap())
                ))
                .join("mcp-log"),
        )
        .unwrap();
        assert!(log.contains("tools/call"), "{log:?}");
    }
}
