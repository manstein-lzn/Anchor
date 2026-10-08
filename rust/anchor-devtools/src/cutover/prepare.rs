use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

use rustix::fs::{
    AtFlags, FlockOperation, Mode, OFlags, RenameFlags, fchmod, flock, fstat, openat,
    renameat_with, unlinkat,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::filesystem::{self, Snapshot, Stamp};

const NAMES: [&str; 2] = ["backup-index.json", "cutover-manifest.json"];
static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

fn payloads(report: &Value) -> Result<[Vec<u8>; 2], String> {
    let mut manifest = report.clone();
    manifest
        .as_object_mut()
        .unwrap()
        .remove("preparation_output");
    manifest["preparation"] = json!({
        "mode": "prepare-only",
        "data_copied": false,
        "data_moved_or_deleted": false,
        "dual_write_enabled": false,
        "secrets_copied": false,
        "backup_index": "backup-index.json",
    });
    let inventory = &report["inventory"];
    let index = json!({
        "schema_version": 1,
        "kind": "path-and-metadata index; no data backup was made",
        "source_root": inventory["legacy_root"],
        "entry_count": inventory["entry_count"],
        "file_count": inventory["file_count"],
        "bytes": inventory["bytes"],
        "entries": inventory["paths"],
    });
    let encode = |value: &Value| -> Result<Vec<u8>, String> {
        let mut bytes = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        Ok(bytes)
    };
    Ok([encode(&index)?, encode(&manifest)?])
}

fn existing(directory: &File, payloads: &[Vec<u8>; 2]) -> Result<bool, String> {
    let names = filesystem::names(directory).map_err(|error| error.to_string())?;
    let metadata = fstat(directory).map_err(|error| error.to_string())?;
    let expected = NAMES
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let unexpected = names
        .iter()
        .filter(|name| !expected.contains(*name))
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(format!(
            "output directory is not isolated; unexpected entries: {unexpected:?}"
        ));
    }
    if metadata.st_mode & 0o077 != 0 {
        return Err(
            "existing output directory must already be private (mode 0700 or stricter)".into(),
        );
    }
    if names.is_empty() {
        return Ok(false);
    }
    if names.len() != NAMES.len() {
        return Err(
            "output contains an incomplete preparation; use a new or empty private directory"
                .into(),
        );
    }
    for (name, payload) in NAMES.iter().zip(payloads) {
        let file = filesystem::regular(directory, name)
            .map_err(|error| format!("unsafe preparation file {name}: {error}"))?;
        let metadata = fstat(&file).map_err(|error| error.to_string())?;
        if metadata.st_nlink != 1
            || metadata.st_mode & 0o077 != 0
            || metadata.st_size as u128 != payload.len() as u128
        {
            return Err(format!(
                "existing preparation file is unsafe or differs: {name}"
            ));
        }
        let mut content = Vec::new();
        (&file)
            .take(payload.len() as u64 + 1)
            .read_to_end(&mut content)
            .map_err(|error| error.to_string())?;
        if &content != payload
            || Stamp::of(&metadata) != Stamp::of(&fstat(&file).map_err(|error| error.to_string())?)
        {
            return Err(format!(
                "existing preparation differs; refusing to overwrite {name}"
            ));
        }
    }
    Ok(true)
}

fn atomic_file(directory: &File, name: &str, payload: &[u8]) -> Result<(), String> {
    let temporary = format!(
        ".{name}.{}-{}-{:x}",
        std::process::id(),
        TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed),
        Sha256::digest(payload)
    );
    let mut file = File::from(
        openat(
            directory,
            temporary.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map_err(|error| error.to_string())?,
    );
    let result = (|| {
        fchmod(&file, Mode::RUSR | Mode::WUSR).map_err(|error| error.to_string())?;
        file.write_all(payload).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        renameat_with(
            directory,
            temporary.as_str(),
            directory,
            name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|error| format!("cannot atomically publish {name}: {error}"))?;
        directory.sync_all().map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = unlinkat(directory, temporary.as_str(), AtFlags::empty());
    }
    result
}

pub(super) fn write(
    report: &Value,
    output: &Path,
    snapshots: &[Snapshot],
) -> Result<Vec<String>, String> {
    if report["decision"] == "blocked" {
        return Err("cutover is blocked; no preparation files were written".into());
    }
    if snapshots
        .iter()
        .any(|snapshot| filesystem::overlap(output, &snapshot.path))
    {
        return Err("preparation output overlaps a data root".into());
    }
    let payloads = payloads(report)?;
    let initial = filesystem::directory(output, false).map_err(|error| error.to_string())?;
    if let Some(directory) = &initial {
        existing(directory, &payloads)?;
    }
    for snapshot in snapshots {
        snapshot.verify()?;
    }
    let directory = match initial {
        Some(directory) => directory,
        None => filesystem::directory(output, true)
            .map_err(|error| error.to_string())?
            .ok_or("could not create output directory")?,
    };
    flock(&directory, FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| "preparation output is locked or cannot be checked")?;
    let current = filesystem::directory(output, false)
        .map_err(|error| error.to_string())?
        .ok_or("preparation output disappeared")?;
    if Stamp::of(&fstat(&directory).map_err(|error| error.to_string())?)
        != Stamp::of(&fstat(&current).map_err(|error| error.to_string())?)
    {
        return Err("preparation output changed during validation".into());
    }
    if !existing(&directory, &payloads)? {
        for snapshot in snapshots {
            snapshot.verify()?;
        }
        for (name, payload) in NAMES.iter().zip(&payloads) {
            atomic_file(&directory, name, payload)?;
        }
    }
    Ok(NAMES
        .iter()
        .map(|name| output.join(name).to_string_lossy().into_owned())
        .collect())
}
