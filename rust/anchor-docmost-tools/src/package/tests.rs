use super::*;
use std::os::unix::fs::symlink;

const SOURCE: &[u8] = b"original executable fixture";

struct Fixture {
    temporary: tempfile::TempDir,
    parent: PathBuf,
    source: PathBuf,
    destination: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("package fixture");
        let parent = temporary.path().join("parent");
        fs::create_dir(&parent).expect("parent");
        let source = temporary.path().join("executable");
        fs::write(&source, SOURCE).expect("fixture executable");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755)).expect("executable mode");
        let destination = parent.join("native");
        Self {
            temporary,
            parent,
            source,
            destination,
        }
    }

    fn package(&self, checkpoint: impl FnMut(Checkpoint, &File)) -> Result<(), PackageError> {
        package_checked(&self.destination, &self.source, None, checkpoint)
    }

    fn parent_empty(&self) {
        assert_eq!(
            fs::read_dir(&self.parent).expect("parent entries").count(),
            0
        );
    }
}

#[test]
fn complete_fixture_copy_preserves_bytes_and_execute_mode() {
    let fixture = Fixture::new();
    fixture.package(|_, _| {}).expect("complete package");
    let binary = fixture.destination.join("bin/anchor-docmost-tools");
    assert_eq!(fs::read(&binary).expect("copied bytes"), SOURCE);
    assert_eq!(
        fs::metadata(binary).expect("copied metadata").mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_dir(&fixture.parent)
            .expect("parent entries")
            .count(),
        1
    );
}

#[test]
fn source_replacement_mutation_and_permission_changes_never_publish() {
    for late in [false, true] {
        for change in ["replace", "same_length", "truncate", "chmod", "symlink"] {
            let fixture = Fixture::new();
            let result = fixture.package(|phase, _| {
                if !matches!(
                    (late, phase),
                    (false, Checkpoint::Copied) | (true, Checkpoint::BeforeRename)
                ) {
                    return;
                }
                match change {
                    "replace" => {
                        let replacement = fixture.temporary.path().join("replacement");
                        fs::write(&replacement, SOURCE).expect("replacement bytes");
                        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755))
                            .expect("replacement mode");
                        fs::rename(replacement, &fixture.source).expect("replace executable inode");
                    }
                    "same_length" => fs::write(&fixture.source, vec![b'X'; SOURCE.len()])
                        .expect("mutate source bytes"),
                    "truncate" => fs::write(&fixture.source, b"short").expect("truncate source"),
                    "chmod" => {
                        fs::set_permissions(&fixture.source, fs::Permissions::from_mode(0o644))
                            .expect("change source mode")
                    }
                    "symlink" => {
                        let original = fixture.temporary.path().join("original");
                        fs::rename(&fixture.source, &original).expect("move executable");
                        symlink(original, &fixture.source).expect("replace source with symlink");
                    }
                    _ => unreachable!(),
                }
            });
            assert!(
                matches!(result, Err(PackageError::Identity)),
                "{change}, late={late}"
            );
            fixture.parent_empty();
        }
    }
}

#[test]
fn changed_copy_bytes_or_length_fail_integrity_check() {
    for truncate in [false, true] {
        let fixture = Fixture::new();
        let result = fixture.package(|phase, output| {
            if !matches!(phase, Checkpoint::Copied) {
                return;
            }
            if truncate {
                output.set_len(1).expect("truncate copy");
            } else {
                let mut copy = output.try_clone().expect("copy descriptor");
                copy.seek(SeekFrom::Start(0)).expect("seek copy");
                copy.write_all(&vec![b'X'; SOURCE.len()])
                    .expect("same-length corruption");
            }
        });
        assert!(matches!(result, Err(PackageError::Identity)));
        fixture.parent_empty();
    }
}

#[test]
fn parent_replacement_before_creation_or_rename_never_redirects_writes() {
    for phase in [
        Checkpoint::ParentOpened,
        Checkpoint::Copied,
        Checkpoint::BeforeRename,
        Checkpoint::Published,
    ] {
        for use_symlink in [false, true] {
            let fixture = Fixture::new();
            let moved = fixture.temporary.path().join("moved-parent");
            let replacement = fixture.temporary.path().join("replacement-parent");
            let result = fixture.package(|checkpoint, _| {
                if std::mem::discriminant(&checkpoint) != std::mem::discriminant(&phase) {
                    return;
                }
                fs::rename(&fixture.parent, &moved).expect("replace parent path");
                if use_symlink {
                    fs::create_dir(&replacement).expect("replacement parent");
                    symlink(&replacement, &fixture.parent).expect("parent symlink");
                } else {
                    fs::create_dir(&fixture.parent).expect("new parent inode");
                }
                fs::create_dir(&fixture.destination).expect("other writer destination");
                fs::write(fixture.destination.join("sentinel"), b"untouched")
                    .expect("other writer sentinel");
            });
            assert!(matches!(result, Err(PackageError::Path)));
            assert_eq!(
                fs::read_dir(&moved)
                    .expect("original parent cleanup")
                    .count(),
                0
            );
            assert_eq!(
                fs::read_dir(&fixture.parent)
                    .expect("replacement contents")
                    .count(),
                1
            );
            assert_eq!(
                fs::read_dir(&fixture.destination)
                    .expect("other writer contents")
                    .count(),
                1
            );
            assert_eq!(
                fs::read(fixture.destination.join("sentinel")).expect("sentinel"),
                b"untouched"
            );
        }
    }
}

