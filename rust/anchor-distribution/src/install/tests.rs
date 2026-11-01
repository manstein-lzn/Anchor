use super::*;
use crate::platform::tests::Fixture;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, os::unix::fs::symlink};

fn model(root: &Path) -> PathBuf {
    let path = root.join("model-input.env");
    fs::write(&path, "ANCHOR_MODEL_URL=https://provider.example/v1\nANCHOR_MODEL_API_KEY='fixture-private-key'\nANCHOR_MODEL_NAME=fixture-model\nANCHOR_MODEL_WIRE_API=chat\n").unwrap();
    path
}

fn request(fixture: &Fixture) -> PlatformInstallRequest {
    let report = fixture.package().unwrap();
    PlatformInstallRequest {
        archive: report.output,
        sha256: report.sha256,
        install_root: fixture.root.path().join("installation"),
        model_config: None,
    }
}

fn admitted_install(
    fixture: &Fixture,
    request: &PlatformInstallRequest,
) -> Result<PlatformInstallReport> {
    install(
        request,
        &fixture.digest,
        &fixture.digest,
        &fixture.digest,
        &fixture.digest,
    )
}

#[test]
fn fresh_install_separates_release_and_host_settings_and_preserves_retired_model_files() {
    let fixture = Fixture::new();
    let request = request(&fixture);
    let report = admitted_install(&fixture, &request).unwrap();
    assert_eq!(
        report.launcher,
        request
            .install_root
            .join("release/anchor-runtime/bin/anchor-platform")
    );
    assert!(report.model_config.is_none());
    assert_eq!(
        fs::metadata(&report.release_root)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    assert_eq!(
        fs::metadata(report.release_root.join("bin/uv"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    assert_eq!(
        fs::metadata(report.release_root.join("runtime-manifest.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
    assert_eq!(
        fs::read_dir(request.install_root.join("data/catalog"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        fs::read_dir(request.install_root.join("data/library"))
            .unwrap()
            .count(),
        0
    );
    assert!(!report.release_root.join("bundle").exists());
    assert!(!report.release_root.join("model.env").exists());
    let legacy_model = request.install_root.join("config/model.env");
    fs::write(
        &legacy_model,
        "retired file with invalid syntax and fixture-private-key",
    )
    .unwrap();
    let command = platform_command(&report.launcher).unwrap();
    let values = command
        .get_envs()
        .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
        .collect::<BTreeMap<_, _>>();
    assert!(!values.contains_key("ANCHOR_MODEL_API_KEY"));
    assert!(!values.contains_key("ANCHOR_MODEL_URL"));
    assert_eq!(values["ANCHOR_GOOSE_LOCAL_NETWORK"], "1");
    assert_eq!(values["ANCHOR_RUNNER_ALLOWED_COMMANDS"], "sh,git");
    assert!(!values.contains_key("ANCHOR_RUNNER_BUNDLE_ROOT"));
    assert!(!values.contains_key("ANCHOR_DEPENDENCIES_PYTHON"));
    assert!(!values.contains_key("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK"));
    let alternate = platform_command_with_listen(&report.launcher, "127.0.0.1:19087").unwrap();
    assert!(
        alternate
            .get_envs()
            .any(|(key, value)| key == "ANCHOR_RUNNER_LISTEN"
                && value == Some(std::ffi::OsStr::new("127.0.0.1:19087")))
    );
    assert!(platform_command_with_listen(&report.launcher, "0.0.0.0:8077").is_err());
    assert!(platform_command_with_listen(&report.launcher, "127.0.0.1:0").is_err());
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("fixture-private-key"));
    let original = fs::read(&legacy_model).unwrap();
    assert!(matches!(
        admitted_install(&fixture, &request),
        Err(PackageError::AlreadyExists)
    ));
    assert_eq!(fs::read(&legacy_model).unwrap(), original);
    // Test cleanup must make private read-only release directories writable.
    make_directories_writable(&request.install_root).unwrap();
}

#[test]
fn model_files_are_rejected_without_reading_or_echoing_credentials() {
    let fixture = Fixture::new();
    let mut request = request(&fixture);
    request.model_config = Some(model(fixture.root.path()));
    let error = admitted_install(&fixture, &request)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("configure model services in WebUI Settings"),
        "{error}"
    );
    assert!(!error.contains("fixture-private-key"));
    assert!(!request.install_root.exists());
}

#[test]
fn wrong_archive_digest_invalid_model_and_symlink_inputs_leave_no_installation() {
    for scenario in ["digest", "model", "symlink", "ancestor"] {
        let fixture = Fixture::new();
        let mut request = request(&fixture);
        match scenario {
            "digest" => request.sha256 = "0".repeat(64),
            "model" => request.model_config = Some(model(fixture.root.path())),
            "symlink" => {
                let alias = fixture.root.path().join("model-alias");
                symlink(model(fixture.root.path()), &alias).unwrap();
                request.model_config = Some(alias);
            }
            "ancestor" => {
                let alias = fixture.root.path().join("parent-alias");
                symlink(fixture.root.path(), &alias).unwrap();
                request.install_root = alias.join("installation");
            }
            _ => unreachable!(),
        }
        assert!(admitted_install(&fixture, &request).is_err(), "{scenario}");
        assert!(!request.install_root.exists());
        assert!(!fs::read_dir(fixture.root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .starts_with(".anchor-install-")
        }));
    }
}

fn custom_archive(root: &Path, names: &[(&str, tar::EntryType)]) -> (PathBuf, String) {
    let file = root.join("custom.tar.gz");
    let compressed =
        flate2::write::GzEncoder::new(File::create(&file).unwrap(), flate2::Compression::fast());
    let mut builder = tar::Builder::new(compressed);
    for (name, kind) in names {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(*kind);
        header.set_size(0);
        header.set_mode(if kind.is_dir() { 0o755 } else { 0o644 });
        if kind.is_symlink() {
            header.set_link_name("/etc/passwd").unwrap();
        }
        builder
            .append_data(&mut header, name, std::io::empty())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap();
    let digest = format!("{:x}", Sha256::digest(fs::read(&file).unwrap()));
    (file, digest)
}

#[test]
fn installer_refuses_links_duplicates_non_platform_payload_and_unlisted_paths() {
    for names in [
        vec![
            ("anchor-runtime", tar::EntryType::Directory),
            ("anchor-runtime/escape", tar::EntryType::Symlink),
        ],
        vec![
            ("anchor-runtime", tar::EntryType::Directory),
            ("anchor-runtime", tar::EntryType::Directory),
        ],
        vec![("other-runtime", tar::EntryType::Directory)],
    ] {
        let fixture = Fixture::new();
        let (archive, sha256) = custom_archive(fixture.root.path(), &names);
        let request = PlatformInstallRequest {
            archive,
            sha256,
            install_root: fixture.root.path().join("install"),
            model_config: None,
        };
        assert!(admitted_install(&fixture, &request).is_err());
        assert!(!request.install_root.exists());
    }
    let fixture = Fixture::new();
    fixture.package().unwrap();
    let extracted = fixture.root.path().join("unpacked");
    fs::create_dir(&extracted).unwrap();
    fixture.extract(&extracted);
    let runtime = extracted.join("anchor-runtime");
    fs::write(runtime.join("unlisted.txt"), "cannot ship\n").unwrap();
    assert!(
        validate_runtime(
            &runtime,
            &fixture.digest,
            &fixture.digest,
            &fixture.digest,
            &fixture.digest,
            false
        )
        .is_err()
    );
}

#[test]
fn fresh_install_without_model_file_starts_with_web_managed_configuration() {
    let fixture = Fixture::new();
    let mut request = request(&fixture);
    request.model_config = None;
    let report = admitted_install(&fixture, &request).unwrap();
    assert!(report.model_config.is_none());
    assert!(!request.install_root.join("config/model.env").exists());
    let command = platform_command(&report.launcher).unwrap();
    let values = command
        .get_envs()
        .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
        .collect::<BTreeMap<_, _>>();
    for name in [
        "ANCHOR_MODEL_URL",
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_NAME",
        "ANCHOR_MODEL_WIRE_API",
        "ANCHOR_MODEL_ALIASES",
        "ANCHOR_MODEL_CONTEXT_WINDOW",
        "ANCHOR_MODEL_MAX_TOKENS",
    ] {
        assert!(
            !values.contains_key(name),
            "omitted model setting became deployment-owned: {name}"
        );
    }
    assert_eq!(values["ANCHOR_MODEL_DIRECT"], "0");
    assert_eq!(
        values["ANCHOR_RUNNER_STATE_ROOT"],
        request.install_root.join("data/state").to_str().unwrap()
    );
    assert!(
        !request
            .install_root
            .join("data/state/managed/model-config.json")
            .exists()
    );
    make_directories_writable(&request.install_root).unwrap();
}

#[test]
fn installed_runtime_validation_uses_read_only_modes_and_needs_no_config_directory() {
    let fixture = Fixture::new();
    let request = request(&fixture);
    let report = admitted_install(&fixture, &request).unwrap();
    fs::remove_dir(request.install_root.join("config")).unwrap();
    assert!(platform_command(&report.launcher).is_ok());
    validate_runtime(
        &report.release_root,
        &fixture.digest,
        &fixture.digest,
        &fixture.digest,
        &fixture.digest,
        true,
    )
    .unwrap();
    assert!(
        validate_runtime(
            &report.release_root,
            &fixture.digest,
            &fixture.digest,
            &fixture.digest,
            &fixture.digest,
            false
        )
        .is_err()
    );
    let path = report.release_root.join("runtime-manifest.json");
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    for (field, value) in [
        ("version", "0.0.0"),
        ("source", "https://example.com/pixi"),
        ("path", "bin/other"),
        ("sha256", PIXI_SHA256),
    ] {
        let mut manifest = original.clone();
        // Public validation must reject a different pixi identity, before
        // inspecting fixture payload hashes against production pins.
        manifest["goose_sha256"] = crate::GOOSE_SHA256.into();
        manifest["bootstrap"]["uv"]["sha256"] = crate::UV_SHA256.into();
        manifest["bootstrap"]["rustup_init"]["sha256"] = crate::RUSTUP_INIT_SHA256.into();
        manifest["bootstrap"]["pixi"]["sha256"] = PIXI_SHA256.into();
        manifest["bootstrap"]["pixi"][field] = if field == "sha256" {
            "0".repeat(64).into()
        } else {
            value.into()
        };
        fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error = validate_platform_runtime(&report.release_root)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("bootstrap identity differs"),
            "{field}: {error}"
        );
    }
    let mut obsolete = original;
    obsolete["format"] = 2.into();
    obsolete["bootstrap"]
        .as_object_mut()
        .unwrap()
        .remove("pixi");
    fs::write(&path, serde_json::to_vec(&obsolete).unwrap()).unwrap();
    let error = validate_platform_runtime(&report.release_root)
        .unwrap_err()
        .to_string();
    assert!(error.contains("format-3"), "{error}");
    make_directories_writable(&request.install_root).unwrap();
}
