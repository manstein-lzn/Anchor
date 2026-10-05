//! Operator-owned per-node grants, frozen separately from distributable Graphs.

use crate::create_durable_directory;
use anchor_runtime_rig::{
    ReadOnlyInput,
    graph::{GraphRunRecord, GraphSnapshot, InvocationKey},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

type Grants = BTreeMap<String, BTreeMap<String, PathBuf>>;

#[derive(Clone, Debug)]
pub(crate) struct LocalInputs {
    state_root: PathBuf,
    config_root: Option<PathBuf>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenInputs {
    format: u32,
    run_id: String,
    graph_digest: String,
    config_root: Option<PathBuf>,
    graph: Option<String>,
    source_file: Option<PathBuf>,
    grants: Grants,
}

pub(crate) fn fact_path(state_root: &Path, run_id: &str) -> PathBuf {
    state_root
        .join("local-inputs")
        .join(format!("li1-{:x}.json", Sha256::digest(run_id.as_bytes())))
}

impl LocalInputs {
    pub(crate) fn from_env(state_root: PathBuf) -> Result<Self, String> {
        Self::new(
            state_root,
            std::env::var_os("ANCHOR_RUNNER_LOCAL_INPUTS_ROOT").map(PathBuf::from),
        )
    }

    pub(crate) fn new(state_root: PathBuf, config_root: Option<PathBuf>) -> Result<Self, String> {
        let config_root = config_root
            .map(|root| {
                if !root.is_absolute() || !root.is_dir() {
                    return Err(
                        "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT must be an existing absolute directory"
                            .into(),
                    );
                }
                root.canonicalize()
                    .map_err(|error| format!("local input config root unavailable: {error}"))
            })
            .transpose()?;
        Ok(Self {
            state_root,
            config_root,
        })
    }

    pub(crate) fn configured(&self) -> bool {
        self.config_root.is_some()
    }

    pub(crate) fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub(crate) fn freeze(
        &self,
        run_id: &str,
        graph_digest: &str,
        snapshot: &GraphSnapshot,
        graph: Option<&str>,
        fresh_run: bool,
    ) -> Result<(), String> {
        let expected = self.current(run_id, graph_digest, snapshot, graph)?;
        if let Some(frozen) = self.load(run_id)? {
            self.compare(&frozen, &expected)?;
            return Ok(());
        }
        if !fresh_run && (expected.config_root.is_some() || !expected.grants.is_empty()) {
            return Err("Run has no frozen local input authorization; start a new Run".into());
        }
        self.publish(&expected)?;
        let frozen = self
            .load(run_id)?
            .ok_or("local input authorization was not persisted")?;
        self.compare(&frozen, &expected)
    }

    pub(crate) fn verify(&self, record: &GraphRunRecord) -> Result<(), String> {
        let frozen = self
            .load(&record.run_id)?
            .ok_or("Run has no frozen local input authorization")?;
        let current = self.current(
            &record.run_id,
            &record.graph_digest,
            &record.snapshot,
            frozen.graph.as_deref(),
        )?;
        self.compare(&frozen, &current)
    }

    pub(crate) fn mounts(
        &self,
        record: &GraphRunRecord,
        key: &InvocationKey,
    ) -> Result<Vec<ReadOnlyInput>, String> {
        if key.run_id != record.run_id
            || key.graph_digest != record.graph_digest
            || !record
                .snapshot
                .nodes
                .iter()
                .any(|node| node.id == key.node_id)
        {
            return Err("local input request does not match the admitted Run/node".into());
        }
        self.verify(record)?;
        let frozen = self
            .load(&record.run_id)?
            .ok_or("Run has no frozen local input authorization")?;
        Ok(frozen
            .grants
            .get(&key.node_id)
            .into_iter()
            .flat_map(|grants| {
                grants.iter().map(|(name, source)| {
                    ReadOnlyInput::new(source, format!("/local-inputs/{name}"))
                })
            })
            .collect())
    }

    fn current(
        &self,
        run_id: &str,
        graph_digest: &str,
        snapshot: &GraphSnapshot,
        graph: Option<&str>,
    ) -> Result<FrozenInputs, String> {
        let (graph, source_file) = match &self.config_root {
            Some(root) => {
                let graph = graph
                    .filter(|name| valid_graph_name(name))
                    .ok_or("local input authorization requires the immutable Graph name")?;
                (
                    Some(graph.to_owned()),
                    Some(root.join(graph).join("local-inputs.json")),
                )
            }
            None => (None, None),
        };
        let grants = match &source_file {
            Some(path) => read_grants(path, snapshot)?,
            None => BTreeMap::new(),
        };
        Ok(FrozenInputs {
            format: 1,
            run_id: run_id.to_owned(),
            graph_digest: graph_digest.to_owned(),
            config_root: self.config_root.clone(),
            graph,
            source_file,
            grants,
        })
    }

    fn load(&self, run_id: &str) -> Result<Option<FrozenInputs>, String> {
        let bytes = match fs::read(fact_path(&self.state_root, run_id)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("frozen local inputs unavailable: {error}")),
        };
        let frozen: FrozenInputs = serde_json::from_slice(&bytes)
            .map_err(|error| format!("frozen local inputs are corrupt: {error}"))?;
        if frozen.format != 1 || frozen.run_id != run_id || frozen.graph_digest.is_empty() {
            return Err("frozen local input identity is corrupt".into());
        }
        Ok(Some(frozen))
    }

    fn compare(&self, frozen: &FrozenInputs, current: &FrozenInputs) -> Result<(), String> {
        if frozen != current {
            return Err(
                "Local input grants or operator config source changed; start a new Run".into(),
            );
        }
        Ok(())
    }

    fn publish(&self, frozen: &FrozenInputs) -> Result<(), String> {
        let target = fact_path(&self.state_root, &frozen.run_id);
        let directory = target.parent().expect("local input fact has a parent");
        create_durable_directory(directory).map_err(|error| error.to_string())?;
        let temporary = directory.join(format!(
            ".li1-{}.{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let result = (|| -> Result<(), io::Error> {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)?;
            file.write_all(&serde_json::to_vec(frozen).map_err(io::Error::other)?)?;
            file.sync_all()?;
            match fs::hard_link(&temporary, &target) {
                Ok(()) => fs::File::open(directory)?.sync_all()?,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            Ok(())
        })();
        let _ = fs::remove_file(&temporary);
        result.map_err(|error| format!("freeze local input authorization: {error}"))
    }
}

fn valid_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn valid_graph_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn read_grants(path: &Path, snapshot: &GraphSnapshot) -> Result<Grants, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(format!("cannot read operator local-inputs.json: {error}")),
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid local-inputs.json: {error}"))?;
    let nodes = value
        .as_object()
        .ok_or("local-inputs.json must map existing node IDs to named paths")?;
    let mut grants = BTreeMap::new();
    for (node, inputs) in nodes {
        if !snapshot.nodes.iter().any(|candidate| candidate.id == *node) {
            return Err(format!(
                "local-inputs.json references unknown node `{node}`"
            ));
        }
        let inputs = inputs
            .as_object()
            .ok_or("local inputs must map names to absolute paths")?;
        let mut sources = BTreeMap::new();
        for (name, source) in inputs {
            if !valid_name(name) {
                return Err(format!("invalid local input name `{name}`"));
            }
            let source = Path::new(
                source
                    .as_str()
                    .ok_or("local input paths must be absolute strings")?,
            );
            if !source.is_absolute() {
                return Err("local input paths must be absolute strings".into());
            }
            let source = source
                .canonicalize()
                .map_err(|error| format!("local input path cannot be resolved: {error}"))?;
            if ["/", "/root", "/tmp"]
                .iter()
                .any(|path| source == Path::new(path))
                || std::env::var_os("HOME").is_some_and(|home| source == Path::new(&home))
                || ["/workspace", "/in", "/plugins", "/tools", "/proc", "/dev"]
                    .iter()
                    .any(|path| source.starts_with(path))
            {
                return Err(format!("invalid local input mount: {}", source.display()));
            }
            sources.insert(name.clone(), source);
        }
        grants.insert(node.clone(), sources);
    }
    Ok(grants)
}

#[cfg(test)]
mod tests;
