use std::{
    fs::File,
    io::Read,
    os::fd::OwnedFd,
    path::{Component, Path},
};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, open, openat, statat};

use crate::{MAX_UPLOAD_BYTES, UploadError};

pub(crate) struct Image {
    pub name: String,
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

pub(crate) fn open_directory(path: &Path) -> Result<OwnedFd, UploadError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(UploadError::Path);
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = open("/", flags, Mode::empty()).map_err(|_| UploadError::Path)?;
    for part in path.components() {
        if let Component::Normal(name) = part {
            directory =
                openat(&directory, name, flags, Mode::empty()).map_err(|_| UploadError::Path)?;
        }
    }
    Ok(directory)
}

pub(crate) fn read_image(root: &Path, path: &Path) -> Result<Image, UploadError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        return Err(UploadError::Path);
    }
    let relative = path.strip_prefix(root).map_err(|_| UploadError::Path)?;
    let mut parts = relative.components().peekable();
    let mut directory = open_directory(root)?;
    let mut filename = None;
    while let Some(Component::Normal(name)) = parts.next() {
        if parts.peek().is_none() {
            filename = Some(name);
            break;
        }
        directory = openat(
            &directory,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| UploadError::Path)?;
    }
    let filename = filename.ok_or(UploadError::Path)?;
    let name = filename.to_str().ok_or(UploadError::Path)?;
    if name
        .chars()
        .any(|character| character.is_control() || matches!(character, '"' | '\\'))
    {
        return Err(UploadError::Path);
    }
    let mime = Path::new(name)
        .extension()
        .and_then(|suffix| suffix.to_str())
        .and_then(mime_for_extension)
        .ok_or(UploadError::Mime)?;
    let metadata =
        statat(&directory, filename, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| UploadError::Path)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Err(UploadError::Path);
    }
    let descriptor = openat(
        &directory,
        filename,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| UploadError::Path)?;
    let file = File::from(descriptor);
    let metadata = file.metadata().map_err(|_| UploadError::Path)?;
    if !metadata.is_file() {
        return Err(UploadError::Path);
    }
    if metadata.len() == 0 || metadata.len() > MAX_UPLOAD_BYTES as u64 {
        return Err(UploadError::Size);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_UPLOAD_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UploadError::Read)?;
    if bytes.is_empty() || bytes.len() > MAX_UPLOAD_BYTES {
        return Err(UploadError::Size);
    }
    Ok(Image {
        name: name.to_owned(),
        mime,
        bytes,
    })
}

fn mime_for_extension(suffix: &str) -> Option<&'static str> {
    match suffix.to_ascii_lowercase().as_str() {
        "svg" => Some("image/svg+xml"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub(crate) fn supported_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/svg+xml" | "image/png" | "image/jpeg" | "image/webp"
    )
}
