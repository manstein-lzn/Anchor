use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::Command,
};

fn package(destination: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_anchor-docmost-tools"))
        .arg("package-plugin")
        .arg(destination)
        .env_clear()
        .env(
            "DOCMOST_API_KEY",
            "fixture-secret-that-must-not-be-packaged",
        )
        .output()
        .expect("package command")
}

fn files(root: &Path, directory: &Path, paths: &mut Vec<String>) {
    for entry in fs::read_dir(directory).expect("package directory") {
        let path = entry.expect("package entry").path();
        let kind = fs::symlink_metadata(&path).expect("package metadata");
        assert!(!kind.file_type().is_symlink());
        if kind.is_dir() {
            files(root, &path, paths);
        } else {
            paths.push(
                path.strip_prefix(root)
                    .expect("relative package path")
                    .to_str()
                    .expect("path")
                    .to_owned(),
            );
        }
    }
}

#[test]
fn native_package_has_only_manifest_original_skill_and_current_executable() {
    let parent = tempfile::tempdir().expect("parent");
    let destination = parent.path().join("native-docmost");
    let output = package(&destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    let mut paths = Vec::new();
    files(&destination, &destination, &mut paths);
    paths.sort();
    assert_eq!(
        paths,
        [
            "bin/anchor-docmost-tools",
            "plugin.json",
            "skills/docmost/SKILL.md"
        ]
    );
    let manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(destination.join("plugin.json")).expect("manifest"))
            .expect("manifest JSON");
    let mut expected: serde_json::Value =
        serde_json::from_str(include_str!("../../../plugins/docmost/plugin.json"))
            .expect("original manifest");
    expected["mcpServers"]["attachments"]["command"] = "bin/anchor-docmost-tools".into();
    expected["mcpServers"]["attachments"]["args"] = serde_json::json!([]);
    expected["mcpServers"]["attachments"]["cwd"] = ".".into();
    assert_eq!(manifest, expected);
    assert_eq!(
        fs::read(destination.join("skills/docmost/SKILL.md")).expect("skill"),
        include_bytes!("../../../plugins/docmost/skills/docmost/SKILL.md")
    );
    let binary = destination.join("bin/anchor-docmost-tools");
    assert_eq!(
        fs::read(&binary).expect("packaged binary"),
        fs::read(env!("CARGO_BIN_EXE_anchor-docmost-tools")).expect("current executable")
    );
    assert_ne!(
        fs::metadata(&binary)
            .expect("binary metadata")
            .permissions()
            .mode()
            & 0o111,
        0
    );
    for path in &paths {
        let bytes = fs::read(destination.join(path)).expect("package bytes");
        assert!(
            !bytes
                .windows(b"fixture-secret-that-must-not-be-packaged".len())
                .any(|part| part == b"fixture-secret-that-must-not-be-packaged")
        );
    }
    let entries: Vec<_> = fs::read_dir(parent.path())
        .expect("parent entries")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], "native-docmost");
    let copied = Command::new(binary)
        .args(["--endpoint", "file:///tmp/no-request"])
        .env_clear()
        .output()
        .expect("packaged binary executes");
    assert!(!copied.status.success());
    assert!(String::from_utf8_lossy(&copied.stderr).contains("endpoint"));
}

#[test]
fn package_refuses_existing_destinations_and_any_destination_symlink() {
    let parent = tempfile::tempdir().expect("parent");
    let existing = parent.path().join("existing");
    fs::create_dir(&existing).expect("existing directory");
    fs::write(existing.join("sentinel"), b"unchanged").expect("sentinel");
    let file = parent.path().join("existing-file");
    fs::write(&file, b"unchanged").expect("existing file");
    let alias = parent.path().join("alias");
    symlink(&existing, &alias).expect("directory symlink");
    let dangling = parent.path().join("dangling");
    symlink(parent.path().join("not-created"), &dangling).expect("dangling symlink");
    for path in [&existing, &file, &alias, &dangling] {
        let output = package(path);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(String::from_utf8_lossy(&output.stderr).contains("must not exist"));
    }
    assert_eq!(
        fs::read(existing.join("sentinel")).expect("sentinel"),
        b"unchanged"
    );
    assert_eq!(fs::read(file).expect("existing file"), b"unchanged");
    assert!(!parent.path().join("not-created").exists());
    let nested = alias.join("native");
    assert!(!package(&nested).status.success());
    assert!(!existing.join("native").exists());
}

#[test]
fn failed_package_leaves_no_partial_destination_or_success_output() {
    let parent = tempfile::tempdir().expect("parent");
    let destination = parent.path().join("absent/native");
    let output = package(&destination);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!destination.exists());
    assert_eq!(
        fs::read_dir(parent.path()).expect("parent entries").count(),
        0
    );
    fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o500))
        .expect("read-only parent");
    let output = package(&parent.path().join("native"));
    fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700))
        .expect("restore parent permissions");
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        fs::read_dir(parent.path()).expect("parent entries").count(),
        0
    );
}

#[test]
fn concurrent_packages_publish_once_without_overwriting() {
    let parent = tempfile::tempdir().expect("parent");
    let destination = parent.path().join("native");
    let mut first = Command::new(env!("CARGO_BIN_EXE_anchor-docmost-tools"))
        .arg("package-plugin")
        .arg(&destination)
        .env_clear()
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("first packager");
    let mut second = Command::new(env!("CARGO_BIN_EXE_anchor-docmost-tools"))
        .arg("package-plugin")
        .arg(&destination)
        .env_clear()
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("second packager");
    let successes = usize::from(first.wait().expect("first status").success())
        + usize::from(second.wait().expect("second status").success());
    assert_eq!(successes, 1);
    assert_eq!(
        fs::read(destination.join("bin/anchor-docmost-tools")).expect("binary"),
        fs::read(env!("CARGO_BIN_EXE_anchor-docmost-tools")).expect("current exe")
    );
    assert_eq!(
        fs::read_dir(parent.path()).expect("parent entries").count(),
        1
    );
}