#[test]
fn replaced_ancestor_and_symlinked_source_parent_fail_closed() {
    let fixture = Fixture::new();
    let nested = fixture.parent.join("nested");
    fs::create_dir(&nested).expect("nested parent");
    let destination = nested.join("native");
    let moved = fixture.temporary.path().join("moved-ancestor");
    let result = package_checked(&destination, &fixture.source, None, |phase, _| {
        if matches!(phase, Checkpoint::BeforeRename) {
            fs::rename(&fixture.parent, &moved).expect("move ancestor");
            fs::create_dir_all(&nested).expect("replacement ancestor tree");
        }
    });
    assert!(matches!(result, Err(PackageError::Path)));
    assert_eq!(
        fs::read_dir(moved.join("nested"))
            .expect("old nested cleanup")
            .count(),
        0
    );
    assert_eq!(
        fs::read_dir(&nested).expect("new nested untouched").count(),
        0
    );
    let alias = fixture.temporary.path().join("source-parent-alias");
    symlink(fixture.temporary.path(), &alias).expect("source parent alias");
    assert!(matches!(
        package_checked(&destination, &alias.join("executable"), None, |_, _| {}),
        Err(PackageError::Identity)
    ));
}

#[test]
fn late_destination_collision_preserves_other_writer_and_cleans_staging() {
    for use_symlink in [false, true] {
        let fixture = Fixture::new();
        let other = fixture.temporary.path().join("other");
        fs::create_dir(&other).expect("other destination");
        fs::write(other.join("sentinel"), b"other writer").expect("other writer bytes");
        let result = fixture.package(|phase, _| {
            if matches!(phase, Checkpoint::BeforeRename) {
                if use_symlink {
                    symlink(&other, &fixture.destination).expect("late destination symlink");
                } else {
                    fs::create_dir(&fixture.destination).expect("late destination");
                    fs::write(fixture.destination.join("sentinel"), b"other writer")
                        .expect("late sentinel");
                }
            }
        });
        assert!(matches!(result, Err(PackageError::Exists)));
        assert_eq!(
            fs::read(fixture.destination.join("sentinel")).expect("sentinel unchanged"),
            b"other writer"
        );
        assert_eq!(
            fs::read_dir(&fixture.parent)
                .expect("staging cleanup")
                .count(),
            1
        );
        assert_eq!(
            fs::read_dir(&fixture.destination)
                .expect("destination contents")
                .count(),
            1
        );
    }
}

#[test]
fn nonregular_nonexecutables_and_running_identity_mismatch_are_rejected() {
    for source_kind in [
        "directory",
        "fifo",
        "symlink",
        "empty",
        "not-executable",
        "wrong-running-inode",
    ] {
        let fixture = Fixture::new();
        let expected = FileIdentity::from(&fs::metadata(&fixture.source).expect("source metadata"));
        let running = if source_kind == "wrong-running-inode" {
            let replacement = fixture.temporary.path().join("replacement");
            fs::write(&replacement, SOURCE).expect("replacement");
            fs::set_permissions(&replacement, fs::Permissions::from_mode(0o755))
                .expect("replacement mode");
            fs::rename(&replacement, &fixture.source).expect("replace running path");
            Some(expected)
        } else {
            fs::remove_file(&fixture.source).expect("remove fixture source");
            match source_kind {
                "directory" => fs::create_dir(&fixture.source).expect("directory source"),
                "fifo" => rustix::fs::mkfifoat(
                    rustix::fs::CWD,
                    &fixture.source,
                    Mode::from_raw_mode(0o755),
                )
                .expect("FIFO source"),
                "symlink" => symlink(fixture.temporary.path().join("missing"), &fixture.source)
                    .expect("symlink source"),
                "empty" | "not-executable" => {
                    fs::write(
                        &fixture.source,
                        if source_kind == "empty" { b"" } else { SOURCE },
                    )
                    .expect("source bytes");
                    fs::set_permissions(
                        &fixture.source,
                        fs::Permissions::from_mode(if source_kind == "empty" {
                            0o755
                        } else {
                            0o644
                        }),
                    )
                    .expect("source mode");
                }
                _ => unreachable!(),
            }
            None
        };
        assert!(
            matches!(
                package_checked(&fixture.destination, &fixture.source, running, |_, _| {}),
                Err(PackageError::Identity)
            ),
            "{source_kind}"
        );
        fixture.parent_empty();
    }
}
