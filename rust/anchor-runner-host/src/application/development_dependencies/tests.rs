use super::test_support::legacy_view;
use super::*;
use crate::application::RunApplication;
use crate::dependency_provision::DependencyAccess;
use crate::dependency_provision::{
    DependencyProvisioner,
    test_support::{config, fixture_wheel, toolchain},
};
use anchor_runtime::{SandboxPort, SandboxRequest, SandboxStatus};
use std::{sync::Arc, time::Duration};

async fn command(
    view: &crate::plugin_view::PreparedView,
    workspace: &Path,
    sandbox: &anchor_sandbox_bwrap::BubblewrapSandbox,
    command: Vec<String>,
) -> String {
    let sandbox = view.authorize(Arc::new(sandbox.clone())).unwrap();
    std::fs::create_dir_all(workspace).unwrap();
    let mut request = SandboxRequest::new(workspace.to_owned(), command);
    view.apply(&mut request);
    let result = sandbox.run(request).await.unwrap();
    assert_eq!(result.status, SandboxStatus::Completed, "{result:?}");
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    result.stdout
}

#[test]
fn projects_are_relative_normalized_and_cannot_traverse_host_paths() {
    for (input, expected) in [
        (".", "."),
        ("./project/", "project"),
        ("one/./two", "one/two"),
    ] {
        assert_eq!(normalize_project(input).unwrap(), expected);
    }
    for input in [
        "",
        "../outside",
        "one/../../two",
        "/tmp/project",
        "one\\two",
        "one\0two",
    ] {
        assert!(normalize_project(input).is_err(), "{input:?}");
    }
}

