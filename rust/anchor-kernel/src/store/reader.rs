//! Disposable physical checkpoints for append-only observers. These are not
//! integrity proofs for arbitrary in-place rewrites of an old prefix. Callers
//! requiring that guarantee must replay the full bounded history.

use super::{HistoryReader, Record, Resource, StoreError, absent, encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

const ANCHOR_BYTES: u64 = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
}

impl Identity {
    fn of(metadata: &Metadata) -> Result<Self, StoreError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "physical history identity requires a Unix filesystem",
            )
            .into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPointer {
    offset: u64,
    bytes: u64,
    digest: String,
}

/// An opaque, serializable optimization, bound to one root/resource/inode and
/// the last newline-complete byte. It contains no resource state or path.
///
/// Only reuse a checkpoint obtained from a trusted reader over an append-only
/// owner. Endpoint anchors detect replacement, truncation and boundary edits,
/// not an out-of-contract rewrite of an arbitrary older line plus an append.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryCursor {
    schema_version: u32,
    resource: String,
    root: Identity,
    directory: Identity,
    file: Option<Identity>,
    offset: u64,
    place: usize,
    boundary_digest: String,
    snapshot: Option<SnapshotPointer>,
    version: u64,
    spent: u64,
}

impl HistoryCursor {
    pub fn resource(&self) -> &str {
        &self.resource
    }

    pub fn place(&self) -> usize {
        self.place
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }
}

/// Application bytes actually read, excluding metadata, path lookup, kernel
/// readahead and OS cache misses. Snapshot reads parse at most one older line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryReadMetrics {
    pub history_bytes_read: u64,
    pub records_parsed: usize,
    pub anchor_bytes_read: u64,
    pub snapshot_bytes_read: u64,
    pub snapshot_records_parsed: usize,
    pub resource_bytes_read: u64,
}

impl HistoryReadMetrics {
    pub fn accumulate(&mut self, other: &Self) {
        self.history_bytes_read += other.history_bytes_read;
        self.records_parsed += other.records_parsed;
        self.anchor_bytes_read += other.anchor_bytes_read;
        self.snapshot_bytes_read += other.snapshot_bytes_read;
        self.snapshot_records_parsed += other.snapshot_records_parsed;
        self.resource_bytes_read += other.resource_bytes_read;
    }
}

#[derive(Debug)]
pub struct HistoryBatch {
    pub records: Vec<Record>,
    /// Exact resource at the batch's complete-record upper bound, recovered
    /// from its last after snapshot. Never the unbounded latest mutable state.
    pub resource: Resource,
    pub cursor: HistoryCursor,
    pub metrics: HistoryReadMetrics,
}

impl HistoryReader {
    /// Read only bytes following a trusted checkpoint. An incomplete final
    /// line is not consumed and will be read again on the next call. A complete
    /// malformed line is always an error, not an empty or skipped record.
    pub fn history_since(
        &self,
        id: &str,
        after: Option<&HistoryCursor>,
    ) -> Result<HistoryBatch, StoreError> {
        self.history_range(id, after, None)
    }

