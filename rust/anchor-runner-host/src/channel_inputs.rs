//! Request bytes frozen as Run-owned inputs. No attachment grants a host path.
use crate::{application::RunMetadata, create_durable_directory};
use anchor_runtime_rig::{ReadOnlyInput, graph::InvocationKey};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const MIB: usize = 1024 * 1024;
pub(crate) const MAX_BODY_BYTES: usize = 72 * MIB;
const MAX_FILES: usize = 16;
const MAX_FILE_BYTES: usize = 20 * MIB;
const MAX_TOTAL_BYTES: usize = 50 * MIB;
const MAX_IMAGES: usize = 8;
const MAX_IMAGE_BYTES: usize = 10 * MIB;
const MAX_IMAGE_TOTAL_BYTES: usize = 20 * MIB;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UploadedAttachment {
    pub(crate) name: String,
    pub(crate) data_base64: String,
    #[serde(default)]
    pub(crate) media_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AttachmentManifest {
    pub(crate) name: String,
    pub(crate) sha256: String,
    pub(crate) size: u64,
    pub(crate) media_type: Option<String>,
}

#[derive(Debug)]
pub(crate) struct PreparedAttachments(Vec<(AttachmentManifest, Vec<u8>)>);

impl PreparedAttachments {
    pub(crate) fn manifest(&self) -> Vec<AttachmentManifest> {
        self.0.iter().map(|(item, _)| item.clone()).collect()
    }
}

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

fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !matches!(name, "." | "..")
        && !name
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\'))
}

fn image_mime(format: ImageFormat) -> Result<&'static str, String> {
    match format {
        ImageFormat::Png => Ok("image/png"),
        ImageFormat::Jpeg => Ok("image/jpeg"),
        ImageFormat::WebP => Ok("image/webp"),
        _ => Err("only PNG, JPEG and WebP image attachments are supported".into()),
    }
}

fn checked_mime(bytes: &[u8], declared: Option<&str>) -> Result<Option<String>, String> {
    let declared = declared
        .map(|value| {
            value
                .parse::<mime::Mime>()
                .map_err(|_| "attachment media_type is not a valid MIME type".to_owned())
        })
        .transpose()?;
    match image::guess_format(bytes) {
        Ok(format) => {
            let actual = image_mime(format)?;
            if declared
                .as_ref()
                .is_some_and(|mime| mime.essence_str() != actual)
            {
                return Err("attachment media_type does not match the image bytes".into());
            }
            Ok(Some(actual.into()))
        }
        Err(_)
            if declared
                .as_ref()
                .is_some_and(|mime| mime.type_() == mime::IMAGE) =>
        {
            Err("image attachment bytes do not contain a supported image".into())
        }
        Err(_) => Ok(declared.map(|mime| mime.to_string())),
    }
}

pub(crate) fn validate_image(bytes: &[u8], media_type: &str) -> Result<(), String> {
    anchor_mcp_host::validate_image_bytes(bytes, media_type)
}

#[derive(Default)]
struct Sizes {
    files: usize,
    total: u64,
    images: usize,
    image_total: u64,
}

impl Sizes {
    fn add(&mut self, size: u64, media_type: Option<&str>) -> Result<(), String> {
        self.files += 1;
        self.total = self
            .total
            .checked_add(size)
            .ok_or("attachment size overflow")?;
        if self.files > MAX_FILES
            || size > MAX_FILE_BYTES as u64
            || self.total > MAX_TOTAL_BYTES as u64
        {
            return Err("attachments exceed 16 files, 20 MiB per file or 50 MiB total".into());
        }
        if media_type.is_some_and(|mime| mime.starts_with("image/")) {
            self.images += 1;
            self.image_total = self
                .image_total
                .checked_add(size)
                .ok_or("image size overflow")?;
            if self.images > MAX_IMAGES
                || size > MAX_IMAGE_BYTES as u64
                || self.image_total > MAX_IMAGE_TOTAL_BYTES as u64
            {
                return Err(
                    "image attachments exceed 8 images, 10 MiB per image or 20 MiB total".into(),
                );
            }
        }
        Ok(())
    }
}

pub(crate) fn prepare(uploads: &[UploadedAttachment]) -> Result<PreparedAttachments, String> {
    if uploads.len() > MAX_FILES {
        return Err("attachments contain more than 16 files".into());
    }
    let mut names = BTreeSet::new();
    for item in uploads {
        if !safe_name(&item.name) || !names.insert(&item.name) {
            return Err("attachment names must be unique safe file names".into());
        }
    }
    let mut sizes = Sizes::default();
    let mut decoded = Vec::new();
    for item in uploads {
        if item.data_base64.len() > MAX_FILE_BYTES.div_ceil(3) * 4 {
            return Err("attachment exceeds the 20 MiB file limit".into());
        }
        let bytes = STANDARD
            .decode(&item.data_base64)
            .map_err(|_| "attachment data_base64 is invalid".to_owned())?;
        let media_type = checked_mime(&bytes, item.media_type.as_deref())?;
        sizes.add(bytes.len() as u64, media_type.as_deref())?;
        if let Some(mime) = media_type
            .as_deref()
            .filter(|mime| mime.starts_with("image/"))
        {
            validate_image(&bytes, mime)?;
        }
        decoded.push((
            AttachmentManifest {
                name: item.name.clone(),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size: bytes.len() as u64,
                media_type,
            },
            bytes,
        ));
    }
    Ok(PreparedAttachments(decoded))
}

fn safe_run(run: &str) -> bool {
    !run.is_empty()
        && run.len() <= 200
        && !matches!(run, "." | "..")
        && run
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

pub(crate) fn input_directory(data_root: &Path, run: &str) -> Result<PathBuf, String> {
    if !safe_run(run) {
        return Err("invalid channel input Run id".into());
    }
    Ok(data_root.join("channel-inputs").join(run))
}

fn checked_path(path: &Path) -> Result<(), String> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::RootDir => current.push(component.as_os_str()),
            _ => return Err("unsafe channel input path".into()),
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("channel input symlinks are not supported".into());
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())
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
    let parent = target.parent().expect("Run input directory has parent");
    create_durable_directory(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(
        ".{}-{}-{}.tmp",
        metadata.run_id,
        std::process::id(),
        crate::application::metadata::now_nanos()
    ));
    fs::create_dir(&temporary).map_err(|error| error.to_string())?;
    let result = (|| {
        let files = temporary.join("files");
        fs::create_dir(&files).map_err(|error| error.to_string())?;
        for (item, bytes) in &prepared.0 {
            write_file(&files.join(&item.name), bytes)?;
        }
        sync_directory(&files)?;
        let frozen = FrozenInputs {
            format: 1,
            run_id: metadata.run_id.clone(),
            graph_digest: metadata.graph_digest.clone(),
            attachments: metadata.attachments.clone(),
        };
        write_file(
            &temporary.join("input.json"),
            &serde_json::to_vec(&frozen).map_err(|error| error.to_string())?,
        )?;
        sync_directory(&temporary)?;
        fs::rename(&temporary, &target).map_err(|error| error.to_string())?;
        sync_directory(parent)?;
        verify(data_root, metadata)
    })();
    if temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
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
        let path = files.join(&item.name);
        checked_path(&path)?;
        let disk = fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if !disk.is_file() || disk.len() != item.size {
            return Err("channel attachment size or type changed".into());
        }
        let mut bytes = Vec::new();
        File::open(&path)
            .map_err(|error| error.to_string())?
            .take(MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 != item.size || format!("{:x}", Sha256::digest(&bytes)) != item.sha256
        {
            return Err("channel attachment bytes changed".into());
        }
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
