//! Operator Graph approvals and immutable Plugin policy for assistant Turns.
//!
//! Graph remains the desired configuration. These secret-free facts prove an
//! operator authorized its grants and what a particular Turn actually received.
use crate::application::run_access::RunProgress;
use crate::application::run_access::{self, RunView};
use crate::plugin_library::Catalog;
use crate::run_metadata::RunMetadata;
use anchor_graph_host::PluginCatalog;
#[cfg(test)]
use anchor_runtime::graph::GraphRunRecord;
use anchor_runtime::graph::{GraphError, GraphSnapshot, PluginBinding, PluginRef, RunLease};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

/// Shared with Graph publication/admission: reading desired policy and freezing
/// its grants is serialized with saving the Graph, never with model execution.
pub(crate) fn graph_lease(root: &Path, source: &Path) -> Result<Box<dyn RunLease>, GraphError> {
    run_access::graph_lease(root, source)
}

pub(crate) fn graph_read_lease(
    root: &Path,
    source: &Path,
) -> Result<Box<dyn RunLease>, GraphError> {
    run_access::graph_read_lease(root, source)
}

pub(crate) fn graph_lease_key(source: &Path) -> Result<String, GraphError> {
    let identity = crate::run_metadata::graph_identity(source).map_err(|error| {
        GraphError::Unsupported(format!("Graph identity unavailable: {error:?}"))
    })?;
    Ok(format!(
        "graph-{}",
        hash(identity.as_os_str().as_encoded_bytes())
    ))
}

/// Only the work node's Plugin selection and network policy are hot.
pub(crate) fn compatible(admitted: &GraphSnapshot, current: &GraphSnapshot, work: &str) -> bool {
    let normalize = |snapshot: &GraphSnapshot| {
        let mut snapshot = snapshot.clone();
        if let Some(node) = snapshot.nodes.iter_mut().find(|node| node.id == work) {
            node.plugins.clear();
            if let Some(agent) = node
                .agent
                .as_ref()
                .and_then(|name| snapshot.agents.get_mut(name))
            {
                agent.network = false;
            }
        }
        snapshot
    };
    normalize(admitted) == normalize(current)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Publication {
    format: u32,
    source: PathBuf,
    pub(crate) graph_digest: String,
    pub(crate) bindings: BTreeMap<String, PluginBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) task_graphs: BTreeMap<String, String>,
}

impl Publication {
    pub(crate) fn task_grants(&self) -> crate::task_delegation::GraphTaskGrants {
        crate::task_delegation::GraphTaskGrants {
            bindings: self.bindings.clone(),
            targets: self.task_graphs.clone(),
        }
    }
}

fn publication_path(root: &Path, source: &Path, digest: &str) -> Result<PathBuf, String> {
    let source = crate::run_metadata::graph_identity(source)
        .map_err(|error| format!("Graph identity unavailable: {error:?}"))?;
    Ok(root
        .join("graph-plugin-authorizations")
        .join(hash(source.as_os_str().as_encoded_bytes()))
        .join(format!("{}.json", hash(digest.as_bytes()))))
}

/// Called only by the operator Graph save path while holding its Graph lease.
/// Agent-authored Graph updates must never call this authorization operation.
#[cfg(test)]
pub(crate) fn publish(
    root: &Path,
    source: &Path,
    snapshot: &GraphSnapshot,
    library_root: &Path,
) -> Result<(), String> {
    publish_with_tasks(root, source, snapshot, library_root, &BTreeMap::new())
}

