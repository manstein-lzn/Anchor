use super::*;

fn request(root: &Path) -> NativeRpmRequest {
    NativeRpmRequest {
        archive: root.join("unused.tar.gz"),
        sha256: "0".repeat(64),
        cli: root.join("unused-cli"),
        release: "1.el8".into(),
        rpmbuild: root.join("must-not-run"),
        output: root.join("anchor.x86_64.rpm"),
    }
}

#[test]
fn rpm_owns_only_release_files_and_includes_literal_fresh_install_lifecycle() {
    let spec = specification("0.20261104.1.el8").unwrap();
    assert!(spec.contains("%global __os_install_post %{nil}"));
    assert!(spec.contains("%global debug_package %{nil}"));
    assert!(spec.contains("BuildArch: x86_64"));
    assert!(spec.contains("Requires: bubblewrap"));
    assert!(spec.contains("Requires: slirp4netns"));
    assert!(spec.contains("Requires: gcc"));
    let files = spec
        .split("%files\n")
        .nth(1)
        .unwrap()
        .split("\n%pre\n")
        .next()
        .unwrap();
    assert!(files.contains("/var/lib/anchor/platform/release\n"));
    assert!(!files.contains("/data"));
    assert!(!files.contains("/config"));
    for (section, body) in [
        ("pre", include_str!("../../../../deploy/native/pre.sh")),
        ("post", include_str!("../../../../deploy/native/post.sh")),
        ("preun", include_str!("../../../../deploy/native/preun.sh")),
        (
            "postun",
            include_str!("../../../../deploy/native/postun.sh"),
        ),
    ] {
        assert!(spec.contains(&format!("\n%{section}\n{}", body.replace('%', "%%"))));
    }
}

#[test]
fn rpm_release_cannot_inject_spec_or_shell_syntax() {
    for release in [
        "",
        ".",
        "..",
        "-1",
        "_1",
        "1-2",
        "1\n%post",
        "%{lua:os.exit()}",
        "1;id",
        "1/2",
    ] {
        assert!(specification(release).is_err(), "{release:?}");
    }
    assert!(specification("1.el8_10").is_ok());
}

#[test]
fn rpm_build_paths_cannot_inject_macros_or_shell_substitutions() {
    for path in [
        "/tmp/space here",
        "/tmp/%{macro}",
        "/tmp/$(touch marker)",
        "/tmp/`id`",
        "/tmp/$HOME",
        "/tmp/quote\"",
        "/tmp/backslash\\",
        "/tmp/line\nbreak",
    ] {
        assert!(rpm_build_path(Path::new(path)).is_err(), "{path:?}");
    }
    assert_eq!(
        rpm_build_path(Path::new("/tmp/anchor-rpm-A1._-")).unwrap(),
        "/tmp/anchor-rpm-A1._-"
    );
}

#[test]
fn native_rpm_refuses_overwrite_before_reading_inputs_or_running_tools() {
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path());
    fs::write(&request.output, "existing bytes").unwrap();
    assert!(matches!(
        build_native_rpm(&request),
        Err(PackageError::AlreadyExists)
    ));
    assert_eq!(fs::read(&request.output).unwrap(), b"existing bytes");
}

#[test]
fn native_rpm_uses_shared_archive_admission_before_running_rpmbuild() {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path());
    request.sha256 = "untrusted".into();
    let error = build_native_rpm(&request).unwrap_err().to_string();
    assert!(error.contains("trusted 64-digit digest"), "{error}");
    assert!(!request.output.exists());
}
