#![allow(dead_code)]
mod support;

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Instant;

use anchor_facts::longitudinal::Longitudinal;
use anchor_facts::view::FactRef;
use anchor_facts::window::{
    MAX_TRANSPORT_BYTES, ResourceScope, ResourceWindow, SCHEMA_VERSION, WindowRequest,
    WindowResponse, WindowSource,
};
use anchor_facts::{
    ProvenanceCategory, SafeGradeView, SafeNodeSlot, SafeRunStatus, SafeRunView, SafeViewSnapshot,
};
use anchor_hub::window::capture;
use serde_json::{Value, json};
use support::World;

const GRAPHS: usize = 10_000;
const RECORDS: usize = 32;
const LONG_RECORDS: usize = 2_500;
const FIRST_CLOSE: u64 = 4_000;
const SECOND_CLOSE: u64 = 8_000;

fn configuration(id: &str) -> anchor_facts::AgentConfiguration {
    let mut config = support::configuration("worker");
    config.graph_digest = anchor_facts::text_digest(id);
    config
}

fn observe(world: &World, run: &str, step: u64) {
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
}

fn empty_view(kind: &str) -> SafeViewSnapshot {
    let mut view = SafeViewSnapshot {
        source_instance: format!("{kind}-view-acceptance"),
        join_namespace: "fixture-install".into(),
        root_fingerprint: anchor_facts::text_digest(kind),
        owner_root_fingerprint: Some(anchor_facts::text_digest(&format!("owner-{kind}"))),
        scope: ResourceScope::All {},
        resources: vec![],
        runs: vec![],
        grades: vec![],
        view_digest: String::new(),
    };
    view.seal();
    view
}

fn position(id: &str, count: usize) -> ResourceWindow {
    ResourceWindow {
        resource: id.into(),
        after: 0,
        through: count,
        history_digest: anchor_facts::text_digest(&format!("history-{id}-{count}")),
        snapshot_digest: anchor_facts::text_digest(&format!("snapshot-{id}-{count}")),
        prefix_digest: Some(anchor_facts::text_digest(&format!("prefix-{id}-{count}"))),
        start_cursor: None,
        end_cursor: None,
    }
}

fn reference(view: &SafeViewSnapshot, resource: &str, place: usize) -> FactRef {
    FactRef {
        source_instance: view.source_instance.clone(),
        resource: resource.into(),
        place,
        record_digest: anchor_facts::text_digest(&format!("record-{resource}-{place}")),
    }
}

// The owner chooses these labels and expected counts before querying the Hub.
// These DTO fixtures benchmark frozen transport; Host export is verified separately.
fn views(graphs: usize) -> Vec<SafeViewSnapshot> {
    let mut runs = empty_view("runs");
    let mut grades = empty_view("grades");
    for index in 0..graphs {
        let id = format!("graph-{index:05}");
        let resource = format!("run/{id}");
        runs.resources.push(position(&resource, 1));
        runs.runs.push(SafeRunView {
            run_id: id.clone(),
            graph: id.clone(),
            graph_digest: configuration(&id).graph_digest,
            status: if index < 100 {
                SafeRunStatus::Executing
            } else {
                SafeRunStatus::Done
            },
            created_at_ms: Some(1),
            last_at_ms: Some(40),
            settled_at_ms: (index >= 100).then_some(40),
            accepted_slots: vec![SafeNodeSlot {
                node_id: "worker".into(),
                invocation: 1,
                at_ms: Some(40),
                cost: 1,
                config: Some(configuration(&id).digest()),
            }],
            provenance: ProvenanceCategory::Production,
            fact_ref: reference(&runs, &resource, 1),
        });
        if index.is_multiple_of(10) {
            add_grade(&mut grades, &id, 1, true, 60);
        }
    }
    runs.seal();
    grades.seal();
    let mut conflict = empty_view("grades");
    conflict.source_instance = "grades-view-conflict".into();
    conflict.root_fingerprint = anchor_facts::text_digest("conflicting-owner");
    add_grade(&mut conflict, "graph-00000", 1, false, 70);
    conflict.grades[0].task = "conflicting-independent-task".into();
    conflict.seal();
    vec![runs, grades, conflict]
}

