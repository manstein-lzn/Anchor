use std::{fs, os::unix::fs::PermissionsExt, process::Command};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[test]
fn help_is_available_without_python_provider_or_fixture_execution() {
    let output = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"))
        .arg("--help")
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("regression fixture"));
    assert!(help.contains("two explicitly selected native_plugins fixtures"));
    assert!(help.contains("Goose tests are excluded"));
    assert!(help.contains("goose-fixture runs standard Host"));
    assert!(help.contains("explicit ANCHOR_GOOSE_BINARY"));
    assert!(help.contains("not Python"));
    assert!(help.contains("production acceptance is separate"));
}

#[test]
fn goose_fixture_requires_an_explicit_pinned_binary_before_launching_cargo() {
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
    for binary in [None, Some("relative/goose".into()), Some(fake_goose)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"));
        command
            .args(["regression", "goose-fixture"])
            .env_clear()
            .env("PATH", &binaries)
            .env("FIXTURE_COMMAND_LOG", &command_log);
        if let Some(binary) = binary {
            command.env("ANCHOR_GOOSE_BINARY", binary);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("ANCHOR_GOOSE_BINARY"));
        assert!(!command_log.exists());
    }
}

#[test]
fn unsupported_live_mode_unknown_flags_and_missing_paths_are_errors() {
    for arguments in [
        vec!["regression", "live"],
        vec!["regression", "fixture", "--unknown"],
        vec!["regression", "fixture", "--target-dir"],
        vec!["regression", "fixture", "--target-dir", "--evidence-root"],
    ] {
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
fn default_fixture_entry_selects_only_deterministic_tests_and_strips_runtime_configuration() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let target = root.path().join("target");
    let binaries = root.path().join("bin");
    let evidence = root.path().join("evidence");
    for directory in [
        workspace.join("rust"),
        target.join("debug"),
        binaries.clone(),
    ] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(workspace.join("rust/Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(target.join("debug/anchor-runner-host"), b"fixture host").unwrap();
    let report = |scenario| {
        json!({
            "status":"passed", "scenario":scenario,
            "provider":"deterministic loopback fixture",
            "production_data_used":false, "dotenv_loaded":false,
            "host_binary_sha256":format!("{:x}", Sha256::digest(b"fixture host")),
            "test_binary_sha256":"f".repeat(64)
        })
    };
    let cargo = binaries.join("cargo");
    fs::write(
        &cargo,
        format!(
            r#"#!/bin/sh
set -eu
if [ -n "${{ANCHOR_RUNNER_AGENT_RUNTIME-}}${{ANCHOR_GOOSE_BINARY-}}${{ANCHOR_MODEL_URL-}}${{ANCHOR_MODEL_API_KEY-}}" ]; then
    exit 90
fi
printf '%s\n' "$*" >> "$FIXTURE_COMMAND_LOG"
case "$*" in
    *goose_acp*|*--include-ignored*|*--ignored*) exit 91 ;;
esac
case "$2" in
    build) exit 0 ;;
    test) ;;
    *) exit 92 ;;
esac
case "$*" in
    *"--test runtime_contract -- --test-threads=4 --nocapture")
        printf '%s\n' 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s'
        printf '%s' '{runtime}' > "$ANCHOR_TEST_EVIDENCE_ROOT/runtime.json"
        ;;
    *"--test native_plugins -- --exact rust_wecom_stdio_plugin_runs_through_host_harness_and_readonly_package rust_docmost_stdio_plugin_uploads_only_frozen_input_through_real_sandbox --test-threads=4 --nocapture")
        printf '%s\n' 'test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0s'
        printf '%s' '{wecom}' > "$ANCHOR_TEST_EVIDENCE_ROOT/wecom.json"
        printf '%s' '{docmost}' > "$ANCHOR_TEST_EVIDENCE_ROOT/docmost.json"
        ;;
    *) exit 93 ;;
esac
"#,
            runtime = report("runtime"),
            wecom = report("wecom"),
            docmost = report("docmost"),
        ),
    )
    .unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o700)).unwrap();
    let command_log = root.path().join("commands.log");
    let output = Command::new(env!("CARGO_BIN_EXE_anchor-devtools"))
        .args(["regression", "fixture", "--workspace-root"])
        .arg(&workspace)
        .arg("--target-dir")
        .arg(&target)
        .arg("--evidence-root")
        .arg(&evidence)
        .env_clear()
        .env("PATH", &binaries)
        .env("FIXTURE_COMMAND_LOG", &command_log)
        .env("ANCHOR_RUNNER_AGENT_RUNTIME", "goose")
        .env("ANCHOR_GOOSE_BINARY", "/must/not/execute/goose")
        .env("ANCHOR_MODEL_URL", "https://must-not-call.invalid")
        .env("ANCHOR_MODEL_API_KEY", "must-not-inherit")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"], "passed");
    assert_eq!(result["tests"]["suites"], 2);
    assert_eq!(result["tests"]["passed"], 3);
    assert_eq!(result["tests"]["ignored"], 0);
    assert_eq!(result["tests"]["filtered"], 1);
    let recorded: Value =
        serde_json::from_slice(&fs::read(result["evidence"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(recorded["commands"].as_array().unwrap().len(), 2);
    assert_eq!(recorded["exit_codes"], json!([0, 0]));
    assert_eq!(
        recorded["native_plugin_fixture_tests"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(fs::read_to_string(command_log).unwrap().lines().count(), 3);
}
