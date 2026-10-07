mod support;

use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
};

use anchor_graph_host::{FileGraphBundleLoader, FilePluginCatalog, PluginCatalog};
use anchor_library::{Checkout, GithubSource, InstallError, Library};
use rustix::fs::{FileType, Mode};
use serde_json::json;
use support::{FixtureCheckout, fixture, request, write};

#[test]
fn github_fixture_install_flattens_manifest_and_reuses_catalog_identity() {
    let temporary = tempfile::tempdir().unwrap();
    let library_root = temporary.path().join("library");
    let outcome = Library::new(&library_root)
        .install_with_checkout(&request(None, false), &FixtureCheckout)
        .unwrap();
    assert_eq!(outcome.id, "demo");
    assert_eq!(outcome.directory, library_root.join("plugins/demo"));
    assert!(outcome.directory.join("plugin.json").is_file());
    assert!(!outcome.directory.join(".codex-plugin").exists());
    let binding = FilePluginCatalog::new(&library_root)
        .resolve(std::slice::from_ref(&outcome.id))
        .unwrap()
        .remove(0);
    assert_eq!(binding.digest, outcome.digest);
    assert_eq!(binding.resources, ["plugin.json", "skills/check/SKILL.md"]);
    assert!(
        !serde_json::to_string(&outcome)
            .unwrap()
            .contains("github.com")
    );
    assert_clean(&library_root);
}

