use super::*;
use crate::dependency_provision::test_support::{config, fixture_wheel, toolchain};
use anchor_runtime::{ToolPort, ToolResultContent};
use std::sync::atomic::{AtomicBool, Ordering};

struct Fixture {
    _root: tempfile::TempDir,
    application: RunApplication,
    provisioner: Arc<DependencyProvisioner>,
    library: PathBuf,
    scope: PathBuf,
    workspace: PathBuf,
    access: DependencyAccess,
}

impl Fixture {
    fn new() -> Option<Self> {
        let chain = toolchain()?;
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        let scope = root.path().join("host/session");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(workspace.join("one")).unwrap();
        std::fs::create_dir_all(workspace.join("two")).unwrap();
        let wheels = workspace.join("wheels");
        std::fs::create_dir_all(&wheels).unwrap();
        fixture_wheel(&wheels, "probepkg", "1.0");
        fixture_wheel(&wheels, "probepkg", "2.0");
        fixture_wheel(&wheels, "trialpkg", "1.0");
        let provisioner = Arc::new(
            DependencyProvisioner::new(config(&root.path().join("build"), &chain, wheels)).unwrap(),
        );
        let application = RunApplication::new(root.path().join("data"), library.clone())
            .with_dependency_provisioner(provisioner.clone());
        Some(Self {
            _root: root,
            application,
            provisioner,
            library,
            scope,
            workspace,
            access: DependencyAccess {
                network: false,
                cancellation: Arc::new(AtomicBool::new(false)),
            },
        })
    }

    async fn prepare(
        &self,
        project: &str,
        action: PrepareAction,
        requirements: &[String],
    ) -> Result<Value, ApplicationError> {
        self.application
            .prepare_development_environment(
                &self.library,
                &self.scope,
                &self.workspace,
                project,
                action,
                requirements,
                &[],
                &self.access,
            )
            .await
    }

    fn commands(&self, cancellation: anchor_runtime::Cancellation) -> crate::node_tools::NodeTools {
        let mut environment = crate::tool_environment::ToolEnvironment::default();
        environment
            .configure_pilot_python(&self.provisioner.config.python)
            .unwrap();
        let sandbox = BubblewrapSandbox::new(
            environment.authorize(
                anchor_sandbox_bwrap::BubblewrapPolicy::new(
                    &self.provisioner.config.bwrap,
                    ["sh", "cat"],
                )
                .authorize_workspace_root(&self.workspace),
            ),
        )
        .unwrap();
        let empty = crate::tool_host::PluginToolHost::new(Vec::<String>::new())
            .for_bindings(&[])
            .unwrap();
        crate::node_tools::NodeTools::new(
            Arc::new(empty),
            Arc::new(sandbox),
            self.workspace.clone(),
            Vec::new(),
            cancellation,
        )
        .with_environment(environment)
        .with_development_projects(self.library.clone(), self.scope.clone())
    }

    async fn run(&self, project: &str, argv: &[&str]) -> Value {
        let result = self
            .commands(self.access.cancellation.clone())
            .call("anchor_run", json!({"project":project,"command":argv}))
            .await
            .unwrap();
        match &result[0] {
            ToolResultContent::Json { value } => value.clone(),
            other => panic!("unexpected {other:?}"),
        }
    }
    async fn ok(&self, project: &str, argv: &[&str]) -> Value {
        let value = self.run(project, argv).await;
        assert_eq!(value["status"], "completed", "{value}");
        assert_eq!(value["exit_code"], 0, "{value}");
        value
    }
}

