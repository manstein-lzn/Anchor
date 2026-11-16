//! L0: where records come from. Every read of recorded history goes through
//! [`HistorySource`]; nothing above this layer touches a data root.

use std::path::{Path, PathBuf};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use anchor_kernel::{
    HistoryBatch, HistoryCursor, HistoryReadMetrics, HistoryReader, Record, Resource, Store,
    StoreError,
};

/// A read-only kernel history: resources by id, each with its records in the
/// order they were appended. A record's place is its 1-based position there.
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceCapabilities {
    pub runs: bool,
    pub observations: bool,
    pub grades: bool,
}

pub trait HistorySource {
    /// The name rows from this source carry, telling its records apart from
    /// another root's.
    fn root(&self) -> &str;
    /// Installation domain used for natural Run and invocation joins.
    fn namespace(&self) -> &str {
        self.root()
    }
    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities::default()
    }
    fn list(&self) -> Result<Vec<String>, SourceError>;
    fn load(&self, id: &str) -> Result<Option<Resource>, SourceError>;
    /// A bounded source may begin after an earlier frozen window. Projection
    /// still uses full-history positions, never delta-relative row numbers.
    fn starting_after(&self, _id: &str) -> usize {
        0
    }
    /// The number of complete records currently visible for a resource. This
    /// is separate from `history_after`: a source must not silently turn a
    /// truncated history into an empty append-only delta.
    fn history_len(&self, id: &str) -> Result<usize, SourceError> {
        let after = self.starting_after(id);
        Ok(after.saturating_add(self.history_after(id, after)?.len()))
    }
    /// Whether the source had to discard a cached cursor and replay a
    /// resource from its beginning. The projector consumes this marker.
    fn take_reset(&self, _id: &str) -> bool {
        false
    }
    /// Drop source-local read state when a resource is no longer present.
    fn forget(&self, _id: &str) {}
    /// The records after the first `after`, so a reader can continue where
    /// it stopped.
    fn history_after(&self, id: &str, after: usize) -> Result<Vec<Record>, SourceError>;
}

#[derive(Debug)]
pub struct SourceError {
    pub root: String,
    pub error: StoreError,
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.root, self.error)
    }
}

impl std::error::Error for SourceError {}

/// A kernel data root on disk, read through [`HistoryReader`]: no lease, no
/// writes, and a writer racing it is seen up to its last complete record.
pub struct DataRoot {
    name: String,
    namespace: String,
    path: PathBuf,
    reader: HistoryReader,
    cache: RefCell<BTreeMap<String, CachedRead>>,
    reset: RefCell<BTreeSet<String>>,
    metrics: RefCell<HistoryReadMetrics>,
}

#[derive(Clone)]
struct CachedRead {
    after: usize,
    batch: Arc<HistoryBatch>,
    fresh: bool,
}

impl DataRoot {
    pub fn open(path: &Path, name: impl Into<String>) -> Result<Self, SourceError> {
        let name = name.into();
        let namespace = namespace_for_path(path);
        let reader = HistoryReader::open(path).map_err(|error| SourceError {
            root: name.clone(),
            error,
        })?;
        Ok(Self {
            name,
            namespace,
            path: path.to_owned(),
            reader,
            cache: RefCell::default(),
            reset: RefCell::default(),
            metrics: RefCell::default(),
        })
    }

    /// Stage read state with the table so an unsuccessful multi-root refresh
    /// cannot consume a reset marker or advance a cache in the live Hub.
    pub(crate) fn staged(&self) -> Result<Self, SourceError> {
        let mut staged = Self::open(&self.path, &self.name)?;
        staged.namespace = self.namespace.clone();
        staged.cache = RefCell::new(self.cache.borrow().clone());
        staged.reset = RefCell::new(self.reset.borrow().clone());
        staged.metrics = RefCell::new(self.metrics.borrow().clone());
        Ok(staged)
    }

    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    pub fn read_metrics(&self) -> HistoryReadMetrics {
        self.metrics.borrow().clone()
    }

    pub fn history_window(
        &self,
        id: &str,
        after: Option<&HistoryCursor>,
        through: Option<&HistoryCursor>,
    ) -> Result<HistoryBatch, SourceError> {
        let batch = self
            .reader
            .history_range(id, after, through)
            .map_err(|error| self.fail(error))?;
        self.metrics.borrow_mut().accumulate(&batch.metrics);
        Ok(batch)
    }