pub(crate) fn publish_with_tasks(
    root: &Path,
    source: &Path,
    snapshot: &GraphSnapshot,
    library_root: &Path,
    tasks: &BTreeMap<String, GraphSnapshot>,
) -> Result<(), String> {
    let _library = crate::plugin_library::PluginLibrary::new(library_root)
        .catalog_read_guard()
        .map_err(|error| error.to_string())?;
    let catalog = Catalog::new(library_root);
    let ids = std::iter::once(snapshot)
        .chain(tasks.values())
        .flat_map(|snapshot| &snapshot.nodes)
        .flat_map(|node| &node.plugins)
        .filter(|plugin| plugin.id != crate::plugin_network::NETWORK_PLUGIN)
        .flat_map(|plugin| {
            std::iter::once(plugin.id.clone()).chain(
                plugin
                    .graph_tasks
                    .iter()
                    .flat_map(|policy| policy.plugins.iter().cloned()),
            )
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut bindings = catalog
        .resolve(&ids)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|binding| (binding.id.clone(), binding))
        .collect::<BTreeMap<_, _>>();
    // A dependency-derived network grant is authorized with the same publication.
    let mut network_required = false;
    for snapshot in std::iter::once(snapshot).chain(tasks.values()) {
        network_required |=
            crate::plugin_network::snapshot_requires_network(library_root, snapshot)?
                || snapshot
                    .nodes
                    .iter()
                    .flat_map(|node| &node.plugins)
                    .filter_map(|plugin| plugin.graph_tasks.as_ref())
                    .any(|policy| {
                        policy.network
                            || policy.plugins.iter().any(|id| {
                                catalog.metadata(id).is_ok_and(|metadata| {
                                    crate::plugin_network::metadata_requires_network(&metadata)
                                })
                            })
                    });
    }
    if network_required {
        let network = catalog
            .resolve(&[crate::plugin_network::NETWORK_PLUGIN.into()])
            .map_err(|error| error.to_string())?
            .pop()
            .ok_or("network system Plugin is unavailable")?;
        bindings.insert(network.id.clone(), network);
    }
    let digest = snapshot.digest().map_err(|error| error.to_string())?;
    let publication = Publication {
        format: 1,
        source: crate::run_metadata::graph_identity(source)
            .map_err(|error| format!("Graph identity unavailable: {error:?}"))?,
        graph_digest: digest.clone(),
        bindings,
        task_graphs: tasks
            .iter()
            .map(|(name, snapshot)| {
                snapshot
                    .digest()
                    .map(|digest| (name.clone(), digest))
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<_, _>>()?,
    };
    if let Some(existing) = self::publication(root, source, &digest)?
        && existing.task_graphs == publication.task_graphs
        && existing.bindings.len() == publication.bindings.len()
        && existing.bindings.iter().all(|(id, frozen)| {
            publication
                .bindings
                .get(id)
                .is_some_and(|current| frozen.same_authorization(current))
        })
    {
        return Ok(());
    }
    // An explicit operator save approves the currently installed declarations,
    // including a previously used Graph digest. Active Turns already own their
    // immutable policies; refreshing this approval only affects future Turns.
    let path = publication_path(root, source, &digest)?;
    let parent = path.parent().ok_or("Graph approval has no parent")?;
    crate::create_durable_directory(parent).map_err(|error| error.to_string())?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    serde_json::to_writer(temporary.as_file_mut(), &publication)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(&path)
        .map_err(|error| error.to_string())?;
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

pub(crate) fn publication(
    root: &Path,
    source: &Path,
    digest: &str,
) -> Result<Option<Publication>, String> {
    let saved: Option<Publication> =
        crate::assistant::read_json(&publication_path(root, source, digest)?)?;
    if let Some(saved) = &saved {
        let source = crate::run_metadata::graph_identity(source)
            .map_err(|error| format!("Graph identity unavailable: {error:?}"))?;
        if saved.format != 1
            || saved.source != source
            || saved.graph_digest != digest
            || saved.bindings.iter().any(|(id, binding)| id != &binding.id)
        {
            return Err("Graph Plugin authorization receipt is corrupt".into());
        }
    }
    Ok(saved)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnPluginPolicy {
    pub(crate) format: u32,
    pub(crate) run: String,
    pub(crate) node: String,
    pub(crate) graph_digest: String,
    pub(crate) mounts: crate::plugin_mounts::MountSnapshot,
    pub(crate) declarations: Vec<PluginRef>,
    pub(crate) bindings: BTreeMap<String, PluginBinding>,
    pub(crate) network: bool,
    pub(crate) reasons: Vec<String>,
    pub(crate) by_declaration: Vec<String>,
    pub(crate) by_switch: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) task_grants: Option<crate::task_delegation::GraphTaskGrants>,
}

pub(crate) fn turn_path(root: &Path, run: &str, session: &str, turn: &str) -> PathBuf {
    root.join("assistant-plugin-turns")
        .join(hash(run.as_bytes()))
        .join(format!(
            "{}.json",
            hash(&serde_json::to_vec(&(session, turn)).expect("identity is serializable"))
        ))
}

fn marker_path(root: &Path, run: &str) -> PathBuf {
    root.join("assistant-plugin-turns")
        .join(hash(run.as_bytes()))
        .join("initialized.json")
}

pub(crate) fn initialized(root: &Path, run: &str) -> Result<bool, String> {
    match crate::assistant::read_json::<String>(&marker_path(root, run))? {
        Some(saved) if saved == run => Ok(true),
        Some(_) => Err("assistant Turn Plugin policy marker is corrupt".into()),
        None => Ok(false),
    }
}

pub(crate) fn mark_initialized(root: &Path, run: &str) -> Result<(), String> {
    crate::assistant::save_immutable(&marker_path(root, run), &run.to_owned())
}

pub(crate) fn load(
    root: &Path,
    run: &str,
    session: &str,
    turn: &str,
) -> Result<Option<TurnPluginPolicy>, String> {
    let saved: Option<TurnPluginPolicy> =
        crate::assistant::read_json(&turn_path(root, run, session, turn))
            .map_err(|error| format!("Turn Plugin policy unavailable: {error}"))?;
    if let Some(saved) = &saved
        && (saved.format != 1
            || !matches!(saved.mounts.format, 1 | 2)
            || saved.run != run
            || saved.mounts.session != session
            || saved.mounts.turn != turn
            || saved.node.is_empty()
            || saved.graph_digest.is_empty()
            || saved.bindings.iter().any(|(id, binding)| id != &binding.id))
    {
        return Err("Turn Plugin policy is corrupt".into());
    }
    Ok(saved)
}

/// A verified Host-only fact selects invocation authority. Generic and legacy
/// Runs keep the Kernel's frozen admission policy.
pub(crate) fn invocation_authority<'record>(
    root: &Path,
    record: impl Into<RunProgress<'record>>,
) -> Result<bool, String> {
    let progress = record.into();
    let Some(metadata) = crate::run_metadata::load(root, progress.run_id)
        .map_err(|error| format!("Run identity unavailable: {error:?}"))?
    else {
        return Ok(false);
    };
    let Some(source) = metadata.assistant else {
        return Ok(false);
    };
    let plan =
        crate::assistant::AssistantPlan::from_snapshot(progress.snapshot, &source.work_node)?
            .ok_or("assistant Graph plan is missing")?;
    if metadata.graph_digest != progress.graph_digest
        || source.owner.is_empty()
        || source.session.is_empty()
        || plan.wait_node != source.wait_node
        || plan.reply_node != source.reply_node
    {
        return Err("assistant authority identity changed".into());
    }
    Ok(true)
}

/// Only the cursor's still executable Turn holds references, never old history.
pub(crate) fn active_bindings<'record>(
    root: &Path,
    record: impl Into<RunView<'record>>,
    metadata: &RunMetadata,
) -> Result<BTreeMap<String, PluginBinding>, String> {
    let view = record.into();
    let progress = view.progress();
    let Some(source) = &metadata.assistant else {
        return Ok(BTreeMap::new());
    };
    let Some(binding) = view.executable_assistant_binding(root, metadata)? else {
        return Ok(BTreeMap::new());
    };
    if let Some(policy) = load(root, progress.run_id, &source.session, &binding.turn)? {
        if policy.node != source.work_node {
            return Err("assistant Turn Plugin node identity changed".into());
        }
        return Ok(policy.bindings);
    }
    if initialized(root, progress.run_id)? {
        return Err("assistant Turn Plugin policy is missing".into());
    }
    // Only pre-upgrade active Turns lack a policy. Retain conservative baseline
    // references until that Turn finishes; idle assistants hold no such grants.
    let bindings = progress.plugin_bindings.clone();
    let path = crate::plugin_mounts::snapshot_path(root, &source.session, &binding.turn);
    if let Some(snapshot) =
        crate::plugin_mounts::read_snapshot_strict(&path, &source.session, &binding.turn)?
    {
        for (id, enabled) in snapshot.mounts {
            if enabled && !bindings.contains_key(&id) {
                return Err(
                    "old assistant Plugin authority format; offline migration required".into(),
                );
            }
        }
    }
    Ok(bindings)
}

/// Delete only parsed facts whose exact owner and hashed filename agree. Open
/// directory descriptors keep cleanup inside that owner even if a path changes.
fn remove_owned_facts(
    root: &Path,
    kind: &str,
    owner: &str,
    validate: impl Fn(&str, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    use rustix::fs::{AtFlags, Mode, OFlags, fsync, open, openat, unlinkat};
    use std::{io::Read, os::fd::AsRawFd};
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let parent = match open(root.join(kind), flags, Mode::empty()) {
        Ok(directory) => directory,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let directory = match openat(&parent, owner, flags, Mode::empty()) {
        Ok(directory) => directory,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let mut names = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))
        .map_err(|error| error.to_string())?
    {
        let name = entry
            .map_err(|error| error.to_string())?
            .file_name()
            .into_string()
            .map_err(|_| "invalid policy fact filename")?;
        let fd = openat(
            &directory,
            name.as_str(),
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|error| error.to_string())?;
        let mut file = std::fs::File::from(fd);
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
            return Err("invalid policy cleanup fact".into());
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        validate(&name, &bytes)?;
        names.push(name);
    }
    for name in names {
        unlinkat(&directory, name.as_str(), AtFlags::empty()).map_err(|error| error.to_string())?;
    }
    fsync(&directory).map_err(|error| error.to_string())?;
    unlinkat(&parent, owner, AtFlags::REMOVEDIR).map_err(|error| error.to_string())?;
    fsync(&parent).map_err(|error| error.to_string())
}

pub(crate) fn cleanup<'record>(
    root: &Path,
    record: impl Into<RunProgress<'record>>,
) -> Result<(), String> {
    let progress = record.into();
    let metadata = crate::run_metadata::load(root, progress.run_id)
        .map_err(|error| format!("policy cleanup identity unavailable: {error:?}"))?;
    remove_owned_facts(
        root,
        "assistant-plugin-turns",
        &hash(progress.run_id.as_bytes()),
        |name, bytes| {
            if name == "initialized.json" {
                let owner: String =
                    serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
                if owner == progress.run_id {
                    return Ok(());
                }
                return Err("policy cleanup marker owner changed".into());
            }
            let policy: TurnPluginPolicy =
                serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
            let source = metadata
                .as_ref()
                .and_then(|metadata| metadata.assistant.as_ref())
                .ok_or("policy cleanup has no assistant owner")?;
            let expected = turn_path(root, progress.run_id, &source.session, &policy.mounts.turn);
            if policy.format != 1
                || policy.run != progress.run_id
                || policy.node != source.work_node
                || policy.mounts.session != source.session
                || expected.file_name().and_then(|name| name.to_str()) != Some(name)
            {
                return Err("policy cleanup owner changed".into());
            }
            Ok(())
        },
    )
}

pub(crate) fn cleanup_publications(root: &Path, source: &Path) -> Result<(), String> {
    let identity = crate::run_metadata::graph_identity(source)
        .map_err(|error| format!("Graph cleanup identity unavailable: {error:?}"))?;
    remove_owned_facts(
        root,
        "graph-plugin-authorizations",
        &hash(identity.as_os_str().as_encoded_bytes()),
        |name, bytes| {
            let publication: Publication =
                serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
            if publication.format != 1
                || publication.source != identity
                || name != format!("{}.json", hash(publication.graph_digest.as_bytes()))
            {
                return Err("Graph authorization cleanup owner changed".into());
            }
            Ok(())
        },
    )
}

#[derive(Debug, Serialize)]
pub(crate) struct PluginUpdateStatus {
    pub(crate) state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
}

/// Current claimed Turn, or the last committed Turn while the assistant waits.
/// This is a display projection of execution facts, never an authorization source.
pub(crate) fn latest_policy<'record>(
    root: &Path,
    record: impl Into<RunView<'record>>,
    metadata: &RunMetadata,
) -> Result<Option<TurnPluginPolicy>, String> {
    let view = record.into();
    let progress = view.progress();
    let source = metadata
        .assistant
        .as_ref()
        .ok_or("Run is not an assistant")?;
    let binding = view.latest_assistant_binding(root)?;
    let policy = binding
        .map(|binding| load(root, progress.run_id, &source.session, &binding.turn))
        .transpose()?
        .flatten();
    if policy
        .as_ref()
        .is_some_and(|policy| policy.node != source.work_node)
    {
        return Err("assistant Turn Plugin node identity changed".into());
    }
    Ok(policy)
}

pub(crate) fn status<'record>(
    root: &Path,
    record: impl Into<RunView<'record>>,
    metadata: &RunMetadata,
    current: &GraphSnapshot,
) -> Result<PluginUpdateStatus, String> {
    let view = record.into();
    let progress = view.progress();
    let source = metadata
        .assistant
        .as_ref()
        .ok_or("Run is not an assistant")?;
    if !compatible(progress.snapshot, current, &source.work_node) {
        return Ok(PluginUpdateStatus {
            state: "restart_required",
            message: Some(
                "Graph structure changed; stop and retire the assistant to apply it".into(),
            ),
        });
    }
    let desired = current.digest().map_err(|error| error.to_string())?;
    if desired != progress.graph_digest
        && publication(root, &metadata.bundle_source, &desired)?.is_none()
    {
        return Ok(PluginUpdateStatus {
            state: "unavailable",
            message: Some("Graph Plugin permissions have no operator authorization".into()),
        });
    }
    let effective = latest_policy(root, &view, metadata)?
        .map(|policy| policy.graph_digest)
        .unwrap_or_else(|| progress.graph_digest.to_owned());
    Ok(PluginUpdateStatus {
        state: if effective == desired {
            "current"
        } else {
            "pending"
        },
        message: None,
    })
}

#[cfg(test)]
mod tests;
