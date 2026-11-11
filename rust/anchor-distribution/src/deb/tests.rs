use super::*;

fn request(root: &Path) -> NativeDebRequest {
    NativeDebRequest {
        archive: root.join("unused.tar.gz"),
        sha256: "0".repeat(64),
        cli: root.join("unused-cli"),
        revision: "1".into(),
        dpkg_deb: root.join("must-not-run"),
        output: root.join("anchor.amd64.deb"),
    }
}

#[test]
fn debian_control_declares_its_version_platform_and_system_dependencies() {
    let version = package_version("0.20261104.1").unwrap();
    assert_eq!(
        version,
        format!("{}-0.20261104.1", env!("CARGO_PKG_VERSION"))
    );
    let control = control(&version);
    for field in [
        "Package: anchor\n",
        &format!("Version: {version}\n"),
        "Architecture: amd64\n",
        "libc6 (>= 2.28)",
        "libc6-dev",
        "libc-bin",
        "libgcc-s1",
        "bubblewrap",
        "slirp4netns",
        "git",
        "ca-certificates",
        "gcc",
        "systemd",
        "passwd",
        "coreutils",
        "util-linux",
        "Existing installations and in-place upgrades are not supported.",
    ] {
        assert!(control.contains(field), "{field}");
    }
}

#[test]
fn debian_revision_cannot_inject_control_fields_shell_or_path_syntax() {
    for revision in [
        "",
        ".",
        "..",
        ".1",
        "-1",
        "_1",
        "1_2",
        "1-2",
        "1+2",
        "1~2",
        "1\nDepends: injected",
        "1;id",
        "$(id)",
        "1/2",
    ] {
        assert!(package_version(revision).is_err(), "{revision:?}");
    }
    assert!(package_version("1").is_ok());
    assert!(package_version("0.20261104.1").is_ok());
}

#[test]
fn debian_metadata_embeds_only_control_and_the_shared_lifecycle_with_adapters() {
    let root = tempfile::tempdir().unwrap();
    write_metadata(root.path(), "0.1.0-1").unwrap();
    let metadata = root.path().join("DEBIAN");
    assert_eq!(
        fs::read_to_string(metadata.join("control")).unwrap(),
        control("0.1.0-1")
    );
    assert_eq!(
        fs::metadata(metadata.join("control"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o644
    );
    for (name, prefix, body) in MAINTAINER_SCRIPTS {
        let path = metadata.join(name);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{prefix}\n{body}")
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o7777,
            0o755
        );
    }
    assert_eq!(fs::read_dir(metadata).unwrap().count(), 5);
}

#[test]
fn native_deb_refuses_overwrite_before_reading_inputs_or_running_tools() {
    let root = tempfile::tempdir().unwrap();
    let request = request(root.path());
    fs::write(&request.output, "existing bytes").unwrap();
    assert!(matches!(
        build_native_deb(&request),
        Err(PackageError::AlreadyExists)
    ));
    assert_eq!(fs::read(&request.output).unwrap(), b"existing bytes");
}

#[test]
fn native_deb_uses_shared_archive_admission_before_running_dpkg() {
    let root = tempfile::tempdir().unwrap();
    let mut request = request(root.path());
    request.sha256 = "untrusted".into();
    let error = build_native_deb(&request).unwrap_err().to_string();
    assert!(error.contains("trusted 64-digit digest"), "{error}");
    assert!(!request.output.exists());
}

#[test]
fn debian_builder_uses_fixed_owner_build_arguments_and_reports_tool_failure() {
    for exit in [0, 9] {
        let root = tempfile::tempdir().unwrap();
        let builder = root.path().join("dpkg-deb");
        fs::write(
            &builder,
            format!(
                "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"${{0%/*}}/calls\"\nexit {exit}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&builder, fs::Permissions::from_mode(0o755)).unwrap();
        let payload = root.path().join("payload with spaces");
        let output = root.path().join("archive.deb");
        let result = package_payload(&builder, &payload, &output);
        assert_eq!(result.is_ok(), exit == 0);
        if let Err(error) = result {
            assert!(error.to_string().contains("dpkg-deb failed with"));
        }
        assert_eq!(
            fs::read_to_string(root.path().join("calls")).unwrap(),
            format!(
                "--root-owner-group\n--build\n{}\n{}\n",
                payload.display(),
                output.display()
            )
        );
    }
}
