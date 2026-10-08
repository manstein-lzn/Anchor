use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File},
    io,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};

use serde_json::{Value, json};

use crate::{TestCounts, digest, test_counts};

#[path = "candidate/evidence.rs"]
mod evidence;

const GOOSE_VERSION: &str = "1.53.0";
const GOOSE_SHA256: &str = "bdf35eb00d8dcc0218fe1150a3673446f351ea699ed579062628351f00cac340";
const DISTRIBUTION_TEST: &str =
    "extracted_goose_runtime_resumes_without_sources_or_replaying_workspace_effects";

#[derive(Debug)]
pub struct CandidateOptions {
    pub workspace_root: PathBuf,
    pub target_dir: PathBuf,
    pub evidence_root: Option<PathBuf>,
    pub goose: Option<PathBuf>,
}

fn configured_binary(options: &CandidateOptions) -> io::Result<PathBuf> {
    let path = options
        .goose
        .clone()
        .or_else(|| std::env::var_os("ANCHOR_GOOSE_BINARY").map(PathBuf::from))
        .or_else(|| std::env::var_os("ANCHOR_DISTRIBUTION_GOOSE").map(PathBuf::from))
        .ok_or_else(|| {
            io::Error::other(
                "candidate requires --goose, ANCHOR_GOOSE_BINARY or ANCHOR_DISTRIBUTION_GOOSE",
            )
        })?;
    if !path.is_absolute()
        || !path.is_file()
        || path.metadata()?.permissions().mode() & 0o111 == 0
        || digest(&path)? != GOOSE_SHA256
    {
        return Err(io::Error::other(format!(
            "candidate Goose must be an absolute executable pinned Goose {GOOSE_VERSION} x86_64 musl binary (SHA256 {GOOSE_SHA256})"
        )));
    }
    path.canonicalize()
}

fn evidence_root(requested: Option<&Path>) -> io::Result<PathBuf> {
    let root = match requested {
        Some(path) => {
            let path = std::path::absolute(path)?;
            fs::create_dir_all(
                path.parent()
                    .ok_or_else(|| io::Error::other("invalid evidence root"))?,
            )?;
            fs::create_dir(&path)?;
            path
        }
        None => tempfile::Builder::new()
            .prefix("anchor-production-candidate-")
            .tempdir()?
            .keep(),
    };
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    root.canonicalize()
}

fn build_environment(root: &Path, target: &Path) -> io::Result<BTreeMap<OsString, OsString>> {
    let original_home = std::env::var_os("HOME").map(PathBuf::from);
    let cargo_cache = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| original_home.as_ref().map(|path| path.join(".cargo")));
    let isolated_home = root.join("build-home");
    let cargo_home = root.join("cargo-home");
    let npm_user_config = root.join("npm-user.conf");
    let npm_global_config = root.join("npm-global.conf");
    fs::create_dir(&isolated_home)?;
    fs::create_dir(&cargo_home)?;
    for path in [&npm_user_config, &npm_global_config] {
        File::create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    if let Some(cache) = cargo_cache {
        for name in ["registry", "git"] {
            let path = cache.join(name);
            if path.is_dir() {
                symlink(path.canonicalize()?, cargo_home.join(name))?;
            }
        }
    }
    let mut environment = BTreeMap::new();
    for name in ["PATH", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN"] {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(name.into(), value);
        }
    }
    if !environment.contains_key(&OsString::from("RUSTUP_HOME"))
        && let Some(original_home) = original_home
    {
        environment.insert("RUSTUP_HOME".into(), original_home.join(".rustup").into());
    }
    for (name, value) in [
        ("HOME", isolated_home.into_os_string()),
        ("CARGO_HOME", cargo_home.into_os_string()),
        ("CARGO_TARGET_DIR", target.as_os_str().to_owned()),
        ("CARGO_NET_OFFLINE", "true".into()),
        ("CARGO_BUILD_JOBS", "2".into()),
        ("NPM_CONFIG_USERCONFIG", npm_user_config.into_os_string()),
        (
            "NPM_CONFIG_GLOBALCONFIG",
            npm_global_config.into_os_string(),
        ),
        ("LANG", "C.UTF-8".into()),
        ("TZ", "UTC".into()),
    ] {
        environment.insert(name.into(), value);
    }
    Ok(environment)
}