fn add_grade(view: &mut SafeViewSnapshot, run: &str, place: usize, passed: bool, at: u64) {
    let resource = format!("grade/{run}");
    if let Some(p) = view.resources.iter_mut().find(|p| p.resource == resource) {
        p.through = place;
    } else {
        view.resources.push(position(&resource, place));
    }
    view.grades.push(SafeGradeView {
        run_id: run.into(),
        task: "independently-labelled-task".into(),
        task_version: 1,
        grader_config_digest: anchor_facts::text_digest("fixture-independent-grader"),
        checks: vec![anchor_facts::Check {
            name: "correct".into(),
            passed,
        }],
        at_ms: Some(at),
        provenance: ProvenanceCategory::Production,
        fact_ref: reference(view, &resource, place),
    });
    view.resources.sort_by(|a, b| a.resource.cmp(&b.resource));
}

fn request(world: &World, graphs: usize) -> WindowRequest {
    let safe_views = views(graphs);
    let ids = (0..graphs)
        .flat_map(|index| {
            let id = format!("graph-{index:05}");
            [
                format!("trace/{id}/worker/1"),
                anchor_facts::configuration_id(&configuration(&id).digest()),
            ]
        })
        .chain(
            (graphs == GRAPHS)
                .then(|| "long-history".to_owned())
                .into_iter()
                .flat_map(|id| {
                    [
                        format!("trace/{id}/worker/1"),
                        anchor_facts::configuration_id(&configuration(&id).digest()),
                    ]
                }),
        )
        .collect();
    let mut sources = vec![WindowSource {
        source_instance: "facts".into(),
        join_namespace: "fixture-install".into(),
        root: world.root("kernel-facts"),
        scope: ResourceScope::Resources { ids },
    }];
    sources.extend(safe_views.iter().map(|view| WindowSource {
        source_instance: view.source_instance.clone(),
        join_namespace: view.join_namespace.clone(),
        // A safe view must never cause the Hub to open a raw owner root.
        root: world.state().join("MUST-NOT-OPEN-RAW-OWNERS"),
        scope: view.scope.clone(),
    }));
    WindowRequest {
        schema_version: SCHEMA_VERSION,
        sources,
        safe_views,
        support_history: true,
        manifest: None,
        advance_from: None,
        exclude_runs: vec![],
        now_ms: FIRST_CLOSE,
        limit: 8,
    }
}

fn advance(mut request: WindowRequest, parent: &WindowResponse, now: u64) -> WindowRequest {
    request.now_ms = now;
    request.advance_from = Some(parent.manifest.clone());
    for view in &mut request.safe_views {
        for p in &mut view.resources {
            p.after = p.through;
        }
        view.seal();
    }
    request
}

fn update_views(request: &mut WindowRequest, graphs: usize) {
    let runs = &mut request.safe_views[0];
    for index in 0..graphs.min(100) {
        let mut next = runs.runs[index].clone();
        next.last_at_ms = Some(6_000);
        next.accepted_slots[0].at_ms = Some(6_000);
        next.fact_ref = reference(runs, &next.fact_ref.resource, 2);
        runs.runs.push(next);
        runs.resources[index].through = 2;
    }
    runs.seal();
    let grades = &mut request.safe_views[1];
    for index in 0..graphs {
        let id = format!("graph-{index:05}");
        if index.is_multiple_of(10) {
            add_grade(grades, &id, 2, false, 6_100);
        } else if index % 10 == 1 {
            add_grade(grades, &id, 1, true, 6_101);
        }
    }
    grades.seal();
}

fn longitudinal(response: &WindowResponse) -> Longitudinal {
    let summary: Longitudinal =
        serde_json::from_value(response.signals["longitudinal"].clone()).unwrap();
    summary.validate().unwrap();
    summary
}

