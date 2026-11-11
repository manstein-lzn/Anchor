use anchor_graph_host::{FilePluginCatalog, SystemPlugin, all_system_plugins};
use anchor_library::{InstallError, InstallRequest, Library};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

fn source(root: &Path, plugin: SystemPlugin) -> PathBuf {
    let directory = root.join("plugins").join(plugin.id());
    for (path, bytes) in plugin.public_files() {
        let target = directory.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    for path in plugin.executables() {
        let target = directory.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(target, fs::Permissions::from_mode(0o755)).unwrap();
    }
    directory
}

#[test]
fn ordinary_publication_and_cli_cannot_install_or_replace_reserved_ids() {
    let root = tempfile::tempdir().unwrap();
    let library_root = root.path().join("library");
    let library = Library::new(&library_root);
    for plugin in all_system_plugins() {
        let directory = source(&root.path().join("source"), *plugin);
        for replace in [false, true] {
            assert!(matches!(
                library.install_directory(plugin.id(), &directory, replace),
                Err(InstallError::SystemPluginProtected(_))
            ));
        }
        assert!(matches!(
            library.install(&InstallRequest {
                source: format!(
                    "https://github.com/example/plugins/tree/main/{}",
                    plugin.id()
                ),
                id: None,
                replace_existing: true,
            }),
            Err(InstallError::SystemPluginProtected(_))
        ));
        assert!(!library_root.exists());
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_anchor-library"))
            .arg("--root")
            .arg(&library_root)
            .args(["install", "--directory"])
            .arg(&directory)
            .args(["--id", plugin.id(), "--replace-existing"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!library_root.exists());
    }
    let lease = library.install_lease().unwrap();
    assert!(matches!(
        library.install_directory_locked("graph-tasks", root.path().join("unused"), true, &lease),
        Err(InstallError::SystemPluginProtected(_))
    ));
    assert!(!library_root.join("plugins/graph-tasks").exists());
}

#[test]
fn trusted_bootstrap_requires_the_admitted_full_digest_and_never_replaces() {
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    let library_root = root.path().join("library");
    let library = Library::new(&library_root);
    for plugin in all_system_plugins() {
        let directory = source(&source_root, *plugin);
        let expected = FilePluginCatalog::new(&source_root)
            .definition(plugin.id())
            .unwrap()
            .digest;
        assert!(matches!(
            library.install_system_directory(*plugin, &directory, "wrong-release-digest"),
            Err(InstallError::SystemPluginMismatch)
        ));
        assert!(!library_root.join("plugins").join(plugin.id()).exists());
        let installed = library
            .install_system_directory(*plugin, &directory, &expected)
            .unwrap();
        assert_eq!(installed.digest, expected);
        assert_eq!(
            FilePluginCatalog::new(&library_root)
                .definition(plugin.id())
                .unwrap()
                .system,
            Some(*plugin)
        );
        assert!(matches!(
            library.install_system_directory(*plugin, &directory, &expected),
            Err(InstallError::AlreadyExists)
        ));
        assert!(matches!(
            library.install_directory(plugin.id(), &directory, true),
            Err(InstallError::SystemPluginProtected(_))
        ));
    }
}

#[test]
fn trusted_bootstrap_refuses_tampered_binaries_and_existing_collisions() {
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    let directory = source(&source_root, SystemPlugin::Network);
    let expected = FilePluginCatalog::new(&source_root)
        .definition("network")
        .unwrap()
        .digest;
    fs::write(
        directory.join("bin/anchor-network-tools"),
        "different executable bytes",
    )
    .unwrap();
    let library_root = root.path().join("library");
    let library = Library::new(&library_root);
    assert!(matches!(
        library.install_system_directory(SystemPlugin::Network, &directory, &expected),
        Err(InstallError::SystemPluginMismatch)
    ));
    let collision = library_root.join("plugins/network");
    fs::create_dir_all(&collision).unwrap();
    fs::write(
        collision.join("plugin.json"),
        r#"{"name":"local collision"}"#,
    )
    .unwrap();
    assert!(matches!(
        library.install_system_directory(SystemPlugin::Network, &directory, &expected),
        Err(InstallError::AlreadyExists)
    ));
    assert_eq!(
        fs::read_to_string(collision.join("plugin.json")).unwrap(),
        r#"{"name":"local collision"}"#
    );
    assert!(
        FilePluginCatalog::new(library_root)
            .definition("network")
            .is_err()
    );
}

#[test]
fn operator_cli_bootstrap_uses_the_verified_review_kind_and_exact_digest() {
    let root = tempfile::tempdir().unwrap();
    let directory = source(&root.path().join("source"), SystemPlugin::GraphTasks);
    let library_root = root.path().join("library");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_anchor-library"))
        .arg("--root")
        .arg(&library_root)
        .args(["install", "--directory"])
        .arg(&directory)
        .args(["--id", "graph-tasks", "--review"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let review: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(review["kind"], "system");
    assert!(!library_root.exists());
    let invoke = |id: &str, digest: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_anchor-library"))
            .arg("--root")
            .arg(&library_root)
            .args(["install-system", "--directory"])
            .arg(&directory)
            .args(["--id", id, "--digest", digest])
            .output()
            .unwrap()
    };
    assert!(
        !invoke("graph-tasks", "not-the-reviewed-digest")
            .status
            .success()
    );
    assert!(!library_root.join("plugins/graph-tasks").exists());
    assert!(
        !invoke("extension", review["digest"].as_str().unwrap())
            .status
            .success()
    );
    let installed = invoke("graph-tasks", review["digest"].as_str().unwrap());
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert!(
        !invoke("graph-tasks", review["digest"].as_str().unwrap())
            .status
            .success()
    );
    assert_eq!(
        FilePluginCatalog::new(&library_root)
            .definition("graph-tasks")
            .unwrap()
            .digest,
        review["digest"]
    );
}

#[test]
fn explicit_system_upgrade_rechecks_both_digests_across_public_resource_releases() {
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    let directory = source(&source_root, SystemPlugin::GraphTasks);
    let expected_new = FilePluginCatalog::new(&source_root)
        .definition("graph-tasks")
        .unwrap()
        .digest;
    let library_root = root.path().join("library");
    let library = Library::new(&library_root);
    assert!(matches!(
        library.upgrade_system_directory(
            SystemPlugin::GraphTasks,
            &directory,
            &expected_new,
            "missing"
        ),
        Err(InstallError::SystemPluginMismatch)
    ));
    let installed = source(&library_root, SystemPlugin::GraphTasks);
    let old_skill = installed.join("skills/graph-tasks/SKILL.md");
    fs::write(&old_skill, "previous release instructions").unwrap();
    let catalog = FilePluginCatalog::new(&library_root);
    assert!(catalog.definition("graph-tasks").is_err());
    let expected_old = catalog
        .inspect_system_plugin(SystemPlugin::GraphTasks)
        .unwrap()
        .digest;
    assert!(matches!(
        library.upgrade_system_directory(
            SystemPlugin::GraphTasks,
            &directory,
            &expected_new,
            "stale-old-digest"
        ),
        Err(InstallError::SystemPluginMismatch)
    ));
    assert_eq!(
        fs::read_to_string(&old_skill).unwrap(),
        "previous release instructions"
    );
    assert!(matches!(
        library.upgrade_system_directory(
            SystemPlugin::GraphTasks,
            &directory,
            "wrong-new-digest",
            &expected_old
        ),
        Err(InstallError::SystemPluginMismatch)
    ));
    assert_eq!(
        fs::read_to_string(&old_skill).unwrap(),
        "previous release instructions"
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_anchor-library"))
        .arg("--root")
        .arg(&library_root)
        .args(["inspect-system", "--id", "graph-tasks"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let inspection: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inspection["digest"], expected_old);
    assert_eq!(inspection["executionAuthorized"], false);
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_anchor-library"))
        .arg("--root")
        .arg(&library_root)
        .args(["upgrade-system", "--directory"])
        .arg(&directory)
        .args([
            "--id",
            "graph-tasks",
            "--digest",
            &expected_new,
            "--existing-digest",
            &expected_old,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let upgraded = catalog.definition("graph-tasks").unwrap();
    assert_eq!(upgraded.system, Some(SystemPlugin::GraphTasks));
    assert_eq!(upgraded.digest, expected_new);
}

#[test]
fn a_system_upgrade_refuses_old_declared_data_instead_of_removing_it() {
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    let directory = source(&source_root, SystemPlugin::GraphTasks);
    let new = FilePluginCatalog::new(&source_root)
        .definition("graph-tasks")
        .unwrap()
        .digest;
    let library_root = root.path().join("library");
    let installed = source(&library_root, SystemPlugin::GraphTasks);
    fs::write(
        installed.join("plugin.json"),
        r#"{"name":"old release","mutable":["data/"]}"#,
    )
    .unwrap();
    fs::create_dir_all(installed.join("data")).unwrap();
    fs::write(installed.join("data/important"), "keep this data").unwrap();
    let old = FilePluginCatalog::new(&library_root)
        .inspect_system_plugin(SystemPlugin::GraphTasks)
        .unwrap()
        .digest;
    assert!(matches!(
        Library::new(&library_root).upgrade_system_directory(
            SystemPlugin::GraphTasks,
            &directory,
            &new,
            &old
        ),
        Err(InstallError::SystemPluginMigrationRequired)
    ));
    assert_eq!(
        fs::read_to_string(installed.join("data/important")).unwrap(),
        "keep this data"
    );
    assert!(
        FilePluginCatalog::new(library_root)
            .definition("graph-tasks")
            .is_err()
    );
}
