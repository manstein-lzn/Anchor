use crate::host_fs::open_directory;
use anchor_file_lease::{FileLease, FlockOperation};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat, unlinkat};
use rustix::io::Errno;
use std::{
    ffi::CStr,
    fs::{File, Permissions},
    io::{self, Write},
    os::fd::AsRawFd,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

const TEMP_PREFIX: &str = ".anchor-write-";
const TEMP_RANDOM_LEN: usize = 6;
const DIRECTORY_FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

struct PendingWrite {
    temporary: tempfile::NamedTempFile,
    target: PathBuf,
    directory: FileLease,
}

impl PendingWrite {
    fn new(path: &Path) -> io::Result<Self> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let directory = FileLease::acquire(File::open(parent)?, FlockOperation::LockShared)?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::other("durable target needs a file name"))?;
        Self::in_directory(directory, name)
    }

    fn in_directory(directory: FileLease, name: &std::ffi::OsStr) -> io::Result<Self> {
        let parent = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
        let target = parent.join(name);
        let permissions = match std::fs::metadata(&target) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let temporary = tempfile::Builder::new()
            .prefix(TEMP_PREFIX)
            .rand_bytes(TEMP_RANDOM_LEN)
            .permissions(Permissions::from_mode(0o666))
            .tempfile_in(&parent)?;
        if let Some(permissions) = permissions {
            temporary.as_file().set_permissions(permissions)?;
        }
        Ok(Self {
            temporary,
            target,
            directory,
        })
    }

    fn publish(mut self, bytes: &[u8]) -> io::Result<()> {
        self.temporary.write_all(bytes)?;
        self.temporary.as_file().sync_all()?;
        self.temporary
            .persist(&self.target)
            .map_err(|error| error.error)?;
        self.directory.sync_all()
    }
}

pub(crate) fn write_durable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    PendingWrite::new(path)?.publish(bytes)
}

#[derive(Debug, Default, PartialEq, Eq)]
struct CleanupReport {
    removed: usize,
    busy: usize,
    failed: usize,
}

fn temporary_name(name: &[u8]) -> bool {
    name.strip_prefix(TEMP_PREFIX.as_bytes())
        .is_some_and(|suffix| {
            suffix.len() == TEMP_RANDOM_LEN && suffix.iter().all(u8::is_ascii_alphanumeric)
        })
}

