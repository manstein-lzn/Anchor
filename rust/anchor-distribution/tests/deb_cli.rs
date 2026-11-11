use std::process::Command;

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-build-deb"));
    command.env_clear().env("PATH", "/usr/bin:/bin");
    command
}

#[test]
fn deb_builder_requires_reviewed_inputs_without_deployment_or_pin_bypass_flags() {
    let output = command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for flag in [
        "--archive",
        "--sha256",
        "--cli",
        "--revision",
        "--dpkg-deb",
        "--output",
    ] {
        assert!(help.contains(flag), "{flag}");
    }
    for flag in ["--model-config", "--install-root", "--skip-validation"] {
        assert!(!help.contains(flag), "{flag}");
        assert_eq!(command().arg(flag).output().unwrap().status.code(), Some(2));
    }
    assert_eq!(command().output().unwrap().status.code(), Some(2));
}

#[test]
fn deb_builder_accepts_default_revision_and_admits_archive_before_running_tools() {
    let root = tempfile::tempdir().unwrap();
    let output_path = root.path().join("anchor.amd64.deb");
    let output = command()
        .args([
            "--archive",
            "/unavailable/archive",
            "--sha256",
            "untrusted",
            "--cli",
            "/unused/cli",
        ])
        .arg("--output")
        .arg(&output_path)
        .arg("--dpkg-deb")
        .arg(root.path().join("must-not-run"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("trusted 64-digit digest"));
    assert!(!output_path.exists());
}
