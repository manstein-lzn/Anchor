use super::*;
use sha2::{Digest, Sha256};

const REQUIRED_CHECKS: &[&str] = &[
    "actual_extracted_binaries",
    "unpackaged_bundle_unavailable",
    "source_free",
    "package_files_verified",
    "readiness_checked",
    "web_served",
    "node_history_checked",
    "native_history_checked",
    "artifact_exact_bytes",
    "workspace_exact_bytes",
    "same_native_session",
    "workspace_effect_once",
    "readonly_input",
    "completed_restart_unchanged",
    "official_binaries_packaged",
];

pub(super) fn validate_counts(counts: &TestCounts) -> io::Result<()> {
    if counts.suites != 1
        || counts.passed != 1
        || counts.failed_suites != 0
        || counts.failed != 0
        || counts.ignored != 0
    {
        return Err(io::Error::other(
            "candidate distribution test was missing, failed or ignored",
        ));
    }
    Ok(())
}

fn require(condition: bool, message: &str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message))
    }
}

fn array<'value>(value: &'value Value, message: &str) -> io::Result<&'value Vec<Value>> {
    value.as_array().ok_or_else(|| io::Error::other(message))
}

fn valid_hash(value: &Value) -> bool {
    value.as_str().is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn source_free_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            let std::path::Component::Normal(name) = component else {
                return false;
            };
            let name = name.to_string_lossy();
            !matches!(
                name.as_ref(),
                "src"
                    | "Cargo.toml"
                    | "Cargo.lock"
                    | ".env"
                    | ".git"
                    | ".venv"
                    | "node_modules"
                    | "__pycache__"
            ) && !name.starts_with(".env.")
                && ![".rs", ".py", ".pyc", ".pyo"]
                    .iter()
                    .any(|suffix| name.ends_with(suffix))
        })
}

fn validate_report(report: &Value, provider: &Value, host_hash: &str) -> io::Result<()> {
    let requests = array(
        &provider["requests"],
        "missing deterministic Provider requests",
    )?;
    require(
        report["status"] == "passed"
            && report["runtime"] == "real Goose native loop over ACP"
            && report["real_model_requests"].as_u64() == Some(0)
            && report["production_data_used"] == false
            && report["dotenv_loaded"] == false
            && report["goose_version"] == GOOSE_VERSION
            && report["goose_binary_sha256"] == GOOSE_SHA256
            && report["host_binary_sha256"] == host_hash
            && report["provider_requests"] == provider["requests"]
            && provider["provider"] == "deterministic loopback OpenAI streaming fixture"
            && provider["real_model_requests"].as_u64() == Some(0)
            && provider["remaining_replies"].as_u64() == Some(0)
            && provider["failures"] == json!([])
            && provider["external_fake_effects"] == json!([])
            && requests.len() == 5,
        "invalid candidate runtime or deterministic Provider evidence",
    )?;
    let checks = &report["checks"];
    for field in REQUIRED_CHECKS {
        require(
            checks[*field] == true,
            &format!("missing candidate check: {field}"),
        )?;
    }
    require(
        checks["session_before"]
            .as_str()
            .is_some_and(|session| !session.is_empty())
            && checks["session_before"] == checks["session_after"]
            && checks["session_after"] == checks["session_after_completed_restart"]
            && checks["workspace_write_count"].as_u64() == Some(1)
            && checks["requests_before_completed_restart"].as_u64() == Some(5)
            && checks["requests_after_completed_restart"].as_u64() == Some(5)
            && checks["run_http"]["state"]["status"] == "completed",
        "candidate session, side effect or restart evidence is incomplete",
    )?;
    for node in ["worker", "verify"] {
        require(
            array(
                &report["run"]["results"][node],
                "missing persisted node results",
            )?
            .len()
                == 1
                && checks["run_http"]["state"]["nodes"][node]["submitted"] == true,
            "candidate node history must contain one completed invocation per node",
        )?;
    }
    let history = array(
        &report["native_goose_conversation"],
        "missing native Goose history",
    )?;
    let writes = history
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|content| {
            content["type"] == "toolRequest"
                && content["toolCall"]
                    .to_string()
                    .contains("printf once > effect.txt")
        })
        .count();
    require(
        writes == 1
            && report["native_goose_conversation"]
                .to_string()
                .contains("final_result"),
        "native history lacks the single side effect and completion",
    )?;
    let once_hash = format!("{:x}", Sha256::digest(b"once"));
    for (inventory, filenames) in [
        ("workspace_files", vec!["effect.txt", "verified.txt"]),
        (
            "state_files",
            vec!["files/effect.txt", "files/verified.txt"],
        ),
    ] {
        let files = array(&report[inventory], "missing workspace/Artifact inventory")?;
        for filename in filenames {
            require(
                files.iter().any(|file| {
                    file["path"]
                        .as_str()
                        .is_some_and(|path| path.ends_with(filename))
                        && file["bytes"].as_u64() == Some(4)
                        && file["sha256"] == once_hash
                        && file["text"] == "once"
                }),
                "workspace/Artifact bytes or hashes are missing",
            )?;
        }
    }
    let paths = array(
        &checks["packaged_paths"],
        "missing archive member inventory",
    )?;
    require(
        !paths.is_empty()
            && paths.iter().all(|path| {
                path.as_str().is_some_and(|path| {
                    path.starts_with("anchor-runtime/") && source_free_path(path)
                })
            }),
        "candidate archive contains sources or unsafe paths",
    )
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        require(
            !kind.is_symlink(),
            "candidate evidence cannot contain symlinks",
        )?;
        if kind.is_dir() {
            collect_files(&entry.path(), files)?;
        } else {
            require(
                kind.is_file(),
                "candidate evidence cannot contain special files",
            )?;
            files.push(entry.path());
        }
    }
    Ok(())
}

