//! Operator-installed tool environments, using the existing Library tool.json.
//! These paths are deployment grants, never fields of an editable Graph/Plugin.

use anchor_runtime::{ReadOnlyInput, SandboxEnvironment, SandboxRequest};
use anchor_sandbox_bwrap::BubblewrapPolicy;
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Default)]
pub(crate) struct ToolEnvironment {
    mounts: Vec<ReadOnlyInput>,
    dirs: Vec<PathBuf>,
    imports: Vec<PathBuf>,
}

#[cfg(test)]
mod tests;

#[derive(Deserialize)]
struct ToolSpec {
    entrypoint: PathBuf,
    environment: Option<PathBuf>,
    #[serde(default)]
    imports: Vec<PathBuf>,
}

impl ToolEnvironment {
    pub fn from_env() -> Result<Self, String> {
        std::env::var_os("ANCHOR_RUNNER_LIBRARY_ROOT")
            .map(|root| Self::load(Path::new(&root)))
            .unwrap_or_else(|| Ok(Self::default()))
    }

    pub fn load(root: &Path) -> Result<Self, String> {
        let mut result = Self::default();
        let directory = root
            .canonicalize()
            .map_err(|e| format!("tool library: {e}"))?
            .join("tools");
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(result),
            Err(error) => return Err(format!("tool library: {error}")),
        };
        let mut entries = entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort();
        for directory in entries {
            if !directory.is_dir() {
                continue;
            }
            let id = directory
                .file_name()
                .and_then(|id| id.to_str())
                .ok_or("invalid tool id")?;
            if !id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                return Err(format!("invalid tool id: {id}"));
            }
            let spec: ToolSpec = serde_json::from_slice(
                &fs::read(directory.join("tool.json")).map_err(|e| format!("tool {id}: {e}"))?,
            )
            .map_err(|e| format!("tool {id}: {e}"))?;
            let entry = if spec.entrypoint.is_absolute() {
                spec.entrypoint
            } else {
                let entry = directory
                    .join(spec.entrypoint)
                    .canonicalize()
                    .map_err(|e| e.to_string())?;
                if !entry.starts_with(directory.canonicalize().map_err(|e| e.to_string())?) {
                    return Err(format!(
                        "tool {id}: relative entrypoint escapes tool directory"
                    ));
                }
                entry
            };
            if !entry.is_file()
                || fs::metadata(&entry)
                    .map_err(|e| e.to_string())?
                    .permissions()
                    .mode()
                    & 0o111
                    == 0
            {
                return Err(format!("tool {id}: entrypoint is not executable"));
            }
            result
                .mounts
                .push(ReadOnlyInput::new(&directory, format!("/tools/{id}/files")));
            result
                .mounts
                .push(ReadOnlyInput::new(&entry, format!("/tools/{id}/run")));
            if let Some(environment) = spec.environment {
                if !environment.is_absolute()
                    || !environment.is_dir()
                    || !entry.starts_with(&environment)
                {
                    return Err(format!(
                        "tool {id}: entrypoint must belong to its absolute environment directory"
                    ));
                }
                result.mount_at_original_path(&environment)?;
                let bin = environment.join("bin");
                if bin.is_dir() {
                    let canonical_bin = bin.canonicalize().map_err(|e| e.to_string())?;
                    for directory in [bin, canonical_bin] {
                        if !result.dirs.contains(&directory) {
                            result.dirs.push(directory);
                        }
                    }
                }
                // Preserve every interpreter alias in a venv symlink chain.
                let mut python = environment.join("bin/python");
                let mut seen = BTreeSet::new();
                while python.is_symlink() {
                    if !seen.insert(python.clone()) {
                        return Err(format!("tool {id}: interpreter symlink cycle"));
                    }
                    let target = fs::read_link(&python).map_err(|e| e.to_string())?;
                    python = if target.is_absolute() {
                        target
                    } else {
                        python.parent().unwrap().join(target)
                    };
                    let parent = python.parent().ok_or("invalid interpreter path")?;
                    let prefix = if parent.file_name().is_some_and(|name| name == "bin") {
                        parent.parent().unwrap()
                    } else {
                        parent
                    };
                    result.mount_at_original_path(prefix)?;
                }
            }
            for import in spec.imports {
                result.mount_at_original_path(&import)?;
                if !result.imports.contains(&import) {
                    result.imports.push(import);
                }
            }
        }
        Ok(result)
    }

    fn mount_at_original_path(&mut self, path: &Path) -> Result<(), String> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(
                "tool environment/import paths must be absolute without parent traversal".into(),
            );
        }
        let resolved = path
            .canonicalize()
            .map_err(|e| format!("tool environment: {e}"))?;
        for candidate in [path, resolved.as_path()] {
            if ["/", "/root", "/home", "/tmp"]
                .iter()
                .any(|p| candidate == Path::new(p))
                || std::env::var_os("HOME").is_some_and(|home| candidate == Path::new(&home))
                || [
                    "/workspace",
                    "/in",
                    "/plugins",
                    "/tools",
                    "/proc",
                    "/dev",
                    "/etc",
                ]
                .iter()
                .any(|p| candidate.starts_with(p))
            {
                return Err("tool environment overlaps a reserved or broad host path".into());
            }
        }
        // Preserve both installation aliases and canonical prefixes: shebangs
        // may name either, and Sandbox PATH uses canonical tool directories.
        for destination in [path, resolved.as_path()] {
            if ["/usr", "/bin", "/lib", "/lib64", "/sbin"]
                .iter()
                .any(|p| destination.starts_with(p))
            {
                continue; // already mounted read-only by Bubblewrap
            }
            let mount = ReadOnlyInput::new(&resolved, destination);
            if !self.mounts.contains(&mount) {
                self.mounts.push(mount);
            }
        }
        Ok(())
    }

    pub fn authorize(&self, mut policy: BubblewrapPolicy) -> BubblewrapPolicy {
        for mount in &self.mounts {
            policy = policy
                .authorize_readonly_input_root(&mount.source)
                .authorize_readonly_destination_root(&mount.destination);
        }
        for directory in &self.dirs {
            policy = policy.authorize_tool_dir(directory);
        }
        policy
    }

    pub fn visible_entrypoint(&self, command: &Path) -> Option<PathBuf> {
        // Keep interpreters at their installation path so venv discovery works.
        if self
            .dirs
            .iter()
            .any(|dir| command.parent() == Some(dir.as_path()))
        {
            return None;
        }
        self.mounts.iter().find_map(|mount| {
            (mount.source == command
                && mount.destination.starts_with("/tools")
                && mount
                    .destination
                    .file_name()
                    .is_some_and(|name| name == "run"))
            .then(|| mount.destination.clone())
        })
    }

    pub fn apply(&self, request: &mut SandboxRequest) {
        request.readonly_inputs.extend(self.mounts.clone());
        request.tool_dirs.extend(self.dirs.clone());
        if !self.imports.is_empty() && !request.environment.iter().any(|v| v.key == "PYTHONPATH") {
            request.environment.push(SandboxEnvironment::new(
                "PYTHONPATH",
                self.imports
                    .iter()
                    .map(|p| p.to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(":"),
            ));
        }
    }
}
