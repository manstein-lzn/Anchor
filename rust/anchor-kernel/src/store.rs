//! Each resource owner stores its own state and its own append-only history.
//! There is no global log: the protocol is shared, storage is not.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::model::{Record, RecordEntry, Resource};

mod history;
use anchor_file_lease::{FileLease, FlockOperation};
use history::HistoryIndex;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("resource `{0}` already exists")]
    Exists(String),
    #[error("resource `{0}` does not exist")]
    Missing(String),
    #[error("invalid resource id `{0}`")]
    InvalidId(String),
    #[error("storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("stored record is unreadable: {0}")]
    Corrupt(String),
}

pub trait Store: Send + Sync {
    /// The current resource, including any accepted change whose state write
    /// was interrupted after its record was appended. A task's spent budget
    /// is derived from every record's cost, independently of resource versions.
    fn load(&self, id: &str) -> Result<Option<Resource>, StoreError>;
    /// Create a resource. Fails if it exists: creation is never a silent
    /// overwrite.
    fn create(&self, resource: &Resource) -> Result<(), StoreError>;
    /// Append `record` to the history of `id`, then, if it was accepted, save
    /// `record.after` as the current state. The record is durable first. Only
    /// newline-complete records are history; an incomplete tail is discarded
    /// before appending. The caller holds the kernel's writer lock.
    fn commit(&self, id: &str, record: &Record) -> Result<(), StoreError>;
    fn history(&self, id: &str) -> Result<Vec<Record>, StoreError>;
    /// Facts without recovery snapshots, for history projections.
    fn record_entries(&self, id: &str) -> Result<Vec<RecordEntry>, StoreError> {
        Ok(self
            .history(id)?
            .into_iter()
            .map(RecordEntry::from)
            .collect())
    }
    /// Complete records, including verdicts that did not advance a version.
    fn history_len(&self, id: &str) -> Result<usize, StoreError> {
        Ok(self.record_entries(id)?.len())
    }
    /// Ids starting with `prefix`, in order.
    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError>;
}

// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct MemoryStore {
    inner: Mutex<BTreeMap<String, (Resource, Vec<Record>)>>,
}

impl Store for MemoryStore {
    fn load(&self, id: &str) -> Result<Option<Resource>, StoreError> {
        Ok(self
            .inner
            .lock()
            .expect("store lock")
            .get(id)
            .map(|(resource, history)| {
                let mut resource = resource.clone();
                derive_spent(&mut resource, history);
                resource
            }))
    }

    fn create(&self, resource: &Resource) -> Result<(), StoreError> {
        let mut inner = self.inner.lock().expect("store lock");
        if inner.contains_key(&resource.id) {
            return Err(StoreError::Exists(resource.id.clone()));
        }
        inner.insert(resource.id.clone(), (resource.clone(), Vec::new()));
        Ok(())
    }

    fn commit(&self, id: &str, record: &Record) -> Result<(), StoreError> {
        let mut inner = self.inner.lock().expect("store lock");
        let (resource, history) = inner
            .get_mut(id)
            .ok_or_else(|| StoreError::Missing(id.into()))?;
        history.push(record.clone());
        if let Some(after) = &record.after {
            *resource = after.clone();
        }
        Ok(())
    }

    fn history(&self, id: &str) -> Result<Vec<Record>, StoreError> {
        Ok(self
            .inner
            .lock()
            .expect("store lock")
            .get(id)
            .map(|(_, h)| h.clone())
            .unwrap_or_default())
    }

