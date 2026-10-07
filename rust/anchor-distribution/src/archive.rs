use crate::{ResourceFile, Result};
use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub(crate) fn build(
    staging: &Path,
    entries: &BTreeMap<String, Option<ResourceFile>>,
) -> Result<File> {
    let mut archive = tempfile::tempfile()?;
    let compressed = GzBuilder::new()
        .mtime(0)
        .operating_system(255)
        .write(&mut archive, Compression::new(6));
    let mut builder = tar::Builder::new(compressed);
    builder.mode(tar::HeaderMode::Deterministic);
    for (path, resource) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_username("")?;
        header.set_groupname("")?;
        let name = format!("anchor-runtime/{path}");
        match resource {
            None => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mode(0o755);
                header.set_size(0);
                builder.append_data(&mut header, name, std::io::empty())?;
            }
            Some(resource) => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_mode(resource.mode);
                header.set_size(resource.size);
                builder.append_data(&mut header, name, File::open(staging.join(path))?)?;
            }
        }
    }
    builder.into_inner()?.finish()?;
    archive.seek(SeekFrom::Start(0))?;
    Ok(archive)
}

pub(crate) fn sha256(file: &mut File) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", digest.finalize()))
}