#[test]
fn catalog_read_guard_coordinates_with_the_independent_installer_process() {
    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", true);
    let root = temporary.path().join("library");
    let library = Library::new(&root);
    assert!(library.catalog_read_guard().unwrap().is_none());
    assert!(!root.exists());
    library.install_directory("demo", &source, false).unwrap();
    let guard = library.catalog_read_guard().unwrap().unwrap();
    let mut installer = std::process::Command::new(env!("CARGO_BIN_EXE_anchor-library"))
        .arg("--root")
        .arg(&root)
        .args(["install", "--directory"])
        .arg(&source)
        .args(["--id", "demo", "--replace-existing"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(60));
    let completed_while_guarded = installer.try_wait().unwrap().is_some();
    drop(guard);
    assert!(installer.wait().unwrap().success());
    assert!(!completed_while_guarded);
}

#[test]
fn local_operator_install_handles_both_manifests_and_preserves_executable_files() {
    for hidden in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let source = fixture(temporary.path(), "source", hidden);
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).unwrap();
        write(source.join(".git/config"), "not installed");
        write(source.join("tools/.git/config"), "not installed");
        write(source.join("tools/probe"), "fixture executable");
        fs::set_permissions(
            source.join("tools/probe"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        if hidden {
            write(
                source.join(".codex-plugin/unused.txt"),
                "discarded source metadata",
            );
        }
        let root = temporary.path().join("library");
        let outcome = Library::new(&root)
            .install_directory("operator-1.2", &source, false)
            .unwrap();
        assert_eq!(
            fs::metadata(&outcome.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert!(!outcome.directory.join(".git").exists());
        assert!(!outcome.directory.join("tools/.git").exists());
        assert!(!outcome.directory.join(".codex-plugin").exists());
        assert_eq!(
            fs::metadata(outcome.directory.join("tools/probe"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            FilePluginCatalog::new(&root)
                .definition(&outcome.id)
                .unwrap()
                .digest,
            outcome.digest
        );
        assert!(
            source
                .join(if hidden {
                    ".codex-plugin/plugin.json"
                } else {
                    "plugin.json"
                })
                .is_file()
        );
        assert_clean(&root);
    }
}

#[test]
fn root_manifest_takes_precedence_over_codex_manifest() {
    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", false);
    write(
        source.join(".codex-plugin/plugin.json"),
        "malformed and ignored",
    );
    let root = temporary.path().join("library");
    let outcome = Library::new(&root)
        .install_directory("demo", source, false)
        .unwrap();
    assert_eq!(
        FilePluginCatalog::new(root)
            .definition(&outcome.id)
            .unwrap()
            .name,
        "source"
    );
}

#[test]
fn explicit_replacement_changes_identity_without_mutating_frozen_graph_bundle() {
    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", true);
    let root = temporary.path().join("library");
    let library = Library::new(&root);
    let before = library.install_directory("demo", &source, false).unwrap();
    let catalog = FilePluginCatalog::new(&root);
    let binding = catalog.resolve(&["demo".into()]).unwrap().remove(0);
    let bundle = temporary.path().join("bundle");
    for relative in &binding.resources {
        write(
            bundle.join("plugins/demo").join(relative),
            fs::read(before.directory.join(relative)).unwrap(),
        );
    }
    write(bundle.join("graph.json"), support::graph().to_string());
    write(bundle.join("manifest.json"), json!({
        "format": 1,
        "graph": "graph.json",
        "plugins": [{"id": binding.id, "digest": binding.digest, "resources": binding.resources, "mcp_servers": binding.mcp_servers}]
    }).to_string());
    let loader = FileGraphBundleLoader::new(&bundle);
    let frozen = loader.load().unwrap();
    assert_eq!(frozen.plugins[0].digest, before.digest);
    write(
        source.join("resources/evidence.txt"),
        "new mutable source content",
    );
    let after = library.install_directory("demo", &source, true).unwrap();
    assert_ne!(after.digest, before.digest);
    assert_eq!(catalog.definition("demo").unwrap().digest, after.digest);
    assert_eq!(loader.load().unwrap().plugins, frozen.plugins);
    assert_eq!(
        fs::read(bundle.join("plugins/demo/resources/evidence.txt")).unwrap(),
        b"source"
    );
    write(
        bundle.join("plugins/demo/resources/evidence.txt"),
        "drift inside frozen bundle",
    );
    assert!(loader.load().is_err());
    assert_clean(&root);
}

#[test]
fn malformed_replacements_never_change_existing_install() {
    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", false);
    let root = temporary.path().join("library");
    let library = Library::new(&root);
    let before = library.install_directory("demo", &source, false).unwrap();
    let manifest_path = source.join("plugin.json");
    let original = fs::read(&manifest_path).unwrap();
    for malformed in [
        "not json",
        "[]",
        "{}",
        r#"{"name":false}"#,
        r#"{"name":"valid","description":42}"#,
        r#"{"name":"valid","skills":"../outside"}"#,
        r#"{"name":"valid","mcpServers":"../outside.json"}"#,
        r#"{"name":"valid","mcpServers":{"probe":{"url":"https://user:sentinel-secret@host.example"}}}"#,
    ] {
        write(&manifest_path, malformed);
        let error = library
            .install_directory("demo", &source, true)
            .unwrap_err();
        assert!(matches!(error, InstallError::InvalidPlugin), "{malformed}");
        assert!(!error.to_string().contains("sentinel-secret"));
        assert_eq!(
            FilePluginCatalog::new(&root)
                .definition("demo")
                .unwrap()
                .digest,
            before.digest
        );
        assert_clean(&root);
    }
    fs::remove_file(&manifest_path).unwrap();
    assert!(matches!(
        library.install_directory("demo", &source, true),
        Err(InstallError::MissingManifest)
    ));
    write(&manifest_path, original);
    for private in [".env", ".env.fixture", ".mcp-auth/credentials.json"] {
        write(source.join(private), "sentinel-secret");
        assert!(matches!(
            library.install_directory("demo", &source, true),
            Err(InstallError::InvalidPlugin)
        ));
        assert_eq!(
            FilePluginCatalog::new(&root)
                .definition("demo")
                .unwrap()
                .digest,
            before.digest
        );
        if source.join(private).is_dir() {
            fs::remove_dir_all(source.join(private)).unwrap();
        } else {
            fs::remove_file(source.join(private)).unwrap();
        }
        if source.join(".mcp-auth").exists() {
            fs::remove_dir_all(source.join(".mcp-auth")).unwrap();
        }
    }
}

#[test]
fn malformed_new_install_never_publishes_a_directory() {
    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", false);
    write(source.join("plugin.json"), "malformed");
    let root = temporary.path().join("library");
    assert!(matches!(
        Library::new(&root).install_directory("demo", source, false),
        Err(InstallError::InvalidPlugin)
    ));
    assert!(!root.join("plugins/demo").exists());
    assert_clean(&root);
}

struct MustNotCheckout;

impl Checkout for MustNotCheckout {
    fn checkout(&self, _: &GithubSource, _: &Path) -> Result<(), InstallError> {
        panic!("invalid or conflicting request must not invoke checkout")
    }
}

#[test]
fn invalid_ids_and_sources_are_rejected_before_any_checkout_or_filesystem_change() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let library = Library::new(&root);
    for id in [
        "",
        ".",
        "..",
        "../outside",
        ".hidden",
        "-option",
        "a/b",
        "a\\b",
        "a b",
        "a\0b",
    ] {
        assert!(matches!(
            library.install_with_checkout(&request(Some(id), false), &MustNotCheckout),
            Err(InstallError::InvalidId)
        ));
        assert!(matches!(
            library.install_directory(id, temporary.path(), false),
            Err(InstallError::InvalidId)
        ));
    }
    let mut invalid = request(None, false);
    invalid.source = "https://user:sentinel-secret@github.com/owner/repo/tree/main/plugin".into();
    let error = library
        .install_with_checkout(&invalid, &MustNotCheckout)
        .unwrap_err();
    assert!(matches!(error, InstallError::InvalidSource));
    assert!(!error.to_string().contains("sentinel-secret"));
    assert!(!root.exists());
}

#[test]
fn existing_plugin_is_rejected_without_checkout_unless_replace_is_explicit() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let library = Library::new(&root);
    let first = library
        .install_with_checkout(&request(None, false), &FixtureCheckout)
        .unwrap();
    assert!(matches!(
        library.install_with_checkout(&request(None, false), &MustNotCheckout),
        Err(InstallError::AlreadyExists)
    ));
    assert_eq!(
        FilePluginCatalog::new(&root)
            .definition("demo")
            .unwrap()
            .digest,
        first.digest
    );
}

#[test]
fn rejects_internal_symlinks_and_special_files_including_ignored_metadata() {
    for relative in [
        "escape",
        "resources/escape",
        ".codex-plugin/escape",
        ".git/escape",
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let source = fixture(temporary.path(), "source", false);
        let root = temporary.path().join("library");
        let library = Library::new(&root);
        let first = library.install_directory("demo", &source, false).unwrap();
        let link = source.join(relative);
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(source.join("plugin.json"), &link).unwrap();
        assert!(matches!(
            library.install_directory("demo", &source, true),
            Err(InstallError::UnsafePath)
        ));
        fs::remove_file(&link).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &link,
            FileType::Fifo,
            Mode::from_bits_truncate(0o600),
            0,
        )
        .unwrap();
        assert!(matches!(
            library.install_directory("demo", &source, true),
            Err(InstallError::UnsafePath)
        ));
        assert_eq!(
            FilePluginCatalog::new(&root)
                .definition("demo")
                .unwrap()
                .digest,
            first.digest
        );
        assert_clean(&root);
    }
}

#[test]
fn rejects_ancestor_and_root_symlinks_for_sources_library_plugins_lock_and_destination() {
    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", false);
    let alias = temporary.path().join("alias");
    symlink(temporary.path(), &alias).unwrap();
    let root = temporary.path().join("library");
    let library = Library::new(&root);
    assert!(matches!(
        library.install_directory("demo", alias.join("source"), false),
        Err(InstallError::UnsafePath)
    ));
    assert!(matches!(
        Library::new(alias.join("root")).install_directory("demo", &source, false),
        Err(InstallError::UnsafePath)
    ));
    let external = temporary.path().join("external");
    fs::create_dir(&external).unwrap();
    fs::remove_dir_all(&root).unwrap();
    fs::create_dir(&root).unwrap();
    symlink(&external, root.join("plugins")).unwrap();
    assert!(matches!(
        library.install_directory("demo", &source, false),
        Err(InstallError::UnsafePath)
    ));
    fs::remove_file(root.join("plugins")).unwrap();
    fs::create_dir(root.join("plugins")).unwrap();
    write(external.join("lock"), "do not alter");
    symlink(external.join("lock"), root.join("plugins/.install.lock")).unwrap();
    assert!(matches!(
        library.install_directory("demo", &source, false),
        Err(InstallError::UnsafePath)
    ));
    assert_eq!(fs::read(external.join("lock")).unwrap(), b"do not alter");
    fs::remove_file(root.join("plugins/.install.lock")).unwrap();
    symlink(&source, root.join("plugins/demo")).unwrap();
    for replace in [false, true] {
        assert!(matches!(
            library.install_directory("demo", &source, replace),
            Err(InstallError::UnsafePath)
        ));
    }
    assert_eq!(
        fs::read(source.join("resources/evidence.txt")).unwrap(),
        b"source"
    );
}

#[test]
fn checkout_failure_cleans_private_staging_and_preserves_existing_plugin() {
    struct FailingCheckout;
    impl Checkout for FailingCheckout {
        fn checkout(&self, _: &GithubSource, destination: &Path) -> Result<(), InstallError> {
            write(destination.join("partial"), "partial checkout");
            Err(InstallError::Io(std::io::Error::other(
                "sentinel-secret from environment and stderr",
            )))
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    let library = Library::new(&root);
    let first = library
        .install_with_checkout(&request(None, false), &FixtureCheckout)
        .unwrap();
    let error = library
        .install_with_checkout(&request(None, true), &FailingCheckout)
        .unwrap_err();
    assert!(!error.to_string().contains("sentinel-secret"));
    assert_eq!(
        FilePluginCatalog::new(&root)
            .definition("demo")
            .unwrap()
            .digest,
        first.digest
    );
    assert_clean(&root);
}

#[test]
fn rejects_local_source_containing_the_install_transaction() {
    let temporary = tempfile::tempdir().unwrap();
    write(
        temporary.path().join("plugin.json"),
        r#"{"name":"source ancestor"}"#,
    );
    let root = temporary.path().join("library");
    assert!(matches!(
        Library::new(&root).install_directory("demo", temporary.path(), false),
        Err(InstallError::UnsafePath)
    ));
    assert_clean(&root);
}

#[test]
fn checkout_plugin_ancestor_symlink_is_rejected() {
    struct AncestorSymlinkCheckout;
    impl Checkout for AncestorSymlinkCheckout {
        fn checkout(&self, source: &GithubSource, destination: &Path) -> Result<(), InstallError> {
            let outside = destination.parent().unwrap().join("outside");
            let fixture_source = fixture(
                &outside,
                source
                    .relative_path()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap(),
                false,
            );
            fs::create_dir_all(destination).unwrap();
            symlink(
                fixture_source.parent().unwrap(),
                destination.join("plugins"),
            )
            .unwrap();
            Ok(())
        }
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("library");
    assert!(matches!(
        Library::new(&root).install_with_checkout(&request(None, false), &AncestorSymlinkCheckout),
        Err(InstallError::UnsafePath)
    ));
    assert!(!root.join("plugins/demo").exists());
    assert_clean(&root);
}

fn assert_clean(root: &Path) {
    assert!(!fs::read_dir(root.join("plugins")).unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".anchor-plugin-")
    }));
}

#[test]
fn operator_cli_installs_replaces_and_reports_only_public_outcomes() {
    use std::process::Command;

    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", true);
    let root = temporary.path().join("library");
    let command = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-library"));
        command
            .arg("--root")
            .arg(&root)
            .args(["install", "--directory"])
            .arg(&source)
            .args(["--id", "demo"]);
        command
    };
    let installed = command().output().unwrap();
    assert!(installed.status.success(), "{:?}", installed);
    assert!(installed.stderr.is_empty());
    let before: serde_json::Value = serde_json::from_slice(&installed.stdout).unwrap();
    assert_eq!(before["id"], "demo");
    assert_eq!(before["digest"].as_str().unwrap().len(), 64);
    let conflicting = command().output().unwrap();
    assert!(!conflicting.status.success());
    assert!(conflicting.stdout.is_empty());
    assert_eq!(
        String::from_utf8(conflicting.stderr).unwrap().trim(),
        InstallError::AlreadyExists.to_string()
    );
    write(
        source.join("resources/evidence.txt"),
        "operator replacement",
    );
    let updated = command().arg("--replace-existing").output().unwrap();
    assert!(updated.status.success());
    let after: serde_json::Value = serde_json::from_slice(&updated.stdout).unwrap();
    assert_ne!(before["digest"], after["digest"]);
    let invalid = Command::new(env!("CARGO_BIN_EXE_anchor-library"))
        .arg("--root")
        .arg(&root)
        .args([
            "install",
            "--source",
            "https://sentinel-secret@github.com/owner/repo/tree/main/plugin",
        ])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(
        !String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("sentinel-secret")
    );
    assert_clean(&root);
}

#[test]
fn operator_cli_requires_an_explicit_id_and_exactly_one_source() {
    use std::process::Command;

    let temporary = tempfile::tempdir().unwrap();
    for arguments in [
        vec!["install", "--directory", "fixture"],
        vec!["install"],
        vec![
            "install",
            "--directory",
            "fixture",
            "--id",
            "demo",
            "--source",
            "https://github.com/owner/repo/tree/main/plugin",
        ],
        vec!["authorize"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_anchor-library"))
            .arg("--root")
            .arg(temporary.path().join("library"))
            .args(arguments)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert!(!temporary.path().join("library").exists());
    }
}

#[test]
fn cross_process_lease_serializes_competing_installs_without_silent_replacement() {
    use rustix::fs::{FlockOperation, flock};
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", false);
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("plugins")).unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("plugins/.install.lock"))
        .unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();
    let spawn = || {
        Command::new(env!("CARGO_BIN_EXE_anchor-library"))
            .arg("--root")
            .arg(&root)
            .args(["install", "--directory"])
            .arg(&source)
            .args(["--id", "demo"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };
    let mut first = spawn();
    let mut second = spawn();
    thread::sleep(Duration::from_millis(100));
    assert!(first.try_wait().unwrap().is_none());
    assert!(second.try_wait().unwrap().is_none());
    assert!(!root.join("plugins/demo").exists());
    assert_clean(&root);
    drop(lock);
    let deadline = Instant::now() + Duration::from_secs(10);
    let wait = |child: &mut std::process::Child| loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("installer process did not release its lease");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let first_status = wait(&mut first);
    let second_status = wait(&mut second);
    assert_ne!(first_status.success(), second_status.success());
    assert_eq!(
        FilePluginCatalog::new(&root)
            .definition("demo")
            .unwrap()
            .name,
        "source"
    );
    assert_clean(&root);
}

#[test]
fn a_killed_lease_owner_does_not_require_manual_lock_file_removal() {
    use rustix::fs::{FlockOperation, flock};
    use std::{
        process::{Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    let temporary = tempfile::tempdir().unwrap();
    let source = fixture(temporary.path(), "source", false);
    let root = temporary.path().join("library");
    fs::create_dir_all(root.join("plugins")).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "lease_holder_child", "--nocapture"])
        .env("ANCHOR_LIBRARY_TEST_LEASE_ROOT", &root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("lease-ready").exists() {
        if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("fixture lease owner did not start");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("plugins/.install.lock"))
        .unwrap();
    assert!(flock(&lock, FlockOperation::NonBlockingLockExclusive).is_err());
    child.kill().unwrap();
    child.wait().unwrap();
    flock(&lock, FlockOperation::NonBlockingLockExclusive).unwrap();
    drop(lock);
    Library::new(&root)
        .install_directory("demo", source, false)
        .unwrap();
    assert_clean(&root);
}

#[test]
fn lease_holder_child() {
    use rustix::fs::{FlockOperation, flock};

    let Some(root) = std::env::var_os("ANCHOR_LIBRARY_TEST_LEASE_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("plugins/.install.lock"))
        .unwrap();
    flock(&lock, FlockOperation::LockExclusive).unwrap();
    write(root.join("lease-ready"), "ready");
    std::thread::sleep(std::time::Duration::from_secs(30));
}