#[tokio::test]
async fn a_runtime_published_by_another_session_only_changes_new_turn_dependency_preparation() {
    use crate::runtime_configuration::{RuntimeConfiguration, RuntimeSnapshot};

    let Some(chain) = toolchain() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let wheels = root.path().join("wheels");
    std::fs::create_dir_all(&wheels).unwrap();
    fixture_wheel(&wheels, "idna", "3.6");
    let old_provisioner = Arc::new(
        DependencyProvisioner::new(config(
            &root.path().join("old-build"),
            &chain,
            wheels.clone(),
        ))
        .unwrap(),
    );

    // A second prefix gives a genuinely different interpreter path without
    // downloading a Python or requiring two versions on the regression host.
    // Its stdlib stays in the existing installation and remains read-only.
    let output = std::process::Command::new(&chain.python)
        .args(["-I", "-c", "import json,os,sys,sysconfig; stdlib=sysconfig.get_path('stdlib'); print(json.dumps([stdlib,os.path.relpath(stdlib,sys.base_prefix)]))"])
        .env_clear().env("PATH", "/usr/bin:/bin").output().unwrap();
    assert!(output.status.success());
    let [stdlib, relative]: [String; 2] = serde_json::from_slice(&output.stdout).unwrap();
    let python_root = root.path().join("new-python");
    let new_python = python_root
        .join("bin")
        .join(chain.python.file_name().unwrap());
    std::fs::create_dir_all(new_python.parent().unwrap()).unwrap();
    std::fs::copy(&chain.python, &new_python).unwrap();
    let stdlib_link = python_root.join(relative);
    std::fs::create_dir_all(stdlib_link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(stdlib, stdlib_link).unwrap();
    let mut new_config = config(&root.path().join("new-build"), &chain, wheels);
    new_config.python = new_python.clone();
    let new_provisioner = Arc::new(DependencyProvisioner::new(new_config).unwrap());
    let library = root.path().join("library");
    let data = root.path().join("data");
    let unavailable_snapshot = RuntimeSnapshot {
        python: None,
        python_enabled: false,
        rust: None,
        pixi: None,
        provisioner: None,
    };
    let configuration = Arc::new(RuntimeConfiguration::from_snapshot_for_test(
        &data,
        &library,
        old_provisioner.config.bwrap.clone(),
        unavailable_snapshot.clone(),
    ));
    let application = RunApplication::new(data.clone(), library.clone())
        .with_runtime_configuration(configuration.clone());
    let unavailable_turn = application
        .clone()
        .with_frozen_runtime_snapshot(unavailable_snapshot)
        .with_dependency_provisioner(old_provisioner.clone());
    let old_snapshot = RuntimeSnapshot {
        python: Some(chain.python.clone()),
        python_enabled: true,
        rust: None,
        pixi: None,
        provisioner: Some(old_provisioner.clone()),
    };
    configuration.publish_snapshot_for_test(old_snapshot.clone());
    let old_turn = application
        .clone()
        .with_frozen_runtime_snapshot(old_snapshot);
    configuration.publish_snapshot_for_test(RuntimeSnapshot {
        python: Some(new_python.clone()),
        python_enabled: true,
        rust: None,
        pixi: None,
        provisioner: Some(new_provisioner),
    });
    assert_eq!(
        old_turn.dependency_provisioner().unwrap().config.python,
        chain.python
    );
    assert!(
        unavailable_turn.dependency_provisioner().is_err(),
        "a frozen unavailable grant must not fall back to dynamic configuration or a test provisioner"
    );
    assert_eq!(
        application.dependency_provisioner().unwrap().config.python,
        new_python
    );
    assert_eq!(
        application.runtime_configuration().map(Arc::as_ptr),
        old_turn.runtime_configuration().map(Arc::as_ptr),
        "frozen capability must preserve access to runtime management"
    );
    let unconfigured = RunApplication::new(root.path().join("unconfigured"), library);
    assert!(unconfigured.dependency_provisioner().is_err());
    assert_eq!(unconfigured.pilot_dependency_status()["configured"], false);
}

#[tokio::test]
async fn legacy_project_views_share_bytes_and_selection_rejects_inconsistent_facts() {
    let Some(chain) = toolchain() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let wheels = root.path().join("wheels");
    std::fs::create_dir_all(&wheels).unwrap();
    fixture_wheel(&wheels, "idna", "3.6");
    fixture_wheel(&wheels, "idna", "3.7");
    fixture_wheel(&wheels, "pytest", "8.0");
    let provisioner =
        Arc::new(DependencyProvisioner::new(config(root.path(), &chain, wheels.clone())).unwrap());
    let library = root.path().join("library");
    let scope_path = root.path().join("pilot/owner-session");
    let access = DependencyAccess {
        network: false,
        ..Default::default()
    };
    let requirements_one = ["idna==3.6".into()];
    let development_one = ["pytest==8.0".into()];
    let requirements_two = ["idna==3.7".into()];
    let first = legacy_view(
        &provisioner,
        &library,
        &scope_path,
        "one",
        &requirements_one,
        &development_one,
        &access,
    );
    let second = legacy_view(
        &provisioner,
        &library,
        &scope_path,
        "two",
        &requirements_two,
        &[],
        &access,
    );
    let (first, second) = tokio::join!(first, second);
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.resolved.packages.len(), 2);
    assert_eq!(second.resolved.packages.len(), 1);
    let one = select_development_view(&library, &scope_path, "one").unwrap();
    let two = select_development_view(&library, &scope_path, "two").unwrap();
    assert_ne!(one.view.view(), two.view.view());
    assert!(
        crate::pilot_resources::release_development_scope(&library, &scope_path)
            .unwrap_err()
            .contains("in use")
    );
    let store = anchor_graph_host::store_root(&library);
    assert_eq!(one.view.view().join("lib").read_dir().unwrap().count(), 1);
    let first_refs =
        crate::plugin_view::referenced_store_directories(one.view.view(), &store).unwrap();
    assert!(first_refs.iter().any(|path| path.ends_with("idna/3.6")));
    assert!(first_refs.iter().any(|path| path.ends_with("pytest/8.0")));
    assert!(
        crate::plugin_view::referenced_store_directories(two.view.view(), &store)
            .unwrap()
            .iter()
            .any(|path| path.ends_with("idna/3.7"))
    );
    assert_eq!(
        one.view.view().join("bin/python").canonicalize().unwrap(),
        chain.python
    );
    let command_workspace = root.path().join("build/development-probe");
    let output = command(
        &one.view,
        &command_workspace,
        &provisioner.sandbox,
        vec![
            one.view
                .view()
                .join("bin/python")
                .to_string_lossy()
                .into_owned(),
            "-c".into(),
            "import idna,pytest; print(idna.__version__,pytest.__version__)".into(),
        ],
    )
    .await;
    assert_eq!(output.trim(), "3.6 8.0");
    let output = command(
        &two.view,
        &command_workspace,
        &provisioner.sandbox,
        vec![
            two.view
                .view()
                .join("bin/python")
                .to_string_lossy()
                .into_owned(),
            "-c".into(),
            "import idna; print(idna.__version__)".into(),
        ],
    )
    .await;
    assert_eq!(output.trim(), "3.7");
    let output = command(
        &one.view,
        &command_workspace,
        &provisioner.sandbox,
        vec![
            one.view
                .view()
                .join("bin/pytest-tool")
                .to_string_lossy()
                .into_owned(),
        ],
    )
    .await;
    assert_eq!(output.trim(), "hello 8.0");
    let output = command(&one.view, &command_workspace, &provisioner.sandbox, vec![one.view.view().join("bin/python").to_string_lossy().into_owned(), "-c".into(), format!("from pathlib import Path\ntry:\n Path({:?}).write_text('bad')\nexcept OSError:\n print('read-only')\nelse:\n raise Exception('shared package was writable')", store.join("idna/3.6/idna/__init__.py").to_string_lossy())]).await;
    assert_eq!(output.trim(), "read-only");
    let third = legacy_view(
        &provisioner,
        &library,
        &scope_path,
        "three",
        &["idna==3.6".into()],
        &[],
        &access,
    )
    .await
    .unwrap();
    let runtime_only = third.resolved;

    // Selection must establish a complete ready fact and the exact shared
    // interpreter/package set rather than accepting a merely existing venv.
    let ready = one
        .view
        .view()
        .parent()
        .unwrap()
        .join(format!("preparation-{}.json", first.declaration_digest));
    let ready_bytes = std::fs::read(&ready).unwrap();
    std::fs::remove_file(&ready).unwrap();
    assert!(select_development_view(&library, &scope_path, "one").is_err());
    std::fs::write(&ready, &ready_bytes).unwrap();
    let current = one
        .view
        .view()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("current.json");
    let current_bytes = std::fs::read(&current).unwrap();
    let mut changed = first.clone();
    changed.resolved.packages.pop();
    std::fs::write(&current, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(select_development_view(&library, &scope_path, "one").is_err());
    std::fs::write(current, current_bytes).unwrap();
    let python = one.view.view().join("bin/python");
    std::fs::remove_file(&python).unwrap();
    std::os::unix::fs::symlink("/bin/false", &python).unwrap();
    assert!(select_development_view(&library, &scope_path, "one").is_err());
    std::fs::remove_file(&python).unwrap();
    std::os::unix::fs::symlink(&chain.python, &python).unwrap();
    let additional = crate::plugin_view::site_packages_directories(one.view.view()).unwrap()[0]
        .join("unexpected.py");
    std::os::unix::fs::symlink(store.join("idna/3.7/idna/__init__.py"), &additional).unwrap();
    assert!(select_development_view(&library, &scope_path, "one").is_err());
    std::fs::remove_file(additional).unwrap();
    assert!(select_development_view(&library, &scope_path, "one").is_ok());
    // Production landing can reuse the shared package without its original wheel.
    std::fs::remove_dir_all(&wheels).unwrap();
    std::fs::create_dir_all(&wheels).unwrap();
    std::fs::remove_dir_all(root.path().join("build/artifacts")).unwrap();
    std::fs::remove_dir_all(root.path().join("build/cache")).unwrap();
    crate::plugin_dependencies::land_with_access(
        &provisioner,
        &library,
        "formal",
        "runtime-declaration",
        &runtime_only,
        &access,
    )
    .await
    .unwrap();
    let formal = anchor_graph_host::plugin_view_path(&library, "formal", "runtime-declaration");
    assert_eq!(
        crate::plugin_view::referenced_store_directories(&formal, &store).unwrap(),
        vec![store.join("idna/3.6")]
    );
    let scope = crate::plugin_library::PluginLibrary::new(&library)
        .development_scope(&scope_path, true)
        .unwrap()
        .unwrap();
    let candidate = crate::plugin_dependencies::prepare_candidate(
        &provisioner,
        &library,
        &scope,
        "candidate",
        "code-digest",
        &runtime_only,
        &access,
    )
    .await
    .unwrap();
    assert_eq!(
        crate::plugin_view::referenced_store_directories(&candidate.view, &store).unwrap(),
        vec![store.join("idna/3.6")]
    );
    assert_eq!(std::fs::read_dir(store.join("idna")).unwrap().count(), 2);
    drop(scope);
    drop(one);
    drop(two);
    tokio::time::timeout(Duration::from_secs(2), async {
        crate::pilot_resources::release_development_scope(&library, &scope_path)
    })
    .await
    .unwrap()
    .unwrap();
    assert!(formal.is_dir() && store.join("idna/3.6").is_dir());
    let formal_view =
        crate::plugin_view::PreparedView::for_candidate(&formal, &store, "runtime-declaration")
            .unwrap();
    let output = command(
        &formal_view,
        &command_workspace,
        &provisioner.sandbox,
        vec![
            formal_view
                .view()
                .join("bin/python")
                .to_string_lossy()
                .into_owned(),
            "-c".into(),
            "import idna; print(idna.__version__)".into(),
        ],
    )
    .await;
    assert_eq!(output.trim(), "3.6");
}

#[tokio::test]
async fn separate_distributions_in_a_namespace_are_imported_together_without_package_copies() {
    let Some(chain) = toolchain() else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let wheels = root.path().join("wheels");
    std::fs::create_dir_all(&wheels).unwrap();
    for (name, module) in [("namespace_one", "one"), ("namespace_two", "two")] {
        let files = vec![
            (
                format!("shared/{module}.py"),
                format!("VALUE = '{module}'\n").into_bytes(),
            ),
            (
                format!("{name}-1.0.dist-info/METADATA"),
                format!("Metadata-Version: 2.1\nName: {name}\nVersion: 1.0\n").into_bytes(),
            ),
            (
                format!("{name}-1.0.dist-info/WHEEL"),
                b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n".to_vec(),
            ),
            (format!("{name}-1.0.dist-info/RECORD"), Vec::new()),
        ];
        std::fs::write(
            wheels.join(format!("{name}-1.0-py3-none-any.whl")),
            crate::dependency_provision::test_support::write_zip(&files),
        )
        .unwrap();
    }
    let provisioner =
        Arc::new(DependencyProvisioner::new(config(root.path(), &chain, wheels)).unwrap());
    let library = root.path().join("library");
    let identity = root.path().join("pilot/namespace");
    legacy_view(
        &provisioner,
        &library,
        &identity,
        "project",
        &["namespace_one==1.0".into(), "namespace_two==1.0".into()],
        &[],
        &DependencyAccess {
            network: false,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let selected = select_development_view(&library, &identity, "project").unwrap();
    let output = command(&selected.view, &root.path().join("build/namespace-probe"), &provisioner.sandbox,
        vec![selected.view.view().join("bin/python").to_string_lossy().into_owned(), "-c".into(), "from shared.one import VALUE as one; from shared.two import VALUE as two; print(one,two)".into()]).await;
    assert_eq!(output.trim(), "one two");
    let refs = crate::plugin_view::referenced_store_directories(
        selected.view.view(),
        &anchor_graph_host::store_root(&library),
    )
    .unwrap();
    assert_eq!(refs.len(), 2);
    assert_eq!(
        anchor_library::reclaim(&library, true)
            .unwrap()
            .kept_referenced,
        2
    );
}
