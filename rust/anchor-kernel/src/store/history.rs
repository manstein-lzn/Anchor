//! Disposable summaries of validated history. The JSONL remains authoritative.
use super::{Record, RecordEntry, Resource, StoreError};
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

#[derive(PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

impl FileStamp {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.ctime(),
                metadata.ctime_nsec(),
            ),
        }
    }
}

struct Snapshot {
    start: u64,
    len: u64,
    version: u64,
}

#[cfg(target_os = "linux")]
mod watch;
#[cfg(target_os = "linux")]
use watch::{HistoryWatch, open as watch_history};

#[cfg(not(target_os = "linux"))]
struct HistoryWatch;
#[cfg(not(target_os = "linux"))]
impl HistoryWatch {
    fn unchanged(&self) -> bool {
        false
    }
    fn written(self) -> Option<Self> {
        None
    }
}
#[cfg(not(target_os = "linux"))]
fn watch_history(_: &File) -> Option<HistoryWatch> {
    None
}

#[derive(Default)]
pub(super) struct HistoryIndex {
    stamp: Option<FileStamp>,
    watch: Option<HistoryWatch>,
    pub entries: Vec<RecordEntry>,
    spent: u64,
    after: Option<Snapshot>,
}

impl HistoryIndex {
    pub fn open(path: &Path, cached: Option<Self>) -> Result<(Self, Option<File>), StoreError> {
        Self::open_with_watch(path, cached, watch_history)
    }

