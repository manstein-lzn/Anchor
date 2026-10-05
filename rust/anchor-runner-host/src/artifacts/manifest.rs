use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub format: u32,
    pub key: InvocationKey,
    pub completion: NodeCompletion,
    pub files: BTreeMap<String, FileHash>,
    pub directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<ArtifactFreezeContext>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileHash {
    pub sha256: String,
    pub bytes: u64,
}

pub(super) fn context_hash(context: &ArtifactFreezeContext) -> Result<String, GraphError> {
    let bytes = serde_json::to_vec(context).map_err(|error| corrupt(error.to_string()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

pub(super) fn control_output(completion: &NodeCompletion, kind: ArtifactKind) -> Value {
    let mut output = completion.output.clone();
    if kind == ArtifactKind::Join {
        // Kernel has no filesystem inventory. Its historical empty `files`
        // placeholders must not be presented as an authoritative file list.
        if let Some(branches) = output.get_mut("branches").and_then(Value::as_array_mut) {
            for branch in branches {
                if let Some(nodes) = branch.get_mut("nodes").and_then(Value::as_array_mut) {
                    for node in nodes {
                        if let Some(fields) = node.as_object_mut() {
                            fields.remove("files");
                        }
                    }
                }
            }
        }
    }
    output
}

impl Manifest {
    pub fn validate_context(&self) -> Result<(), GraphError> {
        match (self.format, &self.context, &self.context_sha256) {
            (1, None, None) => Ok(()),
            (2, Some(context), Some(hash)) if context_hash(context)? == *hash => {
                let mut ids = std::collections::BTreeSet::new();
                for input in &context.input_commits {
                    validate_relative(&input.node_id)?;
                    if !valid_fs_id(&input.id) || input.invocation == 0 || !ids.insert(&input.id) {
                        return Err(corrupt("invalid or duplicate artifact parent reference"));
                    }
                }
                match context.kind {
                    ArtifactKind::Node => Ok(()),
                    // A Graph call commit carries only the selected child result
                    // files copied into the call node workspace. `read_snapshot`
                    // already re-derives and verifies the exact tree, so any
                    // declared files are the authoritative result inventory.
                    ArtifactKind::GraphCall => Ok(()),
                    ArtifactKind::Fanout | ArtifactKind::Join => {
                        let filename = match context.kind {
                            ArtifactKind::Fanout => "fanout.json",
                            _ => "join.json",
                        };
                        if self.files.len() != 1
                            || !self.files.contains_key(filename)
                            || !self.directories.is_empty()
                        {
                            return Err(corrupt("control artifact contains unexpected files"));
                        }
                        let bytes =
                            serde_json::to_vec(&control_output(&self.completion, context.kind))
                                .map_err(|error| corrupt(error.to_string()))?;
                        let expected = FileHash {
                            sha256: format!("{:x}", Sha256::digest(&bytes)),
                            bytes: bytes.len() as u64,
                        };
                        if self.files[filename] != expected {
                            return Err(corrupt(
                                "control file does not match Coordinator completion",
                            ));
                        }
                        Ok(())
                    }
                }
            }
            _ => Err(corrupt("artifact context, checksum or format mismatch")),
        }
    }
}