#[tokio::test]
async fn empty_private_env_native_install_upgrade_uninstall_aliases_scripts_and_isolation() {
    let Some(f) = Fixture::new() else {
        return;
    };
    let first = f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
    assert_eq!(first["created"], true);
    assert_eq!(first["dependencies_verified"], false);
    assert_eq!(
        f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap()["created"],
        false
    );
    f.prepare("two", PrepareAction::Ensure, &[]).await.unwrap();
    for alias in ["python", "python3", "python3.12"] {
        let value=f.ok("one",&[alias,"-c","import sys,os; assert sys.prefix == '/development/env'; assert os.environ['VIRTUAL_ENV'] == sys.prefix; print(sys.base_prefix)"]).await;
        assert!(!value["stdout"].as_str().unwrap().is_empty());
    }
    f.ok("one", &["pip", "--version"]).await;
    f.ok("one", &["python", "-m", "pip", "--version"]).await;
    f.ok(
        "one",
        &[
            "sh",
            "-c",
            "python -c 'import sys; assert sys.prefix == \"/development/env\"'; pip --version",
        ],
    )
    .await;
    f.ok(
        "one",
        &[
            "pip",
            "install",
            "--no-index",
            "../wheels/probepkg-1.0-py3-none-any.whl",
        ],
    )
    .await;
    f.ok(
        "two",
        &[
            "pip",
            "install",
            "--no-index",
            "../wheels/probepkg-2.0-py3-none-any.whl",
        ],
    )
    .await;
    assert!(
        f.ok("one", &["probepkg-tool"]).await["stdout"]
            .as_str()
            .unwrap()
            .contains("1.0")
    );
    assert!(
        f.ok("two", &["probepkg-tool"]).await["stdout"]
            .as_str()
            .unwrap()
            .contains("2.0")
    );
    let root = project_root(&f.scope, "one");
    let other = project_root(&f.scope, "two");
    let store =
        anchor_graph_host::store_root(&anchor_graph_host::resolved_library_root(&f.library));
    let prefix = f
        .provisioner
        .config
        .python
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let code = format!(
        "from pathlib import Path; assert not Path({:?}).exists(); assert not Path({:?}).exists(); p=Path({:?})/'anchor-env-write-probe';\ntry: p.write_text('no')\nexcept (PermissionError,OSError): pass\nelse: raise AssertionError('shared prefix writable')",
        store.display().to_string(),
        other.display().to_string(),
        prefix.display().to_string()
    );
    f.ok("one", &["python", "-c", &code]).await;
    let launcher = std::fs::read_to_string(root.join("env/bin/probepkg-tool")).unwrap();
    assert!(launcher.contains("/development/env/bin/python"));
    assert!(!launcher.contains("dependencies/store"));
    f.ok(
        "one",
        &[
            "pip",
            "install",
            "--no-index",
            "--upgrade",
            "../wheels/probepkg-2.0-py3-none-any.whl",
        ],
    )
    .await;
    assert!(
        f.ok("one", &["probepkg-tool"]).await["stdout"]
            .as_str()
            .unwrap()
            .contains("2.0")
    );
    f.ok("one", &["pip", "uninstall", "-y", "probepkg"]).await;
    f.ok("one",&["python","-c","import importlib.util,pathlib; assert importlib.util.find_spec('probepkg') is None; assert not pathlib.Path('/development/env/bin/probepkg-tool').exists()"]).await;
    assert!(
        anchor_library::reclaim(&f.library, true)
            .unwrap()
            .store_phase_abandoned
            .is_none()
    );
}

#[tokio::test]
async fn ensure_preserves_trials_files_are_native_install_requests_and_rebuild_is_empty() {
    let Some(f) = Fixture::new() else {
        return;
    };
    f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
    f.ok(
        "one",
        &[
            "pip",
            "install",
            "--no-index",
            "../wheels/trialpkg-1.0-py3-none-any.whl",
        ],
    )
    .await;
    std::fs::create_dir_all(f.workspace.join("one/nested")).unwrap();
    std::fs::write(
        f.workspace.join("one/requirements.txt"),
        "-r nested/dev.txt\n-c constraints.txt\n",
    )
    .unwrap();
    std::fs::write(
        f.workspace.join("one/nested/dev.txt"),
        "probepkg>=1.0; python_version >= '3.8'\nmissingpkg==9; python_version < '2'\n",
    )
    .unwrap();
    std::fs::write(f.workspace.join("one/constraints.txt"), "probepkg==1.0\n").unwrap();
    f.ok(
        "one",
        &[
            "pip",
            "install",
            "--no-index",
            "--find-links",
            "../wheels",
            "-r",
            "requirements.txt",
        ],
    )
    .await;
    f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
    f.ok(
        "one",
        &[
            "python",
            "-c",
            "import trialpkg,probepkg; assert probepkg.VALUE == '1.0'",
        ],
    )
    .await;
    std::fs::write(f.workspace.join("one/requirements.txt"), "").unwrap();
    f.ok(
        "one",
        &["pip", "install", "--no-index", "-r", "requirements.txt"],
    )
    .await;
    f.ok("one", &["python", "-c", "import trialpkg,probepkg"])
        .await;
    let installed = f
        .prepare(
            "one",
            PrepareAction::Ensure,
            &["../wheels/probepkg-2.0-py3-none-any.whl".into()],
        )
        .await
        .unwrap();
    assert_eq!(installed["dependencies_installed"], true, "{installed}");
    f.ok(
        "one",
        &[
            "python",
            "-c",
            "import trialpkg,probepkg; assert probepkg.VALUE == '2.0'",
        ],
    )
    .await;
    assert!(
        f.prepare("one", PrepareAction::Rebuild, &["probepkg".into()])
            .await
            .is_err()
    );
    f.prepare("one", PrepareAction::Rebuild, &[]).await.unwrap();
    f.ok("one",&["python","-c","import importlib.util; assert importlib.util.find_spec('trialpkg') is None; assert importlib.util.find_spec('probepkg') is None; import pip"]).await;
    assert!(f.workspace.join("one/requirements.txt").exists());
    f.prepare("one", PrepareAction::Discard, &[]).await.unwrap();
    assert!(!project_root(&f.scope, "one").join("env").exists());
    assert!(
        f.commands(f.access.cancellation.clone())
            .call(
                "anchor_run",
                json!({"project":"one","command":["python","-c","pass"]})
            )
            .await
            .is_err()
    );
    f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
}

