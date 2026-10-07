use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod goose;
pub use goose::run_goose_fixture;

const NATIVE_PLUGIN_FIXTURE_TESTS: &[&str] = &[
    "rust_wecom_stdio_plugin_runs_through_host_harness_and_readonly_package",
    "rust_docmost_stdio_plugin_uploads_only_frozen_input_through_real_sandbox",
];

#[derive(Debug)]
pub struct FixtureOptions {
    pub workspace_root: PathBuf,
    pub target_dir: PathBuf,
    pub evidence_root: PathBuf,
}

#[derive(Debug, Serialize)]
pub struct FixtureResult {
    pub status: String,
    pub evidence: PathBuf,
    pub scenario_evidence: usize,
    pub tests: TestCounts,
    pub real_model_requests: u64,
    pub failure: Option<String>,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct TestCounts {
    pub suites: usize,
    pub failed_suites: usize,
    pub passed: u64,
    pub failed: u64,
    pub ignored: u64,
    pub filtered: u64,
}

impl TestCounts {
    fn accumulate(&mut self, other: &Self) {
        self.suites += other.suites;
        self.failed_suites += other.failed_suites;
        self.passed += other.passed;
        self.failed += other.failed;
        self.ignored += other.ignored;
        self.filtered += other.filtered;
    }
}

fn fixture_test_commands(manifest: &str) -> [Vec<&str>; 2] {
    let base = vec![
        "+stable",
        "test",
        "--manifest-path",
        manifest,
        "-p",
        "anchor-runner-host",
        "--features",
        "legacy-regression",
        "--test",
    ];
    let mut runtime = base.clone();
    runtime.extend(["runtime_contract", "--", "--test-threads=4", "--nocapture"]);
    let mut plugins = base;
    plugins.extend(["native_plugins", "--", "--exact"]);
    plugins.extend_from_slice(NATIVE_PLUGIN_FIXTURE_TESTS);
    plugins.extend(["--test-threads=4", "--nocapture"]);
    [runtime, plugins]
}

fn validate_fixture_suite(counts: &TestCounts, native_plugins: bool) -> io::Result<()> {
    if counts.suites != 1
        || counts.passed == 0
        || counts.failed_suites != 0
        || counts.failed != 0
        || counts.ignored != 0
        || if native_plugins {
            counts.passed != NATIVE_PLUGIN_FIXTURE_TESTS.len() as u64
        } else {
            counts.filtered != 0
        }
    {
        return Err(io::Error::other(
            "selected fixture suite was missing, failed, ignored or incomplete",
        ));
    }
    Ok(())
}

pub fn run_fixture(options: &FixtureOptions) -> io::Result<FixtureResult> {
    let workspace = options.workspace_root.canonicalize()?;
    let manifest = workspace.join("rust/Cargo.toml");
    if !manifest.is_file() {
        return Err(io::Error::other("workspace has no rust/Cargo.toml"));
    }
    let target = std::path::absolute(&options.target_dir)?;
    fs::create_dir_all(&options.evidence_root)?;
    let root = tempfile::Builder::new()
        .prefix("anchor-native-regression-")
        .tempdir_in(&options.evidence_root)?
        .keep();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let fixture = root.join("fixture");
    fs::create_dir(&fixture)?;
    let manifest = manifest.to_string_lossy().into_owned();
    let build = vec![
        "+stable",
        "build",
        "--manifest-path",
        &manifest,
        "-p",
        "anchor-wecom-tools",
        "-p",
        "anchor-docmost-tools",
        "--bins",
    ];
    let tests = fixture_test_commands(&manifest);
    let mut log = File::create(root.join("fixture.log"))?;
    let started = Instant::now();
    let mut build_exit_code = None;
    let mut test_exit_codes = Vec::new();
    let mut failure = None;
    let mut counts = TestCounts::default();
    let executed = (|| -> io::Result<()> {
        build_exit_code = execute(&build, &workspace, &target, &fixture, &mut log)?;
        if build_exit_code != Some(0) {
            return Err(io::Error::other(
                "native Plugin build failed; inspect fixture.log",
            ));
        }
        for (index, command) in tests.iter().enumerate() {
            let suite = if index == 0 {
                "runtime_contract"
            } else {
                "native_plugins"
            };
            let suite_path = root.join(format!("{suite}.log"));
            let mut suite_log = File::create(&suite_path)?;
            let exit_code = execute(command, &workspace, &target, &fixture, &mut suite_log)?;
            test_exit_codes.push(exit_code);
            let output = fs::read_to_string(&suite_path)?;
            log.write_all(output.as_bytes())?;
            let suite_counts = test_counts(&output)?;
            counts.accumulate(&suite_counts);
            if exit_code != Some(0) {
                return Err(io::Error::other(format!(
                    "{suite} tests failed; inspect fixture.log"
                )));
            }
            validate_fixture_suite(&suite_counts, index == 1)?;
        }
        Ok(())
    })();
    if let Err(error) = executed {
        failure = Some(error.to_string());
    }
    let mut reports = Vec::new();
    let checked = (|| -> io::Result<()> {
        reports = fixture_reports(&fixture, &target.join("debug/anchor-runner-host"))?;
        if counts.suites != 2
            || counts.passed == 0
            || counts.failed_suites != 0
            || counts.failed != 0
            || counts.ignored != 0
            || reports.len() < counts.passed as usize
        {
            return Err(io::Error::other(
                "fixture suites were missing, failed, ignored or lacked evidence",
            ));
        }
        Ok(())
    })();
    if let Err(error) = checked {
        failure.get_or_insert_with(|| error.to_string());
    }
    let result = FixtureResult {
        status: if failure.is_none() {
            "passed"
        } else {
            "failed"
        }
        .into(),
        evidence: root.join("evidence.json"),
        scenario_evidence: reports.len(),
        tests: counts,
        real_model_requests: 0,
        failure,
    };
    let report = json!({
        "status":result.status, "mode":"fixture",
        "scope":"existing deterministic Runtime/native Plugin suites; not all product features, live model or production acceptance",
        "build_command":std::iter::once("cargo").chain(build.iter().copied()).collect::<Vec<_>>(),
        "commands":tests.iter().map(|command| std::iter::once("cargo").chain(command.iter().copied()).collect::<Vec<_>>()).collect::<Vec<_>>(),
        "native_plugin_fixture_tests":NATIVE_PLUGIN_FIXTURE_TESTS,
        "build_exit_code":build_exit_code, "exit_codes":test_exit_codes,
        "elapsed_seconds":started.elapsed().as_secs_f64(),
        "scenario_evidence":reports, "tests":result.tests,
        "real_model_requests":0, "dotenv_loaded":false,
        "failure":result.failure
    });
    fs::write(&result.evidence, serde_json::to_vec_pretty(&report)?)?;
    Ok(result)
}

fn execute(
    arguments: &[&str],
    workspace: &Path,
    target: &Path,
    fixture: &Path,
    log: &mut File,
) -> io::Result<Option<i32>> {
    execute_fixture(arguments, workspace, target, fixture, log, None)
}

fn execute_fixture(
    arguments: &[&str],
    workspace: &Path,
    target: &Path,
    fixture: &Path,
    log: &mut File,
    goose: Option<&Path>,
) -> io::Result<Option<i32>> {
    let mut command = Command::new("cargo");
    command
        .args(arguments)
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", target)
        .env("ANCHOR_TEST_EVIDENCE_ROOT", fixture)
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log.try_clone()?));
    for (name, _) in std::env::vars_os() {
        let name_text = name.to_string_lossy();
        if name_text.starts_with("ANCHOR_")
            || goose.is_some()
                && ["GOOSE_", "OPENAI_", "ANTHROPIC_", "DEEPSEEK_"]
                    .iter()
                    .any(|prefix| name_text.starts_with(prefix))
        {
            command.env_remove(name);
        }
    }
    command
        .env("ANCHOR_TEST_EVIDENCE_ROOT", fixture)
        .env("CARGO_BUILD_JOBS", "2");
    if let Some(goose) = goose {
        command.env("ANCHOR_GOOSE_BINARY", goose);
    }
    Ok(command.status()?.code())
}

