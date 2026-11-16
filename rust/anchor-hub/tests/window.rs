#![allow(dead_code)]
mod support;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anchor_facts::window::{
    ANALYZER_VERSION, EvidenceKey, EvidenceRequest, EvidenceResponse, MAX_EVIDENCE_KEYS,
    MAX_TRANSPORT_BYTES, ResourceScope, SCHEMA_VERSION, WindowRequest, WindowResponse,
    WindowSource, source_fingerprint,
};
use anchor_hub::window::{capture, evidence, signal_page};
use anchor_kernel::{Action, Resource, Submission};
use serde_json::{Value, json};
use support::{SECRET, World};

fn request(world: &World, names: &[&str]) -> WindowRequest {
    WindowRequest {
        safe_views: Vec::new(),
        support_history: false,
        schema_version: SCHEMA_VERSION,
        sources: names
            .iter()
            .map(|name| WindowSource {
                source_instance: (*name).into(),
                join_namespace: "fixture-install".into(),
                root: world.root(name),
                scope: ResourceScope::All {},
            })
            .collect(),
        manifest: None,
        advance_from: None,
        exclude_runs: Vec::new(),
        now_ms: 2_000_000,
        limit: 20,
    }
}

fn replay(request: &WindowRequest, response: &WindowResponse) -> WindowRequest {
    WindowRequest {
        manifest: Some(response.manifest.clone()),
        advance_from: None,
        ..request.clone()
    }
}

fn advance(request: &WindowRequest, response: &WindowResponse) -> WindowRequest {
    WindowRequest {
        manifest: None,
        advance_from: Some(response.manifest.clone()),
        now_ms: request.now_ms + 1,
        ..request.clone()
    }
}

fn same_window(mut actual: WindowResponse, mut expected: WindowResponse) {
    // Operational IO varies between a cold capture and strict bounded replay;
    // the frozen manifest, Signals and logical coverage must be identical.
    actual.coverage.io = None;
    expected.coverage.io = None;
    actual.coverage.support_io = None;
    expected.coverage.support_io = None;
    assert_eq!(actual, expected);
}

