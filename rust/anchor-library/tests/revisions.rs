use std::{
    collections::BTreeSet,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};

use anchor_graph_host::{FilePluginCatalog, all_system_plugins, plugin_view_path};
use anchor_library::{
    DependencyViewIdentity, InstallError, Library, PluginPublicationFence, PluginRevision,
    RevisionError, reclaim,
};
use serde_json::json;
use sha2::{Digest, Sha256};

struct Fixture(tempfile::TempDir);

#[test]
fn durable_publication_fences_block_only_default_execution_and_clear_by_exact_intent() {
    let fixture = Fixture::new();
    let source = fixture.source("base-fenced", None);
    let digest = fixture.install(&source, false);
    fixture.retain(&source, &digest, None);
    let library = fixture.library();
    let fence = PluginPublicationFence {
        execute_digest: "a".repeat(64),
        intent_digest: "b".repeat(64),
    };
    let other = PluginPublicationFence {
        execute_digest: "a".repeat(64),
        intent_digest: "c".repeat(64),
    };
    let lease = library.install_lease().unwrap();
    library
        .set_publication_fence_locked("demo", &fence, &lease)
        .unwrap();
    library
        .set_publication_fence_locked("demo", &fence, &lease)
        .unwrap();
    assert_eq!(
        library.publication_fence_locked("demo", &lease).unwrap(),
        Some(fence.clone())
    );
    assert_eq!(
        library
            .inspect_binding_locked("demo", None, &lease)
            .unwrap()
            .digest,
        digest
    );
    assert!(matches!(
        library.set_publication_fence_locked("demo", &other, &lease),
        Err(RevisionError::Changed)
    ));
    assert!(matches!(
        library.clear_publication_fence_locked("demo", &other, &lease),
        Err(RevisionError::Changed)
    ));
    drop(lease);
    // A newly opened Library sees the durable marker after a Host restart.
    assert!(matches!(
        fixture.library().resolve_source("demo", None),
        Err(RevisionError::DefaultUnavailable)
    ));
    let pinned = library.resolve_source("demo", Some(&digest)).unwrap();
    assert_eq!(pinned.binding().digest, digest);
    let lease = library.install_lease().unwrap();
    library
        .clear_publication_fence_locked("demo", &fence, &lease)
        .unwrap();
    library
        .clear_publication_fence_locked("demo", &fence, &lease)
        .unwrap();
    assert_eq!(
        library.publication_fence_locked("demo", &lease).unwrap(),
        None
    );
    drop(lease);
    assert_eq!(
        library
            .resolve_source("demo", None)
            .unwrap()
            .binding()
            .digest,
        digest
    );
}

#[test]
fn unreadable_publication_markers_remain_fenced_and_cannot_be_cleared_as_another_intent() {
    let fixture = Fixture::new();
    let source = fixture.source("base-malformed-fence", None);
    fixture.install(&source, false);
    let marker = fixture
        .root()
        .join("dependencies/.publication-fences/demo.json");
    write(&marker, "not an intent reference");
    assert!(matches!(
        fixture.library().resolve_source("demo", None),
        Err(RevisionError::DefaultUnavailable)
    ));
    let library = fixture.library();
    let lease = library.install_lease().unwrap();
    let fence = PluginPublicationFence {
        execute_digest: "a".repeat(64),
        intent_digest: "b".repeat(64),
    };
    assert!(matches!(
        library.publication_fence_locked("demo", &lease),
        Err(RevisionError::Changed)
    ));
    assert!(
        library
            .clear_publication_fence_locked("demo", &fence, &lease)
            .is_err()
    );
    assert!(marker.exists());
    let another = Fixture::new();
    let other_lease = another.library().install_lease().unwrap();
    assert!(matches!(
        library.set_publication_fence_locked("demo", &fence, &other_lease),
        Err(RevisionError::Install(InstallError::UnsafePath))
    ));
}

impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn root(&self) -> PathBuf {
        self.0.path().join("library")
    }
    fn library(&self) -> Library {
        Library::new(self.root())
    }
    fn source(&self, label: &str, requirement: Option<&str>) -> PathBuf {
        let directory = self.0.path().join("sources").join(label);
        let mut manifest = json!({
            "name":"fixture", "description":label, "mutable":["data/", "cache.txt"],
            "skills":"skills/",
            "mcpServers":{"probe":{"command":"sh", "args":["${PLUGIN_ROOT}/server.sh", label]}}
        });
        if let Some(requirement) = requirement {
            manifest["dependencies"] = json!({"python":[requirement]});
            manifest["mcpServers"]["probe"]["args"] =
                json!(["${ANCHOR_PLUGIN_VIEW}/bin/python", label]);
        }
        write(&directory.join("plugin.json"), manifest.to_string());
        write(
            &directory.join("skills/check/SKILL.md"),
            format!("# {label}\n"),
        );
        write(&directory.join("server.sh"), format!("echo {label}\n"));
        // Staging must prune data without looking at its type or reading it.
        fs::create_dir_all(directory.join("data")).unwrap();
        symlink("/unavailable-business-data", directory.join("data/private")).unwrap();
        write(&directory.join("cache.txt"), "source business data");
        directory
    }

    fn install(&self, source: &Path, replace: bool) -> String {
        self.library()
            .install_directory("demo", source, replace)
            .unwrap()
            .digest
    }

    fn solved(&self, source: &Path, version: &str) -> DependencyViewIdentity {
        let temporary = tempfile::tempdir().unwrap();
        let (catalog, _) = self
            .library()
            .stage_directory("demo", source, temporary.path())
            .unwrap();
        let declaration = FilePluginCatalog::new(catalog)
            .definition("demo")
            .unwrap()
            .dependencies
            .declaration_digest();
        let prefix = self.0.path().join("interpreter");
        let interpreter = prefix.join("bin/python3.12");
        if !interpreter.exists() {
            fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
            fs::copy("/bin/true", &interpreter).unwrap();
        }
        let lock = format!("idna=={version} --hash=sha256:{}\n", "a".repeat(64));
        let identity = DependencyViewIdentity {
            declaration_digest: declaration,
            lock_digest: format!("{:x}", Sha256::digest(lock.as_bytes())),
            lock,
            interpreter: "3.12.13".into(),
            interpreter_path: interpreter.clone(),
            interpreter_prefix: prefix.clone(),
            interpreter_digest: format!("{:x}", Sha256::digest(fs::read(interpreter).unwrap())),
        };
        let view = identity.view_path(&self.root(), "demo");
        write(
            &view.join("pyvenv.cfg"),
            format!("home = {}\n", prefix.join("bin").display()),
        );
        fs::create_dir_all(view.join("bin")).unwrap();
        symlink(&identity.interpreter_path, view.join("bin/python")).unwrap();
        write(
            &view.join("bin/idna"),
            format!("#!{}/bin/python\n", view.display()),
        );
        let package = stored(&self.root(), version);
        let site = view.join("lib/python3.12/site-packages");
        fs::create_dir_all(&site).unwrap();
        symlink(package.join("idna"), site.join("idna")).unwrap();
        identity
    }

    fn retain(
        &self,
        source: &Path,
        digest: &str,
        dependencies: Option<DependencyViewIdentity>,
    ) -> PluginRevision {
        let library = self.library();
        let lease = library.install_lease().unwrap();
        library
            .retain_revision_locked("demo", source, digest, dependencies, &lease)
            .unwrap()
    }

    fn revision_path(&self, revision: &PluginRevision) -> PathBuf {
        self.root()
            .join("revisions")
            .join(&revision.id)
            .join(&revision.digest)
    }

    fn lease_path(&self, revision: &PluginRevision) -> PathBuf {
        self.root()
            .join("dependencies/.revision-leases")
            .join(&revision.id)
            .join(format!("{}.lock", revision.digest))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        writable_directories(self.0.path());
    }
}

fn writable_directories(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        for entry in fs::read_dir(path).unwrap() {
            writable_directories(&entry.unwrap().path());
        }
    }
}

