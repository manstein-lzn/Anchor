use std::process::Command;

#[test]
fn rpm_builder_cli_requires_reviewed_inputs_and_has_no_deployment_or_pin_bypass_flags() {
    let binary = env!("CARGO_BIN_EXE_anchor-build-rpm");
    let output = Command::new(binary).arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for flag in [
        "--archive",
        "--sha256",
        "--cli",
        "--release",
        "--rpmbuild",
        "--output",
    ] {
        assert!(help.contains(flag), "{flag}");
    }
    for flag in ["--model-config", "--install-root", "--skip-validation"] {
        assert!(!help.contains(flag), "{flag}");
        let rejected = Command::new(binary).arg(flag).output().unwrap();
        assert_eq!(rejected.status.code(), Some(2));
    }
    assert_eq!(
        Command::new(binary).output().unwrap().status.code(),
        Some(2)
    );
}
