use crate::application::run_access;
use crate::host_fs::invocation_stem as stem;
use crate::run_metadata::{AssistantSource, RunMetadata};
use anchor_platform_session::{ChannelInboundAdmission, SessionStore};
use anchor_runtime::graph::{GraphError, GraphSnapshot, InvocationKey, NodeExecutionRequest};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};
use tokio::sync::Notify;

pub(crate) const WAIT_INPUT: &str = "session.wait_input";
pub(crate) const REPLY: &str = "session.reply";

pub(crate) fn signal(session: &str) -> Arc<Notify> {
    static SIGNALS: OnceLock<Mutex<std::collections::HashMap<String, Weak<Notify>>>> =
        OnceLock::new();
    let mut signals = SIGNALS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    signals.retain(|_, signal| signal.strong_count() > 0);
    if let Some(signal) = signals.get(session).and_then(Weak::upgrade) {
        return signal;
    }
    let signal = Arc::new(Notify::new());
    signals.insert(session.to_owned(), Arc::downgrade(&signal));
    signal
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AssistantPlan {
    pub(crate) wait_node: String,
    pub(crate) work_node: String,
    pub(crate) reply_node: String,
}

impl AssistantPlan {
    /// The plan of a persistent assistant Graph, whose only Agent is the work
    /// node. `None` when the Graph is not an assistant at all.
    pub(crate) fn discover(snapshot: &GraphSnapshot) -> Result<Option<Self>, String> {
        let mut agents = snapshot.nodes.iter().filter(|node| node.agent.is_some());
        match (agents.next(), agents.next()) {
            (Some(agent), None) => Self::from_snapshot(snapshot, &agent.id),
            _ => Self::from_snapshot(snapshot, ""),
        }
    }

    pub(crate) fn from_snapshot(
        snapshot: &GraphSnapshot,
        work_node: &str,
    ) -> Result<Option<Self>, String> {
        let operations = snapshot
            .nodes
            .iter()
            .filter_map(|node| {
                let operation = node
                    .op
                    .as_ref()
                    .and_then(|name| snapshot.ops.get(name))?
                    .get("host")?
                    .get("operation")?
                    .as_str()?;
                Some((node.id.as_str(), operation))
            })
            .collect::<Vec<_>>();
        if operations.is_empty() {
            return Ok(None);
        }
        let waits = operations
            .iter()
            .filter(|(_, operation)| *operation == WAIT_INPUT)
            .collect::<Vec<_>>();
        let replies = operations
            .iter()
            .filter(|(_, operation)| *operation == REPLY)
            .collect::<Vec<_>>();
        if waits.len() != 1
            || replies.len() != 1
            || operations.len() != 2
            || snapshot.nodes.len() != 3
        {
            return Err(
                "a channel assistant requires one input Op, one Agent and one reply Op".into(),
            );
        }
        let plan = Self {
            wait_node: waits[0].0.to_owned(),
            work_node: work_node.to_owned(),
            reply_node: replies[0].0.to_owned(),
        };
        if snapshot.entry != plan.wait_node
            || !snapshot
                .nodes
                .iter()
                .any(|node| node.id == work_node && node.agent.is_some())
        {
            return Err("assistant entry must wait for input and the configured result node must be an Agent".into());
        }
        let expected = [
            (&plan.wait_node, &plan.work_node),
            (&plan.work_node, &plan.reply_node),
            (&plan.reply_node, &plan.wait_node),
        ];
        if snapshot.edges.len() != 3
            || expected.iter().any(|(from, to)| {
                !snapshot
                    .edges
                    .iter()
                    .any(|edge| &edge.from_node == *from && &edge.to_node == *to)
            })
        {
            return Err("assistant nodes must form the input, Agent, reply loop".into());
        }
        if snapshot.nodes.iter().any(|node| node.max_rounds.is_some())
            || !snapshot.module_rounds.is_empty()
        {
            return Err("a persistent assistant cannot have a finite round ceiling".into());
        }
        Ok(Some(plan))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TurnBinding {
    pub(crate) key: InvocationKey,
    pub(crate) wait_key: String,
    pub(crate) inbound: String,
    pub(crate) turn: String,
    #[serde(default)]
    pub(crate) pending_inbounds: Vec<String>,
    #[serde(default)]
    pub(crate) pending_truncated: bool,
    #[serde(default)]
    pub(crate) notification: Option<anchor_platform_session::ChannelNotification>,
}

fn binding_path(root: &Path, key: &InvocationKey) -> PathBuf {
    root.join("assistant-invocations")
        .join(format!("{}.json", stem(key)))
}

pub(crate) fn save_immutable<T: Serialize + PartialEq + for<'de> Deserialize<'de>>(
    path: &Path,
    value: &T,
) -> Result<(), String> {
    let parent = path.parent().ok_or("fact has no parent")?;
    crate::create_durable_directory(parent).map_err(|error| error.to_string())?;
    if let Some(existing) = read_json::<T>(path)? {
        return if &existing == value {
            Ok(())
        } else {
            Err("immutable assistant fact changed".into())
        };
    }
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .write_all(&bytes)
        .map_err(|error| error.to_string())?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    match temporary.persist_noclobber(path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            if read_json::<T>(path)?.as_ref() != Some(value) {
                return Err("immutable assistant fact conflict".into());
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    fs::File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

pub(crate) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>, String> {
    let parent = path.parent().ok_or("fact has no parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid fact filename")?;
    let file = match crate::resource_read::open_resource(parent, name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if file.metadata().map_err(|error| error.to_string())?.len() > 32 * 1024 * 1024 {
        return Err("assistant fact exceeds its size limit".into());
    }
    serde_json::from_reader(file)
        .map(Some)
        .map_err(|error| error.to_string())
}

pub(crate) fn source(root: &Path, key: &InvocationKey) -> Result<Option<AssistantSource>, String> {
    let Some(metadata) =
        crate::run_metadata::load(root, &key.run_id).map_err(|error| format!("{error:?}"))?
    else {
        return Ok(None);
    };
    if metadata.graph_digest != key.graph_digest {
        return Err("assistant Graph identity changed".into());
    }
    Ok(metadata.assistant)
}

pub(crate) fn sessions(root: &Path) -> Result<SessionStore, String> {
    SessionStore::open(root.join("platform/sessions.sqlite")).map_err(|error| error.to_string())
}

/// The stable workspace scene a new assistant Run inherits from the Run it was
/// handed over from.
///
/// The answer comes only from durable host facts: this Run's immutable metadata
/// names the handed-over Run, and the Session's binding table shows that this
/// Run is the current instance and that the named Run was retired into it. The
/// handed-over Run must be the same assistant instance of the same Graph, so a
/// plain conversation predecessor, a user input or a path string can never
/// authorize a resource binding. Only the first preparation of the instance's own work node
/// inherits a scene.
pub(crate) fn handover_predecessor(
    root: &Path,
    key: &InvocationKey,
) -> Result<Option<InvocationKey>, String> {
    let Some(metadata) = crate::run_metadata::load(root, &key.run_id)
        .map_err(|error| format!("assistant metadata: {error:?}"))?
    else {
        return Ok(None);
    };
    let Some(source) = metadata.assistant.as_ref() else {
        return Ok(None);
    };
    if key.invocation != 1
        || key.node_id != source.work_node
        || metadata.graph_digest != key.graph_digest
    {
        return Ok(None);
    }
    let Some(previous) = metadata
        .conversation
        .as_ref()
        .and_then(|conversation| conversation.previous_run.as_deref())
    else {
        return Ok(None);
    };
    if previous == key.run_id {
        return Ok(None);
    }
    let sessions = sessions(root)?;
    let current = sessions
        .get_channel_assistant(&source.owner, &source.session)
        .map_err(|error| error.to_string())?;
    if current.as_ref().is_none_or(|current| {
        current.run_id != key.run_id
            || current.wait_node != source.wait_node
            || current.work_node != key.node_id
            || current.reply_node != source.reply_node
    }) || !sessions
        .retired_channel_assistant(&source.owner, &source.session, previous)
        .map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    let certified = sessions
        .channel_assistant_predecessor(&source.owner, &source.session, &key.run_id)
        .map_err(|error| error.to_string())?;
    if certified
        .as_ref()
        .is_none_or(|binding| binding.run_id != previous)
    {
        return Err("assistant handover predecessor identity changed".into());
    }
    // The handover is real. From here on the facts must agree, so a mismatch is
    // retained corruption instead of a silently skipped binding.
    let Some(prior) = crate::run_metadata::load(root, previous)
        .map_err(|error| format!("assistant handover source: {error:?}"))?
    else {
        // A deleted source cannot be inherited. Deletion refuses to remove a Run
        // while a later instance still has to be handed over from it, so this is
        // a corruption report rather than a routine path.
        if crate::run_deletions::load(root, previous)
            .map_err(|error| format!("assistant handover deletion: {error}"))?
            .is_some()
        {
            return Err(
                "assistant handover source was deleted; this instance cannot inherit its workspace"
                    .into(),
            );
        }
        return Err("assistant handover source has no immutable metadata".into());
    };
    let (Some(prior_assistant), Some(prior_conversation)) =
        (prior.assistant.as_ref(), prior.conversation.as_ref())
    else {
        return Err("assistant handover source is not an assistant instance".into());
    };
    if prior_assistant.owner != source.owner
        || prior_assistant.session != source.session
        || prior_conversation.session != source.session
    {
        return Err("assistant handover source belongs to another instance".into());
    }
    // The retired binding describes its own immutable instance. An explicitly
    // retired workflow may be replaced with different node identities; compare
    // those revisions only after validating each instance's authoritative facts.
    if certified.as_ref().is_none_or(|binding| {
        binding.wait_node != prior_assistant.wait_node
            || binding.work_node != prior_assistant.work_node
            || binding.reply_node != prior_assistant.reply_node
    }) || prior_conversation.reply_node != prior_assistant.work_node
    {
        return Err("assistant handover predecessor identity changed".into());
    }
    // Plugin-only publications preserve this node's scene and native history.
    // Structural or instruction changes still require an explicit new instance.
    let prior_view = run_access::load(root, previous)
        .map_err(|error| error.to_string())?
        .ok_or("assistant handover source Run is missing")?;
    let current_view = run_access::load(root, &key.run_id)
        .map_err(|error| error.to_string())?
        .ok_or("assistant handover target Run is missing")?;
    let prior_record = prior_view.progress();
    let current_record = current_view.progress();
    if prior_record.graph_digest != prior.graph_digest
        || current_record.graph_digest != metadata.graph_digest
    {
        return Err("assistant handover Run identity changed".into());
    }
    let compatible_revision = crate::assistant_plugins::compatible(
        prior_record.snapshot,
        current_record.snapshot,
        &source.work_node,
    );
    if prior_assistant.work_node != source.work_node
        || prior_conversation.reply_node != source.work_node
        || prior.bundle_source != metadata.bundle_source
        || !compatible_revision
    {
        eprintln!(
            "anchor-runner-host: assistant handover from {previous} belongs to another Graph revision; its workspace scene is not inherited"
        );
        return Ok(None);
    }
    // The stable scene is per Run and node, so the invocation only records which
    // round of the handed-over Run last prepared it.
    let invocation = prior_record
        .invocations
        .get(&key.node_id)
        .copied()
        .filter(|invocation| *invocation > 0)
        .unwrap_or(1);
    Ok(Some(InvocationKey {
        run_id: previous.to_owned(),
        graph_digest: prior.graph_digest,
        node_id: key.node_id.clone(),
        invocation,
    }))
}

/// Skip replacement instances that crashed before preparing their work scene.
/// Every edge comes from immutable metadata and the Session's handover order.
pub(crate) fn handover_workspace_source(
    root: &Path,
    key: &InvocationKey,
    artifacts: &crate::HostArtifacts,
) -> Result<Option<InvocationKey>, String> {
    let Some(mut previous) = handover_predecessor(root, key)? else {
        return Ok(None);
    };
    let metadata = crate::run_metadata::load(root, &key.run_id)
        .map_err(|error| format!("assistant metadata unavailable: {error:?}"))?
        .ok_or("assistant metadata is missing")?;
    let source = metadata
        .assistant
        .as_ref()
        .ok_or("assistant source binding is missing")?;
    let current_view = run_access::load(root, &key.run_id)
        .map_err(|error| error.to_string())?
        .ok_or("assistant Run is missing")?;
    let current = current_view.progress();
    let sessions = sessions(root)?;
    let mut visited = std::collections::BTreeSet::new();
    loop {
        if !visited.insert(previous.run_id.clone()) {
            return Err("assistant workspace handover cycle".into());
        }
        if let Some(owner) = artifacts
            .initialized_workspace_owner(&previous)
            .map_err(|error| error.to_string())?
        {
            return Ok(Some(owner));
        }
        let prior = crate::run_metadata::load(root, &previous.run_id)
            .map_err(|error| format!("assistant workspace predecessor unavailable: {error:?}"))?
            .ok_or("assistant workspace predecessor metadata is missing")?;
        let Some(older) = prior
            .conversation
            .as_ref()
            .and_then(|conversation| conversation.previous_run.as_deref())
        else {
            return Ok(None);
        };
        let certified = sessions
            .channel_assistant_predecessor(&source.owner, &source.session, &previous.run_id)
            .map_err(|error| error.to_string())?;
        if certified
            .as_ref()
            .is_none_or(|binding| binding.run_id != older)
        {
            return Err("assistant workspace predecessor identity changed".into());
        }
        let older_meta = crate::run_metadata::load(root, older)
            .map_err(|error| format!("assistant workspace ancestor unavailable: {error:?}"))?
            .ok_or("assistant workspace ancestor metadata is missing")?;
        let older_source = older_meta
            .assistant
            .as_ref()
            .ok_or("assistant workspace ancestor is not an assistant")?;
        let older_view = run_access::load(root, older)
            .map_err(|error| error.to_string())?
            .ok_or("assistant workspace ancestor Run is missing")?;
        let older_record = older_view.progress();
        if older_source.owner != source.owner
            || older_source.session != source.session
            || older_meta.conversation.as_ref().is_none_or(|conversation| {
                conversation.session != source.session
                    || conversation.reply_node != older_source.work_node
            })
            || older_meta.graph_digest != older_record.graph_digest
        {
            return Err("assistant workspace ancestor belongs to another instance".into());
        }
        if certified.as_ref().is_none_or(|binding| {
            binding.wait_node != older_source.wait_node
                || binding.work_node != older_source.work_node
                || binding.reply_node != older_source.reply_node
        }) {
            return Err("assistant workspace predecessor identity changed".into());
        }
        if older_source.work_node != source.work_node
            || older_meta.bundle_source != metadata.bundle_source
            || !crate::assistant_plugins::compatible(
                older_record.snapshot,
                current.snapshot,
                &source.work_node,
            )
        {
            return Ok(None);
        }
        previous = InvocationKey {
            run_id: older.into(),
            graph_digest: older_record.graph_digest.to_owned(),
            node_id: source.work_node.clone(),
            invocation: 1,
        };
    }
}

pub(crate) fn binding(root: &Path, key: &InvocationKey) -> Result<Option<TurnBinding>, String> {
    let Some(binding) = read_json::<TurnBinding>(&binding_path(root, key))? else {
        return Ok(None);
    };
    if binding.key != *key {
        return Err("assistant invocation identity changed".into());
    }
    Ok(Some(binding))
}

pub(crate) fn bind(
    root: &Path,
    key: &InvocationKey,
    wait_key: String,
    inbound: &ChannelInboundAdmission,
    pending_inbounds: Vec<String>,
    pending_truncated: bool,
    notification: Option<anchor_platform_session::ChannelNotification>,
) -> Result<TurnBinding, String> {
    let binding = TurnBinding {
        key: key.clone(),
        wait_key,
        inbound: inbound.relation.inbound_id.clone(),
        turn: inbound.relation.turn_id.clone(),
        pending_inbounds,
        pending_truncated,
        notification,
    };
    save_immutable(&binding_path(root, key), &binding)?;
    Ok(binding)
}

pub(crate) fn input(
    root: &Path,
    key: &InvocationKey,
) -> Result<Option<ChannelInboundAdmission>, String> {
    let Some(source) = source(root, key)? else {
        return Ok(None);
    };
    let Some(binding) = binding(root, key)? else {
        return Ok(None);
    };
    let input = sessions(root)?
        .get_channel_assistant_input(
            &source.owner,
            &source.session,
            &key.run_id,
            &binding.wait_key,
        )
        .map_err(|error| error.to_string())?
        .ok_or("assistant input binding is missing")?;
    if input.relation.turn_id != binding.turn || input.relation.inbound_id != binding.inbound {
        return Err("assistant input identity changed".into());
    }
    Ok(Some(input))
}

pub(crate) fn bind_request(
    root: &Path,
    request: &NodeExecutionRequest,
) -> Result<Option<ChannelInboundAdmission>, String> {
    let Some(source) = source(root, &request.key)? else {
        return Ok(None);
    };
    if let Some(input) = input(root, &request.key)? {
        return Ok(Some(input));
    }
    for commit in &request.input_commits {
        let predecessor = InvocationKey {
            node_id: commit.node_id.clone(),
            invocation: commit.invocation,
            ..request.key.clone()
        };
        if let Some(prior) = binding(root, &predecessor)? {
            let input = sessions(root)?
                .get_channel_assistant_input(
                    &source.owner,
                    &source.session,
                    &request.key.run_id,
                    &prior.wait_key,
                )
                .map_err(|error| error.to_string())?
                .ok_or("assistant predecessor input is missing")?;
            bind(
                root,
                &request.key,
                prior.wait_key,
                &input,
                prior.pending_inbounds,
                prior.pending_truncated,
                prior.notification,
            )?;
            return Ok(Some(input));
        }
    }
    Err("assistant execution has no trusted input predecessor".into())
}

pub(crate) fn attachment_metadata(
    root: &Path,
    metadata: &RunMetadata,
    key: &InvocationKey,
) -> Result<RunMetadata, String> {
    let Some(input) = input(root, key)? else {
        return Ok(metadata.clone());
    };
    Ok(attachment_metadata_for_input(metadata, &input))
}

pub(crate) fn attachment_metadata_for_input(
    metadata: &RunMetadata,
    input: &ChannelInboundAdmission,
) -> RunMetadata {
    let mut selected = metadata.clone();
    selected.run_id = format!("turn-{}", input.relation.turn_id);
    selected.attachments = input
        .request
        .attachments
        .files
        .iter()
        .map(|file| crate::channel_inputs::AttachmentManifest {
            name: file.name.clone(),
            sha256: file.sha256.clone(),
            size: file.size,
            media_type: file.media_type.clone(),
        })
        .collect();
    selected
}

pub(crate) fn pending_inputs(
    root: &Path,
    key: &InvocationKey,
) -> Result<Vec<ChannelInboundAdmission>, String> {
    let Some(source) = source(root, key)? else {
        return Ok(Vec::new());
    };
    let Some(binding) = binding(root, key)? else {
        return Ok(Vec::new());
    };
    if binding.pending_inbounds.len() > 8 {
        return Err("assistant pending input scope exceeds its limit".into());
    }
    let store = sessions(root)?;
    binding
        .pending_inbounds
        .into_iter()
        .map(|inbound| {
            let input = store
                .get_channel_inbound(&source.owner, &source.session, &inbound)
                .map_err(|error| error.to_string())?;
            if input.relation.inbound_id != inbound || input.relation.turn_id == binding.turn {
                return Err("assistant pending input identity changed".into());
            }
            Ok(input)
        })
        .collect()
}

pub(crate) fn graph_error(error: impl std::fmt::Display) -> GraphError {
    GraphError::Unsupported(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::{RunApplication, run_access::fixtures};
    use crate::run_metadata::{AssistantSource, ConversationSource, RunMetadata};
    use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest};
    use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot};
    use serde_json::json;

    const OWNER: &str = "local";

    fn snapshot() -> GraphSnapshot {
        GraphSnapshot::admit(json!({
            "objective":"handover fixture",
            "entry":"wait_input",
            "agents":{"assistant":{"model":"models.worker","instructions":"fixture"}},
            "ops":{
                "wait_input":{"host":{"operation":"session.wait_input"}},
                "reply":{"host":{"operation":"session.reply"}}
            },
            "nodes":[
                {"id":"wait_input","op":"wait_input"},
                {"id":"assistant","agent":"assistant"},
                {"id":"reply","op":"reply"}
            ],
            "edges":[
                {"from":"wait_input","to":"assistant"},
                {"from":"assistant","to":"reply"},
                {"from":"reply","to":"wait_input"}
            ]
        }))
        .unwrap()
    }

    fn inbound(identity: &str) -> ChannelInboundRequest {
        ChannelInboundRequest {
            inbound_id: identity.into(),
            identity: ChannelIdentity {
                conversation_kind: Default::default(),
                source: "wecom".into(),
                account: None,
                conversation_id: format!("conversation-{identity}"),
                sender_id: "user".into(),
            },
            graph: "fixture".into(),
            reply_node: "assistant".into(),
            text: Some(identity.into()),
            attachments: Default::default(),
            run_id: None,
            replace_running: true,
        }
    }

    struct Fixture {
        _temp: tempfile::TempDir,
        root: PathBuf,
        graph: PathBuf,
        application: RunApplication,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        std::fs::create_dir_all(root.join("platform")).unwrap();
        let graph = temp.path().join("bundle");
        std::fs::create_dir_all(&graph).unwrap();
        let application = RunApplication::new(root.clone(), graph.clone());
        Fixture {
            _temp: temp,
            root,
            graph,
            application,
        }
    }

    fn admit(fixture: &Fixture, run: &str, session: &str, previous: Option<&str>) -> InvocationKey {
        admit_for(fixture, run, session, previous, OWNER)
    }

    fn admit_for(
        fixture: &Fixture,
        run: &str,
        session: &str,
        previous: Option<&str>,
        owner: &str,
    ) -> InvocationKey {
        admit_snapshot(fixture, run, session, previous, owner, snapshot())
    }

    fn admit_snapshot(
        fixture: &Fixture,
        run: &str,
        session: &str,
        previous: Option<&str>,
        owner: &str,
        snapshot: GraphSnapshot,
    ) -> InvocationKey {
        let record =
            GraphRunRecord::create_with_id(snapshot, json!({"session":session}), run).unwrap();
        let digest = record.graph_digest.clone();
        if fixture.application.read_run(run).unwrap().is_none() {
            fixtures::create(&fixture.application, &record);
        }
        let mut metadata =
            RunMetadata::new(run.into(), "fixture".into(), digest.clone(), &fixture.graph).unwrap();
        metadata.assistant = Some(AssistantSource {
            owner: owner.into(),
            session: session.into(),
            wait_node: AssistantPlan::from_snapshot(&record.snapshot, "assistant")
                .unwrap()
                .unwrap()
                .wait_node,
            work_node: "assistant".into(),
            reply_node: "reply".into(),
        });
        metadata.conversation = Some(ConversationSource {
            session: session.into(),
            reply_node: "assistant".into(),
            previous_run: previous.map(str::to_owned),
        });
        crate::run_metadata::save(&fixture.root, &metadata).unwrap();
        InvocationKey {
            run_id: run.into(),
            graph_digest: digest,
            node_id: "assistant".into(),
            invocation: 1,
        }
    }

    #[test]
    fn handover_source_requires_the_binding_and_the_metadata_to_agree() {
        let fixture = fixture();
        let sessions = sessions(&fixture.root).unwrap();
        let session = sessions
            .admit_channel_inbound(OWNER, inbound("one"))
            .unwrap()
            .session
            .id;
        let old = admit(&fixture, "assistant-old", &session, None);
        // Bound to nothing at all.
        assert_eq!(handover_predecessor(&fixture.root, &old).unwrap(), None);
        sessions
            .bind_channel_assistant(
                OWNER,
                &session,
                "assistant-old",
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();
        // Current instance, no handover predecessor.
        assert_eq!(handover_predecessor(&fixture.root, &old).unwrap(), None);
        let new = admit(&fixture, "assistant-new", &session, Some("assistant-old"));
        // `previous_run` alone is a conversation lineage, not a handover.
        assert_eq!(handover_predecessor(&fixture.root, &new).unwrap(), None);
        sessions
            .handover_channel_assistant(
                OWNER,
                &session,
                "assistant-old",
                "assistant-new",
                "new-key",
            )
            .unwrap();
        assert_eq!(
            handover_predecessor(&fixture.root, &new).unwrap(),
            Some(old.clone())
        );
        // Only the instance's own work node, on its first invocation.
        assert_eq!(
            handover_predecessor(
                &fixture.root,
                &InvocationKey {
                    node_id: "reply".into(),
                    ..new.clone()
                }
            )
            .unwrap(),
            None
        );
        assert_eq!(
            handover_predecessor(
                &fixture.root,
                &InvocationKey {
                    invocation: 2,
                    ..new.clone()
                }
            )
            .unwrap(),
            None
        );
        // A Graph identity this Run does not own authorizes nothing.
        assert_eq!(
            handover_predecessor(
                &fixture.root,
                &InvocationKey {
                    graph_digest: "other-digest".into(),
                    ..new.clone()
                }
            )
            .unwrap(),
            None
        );
        // Another Session never inherits this instance's scene.
        let other = sessions
            .admit_channel_inbound(OWNER, inbound("two"))
            .unwrap()
            .session
            .id;
        let foreign = admit_for(
            &fixture,
            "assistant-foreign",
            &other,
            Some("assistant-old"),
            OWNER,
        );
        sessions
            .bind_channel_assistant(
                OWNER,
                &other,
                "assistant-foreign",
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();
        assert_eq!(handover_predecessor(&fixture.root, &foreign).unwrap(), None);
        // The recorded invocation is the round of the handed-over Run that last
        // prepared the very same scene.
        for _ in 0..4 {
            for node in ["wait_input", "assistant", "reply"] {
                fixtures::complete(
                    &fixture.application,
                    "assistant-old",
                    node,
                    fixtures::completion(json!({})),
                    None,
                );
            }
        }
        assert_eq!(
            handover_predecessor(&fixture.root, &new)
                .unwrap()
                .unwrap()
                .invocation,
            4
        );
    }

    #[test]
    fn retired_instance_with_renamed_wait_starts_a_new_workspace_boundary() {
        let fixture = fixture();
        let sessions = sessions(&fixture.root).unwrap();
        let first = sessions
            .admit_channel_inbound(OWNER, inbound("one"))
            .unwrap();
        let session = first.session.id;
        sessions
            .finish_turn(
                OWNER,
                &session,
                &first.turn.id,
                anchor_platform_session::TurnStatus::Completed,
                None,
            )
            .unwrap();
        let old = admit(&fixture, "assistant-old", &session, None);
        sessions
            .bind_channel_assistant(
                OWNER,
                &session,
                &old.run_id,
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();
        sessions
            .retire_channel_assistant(OWNER, &session, &old.run_id)
            .unwrap();
        let mut definition = serde_json::to_value(snapshot()).unwrap();
        definition["entry"] = json!("current_wait");
        definition["nodes"][0]["id"] = json!("current_wait");
        definition["edges"][0]["from"] = json!("current_wait");
        definition["edges"][2]["to"] = json!("current_wait");
        let updated = GraphSnapshot::admit(definition).unwrap();
        let middle = admit_snapshot(
            &fixture,
            "assistant-middle",
            &session,
            Some(&old.run_id),
            OWNER,
            updated.clone(),
        );
        sessions
            .bind_channel_assistant(
                OWNER,
                &session,
                &middle.run_id,
                "current_wait",
                "assistant",
                "reply",
            )
            .unwrap();
        assert_eq!(handover_predecessor(&fixture.root, &middle).unwrap(), None);

        // A crash before the new instance prepares a workspace cannot cross the
        // explicit workflow change to recover the older instance's scene.
        let new = admit_snapshot(
            &fixture,
            "assistant-new",
            &session,
            Some(&middle.run_id),
            OWNER,
            updated,
        );
        sessions
            .handover_channel_assistant(OWNER, &session, &middle.run_id, &new.run_id, "new-key")
            .unwrap();
        let artifacts = crate::HostArtifacts::new(
            fixture.root.join("artifacts"),
            fixture.root.join("workspaces"),
        );
        assert_eq!(
            handover_predecessor(&fixture.root, &new).unwrap(),
            Some(middle.clone())
        );
        assert_eq!(
            handover_workspace_source(&fixture.root, &new, &artifacts).unwrap(),
            None
        );

        // The old binding must still agree with its own immutable metadata.
        let mut metadata = crate::run_metadata::load(&fixture.root, &old.run_id)
            .unwrap()
            .unwrap();
        metadata.assistant.as_mut().unwrap().wait_node = "tampered_wait".into();
        crate::run_metadata::save(&fixture.root, &metadata).unwrap();
        assert!(
            handover_workspace_source(&fixture.root, &new, &artifacts)
                .unwrap_err()
                .contains("identity changed")
        );
    }

    #[test]
    fn handover_source_retains_a_proven_handover_whose_facts_disagree() {
        let fixture = fixture();
        let sessions = sessions(&fixture.root).unwrap();
        let session = sessions
            .admit_channel_inbound(OWNER, inbound("one"))
            .unwrap()
            .session
            .id;
        admit(&fixture, "assistant-old", &session, None);
        sessions
            .bind_channel_assistant(
                OWNER,
                &session,
                "assistant-old",
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();
        let new = admit(&fixture, "assistant-new", &session, Some("assistant-old"));
        sessions
            .handover_channel_assistant(
                OWNER,
                &session,
                "assistant-old",
                "assistant-new",
                "new-key",
            )
            .unwrap();

        // A predecessor that is not an assistant instance is retained corruption.
        let mut foreign = RunMetadata::new(
            "assistant-old".into(),
            "fixture".into(),
            new.graph_digest.clone(),
            &fixture.graph,
        )
        .unwrap();
        foreign.assistant = None;
        crate::run_metadata::save(&fixture.root, &foreign).unwrap();
        assert!(
            handover_predecessor(&fixture.root, &new)
                .unwrap_err()
                .contains("not an assistant instance")
        );
        // A predecessor with no immutable identity cannot be trusted either.
        std::fs::remove_file(fixture.root.join("run-metadata/assistant-old.json")).unwrap();
        assert!(
            handover_predecessor(&fixture.root, &new)
                .unwrap_err()
                .contains("no immutable metadata")
        );
        // And the identity must be the same instance.
        admit_for(&fixture, "assistant-old", &session, None, "another-owner");
        assert!(
            handover_predecessor(&fixture.root, &new)
                .unwrap_err()
                .contains("another instance")
        );
    }

    #[test]
    fn workspace_handover_skips_unprepared_replacements_and_keeps_real_graph_identity() {
        let fixture = fixture();
        let sessions = sessions(&fixture.root).unwrap();
        let session = sessions
            .admit_channel_inbound(OWNER, inbound("one"))
            .unwrap()
            .session
            .id;
        let old = admit(&fixture, "assistant-old", &session, None);
        sessions
            .bind_channel_assistant(
                OWNER,
                &session,
                &old.run_id,
                "wait_input",
                "assistant",
                "reply",
            )
            .unwrap();
        let artifacts = crate::HostArtifacts::new(
            fixture.root.join("artifacts"),
            fixture.root.join("workspaces"),
        );
        artifacts
            .bind_node_workspace(&old.run_id, &old.graph_digest, &old.node_id)
            .unwrap();
        let scene = artifacts.prepare_workspace(&old, &[]).unwrap();
        let execution = artifacts.workspaces().acquire(&old).unwrap();
        std::fs::write(scene.join("draft.md"), "retained work").unwrap();
        execution.finish_interrupted().unwrap();
        // Completed invocations can advance without preparing a newer workspace.
        for _ in 0..8 {
            for node in ["wait_input", "assistant", "reply"] {
                fixtures::complete(
                    &fixture.application,
                    &old.run_id,
                    node,
                    fixtures::completion(json!({})),
                    None,
                );
            }
        }
        let middle = admit(&fixture, "assistant-middle", &session, Some(&old.run_id));
        sessions
            .handover_channel_assistant(OWNER, &session, &old.run_id, &middle.run_id, "middle-key")
            .unwrap();
        artifacts
            .bind_node_workspace(&middle.run_id, &middle.graph_digest, &middle.node_id)
            .unwrap();
        let mut updated = snapshot();
        updated.agents.get_mut("assistant").unwrap().network = true;
        let new = admit_snapshot(
            &fixture,
            "assistant-new",
            &session,
            Some(&middle.run_id),
            OWNER,
            updated.clone(),
        );
        sessions
            .handover_channel_assistant(OWNER, &session, &middle.run_id, &new.run_id, "new-key")
            .unwrap();
        assert_eq!(
            handover_predecessor(&fixture.root, &new)
                .unwrap()
                .unwrap()
                .run_id,
            middle.run_id
        );
        let inherited = handover_workspace_source(&fixture.root, &new, &artifacts)
            .unwrap()
            .unwrap();
        assert_eq!(inherited, old);
        assert_ne!(inherited.graph_digest, new.graph_digest);
        artifacts
            .bind_node_workspace(&new.run_id, &new.graph_digest, &new.node_id)
            .unwrap();
        let path = artifacts
            .prepare_workspace_from_handover(&new, &[], &inherited, &snapshot(), &updated)
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(path.join("draft.md")).unwrap(),
            "retained work"
        );
        // A present but unreadable intermediate ownership fact is corruption,
        // rather than permission to jump to an older scene.
        let state = artifacts.workspace_path(&middle).unwrap();
        std::fs::write(
            state.parent().unwrap().join("owner.json"),
            "invalid owner fact",
        )
        .unwrap();
        assert!(handover_workspace_source(&fixture.root, &new, &artifacts).is_err());
    }
}