fn assert_summary(response: &WindowResponse, request: &WindowRequest) {
    assert!(serde_json::to_vec(&response.signals).unwrap().len() < 256 * 1024);
    for field in ["signals", "exemplars"] {
        let entries = response.signals[field].as_array().unwrap();
        assert!(entries.len() <= request.limit);
        for entry in entries {
            assert!(entry["coordinates"].as_array().unwrap().len() <= 5);
            assert!(entry["evidence"].as_array().unwrap().len() <= 5);
        }
    }
    let summary = longitudinal(response);
    assert!(summary.strata.len() <= request.limit);
    assert!(summary.config_exposures.len() <= request.limit);
    assert!(
        !serde_json::to_string(response)
            .unwrap()
            .contains(support::SECRET)
    );
}

#[test]
fn labelled_multisource_three_cycle_fixture_has_exact_late_grade_and_conflict_counts() {
    let world = World::new();
    for index in 0..20 {
        observe(&world, &format!("graph-{index:05}"), 1);
    }
    let first_request = request(&world, 20);
    let first = capture(&first_request).unwrap();
    // Two grades minus one conflict: owner-specified answers, not analyzer-generated labels.
    assert_eq!(first.signals["applicability"]["graded_runs"], 1);
    assert_eq!(first.signals["applicability"]["grade_conflicts"], 1);
    let mut second_request = advance(first_request, &first, SECOND_CLOSE);
    update_views(&mut second_request, 20);
    let second = capture(&second_request).unwrap();
    assert_eq!(second.signals["applicability"]["graded_runs"], 3);
    assert_eq!(second.signals["applicability"]["grade_conflicts"], 1);
    let summary = longitudinal(&second);
    assert_eq!(summary.late_grades_joined, 3);
    assert!(summary.revised_prior_interval);
    assert_eq!(summary.provenance_runs[&ProvenanceCategory::Production], 20);
    let third_request = advance(second_request, &second, 12_000);
    let third = capture(&third_request).unwrap();
    assert_eq!(third.coverage.seen_records, 0);
    assert_eq!(third.coverage.io.as_ref().unwrap().records_parsed, 0);
    assert_eq!(third.coverage.io.as_ref().unwrap().history_bytes_read, 0);
    assert!(third.coverage.support_io.as_ref().unwrap().records_parsed > 0);
    assert_eq!(third.signals["signals"], json!([]));
    assert_eq!(third.signals["exemplars"], json!([]));
    assert_eq!(longitudinal(&third).late_grades_joined, 0);
    assert_summary(&third, &third_request);
    println!(
        "{}",
        json!({"fixture":"labelled_multisource_three_cycle", "owner_expected": {
        "runs":20,"initial_matched_grades":1,"matched_grades_after_late_and_regrade":3,
        "grade_conflicts":1,"late_grades_joined":3,"idle_duplicate_samples":0},"actual":"all assertions passed"})
    );
}

