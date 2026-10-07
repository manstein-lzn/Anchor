use std::{
    fs::File,
    io::Read,
    path::{Component, Path},
};

use rustix::fs::{Mode, OFlags, open, openat};

use crate::Error;

pub const MAX_QUERY_FILE_BYTES: usize = 1_048_576;

pub fn read_queries(path: &Path) -> Result<Vec<String>, Error> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let initial = if path.is_absolute() { "/" } else { "." };
    let mut directory = open(initial, directory_flags, Mode::empty()).map_err(|_| path_error())?;
    let mut components = path
        .components()
        .filter(|component| !matches!(component, Component::RootDir | Component::CurDir))
        .peekable();
    let mut filename = None;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(path_error());
        };
        if components.peek().is_none() {
            filename = Some(name);
            break;
        }
        directory =
            openat(&directory, name, directory_flags, Mode::empty()).map_err(|_| path_error())?;
    }
    let filename = filename.ok_or_else(path_error)?;
    let descriptor = openat(
        &directory,
        filename,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| path_error())?;
    let file = File::from(descriptor);
    let metadata = file.metadata().map_err(|_| path_error())?;
    if !metadata.is_file() {
        return Err(path_error());
    }
    if metadata.len() > MAX_QUERY_FILE_BYTES as u64 {
        return Err(Error::input("queries file exceeds the 1 MiB size limit"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_QUERY_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| path_error())?;
    if bytes.len() > MAX_QUERY_FILE_BYTES {
        return Err(Error::input("queries file exceeds the 1 MiB size limit"));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| Error::input("queries file must be UTF-8"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect())
}

fn path_error() -> Error {
    Error::input(
        "queries file must be a readable regular file without symlinks or parent traversal",
    )
}
