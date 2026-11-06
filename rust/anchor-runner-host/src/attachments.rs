//! Shared bounded attachment bytes and immutable storage; identity belongs to callers.
use crate::create_durable_directory;
use crate::host_fs::sync_directory;
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const MIB: usize = 1024 * 1024;
pub(crate) const MAX_BODY_BYTES: usize = 72 * MIB;
const MAX_FILES: usize = 16;
pub(crate) const MAX_FILE_BYTES: usize = 20 * MIB;
pub(crate) const MAX_TOTAL_BYTES: usize = 50 * MIB;
const MAX_IMAGES: usize = 8;
pub(crate) const MAX_IMAGE_BYTES: usize = 10 * MIB;
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

#[derive(Debug, Default)]
pub(crate) struct PreparedAttachments(pub(crate) Vec<(AttachmentManifest, Vec<u8>)>);

impl PreparedAttachments {
    pub(crate) fn manifest(&self) -> Vec<AttachmentManifest> {
        self.0.iter().map(|(item, _)| item.clone()).collect()
    }
}

pub(crate) fn safe_name(name: &str) -> bool {
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

pub(crate) fn checked_mime(bytes: &[u8], declared: Option<&str>) -> Result<Option<String>, String> {
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
pub(crate) struct Sizes {
    files: usize,
    total: u64,
    images: usize,
    image_total: u64,
}

impl Sizes {
    pub(crate) fn add(&mut self, size: u64, media_type: Option<&str>) -> Result<(), String> {
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

pub(crate) fn checked_path(path: &Path) -> Result<(), String> {
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

pub(crate) fn write_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| error.to_string())
}

pub(crate) fn freeze_directory(
    target: &Path,
    record_name: &str,
    record: &[u8],
    prepared: &PreparedAttachments,
) -> Result<(), String> {
    checked_path(target)?;
    let parent = target
        .parent()
        .ok_or("attachment directory has no parent")?;
    create_durable_directory(parent).map_err(|error| error.to_string())?;
    let temporary = parent.join(format!(
        ".attachment-{}-{}.tmp",
        std::process::id(),
        crate::run_metadata::now_nanos()
    ));
    fs::create_dir(&temporary).map_err(|error| error.to_string())?;
    let result = (|| {
        let files = temporary.join("files");
        fs::create_dir(&files).map_err(|error| error.to_string())?;
        for (item, bytes) in &prepared.0 {
            write_file(&files.join(&item.name), bytes)?;
        }
        sync_directory(&files)?;
        write_file(&temporary.join(record_name), record)?;
        sync_directory(&temporary)?;
        fs::rename(&temporary, target).map_err(|error| error.to_string())?;
        sync_directory(parent)
    })();
    if temporary.exists() {
        let _ = fs::remove_dir_all(temporary);
    }
    result
}

pub(crate) fn from_bytes(
    name: String,
    bytes: Vec<u8>,
    declared: Option<&str>,
) -> Result<PreparedAttachments, String> {
    if !safe_name(&name) {
        return Err("attachment name must be a safe file name".into());
    }
    let media_type = checked_mime(&bytes, declared)?;
    Sizes::default().add(bytes.len() as u64, media_type.as_deref())?;
    if let Some(mime) = media_type
        .as_deref()
        .filter(|mime| mime.starts_with("image/"))
    {
        validate_image(&bytes, mime)?;
    }
    Ok(PreparedAttachments(vec![(
        AttachmentManifest {
            name,
            size: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            media_type,
        },
        bytes,
    )]))
}

pub(crate) fn read_bytes(files: &Path, item: &AttachmentManifest) -> Result<Vec<u8>, String> {
    if !safe_name(&item.name)
        || item.sha256.len() != 64
        || !item.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("invalid attachment identity".into());
    }
    Sizes::default().add(item.size, item.media_type.as_deref())?;
    checked_path(&files.join(&item.name))?;
    let mut file = crate::resource_read::open_resource(files, &item.name)
        .map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() != item.size {
        return Err("attachment size changed".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 != item.size || format!("{:x}", Sha256::digest(&bytes)) != item.sha256 {
        return Err("attachment bytes changed".into());
    }
    if checked_mime(&bytes, item.media_type.as_deref())? != item.media_type {
        return Err("attachment MIME changed".into());
    }
    if let Some(mime) = item
        .media_type
        .as_deref()
        .filter(|mime| mime.starts_with("image/"))
    {
        validate_image(&bytes, mime)?;
    }
    Ok(bytes)
}