fn run_logged(
    argv: &[String],
    workspace: &Path,
    environment: &BTreeMap<OsString, OsString>,
    root: &Path,
    name: &str,
) -> io::Result<Value> {
    let stdout = root.join(format!("{name}.stdout.log"));
    let stderr = root.join(format!("{name}.stderr.log"));
    let started = Instant::now();
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(workspace)
        .env_clear()
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(&stdout)?))
        .stderr(Stdio::from(File::create(&stderr)?))
        .status();
    fs::set_permissions(&stdout, fs::Permissions::from_mode(0o600))?;
    fs::set_permissions(&stderr, fs::Permissions::from_mode(0o600))?;
    Ok(json!({
        "argv":argv, "cwd":workspace, "exit_code":status.as_ref().ok().and_then(|status|status.code()),
        "spawn_error":status.err().map(|error|error.to_string()),
        "stdout_log":stdout, "stderr_log":stderr,
        "stdout_sha256":digest(&stdout)?, "stderr_sha256":digest(&stderr)?,
        "elapsed_seconds":started.elapsed().as_secs_f64(),
    }))
}

fn cargo_command(manifest: &Path, test: bool) -> Vec<String> {
    let mut arguments = vec![
        "cargo".into(),
        "+stable".into(),
        if test { "test" } else { "build" }.into(),
        "--release".into(),
        "--locked".into(),
        "--offline".into(),
        "--manifest-path".into(),
        manifest.to_string_lossy().into_owned(),
        "-p".into(),
        "anchor-runner-host".into(),
    ];
    if test {
        arguments.extend(
            [
                "--test",
                "goose_distribution",
                "--",
                "--ignored",
                "--exact",
                DISTRIBUTION_TEST,
                "--test-threads=1",
                "--nocapture",
            ]
            .map(str::to_owned),
        );
    } else {
        arguments.extend(
            [
                "-p",
                "anchor-distribution",
                "-p",
                "anchor-scholarly",
                "-p",
                "anchor-wecom-gateway",
                "--bins",
            ]
            .map(str::to_owned),
        );
    }
    arguments
}

fn save_report(report: &Value, path: &Path) -> io::Result<()> {
    fs::write(path, serde_json::to_vec_pretty(report)?)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn checked_command(report: &mut Value, command: Value, path: &Path) -> io::Result<()> {
    let passed = command["exit_code"] == 0;
    let program = command["argv"][0].as_str().unwrap_or("command").to_owned();
    report["commands"].as_array_mut().unwrap().push(command);
    report["command_count"] = json!(report["commands"].as_array().unwrap().len());
    save_report(report, path)?;
    if passed {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{program} failed; inspect retained command logs"
        )))
    }
}

