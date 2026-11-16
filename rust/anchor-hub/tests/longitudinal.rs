#![allow(dead_code)]
mod support;
use anchor_facts::window::*;
use anchor_facts::{
    ProvenanceCategory as P, SafeGradeView, SafeNodeSlot, SafeRunStatus, SafeRunView,
    SafeViewSnapshot, view::FactRef,
};
use anchor_hub::window::{capture, evidence};
use serde_json::{Value, json};
use support::World;

fn view(kind: &str, count: usize, after: usize) -> SafeViewSnapshot {
    let source = format!("{kind}-view-fixture");
    let resource = format!(
        "{}/persistent",
        if kind == "runs" { "run" } else { "grade" }
    );
    let mut v = SafeViewSnapshot {
        source_instance: source,
        join_namespace: "fixture-install".into(),
        root_fingerprint: "a".repeat(64),
        owner_root_fingerprint: None,
        scope: ResourceScope::All {},
        resources: vec![ResourceWindow {
            resource,
            after,
            through: count,
            history_digest: "b".repeat(64),
            snapshot_digest: "c".repeat(64),
            prefix_digest: None,
            start_cursor: None,
            end_cursor: None,
        }],
        runs: vec![],
        grades: vec![],
        view_digest: String::new(),
    };
    v.seal();
    v
}
fn fact(v: &SafeViewSnapshot, place: usize) -> FactRef {
    FactRef {
        source_instance: v.source_instance.clone(),
        resource: v.resources[0].resource.clone(),
        place,
        record_digest: "d".repeat(64),
    }
}
fn run(v: &SafeViewSnapshot, place: usize, invocation: u64, at: u64) -> SafeRunView {
    SafeRunView {
        run_id: "persistent".into(),
        graph: "fixture".into(),
        graph_digest: anchor_facts::text_digest("fixture"),
        status: SafeRunStatus::Executing,
        created_at_ms: Some(10),
        last_at_ms: Some(at),
        settled_at_ms: None,
        accepted_slots: vec![SafeNodeSlot {
            node_id: "worker".into(),
            invocation,
            at_ms: Some(at),
            cost: 1,
            config: None,
        }],
        provenance: P::Production,
        fact_ref: fact(v, place),
    }
}
fn request(world: &World, views: Vec<SafeViewSnapshot>, now: u64) -> WindowRequest {
    let mut sources = vec![WindowSource {
        source_instance: "facts".into(),
        join_namespace: "fixture-install".into(),
        root: world.root("kernel-facts"),
        scope: ResourceScope::All {},
    }];
    sources.extend(views.iter().map(|v| WindowSource {
        source_instance: v.source_instance.clone(),
        join_namespace: v.join_namespace.clone(),
        root: world.state().join("MUST-NOT-OPEN-RAW-RUNS"),
        scope: v.scope.clone(),
    }));
    WindowRequest {
        schema_version: SCHEMA_VERSION,
        sources,
        safe_views: views,
        support_history: true,
        manifest: None,
        advance_from: None,
        exclude_runs: vec![],
        now_ms: now,
        limit: 20,
    }
}
fn longitudinal(response: &WindowResponse) -> anchor_facts::longitudinal::Longitudinal {
    let value = serde_json::from_value(response.signals["longitudinal"].clone()).unwrap();
    anchor_facts::longitudinal::Longitudinal::validate(&value).unwrap();
    value
}
fn observe(world: &World, invocation: u64, at: u64) {
    let mut config = support::configuration("worker");
    config.graph_digest = anchor_facts::text_digest("fixture");
    let key = anchor_facts::Invocation {
        run_id: "persistent".into(),
        graph_digest: config.graph_digest.clone(),
        node_id: "worker".into(),
        invocation,
    };
    if invocation == 1 {
        world.observe(
            "persistent",
            "worker",
            at,
            &config,
            vec![
                support::turn(1, at, 5),
                support::lifecycle((at - 5, at), anchor_facts::Outcome::Completed, 1, 0),
            ],
        );
    } else {
        world.clock.set(at);
        let id = key.trace();
        world
            .facts
            .create(
                anchor_kernel::Resource::new(&id, anchor_facts::OBSERVATION, Value::Null)
                    .configured(config.digest()),
            )
            .unwrap();
        world
            .facts
            .submit(
                &anchor_facts::recorder(&config.digest()),
                0,
                anchor_kernel::Submission::new(id, 0, anchor_kernel::Action::Write).content(json!(
                    anchor_facts::Observation {
                        version: 1,
                        invocation: key,
                        facts: vec![
                            support::turn(1, at, 5),
                            support::lifecycle(
                                (at - 5, at),
                                anchor_facts::Outcome::Completed,
                                1,
                                0
                            )
                        ]
                    }
                )),
            )
            .unwrap();
    }
}
#[test]
fn three_cycles_join_late_regrade_without_cloning_persistent_quality_or_reopening_old_work() {
    let world = World::new();
    observe(&world, 1, 20);
    let mut runs = view("runs", 1, 0);
    runs.runs.push(run(&runs, 1, 1, 20));
    runs.seal();
    let grades = view("grades", 0, 0);
    let first_request = request(&world, vec![runs.clone(), grades], 100);
    let first = capture(&first_request).unwrap();
    assert_eq!(first.signals["applicability"]["graded_runs"], 0);
    let first_bytes = serde_json::to_vec(&first).unwrap();
    observe(&world, 2, 120);
    let other = support::configuration("other");
    world.observe(
        "mixed-task",
        "other",
        130,
        &other,
        vec![support::lifecycle(
            (125, 130),
            anchor_facts::Outcome::Completed,
            1,
            0,
        )],
    );
    runs.resources[0].after = 1;
    runs.resources[0].through = 2;
    runs.runs.push(run(&runs, 2, 2, 120));
    runs.seal();
    let mut grades = view("grades", 2, 0);
    for (place, passed) in [(1, true), (2, false)] {
        grades.grades.push(SafeGradeView {
            run_id: "persistent".into(),
            task: "fixture-task".into(),
            task_version: 1,
            grader_config_digest: "e".repeat(64),
            checks: vec![anchor_facts::Check {
                name: "correct".into(),
                passed,
            }],
            at_ms: Some(130 + place as u64),
            provenance: P::Production,
            fact_ref: fact(&grades, place),
        });
    }
    grades.seal();
    let mut second_request = request(&world, vec![runs.clone(), grades.clone()], 200);
    second_request.advance_from = Some(first.manifest.clone());
    let second = capture(&second_request).unwrap();
    let summary = longitudinal(&second);
    assert_eq!(summary.late_grades_joined, 1);
    assert!(summary.revised_prior_interval);
    assert!(summary.composition_changed);
    assert_eq!(
        summary
            .strata
            .iter()
            .map(|s| s.baseline.graded_runs + s.observation.graded_runs)
            .sum::<usize>(),
        1
    );
    assert_eq!(
        summary
            .strata
            .iter()
            .map(|s| s.baseline.invocations + s.observation.invocations)
            .sum::<usize>(),
        3
    );
    assert_eq!(
        summary
            .strata
            .iter()
            .map(|s| s.observation.invocations)
            .sum::<usize>(),
        2
    );
    assert_eq!(
        summary
            .strata
            .iter()
            .filter_map(|s| s.baseline.quality.as_ref())
            .next()
            .unwrap()
            .mean,
        0.0
    );
    assert!(summary.strata.iter().all(|s| s.cost_delta.is_none()));
    assert!(
        second.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["code"] == "check_failing"
                && s["evidence"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|k| k["root"] == "grades-view-fixture" && k["place"] == 2))
    );
    assert_eq!(serde_json::to_vec(&first).unwrap(), first_bytes);
    runs.resources[0].after = 2;
    runs.seal();
    grades.resources[0].after = 2;
    grades.seal();
    let mut third_request = request(&world, vec![runs, grades], 300);
    third_request.advance_from = Some(second.manifest.clone());
    let third = capture(&third_request).unwrap();
    assert_eq!(third.coverage.io.as_ref().unwrap().records_parsed, 0);
    assert_eq!(third.coverage.io.as_ref().unwrap().history_bytes_read, 0);
    assert!(third.coverage.support_io.as_ref().unwrap().records_parsed > 0);
    assert_eq!(third.signals["signals"], json!([]));
    assert_eq!(third.signals["exemplars"], json!([]));
    for (req, res, key) in [
        (
            &first_request,
            &first,
            EvidenceKey {
                root: "facts".into(),
                resource: "trace/persistent/worker/1".into(),
                place: 1,
            },
        ),
        (
            &second_request,
            &second,
            EvidenceKey {
                root: "grades-view-fixture".into(),
                resource: "grade/persistent".into(),
                place: 2,
            },
        ),
    ] {
        let mut replay = req.clone();
        replay.manifest = Some(res.manifest.clone());
        replay.advance_from = None;
        let drill = evidence(&EvidenceRequest {
            schema_version: 3,
            window: replay,
            keys: vec![key.clone()],
        })
        .unwrap();
        assert_eq!(drill.items[0].key, key);
    }
    let mut replay = third_request;
    replay.manifest = Some(third.manifest);
    replay.advance_from = None;
    assert!(
        evidence(&EvidenceRequest {
            schema_version: 3,
            window: replay,
            keys: vec![EvidenceKey {
                root: "facts".into(),
                resource: "trace/persistent/worker/1".into(),
                place: 1
            }]
        })
        .is_err()
    );
    for encoded in [
        serde_json::to_string(&first_request).unwrap(),
        serde_json::to_string(&second).unwrap(),
        String::from_utf8(first_bytes).unwrap(),
    ] {
        assert!(!encoded.contains(support::SECRET));
        assert!(!encoded.contains("expected"));
    }
}

