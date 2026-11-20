use super::*;

fn scenario() -> (Value, Value) {
    let requests = json!(vec![json!({"model":"fixture"}); 4]);
    let hash = format!("{:x}", Sha256::digest(b"once"));
    let mut report = json!({
        "status":"passed","runtime":"real Goose native loop over ACP","real_model_requests":0,
        "production_data_used":false,"dotenv_loaded":false,"goose_version":GOOSE_VERSION,
        "goose_binary_sha256":GOOSE_SHA256,"host_binary_sha256":"current-host",
        "provider_requests":requests,"run":{"run_id":"fixture-run","results":{"worker":[{}],"verify":[{}]}},
        "native_goose_conversation":[{"content":[{"type":"toolRequest","toolCall":{"name":"anchor_run","arguments":"printf once > effect.txt"}},{"type":"toolRequest","toolCall":{"name":"final_result"}}]}],
        "workspace_files":[{"path":"worker/effect.txt","bytes":4,"sha256":hash,"text":"once"},{"path":"verify/verified.txt","bytes":4,"sha256":hash,"text":"once"}],
        "checks":{"session_before":"same-session","session_after":"same-session","session_after_completed_restart":"same-session",
            "workspace_write_count":1,"requests_before_stop":2,"requests_before_completed_restart":4,"requests_after_completed_restart":4,
            "stopped_run_http":{"run":"fixture-run","projection":{"phase":"waiting","waiting_for":"human","reason":"stopped"}},
            "run_http":{"run":"fixture-run","projection":{"phase":"settled","outcome":"done"},"state":{"nodes":{"worker":{"submitted":true},"verify":{"submitted":true}}}},
            "artifact_files":{"worker":{"path":"effect.txt","size":4,"text":"once","binary":false,"truncated":false},
                "verify":{"path":"verified.txt","size":4,"text":"once","binary":false,"truncated":false}},
            "packaged_paths":["anchor-runtime/bin/goose"]}
    });
    for field in REQUIRED_CHECKS {
        report["checks"][*field] = json!(true);
    }
    (
        report,
        json!({"provider":"deterministic loopback OpenAI streaming fixture","real_model_requests":0,"requests":requests,"remaining_replies":0,"failures":[],"external_fake_effects":[]}),
    )
}

#[test]
fn only_one_executed_distribution_test_can_pass() {
    for summary in [
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s",
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0s",
    ] {
        assert!(validate_counts(&test_counts(summary).unwrap()).is_ok());
    }
    for summary in [
        "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0s",
        "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0s",
        "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s",
        "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0s",
        "",
    ] {
        assert!(validate_counts(&test_counts(summary).unwrap()).is_err());
    }
}

#[test]
fn missing_checks_changed_sessions_or_extra_effects_fail_closed() {
    let (report, provider) = scenario();
    assert_eq!(
        validate_report(&report, &provider, "current-host").unwrap(),
        4
    );
    for field in REQUIRED_CHECKS {
        let mut changed = report.clone();
        changed["checks"].as_object_mut().unwrap().remove(*field);
        assert!(
            validate_report(&changed, &provider, "current-host").is_err(),
            "{field}"
        );
    }
    for (field, value) in [
        ("session_after", json!("different-session")),
        ("session_after_completed_restart", Value::Null),
        ("workspace_write_count", json!(2)),
        ("stopped_run_http", Value::Null),
        ("requests_before_stop", Value::Null),
        ("requests_before_stop", json!(1)),
        ("requests_after_completed_restart", json!(5)),
        ("artifact_files", Value::Null),
        ("packaged_paths", json!(["anchor-runtime/src/main.rs"])),
    ] {
        let mut changed = report.clone();
        changed["checks"][field] = value;
        assert!(
            validate_report(&changed, &provider, "current-host").is_err(),
            "{field}"
        );
    }
    for field in ["native_goose_conversation", "workspace_files"] {
        let mut changed = report.clone();
        changed[field] = json!([]);
        assert!(
            validate_report(&changed, &provider, "current-host").is_err(),
            "{field}"
        );
    }
    assert!(validate_report(&report, &provider, "stale-host").is_err());
}

