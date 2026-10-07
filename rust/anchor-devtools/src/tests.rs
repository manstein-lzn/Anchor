use super::*;

#[test]
fn fixture_commands_select_only_the_complete_runtime_and_exact_native_fixtures() {
    let [runtime, plugins] = fixture_test_commands("/tmp/workspace/rust/Cargo.toml");
    assert_eq!(
        &runtime[runtime
            .iter()
            .position(|argument| *argument == "--test")
            .unwrap()..],
        &[
            "--test",
            "runtime_contract",
            "--",
            "--test-threads=4",
            "--nocapture"
        ]
    );
    assert_eq!(
        &plugins[plugins
            .iter()
            .position(|argument| *argument == "--")
            .unwrap()..],
        &[
            "--",
            "--exact",
            "rust_wecom_stdio_plugin_runs_through_host_harness_and_readonly_package",
            "rust_docmost_stdio_plugin_uploads_only_frozen_input_through_real_sandbox",
            "--test-threads=4",
            "--nocapture",
        ]
    );
    for arguments in [runtime, plugins] {
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--features", "legacy-regression"])
        );
        assert!(!arguments.iter().any(|argument| argument.contains("goose")));
        assert!(!arguments.contains(&"--include-ignored"));
        assert!(!arguments.contains(&"--ignored"));
    }
}

#[test]
fn native_fixture_selection_accepts_intentionally_filtered_goose_tests() {
    let counts = test_counts("test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 1s").unwrap();
    assert!(validate_fixture_suite(&counts, true).is_ok());
    assert!(validate_fixture_suite(&counts, false).is_err());
}

#[test]
fn fixture_selection_still_rejects_ignored_missing_or_failed_tests() {
    for summary in [
        "test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 1s",
        "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 1s",
        "test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 1s",
        "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 1 filtered out; finished in 1s",
    ] {
        assert!(validate_fixture_suite(&test_counts(summary).unwrap(), true).is_err());
    }
    assert!(validate_fixture_suite(&TestCounts::default(), false).is_err());
    assert!(validate_fixture_suite(&TestCounts::default(), true).is_err());
}

#[test]
fn parses_both_full_suite_summaries() {
    assert_eq!(
        test_counts("test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1s\ntest result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1s\n").unwrap(),
        TestCounts { suites: 2, passed: 23, ..TestCounts::default() }
    );
}

#[test]
fn records_ignored_and_filtered_tests_instead_of_hiding_them() {
    let counts = test_counts("test result: ok. 2 passed; 0 failed; 1 ignored; 0 measured; 4 filtered out; finished in 1s").unwrap();
    assert_eq!(counts.ignored, 1);
    assert_eq!(counts.filtered, 4);
}

#[test]
fn rejects_failed_or_malformed_test_summaries() {
    assert!(test_counts("test result: UNKNOWN. 0 passed; 1 failed;").is_err());
    assert!(test_counts("test result: ok. unknown passed;").is_err());
    assert!(test_counts("test result: ok. 1 passed;").is_err());
    assert!(
        test_counts("test result: ok. 1 passed; 1 passed; 0 ignored; 0 measured; 0 filtered out;")
            .is_err()
    );
}

#[test]
fn retains_actual_failed_suite_counts_for_diagnostics() {
    let counts = test_counts("test result: FAILED. 3 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1s").unwrap();
    assert_eq!(counts.passed, 3);
    assert_eq!(counts.failed, 1);
    assert_eq!(counts.failed_suites, 1);
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Value) {
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("host");
    fs::write(&binary, b"fixed host identity").unwrap();
    let evidence = root.path().join("scenario.json");
    let report = json!({
        "status":"passed", "scenario":"small-graph",
        "provider":"deterministic loopback fixture",
        "production_data_used":false, "dotenv_loaded":false,
        "host_binary_sha256":digest(&binary).unwrap(),
        "test_binary_sha256":"f".repeat(64)
    });
    (root, binary, evidence, report)
}

#[test]
fn accepts_evidence_bound_to_the_actual_host_binary() {
    let (root, binary, evidence, report) = fixture();
    fs::write(evidence, serde_json::to_vec(&report).unwrap()).unwrap();
    let reports = fixture_reports(root.path(), &binary).unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["scenario"], "small-graph");
}

#[test]
fn rejects_missing_corrupt_failed_or_remote_evidence() {
    let (root, binary, evidence, report) = fixture();
    assert!(fixture_reports(root.path(), &binary).is_err());
    fs::write(&evidence, b"not json").unwrap();
    assert!(fixture_reports(root.path(), &binary).is_err());
    for (field, value) in [
        ("status", json!("failed")),
        ("provider", json!("remote")),
        ("production_data_used", json!(true)),
        ("dotenv_loaded", json!(true)),
        ("host_binary_sha256", json!("different build")),
        ("scenario", json!("")),
        ("test_binary_sha256", json!("unknown")),
    ] {
        let mut altered = report.clone();
        altered[field] = value;
        fs::write(&evidence, serde_json::to_vec(&altered).unwrap()).unwrap();
        assert!(fixture_reports(root.path(), &binary).is_err(), "{field}");
    }
}

#[test]
fn rejects_duplicate_scenario_evidence() {
    let (root, binary, evidence, report) = fixture();
    fs::write(evidence, serde_json::to_vec(&report).unwrap()).unwrap();
    fs::write(
        root.path().join("duplicate.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    assert!(fixture_reports(root.path(), &binary).is_err());
}
