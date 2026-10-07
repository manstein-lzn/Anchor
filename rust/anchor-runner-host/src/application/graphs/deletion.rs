use super::{GraphManagementError, RunApplication, invalid, storage};
use sha2::{Digest, Sha256};
use std::{
    fs::Metadata,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const PRECONDITION_PREFIX: &str = "graph-delete-v1:";

impl RunApplication {
    pub(crate) async fn graph_delete_precondition(
        &self,
        name: &str,
    ) -> Result<String, GraphManagementError> {
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let path = self.graph_path_checked(name)?;
        let _graph_lease = self.acquire_graph_lease_waiting(&path).await?;
        self.capture_graph_delete_precondition(name, &path)
    }

    pub(crate) async fn remove_graph_if_unchanged(
        &self,
        name: &str,
        workspace_root: &Path,
        expected: &str,
    ) -> Result<usize, GraphManagementError> {
        self.remove_graph_if_unchanged_guarded(name, workspace_root, expected, || Ok(()))
            .await
    }

    pub(crate) async fn remove_graph_if_unchanged_guarded(
        &self,
        name: &str,
        workspace_root: &Path,
        expected: &str,
        authorize: impl FnOnce() -> Result<(), GraphManagementError> + Send,
    ) -> Result<usize, GraphManagementError> {
        let _catalog_guard = self.graph_catalog_mutation_guard().await;
        let path = self.graph_path_checked(name)?;
        validate_precondition(expected)?;
        let graph_lease = self.acquire_graph_lease_waiting(&path).await?;
        let current = self.capture_graph_delete_precondition(name, &path)?;
        if current != expected {
            return Err(changed());
        }
        authorize()?;
        self.delete_graph_with_lease(&path, workspace_root, graph_lease)
            .await
            .map_err(Into::into)
    }

    fn capture_graph_delete_precondition(
        &self,
        name: &str,
        path: &Path,
    ) -> Result<String, GraphManagementError> {
        let before = fingerprint(name, path)?;
        self.load_graph(name)?;
        if fingerprint(name, path)? != before {
            return Err(changed());
        }
        Ok(format!("{PRECONDITION_PREFIX}{before}"))
    }
}

fn validate_precondition(expected: &str) -> Result<(), GraphManagementError> {
    if expected
        .strip_prefix(PRECONDITION_PREFIX)
        .is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    {
        Ok(())
    } else {
        Err(GraphManagementError::BadRequest(
            "invalid Graph delete precondition".into(),
        ))
    }
}

fn changed() -> GraphManagementError {
    GraphManagementError::Conflict(
        "Graph changed or was recreated; request a new delete confirmation".into(),
    )
}

fn resource_metadata(path: &Path) -> Result<Metadata, GraphManagementError> {
    std::fs::symlink_metadata(path).map_err(|failure| {
        if failure.kind() == std::io::ErrorKind::NotFound {
            GraphManagementError::Missing("Graph or its resources no longer exist".into())
        } else {
            storage(failure)
        }
    })
}

fn checked_root(path: &Path) -> Result<PathBuf, GraphManagementError> {
    let absolute = std::path::absolute(path).map_err(storage)?;
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component.as_os_str());
        let metadata = resource_metadata(&current)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(invalid(
                "Graph delete target must use non-symlink directories",
            ));
        }
    }
    absolute.canonicalize().map_err(storage)
}

fn fingerprint(name: &str, path: &Path) -> Result<String, GraphManagementError> {
    let root = checked_root(path)?;
    let mut digest = Sha256::new();
    hash_bytes(&mut digest, PRECONDITION_PREFIX.as_bytes());
    hash_bytes(&mut digest, name.as_bytes());
    hash_bytes(&mut digest, root.as_os_str().as_encoded_bytes());
    hash_resource(&root, &root, &mut digest)?;
    if checked_root(path)? != root {
        return Err(changed());
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn hash_bytes(digest: &mut Sha256, bytes: &[u8]) {
    digest.update((bytes.len() as u64).to_le_bytes());
    digest.update(bytes);
}

fn identity(metadata: &Metadata) -> [u64; 11] {
    [
        metadata.dev(),
        metadata.ino(),
        metadata.mode().into(),
        metadata.uid().into(),
        metadata.gid().into(),
        metadata.nlink(),
        metadata.len(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ]
}

fn hash_resource(
    root: &Path,
    path: &Path,
    digest: &mut Sha256,
) -> Result<(), GraphManagementError> {
    let metadata = resource_metadata(path)?;
    if !metadata.is_dir() && !metadata.is_file() {
        return Err(invalid(
            "Graph delete resources must be regular files or directories",
        ));
    }
    let relative = path.strip_prefix(root).map_err(invalid)?;
    hash_bytes(digest, relative.as_os_str().as_encoded_bytes());
    let before = identity(&metadata);
    for field in before {
        digest.update(field.to_le_bytes());
    }
    if metadata.is_dir() {
        let mut entries = std::fs::read_dir(path)
            .map_err(storage)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        entries.sort();
        for entry in entries {
            hash_resource(root, &entry, digest)?;
        }
    } else {
        let relative = relative
            .to_str()
            .ok_or_else(|| invalid("Graph delete resource path must be UTF-8"))?;
        let mut file = crate::resource_read::open_resource(root, relative).map_err(storage)?;
        if identity(&file.metadata().map_err(storage)?) != before {
            return Err(changed());
        }
        let mut buffer = [0; 16384];
        loop {
            let length = file.read(&mut buffer).map_err(storage)?;
            if length == 0 {
                break;
            }
            digest.update(&buffer[..length]);
        }
        if identity(&file.metadata().map_err(storage)?) != before {
            return Err(changed());
        }
    }
    if identity(&resource_metadata(path)?) != before {
        return Err(changed());
    }
    Ok(())
}