#[test]
fn old_or_extra_request_counts_and_incomplete_provider_scripts_are_rejected() {
    let (report, provider) = scenario();
    for count in [3, 5] {
        let mut changed_report = report.clone();
        let mut changed_provider = provider.clone();
        let requests = json!(vec![json!({"model":"fixture"}); count]);
        changed_report["provider_requests"] = requests.clone();
        changed_provider["requests"] = requests;
        changed_report["checks"]["requests_before_completed_restart"] = json!(count);
        changed_report["checks"]["requests_after_completed_restart"] = json!(count);
        assert!(validate_report(&changed_report, &changed_provider, "current-host").is_err());
    }
    for (field, value) in [
        ("remaining_replies", json!(1)),
        ("failures", json!(["fixture mismatch"])),
        ("real_model_requests", json!(1)),
        ("external_fake_effects", json!([{"kind":"unexpected"}])),
    ] {
        let mut changed = provider.clone();
        changed[field] = value;
        assert!(
            validate_report(&report, &changed, "current-host").is_err(),
            "{field}"
        );
    }
}

#[test]
fn source_and_escaping_archive_members_are_rejected() {
    for path in [
        "/anchor-runtime/bin/goose",
        "anchor-runtime/../escape",
        "anchor-runtime/.env.local",
        "anchor-runtime/server.py",
        "anchor-runtime/__pycache__/cache.pyc",
        "anchor-runtime/node_modules/package.json",
        "anchor-runtime/Cargo.toml",
    ] {
        assert!(!source_free_path(path), "{path}");
    }
    assert!(source_free_path("anchor-runtime/web/assets/index.js"));
}

#[test]
fn absent_or_duplicate_scenario_evidence_is_not_a_pass() {
    let root = tempfile::tempdir().unwrap();
    for count in 0..=2 {
        if count > 0 {
            let path = root.path().join(count.to_string());
            fs::create_dir(&path).unwrap();
            fs::write(path.join("evidence.json"), b"{}").unwrap();
        }
        assert!(
            scenario_reports(
                root.path(),
                Path::new("/missing-target"),
                Path::new("/missing-workspace"),
                Path::new("/missing-web")
            )
            .is_err()
        );
    }
}