    /// Read a physically bounded interval. `None -> through` revalidates the
    /// complete prefix, including the checkpoint's spent/version counters;
    /// this is the strict replay path rather than the incremental fast path.
    pub fn history_range(
        &self,
        id: &str,
        after: Option<&HistoryCursor>,
        through: Option<&HistoryCursor>,
    ) -> Result<HistoryBatch, StoreError> {
        let root = if self.root.is_absolute() {
            self.root.clone()
        } else {
            std::env::current_dir()?.join(&self.root)
        };
        let directory = root.join(encode(id)?);
        let root_identity = Identity::of(&anchor_file_lease::open_directory(&root)?.metadata()?)?;
        let directory_identity = match anchor_file_lease::open_directory(&directory) {
            Ok(file) => Identity::of(&file.metadata()?)?,
            Err(error) if absent(&error) && after.is_none() && through.is_none() => {
                return Err(StoreError::Missing(id.into()));
            }
            Err(error) if absent(&error) => return invalid("resource directory disappeared"),
            Err(error) => return Err(error.into()),
        };
        let path = directory.join("history.jsonl");
        let mut file = match anchor_file_lease::open_regular(&path) {
            Ok(file) => Some(file),
            Err(error) if absent(&error) => None,
            Err(error) => return Err(error.into()),
        };
        let identity = file
            .as_ref()
            .map(|file| Identity::of(&file.metadata()?))
            .transpose()?;
        let size = file
            .as_ref()
            .map(|file| file.metadata().map(|metadata| metadata.len()))
            .transpose()?
            .unwrap_or(0);
        let mut metrics = HistoryReadMetrics::default();
        for cursor in [after, through].into_iter().flatten() {
            if cursor.schema_version != 1
                || cursor.resource != id
                || cursor.root != root_identity
                || cursor.directory != directory_identity
                || (cursor.file.is_some() && cursor.file != identity)
                || (cursor.file.is_none() && (cursor.offset != 0 || cursor.place != 0))
                || cursor.offset > size
                || (cursor.place == 0) != (cursor.offset == 0)
                || cursor.snapshot.as_ref().is_some_and(|pointer| {
                    pointer.bytes == 0
                        || pointer
                            .offset
                            .checked_add(pointer.bytes)
                            .is_none_or(|end| end > cursor.offset)
                })
                || cursor.snapshot.is_none() != (cursor.version == 0)
            {
                return invalid("resource, file or complete-record boundary changed");
            }
            let anchor = boundary(file.as_mut(), cursor.offset, &mut metrics)?;
            if anchor != cursor.boundary_digest {
                return invalid("complete-record endpoint changed");
            }
        }
        let mut cursor = after.cloned().unwrap_or_else(|| HistoryCursor {
            schema_version: 1,
            resource: id.into(),
            root: root_identity.clone(),
            directory: directory_identity.clone(),
            file: identity.clone(),
            offset: 0,
            place: 0,
            boundary_digest: hash(&[]),
            snapshot: None,
            version: 0,
            spent: 0,
        });
        cursor.file = identity.clone();
        let end = through.map_or(size, |cursor| cursor.offset);
        if cursor.offset > end || through.is_some_and(|end| cursor.place > end.place) {
            return invalid("reversed history interval");
        }
        let mut current_file = anchor_file_lease::open_regular(&directory.join("resource.json"))?;
        let mut bytes = Vec::new();
        current_file.read_to_end(&mut bytes)?;
        metrics.resource_bytes_read = bytes.len() as u64;
        let current: Resource = serde_json::from_slice(&bytes)
            .map_err(|error| StoreError::Corrupt(error.to_string()))?;
        if current.id != id {
            return invalid("resource state identity changed");
        }
        let mut records = Vec::new();
        let mut snapshot = None;
        if let Some(file) = &mut file {
            file.seek(SeekFrom::Start(cursor.offset))?;
            let mut reader = BufReader::new(file.take(end - cursor.offset));
            let mut line = Vec::new();
            while reader.read_until(b'\n', &mut line)? > 0 {
                metrics.history_bytes_read += line.len() as u64;
                if line.last() != Some(&b'\n') {
                    break;
                }
                let record: Record = serde_json::from_slice(&line).map_err(|error| {
                    StoreError::Corrupt(format!(
                        "resource {id}, line {}: {error}",
                        cursor.place + 1
                    ))
                })?;
                metrics.records_parsed += 1;
                if record.submission.target != id
                    || (record.verdict.is_accepted() && record.after.is_none())
                {
                    return Err(StoreError::Corrupt(
                        "history has a resource/state gap".into(),
                    ));
                }
                cursor.spent = cursor.spent.saturating_add(record.stamp.cost());
                if let Some(after) = &record.after {
                    if Some(after.version) != cursor.version.checked_add(1)
                        || after.id != id
                        || after.validator != current.validator
                        || after.created != current.created
                    {
                        return Err(StoreError::Corrupt(
                            "history snapshot sequence changed".into(),
                        ));
                    }
                    cursor.version = after.version;
                    cursor.snapshot = Some(SnapshotPointer {
                        offset: cursor.offset,
                        bytes: line.len() as u64,
                        digest: hash(&line),
                    });
                    snapshot = Some(after.clone());
                }
                cursor.offset += line.len() as u64;
                cursor.place = cursor
                    .place
                    .checked_add(1)
                    .ok_or_else(|| StoreError::Corrupt("history position overflow".into()))?;
                records.push(record);
                line.clear();
            }
        }
        if after.is_none_or(|previous| previous.offset != cursor.offset) {
            cursor.boundary_digest = boundary(file.as_mut(), cursor.offset, &mut metrics)?;
        }
        if let Some(expected) = through {
            let mut present = expected.clone();
            if present.file.is_none() && present.offset == 0 {
                present.file = identity.clone();
            }
            if present != cursor {
                return invalid("bounded history no longer matches its checkpoint");
            }
            cursor = expected.clone();
        }
        let mut resource = match (snapshot, &cursor.snapshot) {
            (Some(snapshot), _) => snapshot,
            (None, Some(pointer)) => {
                let file = file
                    .as_mut()
                    .ok_or_else(|| StoreError::CursorInvalid("snapshot file disappeared".into()))?;
                file.seek(SeekFrom::Start(pointer.offset))?;
                let mut bytes = Vec::new();
                file.take(pointer.bytes).read_to_end(&mut bytes)?;
                metrics.snapshot_bytes_read += bytes.len() as u64;
                if bytes.len() as u64 != pointer.bytes || hash(&bytes) != pointer.digest {
                    return invalid("last recovery snapshot changed");
                }
                let record: Record = serde_json::from_slice(&bytes)
                    .map_err(|error| StoreError::Corrupt(error.to_string()))?;
                metrics.snapshot_records_parsed += 1;
                record
                    .after
                    .filter(|after| {
                        after.id == id
                            && after.version == cursor.version
                            && after.validator == current.validator
                            && after.created == current.created
                    })
                    .ok_or_else(|| {
                        StoreError::CursorInvalid("recovery snapshot identity changed".into())
                    })?
            }
            (None, None) if current.version == 0 => current,
            _ => {
                return Err(StoreError::Corrupt(
                    "initial resource snapshot is unavailable".into(),
                ));
            }
        };
        if let Some(task) = &mut resource.task {
            task.budget.spent = cursor.spent;
        }
        // A rename/removal must not return a healthy view of a detached inode.
        if Identity::of(&anchor_file_lease::open_directory(&root)?.metadata()?)? != root_identity
            || Identity::of(&anchor_file_lease::open_directory(&directory)?.metadata()?)?
                != directory_identity
            || file_identity(&path)? != identity
        {
            return invalid("source changed while reading");
        }
        Ok(HistoryBatch {
            records,
            resource,
            cursor,
            metrics,
        })
    }
}

fn file_identity(path: &Path) -> Result<Option<Identity>, StoreError> {
    match anchor_file_lease::open_regular(path) {
        Ok(file) => Ok(Some(Identity::of(&file.metadata()?)?)),
        Err(error) if absent(&error) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn boundary(
    file: Option<&mut File>,
    offset: u64,
    metrics: &mut HistoryReadMetrics,
) -> Result<String, StoreError> {
    if offset == 0 {
        return Ok(hash(&[]));
    }
    let file = file.ok_or_else(|| StoreError::CursorInvalid("history file disappeared".into()))?;
    let length = offset.min(ANCHOR_BYTES);
    file.seek(SeekFrom::Start(offset - length))?;
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes)?;
    metrics.anchor_bytes_read += length;
    if bytes.last() != Some(&b'\n') {
        return invalid("checkpoint is not a complete-record boundary");
    }
    Ok(hash(&bytes))
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn invalid<T>(message: &str) -> Result<T, StoreError> {
    Err(StoreError::CursorInvalid(message.into()))
}