fn write(path: &Path, bytes: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn stored(root: &Path, version: &str) -> PathBuf {
    let package = root.join("dependencies/store/idna").join(version);
    write(&package.join("idna/__init__.py"), version);
    package
}

#[test]
fn pin_keeps_one_code_source_and_original_data_without_blocking_default_upgrade() {
    let fixture = Fixture::new();
    let base = fixture.source("base", None);
    let manifest_path = base.join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["mcpServers"]["probe"]["env"] = json!({"PRIVATE_VALUE":"not-for-debug"});
    write(&manifest_path, manifest.to_string());
    let digest = fixture.install(&base, false);
    write(&fixture.root().join("plugins/demo/data/counter"), "42");
    write(&fixture.root().join("plugins/demo/cache.txt"), "today");
    let revision = fixture.retain(&fixture.root().join("plugins/demo"), &digest, None);
    assert_eq!(fixture.retain(&base, &digest, None), revision);
    let library = fixture.library();
    let pinned = library.resolve_source("demo", Some(&digest)).unwrap();
    assert!(!format!("{pinned:?}").contains("not-for-debug"));
    assert_eq!(pinned.binding().digest, digest);
    assert_eq!(pinned.mutable_root(), fixture.root().join("plugins/demo"));
    assert!(
        pinned
            .code_root()
            .starts_with(fixture.revision_path(&revision))
    );
    assert_eq!(
        fs::read_dir(pinned.code_root().join("data"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(fs::read(pinned.code_root().join("cache.txt")).unwrap(), b"");
    assert_eq!(
        fs::read_to_string(pinned.code_root().join("skills/check/SKILL.md")).unwrap(),
        "# base\n"
    );
    assert!(
        pinned
            .mutable_paths()
            .unwrap()
            .iter()
            .all(|path| path.starts_with(pinned.mutable_root()))
    );
    let candidate = fixture.source("candidate", None);
    let lease = library
        .try_install_lease()
        .expect("immutable invocation does not hold the global lease");
    let candidate_digest = library
        .install_directory_locked("demo", &candidate, true, &lease)
        .unwrap()
        .digest;
    assert_eq!(
        library
            .inspect_binding_locked("demo", None, &lease)
            .unwrap()
            .digest,
        candidate_digest
    );
    assert_eq!(
        library
            .inspect_binding_locked("demo", Some(&digest), &lease)
            .unwrap(),
        pinned.binding()
    );
    assert!(matches!(
        library.release_revision_locked(&revision, &BTreeSet::new(), &lease),
        Err(RevisionError::InUse)
    ));
    drop(lease);
    let current = library.resolve_source("demo", None).unwrap();
    assert_eq!(current.binding().digest, candidate_digest);
    assert_ne!(current.binding().digest, pinned.binding().digest);
    assert!(matches!(
        library.try_install_lease(),
        Err(InstallError::CatalogBusy)
    ));
    let servers = pinned.resolve_mcp_servers_with(&|_| None).unwrap();
    assert_eq!(servers[0].config["args"][1], "base");
    assert!(
        servers[0].config["args"][0]
            .as_str()
            .unwrap()
            .starts_with(pinned.code_root().to_str().unwrap())
    );
    assert_eq!(
        fs::read_to_string(pinned.mutable_root().join("data/counter")).unwrap(),
        "42"
    );
    assert_eq!(
        fs::read_to_string(pinned.mutable_root().join("cache.txt")).unwrap(),
        "today"
    );
    drop(current);
    drop(pinned);
    let lease = library.install_lease().unwrap();
    assert!(matches!(
        library.release_revision_locked(&revision, &BTreeSet::from([revision.clone()]), &lease),
        Err(RevisionError::InUse)
    ));
    assert!(
        library
            .release_revision_locked(&revision, &BTreeSet::new(), &lease)
            .unwrap()
    );
    assert!(
        !library
            .release_revision_locked(&revision, &BTreeSet::new(), &lease)
            .unwrap()
    );
    assert!(!fixture.revision_path(&revision).exists());
    assert!(fixture.root().join("plugins/demo/data/counter").exists());
}

#[test]
fn solved_views_survive_upgrade_restart_and_gc_until_explicit_revision_release() {
    let fixture = Fixture::new();
    let base = fixture.source("base", Some("idna>=3"));
    let digest = fixture.install(&base, false);
    let identity = fixture.solved(&base, "3.6");
    let revision = fixture.retain(&base, &digest, Some(identity.clone()));
    let original_view = identity.view_path(&fixture.root(), "demo");
    let package = fixture.root().join("dependencies/store/idna/3.6");
    let old_formal = plugin_view_path(&fixture.root(), "demo", &identity.declaration_digest);
    write(&old_formal.join("pyvenv.cfg"), "old default\n");
    fs::create_dir_all(old_formal.join("lib/python3.12/site-packages")).unwrap();
    symlink(
        package.join("idna"),
        old_formal.join("lib/python3.12/site-packages/idna"),
    )
    .unwrap();
    let garbage = stored(&fixture.root(), "3.7");
    let candidate = fixture.source("candidate", Some("idna>=4"));
    let candidate_digest = fixture.install(&candidate, true);
    assert!(package.is_dir() && original_view.is_dir());
    assert!(!garbage.exists() && !old_formal.exists());
    let next_identity = fixture.solved(&candidate, "4.0");
    let next_revision = fixture.retain(&candidate, &candidate_digest, Some(next_identity.clone()));
    let restarted = Library::new(fixture.root());
    let source = restarted.resolve_source("demo", Some(&digest)).unwrap();
    assert_eq!(source.dependency_identity(), Some(&identity));
    assert_eq!(source.dependency_view(), Some(original_view.as_path()));
    let servers = source.resolve_mcp_servers_with(&|_| None).unwrap();
    assert_eq!(
        servers[0].config["args"][0],
        "${ANCHOR_PLUGIN_VIEW}/bin/python"
    );
    let report = reclaim(&fixture.root(), true).unwrap();
    assert!(report.deferred.is_none(), "{report:?}");
    assert!(package.is_dir());
    drop(source);
    let lease = restarted.install_lease().unwrap();
    restarted
        .release_revision_locked(&revision, &BTreeSet::from([next_revision]), &lease)
        .unwrap();
    drop(lease);
    let report = reclaim(&fixture.root(), true).unwrap();
    assert!(report.deferred.is_none(), "{report:?}");
    assert!(!package.exists() && !original_view.exists());
    assert!(next_identity.view_path(&fixture.root(), "demo").is_dir());
}

#[test]
fn actual_solve_and_interpreter_key_keeps_its_existing_wire_identity() {
    assert_eq!(
        DependencyViewIdentity::key_for(
            &"a".repeat(64),
            Path::new("/usr/bin/python3.12"),
            &"b".repeat(64)
        ),
        "bd11f19f33173186664112f4030fe15a9be5ebbc1bca3f82f53e918857084f48"
    );
    let fixture = Fixture::new();
    let base = fixture.source("base", Some("idna>=3"));
    let digest = fixture.install(&base, false);
    let first = fixture.solved(&base, "3.6");
    let second = fixture.solved(&base, "3.7");
    assert_eq!(first.declaration_digest, second.declaration_digest);
    assert_ne!(first.key(), second.key());
    assert!(!format!("{first:?}").contains("--hash"));
    fixture.retain(&base, &digest, Some(first.clone()));
    let library = fixture.library();
    let lease = library.install_lease().unwrap();
    assert!(matches!(
        library.retain_revision_locked("demo", &base, &digest, Some(second), &lease),
        Err(RevisionError::Changed)
    ));
    drop(lease);
    assert_eq!(
        library
            .resolve_source("demo", Some(&digest))
            .unwrap()
            .dependency_identity(),
        Some(&first)
    );
}

#[test]
fn a_revision_requires_exact_code_and_a_verified_actual_dependency_view() {
    for cause in [
        "code",
        "missing_identity",
        "declaration",
        "lock",
        "interpreter",
        "missing_view",
        "declaration_view_only",
    ] {
        let fixture = Fixture::new();
        let base = fixture.source("base", Some("idna>=3"));
        let digest = fixture.install(&base, false);
        let mut identity = fixture.solved(&base, "3.6");
        let mut expected = digest;
        match cause {
            "code" => expected = "f".repeat(64),
            "declaration" => identity.declaration_digest = "f".repeat(64),
            "lock" => identity.lock.push_str("changed"),
            "interpreter" => identity.interpreter_digest = "f".repeat(64),
            "missing_view" => {
                fs::remove_dir_all(identity.view_path(&fixture.root(), "demo")).unwrap()
            }
            "declaration_view_only" => fs::rename(
                identity.view_path(&fixture.root(), "demo"),
                plugin_view_path(&fixture.root(), "demo", &identity.declaration_digest),
            )
            .unwrap(),
            "missing_identity" => {}
            _ => unreachable!(),
        }
        let library = fixture.library();
        let lease = library.install_lease().unwrap();
        let dependencies = (cause != "missing_identity").then_some(identity);
        assert!(
            library
                .retain_revision_locked("demo", &base, &expected, dependencies, &lease)
                .is_err(),
            "{cause}"
        );
        assert!(
            !fixture
                .root()
                .join("revisions/demo")
                .join(expected)
                .exists(),
            "{cause}"
        );
    }
}

#[test]
fn old_code_cannot_restore_currently_removed_authority_or_change_system_plugins() {
    let fixture = Fixture::new();
    let base = fixture.source("base", None);
    let digest = fixture.install(&base, false);
    fixture.retain(&base, &digest, None);
    let candidate = fixture.source("revoked", None);
    let path = candidate.join("plugin.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["mcpServers"] = json!({});
    fs::write(path, manifest.to_string()).unwrap();
    fixture.install(&candidate, true);
    assert!(matches!(
        fixture.library().resolve_source("demo", Some(&digest)),
        Err(RevisionError::AuthorizationChanged)
    ));
    let library = fixture.library();
    let lease = library.install_lease().unwrap();
    assert!(matches!(
        library.inspect_binding_locked("demo", Some(&digest), &lease),
        Err(RevisionError::AuthorizationChanged)
    ));
    for system in all_system_plugins() {
        assert!(matches!(
            library.retain_revision_locked(system.id(), &base, &digest, None, &lease),
            Err(RevisionError::Install(InstallError::SystemPluginProtected(
                _
            )))
        ));
    }
}

#[test]
fn missing_assets_and_corrupt_references_never_authorize_dependency_reclamation() {
    for cause in [
        "binding",
        "missing_revision",
        "missing_revisions",
        "missing_code",
        "missing_lease",
        "bad_lease",
        "missing_view",
        "view_changed",
        "view_redirected",
    ] {
        let fixture = Fixture::new();
        let base = fixture.source("base", Some("idna>=3"));
        let digest = fixture.install(&base, false);
        let identity = fixture.solved(&base, "3.6");
        let revision = fixture.retain(&base, &digest, Some(identity.clone()));
        let directory = fixture.revision_path(&revision);
        match cause {
            "binding" => {
                let binding = directory.join("binding.json");
                fs::set_permissions(&binding, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(binding, "broken").unwrap();
            }
            "missing_revision" | "missing_revisions" | "missing_code" => {
                let target = match cause {
                    "missing_revision" => directory.clone(),
                    "missing_revisions" => fixture.root().join("revisions"),
                    _ => directory.join("plugins"),
                };
                fs::set_permissions(target.parent().unwrap(), fs::Permissions::from_mode(0o700))
                    .unwrap();
                writable_directories(&target);
                fs::remove_dir_all(target).unwrap();
            }
            "missing_lease" => fs::remove_file(fixture.lease_path(&revision)).unwrap(),
            "bad_lease" => fs::write(fixture.lease_path(&revision), "broken").unwrap(),
            "missing_view" => {
                fs::remove_dir_all(identity.view_path(&fixture.root(), "demo")).unwrap()
            }
            "view_changed" => write(
                &identity.view_path(&fixture.root(), "demo").join("bin/idna"),
                "changed",
            ),
            "view_redirected" => {
                let package = stored(&fixture.root(), "3.7");
                let link = identity
                    .view_path(&fixture.root(), "demo")
                    .join("lib/python3.12/site-packages/idna");
                fs::remove_file(&link).unwrap();
                symlink(package.join("idna"), &link).unwrap();
            }
            _ => unreachable!(),
        }
        let garbage = stored(&fixture.root(), "garbage");
        let report = reclaim(&fixture.root(), true).unwrap();
        assert!(report.deferred.is_some(), "{cause}: {report:?}");
        assert!(garbage.exists(), "{cause}");
        assert!(
            fixture.root().join("dependencies/store/idna/3.6").exists(),
            "{cause}"
        );
        assert!(
            fixture
                .library()
                .resolve_source("demo", Some(&digest))
                .is_err(),
            "{cause}"
        );
        let library = fixture.library();
        let lease = library.install_lease().unwrap();
        assert!(
            library
                .inspect_binding_locked("demo", Some(&digest), &lease)
                .is_err(),
            "{cause}"
        );
    }
}

#[test]
fn changed_code_view_links_or_mutable_ownership_are_not_usable_sources() {
    for cause in [
        "code",
        "view",
        "mutable",
        "mount_directory",
        "mount_file",
        "revision_symlink",
        "binding_symlink",
        "code_hardlink",
        "lease_symlink",
        "lease_hardlink",
    ] {
        let fixture = Fixture::new();
        let base = fixture.source("base", Some("idna>=3"));
        let digest = fixture.install(&base, false);
        let identity = fixture.solved(&base, "3.6");
        let revision = fixture.retain(&base, &digest, Some(identity.clone()));
        let directory = fixture.revision_path(&revision);
        let code = directory.join("plugins/demo/server.sh");
        let outside = fixture.0.path().join("outside");
        match cause {
            "code" => {
                fs::set_permissions(&code, fs::Permissions::from_mode(0o644)).unwrap();
                fs::write(&code, "changed").unwrap();
                fs::set_permissions(&code, fs::Permissions::from_mode(0o444)).unwrap();
            }
            "view" => write(
                &identity.view_path(&fixture.root(), "demo").join("bin/idna"),
                "changed",
            ),
            "mutable" => {
                let data = fixture.root().join("plugins/demo/data");
                fs::remove_dir(data.clone()).unwrap();
                fs::create_dir_all(&outside).unwrap();
                symlink(&outside, data).unwrap();
            }
            "mount_directory" => {
                let data = directory.join("plugins/demo/data");
                fs::set_permissions(&data, fs::Permissions::from_mode(0o755)).unwrap();
                write(&data.join("not-an-empty-mount"), "unexpected data");
                fs::set_permissions(
                    data.join("not-an-empty-mount"),
                    fs::Permissions::from_mode(0o444),
                )
                .unwrap();
                fs::set_permissions(&data, fs::Permissions::from_mode(0o555)).unwrap();
            }
            "mount_file" => {
                let data = directory.join("plugins/demo/cache.txt");
                fs::set_permissions(&data, fs::Permissions::from_mode(0o644)).unwrap();
                fs::write(&data, "unexpected data").unwrap();
                fs::set_permissions(&data, fs::Permissions::from_mode(0o444)).unwrap();
            }
            "revision_symlink" => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
                fs::rename(&directory, &outside).unwrap();
                fs::set_permissions(&outside, fs::Permissions::from_mode(0o555)).unwrap();
                symlink(&outside, &directory).unwrap();
            }
            "binding_symlink" => {
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
                fs::rename(directory.join("binding.json"), &outside).unwrap();
                symlink(&outside, directory.join("binding.json")).unwrap();
                fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
            }
            "code_hardlink" => {
                fs::copy(&code, &outside).unwrap();
                fs::set_permissions(code.parent().unwrap(), fs::Permissions::from_mode(0o755))
                    .unwrap();
                fs::remove_file(&code).unwrap();
                fs::hard_link(&outside, &code).unwrap();
                fs::set_permissions(code.parent().unwrap(), fs::Permissions::from_mode(0o555))
                    .unwrap();
            }
            "lease_symlink" | "lease_hardlink" => {
                fs::write(&outside, "").unwrap();
                fs::remove_file(fixture.lease_path(&revision)).unwrap();
                if cause == "lease_symlink" {
                    symlink(&outside, fixture.lease_path(&revision)).unwrap();
                } else {
                    fs::hard_link(&outside, fixture.lease_path(&revision)).unwrap();
                }
            }
            _ => unreachable!(),
        }
        assert!(
            fixture
                .library()
                .resolve_source("demo", Some(&digest))
                .is_err(),
            "{cause}"
        );
        let library = fixture.library();
        let lease = library.install_lease().unwrap();
        assert!(
            library
                .inspect_binding_locked("demo", Some(&digest), &lease)
                .is_err(),
            "{cause}"
        );
    }
}

#[test]
fn a_lease_from_another_library_cannot_inspect_publish_or_release_revisions() {
    let fixture = Fixture::new();
    let source = fixture.source("base", None);
    let digest = fixture.install(&source, false);
    let revision = fixture.retain(&source, &digest, None);
    let other = Library::new(fixture.0.path().join("other-library"));
    let lease = other.install_lease().unwrap();
    for requested in [None, Some(digest.as_str())] {
        assert!(matches!(
            fixture
                .library()
                .inspect_binding_locked("demo", requested, &lease),
            Err(RevisionError::Install(InstallError::UnsafePath))
        ));
    }
    assert!(
        fixture
            .library()
            .retain_revision_locked("demo", &source, &digest, None, &lease)
            .is_err()
    );
    assert!(
        fixture
            .library()
            .release_revision_locked(&revision, &BTreeSet::new(), &lease)
            .is_err()
    );
    assert!(matches!(
        fixture
            .library()
            .install_directory_locked("demo", &source, true, &lease),
        Err(InstallError::UnsafePath)
    ));
    assert!(fixture.revision_path(&revision).exists());
}

#[test]
fn explicit_cleanup_can_settle_a_crashed_unpublished_reservation() {
    let fixture = Fixture::new();
    let source = fixture.source("base", None);
    let digest = fixture.install(&source, false);
    let revision = fixture.retain(&source, &digest, None);
    let directory = fixture.revision_path(&revision);
    writable_directories(&directory);
    fs::remove_dir_all(directory).unwrap();
    assert!(reclaim(&fixture.root(), true).unwrap().deferred.is_some());
    let library = fixture.library();
    let lease = library.install_lease().unwrap();
    assert!(
        !library
            .release_revision_locked(&revision, &BTreeSet::new(), &lease)
            .unwrap()
    );
    drop(lease);
    assert!(!fixture.lease_path(&revision).exists());
    assert!(reclaim(&fixture.root(), true).unwrap().deferred.is_none());
}

#[test]
fn exact_retry_finishes_only_the_unsealed_publication_boundary() {
    let fixture = Fixture::new();
    let source = fixture.source("base", None);
    let digest = fixture.install(&source, false);
    let revision = fixture.retain(&source, &digest, None);
    let directory = fixture.revision_path(&revision);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        fixture
            .library()
            .resolve_source("demo", Some(&digest))
            .is_err()
    );
    assert_eq!(fixture.retain(&source, &digest, None), revision);
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o555
    );
    assert!(
        fixture
            .library()
            .resolve_source("demo", Some(&digest))
            .is_ok()
    );
}

#[test]
fn unreferenced_code_can_be_released_after_its_old_interpreter_is_unavailable() {
    let fixture = Fixture::new();
    let source = fixture.source("base", Some("idna>=3"));
    let digest = fixture.install(&source, false);
    let identity = fixture.solved(&source, "3.6");
    let revision = fixture.retain(&source, &digest, Some(identity.clone()));
    fs::remove_file(identity.interpreter_path).unwrap();
    assert!(
        fixture
            .library()
            .resolve_source("demo", Some(&digest))
            .is_err()
    );
    let library = fixture.library();
    let lease = library.install_lease().unwrap();
    assert!(
        library
            .release_revision_locked(&revision, &BTreeSet::new(), &lease)
            .unwrap()
    );
    assert!(!fixture.revision_path(&revision).exists());
}
