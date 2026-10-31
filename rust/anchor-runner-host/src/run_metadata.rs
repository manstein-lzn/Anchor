//! Durable Run identity metadata and Graph source identity; independent of application use cases.
use crate::create_durable_directory;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug)]
pub(crate) enum MetadataError {
    Conflict(String),
    Invalid(String),
    Storage(String),
}

fn storage(error: impl std::fmt::Display) -> MetadataError {
    MetadataError::Storage(error.to_string())
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TriggerSource {
    #[default]
    Manual,
    Schedule,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunTrigger {
    source: TriggerSource,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    scheduled_at: Option<String>,
}

impl RunTrigger {
    pub(crate) fn validate(&self) -> Result<(), String> {
        match self.source {
            TriggerSource::Manual if self.schedule.is_some() || self.scheduled_at.is_some() => {
                Err("manual trigger does not accept schedule or scheduled_at".into())
            }
            TriggerSource::Manual => Ok(()),
            TriggerSource::Schedule => {
                if self
                    .schedule
                    .as_ref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    return Err("schedule trigger requires a non-empty schedule".into());
                }
                let scheduled_at = self
                    .scheduled_at
                    .as_deref()
                    .ok_or_else(|| "schedule trigger requires scheduled_at".to_owned())?;
                // Stored schedules use local ISO timestamps without an offset.
                if chrono::DateTime::parse_from_rfc3339(scheduled_at).is_err()
                    && chrono::NaiveDateTime::parse_from_str(scheduled_at, "%Y-%m-%dT%H:%M:%S%.f")
                        .is_err()
                {
                    return Err("scheduled_at must be an ISO datetime".into());
                }
                Ok(())
            }
        }
    }

    pub(crate) fn apply(self, metadata: &mut RunMetadata) {
        metadata.trigger_source = match self.source {
            TriggerSource::Manual => "manual",
            TriggerSource::Schedule => "schedule",
        }
        .into();
        metadata.schedule = self.schedule;
        metadata.scheduled_at = self.scheduled_at;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunMetadata {
    format: u32,
    pub(crate) run_id: String,
    pub(crate) graph: String,
    pub(crate) graph_digest: String,
    pub(crate) bundle_source: PathBuf,
    pub(crate) created: String,
    pub(crate) trigger_source: String,
    #[serde(default)]
    pub(crate) oauth_owner: Option<String>,
    #[serde(default)]
    pub(crate) graph_call: Option<GraphCallSource>,
    #[serde(default)]
    pub(crate) conversation: Option<ConversationSource>,
    /// Only read to refuse a per-message channel Run by name; every Run before
    /// that shape was removed recorded `"channel": null` here.
    #[serde(default, rename = "channel", skip_serializing)]
    legacy_channel: Option<serde::de::IgnoredAny>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) assistant: Option<AssistantSource>,
    #[serde(default)]
    pub(crate) pilot: Option<PilotRunSource>,
    #[serde(default)]
    pub(crate) session_call: Option<SessionCall>,
    #[serde(default)]
    pub(crate) attachments: Vec<crate::channel_inputs::AttachmentManifest>,
    #[serde(default)]
    pub(crate) schedule: Option<String>,
    #[serde(default)]
    pub(crate) scheduled_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConversationSource {
    pub(crate) session: String,
    pub(crate) reply_node: String,
    pub(crate) previous_run: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssistantSource {
    pub(crate) owner: String,
    pub(crate) session: String,
    pub(crate) wait_node: String,
    pub(crate) work_node: String,
    pub(crate) reply_node: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PilotRunSource {
    pub(crate) owner: String,
    pub(crate) session: String,
    pub(crate) turn: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GraphCallSource {
    pub(crate) parent_run: String,
    pub(crate) parent_graph: String,
    pub(crate) parent_graph_digest: String,
    pub(crate) node: String,
    pub(crate) invocation: u64,
    pub(crate) mode: String,
    pub(crate) root_run: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionContext {
    pub session: String,
    pub reply_node: String,
    pub conversation_id: String,
    pub channel: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionCall {
    pub context: SessionContext,
    pub status: String,
    #[serde(default)]
    pub error: String,
}

pub(crate) fn graph_identity(path: &Path) -> Result<PathBuf, MetadataError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(storage)?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let mut missing = VecDeque::new();
    let mut ancestor = normalized.as_path();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            MetadataError::Invalid("Graph bundle path has no existing ancestor".into())
        })?;
        missing.push_front(name.to_os_string());
        ancestor = ancestor
            .parent()
            .ok_or_else(|| MetadataError::Invalid("Graph bundle path has no parent".into()))?;
    }
    let mut identity = ancestor.canonicalize().map_err(storage)?;
    for component in missing {
        identity.push(component);
    }
    Ok(identity)
}

pub(crate) fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

impl RunMetadata {
    pub(crate) fn node_conversation_key(&self, node: &str) -> String {
        let identity = match &self.conversation {
            Some(binding) => serde_json::to_vec(&(&self.bundle_source, &binding.session, node)),
            None => serde_json::to_vec(&(
                "run",
                &self.run_id,
                &self.bundle_source,
                &self.graph_digest,
                node,
            )),
        }
        .expect("host conversation identity is serializable");
        format!("{:x}", Sha256::digest(identity))
    }

    pub(crate) fn new(
        run_id: String,
        graph: String,
        graph_digest: String,
        source: &Path,
    ) -> Result<Self, MetadataError> {
        Ok(Self {
            format: 2,
            run_id,
            graph,
            graph_digest,
            bundle_source: source.canonicalize().map_err(storage)?,
            created: chrono::DateTime::<chrono::Utc>::from(SystemTime::now()).to_rfc3339(),
            trigger_source: "manual".into(),
            oauth_owner: None,
            graph_call: None,
            conversation: None,
            legacy_channel: None,
            assistant: None,
            pilot: None,
            session_call: None,
            attachments: Vec::new(),
            schedule: None,
            scheduled_at: None,
        })
    }

    pub(crate) fn child(
        run_id: String,
        graph: String,
        graph_digest: String,
        source: &Path,
    ) -> Result<Self, MetadataError> {
        let mut metadata = Self::new(run_id, graph, graph_digest, source)?;
        metadata.trigger_source = "graph_call".into();
        Ok(metadata)
    }

    pub(crate) fn graph_call_child(
        run_id: String,
        graph: String,
        graph_digest: String,
        source: &Path,
        call: GraphCallSource,
    ) -> Result<Self, MetadataError> {
        let mut metadata = Self::child(run_id, graph, graph_digest, source)?;
        if !matches!(call.mode.as_str(), "wait" | "detach")
            || call.parent_run.is_empty()
            || call.parent_graph.is_empty()
            || call.parent_graph_digest.is_empty()
            || call.node.is_empty()
            || call.invocation == 0
            || call.root_run.is_empty()
        {
            return Err(MetadataError::Invalid(
                "invalid Graph call source metadata".into(),
            ));
        }
        metadata.graph_call = Some(call);
        Ok(metadata)
    }
}

fn path(root: &Path, id: &str) -> Result<PathBuf, MetadataError> {
    if !anchor_runtime::identifier::is_name(id) {
        return Err(MetadataError::Invalid("invalid Run id".into()));
    }
    Ok(root.join("run-metadata").join(format!("{id}.json")))
}

pub(crate) fn save(root: &Path, metadata: &RunMetadata) -> Result<(), MetadataError> {
    let target = path(root, &metadata.run_id)?;
    let directory = target.parent().expect("metadata path has parent");
    create_durable_directory(directory).map_err(storage)?;
    let temporary = directory.join(format!(".{}.{}.tmp", metadata.run_id, now_nanos()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .map_err(storage)?;
    file.write_all(&serde_json::to_vec(metadata).map_err(storage)?)
        .map_err(storage)?;
    file.sync_all().map_err(storage)?;
    std::fs::rename(temporary, &target).map_err(storage)?;
    std::fs::File::open(directory)
        .and_then(|f| f.sync_all())
        .map_err(storage)?;
    Ok(())
}

pub(crate) fn save_child_once(
    root: &Path,
    run_id: String,
    graph: String,
    graph_digest: String,
    source: &Path,
    call: GraphCallSource,
    oauth_owner: Option<String>,
) -> Result<(), MetadataError> {
    let mut expected =
        RunMetadata::graph_call_child(run_id.clone(), graph, graph_digest, source, call)?;
    expected.oauth_owner = oauth_owner;
    if let Some(existing) = load(root, &run_id)? {
        if existing.graph != expected.graph
            || existing.graph_digest != expected.graph_digest
            || existing.bundle_source != expected.bundle_source
            || existing.trigger_source != "graph_call"
            || existing.graph_call != expected.graph_call
            || existing.oauth_owner != expected.oauth_owner
        {
            return Err(MetadataError::Conflict(
                "child Run identity metadata conflicts with its durable admission".into(),
            ));
        }
        return Ok(());
    }
    save(root, &expected)
}

pub(crate) fn load(root: &Path, id: &str) -> Result<Option<RunMetadata>, MetadataError> {
    let bytes = match std::fs::read(path(root, id)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(MetadataError::Storage(format!(
                "Run identity metadata unavailable: {error}"
            )));
        }
    };
    let metadata: RunMetadata = serde_json::from_slice(&bytes).map_err(storage)?;
    if !matches!(metadata.format, 1 | 2)
        || metadata.run_id != id
        || metadata.graph.is_empty()
        || metadata.graph_digest.is_empty()
    {
        return Err(MetadataError::Storage(
            "Run identity metadata is corrupt".into(),
        ));
    }
    if metadata.legacy_channel.is_some() {
        return Err(MetadataError::Storage(format!(
            "legacy_channel_run: Run {id} is a per-message channel Run, which is no longer supported; remove it offline before continuing"
        )));
    }
    Ok(Some(metadata))
}

/// Every Run id that still has immutable metadata.
///
/// Metadata is written before a Run's admitted record, so a handover that died
/// in between is only visible here. Listings that must not miss such a pending
/// obligation use this instead of the Run store.
pub(crate) fn ids(root: &Path) -> Result<Vec<String>, MetadataError> {
    let directory = root.join("run-metadata");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(MetadataError::Storage(format!(
                "Run identity metadata unavailable: {error}"
            )));
        }
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| MetadataError::Storage(error.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Temporary files are named `.{run_id}.{nanos}.tmp`.
        let Some(id) = name.strip_suffix(".json") else {
            continue;
        };
        if id.is_empty() || id.starts_with('.') {
            continue;
        }
        ids.push(id.to_owned());
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}
