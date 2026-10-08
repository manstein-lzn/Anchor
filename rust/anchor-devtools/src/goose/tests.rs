use super::*;

fn evidence() -> (Value, Value) {
    let requests = json!([{"model":"fixture-goose"}]);
    (
        json!({
            "status":"passed", "runtime":"real Goose native loop over ACP",
            "real_model_requests":0, "goose_version":GOOSE_VERSION,
            "goose_binary_sha256":GOOSE_SHA256, "host_binary_sha256":"host-digest",
            "production_data_used":false, "dotenv_loaded":false, "provider_requests":requests,
        }),
        json!({
            "provider":"deterministic loopback OpenAI streaming fixture",
            "real_model_requests":0, "requests":requests, "failures":[],
            "remaining_replies":0, "external_fake_effects":[],
        }),
    )
}

#[test]
fn commands_select_standard_ignored_goose_suites() {
    assert!(SUITES.contains(&"goose_compaction"));
    assert!(SUITES.contains(&"goose_pilot_compaction"));
    assert!(SUITES.contains(&"goose_trace"));
    assert!(SUITES.contains(&"goose_session_calls"));
    assert!(SUITES.contains(&"goose_library"));
    for suite in SUITES {
        let command = test_command("/workspace/rust/Cargo.toml", suite);
        assert!(command.windows(2).any(|pair| pair == ["--test", *suite]));
        assert!(command.contains(&"--no-default-features"));
        assert!(command.contains(&"--locked"));
        assert!(!command.contains(&"--features"));
        assert_eq!(
            &command[command
                .iter()
                .position(|argument| *argument == "--")
                .unwrap()..],
            &["--", "--ignored", "--test-threads=4", "--nocapture"]
        );
    }
}

#[test]
fn suite_accepts_filtered_helpers_but_not_missing_or_ignored_fixtures() {
    let counts = test_counts("test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0s").unwrap();
    assert!(validate_suite(&counts).is_ok());
    for counts in [
        TestCounts::default(),
        TestCounts {
            suites: 1,
            ..TestCounts::default()
        },
        TestCounts {
            suites: 1,
            passed: 1,
            ignored: 1,
            ..TestCounts::default()
        },
        TestCounts {
            suites: 1,
            passed: 1,
            failed: 1,
            failed_suites: 1,
            ..TestCounts::default()
        },
    ] {
        assert!(validate_suite(&counts).is_err());
    }
}

#[test]
fn accepts_native_pilot_and_explicit_pre_admission_rejection_shapes() {
    let (mut report, mut provider) = evidence();
    assert!(validate_report(&report, &provider, "host-digest").is_ok());
    report["runtime"] = json!("goose");
    report.as_object_mut().unwrap().remove("dotenv_loaded");
    assert!(validate_report(&report, &provider, "host-digest").is_ok());
    report = json!({
        "status":"passed", "runtime":"goose-acp-spike", "real_model_requests":0,
        "goose_binary_sha256":GOOSE_SHA256, "provider_requests":0,
        "response":{"kind":"rejected"}, "graph_run_admitted":false,
        "goose_process_started":false, "runtime_shared_network_explicitly_authorized":false,
    });
    provider["requests"] = json!([]);
    assert!(validate_report(&report, &provider, "host-digest").is_ok());
    report["goose_process_started"] = json!(true);
    assert!(validate_report(&report, &provider, "host-digest").is_err());
}

#[test]
fn rejects_non_fixture_unbound_incomplete_or_real_model_evidence() {
    let (report, provider) = evidence();
    for (field, value) in [
        ("status", json!("failed")),
        ("runtime", json!("unknown-runtime")),
        ("real_model_requests", json!(1)),
        ("production_data_used", json!(true)),
        ("dotenv_loaded", json!(true)),
        ("host_binary_sha256", json!("old-host")),
        ("goose_binary_sha256", json!("unfixed")),
        ("goose_version", json!("unknown")),
        ("provider_requests", json!([])),
    ] {
        let mut changed = report.clone();
        changed[field] = value;
        assert!(
            validate_report(&changed, &provider, "host-digest").is_err(),
            "{field}"
        );
    }
    for field in [
        "production_data_used",
        "real_model_requests",
        "status",
        "host_binary_sha256",
    ] {
        let mut changed = report.clone();
        changed.as_object_mut().unwrap().remove(field);
        assert!(
            validate_report(&changed, &provider, "host-digest").is_err(),
            "{field}"
        );
    }
    for (field, value) in [
        ("provider", json!("remote")),
        ("real_model_requests", json!(1)),
        ("remaining_replies", json!(1)),
        ("failures", json!(["feedback mismatch"])),
        ("requests", Value::Null),
    ] {
        let mut changed = provider.clone();
        changed[field] = value;
        assert!(
            validate_report(&report, &changed, "host-digest").is_err(),
            "{field}"
        );
    }
}

#[test]
fn nested_evidence_requires_actual_provider_files_and_current_host() {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("host");
    fs::write(&binary, b"current host").unwrap();
    let fixture = root.path().join("fixture");
    let scenario = fixture.join("123/goose-test");
    fs::create_dir_all(&scenario).unwrap();
    let (mut report, provider) = evidence();
    report["host_binary_sha256"] = json!(digest(&binary).unwrap());
    fs::write(
        scenario.join("evidence.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    assert!(scenario_reports(&fixture, &binary, "goose_acp").is_err());
    fs::write(
        scenario.join("provider.json"),
        serde_json::to_vec(&provider).unwrap(),
    )
    .unwrap();
    let reports = scenario_reports(&fixture, &binary, "goose_acp").unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["scenario"], "goose-test");
    fs::write(&binary, b"different host").unwrap();
    assert!(scenario_reports(&fixture, &binary, "goose_acp").is_err());
    fs::write(&binary, b"current host").unwrap();
    let duplicate = fixture.join("456/goose-test");
    fs::create_dir_all(&duplicate).unwrap();
    fs::copy(
        scenario.join("provider.json"),
        duplicate.join("provider.json"),
    )
    .unwrap();
    fs::copy(
        scenario.join("evidence.json"),
        duplicate.join("evidence.json"),
    )
    .unwrap();
    assert!(scenario_reports(&fixture, &binary, "goose_acp").is_err());
}