#[test]
fn safe_run_results_without_model_observations_and_provenance_are_not_production_mix() {
    let world = World::new();
    let mut runs = view("runs", 1, 0);
    runs.runs.push(run(&runs, 1, 1, 20));
    runs.seal();
    let captured = capture(&request(&world, vec![runs], 100)).unwrap();
    assert_eq!(
        captured.signals["applicability"]["runs_without_observations"],
        1
    );
    assert!(
        captured.signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["code"] == "runs_without_observations")
    );
    let facts_only = request(&world, vec![], 100);
    let capture = capture(&facts_only).unwrap();
    assert!(capture.manifest.safe_views.is_empty());
    assert_eq!(
        capture.signals["applicability"]["runs_without_observations"],
        Value::Null
    );
}

#[test]
fn category_denominators_and_ordinary_evidence_do_not_mix_experiment_meta_or_unknown() {
    let world = World::new();
    let mut runs = view("runs", 0, 0);
    runs.resources.clear();
    for (id, provenance) in [
        ("ordinary", P::Production),
        ("development", P::Experiment),
        ("dreaming", P::DreamingMeta),
        ("legacy", P::Unknown),
    ] {
        let resource = format!("run/{id}");
        runs.resources.push(ResourceWindow {
            resource: resource.clone(),
            after: 0,
            through: 1,
            history_digest: "b".repeat(64),
            snapshot_digest: "c".repeat(64),
            prefix_digest: None,
            start_cursor: None,
            end_cursor: None,
        });
        let mut result = run(&runs, 1, 1, 20);
        result.run_id = id.into();
        result.provenance = provenance;
        result.status = SafeRunStatus::Aborted;
        result.settled_at_ms = Some(20);
        result.fact_ref.resource = resource;
        runs.runs.push(result);
    }
    runs.resources.sort_by(|a, b| a.resource.cmp(&b.resource));
    runs.seal();
    let captured = capture(&request(&world, vec![runs.clone()], 100)).unwrap();
    let summary = longitudinal(&captured);
    for category in [P::Production, P::Experiment, P::DreamingMeta, P::Unknown] {
        assert_eq!(summary.provenance_runs[&category], 1);
        assert_eq!(
            summary
                .strata
                .iter()
                .filter(|s| s.provenance == category)
                .map(|s| s.observation.runs)
                .sum::<usize>(),
            1
        );
    }
    for signal in captured.signals["signals"]
        .as_array()
        .unwrap()
        .iter()
        .chain(captured.signals["exemplars"].as_array().unwrap())
    {
        for key in signal["evidence"].as_array().unwrap() {
            assert_ne!(key["resource"], "run/development");
            assert_ne!(key["resource"], "run/dreaming");
        }
    }
    let outcome_signals: Vec<_> = captured.signals["signals"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["code"] == "task_not_done")
        .collect();
    assert_eq!(outcome_signals.len(), 2);
    assert!(outcome_signals.iter().all(|s| s["runs"] == 1));
    for resource in &mut runs.resources {
        resource.after = resource.through;
    }
    runs.runs[0].accepted_slots[0].cost = 99;
    runs.seal();
    let mut next = request(&world, vec![runs], 200);
    next.advance_from = Some(captured.manifest);
    assert!(capture(&next).is_err());
}

