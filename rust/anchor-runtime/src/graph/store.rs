use super::*;
use serde_json;

pub trait RunStore: Send + Sync {
    fn load(&self, run_id: &str) -> Result<Option<GraphRunRecord>, GraphError>;
    fn save(&self, record: &GraphRunRecord) -> Result<(), GraphError>;
    fn acquire_lease(&self, run_id: &str) -> Result<Box<dyn RunLease>, GraphError>;
}
pub trait RunLease: Send {}

#[derive(Debug, Clone)]
pub struct FileRunStore {
    root: PathBuf,
}
impl FileRunStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    fn path(&self, id: &str) -> Result<PathBuf, GraphError> {
        validate_component(id)?;
        Ok(self.root.join(format!("{id}.json")))
    }
}
impl RunStore for FileRunStore {
    fn load(&self, run_id: &str) -> Result<Option<GraphRunRecord>, GraphError> {
        let bytes = match fs::read(self.path(run_id)?) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut record: GraphRunRecord =
            serde_json::from_slice(&bytes).map_err(GraphError::RunDecode)?;
        if record.run_id != run_id {
            return Err(GraphError::CorruptRun(
                "run identity or format mismatch".into(),
            ));
        }
        record.migrate_format()?;
        record.validate()?;
        Ok(Some(record))
    }
    fn save(&self, record: &GraphRunRecord) -> Result<(), GraphError> {
        record.validate()?;
        let path = self.path(&record.run_id)?;
        fs::create_dir_all(&self.root)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let tmp = self.root.join(format!(
            ".{}.{}.{}.tmp",
            record.run_id,
            std::process::id(),
            stamp
        ));
        let bytes = serde_json::to_vec(record).map_err(GraphError::RunDecode)?;
        let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        if let Err(e) = fs::rename(&tmp, &path) {
            let _ = fs::remove_file(tmp);
            return Err(e.into());
        }
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn acquire_lease(&self, run_id: &str) -> Result<Box<dyn RunLease>, GraphError> {
        validate_component(run_id)?;
        fs::create_dir_all(&self.root)?;
        let path = self.root.join(format!(".{run_id}.lock"));
        // Keep the lock inode stable. The OS releases this advisory lock if
        // the process exits, including an unclean crash; never unlink it.
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => GraphError::RunBusy(run_id.to_owned()),
            std::fs::TryLockError::Error(error) => GraphError::Io(error),
        })?;
        file.sync_all()?;
        Ok(Box::new(FileRunLease { _file: file }))
    }
}
struct FileRunLease {
    _file: fs::File,
}
impl RunLease for FileRunLease {}
