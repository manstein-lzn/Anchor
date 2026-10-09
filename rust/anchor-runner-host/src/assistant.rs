use crate::application::metadata::{AssistantSource, RunMetadata};
use anchor_platform_session::{ChannelInboundAdmission, SessionStore};
use anchor_runtime::graph::{
    FileRunStore, GraphError, GraphSnapshot, InvocationKey, NodeExecutionRequest, RunStore,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
}

fn stem(key: &InvocationKey) -> String {
    format!("{:x}", Sha256::digest(key.durable_key().as_bytes()))
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
    let Some(metadata) = crate::application::metadata::load(root, &key.run_id)
        .map_err(|error| format!("{error:?}"))?
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
/// authorize a copy. Only the first preparation of the instance's own work node
/// inherits a scene.
pub(crate) fn handover_workspace_source(
    root: &Path,
    key: &InvocationKey,
) -> Result<Option<InvocationKey>, String> {
    let Some(metadata) = crate::application::metadata::load(root, &key.run_id)
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
            || current.work_node != key.node_id
            || current.reply_node != source.reply_node
    }) || !sessions
        .retired_channel_assistant(&source.owner, &source.session, previous)
        .map_err(|error| error.to_string())?
    {
        return Ok(None);
    }
    // The handover is real. From here on the facts must agree, so a mismatch is
    // retained corruption instead of a silently skipped copy.
    let Some(prior) = crate::application::metadata::load(root, previous)
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
    // A different Graph revision or bundle means the handed-over scene was not
    // produced by this node's current definition, so it is not inherited. The
    // Run continues with an uninitialized workspace instead of copying bytes no
    // current fact describes.
    if prior_assistant.work_node != source.work_node
        || prior_conversation.reply_node != source.work_node
        || prior.bundle_source != metadata.bundle_source
        || prior.graph_digest != metadata.graph_digest
    {
        eprintln!(
            "anchor-runner-host: assistant handover from {previous} belongs to another Graph revision; its workspace scene is not inherited"
        );
        return Ok(None);
    }
    // The stable scene is per Run and node, so the invocation only records which
    // round of the handed-over Run last prepared it.
    let invocation = FileRunStore::new(root.join("runs"))
        .load(previous)
        .map_err(|error| error.to_string())?
        .and_then(|record| record.invocations.get(&key.node_id).copied())
        .filter(|invocation| *invocation > 0)
        .unwrap_or(1);
    Ok(Some(InvocationKey {
        run_id: previous.to_owned(),
        graph_digest: metadata.graph_digest,
        node_id: key.node_id.clone(),
        invocation,
    }))
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
) -> Result<TurnBinding, String> {
    let binding = TurnBinding {
        key: key.clone(),
        wait_key,
        inbound: inbound.relation.inbound_id.clone(),
        turn: inbound.relation.turn_id.clone(),
        pending_inbounds,
        pending_truncated,
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
    use crate::application::metadata::{AssistantSource, ConversationSource, RunMetadata};
    use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest};
    use anchor_runtime::graph::{FileRunStore, GraphRunRecord, GraphSnapshot, RunStore};
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
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        std::fs::create_dir_all(root.join("platform")).unwrap();
        let graph = temp.path().join("bundle");
        std::fs::create_dir_all(&graph).unwrap();
        Fixture {
            _temp: temp,
            root,
            graph,
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
        let record =
            GraphRunRecord::create_with_id(snapshot(), json!({"session":session}), run).unwrap();
        let digest = record.graph_digest.clone();
        FileRunStore::new(fixture.root.join("runs"))
            .save(&record)
            .unwrap();
        let mut metadata =
            RunMetadata::new(run.into(), "fixture".into(), digest.clone(), &fixture.graph).unwrap();
        metadata.assistant = Some(AssistantSource {
            owner: owner.into(),
            session: session.into(),
            wait_node: "wait_input".into(),
            work_node: "assistant".into(),
            reply_node: "reply".into(),
        });
        metadata.conversation = Some(ConversationSource {
            session: session.into(),
            reply_node: "assistant".into(),
            previous_run: previous.map(str::to_owned),
        });
        crate::application::metadata::save(&fixture.root, &metadata).unwrap();
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
        assert_eq!(
            handover_workspace_source(&fixture.root, &old).unwrap(),
            None
        );
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
        assert_eq!(
            handover_workspace_source(&fixture.root, &old).unwrap(),
            None
        );
        let new = admit(&fixture, "assistant-new", &session, Some("assistant-old"));
        // `previous_run` alone is a conversation lineage, not a handover.
        assert_eq!(
            handover_workspace_source(&fixture.root, &new).unwrap(),
            None
        );
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
            handover_workspace_source(&fixture.root, &new).unwrap(),
            Some(old.clone())
        );
        // Only the instance's own work node, on its first invocation.
        assert_eq!(
            handover_workspace_source(
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
            handover_workspace_source(
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
            handover_workspace_source(
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
        assert_eq!(
            handover_workspace_source(&fixture.root, &foreign).unwrap(),
            None
        );
        // The recorded invocation is the round of the handed-over Run that last
        // prepared the very same scene.
        let mut record = FileRunStore::new(fixture.root.join("runs"))
            .load("assistant-old")
            .unwrap()
            .unwrap();
        record.invocations.insert("assistant".into(), 4);
        FileRunStore::new(fixture.root.join("runs"))
            .save(&record)
            .unwrap();
        assert_eq!(
            handover_workspace_source(&fixture.root, &new)
                .unwrap()
                .unwrap()
                .invocation,
            4
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
        crate::application::metadata::save(&fixture.root, &foreign).unwrap();
        assert!(
            handover_workspace_source(&fixture.root, &new)
                .unwrap_err()
                .contains("not an assistant instance")
        );
        // A predecessor with no immutable identity cannot be trusted either.
        std::fs::remove_file(fixture.root.join("run-metadata/assistant-old.json")).unwrap();
        assert!(
            handover_workspace_source(&fixture.root, &new)
                .unwrap_err()
                .contains("no immutable metadata")
        );
        // And the identity must be the same instance.
        admit_for(&fixture, "assistant-old", &session, None, "another-owner");
        assert!(
            handover_workspace_source(&fixture.root, &new)
                .unwrap_err()
                .contains("another instance")
        );
    }
}
