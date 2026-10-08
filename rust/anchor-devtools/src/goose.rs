use super::*;

const GOOSE_VERSION: &str = "1.53.0";
const GOOSE_SHA256: &str = "bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340";
const SUITES: &[&str] = &[
    "goose_acp",
    "goose_pilot",
    "goose_elicitation",
    "goose_media",
    "goose_conversation",
    "goose_channel",
    "goose_compaction",
    "goose_pilot_compaction",
    "goose_trace",
    "goose_session_calls",
    "goose_library",
    "native_plugins",
];

fn configured_binary() -> io::Result<PathBuf> {
    let path = std::env::var_os("ANCHOR_GOOSE_BINARY")
        .map(PathBuf::from)
        .ok_or_else(|| {
            io::Error::other("regression fixture requires explicit ANCHOR_GOOSE_BINARY")
        })?;
    validate_binary(&path)?;
    path.canonicalize()
}

fn validate_binary(path: &Path) -> io::Result<()> {
    if !path.is_absolute()
        || !path.is_file()
        || path.metadata()?.permissions().mode() & 0o111 == 0
        || digest(path)? != GOOSE_SHA256
    {
        return Err(io::Error::other(format!(
            "ANCHOR_GOOSE_BINARY must be the absolute executable pinned Goose {GOOSE_VERSION} x86_64 musl binary (SHA256 {GOOSE_SHA256})"
        )));
    }
    Ok(())
}

fn test_command<'manifest>(manifest: &'manifest str, suite: &'manifest str) -> Vec<&'manifest str> {
    vec![
        "+stable",
        "test",
        "--manifest-path",
        manifest,
        "-p",
        "anchor-runner-host",
        "--no-default-features",
        "--locked",
        "--test",
        suite,
        "--",
        "--ignored",
        "--test-threads=4",
        "--nocapture",
    ]
}

fn command_record(arguments: &[&str]) -> Vec<String> {
    std::iter::once("cargo")
        .chain(arguments.iter().copied())
        .map(str::to_owned)
        .collect()
}

pub fn run_goose_fixture(options: &FixtureOptions) -> io::Result<FixtureResult> {
    let goose = configured_binary()?;
    let workspace = options.workspace_root.canonicalize()?;
    let manifest = workspace.join("rust/Cargo.toml");
    if !manifest.is_file() {
        return Err(io::Error::other("workspace has no rust/Cargo.toml"));
    }
    let manifest = manifest.to_string_lossy().into_owned();
    let target = std::path::absolute(&options.target_dir)?;
    let evidence_root = std::path::absolute(&options.evidence_root)?;
    fs::create_dir_all(&evidence_root)?;
    let root = tempfile::Builder::new()
        .prefix("anchor-goose-regression-")
        .tempdir_in(&evidence_root)?
        .keep();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let fixture = root.join("fixture");
    fs::create_dir(&fixture)?;
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
        "--locked",
    ];
    let started = Instant::now();
    let mut log = File::create(root.join("fixture.log"))?;
    let mut commands = Vec::new();
    let mut exit_codes = Vec::new();
    let mut counts = TestCounts::default();
    let mut reports = Vec::new();
    let mut build_exit_code = None;
    let executed = (|| -> io::Result<()> {
        commands.push(command_record(&build));
        build_exit_code = execute(&build, &workspace, &target, &fixture, &mut log, &goose)?;
        exit_codes.push(build_exit_code);
        if build_exit_code != Some(0) {
            return Err(io::Error::other(
                "official Plugin binary build failed; inspect fixture.log",
            ));
        }
        for suite in SUITES {
            let command = test_command(&manifest, suite);
            let suite_root = fixture.join(suite);
            fs::create_dir(&suite_root)?;
            let suite_path = root.join(format!("{suite}.log"));
            let mut suite_log = File::create(&suite_path)?;
            commands.push(command_record(&command));
            let exit_code = execute(
                &command,
                &workspace,
                &target,
                &suite_root,
                &mut suite_log,
                &goose,
            )?;
            exit_codes.push(exit_code);
            let output = fs::read_to_string(suite_path)?;
            log.write_all(output.as_bytes())?;
            let suite_counts = test_counts(&output)?;
            counts.accumulate(&suite_counts);
            if exit_code != Some(0) {
                return Err(io::Error::other(format!(
                    "{suite} failed; inspect fixture.log"
                )));
            }
            validate_suite(&suite_counts)?;
            let suite_reports =
                scenario_reports(&suite_root, &target.join("debug/anchor-runner-host"), suite)?;
            if suite_reports.len() as u64 != suite_counts.passed {
                return Err(io::Error::other(format!(
                    "{suite} lacks one valid evidence report per passed test"
                )));
            }
            reports.extend(suite_reports);
        }
        Ok(())
    })();
    let failure = executed.err().map(|error| error.to_string());
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
    fs::write(
        &result.evidence,
        serde_json::to_vec_pretty(&json!({
            "status":result.status, "mode":"goose-fixture",
            "scope":"selected deterministic Goose ACP/Pilot/native Plugin fixtures, including spike negatives; not all product capabilities, live model or production acceptance",
            "goose_binary":goose, "goose_version":GOOSE_VERSION, "goose_binary_sha256":GOOSE_SHA256,
            "commands":commands, "exit_codes":exit_codes, "build_exit_code":build_exit_code,
            "elapsed_seconds":started.elapsed().as_secs_f64(), "tests":result.tests,
            "scenario_evidence":reports, "real_model_requests":0,
            "production_data_used":false, "dotenv_loaded":false, "failure":result.failure,
        }))?,
    )?;
    Ok(result)
}