#[tokio::test]
async fn legacy_view_is_preserved_and_store_only_offline_install_is_explicitly_unsupported() {
    use std::os::unix::fs::MetadataExt;
    let Some(f) = Fixture::new() else {
        return;
    };
    crate::application::development_dependencies::test_support::legacy_view(
        &f.provisioner,
        &f.library,
        &f.scope,
        "one",
        &["probepkg==1.0".into()],
        &[],
        &f.access,
    )
    .await
    .unwrap();
    let legacy = f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
    assert_eq!(legacy["environment_kind"], "legacy_read_only");
    assert_eq!(legacy["pip_available"], false);
    assert!(
        f.ok("one", &["probepkg-tool"]).await["stdout"]
            .as_str()
            .unwrap()
            .contains("1.0")
    );
    assert!(
        f.prepare("one", PrepareAction::Ensure, &["probepkg==1.0".into()])
            .await
            .is_err()
    );
    assert!(
        select_environment(&f.library, &f.scope, "one", None, &f.access)
            .await
            .is_err()
    );
    assert!(
        select_environment(
            &f.library,
            &f.scope,
            "one",
            Some(Path::new("/bin/sh")),
            &f.access
        )
        .await
        .is_err()
    );
    let unavailable = f.application.clone().with_frozen_runtime_snapshot(
        crate::runtime_configuration::RuntimeSnapshot {
            python: None,
            python_enabled: false,
            rust: None,
            pixi: None,
            provisioner: None,
        },
    );
    assert!(
        unavailable
            .prepare_development_environment(
                &f.library,
                &f.scope,
                &f.workspace,
                "one",
                PrepareAction::Ensure,
                &[],
                &[],
                &f.access
            )
            .await
            .is_err()
    );
    let store =
        anchor_graph_host::store_root(&anchor_graph_host::resolved_library_root(&f.library));
    let store_package =
        store.join("probepkg/1.0/lib/python3.12/site-packages/probepkg/__init__.py");
    // Locate the formal store's package layout without running package Python.
    fn locate(root: &Path) -> PathBuf {
        for entry in std::fs::read_dir(root).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                let found = locate(&entry.path());
                if !found.as_os_str().is_empty() {
                    return found;
                }
            } else if entry.file_name() == "__init__.py"
                && entry.path().parent().unwrap().file_name().unwrap() == "probepkg"
            {
                return entry.path();
            }
        }
        PathBuf::new()
    }
    let store_package = if store_package.exists() {
        store_package
    } else {
        locate(&store.join("probepkg/1.0"))
    };
    assert!(!store_package.as_os_str().is_empty());
    let before = std::fs::read(&store_package).unwrap();
    let metadata = std::fs::metadata(&store_package).unwrap();
    let before_identity = (metadata.ino(), metadata.mtime(), metadata.mtime_nsec());
    // A failure during the first explicit rebuild never selects a partial env.
    let mut bad_config = f.provisioner.config.clone();
    let fake = f._root.path().join("fake/uv");
    std::fs::create_dir_all(fake.parent().unwrap()).unwrap();
    std::fs::write(&fake, "#!/bin/sh\nexit 17\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    bad_config.uv = fake.clone();
    bad_config.uv_digest = hash(&std::fs::read(fake).unwrap());
    let bad = f
        .application
        .clone()
        .with_dependency_provisioner(Arc::new(DependencyProvisioner::new(bad_config).unwrap()));
    assert!(
        bad.prepare_development_environment(
            &f.library,
            &f.scope,
            &f.workspace,
            "one",
            PrepareAction::Rebuild,
            &[],
            &[],
            &f.access
        )
        .await
        .is_err()
    );
    assert!(
        bad.prepare_development_environment(
            &f.library,
            &f.scope,
            &f.workspace,
            "two",
            PrepareAction::Ensure,
            &[],
            &[],
            &f.access
        )
        .await
        .is_err()
    );
    // An unpublished temporary environment can retry through ordinary ensure.
    f.prepare("two", PrepareAction::Ensure, &[]).await.unwrap();
    assert_eq!(
        f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap()["environment_kind"],
        "legacy_read_only"
    );
    f.prepare("one", PrepareAction::Rebuild, &[]).await.unwrap();
    f.ok(
        "one",
        &[
            "pip",
            "install",
            "--no-index",
            "../wheels/probepkg-1.0-py3-none-any.whl",
        ],
    )
    .await;
    let store_access = format!(
        "from pathlib import Path; assert not Path({:?}).exists()",
        store_package.display().to_string()
    );
    f.ok("one", &["python", "-c", &store_access]).await;
    let private = locate(&project_root(&f.scope, "one").join("env/lib"));
    assert_ne!(
        std::fs::metadata(&private).unwrap().ino(),
        before_identity.0
    );
    f.ok("one",&["python","-c","import probepkg,pathlib; pathlib.Path(probepkg.__file__).write_text(\"VALUE='private'\\n\")"]).await;
    f.ok("one", &["pip", "uninstall", "-y", "probepkg"]).await;
    assert_eq!(std::fs::read(&store_package).unwrap(), before);
    let after = std::fs::metadata(&store_package).unwrap();
    assert_eq!(
        (after.ino(), after.mtime(), after.mtime_nsec()),
        before_identity
    );
    std::fs::remove_dir_all(f.workspace.join("wheels")).unwrap();
    // Both the original verified wheel source and builder wheel caches are
    // gone. A store view still works; native pip cannot consume unpacked store.
    for entry in std::fs::read_dir(&f.provisioner.config.build_root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            std::fs::remove_dir_all(entry.path()).unwrap();
        }
    }
    let legacy =
        development_dependencies::select_development_view(&f.library, &f.scope, "one").unwrap();
    let sandbox = BubblewrapSandbox::new(
        anchor_sandbox_bwrap::BubblewrapPolicy::new(&f.provisioner.config.bwrap, ["sh"])
            .authorize_workspace_root(&f.workspace),
    )
    .unwrap();
    let sandbox = legacy.view.authorize(Arc::new(sandbox)).unwrap();
    let mut request = SandboxRequest::new(
        &f.workspace,
        [legacy
            .view
            .view()
            .join("bin/probepkg-tool")
            .display()
            .to_string()],
    );
    legacy.view.apply(&mut request);
    let legacy_result = sandbox.run(request).await.unwrap();
    assert_eq!(legacy_result.exit_code, Some(0), "{legacy_result:?}");
    assert!(legacy_result.stdout.contains("hello 1.0"));
    drop(legacy);
    let failed = f
        .run("one", &["pip", "install", "--no-index", "probepkg==1.0"])
        .await;
    assert_ne!(failed["exit_code"], 0);
    assert!(
        failed["stderr"]
            .as_str()
            .unwrap()
            .contains("No matching distribution")
    );
    f.prepare("one", PrepareAction::Rebuild, &[]).await.unwrap();
    // Removing the Host selection cannot reinterpret a published private env
    // as legacy or authorize default clearing.
    let selection_path = project_root(&f.scope, "one").join("selection.json");
    let saved = std::fs::read(&selection_path).unwrap();
    std::fs::remove_file(&selection_path).unwrap();
    assert!(f.prepare("one", PrepareAction::Ensure, &[]).await.is_err());
    assert!(f.prepare("one", PrepareAction::Discard, &[]).await.is_err());
    assert!(
        select_environment(
            &f.library,
            &f.scope,
            "one",
            Some(&f.provisioner.config.python),
            &f.access
        )
        .await
        .is_err()
    );
    std::fs::write(&selection_path, saved).unwrap();
    f.ok("one", &["pip", "--version"]).await;
    let report = anchor_library::reclaim(&f.library, true).unwrap();
    assert!(report.store_phase_abandoned.is_none(), "{report:?}");
    assert!(
        store_package.exists(),
        "legacy reference must preserve its store bytes"
    );
    // A published damaged private selection must never fall back to legacy.
    std::fs::remove_file(project_root(&f.scope, "one").join("env/bin/pip")).unwrap();
    assert!(f.prepare("one", PrepareAction::Ensure, &[]).await.is_err());
    assert!(
        f.commands(f.access.cancellation.clone())
            .call(
                "anchor_run",
                json!({"project":"one","command":["python","-c","pass"]})
            )
            .await
            .is_err()
    );
    f.prepare("one", PrepareAction::Rebuild, &[]).await.unwrap();
    let leased = select_environment(
        &f.library,
        &f.scope,
        "one",
        Some(&f.provisioner.config.python),
        &f.access,
    )
    .await
    .unwrap();
    assert!(crate::pilot_resources::release_development_scope(&f.library, &f.scope).is_err());
    drop(leased);
    crate::pilot_resources::release_development_scope(&f.library, &f.scope).unwrap();
    assert!(!f.scope.join("development").exists());
    assert!(f.workspace.join("one").exists());
    assert!(f.prepare("one", PrepareAction::Ensure, &[]).await.is_err());
}