fn run_inner(options: &CandidateOptions) -> io::Result<Value> {
    let goose = configured_binary(options)?;
    let workspace = options.workspace_root.canonicalize()?;
    let manifest = workspace.join("rust/Cargo.toml");
    if !manifest.is_file() || !workspace.join("apps/web/package.json").is_file() {
        return Err(io::Error::other(
            "candidate workspace requires rust/Cargo.toml and apps/web/package.json",
        ));
    }
    let target = std::path::absolute(&options.target_dir)?;
    let root = evidence_root(options.evidence_root.as_deref())?;
    let report_path = root.join("report.json");
    let started = Instant::now();
    let mut report = json!({
        "status":"failed", "mode":"candidate", "evidence":report_path,
        "workspace_root":workspace, "target_dir":target,
        "goose_binary":goose, "goose_version":GOOSE_VERSION, "goose_binary_sha256":GOOSE_SHA256,
        "commands":[], "command_count":0, "tests":TestCounts::default(),
        "scenario_evidence":[], "scenario_evidence_count":0, "hashes":{},
        "local_model_requests":0, "real_model_requests":0,
        "production_data_used":false, "dotenv_loaded":false,
        "scope":"release Web/source-free Goose distribution with five local requests, stop/resume and completed-run restart; official binaries packaged without business calls",
        "failure":"candidate has not completed",
    });
    save_report(&report, &report_path)?;
    let executed = (|| -> io::Result<()> {
        let mut environment = build_environment(&root, &target)?;
        let web_root = workspace.join("apps/web");
        for name in [
            ".env",
            ".env.local",
            ".env.production",
            ".env.production.local",
        ] {
            if web_root.join(name).symlink_metadata().is_ok() {
                return Err(io::Error::other(format!(
                    "candidate refuses Web dotenv input: {name}"
                )));
            }
        }
        let web_command = ["npm", "--prefix", "apps/web", "run", "build"].map(str::to_owned);
        checked_command(
            &mut report,
            run_logged(&web_command, &workspace, &environment, &root, "web-build")?,
            &report_path,
        )?;
        let web_dist = web_root.join("dist");
        if !web_dist.join("index.html").is_file() {
            return Err(io::Error::other("Web production build output is missing"));
        }
        checked_command(
            &mut report,
            run_logged(
                &cargo_command(&manifest, false),
                &workspace,
                &environment,
                &root,
                "release-build",
            )?,
            &report_path,
        )?;
        for name in [
            "anchor-runner-host",
            "anchor-distribution",
            "anchor-scholarly",
            "anchor-wecom-gateway",
        ] {
            let binary = target.join("release").join(name);
            if !binary.is_file() || binary.metadata()?.permissions().mode() & 0o111 == 0 {
                return Err(io::Error::other(format!(
                    "missing release executable: {name}"
                )));
            }
            report["hashes"][name] = json!({"path":binary,"sha256":digest(&binary)?});
        }
        report["hashes"]["web_index"] = json!({"path":web_dist.join("index.html"),"sha256":digest(&web_dist.join("index.html"))?});
        let fixture = root.join("fixture");
        fs::create_dir(&fixture)?;
        for (name, value) in [
            ("ANCHOR_TEST_EVIDENCE_ROOT", fixture.as_path()),
            ("ANCHOR_GOOSE_BINARY", goose.as_path()),
            ("ANCHOR_TEST_WEB_DIST", web_dist.as_path()),
            (
                "ANCHOR_TEST_ACADEMIC_PLUGIN",
                workspace.join("plugins/academic-research").as_path(),
            ),
            (
                "ANCHOR_TEST_SCHOLARLY_BINARY",
                target.join("release/anchor-scholarly").as_path(),
            ),
            (
                "ANCHOR_TEST_WECOM_GATEWAY_BINARY",
                target.join("release/anchor-wecom-gateway").as_path(),
            ),
        ] {
            environment.insert(name.into(), value.as_os_str().to_owned());
        }
        let test = run_logged(
            &cargo_command(&manifest, true),
            &workspace,
            &environment,
            &root,
            "distribution-test",
        )?;
        let output = fs::read_to_string(root.join("distribution-test.stdout.log"))?;
        let counts = test_counts(&output);
        if let Ok(counts) = &counts {
            report["tests"] = serde_json::to_value(counts)?;
        }
        checked_command(&mut report, test, &report_path)?;
        evidence::validate_counts(&counts?)?;
        let scenarios = evidence::scenario_reports(&fixture, &target, &workspace, &web_dist)?;
        report["scenario_evidence_count"] = json!(scenarios.len());
        report["scenario_evidence"] = json!(scenarios);
        report["local_model_requests"] = json!(5);
        Ok(())
    })();
    let mut failure = executed.err().map(|error| error.to_string());
    if root.join("fixture").is_dir() {
        match evidence::observed_evidence(&root.join("fixture")) {
            Ok(observed) => {
                report["local_model_requests"] = observed["local_model_requests"].clone();
                report["observed_evidence"] = observed;
            }
            Err(error) => {
                report["local_model_requests"] = Value::Null;
                if failure.is_none() {
                    failure = Some(error.to_string());
                }
            }
        }
    }
    report["failure"] = json!(failure);
    report["status"] = json!(if report["failure"].is_null() {
        "passed"
    } else {
        "failed"
    });
    report["elapsed_seconds"] = json!(started.elapsed().as_secs_f64());
    save_report(&report, &report_path)?;
    Ok(report)
}

pub fn run(options: &CandidateOptions) -> Result<Value, String> {
    run_inner(options).map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "candidate/tests.rs"]
mod tests;
