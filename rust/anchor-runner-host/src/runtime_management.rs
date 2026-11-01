//! The single Host runtime initialization operation. This records observations,
//! not Turns or Agent work, and never changes an admitted execution's snapshot.
use crate::{dependency_provision::DependencyAccess, runtime_configuration::RuntimeConfiguration};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimePrepare {
    pub(crate) kind: String,
    pub(crate) version: Option<String>,
    pub(crate) allow_network: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    kind: String,
    version: Option<String>,
    status: String,
    phase: String,
    started_at: String,
    updated_at: String,
    message: Option<String>,
}
struct State {
    operation: Option<Operation>,
    cancellation: Option<Arc<AtomicBool>>,
}
pub(crate) struct RuntimeManagement {
    configuration: Arc<RuntimeConfiguration>,
    root: PathBuf,
    state: Mutex<State>,
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
impl RuntimeManagement {
    pub(crate) fn new(
        configuration: Arc<RuntimeConfiguration>,
        state_root: &Path,
    ) -> Result<Self, String> {
        let root = state_root.join("managed");
        crate::host_fs::directory(&root)?;
        let mut operation = Self::load(&root)?;
        if let Some(operation) = &mut operation
            && matches!(operation.status.as_str(), "running" | "cancelling")
        {
            operation.status = "interrupted".into();
            operation.phase = "interrupted".into();
            operation.updated_at = now();
            operation.message = Some(
                "Host 重启中断了初始化。请重新初始化以检查并继续；此前成功的配置保留。".into(),
            );
        }
        let result = Self {
            configuration,
            root,
            state: Mutex::new(State {
                operation,
                cancellation: None,
            }),
        };
        result.persist(
            &result
                .state
                .lock()
                .map_err(|_| "runtime management lock unavailable")?
                .operation,
        )?;
        Ok(result)
    }
    fn load(root: &Path) -> Result<Option<Operation>, String> {
        let mut file = match fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(root.join("runtime-initialization.json"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("runtime initialization state unavailable".into()),
        };
        let metadata = file
            .metadata()
            .map_err(|_| "runtime initialization state unavailable")?;
        if !metadata.is_file()
            || metadata.len() > 65536
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(
                "runtime initialization state must be a bounded private regular file".into(),
            );
        }
        let mut bytes = vec![];
        file.read_to_end(&mut bytes)
            .map_err(|_| "runtime initialization state unavailable")?;
        serde_json::from_slice(&bytes).map_err(|_| "runtime initialization state malformed".into())
    }
    fn persist(&self, operation: &Option<Operation>) -> Result<(), String> {
        let mut file = tempfile::NamedTempFile::new_in(&self.root)
            .map_err(|_| "runtime initialization state could not be saved")?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|_| "runtime initialization state could not be saved")?;
        file.write_all(
            &serde_json::to_vec(operation).map_err(|_| "invalid runtime initialization state")?,
        )
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| "runtime initialization state could not be saved")?;
        file.persist(self.root.join("runtime-initialization.json"))
            .map_err(|_| "runtime initialization state could not be published")?;
        fs::File::open(&self.root)
            .and_then(|file| file.sync_all())
            .map_err(|_| "runtime initialization state could not be synchronized".to_owned())
    }
    pub(crate) fn status(&self) -> Result<Value, String> {
        let state = self
            .state
            .lock()
            .map_err(|_| "runtime management lock unavailable")?;
        Ok(json!({"runtime":self.configuration.status(),"operation":state.operation}))
    }

    /// GET-only observations run on the HTTP blocking pool after status() has
    /// released operation and configuration locks. POST responses stay cheap.
    pub(crate) fn observed_status(&self) -> Result<Value, String> {
        let mut status = self.status()?;
        let observed = status["runtime"]["python_path"]
            .as_str()
            .map(|path| crate::tool_environment::probe_python_version(Path::new(path)));
        let (version, error) = match observed {
            Some(Ok(version)) => (Some(version), None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        status["runtime"]["python_version"] = json!(version);
        status["runtime"]["python_version_error"] = json!(error);
        Ok(status)
    }

    fn progress(&self, phase: &str) {
        if let Ok(mut state) = self.state.lock()
            && let Some(operation) = &mut state.operation
        {
            operation.phase = phase.into();
            operation.updated_at = now();
            let _ = self.persist(&state.operation);
        }
    }
    pub(crate) fn start(self: &Arc<Self>, request: RuntimePrepare) -> Result<Value, String> {
        if !matches!(request.kind.as_str(), "python" | "rust") {
            return Err("runtime kind must be python or rust".into());
        }
        if !request.allow_network {
            return Err("runtime initialization needs explicit network authorization".into());
        }
        let cancellation = Arc::new(AtomicBool::new(false));
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "runtime management lock unavailable")?;
            if state.cancellation.is_some() {
                return Ok(
                    json!({"runtime":self.configuration.status(),"operation":state.operation,"already_running":true}),
                );
            }
            let operation = Some(Operation {
                kind: request.kind.clone(),
                version: request.version.clone(),
                status: "running".into(),
                phase: "starting".into(),
                started_at: now(),
                updated_at: now(),
                message: None,
            });
            self.persist(&operation)?;
            state.operation = operation;
            state.cancellation = Some(cancellation.clone());
        }
        let management = self.clone();
        tokio::spawn(async move {
            let access = DependencyAccess {
                network: true,
                cancellation: cancellation.clone(),
            };
            let result = management
                .configuration
                .prepare_observed(
                    &request.kind,
                    request.version.as_deref(),
                    &access,
                    |phase| management.progress(phase),
                )
                .await;
            if let Ok(mut state) = management.state.lock() {
                if let Some(operation) = &mut state.operation {
                    operation.status = if result.is_ok() {
                        "succeeded"
                    } else if cancellation.load(Ordering::Acquire) {
                        "cancelled"
                    } else {
                        "failed"
                    }
                    .into();
                    operation.phase = operation.status.clone();
                    operation.updated_at = now();
                    operation.message =
                        result.err().map(|error| error.chars().take(2048).collect());
                }
                state.cancellation = None;
                if management.persist(&state.operation).is_err()
                    && let Some(operation) = &mut state.operation
                {
                    operation.message =
                        Some("初始化已结束，但状态保存失败。请刷新检查环境状态。".into());
                }
            }
        });
        self.status()
    }
    pub(crate) fn cancel(&self) -> Result<Value, String> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| "runtime management lock unavailable")?;
            if let Some(cancellation) = &state.cancellation {
                cancellation.store(true, Ordering::Release);
                if let Some(operation) = &mut state.operation {
                    operation.status = "cancelling".into();
                    operation.phase = "cancelling".into();
                    operation.updated_at = now();
                }
                self.persist(&state.operation)?;
            }
        }
        self.status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Arc<RuntimeManagement>) {
        let root = tempfile::tempdir().unwrap();
        let configuration = Arc::new(RuntimeConfiguration::from_snapshot_for_test(
            root.path(),
            &root.path().join("library"),
            "bwrap".into(),
            crate::runtime_configuration::RuntimeSnapshot {
                python: None,
                python_enabled: false,
                rust: None,
                pixi: None,
                provisioner: None,
            },
        ));
        let management = Arc::new(RuntimeManagement::new(configuration, root.path()).unwrap());
        (root, management)
    }
    fn request(network: bool) -> RuntimePrepare {
        RuntimePrepare {
            kind: "python".into(),
            version: Some("3.12".into()),
            allow_network: network,
        }
    }

    fn configure_python(management: &RuntimeManagement, python: &Path, script: &str) {
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        // Write via subprocess to avoid fd inheritance. Concurrent test thread spawns
        // can inherit the write fd, blocking exec of this script with ETXTBSY.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("cat > {:?}", python))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        child.wait().unwrap();
        fs::set_permissions(python, fs::Permissions::from_mode(0o755)).unwrap();
        management.configuration.publish_snapshot_for_test(
            crate::runtime_configuration::RuntimeSnapshot {
                python: Some(python.to_path_buf()),
                python_enabled: true,
                rust: None,
                pixi: None,
                provisioner: None,
            },
        );
    }

    #[test]
    fn runtime_python_version_is_a_refreshable_observation_without_publishing_configuration() {
        let (root, management) = fixture();
        let empty = management.observed_status().unwrap();
        assert!(empty["runtime"].get("python_version").unwrap().is_null());
        assert!(empty["runtime"]["python_version_error"].is_null());
        let python = root.path().join("python-3.9.0/bin/python3");
        configure_python(&management, &python, "#!/bin/sh\nprintf '3.12.13\\n'\n");
        let first = management.observed_status().unwrap();
        assert_eq!(first["runtime"]["python_version"], "3.12.13");
        assert!(first["runtime"]["python_version_error"].is_null());
        assert_eq!(first["runtime"]["python_configured"], true);
        assert_eq!(first["runtime"]["python_enabled"], true);
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("cat > {:?}", &python))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"#!/bin/sh\nprintf '3.13.2\\n'\n")
            .unwrap();
        child.wait().unwrap();
        assert_eq!(
            management.observed_status().unwrap()["runtime"]["python_version"],
            "3.13.2"
        );
        assert_eq!(
            management.configuration.snapshot().unwrap().python,
            Some(python)
        );
        assert!(!root.path().join("managed/runtime-config.json").exists());
        assert!(
            management.status().unwrap()["runtime"]
                .get("python_version")
                .is_none()
        );
        assert!(
            management.cancel().unwrap()["runtime"]
                .get("python_version")
                .is_none()
        );
    }

    #[test]
    fn runtime_python_version_failure_is_reported_without_changing_execution_facts() {
        let (root, management) = fixture();
        let python = root.path().join("python/bin/python3");
        configure_python(
            &management,
            &python,
            "#!/bin/sh\nprintf 'fixture probe failure' >&2\nexit 1\n",
        );
        let failed = management.observed_status().unwrap();
        assert!(failed["runtime"]["python_version"].is_null());
        assert!(
            failed["runtime"]["python_version_error"]
                .as_str()
                .unwrap()
                .contains("Python version probe failed")
        );
        assert!(!failed.to_string().contains("fixture probe failure"));
        assert_eq!(failed["runtime"]["python_configured"], true);
        assert_eq!(failed["runtime"]["python_enabled"], true);
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("cat > {:?}", &python))
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"#!/bin/sh\nprintf '3.12.13\\n'\n")
            .unwrap();
        child.wait().unwrap();
        let recovered = management.observed_status().unwrap();
        assert_eq!(recovered["runtime"]["python_version"], "3.12.13");
        assert!(recovered["runtime"]["python_version_error"].is_null());
        assert!(!root.path().join("managed/runtime-config.json").exists());
    }

    #[test]
    fn runtime_python_version_probe_does_not_hold_the_operation_lock() {
        let (root, management) = fixture();
        let python = root.path().join("python/bin/python3");
        let started = root.path().join("probe-started");
        let release = root.path().join("probe-release");
        configure_python(
            &management,
            &python,
            &format!(
                "#!/bin/sh\n: > '{}'\nwhile [ ! -f '{}' ]; do /bin/sleep 0.01; done\nprintf '3.12.13\\n'\n",
                started.display(),
                release.display()
            ),
        );
        let observer = management.clone();
        let probe = std::thread::spawn(move || observer.observed_status());
        let waiting = std::time::Instant::now();
        while !started.exists() {
            assert!(waiting.elapsed() < std::time::Duration::from_secs(2));
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(management.state.try_lock().is_ok());
        management
            .configuration
            .publish_snapshot_for_test(management.configuration.snapshot().unwrap());
        fs::write(release, "continue").unwrap();
        assert_eq!(
            probe.join().unwrap().unwrap()["runtime"]["python_version"],
            "3.12.13"
        );
    }

    async fn settled(management: &RuntimeManagement) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let status = management.status().unwrap();
                if !matches!(
                    status["operation"]["status"].as_str(),
                    Some("running" | "cancelling")
                ) {
                    break status;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn initialization_requires_authorization_deduplicates_and_can_retry_failed_operation() {
        let (root, management) = fixture();
        assert!(management.start(request(false)).is_err());
        assert!(management.status().unwrap()["operation"].is_null());
        assert_eq!(
            management.start(request(true)).unwrap()["operation"]["status"],
            "running"
        );
        assert_eq!(
            management.start(request(true)).unwrap()["already_running"],
            true
        );
        assert_eq!(settled(&management).await["operation"]["status"], "failed");
        assert_eq!(
            management.start(request(true)).unwrap()["operation"]["status"],
            "running"
        );
        assert_eq!(settled(&management).await["operation"]["status"], "failed");
        assert!(
            !management.status().unwrap()["runtime"]["python_configured"]
                .as_bool()
                .unwrap()
        );
        let reopened =
            RuntimeManagement::new(management.configuration.clone(), root.path()).unwrap();
        assert_eq!(reopened.status().unwrap()["operation"]["status"], "failed");
    }
    #[tokio::test]
    async fn cancellation_releases_waiting_preparation_and_restart_marks_interrupted() {
        let (root, management) = fixture();
        let lease = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(root.path().join("managed/runtime-prepare.lock"))
            .unwrap();
        rustix::fs::flock(&lease, rustix::fs::FlockOperation::LockExclusive).unwrap();
        management.start(request(true)).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert_eq!(
            management.status().unwrap()["operation"]["phase"],
            "waiting_for_lock"
        );
        assert_eq!(
            management.cancel().unwrap()["operation"]["status"],
            "cancelling"
        );
        assert_eq!(
            settled(&management).await["operation"]["status"],
            "cancelled"
        );
        let mut operation = management.state.lock().unwrap().operation.clone().unwrap();
        operation.status = "running".into();
        operation.phase = "installing_python".into();
        management.persist(&Some(operation)).unwrap();
        let restarted =
            RuntimeManagement::new(management.configuration.clone(), root.path()).unwrap();
        assert_eq!(
            restarted.status().unwrap()["operation"]["status"],
            "interrupted"
        );
        assert!(
            restarted.status().unwrap()["operation"]["message"]
                .as_str()
                .unwrap()
                .contains("重启")
        );
    }
    #[test]
    fn operation_file_refuses_fifo_and_symlink_without_waiting_for_a_writer() {
        let (root, management) = fixture();
        let path = root.path().join("managed/runtime-initialization.json");
        fs::remove_file(&path).unwrap();
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            &path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        assert!(RuntimeManagement::new(management.configuration.clone(), root.path()).is_err());
        fs::remove_file(&path).unwrap();
        let target = root.path().join("private-operation.json");
        fs::write(&target, b"null").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(RuntimeManagement::new(management.configuration.clone(), root.path()).is_err());
    }
}
