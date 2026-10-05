//! Per-call durable files around io-harness's native successful exchanges.
//! These are observations only; Harness checkpoints remain the recovery owner.

use anchor_runtime_rig::graph::InvocationKey;
use io_harness::{CompletionRequest, Error, Provider, provider::Record};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Host cleanup may remove this directory when deleting the owning invocation.
pub fn directory(io_store_root: &Path, key: &InvocationKey) -> PathBuf {
    io_store_root.join(format!(
        "np1-{:x}.recordings",
        Sha256::digest(key.durable_key().as_bytes())
    ))
}

/// Provider call IDs are adapter state rather than per-exchange recordings.
/// Keep the sidecar beside (rather than inside) the append-only recordings
/// directory so existing recording enumeration remains one entry per attempt.
pub fn call_ids_path(recordings_root: &Path) -> PathBuf {
    let name = recordings_root
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(".recordings"))
        .map(|name| format!("{name}.call-ids.json"))
        .unwrap_or_else(|| "call-ids.json".to_owned());
    recordings_root
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(name)
}

pub(crate) struct Attempt {
    path: PathBuf,
}

impl Attempt {
    pub(crate) fn begin(root: &Path, request: &CompletionRequest) -> io_harness::Result<Self> {
        private_directory(root)?;
        let mut sequence = 0;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(number) = name.to_str().and_then(|name| name.parse::<u64>().ok()) {
                sequence = sequence.max(number);
            }
        }
        let path = loop {
            sequence = sequence
                .checked_add(1)
                .ok_or_else(|| Error::Config("provider recording sequence exhausted".into()))?;
            let path = root.join(format!("{sequence:020}"));
            match create_private_directory(&path) {
                Ok(()) => break path,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        fs::File::open(root)?.sync_all()?;
        let attempt = Self { path };
        attempt.write_json(
            "request.json",
            &serde_json::to_value(request).map_err(|error| Error::Config(error.to_string()))?,
        )?;
        Ok(attempt)
    }

    pub(crate) fn rig_request(
        &self,
        request: &rig_core::completion::CompletionRequest,
    ) -> io_harness::Result<()> {
        self.write_json(
            "rig-request.json",
            &serde_json::to_value(request).map_err(|error| Error::Config(error.to_string()))?,
        )
    }

    pub(crate) fn rig_response(
        &self,
        response: &rig_core::completion::CompletionResponse,
    ) -> io_harness::Result<()> {
        let mut value =
            serde_json::to_value(response).map_err(|error| Error::Config(error.to_string()))?;
        // The typed content and reported metadata are evidence; the provider's
        // raw document can include transport details outside this contract.
        if let Some(object) = value.as_object_mut() {
            object.remove("raw");
        }
        self.write_json("rig-response.json", &value)
    }

    pub(crate) fn save<P: Provider>(&self, record: &Record<P>) -> io_harness::Result<()> {
        let target = self.path.join("recording.json");
        publish(&target, |path| {
            record.save(path)?;
            Ok(())
        })?;
        if !self.path.join("rig-response.json").is_file() {
            return Err(Error::Config(
                "typed Rig response recording is unavailable".into(),
            ));
        }
        Ok(())
    }

    pub(crate) fn outcome(&self, status: &str, error: Option<&Error>) -> io_harness::Result<()> {
        let kind = error.map(|error| match error {
            Error::Provider { kind, .. } => format!("provider_{kind:?}"),
            Error::Config(_) => "configuration".into(),
            Error::Io(_) => "io".into(),
            _ => "other".into(),
        });
        self.write_json("outcome.json", &json!({"status":status,"error_kind":kind}))
    }

    fn write_json(&self, name: &str, value: &Value) -> io_harness::Result<()> {
        let bytes =
            serde_json::to_vec_pretty(value).map_err(|error| Error::Config(error.to_string()))?;
        publish(&self.path.join(name), |path| {
            let mut file = private_file(path)?;
            file.write_all(&bytes)?;
            Ok(())
        })
    }
}

fn private_directory(path: &Path) -> io::Result<()> {
    if !path.exists() {
        fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
        match create_private_directory(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "provider recording root must be a real directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    for ancestor in path.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        fs::File::open(ancestor)?.sync_all()?;
    }
    if path.is_relative() {
        fs::File::open(".")?.sync_all()?;
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn publish(
    path: &Path,
    write: impl FnOnce(&Path) -> io_harness::Result<()>,
) -> io_harness::Result<()> {
    let parent = path.parent().expect("recording file has a parent");
    let temporary = parent.join(format!(
        ".{}-{}.tmp",
        std::process::id(),
        TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        write(&temporary)?;
        fs::File::open(&temporary)?.sync_all()?;
        fs::hard_link(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    let _ = fs::remove_file(temporary);
    result
}

#[cfg(test)]
mod tests;
