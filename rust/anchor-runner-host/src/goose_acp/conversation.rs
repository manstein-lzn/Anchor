use super::{Fact, read_fact};
#[cfg(test)]
mod tests;
use anchor_runtime_rig::graph::{GraphError, InvocationKey};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    version: u32,
    scope: String,
    binary_sha256: String,
    model_binding: String,
    session_id: Option<String>,
    latest: Option<InvocationKey>,
}

pub(super) struct ConversationScope {
    pub(super) root: PathBuf,
    pub(super) scope: String,
    binding: Binding,
    pub(super) previous_observation: Option<serde_json::Value>,
    _lease: File,
}

fn scope_name(hint: &str) -> Result<String, String> {
    if hint.len() != 64
        || !hint
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("invalid Goose conversation scope".into());
    }
    Ok(format!("gc1-{hint}"))
}

fn lease(process_root: &Path, scope: &str) -> Result<File, String> {
    let directory = process_root.join("conversation-locks");
    super::pilot::directory(&directory)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(directory.join(format!("{scope}.lock")))
        .map_err(|_| "Goose conversation lease unavailable")?;
    if !file
        .metadata()
        .map_err(|_| "Goose conversation lease unavailable")?
        .is_file()
    {
        return Err("Goose conversation lease is not a regular file".into());
    }
    rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| "Goose conversation is already active")?;
    Ok(file)
}

fn load(root: &Path) -> Result<Option<Binding>, String> {
    let mut file = match crate::resource_read::open_resource(root, "scope.json") {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Goose conversation binding unavailable".into()),
    };
    if file
        .metadata()
        .map_err(|_| "Goose conversation binding unavailable")?
        .len()
        > 65536
    {
        return Err("Goose conversation binding exceeds its size limit".into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| "Goose conversation binding unavailable")?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "Goose conversation binding malformed".into())
}

