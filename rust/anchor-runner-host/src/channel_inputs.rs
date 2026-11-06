//! Run-owned attachment identity and authorization; bounded bytes use shared Host primitives.
pub(crate) use crate::attachments::{
    AttachmentManifest, MAX_BODY_BYTES, PreparedAttachments, UploadedAttachment, prepare,
};
use crate::attachments::{Sizes, checked_mime, checked_path, safe_name};
use crate::host_fs::sync_directory;
use crate::run_metadata::RunMetadata;
use anchor_runtime::{ReadOnlyInput, graph::InvocationKey};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

pub(crate) struct ChannelImage {
    pub(crate) bytes: Vec<u8>,
    pub(crate) media_type: String,
}
pub(crate) struct ChannelNodeInputs {
    pub(crate) mount: Option<ReadOnlyInput>,
    pub(crate) images: Vec<ChannelImage>,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct FrozenInputs {
    format: u32,
    run_id: String,
    graph_digest: String,
    attachments: Vec<AttachmentManifest>,
}

fn safe_run(run: &str) -> bool {
    run.len() <= 200 && anchor_runtime::identifier::is_name(run)
}

pub(crate) fn input_directory(data_root: &Path, run: &str) -> Result<PathBuf, String> {
    if !safe_run(run) {
        return Err("invalid channel input Run id".into());
    }
    Ok(data_root.join("channel-inputs").join(run))
}

pub(crate) fn freeze(
    data_root: &Path,
    metadata: &RunMetadata,
    prepared: &PreparedAttachments,
) -> Result<(), String> {
    if metadata.attachments != prepared.manifest() {
        return Err("channel attachment manifest differs from the request bytes".into());
    }
    let target = input_directory(data_root, &metadata.run_id)?;
    checked_path(&target)?;
    if target.exists() {
        return verify(data_root, metadata);
    }
    if metadata.attachments.is_empty() {
        return Ok(());
    }
    let frozen = FrozenInputs {
        format: 1,
        run_id: metadata.run_id.clone(),
        graph_digest: metadata.graph_digest.clone(),
        attachments: metadata.attachments.clone(),
    };
    crate::attachments::freeze_directory(
        &target,
        "input.json",
        &serde_json::to_vec(&frozen).map_err(|error| error.to_string())?,
        prepared,
    )?;
    verify(data_root, metadata)
}

fn read_verified(data_root: &Path, metadata: &RunMetadata) -> Result<Vec<ChannelImage>, String> {
    let directory = input_directory(data_root, &metadata.run_id)?;
    checked_path(&directory)?;
    if metadata.attachments.is_empty() {
        return if directory.exists() {
            Err("unexpected channel inputs for a Run without attachments".into())
        } else {
            Ok(Vec::new())
        };
    }
    if metadata.conversation.is_none() {
        return Err("channel attachments require a trusted conversation Run".into());
    }
    let manifest_path = directory.join("input.json");
    checked_path(&manifest_path)?;
    if !fs::symlink_metadata(&manifest_path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("channel attachment manifest is not a regular file".into());
    }
    let mut raw = Vec::new();
    File::open(&manifest_path)
        .map_err(|error| error.to_string())?
        .take(64 * 1024 + 1)
        .read_to_end(&mut raw)
        .map_err(|error| error.to_string())?;
    if raw.len() > 64 * 1024 {
        return Err("channel input manifest exceeds its limit".into());
    }
    let frozen: FrozenInputs = serde_json::from_slice(&raw)
        .map_err(|_| "invalid frozen channel input manifest".to_owned())?;
    if frozen.format != 1
        || frozen.run_id != metadata.run_id
        || frozen.graph_digest != metadata.graph_digest
        || frozen.attachments != metadata.attachments
    {
        return Err("channel attachment identity differs from the admitted Run".into());
    }
    let files = directory.join("files");
    checked_path(&files)?;
    let entries = fs::read_dir(&files)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            let entry = entry.map_err(|error| error.to_string())?;
            entry
                .file_name()
                .into_string()
                .map_err(|_| "channel attachment file name is not UTF-8".into())
        })
        .collect::<Result<BTreeSet<String>, String>>()?;
    let mut names = BTreeSet::new();
    let mut sizes = Sizes::default();
    let mut images = Vec::new();
    for item in &metadata.attachments {
        if !safe_name(&item.name)
            || !names.insert(item.name.clone())
            || item.sha256.len() != 64
            || !item.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("invalid channel attachment identity".into());
        }
        sizes.add(item.size, item.media_type.as_deref())?;
        let bytes = crate::attachments::read_bytes(&files, item)?;
        if let Some(media_type) = item
            .media_type
            .as_ref()
            .filter(|mime| mime.starts_with("image/"))
        {
            if checked_mime(&bytes, Some(media_type))?.as_ref() != Some(media_type) {
                return Err("frozen image attachment MIME changed".into());
            }
            images.push(ChannelImage {
                bytes,
                media_type: media_type.clone(),
            });
        }
    }
    if entries != names {
        return Err("channel attachment file set changed".into());
    }
    Ok(images)
}

pub(crate) fn verify(data_root: &Path, metadata: &RunMetadata) -> Result<(), String> {
    read_verified(data_root, metadata).map(|_| ())
}

pub(crate) fn node_inputs(
    data_root: &Path,
    metadata: &RunMetadata,
    key: &InvocationKey,
) -> Result<ChannelNodeInputs, String> {
    if key.run_id != metadata.run_id
        || key.graph_digest != metadata.graph_digest
        || key.node_id.is_empty()
        || key.invocation == 0
    {
        return Err("channel input invocation differs from the admitted Run".into());
    }
    let images = read_verified(data_root, metadata)?;
    let mount = (!metadata.attachments.is_empty()).then(|| ReadOnlyInput {
        source: input_directory(data_root, &metadata.run_id)
            .expect("validated Run id")
            .join("files"),
        destination: "/in/channel".into(),
    });
    Ok(ChannelNodeInputs { mount, images })
}

pub(crate) fn remove(data_root: &Path, run: &str) -> Result<(), String> {
    let directory = input_directory(data_root, run)?;
    checked_path(&directory)?;
    match fs::remove_dir_all(&directory) {
        Ok(()) => sync_directory(directory.parent().expect("Run input directory has parent")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests;