    fn record_entries(&self, id: &str) -> Result<Vec<RecordEntry>, StoreError> {
        let inner = self.inner.lock().expect("store lock");
        Ok(inner
            .get(id)
            .map(|(_, history)| {
                history
                    .iter()
                    .map(|record| RecordEntry {
                        stamp: record.stamp.clone(),
                        submission: record.submission.clone(),
                        verdict: record.verdict,
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    fn history_len(&self, id: &str) -> Result<usize, StoreError> {
        Ok(self
            .inner
            .lock()
            .expect("store lock")
            .get(id)
            .map(|(_, history)| history.len())
            .unwrap_or(0))
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        Ok(self
            .inner
            .lock()
            .expect("store lock")
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect())
    }
}

/// Costs are facts in the history even when a submission changes no version.
fn derive_spent(resource: &mut Resource, history: &[Record]) {
    if let Some(task) = &mut resource.task {
        task.budget.spent = history.iter().fold(0_u64, |spent, record| {
            spent.saturating_add(record.stamp.cost())
        });
    }
}

// ---------------------------------------------------------------------------

/// One directory per resource: `resource.json` (current state, replaced
/// atomically) and `history.jsonl` (append-only, fsynced before the state).
pub struct FileStore {
    root: PathBuf,
    histories: Mutex<BTreeMap<String, HistoryIndex>>,
}

impl FileStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            histories: Mutex::new(BTreeMap::new()),
        })
    }

    /// Physically remove a resource and all of its history. The caller owns
    /// authorization and must exclude new writers for the resource's lifetime.
    /// The directory rename is the deletion's visibility boundary; a later
    /// read completes cleanup after a crash. Lock files are retained separately.
    pub fn remove(&self, id: &str) -> Result<(), StoreError> {
        let _lease = self.lease(id)?;
        self.finish_removal(id)?;
        let live = self.dir(id)?;
        self.histories.lock().expect("history cache").remove(id);
        match fs::rename(&live, self.deleting_dir(id)?) {
            Ok(()) => {
                File::open(&self.root)?.sync_all()?;
                self.finish_removal(id)
            }
            Err(error) => self.absent_directory(&live, error),
        }
    }

    /// The resource lease protects this entry; unrelated resources never hold
    /// the cache mutex while scanning a cold history or doing file I/O.
    fn with_history<T>(
        &self,
        id: &str,
        path: &Path,
        read: impl FnOnce(&mut HistoryIndex, Option<&mut File>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let cached = self.histories.lock().expect("history cache").remove(id);
        let (mut index, mut file) = HistoryIndex::open(path, cached)?;
        let result = read(&mut index, file.as_mut())?;
        self.histories
            .lock()
            .expect("history cache")
            .insert(id.into(), index);
        Ok(result)
    }

    fn dir(&self, id: &str) -> Result<PathBuf, StoreError> {
        Ok(self.root.join(encode(id)?))
    }

    fn deleting_dir(&self, id: &str) -> Result<PathBuf, StoreError> {
        Ok(self.root.join(format!(".deleting-{}", encode(id)?)))
    }

    /// A new descriptor per operation serializes threads and separate store
    /// instances alike. Never acquire it recursively (load uses history_at).
    fn lease(&self, id: &str) -> Result<FileLease, StoreError> {
        encode(id)?;
        let locks = self.root.join(".locks");
        fs::create_dir_all(&locks)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            // A valid id can encode to more than a filesystem component can
            // hold. Keep the lock name bounded without changing resource ids.
            .open(locks.join(format!("{:x}", Sha256::digest(id.as_bytes()))))?;
        Ok(
            FileLease::acquire(file, FlockOperation::LockExclusive)
                .map_err(std::io::Error::from)?,
        )
    }

    /// Also called with a live directory present: a stale tombstone belongs to
    /// an older deletion and must never remove a newly created resource.
    fn finish_removal(&self, id: &str) -> Result<(), StoreError> {
        let deleting = self.deleting_dir(id)?;
        match fs::remove_dir_all(&deleting) {
            Ok(()) => File::open(&self.root)?.sync_all().map_err(StoreError::from),
            Err(error) => self.absent_directory(&deleting, error),
        }
    }

    /// An overlong encoded filename cannot exist on this filesystem. Confirm
    /// absence by enumerating the parent instead of making normal reads fail
    /// merely because the deletion prefix makes their probe too long.
    fn absent_directory(&self, path: &Path, error: std::io::Error) -> Result<(), StoreError> {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        if error.kind() == std::io::ErrorKind::InvalidFilename {
            let name = path.file_name().expect("resource directory name");
            for entry in fs::read_dir(&self.root)? {
                if entry?.file_name() == name {
                    return Err(error.into());
                }
            }
            return Ok(());
        }
        Err(error.into())
    }
}

/// Keep `[A-Za-z0-9-_]`, escape everything else, so ids map to one safe
/// directory name each and `/` or `..` can never escape the root.
fn encode(id: &str) -> Result<String, StoreError> {
    if id.is_empty() || id.len() > 512 {
        return Err(StoreError::InvalidId(id.into()));
    }
    let mut out = String::with_capacity(id.len());
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    Ok(out)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let tmp = path.with_extension("tmp");
    {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn read_resource(path: &Path) -> Result<Resource, StoreError> {
    let bytes = fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(|e| StoreError::Corrupt(e.to_string()))
}

/// A crash can leave bytes that never formed a record. Locate the last
/// newline from the end without rewriting any existing complete records.
fn trim_incomplete_tail(file: &mut File) -> Result<(), std::io::Error> {
    let original_len = file.metadata()?.len();
    let mut end = original_len;
    let mut chunk = [0_u8; 8192];
    while end > 0 {
        let start = end.saturating_sub(chunk.len() as u64);
        let len = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut chunk[..len])?;
        if let Some(newline) = chunk[..len].iter().rposition(|byte| *byte == b'\n') {
            let complete_len = start + newline as u64 + 1;
            if complete_len != original_len {
                file.set_len(complete_len)?;
            }
            return Ok(());
        }
        end = start;
    }
    if original_len > 0 {
        file.set_len(0)?;
    }
    Ok(())
}

impl Store for FileStore {
    fn load(&self, id: &str) -> Result<Option<Resource>, StoreError> {
        let _lease = self.lease(id)?;
        self.finish_removal(id)?;
        let dir = self.dir(id)?;
        let path = dir.join("resource.json");
        if !path.exists() {
            return Ok(None);
        }
        let mut resource = read_resource(&path)?;
        self.with_history(id, &dir.join("history.jsonl"), |index, file| {
            index.restore(&mut resource, file)
        })?;
        Ok(Some(resource))
    }

    fn create(&self, resource: &Resource) -> Result<(), StoreError> {
        let _lease = self.lease(&resource.id)?;
        self.finish_removal(&resource.id)?;
        let dir = self.dir(&resource.id)?;
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(StoreError::Exists(resource.id.clone()));
            }
            Err(e) => return Err(e.into()),
        }
        self.histories
            .lock()
            .expect("history cache")
            .remove(&resource.id);
        let bytes = serde_json::to_vec(resource).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        write_atomic(&dir.join("resource.json"), &bytes)
    }

    fn commit(&self, id: &str, record: &Record) -> Result<(), StoreError> {
        let _lease = self.lease(id)?;
        self.finish_removal(id)?;
        let dir = self.dir(id)?;
        if !dir.join("resource.json").exists() {
            return Err(StoreError::Missing(id.into()));
        }
        let mut line =
            serde_json::to_vec(record).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        line.push(b'\n');
        let path = dir.join("history.jsonl");
        self.with_history(id, &path, |index, _| {
            let mut history = OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&path)?;
            trim_incomplete_tail(&mut history)?;
            let start = history.metadata()?.len();
            history.write_all(&line)?;
            history.sync_all()?;
            // Serialization can exceed the decoder's recursion limit. Preserve
            // the durable commit, but cache only facts that a cold read accepts.
            // Explicit invalidation also covers equal-length torn-tail replacement.
            if let Ok(decoded) = serde_json::from_slice::<Record>(&line) {
                index.push(&decoded, start, line.len() as u64);
                index.written(&history.metadata()?);
            } else {
                index.invalidate();
            }
            if let Some(after) = &record.after {
                let bytes =
                    serde_json::to_vec(after).map_err(|e| StoreError::Corrupt(e.to_string()))?;
                write_atomic(&dir.join("resource.json"), &bytes)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    fn history(&self, id: &str) -> Result<Vec<Record>, StoreError> {
        let _lease = self.lease(id)?;
        self.finish_removal(id)?;
        history_at(&self.dir(id)?.join("history.jsonl"))
    }

    fn record_entries(&self, id: &str) -> Result<Vec<RecordEntry>, StoreError> {
        let _lease = self.lease(id)?;
        self.finish_removal(id)?;
        self.with_history(id, &self.dir(id)?.join("history.jsonl"), |index, _| {
            Ok(index.entries.clone())
        })
    }

    fn history_len(&self, id: &str) -> Result<usize, StoreError> {
        let _lease = self.lease(id)?;
        self.finish_removal(id)?;
        self.with_history(id, &self.dir(id)?.join("history.jsonl"), |index, _| {
            Ok(index.entries.len())
        })
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        list_at(&self.root, prefix)
    }
}

/// Ids under `root` starting with `prefix`, in order.
fn list_at(root: &Path, prefix: &str) -> Result<Vec<String>, StoreError> {
    let mut ids = Vec::new();
    for entry in fs::read_dir(root)? {
        let name = entry?.file_name();
        // Internal directories (.locks and .deleting-*) can never be a
        // canonical encoded resource name, even for ids with those words.
        if let Some(name) = name.to_str()
            && let Some(id) = decode(name)
            && encode(&id).ok().as_deref() == Some(name)
            && id.starts_with(prefix)
        {
            ids.push(id);
        }
    }
    ids.sort();
    Ok(ids)
}

// ---------------------------------------------------------------------------

/// A read-only view of a [`FileStore`] root for observers outside the writer:
/// it takes no lease, creates nothing and writes nothing. Racing a writer, it
/// sees a prefix of complete records; an unterminated tail is not returned.
/// A resource removed meanwhile reads as absent.
pub struct HistoryReader {
    root: PathBuf,
}

impl HistoryReader {
    /// The root must already exist; the reader never creates it.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        if !fs::metadata(&root)?.is_dir() {
            return Err(StoreError::Io(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                root.display().to_string(),
            )));
        }
        Ok(Self { root })
    }

    pub fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        list_at(&self.root, prefix)
    }

    /// The current resource as [`Store::load`] would return it.
    pub fn load(&self, id: &str) -> Result<Option<Resource>, StoreError> {
        let state = self.root.join(encode(id)?).join("resource.json");
        let mut resource = match fs::read(&state) {
            Ok(bytes) => serde_json::from_slice::<Resource>(&bytes)
                .map_err(|e| StoreError::Corrupt(e.to_string()))?,
            Err(error) if absent(&error) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let path = self.root.join(encode(id)?).join("history.jsonl");
        let (index, mut file) = HistoryIndex::open(&path, None)?;
        // Removal renames the directory. Recheck after scanning its history.
        if !state.exists() {
            return Ok(None);
        }
        match index.restore(&mut resource, file.as_mut()) {
            Err(StoreError::Io(error)) if absent(&error) => return Ok(None),
            result => result?,
        }
        Ok(Some(resource))
    }

    pub fn history(&self, id: &str) -> Result<Vec<Record>, StoreError> {
        match history_at(&self.root.join(encode(id)?).join("history.jsonl")) {
            Err(StoreError::Io(error)) if absent(&error) => Ok(Vec::new()),
            result => result,
        }
    }
}

/// A missing path, or a name too long to exist on this filesystem: either
/// way there is no such resource, as [`FileStore`] reads it.
fn absent(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidFilename
    )
}

/// Read a history while the caller already holds the resource lease.
fn history_at(path: &Path) -> Result<Vec<Record>, StoreError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line)? > 0 {
        // Even valid JSON without its final newline was never a complete
        // record. Only this unterminated tail may be ignored.
        if line.last() != Some(&b'\n') {
            break;
        }
        let record = serde_json::from_slice(&line).map_err(|error| {
            StoreError::Corrupt(format!(
                "{}: line {}: {error}",
                path.display(),
                records.len() + 1
            ))
        })?;
        records.push(record);
        line.clear();
    }
    Ok(records)
}

fn decode(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}