fn sweep(directory: File, report: &mut CleanupReport) -> io::Result<()> {
    let directory = match FileLease::acquire(directory, FlockOperation::NonBlockingLockExclusive) {
        Ok(directory) => directory,
        Err(Errno::WOULDBLOCK) => {
            report.busy += 1;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let before = report.removed;
    for entry in Dir::read_from(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        let result = (|| -> io::Result<()> {
            let metadata = match statat(&directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(metadata) => metadata,
                Err(Errno::NOENT) => return Ok(()),
                Err(error) => return Err(error.into()),
            };
            match FileType::from_raw_mode(metadata.st_mode) {
                FileType::RegularFile if temporary_name(name.to_bytes()) => {
                    match unlinkat(&directory, name, AtFlags::empty()) {
                        Ok(()) => report.removed += 1,
                        Err(Errno::NOENT) => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                _ => {}
            }
            Ok(())
        })();
        if result.is_err() {
            report.failed += 1;
        }
    }
    if report.removed != before {
        directory.sync_all()?;
    }
    Ok(())
}

enum Directory {
    Named(&'static str),
    Hash,
    Conversation,
}

impl Directory {
    fn matches(&self, name: &CStr) -> bool {
        let name = name.to_bytes();
        match self {
            Self::Named(expected) => name == expected.as_bytes(),
            Self::Hash => hash_name(name),
            Self::Conversation => name.strip_prefix(b"gc1-").is_some_and(hash_name),
        }
    }
}

fn hash_name(name: &[u8]) -> bool {
    name.len() == 64 && name.iter().all(u8::is_ascii_hexdigit)
}

fn visit(directory: File, path: &[Directory], report: &mut CleanupReport) -> io::Result<()> {
    let Some((selector, remaining)) = path.split_first() else {
        return sweep(directory, report);
    };
    for entry in Dir::read_from(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if !selector.matches(name) {
            continue;
        }
        let child = match openat(&directory, name, DIRECTORY_FLAGS, Mode::empty()) {
            Ok(child) => child,
            Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => continue,
            Err(_) => {
                report.failed += 1;
                continue;
            }
        };
        if visit(child.into(), remaining, report).is_err() {
            report.failed += 1;
        }
    }
    Ok(())
}

fn cleanup(root: &Path, path: &[Directory], report: &mut CleanupReport) {
    let result = match open_directory(root) {
        Ok(directory) => visit(directory, path, report),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => Err(error),
    };
    if result.is_err() {
        report.failed += 1;
    }
}

pub(crate) fn cleanup_at_startup(
    data_root: &Path,
    workspace_root: &Path,
    input_workspaces: impl FnOnce() -> Option<Vec<PathBuf>>,
) {
    let report = cleanup_host_directories(data_root, workspace_root, input_workspaces);
    if report != CleanupReport::default() {
        eprintln!(
            "anchor-runner-host: durable write cleanup: removed={}, busy={}, failed={}",
            report.removed, report.busy, report.failed
        );
    }
}

fn cleanup_host_directories(
    data_root: &Path,
    workspace_root: &Path,
    input_workspaces: impl FnOnce() -> Option<Vec<PathBuf>>,
) -> CleanupReport {
    use Directory::{Conversation, Hash, Named};

    let mut report = CleanupReport::default();
    for name in ["facts", "goose-acp", "goose-acp-spike"] {
        cleanup(data_root, &[Named(name)], &mut report);
    }
    for path in [
        &[Named(".goose-process"), Hash, Named("config")][..],
        &[
            Named(".goose-process"),
            Named("conversations"),
            Conversation,
            Named("process"),
            Named("config"),
        ],
    ] {
        cleanup(workspace_root, path, &mut report);
    }
    match input_workspaces() {
        Some(paths) => {
            for path in paths {
                cleanup(&path, &[], &mut report);
            }
        }
        None => report.failed += 1,
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read},
        os::unix::fs::symlink,
        process::{Child, Command, Stdio},
        sync::mpsc,
        time::Duration,
    };

    fn cleanup_host_directories(data: &Path, work: &Path) -> CleanupReport {
        super::cleanup_host_directories(data, work, || {
            crate::application::RunApplication::new(data.into(), data.into())
                .durable_input_workspaces(work)
                .ok()
        })
    }

    fn clean(root: &Path) -> CleanupReport {
        let mut report = CleanupReport::default();
        cleanup(root, &[], &mut report);
        report
    }

    #[test]
    fn cleanup_removes_only_reserved_regular_files_without_following_links() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let nested = root.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(root.path().join(".anchor-write-Ab123x"), b"partial").unwrap();
        std::fs::write(nested.join(".anchor-write-Cd456y"), b"partial").unwrap();
        std::fs::write(outside.path().join(".anchor-write-Ef789z"), b"outside").unwrap();
        for name in [
            "fact.json",
            ".anchor-write-not-a-temp",
            ".anchor-write-Ab123x.json",
        ] {
            std::fs::write(root.path().join(name), b"retained").unwrap();
        }
        symlink(outside.path(), root.path().join("external")).unwrap();
        symlink(
            outside.path().join(".anchor-write-Ef789z"),
            root.path().join(".anchor-write-Gh012a"),
        )
        .unwrap();
        assert_eq!(
            clean(root.path()),
            CleanupReport {
                removed: 1,
                ..CleanupReport::default()
            }
        );
        assert!(nested.join(".anchor-write-Cd456y").exists());
        assert!(root.path().join(".anchor-write-Gh012a").is_symlink());
        assert_eq!(
            std::fs::read(outside.path().join(".anchor-write-Ef789z")).unwrap(),
            b"outside"
        );
        assert_eq!(
            std::fs::read(root.path().join("fact.json")).unwrap(),
            b"retained"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 6);
        assert_eq!(clean(root.path()), CleanupReport::default());
        let alias = root.path().join("external");
        assert_eq!(clean(&alias).failed, 1);
    }

    #[test]
    fn missing_cleanup_root_is_a_noop() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            clean(&root.path().join("missing")),
            CleanupReport::default()
        );
        assert!(!root.path().join("missing").exists());
    }

    #[test]
    fn writer_stays_on_locked_directory_when_parent_path_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let moved = root.path().join("moved");
        std::fs::create_dir(&original).unwrap();
        let directory =
            FileLease::acquire(File::open(&original).unwrap(), FlockOperation::LockShared).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("fact.json"), b"unrelated").unwrap();
        let pending =
            PendingWrite::in_directory(directory, std::ffi::OsStr::new("fact.json")).unwrap();
        assert_eq!(clean(&moved).busy, 1);
        assert_eq!(clean(&original), CleanupReport::default());
        pending.publish(b"complete").unwrap();
        assert_eq!(std::fs::read(moved.join("fact.json")).unwrap(), b"complete");
        assert_eq!(
            std::fs::read(original.join("fact.json")).unwrap(),
            b"unrelated"
        );
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 1);
        assert_eq!(clean(&moved), CleanupReport::default());
    }

    #[test]
    fn publication_uses_locked_directory_after_temporary_file_creation() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let moved = root.path().join("moved");
        std::fs::create_dir(&original).unwrap();
        let pending = PendingWrite::new(&original.join("fact.json")).unwrap();
        std::fs::rename(&original, &moved).unwrap();
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("fact.json"), b"unrelated").unwrap();
        assert_eq!(clean(&moved).busy, 1);
        pending.publish(b"complete").unwrap();
        assert_eq!(std::fs::read(moved.join("fact.json")).unwrap(), b"complete");
        assert_eq!(
            std::fs::read(original.join("fact.json")).unwrap(),
            b"unrelated"
        );
        assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 1);
    }

    #[test]
    fn startup_cleanup_targets_only_recorded_wait_input_workspaces() {
        use crate::application::{RunApplication, run_access::fixtures};
        use anchor_runtime::graph::{GraphRunRecord, GraphSnapshot, InvocationKey};

        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("state");
        let work = root.path().join("work");
        let application = RunApplication::new(data.clone(), root.path().join("catalog"));
        let artifacts = crate::HostArtifacts::new(data.join("artifacts"), work.clone());
        let snapshot = GraphSnapshot::from_authoring(serde_json::json!({
            "entry":"wait", "ops":{"wait":{"host":{"operation":"session.wait_input"}},"other":{"run":"true"}},
            "nodes":[{"id":"wait","op":"wait"},{"id":"other","op":"other"}],
            "edges":[{"from":"wait","to":"other"}]
        })).unwrap();
        let mut cleaned = Vec::new();
        let mut retained = Vec::new();
        for (run, bound) in [(".bound", true), (".unbound", false)] {
            let record =
                GraphRunRecord::create_with_id(snapshot.clone(), serde_json::Value::Null, run)
                    .unwrap();
            fixtures::create(&application, &record);
            fixtures::complete(
                &application,
                run,
                "wait",
                fixtures::completion(serde_json::json!({})),
                None,
            );
            for node in ["wait", "other"] {
                let key = InvocationKey {
                    run_id: run.into(),
                    graph_digest: record.graph_digest.clone(),
                    node_id: node.into(),
                    invocation: 1,
                };
                if bound {
                    artifacts
                        .bind_node_workspace(run, &record.graph_digest, node)
                        .unwrap();
                }
                let workspace = artifacts.prepare_workspace(&key, &[]).unwrap();
                let temporary = workspace.join(".anchor-write-Ab123x");
                std::fs::write(&temporary, b"partial").unwrap();
                if node == "wait" {
                    cleaned.push(temporary);
                } else {
                    retained.push(temporary);
                }
            }
        }
        let foreign = work.join("foreign").join("a".repeat(64));
        std::fs::create_dir_all(&foreign).unwrap();
        retained.push(foreign.join(".anchor-write-Ab123x"));
        std::fs::write(retained.last().unwrap(), b"foreign").unwrap();
        drop(application);
        assert_eq!(
            cleanup_host_directories(&data, &work),
            CleanupReport {
                removed: 2,
                ..CleanupReport::default()
            }
        );
        assert!(cleaned.into_iter().all(|path| !path.exists()));
        assert!(retained.into_iter().all(|path| path.exists()));
    }

    #[test]
    fn startup_cleanup_visits_only_known_writer_directory_layouts() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("state");
        let work = root.path().join("work");
        let hash = "a".repeat(64);
        let state_leaves = ["facts", "goose-acp", "goose-acp-spike"];
        let work_leaves = [
            format!(".goose-process/{hash}/config"),
            format!(".goose-process/conversations/gc1-{hash}/process/config"),
        ];
        let leaves: Vec<_> = state_leaves
            .iter()
            .map(|path| data.join(path))
            .chain(work_leaves.iter().map(|path| work.join(path)))
            .collect();
        for leaf in &leaves {
            std::fs::create_dir_all(leaf.join("project")).unwrap();
            std::fs::write(leaf.join(".anchor-write-Ab123x"), b"partial").unwrap();
            std::fs::write(leaf.join("project/.anchor-write-Ab123x"), b"user file").unwrap();
        }
        std::fs::create_dir_all(data.join("artifacts")).unwrap();
        std::fs::write(data.join("artifacts/.anchor-write-Ab123x"), b"retained").unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join(".anchor-write-Ab123x"), b"outside").unwrap();
        std::fs::create_dir_all(work.join("alias")).unwrap();
        symlink(outside.path(), work.join("alias").join(&hash)).unwrap();
        std::fs::create_dir_all(work.join("indirect")).unwrap();
        symlink(outside.path(), work.join("indirect/nodes")).unwrap();
        symlink(
            outside.path(),
            work.join(".goose-process").join("b".repeat(64)),
        )
        .unwrap();
        let report = cleanup_host_directories(&data, &work);
        assert_eq!(
            report,
            CleanupReport {
                removed: 5,
                ..CleanupReport::default()
            }
        );
        for leaf in leaves {
            assert!(!leaf.join(".anchor-write-Ab123x").exists());
            assert_eq!(
                std::fs::read(leaf.join("project/.anchor-write-Ab123x")).unwrap(),
                b"user file"
            );
        }
        assert!(data.join("artifacts/.anchor-write-Ab123x").exists());
        assert_eq!(
            std::fs::read(outside.path().join(".anchor-write-Ab123x")).unwrap(),
            b"outside"
        );
    }

    #[test]
    fn busy_leaf_does_not_prevent_cleanup_of_other_writer_directories() {
        let root = tempfile::tempdir().unwrap();
        for name in ["facts", "goose-acp"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
            std::fs::write(
                root.path().join(name).join(".anchor-write-Ab123x"),
                b"partial",
            )
            .unwrap();
        }
        let pending = PendingWrite::new(&root.path().join("facts/fact.json")).unwrap();
        assert_eq!(
            cleanup_host_directories(root.path(), &root.path().join("missing")),
            CleanupReport {
                removed: 1,
                busy: 1,
                failed: 0
            }
        );
        assert!(root.path().join("facts/.anchor-write-Ab123x").exists());
        drop(pending);
        assert_eq!(
            cleanup_host_directories(root.path(), &root.path().join("missing")),
            CleanupReport {
                removed: 1,
                ..CleanupReport::default()
            }
        );
    }

    #[test]
    fn crash_writer_child() {
        let Some(root) = std::env::var_os("ANCHOR_DURABLE_WRITE_CRASH_DIR") else {
            return;
        };
        let mut pending = PendingWrite::new(&Path::new(&root).join("fact.json")).unwrap();
        pending.temporary.write_all(b"partial").unwrap();
        pending.temporary.as_file().sync_all().unwrap();
        println!("anchor-write-ready");
        io::stdout().flush().unwrap();
        io::stdin().read_exact(&mut [0]).unwrap();
        panic!("crash fixture must be killed before publication");
    }

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn startup_cleanup_preserves_live_writer_then_removes_its_crash_residue() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("fact.json");
        std::fs::write(&target, b"original").unwrap();
        let mut child = ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "durable_write::tests::crash_writer_child",
                    "--nocapture",
                ])
                .env("ANCHOR_DURABLE_WRITE_CRASH_DIR", root.path())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let output = child.0.stdout.take().unwrap();
        let (ready, entered) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let found = BufReader::new(output)
                .lines()
                .any(|line| line.is_ok_and(|line| line.contains("anchor-write-ready")));
            ready.send(found).unwrap();
        });
        assert!(entered.recv_timeout(Duration::from_secs(5)).unwrap());
        reader.join().unwrap();
        assert_eq!(
            clean(root.path()),
            CleanupReport {
                busy: 1,
                ..CleanupReport::default()
            }
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2);
        child.0.kill().unwrap();
        assert!(!child.0.wait().unwrap().success());
        assert_eq!(
            clean(root.path()),
            CleanupReport {
                removed: 1,
                ..CleanupReport::default()
            }
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        write_durable(&target, b"replacement").unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"replacement");
    }
}
