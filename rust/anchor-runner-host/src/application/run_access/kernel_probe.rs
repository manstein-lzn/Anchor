//! Disposable measurement projection. Run history remains the authority.
use anchor_kernel::{Point, Probe, RecordEntry, Store};
use anchor_runtime::graph::validator::GRAPH_VALIDATOR;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub(super) struct HostProbe {
    path: PathBuf,
    writer: Mutex<()>,
}

impl HostProbe {
    pub(super) fn new(root: &Path, store: &impl Store) -> Self {
        let probe = Self {
            path: root.join("points.jsonl"),
            writer: Mutex::new(()),
        };
        if let Err(error) = probe.rebuild(store) {
            eprintln!("Kernel measurement projection rebuild unavailable: {error}");
        }
        probe
    }

    fn rebuild(&self, store: &impl Store) -> Result<(), Box<dyn std::error::Error>> {
        let root = self.path.parent().expect("projection root");
        std::fs::create_dir_all(root)?;
        let mut temporary = tempfile::NamedTempFile::new_in(root)?;
        // Native Run validators are immutable. Every retained verdict,
        // including refusals, is recoverable from its owner's history.
        for id in store.list("run/")? {
            let Some(resource) = store.load(&id)? else {
                continue;
            };
            if resource.validator != GRAPH_VALIDATOR {
                continue;
            }
            for record in store.record_entries(&id)? {
                write_point(&mut temporary, &point_of(&record, &resource.validator))?;
            }
        }
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path)?;
        File::open(root)?.sync_all()?;
        Ok(())
    }
}

impl Probe for HostProbe {
    fn observe(&self, point: &Point) {
        // Projection failures must never change a verdict or its durable facts.
        let _writer = self
            .writer
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            std::fs::create_dir_all(self.path.parent().expect("projection root"))?;
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?;
            write_point(&mut file, point)
        })();
        if let Err(error) = result {
            eprintln!("Kernel measurement projection append unavailable: {error}");
        }
    }
}

fn write_point(writer: &mut impl Write, point: &Point) -> Result<(), Box<dyn std::error::Error>> {
    serde_json::to_writer(&mut *writer, point)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn point_of(record: &RecordEntry, validator: &str) -> Point {
    Point {
        component: record
            .submission
            .target
            .split('/')
            .next()
            .unwrap_or_default()
            .into(),
        validator: validator.into(),
        principal: record.stamp.principal().kind,
        action: record.submission.action,
        effect: record.verdict.effect,
        verification: record.verdict.verification,
        reason: record.verdict.reason,
        cost: record.stamp.cost(),
        at: record.stamp.at(),
        config: record.stamp.config().map(str::to_owned),
    }
}

#[cfg(test)]
#[path = "kernel_probe/tests.rs"]
mod tests;