#[test]
fn excluded_runs_are_removed_from_safe_views_and_physical_support_before_analysis() {
    safe_view_exclusions(None);
}

#[test]
#[ignore = "requires ANCHOR_TEST_LEGACY_HUB pointing to retained analyzer-v4 binary"]
fn retained_v4_safe_view_exclusions_fail_without_rewriting_old_rankings() {
    let binary = std::env::var_os("ANCHOR_TEST_LEGACY_HUB").expect("retained v4 binary");
    safe_view_exclusions(Some(std::path::PathBuf::from(binary)));
}

fn safe_view_exclusions(legacy: Option<std::path::PathBuf>) {
    let legacy_capture = |input: &WindowRequest| -> WindowResponse {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new(legacy.as_ref().unwrap())
            .arg("window")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(input).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let world = World::new();
    observe(&world, 1, 20);
    let mut runs = view("runs", 1, 0);
    runs.runs.push(run(&runs, 1, 1, 20));
    runs.seal();
    let mut grades = view("grades", 1, 0);
    grades.grades.push(SafeGradeView {
        run_id: "persistent".into(),
        task: "fixture-task".into(),
        task_version: 1,
        grader_config_digest: "e".repeat(64),
        checks: vec![anchor_facts::Check {
            name: "correct".into(),
            passed: false,
        }],
        at_ms: Some(30),
        provenance: P::Production,
        fact_ref: fact(&grades, 1),
    });
    grades.seal();
    let mut input = request(&world, vec![runs, grades], 100);
    input.exclude_runs = vec!["another-install::persistent".into()];
    assert_eq!(
        capture(&input).unwrap().signals["applicability"]["graded_runs"],
        1
    );
    if legacy.is_some() {
        let old = legacy_capture(&input);
        let mut replay = input.clone();
        replay.manifest = Some(old.manifest.clone());
        let current = capture(&replay).unwrap();
        assert_eq!(current.signals, old.signals);
        assert_eq!(current.manifest_digest, old.manifest_digest);
    }
    for excluded in ["persistent", "fixture-install::persistent"] {
        input.exclude_runs = vec![excluded.into()];
        let captured = capture(&input).unwrap();
        if legacy.is_some() {
            let old = legacy_capture(&input);
            assert_eq!(old.manifest.analyzer_version, 4);
            assert_eq!(old.signals["applicability"]["graded_runs"], 1);
            let mut replay = input.clone();
            replay.manifest = Some(old.manifest);
            assert!(
                capture(&replay)
                    .unwrap_err()
                    .contains("legacy safe-view exclusions")
            );
            assert!(
                anchor_hub::window::signal_page(&SignalPageRequest {
                    schema_version: SCHEMA_VERSION,
                    window: replay.clone(),
                    offset: 1,
                    limit: 1,
                })
                .unwrap_err()
                .contains("legacy safe-view exclusions")
            );
            assert!(
                evidence(&EvidenceRequest {
                    schema_version: SCHEMA_VERSION,
                    window: replay,
                    keys: vec![EvidenceKey {
                        root: "grades-view-fixture".into(),
                        resource: "grade/persistent".into(),
                        place: 1,
                    }],
                })
                .unwrap_err()
                .contains("legacy safe-view exclusions")
            );
        }
        assert_eq!(captured.signals["applicability"]["graded_runs"], 0);
        assert_eq!(longitudinal(&captured).strata_total, 0);
        assert_eq!(captured.signals["signals"], json!([]));
        assert_eq!(captured.signals["exemplars"], json!([]));
        // Authorization and frozen source positions are retained, not rewritten.
        assert_eq!(captured.manifest.safe_views, input.safe_views);
        let mut replay = input.clone();
        replay.manifest = Some(captured.manifest.clone());
        assert_eq!(capture(&replay).unwrap().signals, captured.signals);
        let mut unsafe_legacy = replay.clone();
        unsafe_legacy.manifest.as_mut().unwrap().analyzer_version = 4;
        assert!(
            capture(&unsafe_legacy)
                .unwrap_err()
                .contains("legacy safe-view exclusions")
        );
        assert!(
            anchor_hub::window::signal_page(&anchor_facts::window::SignalPageRequest {
                schema_version: SCHEMA_VERSION,
                window: unsafe_legacy,
                offset: 1,
                limit: 1,
            })
            .unwrap_err()
            .contains("legacy safe-view exclusions")
        );
        assert!(
            evidence(&EvidenceRequest {
                schema_version: SCHEMA_VERSION,
                window: replay,
                keys: vec![EvidenceKey {
                    root: "grades-view-fixture".into(),
                    resource: "grade/persistent".into(),
                    place: 1,
                }],
            })
            .is_err()
        );
        let mut next = input.clone();
        next.advance_from = Some(captured.manifest);
        next.now_ms = 200;
        for view in &mut next.safe_views {
            for resource in &mut view.resources {
                resource.after = resource.through;
                resource.through += 1;
            }
            if !view.runs.is_empty() {
                view.runs.push(run(view, 2, 2, 120));
            } else {
                let mut grade = view.grades[0].clone();
                grade.fact_ref = fact(view, 2);
                grade.at_ms = Some(130);
                view.grades.push(grade);
            }
            view.seal();
        }
        let advanced = capture(&next).unwrap();
        assert_eq!(advanced.signals["applicability"]["graded_runs"], 0);
        assert_eq!(longitudinal(&advanced).strata_total, 0);
        assert_eq!(advanced.coverage.seen_records, 2);
        assert_eq!(advanced.manifest.safe_views, next.safe_views);
        assert!(
            advanced
                .coverage
                .support_io
                .as_ref()
                .unwrap()
                .records_parsed
                > 0
        );
        next.advance_from = None;
        next.manifest = Some(advanced.manifest);
        assert_eq!(capture(&next).unwrap().signals, advanced.signals);
    }
    let mut multiple = input.clone();
    for mut view in input.safe_views {
        view.source_instance.push_str("-other");
        view.join_namespace = "another-install".into();
        for fact in view
            .runs
            .iter_mut()
            .map(|run| &mut run.fact_ref)
            .chain(view.grades.iter_mut().map(|grade| &mut grade.fact_ref))
        {
            fact.source_instance = view.source_instance.clone();
        }
        view.seal();
        multiple.sources.push(WindowSource {
            source_instance: view.source_instance.clone(),
            join_namespace: view.join_namespace.clone(),
            root: world.state().join("MUST-NOT-OPEN-RAW-RUNS"),
            scope: view.scope.clone(),
        });
        multiple.safe_views.push(view);
    }
    let isolated = capture(&multiple).unwrap();
    assert_eq!(isolated.signals["applicability"]["graded_runs"], 1);
    assert_eq!(longitudinal(&isolated).provenance_runs[&P::Production], 1);
    multiple.exclude_runs = vec!["persistent".into()];
    assert!(
        capture(&multiple)
            .unwrap_err()
            .contains("ambiguous across namespaces")
    );
}