fn validate_suite(counts: &TestCounts) -> io::Result<()> {
    if counts.suites != 1
        || counts.passed == 0
        || counts.failed_suites != 0
        || counts.failed != 0
        || counts.ignored != 0
    {
        return Err(io::Error::other(
            "Goose fixture suite was missing, failed or ignored",
        ));
    }
    Ok(())
}

fn evidence_paths(root: &Path, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(io::Error::other(
                "Goose fixture evidence cannot contain symlinks",
            ));
        }
        if kind.is_dir() {
            evidence_paths(&entry.path(), paths)?;
        } else if entry.file_name() == "evidence.json" {
            paths.push(entry.path());
        }
    }
    Ok(())
}

fn scenario_reports(root: &Path, host_binary: &Path, suite: &str) -> io::Result<Vec<Value>> {
    let host_digest = digest(host_binary)?;
    let mut paths = Vec::new();
    evidence_paths(root, &mut paths)?;
    paths.sort();
    let mut reports = Vec::new();
    let mut scenarios = std::collections::BTreeSet::new();
    for path in paths {
        let report: Value = serde_json::from_slice(&fs::read(&path)?)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("missing evidence parent"))?;
        let provider_path = parent.join("provider.json");
        let provider: Value = serde_json::from_slice(&fs::read(&provider_path)?)?;
        validate_report(&report, &provider, &host_digest)?;
        let scenario = parent
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("missing scenario identity"))?;
        if !scenarios.insert(scenario.to_owned()) {
            return Err(io::Error::other(
                "duplicate Goose fixture scenario evidence",
            ));
        }
        reports.push(json!({
            "path":path, "provider_path":provider_path, "suite":suite, "scenario":scenario,
            "status":report["status"], "runtime":report["runtime"],
            "goose_binary_sha256":report["goose_binary_sha256"],
            "host_binary_sha256":report["host_binary_sha256"],
            "real_model_requests":0, "production_data_used":false,
            "production_verification":if report["production_data_used"] == false {
                "explicit scenario assertion"
            } else { "configuration rejected before Graph admission and Goose process startup" },
        }));
    }
    if reports.is_empty() {
        return Err(io::Error::other(
            "Goose fixture produced no scenario evidence",
        ));
    }
    Ok(reports)
}

fn validate_report(report: &Value, provider: &Value, host_digest: &str) -> io::Result<()> {
    let requests = provider["requests"]
        .as_array()
        .ok_or_else(|| io::Error::other("missing deterministic Provider requests"))?;
    let rejected_before_start = report["runtime"] == "goose-acp-spike"
        && report["response"]["kind"] == "rejected"
        && report["graph_run_admitted"] == false
        && report["goose_process_started"] == false
        && report["runtime_shared_network_explicitly_authorized"] == false
        && report["provider_requests"].as_u64() == Some(0)
        && requests.is_empty()
        && provider["external_fake_effects"] == json!([]);
    if report["status"] != "passed"
        || !matches!(
            report["runtime"].as_str(),
            Some("real Goose native loop over ACP" | "goose" | "goose-acp-spike")
        )
        || report["real_model_requests"].as_u64() != Some(0)
        || report["goose_binary_sha256"] != GOOSE_SHA256
        || report
            .get("goose_version")
            .is_some_and(|value| value != GOOSE_VERSION)
        || report
            .get("dotenv_loaded")
            .is_some_and(|value| value != false)
        || if rejected_before_start {
            report
                .get("production_data_used")
                .is_some_and(|value| value != false)
                || report
                    .get("host_binary_sha256")
                    .is_some_and(|value| value != host_digest)
        } else {
            report["production_data_used"] != false || report["host_binary_sha256"] != host_digest
        }
        || provider["provider"] != "deterministic loopback OpenAI streaming fixture"
        || provider["real_model_requests"].as_u64() != Some(0)
        || provider["remaining_replies"].as_u64() != Some(0)
        || provider["failures"] != json!([])
        || if let Some(reported) = report["provider_requests"].as_array() {
            reported != requests
        } else {
            !rejected_before_start
        }
    {
        return Err(io::Error::other(
            "invalid Goose scenario or deterministic Provider evidence",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
