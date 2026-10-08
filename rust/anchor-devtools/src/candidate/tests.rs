use super::*;

fn options(root: &Path) -> CandidateOptions {
    CandidateOptions {
        workspace_root: root.join("workspace"),
        target_dir: root.join("target"),
        evidence_root: Some(root.join("evidence")),
        goose: Some(root.join("unreviewed-goose")),
    }
}

#[test]
fn unreviewed_goose_is_rejected_before_any_build_or_evidence_creation() {
    let root = tempfile::tempdir().unwrap();
    let options = options(root.path());
    fs::write(options.goose.as_ref().unwrap(), b"unreviewed binary").unwrap();
    fs::set_permissions(
        options.goose.as_ref().unwrap(),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(run(&options).unwrap_err().contains("pinned Goose"));
    assert!(!options.target_dir.exists());
    assert!(!options.evidence_root.unwrap().exists());
}

#[test]
fn evidence_is_private_and_existing_evidence_is_never_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("evidence");
    let created = evidence_root(Some(&path)).unwrap();
    assert_eq!(
        created.metadata().unwrap().permissions().mode() & 0o777,
        0o700
    );
    fs::write(created.join("retained"), b"existing evidence").unwrap();
    assert!(evidence_root(Some(&path)).is_err());
    assert_eq!(
        fs::read(created.join("retained")).unwrap(),
        b"existing evidence"
    );
}

#[test]
fn build_environment_only_inherits_tool_locations_and_is_offline() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("target");
    let environment = build_environment(root.path(), &target).unwrap();
    for name in environment.keys() {
        assert!(matches!(
            name.to_str().unwrap(),
            "PATH"
                | "RUSTUP_HOME"
                | "RUSTUP_TOOLCHAIN"
                | "HOME"
                | "CARGO_HOME"
                | "CARGO_TARGET_DIR"
                | "CARGO_NET_OFFLINE"
                | "CARGO_BUILD_JOBS"
                | "NPM_CONFIG_USERCONFIG"
                | "NPM_CONFIG_GLOBALCONFIG"
                | "LANG"
                | "TZ"
        ));
    }
    assert_eq!(environment[&OsString::from("CARGO_NET_OFFLINE")], "true");
    assert_eq!(
        Path::new(&environment[&OsString::from("HOME")]),
        root.path().join("build-home")
    );
    let cargo_home = Path::new(&environment[&OsString::from("CARGO_HOME")]);
    assert!(!cargo_home.join("config.toml").exists());
    assert!(!cargo_home.join("credentials.toml").exists());
    let npm_user_config = Path::new(&environment[&OsString::from("NPM_CONFIG_USERCONFIG")]);
    let npm_global_config = Path::new(&environment[&OsString::from("NPM_CONFIG_GLOBALCONFIG")]);
    assert_ne!(npm_user_config, npm_global_config);
    assert_eq!(fs::read(npm_user_config).unwrap(), b"");
    assert_eq!(fs::read(npm_global_config).unwrap(), b"");
    for path in [npm_user_config, npm_global_config] {
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
}

#[test]
fn failed_command_retains_argv_exit_logs_and_cannot_become_passed() {
    let root = tempfile::tempdir().unwrap();
    let command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "printf failure >&2; exit 7".into(),
    ];
    let record = run_logged(
        &command,
        root.path(),
        &BTreeMap::new(),
        root.path(),
        "failed",
    )
    .unwrap();
    assert_eq!(record["argv"], json!(command));
    assert_eq!(record["exit_code"], 7);
    assert_eq!(
        record["stderr_sha256"],
        digest(&root.path().join("failed.stderr.log")).unwrap()
    );
    let mut report = json!({"status":"failed","commands":[],"command_count":0});
    let path = root.path().join("report.json");
    assert!(checked_command(&mut report, record, &path).is_err());
    let saved: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(saved["status"], "failed");
    assert_eq!(saved["command_count"], 1);
    assert_eq!(saved["commands"][0]["exit_code"], 7);
}

#[test]
fn spawn_failure_is_recorded_with_no_invented_exit_code() {
    let root = tempfile::tempdir().unwrap();
    let record = run_logged(
        &[root
            .path()
            .join("missing-program")
            .to_string_lossy()
            .into_owned()],
        root.path(),
        &BTreeMap::new(),
        root.path(),
        "missing",
    )
    .unwrap();
    assert!(record["exit_code"].is_null());
    assert!(record["spawn_error"].is_string());
    assert!(record["stdout_sha256"].is_string());
}

#[test]
fn candidate_selects_only_the_release_distribution_recovery_test() {
    let command = cargo_command(Path::new("/workspace/rust/Cargo.toml"), true);
    for required in ["--release", "--locked", "--offline"] {
        assert!(command.iter().any(|argument| argument == required));
    }
    assert_eq!(
        &command[command
            .iter()
            .position(|argument| argument == "--")
            .unwrap()..],
        [
            "--",
            "--ignored",
            "--exact",
            DISTRIBUTION_TEST,
            "--test-threads=1",
            "--nocapture"
        ]
    );
    assert!(!command.iter().any(|argument| argument == "--workspace"));
    let build = cargo_command(Path::new("/workspace/rust/Cargo.toml"), false);
    assert_eq!(
        build.iter().filter(|argument| *argument == "build").count(),
        1
    );
    for binary in [
        "anchor-runner-host",
        "anchor-distribution",
        "anchor-scholarly",
        "anchor-wecom-gateway",
    ] {
        assert!(
            build
                .windows(2)
                .any(|pair| pair[0] == "-p" && pair[1] == binary)
        );
    }
}