#[test]
fn partial_fixture_evidence_counts_actual_requests_without_approving_the_scenario() {
    let root = tempfile::tempdir().unwrap();
    let provider = root.path().join("provider.json");
    fs::write(&provider, br#"{"requests":[{},{}],"remaining_replies":1}"#).unwrap();
    let observed = observed_evidence(root.path()).unwrap();
    assert_eq!(observed["local_model_requests"], 2);
    assert_eq!(observed["provider_report_count"], 1);
    assert_eq!(observed["scenario_report_count"], 0);
    assert_eq!(
        observed["provider_reports"][0]["sha256"],
        digest(&provider).unwrap()
    );
    assert!(
        scenario_reports(
            root.path(),
            Path::new("/missing-target"),
            Path::new("/missing-workspace"),
            Path::new("/missing-web")
        )
        .is_err()
    );
}

#[test]
fn missing_or_changed_official_binary_and_web_hashes_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let binaries = root.path().join("release-binaries");
    let web = root.path().join("web");
    fs::create_dir(&binaries).unwrap();
    fs::create_dir(&web).unwrap();
    fs::write(web.join("index.html"), b"built Web").unwrap();
    fs::create_dir(web.join("assets")).unwrap();
    let mut files = Vec::new();
    let mut executables = Vec::new();
    for (name, path) in [
        ("anchor-runner-host", "bin/anchor-runner-host"),
        ("anchor-net-relay", "bin/anchor-net-relay"),
        ("anchor-sandbox-network", "bin/anchor-sandbox-network"),
        (
            "anchor-scholarly",
            "plugins/academic-research/bin/anchor-scholarly",
        ),
        ("anchor-wecom-gateway", "bin/anchor-wecom-gateway"),
    ] {
        let binary = binaries.join(name);
        fs::write(&binary, name.as_bytes()).unwrap();
        let hash = digest(&binary).unwrap();
        files.push(json!({"path":path,"sha256":hash,"size":name.len()}));
        executables.push(json!({"path":path,"sha256":hash}));
    }
    files.push(json!({"path":"bin/goose","sha256":GOOSE_SHA256,"size":1}));
    executables.push(json!({"path":"bin/goose","sha256":GOOSE_SHA256}));
    files.push(
        json!({"path":"web/index.html","sha256":digest(&web.join("index.html")).unwrap(),"size":9}),
    );
    let mut served_assets = Vec::new();
    for name in ["app.js", "app.css"] {
        let path = format!("assets/{name}");
        fs::write(web.join(&path), name.as_bytes()).unwrap();
        let hash = digest(&web.join(&path)).unwrap();
        files.push(json!({"path":format!("web/{path}"),"sha256":hash,"size":name.len()}));
        served_assets.push(json!({"path":path,"sha256":hash,"bytes":name.len()}));
    }
    let manifest = json!({"format":1,"goose_version":GOOSE_VERSION,"goose_sha256":GOOSE_SHA256,"files":files,"executables":executables});
    let (mut report, _) = scenario();
    report["checks"]["web_index_sha256"] = json!(digest(&web.join("index.html")).unwrap());
    report["checks"]["web_http"] = json!({"http_success":true,"index_sha256":report["checks"]["web_index_sha256"],"assets":served_assets});
    report["checks"]["packaged_paths"] = json!(
        files
            .iter()
            .map(|file| format!("anchor-runtime/{}", file["path"].as_str().unwrap()))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        validate_manifest(&manifest, &report, &binaries, &web).unwrap(),
        files.len()
    );
    let evidence_root = root.path().join("fixture");
    let workspace = root.path().join("workspace");
    let sources = workspace.join("rust/anchor-runner-host/tests");
    fs::create_dir(&evidence_root).unwrap();
    fs::create_dir_all(sources.join("support")).unwrap();
    fs::write(
        sources.join("goose_distribution.rs"),
        b"current distribution fixture",
    )
    .unwrap();
    fs::write(
        sources.join("support/goose_fixture.rs"),
        b"current Provider fixture",
    )
    .unwrap();
    let archive = evidence_root.join("anchor-runtime.tar.gz");
    fs::write(&archive, b"archive evidence").unwrap();
    report["checks"]["runtime_manifest"] = manifest.clone();
    report["checks"]["archive_sha256"] = json!(digest(&archive).unwrap());
    let binary = binaries.join("anchor-runner-host");
    report["checks"]["test_binary"] = json!(binary);
    report["test_binary_sha256"] = json!(digest(&binary).unwrap());
    report["host_binary_sha256"] = json!(digest(&binary).unwrap());
    report["test_source_sha256"] = json!(digest(&sources.join("goose_distribution.rs")).unwrap());
    report["fixture_source_sha256"] =
        json!(digest(&sources.join("support/goose_fixture.rs")).unwrap());
    let (_, provider) = scenario();
    fs::write(
        evidence_root.join("provider.json"),
        serde_json::to_vec(&provider).unwrap(),
    )
    .unwrap();
    fs::write(
        evidence_root.join("runtime-manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        evidence_root.join("evidence.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let accepted = scenario_reports(&evidence_root, &binaries, &workspace, &web).unwrap();
    assert_eq!(accepted.len(), 1);
    assert_eq!(accepted[0]["local_model_requests"], 4);
    assert_eq!(accepted[0]["archive_sha256"], digest(&archive).unwrap());
    let mut missing_http = report.clone();
    missing_http["checks"]
        .as_object_mut()
        .unwrap()
        .remove("web_http");
    assert!(validate_manifest(&manifest, &missing_http, &binaries, &web).is_err());
    fs::write(&archive, b"changed archive").unwrap();
    assert!(scenario_reports(&evidence_root, &binaries, &workspace, &web).is_err());
    fs::remove_file(&archive).unwrap();
    assert!(scenario_reports(&evidence_root, &binaries, &workspace, &web).is_err());
    let mut changed = manifest.clone();
    changed["files"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|file| file["path"] == "web/index.html")
        .unwrap()["sha256"] = json!(GOOSE_SHA256);
    assert!(validate_manifest(&changed, &report, &binaries, &web).is_err());
    fs::write(web.join("index.html"), b"different Web").unwrap();
    assert!(validate_manifest(&manifest, &report, &binaries, &web).is_err());
    fs::remove_file(binaries.join("anchor-scholarly")).unwrap();
    assert!(validate_manifest(&manifest, &report, &binaries, &web).is_err());
}