fn digest(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65_536];
    loop {
        let length = file.read(&mut buffer)?;
        if length == 0 {
            return Ok(format!("{:x}", digest.finalize()));
        }
        digest.update(&buffer[..length]);
    }
}

fn fixture_reports(root: &Path, host_binary: &Path) -> io::Result<Vec<Value>> {
    let host_digest = digest(host_binary)?;
    let mut paths = fs::read_dir(root)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    paths.sort();
    let mut reports = Vec::new();
    let mut scenarios = std::collections::BTreeSet::new();
    for path in paths {
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let report: Value = serde_json::from_slice(&fs::read(&path)?)?;
        if report["status"] != "passed"
            || report["provider"] != "deterministic loopback fixture"
            || report["production_data_used"] != false
            || report["dotenv_loaded"] != false
            || report["host_binary_sha256"] != host_digest
            || report["scenario"]
                .as_str()
                .is_none_or(|name| name.is_empty())
            || !report["test_binary_sha256"].as_str().is_some_and(|hash| {
                hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(io::Error::other(format!(
                "invalid fixture evidence: {}",
                path.display()
            )));
        }
        if !scenarios.insert(report["scenario"].as_str().unwrap().to_owned()) {
            return Err(io::Error::other("duplicate fixture scenario evidence"));
        }
        reports.push(json!({"path":path, "scenario":report["scenario"], "host_binary_sha256":host_digest, "test_binary_sha256":report["test_binary_sha256"]}));
    }
    if reports.is_empty() {
        return Err(io::Error::other(
            "fixture suite produced no scenario evidence",
        ));
    }
    Ok(reports)
}

fn test_counts(log: &str) -> io::Result<TestCounts> {
    let mut counts = TestCounts::default();
    for line in log.lines() {
        let Some(summary) = line.strip_prefix("test result: ") else {
            continue;
        };
        let summary = if let Some(summary) = summary.strip_prefix("ok. ") {
            summary
        } else if let Some(summary) = summary.strip_prefix("FAILED. ") {
            counts.failed_suites += 1;
            summary
        } else {
            return Err(io::Error::other("unknown fixture test outcome"));
        };
        counts.suites += 1;
        let fields = summary.split(';').take(5).collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(io::Error::other("incomplete fixture test summary"));
        }
        for (field, expected) in fields
            .into_iter()
            .zip(["passed", "failed", "ignored", "measured", "filtered"])
        {
            let mut parts = field.split_whitespace();
            let value = parts
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| io::Error::other("invalid fixture test count"))?;
            let kind = parts.next();
            if kind != Some(expected) {
                return Err(io::Error::other("invalid fixture test summary fields"));
            }
            match kind {
                Some("passed") => counts.passed += value,
                Some("failed") => counts.failed += value,
                Some("ignored") => counts.ignored += value,
                Some("filtered") => counts.filtered += value,
                Some("measured") => {}
                _ => return Err(io::Error::other("unknown fixture test count")),
            }
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests;