fn process_capture(request: &WindowRequest) -> (WindowResponse, Value) {
    let started = Instant::now();
    let input = serde_json::to_vec(request).unwrap();
    let request_serialize_ms = started.elapsed().as_millis();
    assert!(
        input.len() <= MAX_TRANSPORT_BYTES,
        "request exceeds 64 MiB: {}",
        input.len()
    );
    let started = Instant::now();
    let mut child = Command::new("/usr/bin/time")
        .args([
            "-f",
            "anchor-hub-peak-rss-kib=%M",
            env!("CARGO_BIN_EXE_anchor-hub"),
            "window",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let output = child.wait_with_output().unwrap();
    let subprocess_total_ms = started.elapsed().as_millis();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() <= MAX_TRANSPORT_BYTES);
    assert!(
        subprocess_total_ms < 120_000,
        "subprocess exceeds Host deadline"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    let child_peak_rss_kib: usize = stderr
        .trim()
        .strip_prefix("anchor-hub-peak-rss-kib=")
        .unwrap()
        .parse()
        .unwrap();
    let started = Instant::now();
    let response = serde_json::from_slice(&output.stdout).unwrap();
    let response_deserialize_ms = started.elapsed().as_millis();
    (
        response,
        json!({"request_bytes":input.len(),"response_bytes":output.stdout.len(),
        "request_serialize_ms":request_serialize_ms,"subprocess_total_ms":subprocess_total_ms,
        "response_deserialize_ms":response_deserialize_ms,"hub_child_peak_rss_kib":child_peak_rss_kib,
        "rss_scope":"only anchor-hub child, excludes fixture generation and parent"}),
    )
}

#[test]
#[ignore = "low-frequency 10,000-graph multi-source/multi-cycle benchmark"]
fn ten_thousand_graph_multisource_multicycle_costs_are_measured() {
    let generated = Instant::now();
    let mut world = World::new();
    for index in 0..GRAPHS {
        for step in 0..RECORDS {
            observe(&world, &format!("graph-{index:05}"), step as u64);
        }
        if index % 32 == 31 {
            world.reopen_facts();
        }
    }
    for step in 0..LONG_RECORDS {
        observe(&world, "long-history", step as u64);
    }
    world.reopen_facts();
    println!(
        "{}",
        json!({"phase":"fixture_generation","elapsed_ms":generated.elapsed().as_millis(),
        "physical_records":GRAPHS*RECORDS+LONG_RECORDS,"provider_runs":0,"fixture_peak_rss":"not measured"})
    );
    for graphs in [100, 1_000, GRAPHS] {
        let mut request = request(&world, graphs);
        let mut previous: Option<WindowResponse> = None;
        for phase in ["cold", "no_new", "100_new", "long_history_one_new"] {
            if let Some(parent) = &previous {
                let now = if phase == "no_new" {
                    FIRST_CLOSE + 1
                } else if phase == "100_new" {
                    SECOND_CLOSE
                } else {
                    12_000
                };
                request = advance(request, parent, now);
            }
            let delta_facts = match phase {
                "cold" => graphs * RECORDS + if graphs == GRAPHS { LONG_RECORDS } else { 0 },
                "100_new" => {
                    update_views(&mut request, graphs);
                    if graphs == GRAPHS {
                        for index in 0..100 {
                            observe(&world, &format!("graph-{index:05}"), 6_000);
                        }
                        100
                    } else {
                        0
                    }
                }
                "long_history_one_new" if graphs == GRAPHS => {
                    observe(&world, "long-history", 9_000);
                    1
                }
                _ => 0,
            };
            world.reopen_facts();
            let started = Instant::now();
            let direct = capture(&request).unwrap();
            let capture_load_project_analyze_ms = started.elapsed().as_millis();
            assert_eq!(
                direct.coverage.io.as_ref().unwrap().records_parsed,
                delta_facts
            );
            if delta_facts == 0 {
                assert_eq!(direct.coverage.io.as_ref().unwrap().history_bytes_read, 0);
            }
            assert_summary(&direct, &request);
            if phase == "no_new" {
                assert_eq!(direct.signals["signals"], json!([]));
                assert_eq!(direct.signals["exemplars"], json!([]));
            }
            let expected_graded = if matches!(phase, "100_new" | "long_history_one_new") {
                graphs / 5 - 1
            } else {
                graphs / 10 - 1
            };
            assert_eq!(
                direct.signals["applicability"]["graded_runs"],
                expected_graded
            );
            assert_eq!(direct.signals["applicability"]["grade_conflicts"], 1);
            assert_eq!(
                longitudinal(&direct).late_grades_joined,
                if phase == "100_new" {
                    graphs / 5 - 1
                } else {
                    0
                }
            );
            let started = Instant::now();
            let direct_bytes = serde_json::to_vec(&direct).unwrap();
            let response_serialize_ms = started.elapsed().as_millis();
            let (response, process) = process_capture(&request);
            assert_eq!(response, direct);
            println!(
                "{}",
                json!({"phase":phase,"graphs":graphs,"sources":4,"source_classes":["facts","safe_run_view","safe_grade_view"],
                "capture_load_project_analyze_ms":capture_load_project_analyze_ms,"response_serialize_ms":response_serialize_ms,
                "summary_bytes":serde_json::to_vec(&response.signals).unwrap().len(),"manifest_bytes":serde_json::to_vec(&response.manifest).unwrap().len(),
                "response_bytes":direct_bytes.len(),"io":response.coverage.io,"support_io":response.coverage.support_io,
                "matched_grades":expected_graded,"process":process,"provider_runs":0,
                "host_export":"not measured by this DTO fixture","query_only":"not separately exposed; included in capture_load_project_analyze_ms"})
            );
            previous = Some(response);
        }
    }
}

#[derive(Default)]
struct LoadedFacts {
    values:
        std::collections::BTreeMap<String, (anchor_kernel::Resource, Vec<anchor_kernel::Record>)>,
}

impl anchor_hub::source::HistorySource for LoadedFacts {
    fn root(&self) -> &str {
        "facts"
    }
    fn namespace(&self) -> &str {
        "fixture-install"
    }
    fn capabilities(&self) -> anchor_hub::source::SourceCapabilities {
        anchor_hub::source::SourceCapabilities {
            observations: true,
            runs: false,
            grades: false,
        }
    }
    fn list(&self) -> Result<Vec<String>, anchor_hub::source::SourceError> {
        Ok(self.values.keys().cloned().collect())
    }
    fn load(
        &self,
        id: &str,
    ) -> Result<Option<anchor_kernel::Resource>, anchor_hub::source::SourceError> {
        Ok(self.values.get(id).map(|(resource, _)| resource.clone()))
    }
    fn history_len(&self, id: &str) -> Result<usize, anchor_hub::source::SourceError> {
        Ok(self.values.get(id).map_or(0, |(_, records)| records.len()))
    }
    fn history_after(
        &self,
        id: &str,
        after: usize,
    ) -> Result<Vec<anchor_kernel::Record>, anchor_hub::source::SourceError> {
        Ok(self.values.get(id).map_or_else(Vec::new, |(_, records)| {
            records.iter().skip(after).cloned().collect()
        }))
    }
}

fn process_memory() -> Value {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kib = |key: &str| {
        status.lines().find_map(|line| {
            line.strip_prefix(key)
                .and_then(|tail| tail.split_whitespace().next())
                .and_then(|n| n.parse::<u64>().ok())
        })
    };
    json!({"rss_kib":kib("VmRSS:"),"peak_rss_kib":kib("VmHWM:")})
}

fn reset_peak() -> Value {
    // Linux exposes a per-process peak reset; retain resident baseline explicitly.
    std::fs::write("/proc/self/clear_refs", "5\n").unwrap();
    process_memory()
}

fn fixture_root() -> std::path::PathBuf {
    let path = std::fs::read_to_string("/tmp/anchor-s6-stage-fixture-path")
        .expect("create the explicitly selected private fixture copy before this ignored test");
    let root = std::path::PathBuf::from(path.trim()).join("kernel-facts");
    let stage = root.parent().unwrap();
    assert_eq!(
        stage.parent(),
        Some(std::path::Path::new("/dev/shm/anchor-bench-tmp")),
        "only private benchmark fixture copies are permitted"
    );
    assert!(
        stage
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("stage."),
        "only private benchmark fixture copies are permitted"
    );
    assert!(root.is_dir());
    root
}

#[test]
#[ignore = "manual per-phase measurement of the explicitly selected private scale fixture"]
fn cached_projection_and_query_phases_are_measured_separately() {
    use anchor_hub::query::Query;
    use anchor_hub::source::{DataRoot, HistorySource};
    use anchor_hub::table::{FactTable, MemoryTable, Projector};
    let root = fixture_root();
    let started = Instant::now();
    let source = DataRoot::open(&root, "facts")
        .unwrap()
        .with_namespace("fixture-install");
    let mut loaded = LoadedFacts::default();
    for id in source.list().unwrap() {
        loaded.values.insert(
            id.clone(),
            (
                source.load(&id).unwrap().unwrap(),
                source.history_after(&id, 0).unwrap(),
            ),
        );
    }
    let records: usize = loaded
        .values
        .values()
        .map(|(_, records)| records.len())
        .sum();
    assert!(records >= GRAPHS * RECORDS + LONG_RECORDS);
    println!(
        "{}",
        json!({"phase":"preload_kernel_records","elapsed_ms":started.elapsed().as_millis(),"records":records,"io":source.read_metrics(),"memory":process_memory(),"scope":"preload only; not projection or query memory"})
    );
    drop(source);
    let mut table = MemoryTable::default();
    let mut projector = Projector::default();
    let before = reset_peak();
    let started = Instant::now();
    assert_eq!(projector.pull(&loaded, &mut table).unwrap(), records);
    println!(
        "{}",
        json!({"phase":"projection_only_cached_input","elapsed_ms":started.elapsed().as_millis(),"resources":loaded.values.len(),"records":records,"before":before,"after":process_memory(),"scope":"Projector pull from preloaded owner records; includes required record/resource clones, no filesystem read; RSS includes cached input"})
    );
    drop(loaded);
    drop(projector);
    let safe_views = views(GRAPHS);
    let before = reset_peak();
    let started = Instant::now();
    for view in &safe_views {
        anchor_hub::table::project_safe_view(view, &mut table);
    }
    println!(
        "{}",
        json!({"phase":"safe_view_projection_only","elapsed_ms":started.elapsed().as_millis(),"before":before,"after":process_memory(),"scope":"DTO projection only; not Host export; resident baseline includes retained fact table"})
    );
    let manifest = anchor_facts::window::WindowManifest {
        schema_version: SCHEMA_VERSION,
        analyzer_version: anchor_facts::window::ANALYZER_VERSION,
        now_ms: FIRST_CLOSE,
        limit: 8,
        sources: vec![],
        safe_views: vec![],
        support_history: true,
        baseline_at_ms: None,
        parent_manifest_digest: None,
        exclude_runs: vec![],
    };
    let before = reset_peak();
    let started = Instant::now();
    let query = Query::new(table.facts(), Some(FIRST_CLOSE));
    let mut signals = anchor_hub::analyzers::signals::Signals { limit: 8 }.window_table(&query);
    signals.longitudinal = Some(anchor_hub::analyzers::longitudinal::summarize(
        &query, &manifest,
    ));
    assert_eq!(signals.applicability.grade_conflicts, 1);
    assert_eq!(signals.applicability.graded_runs, 999);
    println!(
        "{}",
        json!({"phase":"query_only_signals_and_longitudinal","elapsed_ms":started.elapsed().as_millis(),"projected_rows":query.rows().len(),"before":before,"after":process_memory(),"scope":"Query+Signals+longitudinal over retained table; no owner filesystem IO; RSS includes resident table and allocator retained pages"})
    );
    let before = reset_peak();
    let started = Instant::now();
    let output = serde_json::to_vec(&signals).unwrap();
    assert!(output.len() < 256 * 1024);
    println!(
        "{}",
        json!({"phase":"summary_serialize_only","elapsed_ms":started.elapsed().as_millis(),"bytes":output.len(),"before":before,"after":process_memory()})
    );
}

#[test]
#[ignore = "manual per-phase measurement of the explicitly selected private scale fixture"]
fn facts_only_hub_cold_and_warm_refresh_resources_are_measured() {
    let root = fixture_root();
    let mut hub = anchor_hub::adapters::Hub::new(vec![root]);
    let before = reset_peak();
    let started = Instant::now();
    let initial = hub.refresh().unwrap();
    assert!(initial >= GRAPHS * RECORDS + LONG_RECORDS);
    println!(
        "{}",
        json!({"phase":"hub_cold_refresh_facts_only","elapsed_ms":started.elapsed().as_millis(),"new_rows":initial,"projected_rows":hub.query(None).rows().len(),"before":before,"after":process_memory(),"scope":"production Hub.refresh facts-only; includes disk read/project/staging; no safe-view injection API"})
    );
    let before = reset_peak();
    let started = Instant::now();
    assert_eq!(hub.refresh().unwrap(), 0);
    println!(
        "{}",
        json!({"phase":"hub_idle_warm_refresh_facts_only","elapsed_ms":started.elapsed().as_millis(),"new_rows":0,"projected_rows":hub.query(None).rows().len(),"before":before,"after":process_memory(),"scope":"production Hub.refresh facts-only; includes transactional staged table/cache clone; resident baseline retained; physical IO not exposed by Hub"})
    );
}