#[tokio::test]
async fn src_editable_extras_markers_and_recursive_requirements_execute_inside_sandbox() {
    let Some(f) = Fixture::new() else {
        return;
    };
    f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
    let project = f.workspace.join("one");
    std::fs::create_dir_all(project.join("src/editableprobe")).unwrap();
    std::fs::write(
        project.join("src/editableprobe/__init__.py"),
        "VALUE='first'\ndef main(): print(VALUE)\n",
    )
    .unwrap();
    std::fs::write(
        project.join("pyproject.toml"),
        include_str!("../../../tests/fixtures/development_environment/pyproject.toml"),
    )
    .unwrap();
    // A minimal PEP 660 backend uses only the shared stdlib. Its code and .pth
    // run in the real command sandbox; this fixture asserts that explicitly.
    std::fs::write(
        project.join("backend.py"),
        include_str!("../../../tests/fixtures/development_environment/backend.py"),
    )
    .unwrap();
    f.ok(
        "one",
        &[
            "pip",
            "install",
            "--no-index",
            "--find-links",
            "../wheels",
            "--no-build-isolation",
            "-e",
            ".[dev]",
        ],
    )
    .await;
    assert!(
        f.ok("one", &["editable-probe"]).await["stdout"]
            .as_str()
            .unwrap()
            .contains("first")
    );
    f.ok(
        "one",
        &[
            "python",
            "-c",
            "import editableprobe,probepkg,trialpkg; assert probepkg.VALUE == '1.0'",
        ],
    )
    .await;
    std::fs::write(
        project.join("src/editableprobe/__init__.py"),
        "VALUE='changed'\ndef main(): print(VALUE)\n",
    )
    .unwrap();
    assert!(
        f.ok("one", &["editable-probe"]).await["stdout"]
            .as_str()
            .unwrap()
            .contains("changed")
    );
    f.ok("one", &["python", "-m", "pip", "check"]).await;
    f.ok("one", &["pip", "uninstall", "-y", "editableprobe"])
        .await;
    f.ok("one",&["python","-c","import pathlib,importlib.util; assert not pathlib.Path('/development/env/bin/editable-probe').exists(); assert importlib.util.find_spec('editableprobe') is None"]).await;
    // requirements cannot authorize a Host path merely by naming it.
    let secret = f._root.path().join("host-only-requirements.txt");
    std::fs::write(&secret, "trialpkg==1.0\n").unwrap();
    std::fs::write(
        project.join("escape.txt"),
        format!("-r {}\n", secret.display()),
    )
    .unwrap();
    assert_ne!(
        f.run("one", &["pip", "install", "--no-index", "-r", "escape.txt"])
            .await["exit_code"],
        0
    );
    let remote = f
        .run(
            "one",
            &[
                "pip",
                "install",
                "--retries",
                "0",
                "--timeout",
                "1",
                "--index-url",
                "https://pypi.org/simple",
                "probepkg==999",
            ],
        )
        .await;
    assert_ne!(remote["exit_code"], 0);
    f.ok("one",&["python","-c","import socket; s=socket.socket(); s.settimeout(1);\ntry: s.connect(('1.1.1.1',443))\nexcept OSError as e: print('remote denied',e)\nelse: raise AssertionError('unexpected external network')"]).await;
}

