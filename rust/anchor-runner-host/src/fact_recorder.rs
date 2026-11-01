//! The execution port's own record of what it observed: tool calls, model
//! turns and invocation lifecycles, as `anchor-facts` observations.
//!
//! Observations are ordinary kernel submissions to resources this port owns,
//! in a state root of their own (`kernel-facts`), so recording never contends
//! with Run adjudication. Recording is best effort: a failure is reported in
//! the invocation evidence and never changes an execution.

use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Instant,
};

use anchor_facts::{AgentConfiguration, Fact, Invocation, Observation};
use anchor_kernel::{
    Action, FileStore, Kernel, KernelError, Reason, Resource, StoreError, Submission,
};
use serde::Serialize;
use serde_json::Value;
use sha2::Digest;

pub(crate) const ROOT: &str = "kernel-facts";

pub(crate) struct FactRecorder {
    kernel: Kernel<FileStore>,
    _lease: anchor_file_lease::FileLease,
}

/// What one recording did, for the invocation evidence.
#[derive(Debug, Default, Serialize)]
pub(crate) struct Recording {
    pub(crate) facts: u64,
    pub(crate) accepted: bool,
    pub(crate) reason: Option<Reason>,
    pub(crate) elapsed_ms: u64,
    pub(crate) error: Option<String>,
}

pub(crate) fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

/// The digest of this Host executable, the identity of every harness
/// behavior it implements. Hashed once; `open` starts it in the background so
/// that no invocation waits for it.
pub(crate) fn host_sha256() -> String {
    static HOST: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    HOST.get_or_init(|| {
        let digest = || -> std::io::Result<String> {
            let mut file = std::fs::File::open(std::env::current_exe()?)?;
            let mut hasher = sha2::Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            Ok(format!("{:x}", hasher.finalize()))
        };
        digest().unwrap_or_else(|error| format!("unknown: {error}"))
    })
    .clone()
}

impl FactRecorder {
    /// The process's recorder for `state`. Every Run builds its own execution
    /// port, but the store has one writer per process: a resident assistant
    /// holds it for as long as it waits, and a second `open` would leave every
    /// concurrent Run unrecorded. Ports share one recorder instead; the lease
    /// is released when the last of them is dropped.
    pub(crate) fn shared(state: &Path) -> Result<Arc<Self>, String> {
        static OPEN: Mutex<BTreeMap<PathBuf, Weak<FactRecorder>>> = Mutex::new(BTreeMap::new());
        let key = std::fs::canonicalize(state).unwrap_or_else(|_| state.to_path_buf());
        let mut open = OPEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        open.retain(|_, recorder| recorder.strong_count() > 0);
        if let Some(recorder) = open.get(&key).and_then(Weak::upgrade) {
            return Ok(recorder);
        }
        let recorder = Arc::new(Self::open(state)?);
        open.insert(key, Arc::downgrade(&recorder));
        Ok(recorder)
    }

    pub(crate) fn open(state: &Path) -> Result<Self, String> {
        let root = state.join(ROOT);
        std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join(".writer.lock"))
            .map_err(|e| e.to_string())?;
        let lease = anchor_file_lease::FileLease::acquire(
            lease,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(|e| format!("fact store already has a writer: {e}"))?;
        let mut kernel = Kernel::new(
            FileStore::new(&root).map_err(|e| e.to_string())?,
            Box::new(anchor_facts::RecorderOnly),
        )
        .map_err(|e| e.to_string())?;
        anchor_facts::install(&mut kernel).map_err(|e| e.to_string())?;
        std::thread::spawn(host_sha256);
        Ok(Self {
            kernel,
            _lease: lease,
        })
    }

    /// Record one invocation's observations under its configuration.
    pub(crate) fn record(
        &self,
        configuration: &AgentConfiguration,
        envelope: Value,
        invocation: &Invocation,
        facts: Vec<Fact>,
    ) -> Recording {
        let started = Instant::now();
        let mut recording = Recording::default();
        if let Err(error) =
            self.submit_all(configuration, envelope, invocation, facts, &mut recording)
        {
            recording.error = Some(error.to_string());
        }
        recording.elapsed_ms = started.elapsed().as_millis() as u64;
        recording
    }

