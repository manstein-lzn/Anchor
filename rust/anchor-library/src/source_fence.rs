//! A publication outage marker, not a version or authorization authority. The
//! Host reconstructs it from its unfinished durable effects before opening work.

use crate::{InstallLease, Library, RevisionError, filesystem};
use anchor_runtime::artifact::store::valid_sha256;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginPublicationFence {
    /// Digest of the original accepted Execute fact.
    pub execute_digest: String,
    /// Digest of the original publication intent, not another effect fact.
    pub intent_digest: String,
}

impl PluginPublicationFence {
    fn validate(&self) -> Result<(), RevisionError> {
        if !valid_sha256(&self.execute_digest) || !valid_sha256(&self.intent_digest) {
            return Err(RevisionError::Invalid);
        }
        Ok(())
    }
}

fn path(root: &Path, id: &str) -> PathBuf {
    root.join(anchor_graph_host::DEPENDENCIES_DIRECTORY)
        .join(".publication-fences")
        .join(format!("{id}.json"))
}

pub(crate) fn ensure_default_available(root: &Path, id: &str) -> Result<(), RevisionError> {
    crate::validate_id(id)?;
    let marker = path(root, id);
    match anchor_file_lease::open_regular(&marker) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        // Malformed, linked or unreadable markers still mean unavailable.
        _ => Err(RevisionError::DefaultUnavailable),
    }
}

fn read(root: &Path, id: &str) -> Result<Option<PluginPublicationFence>, RevisionError> {
    let marker = path(root, id);
    let bytes = match anchor_file_lease::read_regular(&marker, 1024) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let fence: PluginPublicationFence =
        serde_json::from_slice(&bytes).map_err(|_| RevisionError::Changed)?;
    fence.validate()?;
    Ok(Some(fence))
}

impl Library {
    pub fn has_publication_fence(&self, id: &str) -> Result<bool, RevisionError> {
        crate::validate_id(id)?;
        let root = match self.root.canonicalize() {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        match ensure_default_available(&root, id) {
            Ok(()) => Ok(false),
            Err(RevisionError::DefaultUnavailable) => Ok(true),
            Err(error) => Err(error),
        }
    }

    /// The inspector deliberately bypasses the execution outage. Only the
    /// effect owner may classify the raw default and subsequently clear it.
    pub fn publication_fence_locked(
        &self,
        id: &str,
        lease: &InstallLease,
    ) -> Result<Option<PluginPublicationFence>, RevisionError> {
        Self::validate_extension_install(id)?;
        read(&self.leased_root(lease)?, id)
    }

    pub fn set_publication_fence_locked(
        &self,
        id: &str,
        fence: &PluginPublicationFence,
        lease: &InstallLease,
    ) -> Result<(), RevisionError> {
        Self::validate_extension_install(id)?;
        fence.validate()?;
        let root = self.leased_root(lease)?;
        if let Some(existing) = read(&root, id)? {
            return if existing == *fence {
                Ok(())
            } else {
                Err(RevisionError::Changed)
            };
        }
        let marker = path(&root, id);
        let parent = marker.parent().expect("fence parent");
        let (_, directory) = filesystem::directory(parent, true)?;
        let mut staged = tempfile::NamedTempFile::new_in(parent)?;
        staged.write_all(&serde_json::to_vec(fence).map_err(|_| RevisionError::Invalid)?)?;
        staged.as_file().sync_all()?;
        staged
            .persist_noclobber(marker)
            .map_err(|error| error.error)?;
        directory.sync_all()?;
        Ok(())
    }

    /// Removal is an exact effect CAS, never inferred from the installed code.
    pub fn clear_publication_fence_locked(
        &self,
        id: &str,
        expected: &PluginPublicationFence,
        lease: &InstallLease,
    ) -> Result<(), RevisionError> {
        Self::validate_extension_install(id)?;
        expected.validate()?;
        let root = self.leased_root(lease)?;
        match read(&root, id)? {
            None => return Ok(()),
            Some(current) if current == *expected => {}
            Some(_) => return Err(RevisionError::Changed),
        }
        let marker = path(&root, id);
        let (_, directory) = filesystem::directory(marker.parent().expect("fence parent"), false)?;
        fs::remove_file(marker)?;
        directory.sync_all()?;
        Ok(())
    }
}
