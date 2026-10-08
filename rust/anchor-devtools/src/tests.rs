use super::*;

#[test]
fn parses_multiple_suite_summaries() {
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