fn process_capture(request: &WindowRequest) -> WindowResponse {
    let output = cli(&serde_json::to_string(request).unwrap(), &["window"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn evidence_request(
    request: &WindowRequest,
    response: &WindowResponse,
    keys: &[(&str, &str, usize)],
) -> EvidenceRequest {
    EvidenceRequest {
        schema_version: SCHEMA_VERSION,
        window: replay(request, response),
        keys: keys
            .iter()
            .map(|(root, resource, place)| EvidenceKey {
                root: (*root).into(),
                resource: (*resource).into(),
                place: *place,
            })
            .collect(),
    }
}

fn resource_file(root: &Path, id: &str, file: &str) -> PathBuf {
    std::fs::read_dir(root)
        .unwrap()
        .map(Result::unwrap)
        .find(|entry| {
            std::fs::read(entry.path().join("resource.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Resource>(&bytes).ok())
                .is_some_and(|resource| resource.id == id)
        })
        .unwrap_or_else(|| panic!("resource {id} missing"))
        .path()
        .join(file)
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.insert(path.clone(), std::fs::read(path).unwrap());
        }
    }
    found
}

#[test]
fn exact_prefix_replays_after_new_records_resources_and_grades_without_writes_or_paths() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    world.grade("r1", "task", &[("correct", false)], 300_000);
    let evidence = world.state().join("goose-acp");
    std::fs::create_dir(&evidence).unwrap();
    std::fs::write(
        evidence.join("invocation.evidence.json"),
        json!({"key":{"run_id":"r2","node_id":"worker","invocation":1},
            "tool_calls":[{"tool":"raw_tool","arguments":{"value":SECRET},
            "result":{"ok":true,"output":SECRET}}]})
        .to_string(),
    )
    .unwrap();
    let initial_files = files(world.state().as_path());
    let request = request(&world, &["kernel-runs", "kernel-facts", "kernel-grades"]);
    let frozen = capture(&request).unwrap();
    assert_eq!(files(world.state().as_path()), initial_files);
    assert_eq!(frozen.manifest.analyzer_version, ANALYZER_VERSION);
    assert_eq!(frozen.manifest_digest, frozen.manifest.digest());
    assert!(frozen.coverage.seen_records > 5);
    assert!(frozen.coverage.seen_resources > 5);
    assert_eq!(frozen.coverage.investigated, 0);
    assert_eq!(frozen.coverage.unavailable, 0);
    let text = serde_json::to_string(&frozen).unwrap();
    for absent in [
        SECRET,
        world.path().to_str().unwrap(),
        "raw_tool",
        "\"arguments\":",
    ] {
        assert!(!text.contains(absent), "leaked {absent}");
    }
    assert!(!text.contains("resource.json"));
    assert!(!text.contains("details"));

    world.result("r2", "done#1", 2_100_000, 0, true);
    world.grade("r1", "task", &[("correct", true)], 2_100_001);
    world.run("late", 2_100_002);
    world.result("late", "worker#1", 2_100_003, 0, true);
    world.observe(
        "late",
        "new_worker",
        2_100_004,
        &support::configuration("new_worker"),
        vec![support::lifecycle(
            (2_100_002, 2_100_003),
            anchor_facts::Outcome::Failed,
            0,
            0,
        )],
    );
    same_window(capture(&replay(&request, &frozen)).unwrap(), frozen.clone());
    assert_ne!(
        capture(&request).unwrap().manifest_digest,
        frozen.manifest_digest
    );
}

#[test]
#[ignore = "requires ANCHOR_TEST_LEGACY_HUB pointing to retained analyzer-v4 binary"]
fn retained_v4_binary_windows_keep_counts_citations_and_page_offsets() {
    use anchor_facts::window::SignalPageRequest;
    let legacy = std::env::var_os("ANCHOR_TEST_LEGACY_HUB").expect("retained v4 binary");
    let old = |command: &str, value: &Value| -> Value {
        let mut child = Command::new(&legacy)
            .arg(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(serde_json::to_string(value).unwrap().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let world = World::new();
    for index in 0..2001 {
        world.grade(
            &format!("graded-{index:04}"),
            "ledger",
            &[("total", false)],
            100,
        );
    }
    let append = |run: &str| {
        for _ in 0..7 {
            world.observe(
                run,
                "worker",
                100,
                &support::configuration("worker"),
                vec![support::call(
                    "anchor_read",
                    1,
                    json!({"path":"/outside"}),
                    (10, 20),
                    Some(anchor_facts::Failure::NotAuthorized),
                )],
            );
        }
    };
    append("ordinary-first");
    let mut request = request(&world, &["kernel-facts", "kernel-grades"]);
    request.support_history = true;
    request.limit = 1;
    let first: WindowResponse = serde_json::from_value(old("window", &json!(request))).unwrap();
    assert_eq!(first.manifest.analyzer_version, 4);
    let first_signal = first.signals["signals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|signal| signal["code"] == "check_failing")
        .unwrap();
    assert_eq!(first_signal["evidence_total"], 2000);
    let fresh = capture(&request).unwrap();
    assert_eq!(fresh.manifest.analyzer_version, ANALYZER_VERSION);
    assert_eq!(fresh.signals["signals"][0]["evidence_total"], 2001);
    append("ordinary-first");
    append("ordinary-second");
    let next = advance(&request, &first);
    let second: WindowResponse = serde_json::from_value(old("window", &json!(next))).unwrap();
    assert!(second.signals["signals"].as_array().unwrap().is_empty());
    for (request, frozen) in [(&request, &first), (&next, &second)] {
        let replay = replay(request, frozen);
        same_window(capture(&replay).unwrap(), frozen.clone());
        let mut offset = 0;
        loop {
            let page = SignalPageRequest {
                schema_version: SCHEMA_VERSION,
                window: replay.clone(),
                offset,
                limit: 1,
            };
            let expected = old("window-signals", &json!(page));
            let actual = signal_page(&page).unwrap();
            assert_eq!(json!(actual), expected);
            let Some(next_offset) = actual.next_offset else {
                break;
            };
            offset = next_offset;
        }
    }
    let upgraded = capture(&next).unwrap();
    assert_eq!(upgraded.manifest.analyzer_version, ANALYZER_VERSION);
    assert_eq!(
        upgraded.manifest.parent_manifest_digest,
        Some(first.manifest_digest.clone())
    );
    assert!(!upgraded.signals["signals"].as_array().unwrap().is_empty());
    assert_eq!(first.manifest.analyzer_version, 4);
}

#[test]
fn advanced_signals_cite_delta_before_bounding_historical_support() {
    let world = World::new();
    let count = anchor_facts::window::MAX_SIGNAL_CITATIONS + 1;
    let append = |run: &str| {
        for place in 0..count {
            world.observe(
                run,
                "worker",
                100 + place as u64,
                &support::configuration("worker"),
                vec![support::call(
                    "anchor_read",
                    1,
                    json!({"path":"/outside"}),
                    (10, 20),
                    Some(anchor_facts::Failure::NotAuthorized),
                )],
            );
        }
    };
    append("ordinary-first");
    let mut request = request(&world, &["kernel-facts"]);
    request.support_history = true;
    let first = capture(&request).unwrap();
    let first_signal = first.signals["signals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|signal| signal["code"] == "tool_failures")
        .unwrap();
    assert_eq!(first_signal["evidence_total"], count);

    append("ordinary-second");
    let mut next = advance(&request, &first);
    next.limit = 20;
    let second = capture(&next).unwrap();
    let signal = second.signals["signals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|signal| signal["id"] == first_signal["id"])
        .expect("current evidence must not be hidden behind earlier support citations");
    assert_eq!(signal["evidence_total"], count * 2);
    assert_eq!(signal["runs"], 2);
    assert_eq!(
        signal["evidence"].as_array().unwrap().len(),
        anchor_facts::window::MAX_SIGNAL_CITATIONS
    );
    assert!(
        signal["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|key| { key["resource"] == "trace/ordinary-second/worker/1" })
    );
    let samples = second.signals["exemplars"].as_array().unwrap();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0]["code"], "ordinary_sample");
    assert_eq!(samples[0]["evidence_total"], count);
    assert!(
        samples[0]["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .all(|key| { key["resource"] == "trace/ordinary-second/worker/1" })
    );

    let page = signal_page(&anchor_facts::window::SignalPageRequest {
        schema_version: SCHEMA_VERSION,
        window: replay(&next, &second),
        offset: 0,
        limit: 40,
    })
    .unwrap();
    assert!(page.entries.iter().any(|entry| entry.signal == *signal));
    let keys = serde_json::from_value(signal["evidence"].clone()).unwrap();
    let drill = evidence(&EvidenceRequest {
        schema_version: SCHEMA_VERSION,
        window: replay(&next, &second),
        keys,
    })
    .unwrap();
    assert_eq!(
        drill.items.len(),
        anchor_facts::window::MAX_SIGNAL_CITATIONS
    );
    let replayed = capture(&replay(&next, &second)).unwrap();
    let full_records: usize = second
        .manifest
        .sources
        .iter()
        .flat_map(|source| &source.resources)
        .map(|position| position.through)
        .sum();
    assert_eq!(
        replayed.coverage.io.as_ref().unwrap().records_parsed,
        full_records
    );
    assert_eq!(
        replayed.coverage.support_io.as_ref().unwrap(),
        &anchor_kernel::HistoryReadMetrics::default(),
        "support reuses the prefix that strict replay already verified"
    );
    same_window(replayed, second);
    same_window(capture(&replay(&request, &first)).unwrap(), first);
}

#[test]
fn ordinary_samples_are_frozen_neutral_and_independent_of_failure_or_positive_taxonomies() {
    let world = World::new();
    for index in 0..12 {
        world.observe(
            &format!("ordinary-{index}"),
            "worker",
            100 + index,
            &support::configuration("worker"),
            vec![support::lifecycle(
                (10, 20),
                anchor_facts::Outcome::Completed,
                1,
                1,
            )],
        );
    }
    let mut request = request(&world, &["kernel-facts"]);
    request.limit = 4;
    let frozen = capture(&request).unwrap();
    let samples = frozen.signals["exemplars"].as_array().unwrap();
    assert_eq!(samples.len(), 2);
    assert!(
        samples
            .iter()
            .all(|sample| sample["code"] == "ordinary_sample"
                && sample["source"] == "stable_sampler_v1"
                && sample["weight"] == 12)
    );
    assert!(samples.iter().all(|sample| {
        sample["detail"]
            .as_str()
            .unwrap()
            .contains("no quality or defect assertion")
    }));
    assert_eq!(frozen.signals["omitted"]["ordinary_sample"], 10);
    let keys: Vec<EvidenceKey> = samples
        .iter()
        .flat_map(|sample| {
            serde_json::from_value::<Vec<EvidenceKey>>(sample["evidence"].clone()).unwrap()
        })
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        evidence(&EvidenceRequest {
            schema_version: SCHEMA_VERSION,
            window: replay(&request, &frozen),
            keys
        })
        .unwrap()
        .items
        .len(),
        2
    );
    world.observe(
        "arrived-later",
        "worker",
        200,
        &support::configuration("worker"),
        vec![support::lifecycle(
            (10, 20),
            anchor_facts::Outcome::Failed,
            1,
            1,
        )],
    );
    same_window(capture(&replay(&request, &frozen)).unwrap(), frozen.clone());
    let advanced = capture(&advance(&request, &frozen)).unwrap();
    let new_samples = advanced.signals["exemplars"].as_array().unwrap();
    assert_eq!(new_samples.len(), 1);
    assert_eq!(new_samples[0]["weight"], 1);
    assert!(
        new_samples[0]["subject"]
            .as_str()
            .unwrap()
            .contains("arrived-later")
    );
}

#[test]
fn structured_evidence_replays_only_projected_rows_without_raw_content_or_writes() {
    let world = World::new();
    support::r1(&world);
    world.grade("r1", "task", &[("correct", false)], 300_000);
    let raw_evidence = world.state().join("goose-acp");
    std::fs::create_dir(&raw_evidence).unwrap();
    std::fs::write(
        raw_evidence.join("invocation.evidence.json"),
        json!({"key":{"run_id":"r1","node_id":"worker","invocation":1},
            "tool_calls":[{"tool":"only-in-raw-evidence","arguments":{"value":SECRET},
            "result":{"ok":true,"output":SECRET}}]})
        .to_string(),
    )
    .unwrap();
    let window = request(&world, &["kernel-facts", "kernel-runs", "kernel-grades"]);
    let frozen = capture(&window).unwrap();
    let request = evidence_request(
        &window,
        &frozen,
        &[
            ("kernel-facts", "trace/r1/worker/1", 1),
            ("kernel-runs", "run/r1", 1),
            ("kernel-grades", "grade/r1", 1),
        ],
    );
    let before = files(&world.state());
    let response = evidence(&request).unwrap();
    assert_eq!(files(&world.state()), before);
    assert_eq!(response.manifest_digest, frozen.manifest_digest);
    assert_eq!(response.schema_version, SCHEMA_VERSION);
    assert_eq!(response.items.len(), 3);
    for (item, key) in response.items.iter().zip(&request.keys) {
        assert_eq!(&item.key, key);
        assert_eq!(item.row["key"], serde_json::to_value(key).unwrap());
        assert!(item.row["cost"].is_u64());
        assert!(item.row["principal"].is_string());
    }
    assert_eq!(response.items[0].row["body"], "observation");
    assert!(response.items[0].row["facts"].is_array());
    assert_eq!(response.items[1].row["body"], "kernel");
    assert_eq!(response.items[2].row["body"], "grade");
    let text = serde_json::to_string(&response).unwrap();
    for absent in [
        SECRET,
        world.path().to_str().unwrap(),
        "only-in-raw-evidence",
        "\"arguments\":",
        "\"submission\":",
        "\"after\":",
        "\"state\":",
        "instructions_sha256",
        "resource.json",
    ] {
        assert!(!text.contains(absent), "leaked {absent}");
    }

    world.observe(
        "r1",
        "worker",
        2_100_000,
        &support::configuration("worker"),
        vec![support::lifecycle(
            (2_000_000, 2_100_000),
            anchor_facts::Outcome::Completed,
            0,
            0,
        )],
    );
    world.result("r1", "late#1", 2_100_001, 0, false);
    world.grade("r1", "task", &[("correct", true)], 2_100_002);
    world.run("late", 2_100_003);
    world.result("late", "worker#1", 2_100_004, 0, true);
    assert_eq!(evidence(&request).unwrap(), response);
    assert_ne!(
        capture(&window).unwrap().manifest_digest,
        response.manifest_digest
    );
}

#[test]
fn evidence_keys_are_bounded_unique_exact_frozen_positions_not_live_or_related_data() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    let mut window = request(&world, &["kernel-facts"]);
    window.sources[0].scope = ResourceScope::Resources {
        ids: vec!["trace/r1/worker/1".into()],
    };
    let frozen = capture(&window).unwrap();
    let authorized = evidence_request(
        &window,
        &frozen,
        &[("kernel-facts", "trace/r1/worker/1", 1)],
    );
    evidence(&authorized).unwrap();
    for (root, resource, place) in [
        ("unregistered", "trace/r1/worker/1", 1),
        ("kernel-facts", "trace/r2/worker/1", 1),
        ("kernel-facts", "../trace/r1/worker/1", 1),
        ("kernel-facts", "trace/r1/worker/1", 0),
        ("kernel-facts", "trace/r1/worker/1", 2),
    ] {
        let mut invalid = authorized.clone();
        invalid.keys[0] = EvidenceKey {
            root: root.into(),
            resource: resource.into(),
            place,
        };
        assert!(evidence(&invalid).is_err(), "{root}/{resource}#{place}");
    }
    let mut duplicate = authorized.clone();
    duplicate.keys.push(duplicate.keys[0].clone());
    assert_eq!(
        evidence(&duplicate).unwrap_err(),
        "evidence request contains duplicate keys"
    );
    let mut oversized = authorized.clone();
    oversized.keys = (1..=MAX_EVIDENCE_KEYS + 1)
        .map(|place| EvidenceKey {
            place,
            ..authorized.keys[0].clone()
        })
        .collect();
    assert_eq!(
        evidence(&oversized).unwrap_err(),
        "evidence request exceeds the key limit"
    );

    // A related configuration exists but was not granted, and has no projected
    // submission row even when explicitly present in a later authorized window.
    let config = anchor_facts::configuration_id(&support::configuration("worker").digest());
    let mut config_request = authorized.clone();
    config_request.keys[0].resource = config.clone();
    assert!(evidence(&config_request).is_err());
    if let ResourceScope::Resources { ids } = &mut window.sources[0].scope {
        ids.push(config.clone());
    }
    let with_config = capture(&window).unwrap();
    assert!(
        evidence(&evidence_request(
            &window,
            &with_config,
            &[("kernel-facts", &config, 1)]
        ))
        .is_err()
    );

    let mut forged_scope = authorized.clone();
    forged_scope.window.manifest.as_mut().unwrap().sources[0]
        .resources
        .push(anchor_facts::window::ResourceWindow {
            resource: "trace/r2/worker/1".into(),
            ..frozen.manifest.sources[0].resources[0].clone()
        });
    forged_scope.keys[0].resource = "trace/r2/worker/1".into();
    assert!(evidence(&forged_scope).is_err());
}

#[test]
fn evidence_requires_current_versions_and_reuses_window_integrity_checks() {
    let world = World::new();
    support::r1(&world);
    let window = request(&world, &["kernel-facts"]);
    let frozen = capture(&window).unwrap();
    let request = evidence_request(
        &window,
        &frozen,
        &[("kernel-facts", "trace/r1/worker/1", 1)],
    );
    for alteration in [
        "no_manifest",
        "evidence_version",
        "window_version",
        "manifest_version",
        "analyzer_version",
        "source_identity",
        "history_digest",
        "snapshot_digest",
    ] {
        let mut changed = request.clone();
        match alteration {
            "no_manifest" => changed.window.manifest = None,
            "evidence_version" => changed.schema_version += 1,
            "window_version" => changed.window.schema_version += 1,
            "manifest_version" => changed.window.manifest.as_mut().unwrap().schema_version += 1,
            "analyzer_version" => changed.window.manifest.as_mut().unwrap().analyzer_version += 1,
            "source_identity" => {
                changed.window.manifest.as_mut().unwrap().sources[0].root_fingerprint =
                    "0".repeat(64)
            }
            "history_digest" | "snapshot_digest" => {
                let position = changed.window.manifest.as_mut().unwrap().sources[0]
                    .resources
                    .iter_mut()
                    .find(|position| position.resource == "trace/r1/worker/1")
                    .unwrap();
                if alteration == "history_digest" {
                    position.history_digest = "0".repeat(64);
                } else {
                    position.snapshot_digest = "0".repeat(64);
                }
            }
            _ => unreachable!(),
        }
        assert!(evidence(&changed).is_err(), "{alteration}");
    }
    let history = resource_file(
        &world.root("kernel-facts"),
        "trace/r1/worker/1",
        "history.jsonl",
    );
    let bytes = std::fs::read(&history).unwrap();
    let mut record: Value = serde_json::from_slice(&bytes).unwrap();
    record["stamp"]["cost"] = json!(99);
    std::fs::write(&history, format!("{record}\n")).unwrap();
    assert_eq!(
        evidence(&request).unwrap_err(),
        capture(&request.window).unwrap_err()
    );
    std::fs::write(&history, bytes).unwrap();
    evidence(&request).unwrap();
    std::fs::remove_file(&history).unwrap();
    assert!(evidence(&request).is_err());
}

#[test]
fn scope_filters_before_aggregation_and_never_implicitly_loads_related_configs() {
    let world = World::new();
    for (run, model) in [("allowed", "visible_model"), ("other", "outside_model")] {
        let mut config = support::configuration(run);
        config.model = model.into();
        world.observe(
            run,
            run,
            100,
            &config,
            vec![
                support::call(
                    "same_tool",
                    1,
                    json!({"path":"/outside"}),
                    (10, 11),
                    Some(anchor_facts::Failure::NotAuthorized),
                ),
                support::lifecycle((1, 12), anchor_facts::Outcome::Failed, 0, 1),
            ],
        );
    }
    let mut request = request(&world, &["kernel-facts"]);
    let all = capture(&request).unwrap();
    let failures = |response: &WindowResponse| {
        response.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|signal| signal["code"] == "tool_failures")
            .unwrap()["runs"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(failures(&all), 2);
    request.sources[0].scope = ResourceScope::Resources {
        ids: vec!["trace/allowed/allowed/1".into()],
    };
    let scoped = capture(&request).unwrap();
    assert_eq!(scoped.coverage.seen_records, 1);
    assert_eq!(scoped.coverage.seen_resources, 1);
    assert_eq!(failures(&scoped), 1);
    let text = scoped.signals.to_string();
    assert!(!text.contains("outside_model"));
    assert!(!text.contains("visible_model"));
    assert!(
        scoped.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|signal| signal["coordinates"].as_array().unwrap())
            .all(|coordinate| coordinate["model"].is_null())
    );
    let mut config = support::configuration("allowed");
    config.model = "visible_model".into();
    if let ResourceScope::Resources { ids } = &mut request.sources[0].scope {
        ids.push(anchor_facts::configuration_id(&config.digest()));
    }
    let with_config = capture(&request).unwrap();
    assert!(with_config.signals.to_string().contains("visible_model"));
    assert!(!with_config.signals.to_string().contains("outside_model"));

    // An unauthorized body's corruption cannot affect the authorized aggregate.
    let other = resource_file(
        &world.root("kernel-facts"),
        "trace/other/other/1",
        "resource.json",
    );
    std::fs::write(&other, "not JSON").unwrap();
    assert_eq!(capture(&request).unwrap(), with_config);
    request.sources[0].scope = ResourceScope::All {};
    assert!(capture(&request).is_err());
}

#[test]
fn empty_scope_and_handshake_are_explicit_and_missing_sources_are_not_empty() {
    let world = World::new();
    support::r1(&world);
    let mut request = request(&world, &["kernel-facts"]);
    request.sources[0].scope = ResourceScope::Resources { ids: vec![] };
    let response = capture(&request).unwrap();
    assert_eq!(response.coverage.seen_resources, 0);
    assert_eq!(response.coverage.seen_records, 0);
    assert_eq!(response.coverage.sources, 1);
    request.sources[0].root = world.state().join("missing");
    assert!(capture(&request).is_err());
    request.sources.clear();
    let handshake = capture(&request).unwrap();
    assert_eq!(handshake.coverage.sources, 0);
    assert_eq!(handshake.signals["signals"], json!([]));
    assert_eq!(handshake.signals["exemplars"], json!([]));

    request.sources.push(WindowSource {
        source_instance: "not-a-structured-root".into(),
        join_namespace: "fixture-install".into(),
        root: world.state(),
        scope: ResourceScope::All {},
    });
    assert!(capture(&request).is_err(), "must not discover child roots");
}

#[test]
fn changed_prefix_truncation_deletion_and_replaced_roots_invalidate_the_window() {
    for alteration in [
        "edit",
        "truncate",
        "history_deleted",
        "resource_deleted",
        "root_replaced",
    ] {
        let world = World::new();
        support::r1(&world);
        let request = request(&world, &["kernel-runs"]);
        let frozen = capture(&request).unwrap();
        let history = resource_file(&world.root("kernel-runs"), "run/r1", "history.jsonl");
        match alteration {
            "edit" => {
                let text = std::fs::read_to_string(&history).unwrap();
                let mut lines: Vec<Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                lines[0]["stamp"]["cost"] = json!(99);
                std::fs::write(
                    &history,
                    lines
                        .iter()
                        .map(|line| format!("{line}\n"))
                        .collect::<String>(),
                )
                .unwrap();
            }
            "truncate" => {
                let text = std::fs::read_to_string(&history).unwrap();
                std::fs::write(&history, format!("{}\n", text.lines().next().unwrap())).unwrap();
            }
            "history_deleted" => std::fs::remove_file(&history).unwrap(),
            "resource_deleted" => std::fs::remove_dir_all(history.parent().unwrap()).unwrap(),
            "root_replaced" => {
                let root = world.root("kernel-runs");
                std::fs::rename(&root, world.state().join("old-runs")).unwrap();
                std::fs::create_dir(&root).unwrap();
            }
            _ => unreachable!(),
        }
        let error = capture(&replay(&request, &frozen)).unwrap_err();
        assert!(
            !error.contains(world.path().to_str().unwrap()),
            "{alteration}"
        );
    }
}

#[test]
fn complete_line_prefix_ignores_only_an_unterminated_tail() {
    let world = World::new();
    support::r1(&world);
    let request = request(&world, &["kernel-facts"]);
    let frozen = capture(&request).unwrap();
    let history = resource_file(
        &world.root("kernel-facts"),
        "trace/r1/worker/1",
        "history.jsonl",
    );
    let original = std::fs::read(&history).unwrap();
    for tail in [
        b"not JSON".as_slice(),
        original.strip_suffix(b"\n").unwrap(),
    ] {
        let mut bytes = original.clone();
        bytes.extend(tail);
        std::fs::write(&history, bytes).unwrap();
        same_window(capture(&replay(&request, &frozen)).unwrap(), frozen.clone());
    }
    let mut bytes = original;
    bytes.extend(b"not JSON\n");
    std::fs::write(&history, bytes).unwrap();
    // Corruption strictly after the frozen upper bound is not part of replay.
    same_window(capture(&replay(&request, &frozen)).unwrap(), frozen.clone());
    assert!(capture(&request).is_err());
}

#[test]
fn state_gaps_fail_instead_of_using_latest_state_and_refused_after_is_authoritative() {
    let world = World::new();
    world.run("initial", 1);
    let request = request(&world, &["kernel-runs"]);
    let initial = capture(&request).unwrap();
    world.result("initial", "step#1", 2, 0, true);
    assert!(capture(&replay(&request, &initial)).is_err());

    world
        .runs
        .create(Resource::new("run/exhausted", "validator/step", Value::Null).task(1))
        .unwrap();
    let verdict = world
        .runs
        .submit(
            &support::executor(),
            2,
            Submission::new("run/exhausted", 0, Action::Write),
        )
        .unwrap();
    assert!(!verdict.is_accepted());
    assert!(
        world.runs.history("run/exhausted").unwrap()[0]
            .after
            .is_some()
    );
    let response = capture(&request).unwrap();
    assert!(
        response.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|signal| signal["code"] == "task_not_done")
    );
    let history = resource_file(&world.root("kernel-runs"), "run/initial", "history.jsonl");
    let mut record: Value =
        serde_json::from_str(std::fs::read_to_string(&history).unwrap().trim()).unwrap();
    record.as_object_mut().unwrap().remove("after");
    std::fs::write(&history, format!("{record}\n")).unwrap();
    assert!(
        capture(&request).is_err(),
        "accepted record without state is a gap"
    );

    // A complete Kernel record with an unsupported structured body is not a
    // healthy empty observation; the exact-window protocol fails explicitly.
    support::r1(&world);
    let facts_request = self::request(&world, &["kernel-facts"]);
    let history = resource_file(
        &world.root("kernel-facts"),
        "trace/r1/worker/1",
        "history.jsonl",
    );
    let mut record: Value =
        serde_json::from_str(std::fs::read_to_string(&history).unwrap().trim()).unwrap();
    record["submission"]["content"] = json!({"unsupported_schema": true});
    std::fs::write(&history, format!("{record}\n")).unwrap();
    assert!(capture(&facts_request).is_err());
}

#[test]
fn shape_compatible_future_fact_and_configuration_versions_are_unavailable() {
    for (root, id) in [
        ("kernel-facts", "trace/r1/worker/1"),
        ("kernel-grades", "grade/r1"),
    ] {
        let world = World::new();
        support::r1(&world);
        world.grade("r1", "task", &[("correct", true)], 10_000);
        let request = request(&world, &[root]);
        capture(&request).unwrap();
        let history = resource_file(&world.root(root), id, "history.jsonl");
        let mut record: Value =
            serde_json::from_str(std::fs::read_to_string(&history).unwrap().trim()).unwrap();
        record["submission"]["content"]["version"] = json!(anchor_facts::VERSION + 1);
        std::fs::write(&history, format!("{record}\n")).unwrap();
        assert_eq!(
            capture(&request).unwrap_err(),
            "authorized structured fact schema is unavailable",
            "{id}"
        );
    }

    let world = World::new();
    let mut configuration = support::configuration("future");
    configuration.version = anchor_facts::CONFIGURATION_VERSION + 1;
    let digest = configuration.digest();
    world
        .facts
        .create(
            Resource::new(
                anchor_facts::configuration_id(&digest),
                anchor_facts::CONFIGURATION,
                serde_json::to_value(configuration).unwrap(),
            )
            .configured(digest),
        )
        .unwrap();
    assert_eq!(
        capture(&request(&world, &["kernel-facts"])).unwrap_err(),
        "authorized structured fact schema is unavailable"
    );
}

#[test]
fn replay_pins_scope_sources_analyzer_clock_and_summary_limit() {
    let world = World::new();
    support::r1(&world);
    let mut request = request(&world, &["kernel-facts"]);
    request.sources[0].scope = ResourceScope::Resources {
        ids: vec!["trace/r1/worker/1".into()],
    };
    let frozen = capture(&request).unwrap();
    for alteration in ["scope", "source", "now", "limit", "analyzer", "position"] {
        let mut replay = replay(&request, &frozen);
        match alteration {
            "scope" => replay.sources[0].scope = ResourceScope::All {},
            "source" => replay.sources[0].source_instance = "different-source".into(),
            "now" => replay.now_ms += 1,
            "limit" => replay.limit += 1,
            "analyzer" => replay.manifest.as_mut().unwrap().analyzer_version += 1,
            "position" => replay.manifest.as_mut().unwrap().sources[0].resources[0].after = 1,
            _ => unreachable!(),
        }
        assert!(capture(&replay).is_err(), "{alteration}");
    }
    request.sources.push(request.sources[0].clone());
    assert!(capture(&request).is_err());
    request.sources[1].source_instance = "same-physical-root".into();
    assert!(capture(&request).is_err());
}

#[test]
fn bounded_signals_count_omitted_without_claiming_investigations() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    let mut request = request(&world, &["kernel-facts", "kernel-runs"]);
    let all = capture(&request).unwrap();
    let count = all.coverage.included_in_summary + all.coverage.deferred;
    assert!(count > 2);
    for limit in [0, 1] {
        request.limit = limit;
        let response = capture(&request).unwrap();
        for key in ["signals", "exemplars"] {
            assert!(response.signals[key].as_array().unwrap().len() <= limit);
        }
        assert_eq!(
            response.coverage.included_in_summary + response.coverage.deferred,
            count
        );
        assert_eq!(response.coverage.investigated, 0);
        assert_eq!(response.coverage.seen_records, all.coverage.seen_records);
    }
}

#[test]
fn frozen_signal_pages_reach_omitted_entries_without_changing_the_ranking_or_clock() {
    use anchor_facts::window::{SignalPageRequest, SignalPageResponse};
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    let mut request = request(&world, &["kernel-facts", "kernel-runs"]);
    request.limit = 1;
    let frozen = capture(&request).unwrap();
    assert!(frozen.coverage.deferred > 0);
    let mut page = SignalPageRequest {
        schema_version: SCHEMA_VERSION,
        window: replay(&request, &frozen),
        offset: 0,
        limit: 1,
    };
    let mut entries = Vec::new();
    loop {
        let output = cli(&serde_json::to_string(&page).unwrap(), &["window-signals"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: SignalPageResponse = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response, signal_page(&page).unwrap());
        assert_eq!(response.manifest_digest, frozen.manifest_digest);
        entries.extend(response.entries);
        match response.next_offset {
            Some(offset) => page.offset = offset,
            None => {
                assert_eq!(entries.len(), response.total);
                break;
            }
        }
    }
    assert_eq!(
        entries.len(),
        frozen.coverage.included_in_summary + frozen.coverage.deferred
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.signal["id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        entries.len()
    );
    assert!(entries.iter().any(|entry| {
        !frozen.signals[if entry.exemplar {
            "exemplars"
        } else {
            "signals"
        }]
        .as_array()
        .unwrap()
        .contains(&entry.signal)
    }));
    world.run("later", request.now_ms + 1);
    assert_eq!(
        signal_page(&page).unwrap().manifest_digest,
        frozen.manifest_digest
    );
    page.window.now_ms += 1;
    assert!(signal_page(&page).is_err());
    page.window.now_ms -= 1;
    page.limit = 41;
    assert!(signal_page(&page).is_err());
    page.limit = 1;
    page.window.manifest = None;
    assert!(signal_page(&page).is_err());
}

#[test]
fn host_run_exclusions_precede_merged_signals_and_advance_can_only_add_them() {
    let world = World::new();
    for run in ["ordinary", "dreaming", "experiment"] {
        world.observe(
            run,
            "same-node",
            100,
            &support::configuration("same-node"),
            vec![
                support::call(
                    "same_tool",
                    1,
                    json!({"path":"/outside"}),
                    (10, 11),
                    Some(anchor_facts::Failure::NotAuthorized),
                ),
                support::lifecycle((1, 12), anchor_facts::Outcome::Failed, 0, 1),
            ],
        );
    }
    let mut request = request(&world, &["kernel-facts"]);
    let all = capture(&request).unwrap();
    let failures = |window: &WindowResponse| {
        window.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .find(|signal| signal["code"] == "tool_failures")
            .unwrap()["runs"]
            .as_u64()
            .unwrap()
    };
    assert_eq!(failures(&all), 3);
    request.exclude_runs = vec!["dreaming".into(), "experiment".into()];
    let scoped = capture(&request).unwrap();
    assert_eq!(failures(&scoped), 1);
    assert!(
        scoped.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|signal| signal["evidence"].as_array().unwrap())
            .all(
                |key| !key["resource"].as_str().unwrap().contains("/dreaming/")
                    && !key["resource"].as_str().unwrap().contains("/experiment/")
            )
    );
    assert!(
        evidence(&evidence_request(
            &request,
            &scoped,
            &[("kernel-facts", "trace/dreaming/same-node/1", 1)]
        ))
        .is_err()
    );
    let mut next = advance(&request, &scoped);
    next.exclude_runs.push("ordinary".into());
    let empty = capture(&next).unwrap();
    assert_eq!(empty.signals["signals"], json!([]));
    let mut removed = advance(&next, &empty);
    removed.exclude_runs.remove(0);
    assert!(capture(&removed).is_err());
    let mut wrong = replay(&request, &scoped);
    wrong.exclude_runs.clear();
    assert!(capture(&wrong).is_err());
    same_window(capture(&replay(&request, &scoped)).unwrap(), scoped);
}

#[test]
fn an_idle_delta_does_not_fabricate_last_activity_from_an_old_creation_time() {
    let world = World::new();
    world.run("active", 1);
    world.result("active", "worker#1", 1_999_999, 0, false);
    let request = request(&world, &["kernel-runs"]);
    let first = capture(&request).unwrap();
    assert!(
        !first.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|signal| signal["code"] == "idle_task")
    );
    let mut next = advance(&request, &first);
    next.now_ms += 10_000_000;
    let delta = capture(&next).unwrap();
    assert_eq!(delta.coverage.seen_records, 0);
    assert!(
        !delta.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|signal| signal["code"] == "idle_task")
    );
}

#[test]
fn shared_scope_contract_accepts_logical_names_not_paths_patterns_or_duplicates() {
    assert!(
        serde_json::from_value::<ResourceScope>(json!({"kind": "all", "root": "/state"})).is_err()
    );
    assert!(
        serde_json::from_value::<ResourceScope>(
            json!({"kind": "resources", "ids": [], "details": true})
        )
        .is_err()
    );
    for id in [
        "",
        "/tmp/data",
        "../trace",
        "trace//x",
        "trace/./x",
        "trace/../x",
        "trace/*",
        "trace/?",
        "C:\\data",
        "trace\nvalue",
    ] {
        assert!(
            ResourceScope::Resources {
                ids: vec![id.into()]
            }
            .validate()
            .is_err(),
            "{id}"
        );
    }
    assert!(
        ResourceScope::Resources {
            ids: vec!["trace/r/node/1".into(), "trace/r/node/1".into()]
        }
        .validate()
        .is_err()
    );
    for scope in [
        ResourceScope::All {},
        ResourceScope::Resources { ids: vec![] },
        ResourceScope::Resources {
            ids: vec!["trace/r/node/1".into(), "config/abc".into()],
        },
    ] {
        scope.validate().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn source_identity_is_opaque_and_links_fail_without_following_them() {
    use std::os::unix::{fs::MetadataExt, fs::symlink};
    let world = World::new();
    support::r1(&world);
    let mut request = request(&world, &["kernel-facts"]);
    let response = capture(&request).unwrap();
    let metadata = std::fs::metadata(&request.sources[0].root).unwrap();
    assert_eq!(
        response.manifest.sources[0].root_fingerprint,
        source_fingerprint(metadata.dev(), metadata.ino())
    );

    let linked = world.state().join("linked-facts");
    symlink(&request.sources[0].root, &linked).unwrap();
    request.sources[0].root = linked;
    assert!(capture(&request).is_err());
    request.sources[0].root = world.root("kernel-facts");
    let body = resource_file(
        &request.sources[0].root,
        "trace/r1/worker/1",
        "resource.json",
    );
    let outside = world.state().join("outside-body");
    std::fs::rename(&body, &outside).unwrap();
    symlink(&outside, &body).unwrap();
    assert!(capture(&request).is_err());
    // The metadata boundary is conservative even for an excluded link; no
    // unauthorized body is opened to decide whether the source is usable.
    request.sources[0].scope = ResourceScope::Resources { ids: vec![] };
    assert!(capture(&request).is_err());
}

fn cli(input: &str, args: &[&str]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_anchor-hub"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[test]
fn window_command_is_one_strict_json_request_and_one_response_not_discovery_or_mcp() {
    let world = World::new();
    support::r1(&world);
    let request = request(&world, &["kernel-facts"]);
    let output = cli(&serde_json::to_string(&request).unwrap(), &["window"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    assert_eq!(
        serde_json::from_slice::<WindowResponse>(&output.stdout).unwrap(),
        capture(&request).unwrap()
    );
    let mut unknown = serde_json::to_value(&request).unwrap();
    unknown["details"] = json!(true);
    let output = cli(&unknown.to_string(), &["window"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let output = cli(
        &format!("{}\n{{}}", serde_json::to_string(&request).unwrap()),
        &["window"],
    );
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let output = cli("{}", &["window", "--json"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn window_evidence_command_and_shared_dtos_are_strict_one_request_one_response() {
    let world = World::new();
    support::r1(&world);
    let window = request(&world, &["kernel-facts"]);
    let frozen = capture(&window).unwrap();
    let request = evidence_request(
        &window,
        &frozen,
        &[("kernel-facts", "trace/r1/worker/1", 1)],
    );
    let input = serde_json::to_string(&request).unwrap();
    let output = cli(&input, &["window-evidence"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    assert_eq!(String::from_utf8_lossy(&output.stdout).lines().count(), 1);
    let response: EvidenceResponse = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response, evidence(&request).unwrap());

    for target in ["request", "key", "window"] {
        let mut unknown = serde_json::to_value(&request).unwrap();
        match target {
            "request" => unknown["details"] = json!(true),
            "key" => unknown["keys"][0]["details"] = json!(true),
            "window" => unknown["window"]["details"] = json!(true),
            _ => unreachable!(),
        }
        assert!(serde_json::from_value::<EvidenceRequest>(unknown.clone()).is_err());
        let output = cli(&unknown.to_string(), &["window-evidence"]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
    for target in ["response", "item"] {
        let mut unknown = serde_json::to_value(&response).unwrap();
        match target {
            "response" => unknown["details"] = json!(true),
            "item" => unknown["items"][0]["details"] = json!(true),
            _ => unreachable!(),
        }
        assert!(serde_json::from_value::<EvidenceResponse>(unknown).is_err());
    }
    for (input, args) in [
        (format!("{input}\n{{}}"), vec!["window-evidence"]),
        (input, vec!["window-evidence", "--json"]),
    ] {
        let output = cli(&input, &args);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
    }
}

#[test]
fn both_window_commands_reject_input_above_the_shared_transport_limit() {
    let oversized = " ".repeat(MAX_TRANSPORT_BYTES + 1);
    for command in ["window", "window-evidence"] {
        let output = cli(&oversized, &[command]);
        assert!(!output.status.success(), "{command}");
        assert!(output.stdout.is_empty(), "{command}");
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("window request exceeds 64 MiB transport limit"),
            "{command}"
        );
    }
}

#[test]
fn incremental_windows_restart_between_processes_and_keep_full_history_evidence_positions() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    let initial_request = request(&world, &["kernel-runs", "kernel-facts"]);
    let initial = process_capture(&initial_request);
    let idle_request = advance(&initial_request, &initial);
    let idle = process_capture(&idle_request);
    assert_eq!(
        idle.manifest.parent_manifest_digest,
        Some(initial.manifest_digest.clone())
    );
    assert_eq!(idle.coverage.seen_records, 0);
    let io = idle.coverage.io.as_ref().unwrap();
    assert_eq!(io.records_parsed, 0);
    assert_eq!(io.history_bytes_read, 0);
    assert!(io.snapshot_records_parsed > 0);
    assert!(
        idle.manifest
            .sources
            .iter()
            .flat_map(|source| &source.resources)
            .all(|position| position.after == position.through)
    );

    world.result("r2", "later#1", 2_100_000, 2, false);
    world.observe(
        "r2",
        "worker",
        2_100_001,
        &support::configuration("worker"),
        vec![support::lifecycle(
            (2_100_000, 2_100_001),
            anchor_facts::Outcome::Completed,
            0,
            0,
        )],
    );
    world.run("new", 2_100_002);
    world.result("new", "worker#1", 2_100_003, 1, true);
    let next_request = advance(&idle_request, &idle);
    let next = process_capture(&next_request);
    assert_eq!(next.coverage.seen_records, 3);
    assert_eq!(next.coverage.io.as_ref().unwrap().records_parsed, 3);
    for source in &next.manifest.sources {
        let previous = idle
            .manifest
            .sources
            .iter()
            .find(|old| old.source_instance == source.source_instance)
            .unwrap();
        for position in &source.resources {
            if let Some(old) = previous
                .resources
                .iter()
                .find(|old| old.resource == position.resource)
            {
                assert_eq!(position.after, old.through);
                assert_eq!(position.start_cursor, old.end_cursor);
            } else {
                assert_eq!(position.after, 0);
                assert!(position.start_cursor.is_none());
            }
        }
    }
    let lookup = evidence_request(&next_request, &next, &[("kernel-runs", "run/r2", 2)]);
    let row = evidence(&lookup).unwrap().items.remove(0).row;
    assert_eq!(row["key"]["place"], 2);
    let mut outside = lookup.clone();
    outside.keys[0].place = 1;
    assert!(evidence(&outside).is_err());
    world.result("r2", "even-later#1", 2_200_000, 0, false);
    same_window(process_capture(&replay(&next_request, &next)), next.clone());
    let text = serde_json::to_string(&next).unwrap();
    for value in [
        SECRET,
        world.path().to_str().unwrap(),
        "\"state\":",
        "\"after\":{",
    ] {
        assert!(!text.contains(value), "leaked {value}");
    }
    let mut ambiguous = replay(&next_request, &next);
    ambiguous.advance_from = Some(idle.manifest.clone());
    assert!(capture(&ambiguous).is_err());
}

#[test]
fn advancing_refuses_changed_scope_or_lost_replaced_and_truncated_history() {
    for scenario in ["scope", "missing", "replaced", "truncated", "boundary"] {
        let world = World::new();
        support::r1(&world);
        let request = request(&world, &["kernel-runs"]);
        let initial = capture(&request).unwrap();
        let mut next = advance(&request, &initial);
        let path = resource_file(&world.root("kernel-runs"), "run/r1", "history.jsonl");
        match scenario {
            "scope" => {
                next.sources[0].scope = ResourceScope::Resources {
                    ids: vec!["run/r1".into()],
                }
            }
            "missing" => std::fs::remove_dir_all(path.parent().unwrap()).unwrap(),
            "replaced" => {
                let bytes = std::fs::read(&path).unwrap();
                std::fs::rename(&path, path.with_extension("old")).unwrap();
                std::fs::write(path, bytes).unwrap();
            }
            "truncated" => std::fs::write(path, []).unwrap(),
            "boundary" => {
                let mut bytes = std::fs::read(&path).unwrap();
                let last = bytes.len() - 2;
                bytes[last] ^= 1;
                std::fs::write(path, bytes).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(capture(&next).is_err(), "{scenario}");
    }
}

#[test]
fn legacy_full_prefix_replay_keeps_its_serialized_digest_without_gaining_physical_cursors() {
    let world = World::new();
    support::r1(&world);
    let request = request(&world, &["kernel-runs"]);
    let mut legacy = capture(&request).unwrap();
    legacy.manifest.schema_version = anchor_facts::window::LEGACY_SCHEMA_VERSION;
    legacy
        .signals
        .as_object_mut()
        .unwrap()
        .remove("longitudinal");
    for status in legacy.signals["applicability"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        if status == "not_granted" {
            *status = json!("not_authorized_or_missing_source");
        }
    }
    for position in &mut legacy.manifest.sources[0].resources {
        position.start_cursor = None;
        position.end_cursor = None;
    }
    legacy.manifest_digest = legacy.manifest.digest();
    let encoded = serde_json::to_string(&legacy.manifest).unwrap();
    for absent in ["parent_manifest_digest", "start_cursor", "end_cursor"] {
        assert!(!encoded.contains(absent));
    }
    let replayed = process_capture(&replay(&request, &legacy));
    same_window(replayed, legacy.clone());
    assert!(capture(&advance(&request, &legacy)).is_err());
}

#[test]
fn ordinary_data_root_pulls_physically_parse_only_new_records_after_the_cold_pull() {
    use anchor_hub::{
        source::DataRoot,
        table::{MemoryTable, Projector},
    };
    let world = World::new();
    support::r1(&world);
    let source = DataRoot::open(&world.root("kernel-runs"), "runs").unwrap();
    let mut table = MemoryTable::default();
    let mut projector = Projector::default();
    assert_eq!(projector.pull(&source, &mut table).unwrap(), 5);
    let cold = source.read_metrics();
    assert_eq!(projector.pull(&source, &mut table).unwrap(), 0);
    let idle = source.read_metrics();
    assert_eq!(idle.records_parsed, cold.records_parsed);
    assert_eq!(idle.history_bytes_read, cold.history_bytes_read);
    // Even a refused submission is a new fact and carries its actual cost.
    world.result("r1", "later#1", 2_100_000, 1, false);
    assert_eq!(projector.pull(&source, &mut table).unwrap(), 1);
    let delta = source.read_metrics();
    assert_eq!(delta.records_parsed - idle.records_parsed, 1);
    assert!(delta.history_bytes_read - idle.history_bytes_read < cold.history_bytes_read);
}

#[test]
#[ignore = "low-frequency 10,000-graph structured-history benchmark"]
fn ten_thousand_graph_histories_cold_idle_delta_and_long_history_reads_are_measured() {
    use std::time::Instant;
    const GRAPHS: usize = 10_000;
    const RECORDS: usize = 32;
    const LONG_RECORDS: usize = 2_500;
    let mut world = World::new();
    let configuration = |run: &str| {
        let mut configuration = support::configuration("worker");
        configuration.graph_digest = anchor_facts::text_digest(run);
        configuration
    };
    let observe = |world: &World, run: &str, step: u64| {
        world.observe(
            run,
            "worker",
            step + 2,
            &configuration(run),
            vec![
                support::turn(step + 1, step + 2, 1),
                support::lifecycle((step + 1, step + 2), anchor_facts::Outcome::Completed, 1, 0),
            ],
        );
    };
    for index in 0..GRAPHS {
        let id = format!("graph-{index:05}");
        for step in 0..RECORDS {
            observe(&world, &id, step as u64);
        }
        // Keep this read benchmark from exhausting the machine's per-user
        // inotify instance limit through unrelated writer-owned caches.
        if index % 32 == 31 {
            world.reopen_facts();
        }
    }
    for step in 0..LONG_RECORDS {
        observe(&world, "long-history", step as u64);
    }
    world.reopen_facts();
    let mut request = request(&world, &["kernel-facts"]);
    request.now_ms = 4_000;
    request.limit = 8;
    let model_input_shape = |response: &WindowResponse| {
        json!({
            "longitudinal": {
                "fields": response.signals["longitudinal"].as_object().unwrap().keys().collect::<Vec<_>>(),
                "strata": response.signals["longitudinal"]["strata"].as_array().unwrap().iter().map(|row| row.as_object().unwrap().keys().collect::<Vec<_>>()).collect::<Vec<_>>(),
                "exposures": response.signals["longitudinal"]["config_exposures"].as_array().unwrap().iter().map(|row| row.as_object().unwrap().keys().collect::<Vec<_>>()).collect::<Vec<_>>()
            },
            "signals":response.signals["signals"].as_array().unwrap().len(),
            "exemplars":response.signals["exemplars"].as_array().unwrap().iter().map(|sample| {
                json!({"fields":sample.as_object().unwrap().keys().collect::<Vec<_>>(),
                    "coordinates":sample["coordinates"].as_array().unwrap().len(),
                    "evidence":sample["evidence"].as_array().unwrap().len()})
            }).collect::<Vec<_>>()
        })
    };
    let check_summary_bounds = |response: &WindowResponse| {
        let mut core = response.signals.clone();
        let longitudinal = core
            .as_object_mut()
            .unwrap()
            .remove("longitudinal")
            .unwrap();
        // Keep the original core budget; v3 adds a separately bounded section.
        assert!(serde_json::to_vec(&core).unwrap().len() <= 8 * 1024);
        assert!(serde_json::to_vec(&longitudinal).unwrap().len() <= 16 * 1024);
        assert!(serde_json::to_vec(&response.signals).unwrap().len() <= 256 * 1024);
        let summary: anchor_facts::longitudinal::Longitudinal =
            serde_json::from_value(longitudinal).unwrap();
        summary.validate().unwrap();
        assert!(summary.strata.len() <= response.manifest.limit);
        assert!(summary.config_exposures.len() <= response.manifest.limit);
    };
    let mut input_shape = None;
    for graphs in [100, 1_000] {
        let mut bounded = request.clone();
        bounded.sources[0].scope = ResourceScope::Resources {
            ids: (0..graphs)
                .flat_map(|index| {
                    let run = format!("graph-{index:05}");
                    [
                        format!("trace/{run}/worker/1"),
                        anchor_facts::configuration_id(&configuration(&run).digest()),
                    ]
                })
                .collect(),
        };
        let response = process_capture(&bounded);
        let shape = model_input_shape(&response);
        if let Some(previous) = &input_shape {
            assert_eq!(&shape, previous);
        }
        input_shape = Some(shape.clone());
        let bytes = serde_json::to_vec(&response.signals).unwrap().len();
        check_summary_bounds(&response);
        println!(
            "{}",
            json!({"phase":"bounded_model_input","graphs":graphs,
            "summary_bytes":bytes,"shape":shape,"provider_runs":0})
        );
    }
    let phases = ["cold", "no_new", "100_new", "long_history_one_new"];
    let mut previous: Option<WindowResponse> = None;
    for phase in phases {
        if let Some(previous) = &previous {
            request = advance(&request, previous);
        }
        if phase == "100_new" {
            for index in 0..100 {
                observe(&world, &format!("graph-{index:05}"), 3_000);
                if index % 32 == 31 {
                    world.reopen_facts();
                }
            }
        } else if phase == "long_history_one_new" {
            observe(&world, "long-history", 3_001);
        }
        world.reopen_facts();
        let started = Instant::now();
        let response = process_capture(&request);
        let elapsed = started.elapsed().as_millis();
        let expected = match phase {
            "cold" => GRAPHS * RECORDS + LONG_RECORDS,
            "100_new" => 100,
            "long_history_one_new" => 1,
            _ => 0,
        };
        assert_eq!(response.coverage.seen_records, expected);
        let io = response.coverage.io.as_ref().unwrap();
        assert_eq!(io.records_parsed, expected);
        if expected == 0 {
            assert_eq!(io.history_bytes_read, 0);
        }
        let signals = response.signals["signals"].as_array().unwrap().len();
        let exemplars = response.signals["exemplars"].as_array().unwrap().len();
        assert!(signals <= request.limit && exemplars <= request.limit);
        if phase == "cold" {
            assert_eq!(&model_input_shape(&response), input_shape.as_ref().unwrap());
            check_summary_bounds(&response);
            assert!(
                response.signals["exemplars"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|sample| sample["code"] == "ordinary_sample")
            );
        }
        println!(
            "{}",
            json!({
                "phase":phase, "graphs":GRAPHS, "distinct_graph_configurations":GRAPHS + 1,
                "source":"kernel-facts", "provider_runs":0, "long_history_records":LONG_RECORDS,
                "resources":response.coverage.seen_resources, "records":expected,
                "elapsed_ms":elapsed, "io":io,
                "summary_bytes":serde_json::to_vec(&response.signals).unwrap().len(),
                "manifest_bytes":serde_json::to_vec(&response.manifest).unwrap().len(),
                "response_bytes":serde_json::to_vec(&response).unwrap().len()
            })
        );
        previous = Some(response);
    }
}

#[test]
fn facts_only_discovery_discloses_missing_quality_without_blocking_efficiency() {
    let world = World::new();
    support::r1(&world);
    let request = request(&world, &["kernel-facts"]);
    let response = capture(&request).unwrap();
    let table = &response.signals;
    assert!(
        table["not_applicable"]
            .as_array()
            .unwrap()
            .contains(&json!("grades"))
    );
    assert!(
        table["not_applicable"]
            .as_array()
            .unwrap()
            .contains(&json!("runs"))
    );
    assert_eq!(table["applicability"]["grades"], "not_granted");
    assert_eq!(table["applicability"]["run_lifecycle"], "not_granted");
    assert_eq!(
        table["applicability"]["observations_without_runs"],
        Value::Null
    );
    assert_eq!(
        table["applicability"]["runs_without_observations"],
        Value::Null
    );
    assert!(!table["signals"].as_array().unwrap().is_empty());
    assert!(
        table["exemplars"]
            .as_array()
            .unwrap()
            .iter()
            .any(|signal| signal["code"] == "ordinary_sample")
    );
    same_window(capture(&replay(&request, &response)).unwrap(), response);
}

#[test]
fn safe_view_capabilities_distinguish_missing_empty_and_unavailable_sources() {
    let world = World::new();
    let mut request = request(&world, &[]);
    for prefix in ["runs-view-", "grades-view-"] {
        let source_instance = format!("{prefix}fixture");
        let scope = ResourceScope::All {};
        let mut view = anchor_facts::SafeViewSnapshot {
            source_instance: source_instance.clone(),
            join_namespace: "fixture-install".into(),
            root_fingerprint: "a".repeat(64),
            owner_root_fingerprint: None,
            scope: scope.clone(),
            resources: Vec::new(),
            runs: Vec::new(),
            grades: Vec::new(),
            view_digest: String::new(),
        };
        view.seal();
        request.sources.push(WindowSource {
            source_instance,
            join_namespace: "fixture-install".into(),
            root: world.root("not-opened-by-safe-view"),
            scope,
        });
        request.safe_views.push(view);
    }
    let missing = capture(&request).unwrap();
    for capability in ["run_lifecycle", "grades"] {
        assert_eq!(
            missing.signals["applicability"][capability],
            "missing_source"
        );
    }
    assert!(missing.signals["applicability"]["runs_without_observations"].is_null());
    same_window(capture(&replay(&request, &missing)).unwrap(), missing);
    for view in &mut request.safe_views {
        view.owner_root_fingerprint = Some("b".repeat(64));
        view.seal();
    }
    let empty = capture(&request).unwrap();
    for capability in ["run_lifecycle", "grades"] {
        assert_eq!(empty.signals["applicability"][capability], "empty_in_scope");
    }
    request.safe_views.clear();
    assert!(
        capture(&request)
            .unwrap_err()
            .contains("safe view is missing")
    );
}

#[test]
fn frozen_namespaces_bind_joins_and_excluded_runs_without_colliding() {
    let left = World::new();
    let right = World::new();
    for (world, graph, task) in [(&left, "graph-a", "task-a"), (&right, "graph-b", "task-b")] {
        let mut configuration = support::configuration("worker");
        configuration.graph_digest = graph.into();
        world.run("same-run", 1);
        world.observe(
            "same-run",
            "worker",
            100,
            &configuration,
            vec![
                support::turn(1, 50, 10),
                support::lifecycle((1, 100), anchor_facts::Outcome::Completed, 1, 0),
            ],
        );
        world.result("same-run", "worker#1", 101, 1, true);
        world.grade("same-run", task, &[("quality", true)], 102);
    }
    let mut input = request(&left, &["kernel-facts", "kernel-runs", "kernel-grades"]);
    for source in &mut input.sources {
        source.join_namespace = "left".into();
        source.source_instance = format!("left-{}", source.source_instance);
    }
    let mut others = request(&right, &["kernel-facts", "kernel-runs", "kernel-grades"]).sources;
    for source in &mut others {
        source.join_namespace = "right".into();
        source.source_instance = format!("right-{}", source.source_instance);
    }
    input.sources.extend(others);
    let response = capture(&input).unwrap();
    assert_eq!(response.signals["applicability"]["graded_runs"], 2);
    assert_eq!(response.signals["applicability"]["complete_token_runs"], 2);
    assert_eq!(response.signals["applicability"]["grade_conflicts"], 0);
    assert_eq!(response.signals["applicability"]["graph_conflicts"], 0);
    input.exclude_runs = vec!["same-run".into()];
    assert!(capture(&input).unwrap_err().contains("ambiguous"));
    input.exclude_runs = vec!["left::same-run".into()];
    let response = capture(&input).unwrap();
    assert_eq!(response.signals["applicability"]["graded_runs"], 1);
    assert_eq!(response.signals["applicability"]["complete_token_runs"], 1);
    let mut changed = replay(&input, &response);
    changed.sources[0].join_namespace = "right".into();
    assert!(capture(&changed).is_err());
}

#[test]
fn v2_window_replay_keeps_empty_namespace_and_current_capture_requires_one() {
    let world = World::new();
    support::r1(&world);
    let mut request = request(&world, &["kernel-facts"]);
    let mut old = capture(&request).unwrap();
    old.manifest.schema_version = anchor_facts::window::PREVIOUS_SCHEMA_VERSION;
    old.manifest.analyzer_version = 3;
    for source in &mut old.manifest.sources {
        source.join_namespace.clear();
    }
    request.schema_version = anchor_facts::window::PREVIOUS_SCHEMA_VERSION;
    for source in &mut request.sources {
        source.join_namespace.clear();
    }
    request.manifest = Some(old.manifest.clone());
    let replayed = capture(&request).unwrap();
    assert_eq!(replayed.manifest, old.manifest);
    assert_eq!(replayed.manifest_digest, old.manifest.digest());
    assert!(
        !serde_json::to_string(&replayed.manifest)
            .unwrap()
            .contains("join_namespace")
    );
    request.schema_version = SCHEMA_VERSION;
    request.manifest = None;
    assert!(capture(&request).is_err());
}

#[test]
fn authorized_empty_sources_have_empty_status_not_missing_source_status() {
    let world = World::new();
    let mut input = request(&world, &["kernel-facts", "kernel-runs", "kernel-grades"]);
    for source in &mut input.sources {
        source.scope = ResourceScope::Resources { ids: vec![] };
    }
    let response = capture(&input).unwrap();
    for field in ["run_lifecycle", "observations", "grades"] {
        assert_eq!(
            response.signals["applicability"][field], "empty_in_scope",
            "{field}"
        );
    }
    assert_eq!(
        response.signals["applicability"]["observations_without_runs"],
        0
    );
    assert_eq!(
        response.signals["applicability"]["runs_without_observations"],
        0
    );
}
