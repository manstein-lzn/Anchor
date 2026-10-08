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

pub mod candidate;
pub mod cutover;
mod goose;
pub mod preflight;
pub use goose::run_goose_fixture;

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

fn execute(
    arguments: &[&str],
    workspace: &Path,
    target: &Path,
    fixture: &Path,
    log: &mut File,
    goose: &Path,
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
        if ["ANCHOR_", "GOOSE_", "OPENAI_", "ANTHROPIC_", "DEEPSEEK_"]
            .iter()
            .any(|prefix| name_text.starts_with(prefix))
        {
            command.env_remove(name);
        }
    }
    command
        .env("ANCHOR_TEST_EVIDENCE_ROOT", fixture)
        .env("CARGO_BUILD_JOBS", "2")
        .env("ANCHOR_GOOSE_BINARY", goose);
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