#[tokio::test]
async fn project_lock_wait_is_cancellable_and_interrupted_observation_survives_reselection() {
    let Some(f) = Fixture::new() else {
        return;
    };
    f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
    let leased = select_environment(
        &f.library,
        &f.scope,
        "one",
        Some(&f.provisioner.config.python),
        &f.access,
    )
    .await
    .unwrap();
    f.prepare("two", PrepareAction::Ensure, &[]).await.unwrap();
    let access = DependencyAccess {
        network: false,
        cancellation: Arc::new(AtomicBool::new(false)),
    };
    let wait = select_environment(
        &f.library,
        &f.scope,
        "one",
        Some(&f.provisioner.config.python),
        &access,
    );
    let cancel = async {
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        access.cancellation.store(true, Ordering::Release);
    };
    let (wait, ()) = tokio::join!(wait, cancel);
    assert!(wait.err().unwrap().contains("cancelled"));
    leased.begin_operation().unwrap();
    // Simulate Host restart after a saved start fact, without replaying code.
    drop(leased);
    let result = f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
    assert_eq!(
        result["environment_observation"]["inspection_required"],
        true
    );
    let inspected = f.ok("one", &["python", "-m", "pip", "check"]).await;
    assert_eq!(
        inspected["environment_observation"]["inspection_required"],
        true
    );
    f.prepare("one", PrepareAction::Rebuild, &[]).await.unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let commands = f.commands(cancelled.clone());
    let run=commands.call("anchor_run",json!({"project":"one","command":["sh","-c","printf partial > /development/env/partial; (sleep 1; printf leaked > /development/env/leaked) >/dev/null 2>&1 & sleep 10"]}));
    let cancellation = async {
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        cancelled.store(true, Ordering::Release);
    };
    let (result, ()) = tokio::join!(run, cancellation);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("partially changed")
    );
    assert!(project_root(&f.scope, "one").join("env/partial").exists());
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(!project_root(&f.scope, "one").join("env/leaked").exists());
    assert_eq!(
        f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap()["environment_observation"]["inspection_required"],
        true
    );
    f.prepare("one", PrepareAction::Rebuild, &[]).await.unwrap();
    let refused = f
        .commands(f.access.cancellation.clone())
        .call(
            "anchor_run",
            json!({"project":"one","command":["unauthorized"]}),
        )
        .await;
    assert!(refused.is_err());
    assert_eq!(
        f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap()["environment_observation"]["inspection_required"],
        false
    );
    let commands = f.commands(f.access.cancellation.clone());
    let running = commands.call(
        "anchor_run",
        json!({"project":"one","command":["sh","-c","sleep 0.2; printf settled > settled"]}),
    );
    let preparing = async {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        f.prepare("one", PrepareAction::Ensure, &[]).await.unwrap();
        assert!(
            f.workspace.join("one/settled").exists(),
            "prepare returned before the project command settled"
        );
    };
    let (running, ()) = tokio::join!(running, preparing);
    assert!(running.is_ok());
    f.ok(
        "one",
        &[
            "sh",
            "-c",
            "(sleep 1; printf leaked > /development/env/completed-leak) >/dev/null 2>&1 & exit 0",
        ],
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(
        !project_root(&f.scope, "one")
            .join("env/completed-leak")
            .exists()
    );
    let timeout = commands.call("anchor_run",json!({"project":"one","timeout_seconds":1,"command":["sh","-c","printf partial > /development/env/timeout-partial; sleep 10"]})).await.unwrap();
    let ToolResultContent::Json { value } = &timeout[0] else {
        panic!("unexpected timeout result")
    };
    assert_eq!(value["status"], "timed_out");
    assert_eq!(
        value["environment_observation"]["inspection_required"],
        true
    );
}
