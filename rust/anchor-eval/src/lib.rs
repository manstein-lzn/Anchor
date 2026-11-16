//! Records grades. A grader judges a finished Run against its task's checks;
//! this crate turns that judgement into an ordinary kernel submission to
//! `grade/{run}`, in a state root of its own (`kernel-grades`), so grades
//! share the one record format with every other fact and the data hub reads
//! them like any other root.

use std::fs::OpenOptions;
use std::path::Path;

use anchor_facts::Grade;
use anchor_file_lease::{FileLease, FlockOperation};
use anchor_kernel::{
    Action, FileStore, Kernel, KernelError, Resource, StoreError, Submission, Verdict,
};
use serde_json::Value;

pub mod evaluation;

pub const ROOT: &str = anchor_facts::evaluation::GRADE_ROOT;

pub struct GradeRecorder {
    kernel: Kernel<FileStore>,
    _lease: FileLease,
}

impl GradeRecorder {
    /// Open the grade root under `state`, as its only writer.
    pub fn open(state: &Path) -> Result<Self, String> {
        let root = state.join(ROOT);
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".writer.lock"))
            .map_err(|e| e.to_string())?;
        let lease = FileLease::acquire(lock, FlockOperation::NonBlockingLockExclusive)
            .map_err(|e| format!("grade store already has a writer: {e}"))?;
        let mut kernel = Kernel::new(
            FileStore::new(&root).map_err(|e| e.to_string())?,
            Box::new(anchor_facts::GraderOnly),
        )
        .map_err(|e| e.to_string())?;
        anchor_facts::install_grades(&mut kernel).map_err(|e| e.to_string())?;
        Ok(Self {
            kernel,
            _lease: lease,
        })
    }

    /// Record `grade` as judged by the grader configuration `config`.
    pub fn record(&self, grade: &Grade, config: &str) -> Result<Verdict, KernelError> {
        let id = anchor_facts::grade_id(&grade.run_id);
        match self
            .kernel
            .create(Resource::new(&id, anchor_facts::GRADE, Value::Null).configured(config))
        {
            Err(KernelError::Store(StoreError::Exists(_))) | Ok(()) => {}
            Err(error) => return Err(error),
        }
        // The only writer: the version it reads is the one it submits against.
        let seen = self
            .kernel
            .load(&id)?
            .ok_or_else(|| KernelError::Store(StoreError::Missing(id.clone())))?
            .version;
        self.kernel.submit(
            &anchor_facts::grader(config),
            0,
            Submission::new(&id, seen, Action::Write)
                .content(serde_json::to_value(grade).expect("grade serializes")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_facts::Check;
    use anchor_kernel::{HistoryReader, Reason};

    fn grade(passed: bool) -> Grade {
        Grade {
            version: anchor_facts::VERSION,
            run_id: "r1".into(),
            task: "ledger".into(),
            task_version: 1,
            checks: vec![Check {
                name: "total".into(),
                passed,
            }],
        }
    }

    #[test]
    fn grades_land_in_their_own_root_under_the_grader_configuration() {
        let state = tempfile::tempdir().unwrap();
        {
            let recorder = GradeRecorder::open(state.path()).unwrap();
            assert!(recorder.record(&grade(false), "g1").unwrap().is_accepted());
        }
        // Reopened later, a regrade appends to the same history.
        let recorder = GradeRecorder::open(state.path()).unwrap();
        assert!(recorder.record(&grade(true), "g2").unwrap().is_accepted());
        let reader = HistoryReader::open(state.path().join(ROOT)).unwrap();
        let history = reader.history("grade/r1").unwrap();
        assert_eq!(
            history.iter().map(|r| r.stamp.config()).collect::<Vec<_>>(),
            vec![Some("g1"), Some("g2")]
        );
        let latest: Grade = serde_json::from_value(history[1].submission.content.clone()).unwrap();
        assert!(latest.passed());
    }

    #[test]
    fn a_malformed_grade_is_returned_and_a_second_writer_refused() {
        let state = tempfile::tempdir().unwrap();
        let recorder = GradeRecorder::open(state.path()).unwrap();
        let mut empty = grade(true);
        empty.checks.clear();
        assert_eq!(
            recorder.record(&empty, "g").unwrap().reason,
            Reason::ContentInvalid
        );
        assert!(GradeRecorder::open(state.path()).is_err());
    }
}