    fn submit_all(
        &self,
        configuration: &AgentConfiguration,
        envelope: Value,
        invocation: &Invocation,
        facts: Vec<Fact>,
        recording: &mut Recording,
    ) -> Result<(), KernelError> {
        self.create(
            Resource::new(
                anchor_facts::configuration_id(&configuration.envelope_sha256),
                anchor_facts::CONFIGURATION,
                envelope,
            )
            .configured(&configuration.envelope_sha256),
        )?;
        let digest = configuration.digest();
        let manifest =
            serde_json::to_value(configuration.clone().normalized()).expect("manifest serializes");
        self.create(
            Resource::new(
                anchor_facts::configuration_id(&digest),
                anchor_facts::CONFIGURATION,
                manifest,
            )
            .configured(&digest),
        )?;
        let trace = invocation.trace();
        self.create(
            Resource::new(&trace, anchor_facts::OBSERVATION, Value::Null).configured(&digest),
        )?;
        recording.facts = facts.len() as u64;
        let content = serde_json::to_value(Observation {
            version: anchor_facts::VERSION,
            invocation: invocation.clone(),
            facts,
        })
        .expect("observation serializes");
        // This recorder is the trace's only writer, so the version it reads is
        // the one it submits against.
        let seen = self
            .kernel
            .load(&trace)?
            .ok_or_else(|| KernelError::Store(StoreError::Missing(trace.clone())))?
            .version;
        let verdict = self.kernel.submit(
            &anchor_facts::recorder(&digest),
            0,
            Submission::new(&trace, seen, Action::Write).content(content),
        )?;
        recording.accepted = verdict.is_accepted();
        recording.reason = Some(verdict.reason);
        Ok(())
    }

