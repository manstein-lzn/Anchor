use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn help_is_available_without_provider_or_fixture_execution() {
    for arguments in [
        vec![],
        vec!["--help"],
        vec!["-h"],
        vec!["regression", "--help"],
        vec!["regression", "fixture", "--help"],
        vec!["regression", "goose-fixture", "-h"],
        vec!["regression", "candidate", "--help"],
        vec!["preflight", "--help"],
        vec!["cutover", "-h"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"))
            .args(&arguments)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("regression fixture|goose-fixture"));
        assert!(help.contains("fixture and goose-fixture are aliases"));
        assert!(help.contains("standard Goose small-Graph regression"));
        assert!(help.contains("deterministic local Provider"));
        assert!(help.contains("explicit ANCHOR_GOOSE_BINARY"));
        assert!(help.contains("pinned Goose 1.53.0"));
        assert!(help.contains("validated by SHA256"));
        assert!(help.contains("anchor-devtools regression candidate"));
        assert!(help.contains("anchor-devtools preflight"));
        assert!(help.contains("anchor-devtools cutover"));
        assert!(help.contains("locked offline release binaries once"));
        assert!(help.contains("ANCHOR_DISTRIBUTION_GOOSE"));
        assert!(help.contains("a blocked decision exits 2"));
        assert!(help.contains("Does not call a real model or load dotenv"));
        assert!(help.contains("Live/business/production acceptance is separate"));
    }
}

#[test]
fn fixture_aliases_require_an_explicit_pinned_binary_before_launching_cargo() {
    let root = tempfile::tempdir().unwrap();
    let binaries = root.path().join("bin");
    fs::create_dir(&binaries).unwrap();
    let cargo = binaries.join("cargo");
    fs::write(
        &cargo,
        b"#!/bin/sh\nprintf called > \"$FIXTURE_COMMAND_LOG\"\nexit 99\n",
    )
    .unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
    let fake_goose = binaries.join("goose");
    fs::write(&fake_goose, b"#!/bin/sh\nprintf '1.53.0\\n'\n").unwrap();
    fs::set_permissions(&fake_goose, fs::Permissions::from_mode(0o700)).unwrap();
    let command_log = root.path().join("commands.log");
    let target = root.path().join("target");
    let evidence = root.path().join("evidence");
    for entry in ["fixture", "goose-fixture"] {
        for binary in [
            None,
            Some("relative/goose".into()),
            Some(fake_goose.clone()),
        ] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"));
            command
                .args(["regression", entry, "--workspace-root"])
                .arg(root.path().join("workspace"))
                .arg("--target-dir")
                .arg(&target)
                .arg("--evidence-root")
                .arg(&evidence)
                .env_clear()
                .env("PATH", &binaries)
                .env("FIXTURE_COMMAND_LOG", &command_log);
            if let Some(binary) = binary {
                command.env("ANCHOR_GOOSE_BINARY", binary);
            }
            let output = command.output().unwrap();
            assert_eq!(output.status.code(), Some(1), "{entry}: {output:?}");
            assert!(String::from_utf8_lossy(&output.stderr).contains("ANCHOR_GOOSE_BINARY"));
            assert!(output.stdout.is_empty());
            assert!(!command_log.exists());
        }
    }
    assert!(!target.exists());
    assert!(!evidence.exists());
}

#[test]
fn unsupported_live_mode_unknown_flags_and_missing_paths_are_errors() {
    let mut cases = vec![
        vec!["unknown"],
        vec!["regression"],
        vec!["regression", "live"],
    ];
    for entry in ["fixture", "goose-fixture", "candidate"] {
        for arguments in [
            vec!["--unknown"],
            vec!["--workspace-root"],
            vec!["--target-dir"],
            vec!["--evidence-root"],
            vec!["--target-dir", "--evidence-root"],
            vec!["--target-dir", "-h"],
            vec!["--workspace-root", ""],
            vec!["--target-dir", "/first", "--target-dir", "/second"],
            vec!["--help", "--unknown"],
        ] {
            let mut case = vec!["regression", entry];
            case.extend(arguments);
            cases.push(case);
        }
    }
    cases.extend([
        vec!["regression", "candidate", "--goose"],
        vec!["regression", "candidate", "--goose", "--target-dir"],
        vec!["regression", "fixture", "--goose", "/goose"],
        vec!["preflight", "--unknown"],
        vec!["preflight", "--target-dir", "/target"],
        vec!["preflight", "extra"],
        vec!["cutover", "--unknown"],
        vec!["cutover", "--rust-state-root"],
        vec!["--help", "unexpected"],
    ]);
    for arguments in cases {
        let output = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"))
            .args(&arguments)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        assert!(!output.stderr.is_empty());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn candidate_checks_explicit_and_environment_goose_before_running_build_tools() {
    let root = tempfile::tempdir().unwrap();
    let binaries = root.path().join("bin");
    fs::create_dir(&binaries).unwrap();
    let command_log = root.path().join("commands.log");
    for name in ["cargo", "npm"] {
        let path = binaries.join(name);
        fs::write(
            &path,
            b"#!/bin/sh\nprintf called > \"$FIXTURE_COMMAND_LOG\"\nexit 99\n",
        )
        .unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let goose = root.path().join("unreviewed-goose");
    fs::write(&goose, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&goose, fs::Permissions::from_mode(0o700)).unwrap();
    for source in [
        "missing",
        "--goose",
        "ANCHOR_GOOSE_BINARY",
        "ANCHOR_DISTRIBUTION_GOOSE",
    ] {
        let evidence = root.path().join(format!("evidence-{source}"));
        let target = root.path().join(format!("target-{source}"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"));
        command
            .args(["regression", "candidate", "--workspace-root"])
            .arg(root.path().join("workspace"))
            .arg("--target-dir")
            .arg(&target)
            .arg("--evidence-root")
            .arg(&evidence)
            .env_clear()
            .env("PATH", &binaries)
            .env("FIXTURE_COMMAND_LOG", &command_log);
        if source == "--goose" {
            command.arg("--goose").arg(&goose);
            command.env("ANCHOR_GOOSE_BINARY", "/ignored-environment-goose");
        } else if source != "missing" {
            command.env(source, &goose);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{source}: {output:?}");
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains(if source == "missing" {
                "candidate requires --goose"
            } else {
                "pinned Goose"
            }),
            "{source}: {error}"
        );
        assert!(!command_log.exists());
        assert!(!target.exists());
        assert!(!evidence.exists());
    }
}

#[test]
fn preflight_without_flags_calls_validation_and_fails_closed_when_unconfigured() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"))
        .arg("preflight")
        .env_clear()
        .env("HOME", root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("ANCHOR_ENV_FILE is required"));
}

#[test]
fn cutover_forwards_paths_and_returns_blocked_json_with_exit_two() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("legacy");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("retained.txt"), b"retained facts").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"))
        .arg("cutover")
        .arg("--legacy-root")
        .arg(&legacy)
        .arg("--rust-state-root")
        .arg(root.path().join("state"))
        .arg("--rust-workspace-root")
        .arg(root.path().join("work"))
        .arg("--rust-catalog-root")
        .arg(root.path().join("catalog"))
        .env_clear()
        .env("HOME", root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["decision"], "blocked");
    assert!(String::from_utf8_lossy(&output.stdout).contains(legacy.to_str().unwrap()));
    assert_eq!(
        fs::read(legacy.join("retained.txt")).unwrap(),
        b"retained facts"
    );
    for directory in ["state", "work", "catalog"] {
        assert!(!root.path().join(directory).exists());
    }
}
