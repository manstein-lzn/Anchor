use anchor_distribution::{GOOSE_SHA256, PackageReport};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    path::PathBuf,
    process::{Command, Output},
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-distribution"));
    command.env_clear().env("PATH", "/usr/bin:/bin");
    command
}

fn assert_failure(output: Output) {
    assert!(!output.status.success());
    assert!(!output.stderr.is_empty());
    assert!(output.stdout.is_empty());
}

#[test]
fn cli_exposes_the_agreed_inputs_without_a_pin_bypass() {
    let output = command().arg("--help").output().unwrap();
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for argument in [
        "--host", "--goose", "--bundle", "--web", "--tool", "--output",
    ] {
        assert!(help.contains(argument), "{argument}");
    }
    assert!(!help.contains("skip"));
    assert!(!help.contains("python"));
}

#[test]
fn cli_rejects_missing_required_inputs_and_malformed_tool_mappings() {
    assert_failure(command().output().unwrap());
    assert_failure(
        command()
            .args([
                "--host",
                "/unused/host",
                "--goose",
                "/unused/goose",
                "--bundle",
                "/unused/bundle",
                "--output",
                "/unused/runtime.tar.gz",
                "--tool",
                "broken",
            ])
            .output()
            .unwrap(),
    );
}

#[test]
fn cli_rejects_overwrite_before_reading_other_inputs() {
    let directory = tempfile::tempdir().unwrap();
    let output = directory.path().join("runtime.tar.gz");
    fs::write(&output, "retained").unwrap();
    assert_failure(
        command()
            .args([
                "--host",
                "/unused/host",
                "--goose",
                "/unused/goose",
                "--bundle",
                "/unused/bundle",
                "--output",
            ])
            .arg(&output)
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read(output).unwrap(), b"retained");
}

#[test]
fn cli_rejects_an_unpinned_elf_without_creating_output() {
    let directory = tempfile::tempdir().unwrap();
    let host = directory.path().join("host");
    let goose = directory.path().join("goose");
    fs::copy("/usr/bin/true", &host).unwrap();
    fs::copy("/usr/bin/true", &goose).unwrap();
    let output = directory.path().join("runtime.tar.gz");
    let result = command()
        .arg("--host")
        .arg(host)
        .arg("--goose")
        .arg(goose)
        .args(["--bundle", "/unused/bundle", "--output"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("pinned v1.53.0"));
    assert_failure(result);
    assert!(!output.exists());
}

#[test]
#[ignore = "separate local pinned-binary packaging smoke; requires ANCHOR_DISTRIBUTION_GOOSE"]
fn cli_packages_pinned_goose_and_reports_archive_hash_without_executing_inputs() {
    let goose = PathBuf::from(
        std::env::var_os("ANCHOR_DISTRIBUTION_GOOSE").expect("set fixed v1.53.0 musl Goose path"),
    );
    let directory = tempfile::tempdir().unwrap();
    let host = directory.path().join("host");
    fs::copy("/usr/bin/true", &host).unwrap();
    let bundle = directory.path().join("bundle");
    fs::create_dir(&bundle).unwrap();
    fs::write(bundle.join("graph.json"), r#"{"objective":"package smoke","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]}"#).unwrap();
    fs::write(
        bundle.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let archive = directory.path().join("runtime.tar.gz");
    let result = command()
        .arg("--host")
        .arg(host)
        .arg("--goose")
        .arg(goose)
        .arg("--bundle")
        .arg(bundle)
        .arg("--output")
        .arg(&archive)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(result.stderr.is_empty());
    let report: PackageReport = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report.output, archive);
    assert_eq!(report.inventory.goose_sha256, GOOSE_SHA256);
    let mut hash = Sha256::new();
    let mut input = fs::File::open(archive).unwrap();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    assert_eq!(report.sha256, format!("{:x}", hash.finalize()));
    assert_eq!(report.inventory.executables.len(), 2);
    let goose = report
        .inventory
        .executables
        .iter()
        .find(|executable| executable.path == "bin/goose")
        .unwrap();
    assert!(goose.runtime.interpreter.is_none());
    assert!(goose.runtime.needed.is_empty());
}
