//! Minimal Graph ownership that outlives deleted Run metadata. Native scope
//! leases and surviving Run/hand-off checks remain the cleanup authorities.

use crate::application::run_access::{self, RunView};
use crate::application::{ApplicationError, RunApplication};
use crate::run_metadata::RunMetadata;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Ownership {
    format: u32,
    graph: PathBuf,
    session: String,
    node: String,
    hint: String,
}

pub(crate) fn retain<'record>(
    data: &Path,
    record: impl Into<RunView<'record>>,
    metadata: &RunMetadata,
) -> Result<(), ApplicationError> {
    let view = record.into();
    let record = view.progress();
    let Some(conversation) = &metadata.conversation else {
        return Ok(());
    };
    let graph = crate::run_metadata::graph_identity(&metadata.bundle_source)?;
    for node in &record.snapshot.nodes {
        let ownership = Ownership {
            format: 1,
            graph: graph.clone(),
            session: conversation.session.clone(),
            node: node.id.clone(),
            hint: metadata.node_conversation_key(&node.id),
        };
        crate::assistant::save_immutable(
            &data
                .join("conversation-resources")
                .join(format!("{}.json", ownership.hint)),
            &ownership,
        )
        .map_err(ApplicationError::Storage)?;
    }
    Ok(())
}

fn ownerships(data: &Path, graph: &Path) -> Result<Vec<Ownership>, ApplicationError> {
    let directory = data.join("conversation-resources");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(ApplicationError::Storage(error.to_string())),
    };
    let mut retained = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| ApplicationError::Storage(error.to_string()))?
            .path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let ownership = crate::assistant::read_json::<Ownership>(&path)
            .map_err(ApplicationError::Storage)?
            .ok_or_else(|| {
                ApplicationError::Storage("conversation ownership disappeared".into())
            })?;
        if ownership.format != 1
            || ownership.hint.len() != 64
            || !ownership
                .hint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || path.file_stem().and_then(|stem| stem.to_str()) != Some(&ownership.hint)
        {
            return Err(ApplicationError::Storage(
                "conversation ownership identity changed".into(),
            ));
        }
        if ownership.graph == graph {
            retained.push(ownership);
        }
    }
    Ok(retained)
}

pub(crate) fn for_graph(data: &Path, graph: &Path) -> Result<Vec<String>, ApplicationError> {
    Ok(ownerships(data, graph)?
        .into_iter()
        .map(|ownership| ownership.hint)
        .collect())
}

fn remove(data: &Path, workspaces: &Path, hint: &str) -> Result<(), ApplicationError> {
    crate::goose_acp::remove_conversation(&workspaces.join(".goose-process"), hint)
        .map_err(ApplicationError::Storage)?;
    crate::run_data::remove_legacy_conversation(data, hint).map_err(ApplicationError::Storage)
}

/// Called with Graph admission and active-execution gates held. A surviving
/// Run retains only the nodes it actually owns; a metadata-only hand-off keeps
/// all of its Session's scopes until its snapshot is known.
pub(crate) fn release_unreferenced(
    application: &RunApplication,
    data: &Path,
    graph: &Path,
    deleting: &str,
    workspaces: &Path,
) -> Result<(), ApplicationError> {
    let references = crate::run_metadata::ids(data)?
        .into_iter()
        .filter(|id| id != deleting)
        .map(|id| application.metadata(&id).map(|metadata| (id, metadata)))
        .collect::<Result<Vec<_>, _>>()?;
    for ownership in ownerships(data, graph)? {
        let mut referenced = false;
        for (id, metadata) in &references {
            let Some(metadata) = metadata else {
                continue;
            };
            if crate::run_metadata::graph_identity(&metadata.bundle_source)? != ownership.graph
                || metadata
                    .conversation
                    .as_ref()
                    .is_none_or(|source| source.session != ownership.session)
            {
                continue;
            }
            let view = run_access::load(data, id)?;
            if view.as_ref().is_none_or(|view| {
                view.progress()
                    .snapshot
                    .nodes
                    .iter()
                    .any(|node| node.id == ownership.node)
            }) {
                referenced = true;
                break;
            }
        }
        if !referenced {
            remove(data, workspaces, &ownership.hint)?;
        }
    }
    Ok(())
}