    fn open_with_watch(
        path: &Path,
        cached: Option<Self>,
        watch: impl FnOnce(&File) -> Option<HistoryWatch>,
    ) -> Result<(Self, Option<File>), StoreError> {
        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if super::absent(&error) => return Ok((Self::default(), None)),
            Err(error) => return Err(error.into()),
        };
        let stamp = FileStamp::of(&file.metadata()?);
        if let Some(cached) = cached
            && cached.stamp.as_ref() == Some(&stamp)
            && cached.unchanged()
        {
            return Ok((cached, Some(file)));
        }
        // Only our own durable appends advance the cache incrementally. Any
        // other change (including same-length edits or replacement) validates
        // the entire prefix again, so complete corrupt records remain errors.
        let mut index = Self {
            watch: watch(&file),
            ..Self::default()
        };
        let mut reader = BufReader::with_capacity(64 * 1024, (&mut file).take(stamp.len));
        let mut line = Vec::new();
        let mut start = 0;
        while reader.read_until(b'\n', &mut line)? > 0 {
            if line.last() != Some(&b'\n') {
                break;
            }
            let record: Record = serde_json::from_slice(&line).map_err(|error| {
                StoreError::Corrupt(format!(
                    "{}: line {}: {error}",
                    path.display(),
                    index.entries.len() + 1
                ))
            })?;
            index.push(&record, start, line.len() as u64);
            start += line.len() as u64;
            line.clear();
        }
        index.stamp = Some(stamp);
        drop(reader);
        Ok((index, Some(file)))
    }

    pub fn push(&mut self, record: &Record, start: u64, len: u64) {
        self.spent = self.spent.saturating_add(record.stamp.cost());
        if let Some(after) = &record.after {
            self.after = Some(Snapshot {
                start,
                len,
                version: after.version,
            });
        }
        self.entries.push(RecordEntry {
            stamp: record.stamp.clone(),
            submission: record.submission.clone(),
            verdict: record.verdict,
        });
    }

    pub fn invalidate(&mut self) {
        self.stamp = None;
        self.watch = None;
    }

    fn unchanged(&self) -> bool {
        self.watch.as_ref().is_some_and(HistoryWatch::unchanged)
    }

    pub fn written(&mut self, file: &File) -> Result<(), StoreError> {
        // The lease excludes other writers. Refresh only this cache's cursor;
        // other caches watching the same inode still observe the mutation.
        self.watch = match self.watch.take() {
            Some(watch) => watch.written(),
            None => watch_history(file),
        };
        self.stamp = Some(FileStamp::of(&file.metadata()?));
        Ok(())
    }

    pub fn restore(
        &self,
        resource: &mut Resource,
        file: Option<&mut File>,
    ) -> Result<(), StoreError> {
        // The last accepted snapshot repairs a lost resource.json write.
        // Budget totals always come from stamps, never from that snapshot.
        if let Some(snapshot) = &self.after
            && snapshot.version > resource.version
        {
            let file =
                file.ok_or_else(|| StoreError::Corrupt("recovery history disappeared".into()))?;
            file.seek(SeekFrom::Start(snapshot.start))?;
            let mut bytes = Vec::new();
            file.take(snapshot.len).read_to_end(&mut bytes)?;
            let record: Record = serde_json::from_slice(&bytes)
                .map_err(|error| StoreError::Corrupt(error.to_string()))?;
            *resource = record.after.ok_or_else(|| {
                StoreError::Corrupt("recovery snapshot changed while reading".into())
            })?;
        }
        if let Some(task) = &mut resource.task {
            task.budget.spent = self.spent;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record_bytes() -> Vec<u8> {
        let mut bytes = serde_json::to_vec(&Record {
            stamp: crate::Stamp {
                principal: crate::Principal::new("host", crate::PrincipalKind::Code),
                cost: 2,
                at: None,
            },
            submission: crate::Submission::new("run/cache", 0, crate::Action::Write),
            verdict: crate::Verdict::returned(crate::Reason::ContentInvalid),
            after: None,
        })
        .unwrap();
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn identical_metadata_or_an_unavailable_watch_cannot_hide_corruption() {
        #[cfg(target_os = "linux")]
        let _test = watch::TEST_LOCK.lock().unwrap();
        for unavailable_watch in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("history.jsonl");
            let mut bytes = record_bytes();
            std::fs::write(&path, &bytes).unwrap();
            let (mut cached, _) = HistoryIndex::open(&path, None).unwrap();
            #[cfg(target_os = "linux")]
            assert!(cached.unchanged());
            if unavailable_watch {
                cached.watch = None;
            }
            bytes[0] = b'!';
            std::fs::write(&path, bytes).unwrap();
            // Deterministically reproduce a coarse filesystem clock even
            // where this write happens to advance ctime or mtime.
            cached.stamp = Some(FileStamp::of(&std::fs::metadata(&path).unwrap()));
            assert!(matches!(
                HistoryIndex::open(&path, Some(cached)),
                Err(StoreError::Corrupt(_))
            ));
        }
    }

    #[test]
    fn recovery_after_path_replacement_keeps_snapshot_and_cost_from_one_history() {
        #[cfg(target_os = "linux")]
        let _test = watch::TEST_LOCK.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.jsonl");
        let original = Resource::new("run/race", "validator/test", json!({"old":true})).task(20);
        let mut after = original.clone();
        after.version = 1;
        let mut bytes = serde_json::to_vec(&json!({
            "stamp":{"principal":{"id":"host","kind":"code"},"cost":2},
            "submission":{"target":"run/race","seen":0,"action":"write"},
            "verdict":{"effect":"accepted","verification":"asserted","reason":"ok"},
            "after":after
        }))
        .unwrap();
        bytes.push(b'\n');
        std::fs::write(&path, &bytes).unwrap();
        let (index, mut file) = HistoryIndex::open(&path, None).unwrap();
        let mut changed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        changed["stamp"]["cost"] = json!(7);
        changed["after"]["state"] = json!({"new":true});
        let replacement = root.path().join("new-history");
        std::fs::write(&replacement, serde_json::to_vec(&changed).unwrap()).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        let mut restored = original;
        index.restore(&mut restored, file.as_mut()).unwrap();
        assert_eq!(restored.state, json!({"old":true}));
        assert_eq!(restored.task.unwrap().budget.spent, 2);
    }
}
