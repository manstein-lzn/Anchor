use super::{ApplicationError, storage};
use crate::create_durable_directory;
use serde::{Deserialize, Serialize};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

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

    pub(super) fn apply(self, metadata: &mut RunMetadata) {
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
    #[serde(default)]
    pub(crate) channel: Option<ChannelRunSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) assistant: Option<AssistantSource>,
    #[serde(default)]
    pub(crate) pilot: Option<PilotRunSource>,
    #[serde(default)]
    pub(crate) session_call: Option<super::session_calls::SessionCall>,
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
pub(crate) struct ChannelRunSource {
    pub(crate) owner: String,
    pub(crate) session: String,
    pub(crate) inbound: String,
    pub(crate) turn: String,
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

pub(crate) fn now_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

impl RunMetadata {
    pub(crate) fn new(
        run_id: String,
        graph: String,
        graph_digest: String,
        source: &Path,
    ) -> Result<Self, ApplicationError> {
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
            channel: None,
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
    ) -> Result<Self, ApplicationError> {
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
    ) -> Result<Self, ApplicationError> {
        let mut metadata = Self::child(run_id, graph, graph_digest, source)?;
        if !matches!(call.mode.as_str(), "wait" | "detach")
            || call.parent_run.is_empty()
            || call.parent_graph.is_empty()
            || call.parent_graph_digest.is_empty()
            || call.node.is_empty()
            || call.invocation == 0
            || call.root_run.is_empty()
        {
            return Err(ApplicationError::Invalid(
                "invalid Graph call source metadata".into(),
            ));
        }
        metadata.graph_call = Some(call);
        Ok(metadata)
    }
}

fn path(root: &Path, id: &str) -> Result<PathBuf, ApplicationError> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        return Err(ApplicationError::Invalid("invalid Run id".into()));
    }
    Ok(root.join("run-metadata").join(format!("{id}.json")))
}

pub(crate) fn save(root: &Path, metadata: &RunMetadata) -> Result<(), ApplicationError> {
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
) -> Result<(), ApplicationError> {
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
            return Err(ApplicationError::Conflict(
                "child Run identity metadata conflicts with its durable admission".into(),
            ));
        }
        return Ok(());
    }
    save(root, &expected)
}

pub(crate) fn load(root: &Path, id: &str) -> Result<Option<RunMetadata>, ApplicationError> {
    let bytes = match std::fs::read(path(root, id)?) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ApplicationError::Storage(format!(
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
        return Err(ApplicationError::Storage(
            "Run identity metadata is corrupt".into(),
        ));
    }
    Ok(Some(metadata))
}

/// Every Run id that still has immutable metadata.
///
/// Metadata is written before a Run's admitted record, so a handover that died
/// in between is only visible here. Listings that must not miss such a pending
/// obligation use this instead of the Run store.
pub(crate) fn ids(root: &Path) -> Result<Vec<String>, ApplicationError> {
    let directory = root.join("run-metadata");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(ApplicationError::Storage(format!(
                "Run identity metadata unavailable: {error}"
            )));
        }
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| ApplicationError::Storage(error.to_string()))?;
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