impl ConversationScope {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn open(
        process_root: &Path,
        hint: &str,
        key: &InvocationKey,
        facts: &Path,
        binary: &str,
        model: &str,
        current: Option<&Fact>,
        predecessors: &[InvocationKey],
    ) -> Result<Self, GraphError> {
        let scope = scope_name(hint).map_err(GraphError::Unsupported)?;
        let held = lease(process_root, &scope).map_err(GraphError::Unsupported)?;
        let root = process_root.join("conversations").join(&scope);
        super::pilot::directory(&root).map_err(GraphError::Unsupported)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        if let Some(current) = current
            && (current.conversation_scope.as_deref() != Some(scope.as_str())
                || current.key != *key
                || current.version != 2
                || current.binary_sha256 != binary
                || current.model_binding.as_deref() != Some(model))
        {
            return Err(GraphError::CorruptRun(
                "Goose invocation conversation identity changed".into(),
            ));
        }
        let saved_binding = load(&root).map_err(GraphError::Unsupported)?;
        let latest = saved_binding
            .as_ref()
            .and_then(|binding| binding.latest.as_ref());
        let mut previous = None;
        for predecessor in predecessors {
            let path = facts.join(format!("{}.json", super::GooseNodePort::stem(predecessor)));
            if let Some(fact) = read_fact(&path)? {
                if fact.key != *predecessor
                    || fact.version != 2
                    || fact.conversation_scope.as_deref() != Some(scope.as_str())
                    || fact.binary_sha256 != binary
                    || fact.model_binding.as_deref() != Some(model)
                {
                    return Err(GraphError::CorruptRun(
                        "Goose predecessor conversation identity changed".into(),
                    ));
                }
                if latest == Some(&fact.key) || latest == Some(key) || latest.is_none() {
                    previous = Some(fact);
                    break;
                }
                if previous.is_none() {
                    previous = Some(fact);
                }
            }
            #[cfg(not(feature = "legacy-regression"))]
            if crate::run_data::legacy_invocation_exists(facts.parent().unwrap(), predecessor)
                .map_err(GraphError::Unsupported)?
            {
                return Err(GraphError::Unsupported(
                    "legacy conversation history cannot silently become Goose history".into(),
                ));
            }
        }
        let mut binding = match saved_binding {
            Some(binding) => binding,
            None if current.is_none() && previous.is_none() => Binding {
                version: 1,
                scope: scope.clone(),
                binary_sha256: binary.into(),
                model_binding: model.into(),
                session_id: None,
                latest: None,
            },
            None => {
                return Err(GraphError::CorruptRun(
                    "Goose conversation binding missing; retained history cannot be replaced"
                        .into(),
                ));
            }
        };
        if binding.version != 1
            || binding.scope != scope
            || binding.binary_sha256 != binary
            || binding.model_binding != model
        {
            return Err(GraphError::CorruptRun(
                "Goose conversation binary/model/scope binding changed".into(),
            ));
        }
        if binding.latest.is_none() && binding.session_id.is_some() {
            return Err(GraphError::CorruptRun(
                "Goose native session has no invocation identity".into(),
            ));
        }
        if let Some(latest) = &binding.latest {
            if latest == key {
                if current.is_none() {
                    return Err(GraphError::CorruptRun(
                        "Goose latest invocation fact is missing".into(),
                    ));
                }
            } else if previous.as_ref().map(|fact| &fact.key) != Some(latest) {
                return Err(GraphError::CorruptRun(
                    "Goose conversation latest invocation is outside the trusted predecessor chain"
                        .into(),
                ));
            }
        } else if previous.is_some() {
            return Err(GraphError::CorruptRun(
                "Goose conversation lost its predecessor identity".into(),
            ));
        }
        if let Some(session) = &binding.session_id {
            let retained = current
                .or(previous.as_ref())
                .and_then(|fact| fact.session_id.as_ref());
            if retained != Some(session) || session.is_empty() {
                return Err(GraphError::CorruptRun(
                    "Goose conversation native session differs from retained invocation".into(),
                ));
            }
        }
        if binding.session_id.is_none() {
            binding.session_id = current
                .or(previous.as_ref())
                .and_then(|fact| fact.session_id.clone());
        }
        let previous_observation = previous
            .filter(|fact| fact.completion.is_none())
            .and_then(|fact| fact.tool_observation);
        let state = Self {
            root,
            scope,
            binding,
            previous_observation,
            _lease: held,
        };
        state.save().map_err(GraphError::Unsupported)?;
        Ok(state)
    }

    pub(super) fn session_id(&self) -> Option<String> {
        self.binding.session_id.clone()
    }

    pub(super) fn claim(&mut self, fact: &Fact) -> Result<(), String> {
        if let Some(session) = &self.binding.session_id
            && fact.session_id.as_ref() != Some(session)
        {
            return Err("Goose invocation cannot replace the bound native session".into());
        }
        self.binding.latest = Some(fact.key.clone());
        self.binding.session_id = fact.session_id.clone();
        self.save()
    }

    fn save(&self) -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(self.root.join("scope.json.tmp"))
            .map_err(|_| "Goose conversation binding could not be opened safely")?;
        file.write_all(
            &serde_json::to_vec(&self.binding)
                .map_err(|_| "Goose conversation binding cannot be encoded")?,
        )
        .and_then(|_| file.sync_all())
        .map_err(|_| "Goose conversation binding could not be persisted")?;
        fs::rename(
            self.root.join("scope.json.tmp"),
            self.root.join("scope.json"),
        )
        .map_err(|_| "Goose conversation binding could not be committed")?;
        File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(|_| "Goose conversation directory could not be persisted".into())
    }
}

pub(crate) fn remove(process_root: &Path, hint: &str) -> Result<(), String> {
    let scope = scope_name(hint)?;
    let root = process_root.join("conversations").join(&scope);
    let _held = lease(process_root, &scope)?;
    let metadata = match fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.file_type().is_symlink() {
        return Err("Goose conversation directory must not be a symlink".into());
    }
    super::pilot::directory(&root)?;
    fs::remove_dir_all(&root).map_err(|error| error.to_string())
}