    /// Manifests are content-addressed and a resumed invocation keeps its
    /// trace: an existing resource is the same one.
    fn create(&self, resource: Resource) -> Result<(), KernelError> {
        match self.kernel.create(resource) {
            Err(KernelError::Store(StoreError::Exists(_))) => Ok(()),
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_facts::{Failure, Outcome};
    use anchor_kernel::HistoryReader;

    fn configuration() -> AgentConfiguration {
        AgentConfiguration {
            version: anchor_facts::VERSION,
            graph_digest: "g".into(),
            node_id: "worker".into(),
            model: "flash".into(),
            model_binding: "binding".into(),
            runtime_sha256: "goose".into(),
            instructions_sha256: anchor_facts::text_digest("work"),
            plugin_instructions_sha256: anchor_facts::text_digest(""),
            plugins: Vec::new(),
            routes: vec!["verify".into()],
            host_sha256: host_sha256(),
            tools_sha256: anchor_facts::tools_digest([]),
            envelope_sha256: anchor_facts::text_digest("{\"network\":false}"),
        }
    }

    fn invocation() -> Invocation {
        Invocation {
            run_id: "rust-1".into(),
            graph_digest: "g".into(),
            node_id: "worker".into(),
            invocation: 1,
        }
    }

    fn facts() -> Vec<Fact> {
        vec![
            Fact::ToolCall {
                tool: "anchor_run".into(),
                tool_call_id: Some("call_1".into()),
                model_request: 1,
                arguments_shape: anchor_facts::arguments_shape(&serde_json::json!({})),
                path: None,
                program: None,
                command: None,
                started_at_ms: Some(1),
                ended_at_ms: Some(2),
                ok: false,
                failure: Some(Failure::NotAuthorized),
            },
            Fact::Lifecycle {
                started_at_ms: 0,
                ended_at_ms: 3,
                outcome: Outcome::Completed,
                model_turns: 1,
                tool_calls: 1,
                tool_calls_dropped: 0,
                resumed: false,
            },
        ]
    }

    #[test]
    fn observations_land_in_their_own_root_under_their_configuration() {
        let state = tempfile::tempdir().unwrap();
        let recorder = FactRecorder::open(state.path()).unwrap();
        let recording = recorder.record(
            &configuration(),
            serde_json::json!({"network":false}),
            &invocation(),
            facts(),
        );
        assert!(recording.accepted, "{recording:?}");
        assert_eq!((recording.facts, recording.error), (2, None));
        // A resumed invocation appends to the same trace.
        let again = recorder.record(
            &configuration(),
            serde_json::json!({"network":false}),
            &invocation(),
            facts(),
        );
        assert!(again.accepted);

        let reader = HistoryReader::open(state.path().join(ROOT)).unwrap();
        let digest = configuration().digest();
        assert_eq!(reader.list("trace/").unwrap(), vec![invocation().trace()]);
        let mut manifests = vec![
            anchor_facts::configuration_id(&digest),
            anchor_facts::configuration_id(&configuration().envelope_sha256),
        ];
        manifests.sort();
        assert_eq!(reader.list("config/").unwrap(), manifests);
        let envelope = reader
            .load(&anchor_facts::configuration_id(
                &configuration().envelope_sha256,
            ))
            .unwrap()
            .unwrap();
        assert_eq!(envelope.state, serde_json::json!({"network":false}));
        assert_eq!(envelope.version, 0);
        let history = reader.history(&invocation().trace()).unwrap();
        assert_eq!(history.len(), 2);
        let first: Observation =
            serde_json::from_value(history[0].submission.content.clone()).unwrap();
        assert_eq!(first.facts, facts());
        assert!(
            history
                .iter()
                .all(|r| r.verdict.is_accepted() && r.stamp.config() == Some(digest.as_str()))
        );
        let trace = reader.load(&invocation().trace()).unwrap().unwrap();
        assert_eq!(
            trace.created.unwrap().config.as_deref(),
            Some(digest.as_str())
        );
    }

    #[test]
    fn the_host_is_identified_by_the_digest_of_its_executable() {
        let digest = host_sha256();
        assert_eq!(digest.len(), 64, "{digest}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(host_sha256(), digest);
    }

    #[test]
    fn an_invocation_without_observations_is_not_recorded_as_empty() {
        let state = tempfile::tempdir().unwrap();
        let recorder = FactRecorder::open(state.path()).unwrap();
        let recording = recorder.record(
            &configuration(),
            serde_json::json!({"network":false}),
            &invocation(),
            Vec::new(),
        );
        assert!(!recording.accepted);
        assert_eq!(recording.reason, Some(Reason::ContentInvalid));
    }

    #[test]
    fn a_second_writer_is_refused_instead_of_sharing_the_root() {
        let state = tempfile::tempdir().unwrap();
        let _first = FactRecorder::open(state.path()).unwrap();
        assert!(FactRecorder::open(state.path()).is_err());
    }

    #[test]
    fn runs_of_one_process_share_the_recorder_and_release_it_together() {
        let state = tempfile::tempdir().unwrap();
        let first = FactRecorder::shared(state.path()).unwrap();
        let second = FactRecorder::shared(state.path()).unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(FactRecorder::open(state.path()).is_err());
        drop(first);
        assert!(FactRecorder::open(state.path()).is_err());
        drop(second);
        let reopened = FactRecorder::shared(state.path()).unwrap();
        drop(reopened);
        assert!(FactRecorder::open(state.path()).is_ok());
    }

    /// Only this module records observations; every other Host module goes
    /// through `FactRecorder::record`.
    #[test]
    fn only_the_recorder_submits_observations() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src.clone()];
        let mut writers = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    if [
                        "anchor_facts::recorder(",
                        "anchor_facts::install(",
                        "\"kernel-facts\"",
                    ]
                    .iter()
                    .any(|needle| text.contains(needle))
                    {
                        writers.push(path.strip_prefix(&src).unwrap().display().to_string());
                    }
                }
            }
        }
        assert_eq!(writers, vec!["fact_recorder.rs"]);
    }
}