    fn refresh(&self, id: &str, cold: bool) -> Result<(), SourceError> {
        let cursor = (!cold)
            .then(|| {
                self.cache
                    .borrow()
                    .get(id)
                    .map(|cached| cached.batch.cursor.clone())
            })
            .flatten();
        let mut after = cursor.as_ref().map_or(0, HistoryCursor::place);
        let batch = match self.history_window(id, cursor.as_ref(), None) {
            Ok(batch) => batch,
            Err(error) if !cold && matches!(error.error, StoreError::CursorInvalid(_)) => {
                // The resource was replaced, truncated, or its history file
                // was otherwise detached. A failed cold replay remains an
                // error (except that `load` maps a cleanly missing resource
                // to `None`), never an empty healthy projection.
                self.cache.borrow_mut().remove(id);
                let batch = self.history_window(id, None, None)?;
                self.reset.borrow_mut().insert(id.to_owned());
                after = 0;
                batch
            }
            Err(error) => return Err(error),
        };
        self.cache.borrow_mut().insert(
            id.into(),
            CachedRead {
                after,
                batch: Arc::new(batch),
                fresh: true,
            },
        );
        Ok(())
    }

    fn fail(&self, error: StoreError) -> SourceError {
        SourceError {
            root: self.name.clone(),
            error,
        }
    }
}

/// Only standard sibling roots share a domain; arbitrary roots stay isolated.
pub(crate) fn namespace_for_path(path: &Path) -> String {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let standard = canonical.file_name().is_some_and(|name| {
        matches!(
            name.to_str(),
            Some("kernel-runs" | "kernel-facts" | "goose-acp")
        ) || name == anchor_facts::evaluation::GRADE_ROOT
    });
    let identity = if standard
        && canonical
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == "state")
    {
        canonical.parent().unwrap()
    } else {
        &canonical
    };
    format!(
        "install-{}",
        anchor_facts::text_digest(&identity.to_string_lossy())
    )
}

impl HistorySource for DataRoot {
    fn capabilities(&self) -> SourceCapabilities {
        let name = self.path.file_name().and_then(|name| name.to_str());
        SourceCapabilities {
            runs: name == Some("kernel-runs"),
            observations: name == Some("kernel-facts"),
            grades: name == Some(anchor_facts::evaluation::GRADE_ROOT),
        }
    }
    fn namespace(&self) -> &str {
        &self.namespace
    }
    fn root(&self) -> &str {
        &self.name
    }
    fn list(&self) -> Result<Vec<String>, SourceError> {
        self.reader.list("").map_err(|e| self.fail(e))
    }
    fn load(&self, id: &str) -> Result<Option<Resource>, SourceError> {
        match self.refresh(id, false) {
            Ok(()) => Ok(self
                .cache
                .borrow()
                .get(id)
                .map(|cached| cached.batch.resource.clone())),
            Err(error) if matches!(error.error, StoreError::Missing(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn history_after(&self, id: &str, after: usize) -> Result<Vec<Record>, SourceError> {
        let reusable = self.cache.borrow().get(id).is_some_and(|cached| {
            cached.fresh && after >= cached.after && after <= cached.batch.cursor.place()
        });
        if !reusable {
            let continuation = self
                .cache
                .borrow()
                .get(id)
                .is_some_and(|cached| cached.batch.cursor.place() == after);
            self.refresh(id, !continuation)?;
        }
        let mut cache = self.cache.borrow_mut();
        let cached = cache.get_mut(id).expect("read just cached");
        cached.fresh = false;
        Ok(cached
            .batch
            .records
            .iter()
            .skip(after.saturating_sub(cached.after))
            .cloned()
            .collect())
    }

    fn history_len(&self, id: &str) -> Result<usize, SourceError> {
        if !self.cache.borrow().contains_key(id) {
            self.refresh(id, true)?;
        }
        Ok(self
            .cache
            .borrow()
            .get(id)
            .map_or(0, |cached| cached.batch.cursor.place()))
    }

    fn take_reset(&self, id: &str) -> bool {
        self.reset.borrow_mut().remove(id)
    }

    fn forget(&self, id: &str) {
        self.cache.borrow_mut().remove(id);
        self.reset.borrow_mut().remove(id);
    }
}

/// Any kernel store read as a source, such as a `MemoryStore` in tests. Only
/// its reading methods are used.
pub struct Stored<'a, S: Store> {
    name: String,
    store: &'a S,
}

impl<'a, S: Store> Stored<'a, S> {
    pub fn new(store: &'a S, name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            store,
        }
    }

    fn fail(&self, error: StoreError) -> SourceError {
        SourceError {
            root: self.name.clone(),
            error,
        }
    }
}

impl<S: Store> HistorySource for Stored<'_, S> {
    fn root(&self) -> &str {
        &self.name
    }
    fn list(&self) -> Result<Vec<String>, SourceError> {
        self.store.list("").map_err(|e| self.fail(e))
    }
    fn load(&self, id: &str) -> Result<Option<Resource>, SourceError> {
        self.store.load(id).map_err(|e| self.fail(e))
    }
    fn history_after(&self, id: &str, after: usize) -> Result<Vec<Record>, SourceError> {
        let mut records = self.store.history(id).map_err(|e| self.fail(e))?;
        Ok(records.split_off(after.min(records.len())))
    }

    fn history_len(&self, id: &str) -> Result<usize, SourceError> {
        self.store.history_len(id).map_err(|e| self.fail(e))
    }
}