fn read_json(path: &Path) -> io::Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

pub(super) fn observed_evidence(root: &Path) -> io::Result<Value> {
    let mut paths = Vec::new();
    collect_files(root, &mut paths)?;
    paths.sort();
    let mut scenarios = Vec::new();
    let mut providers = Vec::new();
    let mut requests = 0;
    for path in paths {
        if !path
            .file_name()
            .is_some_and(|name| name == "evidence.json" || name == "provider.json")
        {
            continue;
        }
        let record = json!({"path":path,"sha256":digest(&path)?});
        if path.file_name().is_some_and(|name| name == "evidence.json") {
            scenarios.push(record);
        } else if path.file_name().is_some_and(|name| name == "provider.json") {
            requests += array(
                &read_json(&path)?["requests"],
                "missing observed Provider requests",
            )?
            .len();
            providers.push(record);
        }
    }
    Ok(json!({
        "scenario_report_count":scenarios.len(),"provider_report_count":providers.len(),
        "local_model_requests":requests,"scenario_reports":scenarios,"provider_reports":providers,
    }))
}

fn validate_manifest(
    manifest: &Value,
    report: &Value,
    target: &Path,
    web_dist: &Path,
) -> io::Result<usize> {
    require(
        manifest["format"] == 1
            && manifest["goose_version"] == GOOSE_VERSION
            && manifest["goose_sha256"] == GOOSE_SHA256,
        "invalid Runtime manifest identity",
    )?;
    let files = array(&manifest["files"], "missing packaged file hashes")?;
    let mut paths = std::collections::BTreeSet::new();
    for file in files {
        let path = file["path"]
            .as_str()
            .ok_or_else(|| io::Error::other("missing packaged path"))?;
        require(
            source_free_path(path)
                && paths.insert(path)
                && valid_hash(&file["sha256"])
                && file["size"].as_u64().is_some(),
            "invalid packaged file inventory",
        )?;
        require(
            array(&report["checks"]["packaged_paths"], "missing archive paths")?
                .contains(&json!(format!("anchor-runtime/{path}"))),
            "manifest member is missing from archive",
        )?;
    }
    for (path, expected) in [
        (
            "bin/anchor-runner-host",
            digest(&target.join("release/anchor-runner-host"))?,
        ),
        ("bin/goose", GOOSE_SHA256.into()),
        (
            "bundle/plugins/academic-research/bin/anchor-scholarly",
            digest(&target.join("release/anchor-scholarly"))?,
        ),
        (
            "bin/anchor-wecom-gateway",
            digest(&target.join("release/anchor-wecom-gateway"))?,
        ),
    ] {
        let executable = array(
            &manifest["executables"],
            "missing packaged executable identities",
        )?
        .iter()
        .find(|entry| entry["path"] == path);
        require(
            executable.is_some_and(|entry| entry["sha256"] == expected)
                && files
                    .iter()
                    .any(|entry| entry["path"] == path && entry["sha256"] == expected),
            "official packaged executable does not match release bytes",
        )?;
    }
    let mut web_files = Vec::new();
    collect_files(web_dist, &mut web_files)?;
    require(!web_files.is_empty(), "missing built Web files")?;
    let mut expected_web = std::collections::BTreeSet::new();
    for file in web_files {
        let path = format!(
            "web/{}",
            file.strip_prefix(web_dist).unwrap().to_string_lossy()
        );
        expected_web.insert(path.clone());
        let hash = digest(&file)?;
        require(
            files.iter().any(|entry| {
                entry["path"] == path
                    && entry["sha256"] == hash
                    && entry["size"].as_u64() == Some(file.metadata().unwrap().len())
            }),
            "packaged Web does not match production build",
        )?;
    }
    let packaged_web = paths
        .iter()
        .filter(|path| path.starts_with("web/"))
        .map(|path| (*path).to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    require(
        packaged_web == expected_web,
        "packaged Web inventory differs from production build",
    )?;
    require(
        report["checks"]["web_index_sha256"] == digest(&web_dist.join("index.html"))?,
        "served Web index does not match production build",
    )?;
    let web_http = &report["checks"]["web_http"];
    require(
        web_http["http_success"] == true
            && web_http["index_sha256"] == report["checks"]["web_index_sha256"],
        "missing successful Web HTTP evidence",
    )?;
    let assets = array(&web_http["assets"], "missing served Web assets")?;
    for suffix in [".js", ".css"] {
        require(
            assets.iter().any(|asset| {
                asset["path"].as_str().is_some_and(|path| {
                    path.starts_with("assets/")
                        && path.ends_with(suffix)
                        && files.iter().any(|file| {
                            file["path"] == format!("web/{path}")
                                && file["sha256"] == asset["sha256"]
                                && file["size"] == asset["bytes"]
                        })
                })
            }),
            "served JavaScript/CSS bytes do not match packaged Web",
        )?;
    }
    Ok(files.len())
}

pub(super) fn scenario_reports(
    root: &Path,
    target: &Path,
    workspace: &Path,
    web_dist: &Path,
) -> io::Result<Vec<Value>> {
    let mut paths = Vec::new();
    collect_files(root, &mut paths)?;
    let reports = paths
        .into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name == "evidence.json"))
        .collect::<Vec<_>>();
    require(
        reports.len() == 1,
        "candidate requires exactly one distribution scenario evidence report",
    )?;
    let path = &reports[0];
    let parent = path.parent().unwrap();
    let provider_path = parent.join("provider.json");
    let archive_path = parent.join("anchor-runtime.tar.gz");
    let manifest_path = parent.join("runtime-manifest.json");
    let report = read_json(path)?;
    validate_report(
        &report,
        &read_json(&provider_path)?,
        &digest(&target.join("release/anchor-runner-host"))?,
    )?;
    require(
        report["test_source_sha256"]
            == digest(&workspace.join("rust/anchor-runner-host/tests/goose_distribution.rs"))?
            && report["fixture_source_sha256"]
                == digest(
                    &workspace.join("rust/anchor-runner-host/tests/support/goose_fixture.rs"),
                )?,
        "candidate evidence comes from different fixture sources",
    )?;
    let test_binary = report["checks"]["test_binary"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing test binary path"))?;
    require(
        report["test_binary_sha256"] == digest(Path::new(test_binary))?,
        "candidate test binary hash is stale",
    )?;
    let manifest = read_json(&manifest_path)?;
    require(
        manifest == report["checks"]["runtime_manifest"]
            && report["checks"]["archive_sha256"] == digest(&archive_path)?,
        "candidate archive/manifest hashes are missing or mismatched",
    )?;
    let file_count = validate_manifest(&manifest, &report, target, web_dist)?;
    Ok(vec![json!({
        "path":path,"sha256":digest(path)?, "provider_path":provider_path,"provider_sha256":digest(&provider_path)?,
        "archive_path":archive_path,"archive_sha256":digest(&archive_path)?,
        "manifest_path":manifest_path,"manifest_sha256":digest(&manifest_path)?,
        "package_file_count":file_count,"status":"passed","local_model_requests":5,"real_model_requests":0,
    })])
}

#[cfg(test)]
#[path = "evidence/tests.rs"]
mod tests;
