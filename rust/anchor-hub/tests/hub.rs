mod support;

use std::collections::BTreeSet;

use anchor_facts::{AgentConfiguration, Outcome};
use anchor_hub::adapters::{Hub, discover};
use anchor_hub::analyzers::{
    Analyzer, Finding, Need, Report, contrast::Contrast, gate::Gate, health::Health,
    signals::Signals, stall::Stall,
};
use anchor_hub::mcp::handle;
use anchor_hub::query::{
    Activity, GapPlace, Query, Status, Wealth, covered, e_bh, mann_whitney, node_result,
    normal_cdf, repeats, summarize, two_proportions,
};
use anchor_hub::source::{DataRoot, HistorySource, SourceError, Stored};
use anchor_hub::table::{Body, FactTable, MemoryTable, Projector};
use anchor_kernel::{MemoryStore, Record, Resource, Store};
use serde_json::{Value, json};
use support::{SECRET, World};

fn project(world: &World) -> (MemoryTable, Projector) {
    let mut table = MemoryTable::default();
    let mut projector = Projector::default();
    for name in ["kernel-runs", "kernel-facts"] {
        let root = DataRoot::open(&world.root(name), name).unwrap();
        projector.pull(&root, &mut table).unwrap();
    }
    (table, projector)
}

fn codes(report: &Report) -> BTreeSet<(&'static str, String)> {
    report
        .findings
        .iter()
        .map(|f| (f.code, f.subject.clone()))
        .collect()
}

fn finding<'r>(report: &'r Report, code: &str) -> &'r Finding {
    report
        .findings
        .iter()
        .find(|f| f.code == code)
        .unwrap_or_else(|| panic!("no {code} in {:#?}", report.findings))
}

fn visible(hub: &Hub) -> Value {
    let query = hub.query(None);
    let configurations: Vec<_> = query.configurations().collect();
    let manifests: Vec<_> = configurations
        .iter()
        .map(|(_, config)| {
            (
                &config.envelope_sha256,
                query.manifest(&config.envelope_sha256),
            )
        })
        .collect();
    let details: Vec<_> = query
        .invocations()
        .iter()
        .map(|invocation| (invocation.key(), invocation.details))
        .collect();
    json!({
        "rows": query.rows(),
        "resources": query.resources().collect::<Vec<_>>(),
        "configurations": configurations,
        "manifests": manifests,
        "details": details,
    })
}

#[test]
fn rows_are_keyed_by_root_resource_and_place_and_carry_no_business_content() {
    let world = World::new();
    support::r1(&world);
    let (table, _) = project(&world);
    let facts = table.facts();
    let run: Vec<_> = facts
        .rows
        .iter()
        .filter(|r| r.key.root == "kernel-runs" && r.key.resource == "run/r1")
        .collect();
    assert_eq!(
        run.iter()
            .map(|r| (r.key.place, r.slot.as_deref().unwrap(), r.at))
            .collect::<Vec<_>>(),
        vec![
            (1, "prepare#1", Some(1_200)),
            (2, "worker#1", Some(4_800)),
            (3, "critic#1", Some(6_000)),
            (4, "verify#1", Some(6_300)),
            (5, "late#1", Some(6_400)),
        ]
    );
    assert_eq!(run[3].status, Some(Status::Done));
    assert!(!run[4].verdict.is_accepted());
    assert!(run.iter().all(|r| r.body == Body::Kernel));

    let traces: Vec<_> = facts
        .rows
        .iter()
        .filter(|r| r.key.root == "kernel-facts")
        .collect();
    assert_eq!(traces.len(), 2);
    assert!(
        traces
            .iter()
            .all(|r| matches!(r.body, Body::Observation(_)))
    );
    let worker = support::configuration("worker").digest();
    assert_eq!(
        facts.configurations.keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([&worker, &support::configuration("ghost").digest()])
    );

    let projected = serde_json::to_string(facts.rows).unwrap();
    assert!(!projected.contains(SECRET));
}

#[test]
fn a_later_pull_projects_only_what_was_appended() {
    let world = World::new();
    support::r1(&world);
    let (mut table, mut projector) = project(&world);
    let before = table.facts().rows.len();
    support::r2(&world);
    let mut added = 0;
    for name in ["kernel-runs", "kernel-facts"] {
        let root = DataRoot::open(&world.root(name), name).unwrap();
        added += projector.pull(&root, &mut table).unwrap();
    }
    assert_eq!(added, 2);
    assert_eq!(table.facts().rows.len(), before + 2);
    let keys: BTreeSet<_> = table.facts().rows.iter().map(|r| r.key.clone()).collect();
    assert_eq!(keys.len(), table.facts().rows.len(), "no row twice");
}

#[test]
fn a_manifest_that_does_not_hash_to_its_name_is_not_that_configuration() {
    let world = World::new();
    world.run("r", 1);
    world.observe_as(
        "r",
        "worker",
        10,
        "not-its-digest",
        Some(&support::configuration("worker")),
        vec![support::lifecycle(
            (2, 9),
            anchor_facts::Outcome::Completed,
            0,
            0,
        )],
    );
    world.result("r", "worker#1", 11, 1, true);
    let (table, _) = project(&world);
    assert!(table.facts().configurations.is_empty());
    let report = Health.analyze(&Query::new(table.facts(), None));
    assert_eq!(
        finding(&report, "configuration_unknown").subject,
        "config not-its-digest"
    );
}

#[test]
fn invocations_join_run_results_and_observations_and_keep_dangling_sides() {
    let world = World::new();
    support::r1(&world);
    let (table, _) = project(&world);
    let query = Query::new(table.facts(), None);
    let invocations = query.invocations();
    let sides: Vec<_> = invocations
        .iter()
        .map(|i| (i.key(), i.result.is_some(), i.traces.len()))
        .collect();
    assert_eq!(
        sides,
        vec![
            ("r1/critic#1".into(), true, 0),
            ("r1/ghost#1".into(), false, 1),
            ("r1/prepare#1".into(), true, 0),
            ("r1/verify#1".into(), true, 0),
            ("r1/worker#1".into(), true, 1),
        ]
    );
    let worker = &invocations[4];
    assert_eq!(worker.evidence().len(), 2);
    assert_eq!(
        worker.config(),
        Some(support::configuration("worker").digest().as_str())
    );
    assert_eq!(
        node_result(worker.result.unwrap()),
        Some(("r1", "worker", 1))
    );
}

#[test]
fn a_turn_ends_when_its_first_call_starts_even_if_its_usage_is_observed_later() {
    let world = World::new();
    support::r1(&world);
    let (table, _) = project(&world);
    let query = Query::new(table.facts(), None);
    let worker = query
        .invocations()
        .into_iter()
        .find(|i| i.key() == "r1/worker#1")
        .unwrap();
    assert_eq!(
        worker.turn_spans(),
        vec![(1, 1_400..2_400), (2, 2_480..3_480), (3, 3_630..4_630)]
    );
    let gaps: Vec<_> = worker.gaps().iter().map(|g| (g.place(), g.ms())).collect();
    assert_eq!(
        gaps,
        vec![
            (GapPlace::Startup, 100),
            (GapPlace::ToolToTurn, 30),
            (GapPlace::ToolToTurn, 30),
            (GapPlace::Teardown, 69),
        ]
    );
    // Model, tools and gaps partition the invocation.
    let model = covered(worker.turn_spans().into_iter().map(|(_, s)| s));
    let tools = covered(worker.calls().iter().filter_map(|c| c.span()));
    let idle: u64 = worker.gaps().iter().map(|g| g.ms()).sum();
    assert_eq!(model + tools + idle, 4_700 - 1_300);
    assert_eq!(
        worker.timeline().first().map(|s| s.activity),
        Some(Activity::Turn { index: 1 })
    );
}

#[test]
fn task_steps_measure_time_since_the_previous_record() {
    let world = World::new();
    support::r1(&world);
    let (table, _) = project(&world);
    let query = Query::new(table.facts(), None);
    let tasks = query.tasks();
    assert_eq!(tasks.len(), 1);
    let steps: Vec<_> = tasks[0]
        .steps()
        .iter()
        .map(|(row, since)| (row.slot.clone().unwrap(), *since))
        .collect();
    assert_eq!(
        steps,
        vec![
            ("prepare#1".into(), Some(200)),
            ("worker#1".into(), Some(3_600)),
            ("critic#1".into(), Some(1_200)),
            ("verify#1".into(), Some(300)),
            ("late#1".into(), Some(100)),
        ]
    );
    assert_eq!(tasks[0].span_ms(), Some(5_400));
}

#[test]
fn health_reports_each_irregularity_with_its_records() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    let (table, _) = project(&world);
    let report = Health.analyze(&Query::new(table.facts(), Some(0)));
    assert_eq!(report.not_applicable, vec![Need::Grades]);
    assert_eq!(
        codes(&report),
        BTreeSet::from([
            ("not_accepted", "run/*".into()),
            ("unobserved_invocation", "r1/critic#1".into()),
            ("observation_without_result", "r1/ghost#1".into()),
            ("invocation_not_completed", "r1/ghost#1".into()),
            ("usage_incomplete", "r1/ghost#1".into()),
            ("tool_failures", "tool anchor_run".into()),
            ("tool_failures", "tool anchor_read".into()),
            ("same_tool_after_failure", "tool anchor_run".into()),
        ])
    );
    let refused = finding(&report, "not_accepted");
    assert!(refused.detail.contains("task_settled"));
    assert_eq!(refused.evidence[0].place, 5);
    assert_eq!(finding(&report, "unobserved_invocation").evidence.len(), 1);
    assert_eq!(
        finding(&report, "same_tool_after_failure").evidence.len(),
        2
    );

    let time = &report.summary["time"];
    assert!(time["model_share"].is_null());
    assert!(time["unobserved_share"].is_null());
    assert_eq!(time["timeline_complete"], false);
    let shares = [
        "observed_model_share",
        "tool_share",
        "uncovered_observation_share",
    ]
    .map(|k| time[k].as_f64().unwrap())
    .iter()
    .sum::<f64>();
    assert!((shares - 1.0).abs() < 0.01, "{time}");
    let steps = &report.summary["steps"];
    assert_eq!(steps["observed_overhead_ms"]["min"], json!(150));
    assert_eq!(steps["observed_overhead_ms"]["max"], json!(200));
    assert_eq!(
        steps["unobserved_step_ms"]["critic"]["median"],
        json!(1_200)
    );
    assert_eq!(
        report.summary["tools"]["anchor_read"]["failures"]["not_authorized"],
        json!(3)
    );
}

#[test]
fn health_flags_a_configuration_that_waits_long_before_the_model() {
    let world = World::new();
    world.run("r", 0);
    world.observe(
        "r",
        "answer",
        2_100,
        &support::configuration("answer"),
        vec![
            support::turn(1, 2_000, 1_000),
            support::call("final_result", 1, json!({}), (2_000, 2_001), None),
            support::lifecycle((500, 2_050), anchor_facts::Outcome::Completed, 1, 1),
        ],
    );
    world.result("r", "answer#1", 2_200, 1, true);
    let (table, _) = project(&world);
    let report = Health.analyze(&Query::new(table.facts(), None));
    let startup = finding(&report, "startup_heavy");
    assert!(startup.detail.contains("32%"), "{}", startup.detail);
    assert!(
        startup.detail.contains("median 500 ms"),
        "{}",
        startup.detail
    );
}

#[test]
fn stall_finds_idle_runs_silences_slow_calls_and_loops() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    let (table, _) = project(&world);
    // The accepted observation at 200_000 is newer than the Run result.
    let now = 200_000 + 16 * 60_000;
    let report = Stall::default().analyze(&Query::new(table.facts(), Some(now)));
    assert_eq!(
        codes(&report),
        BTreeSet::from([
            ("idle_task", "run/r2".into()),
            ("idle_gap", "r2/worker#1".into()),
            ("slow_tool_call", "tool anchor_run".into()),
            ("consecutive_same_shape_failures", "r2/worker#1".into()),
        ])
    );
    let gap = finding(&report, "idle_gap");
    assert!(
        gap.detail
            .contains("60 s with no model turn or tool call observed, after tool call anchor_run and before model turn 5"),
        "{}",
        gap.detail
    );
    assert!(
        finding(&report, "consecutive_same_shape_failures")
            .detail
            .contains("3 consecutive failed anchor_read")
    );

    // Not yet idle long enough: no task finding.
    let early = Stall::default().analyze(&Query::new(table.facts(), Some(200_000 + 60_000)));
    assert!(!early.findings.iter().any(|f| f.code == "idle_task"));
}

fn observe_input_wait(world: &World, run: &str, node: &str, at: u64, waiting: bool) {
    world.clock.set(at);
    let invocation = anchor_facts::Invocation {
        run_id: run.into(),
        graph_digest: "g".into(),
        node_id: node.into(),
        invocation: 1,
    };
    let trace = invocation.trace();
    if world.facts.load(&trace).unwrap().is_none() {
        world
            .facts
            .create(Resource::new(
                &trace,
                anchor_facts::OBSERVATION,
                json!(null),
            ))
            .unwrap();
    }
    let seen = world.facts.load(&trace).unwrap().unwrap().version;
    let verdict = world
        .facts
        .submit(
            &anchor_kernel::Principal::new(
                anchor_facts::RECORDER_ID,
                anchor_kernel::PrincipalKind::Code,
            ),
            0,
            anchor_kernel::Submission::new(&trace, seen, anchor_kernel::Action::Write).content(
                json!(anchor_facts::Observation {
                    version: anchor_facts::VERSION,
                    invocation,
                    facts: vec![anchor_facts::Fact::InputWait { waiting }],
                }),
            ),
        )
        .unwrap();
    assert!(verdict.is_accepted());
}

#[test]
fn input_wait_facts_not_node_names_distinguish_waiting_from_unknown_idle_activity() {
    let world = World::new();
    world.run("receiver", 1);
    world.run("wait_input", 1);
    observe_input_wait(&world, "receiver", "arbitrary-node", 10, true);
    let (table, _) = project(&world);
    let now = 2 * 60 * 60_000;
    let report = Stall::default().analyze(&Query::new(table.facts(), Some(now)));
    assert_eq!(
        codes(&report),
        BTreeSet::from([("idle_task", "run/wait_input".into())])
    );
    assert_eq!(report.summary["waiting_input_tasks"], json!(1));
    assert!(
        finding(&report, "idle_task")
            .detail
            .contains("activity unknown")
    );
    let health = Health.analyze(&Query::new(table.facts(), Some(now)));
    assert!(!health.findings.iter().any(|finding| {
        matches!(
            finding.code,
            "observation_without_result" | "configuration_unknown"
        )
    }));

    // A freshly claimed input is activity, even before its node result exists.
    observe_input_wait(&world, "receiver", "arbitrary-node", now, false);
    let (table, _) = project(&world);
    let report = Stall::default().analyze(&Query::new(table.facts(), Some(now + 1)));
    assert_eq!(report.summary["waiting_input_tasks"], json!(0));
    assert!(
        !report
            .findings
            .iter()
            .any(|finding| finding.subject == "run/receiver")
    );
    let late = Stall::default().analyze(&Query::new(table.facts(), Some(now + 16 * 60_000)));
    assert!(codes(&late).contains(&("idle_task", "run/receiver".into())));
    let health = Health.analyze(&Query::new(table.facts(), None));
    assert_eq!(health.summary["invocations"]["resumed"], json!(0));

    // A committed result invalidates a stale waiting observation, even if its
    // closing observation was lost. No Graph scheduling is reimplemented here.
    observe_input_wait(&world, "receiver", "arbitrary-node", now + 2, true);
    world.result("receiver", "arbitrary-node#1", now + 3, 0, false);
    let (table, _) = project(&world);
    let report = Stall::default().analyze(&Query::new(table.facts(), Some(now + 16 * 60_000)));
    assert_eq!(report.summary["waiting_input_tasks"], json!(0));
    assert!(codes(&report).contains(&("idle_task", "run/receiver".into())));
}

#[test]
fn records_without_time_or_observations_are_reported_not_applicable() {
    // A history from before records carried time.
    let store = MemoryStore::default();
    let mut resource = Resource::new("run/old", "validator/graph", json!(null)).task(10);
    store.create(&resource).unwrap();
    resource.version = 1;
    let record: Record = serde_json::from_value(json!({
        "stamp": {"principal": {"id": "anchor/executor", "kind": "agent"}, "cost": 1},
        "submission": {"target": "run/old", "seen": 0, "action": "write", "slot": "a#1"},
        "verdict": {"effect": "accepted", "verification": "evidenced", "reason": "ok"},
        "after": resource,
    }))
    .unwrap();
    store.commit("run/old", &record).unwrap();

    let mut table = MemoryTable::default();
    Projector::default()
        .pull(&Stored::new(&store, "memory"), &mut table)
        .unwrap();
    let query = Query::new(table.facts(), Some(u64::MAX));
    let health = Health.analyze(&query);
    assert_eq!(
        health.not_applicable,
        vec![Need::Time, Need::Observations, Need::Grades]
    );
    assert_eq!(health.summary["records"]["rows"], json!(1));
    assert!(!health.summary.contains_key("tools"));
    let stall = Stall::default().analyze(&query);
    assert!(stall.findings.is_empty());
}

#[test]
fn the_cli_hub_discovers_roots_and_follows_appends() {
    let world = World::new();
    support::r1(&world);
    // A directory that is no data root is not read.
    std::fs::create_dir_all(world.path().join("host/state/goose-acp")).unwrap();
    assert_eq!(
        discover(world.path()),
        vec![
            world.root("kernel-facts"),
            world.root("kernel-grades"),
            world.root("kernel-runs")
        ]
    );
    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    let first = hub.refresh().unwrap();
    assert_eq!((hub.roots(), first), (3, 7));
    assert_eq!(hub.refresh().unwrap(), 0);
    support::r2(&world);
    assert_eq!(hub.refresh().unwrap(), 2);
}

#[test]
fn warm_refresh_matches_cold_after_resource_root_and_evidence_retirement() {
    let world = World::new();
    let config = support::configuration("worker");
    world.run("deleted", 1_000);
    world.observe(
        "deleted",
        "worker",
        2_000,
        &config,
        vec![
            support::call(
                "anchor_run",
                1,
                json!({"command": ["printf", "ok"]}),
                (1_100, 1_110),
                None,
            ),
            support::lifecycle((1_000, 2_000), Outcome::Completed, 0, 1),
        ],
    );
    world.result("deleted", "worker#1", 2_100, 1, true);
    world.grade("deleted", "task", &[("quality", true)], 2_200);

    // Optional evidence gives the invocation one value-level call detail.
    let evidence = world.state().join("goose-acp");
    std::fs::create_dir_all(&evidence).unwrap();
    let evidence_file = evidence.join("deleted.evidence.json");
    std::fs::write(
        &evidence_file,
        json!({
            "key": {"run_id": "deleted", "node_id": "worker", "invocation": 1},
            "tool_calls": [{"tool": "anchor_run", "tool_call_id": "c1",
                "arguments": {"command": ["printf", "ok"]},
                "result": {"ok": true, "output": {"output": [{"type": "json", "value":
                    {"status": "completed", "exit_code": 0, "stdout": "ok", "stderr": ""}}]}}}]
        })
        .to_string(),
    )
    .unwrap();

    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    hub.refresh().unwrap();
    assert_eq!(hub.query(None).invocations()[0].details.len(), 1);

    let trace = anchor_facts::Invocation {
        run_id: "deleted".into(),
        graph_digest: "g".into(),
        node_id: "worker".into(),
        invocation: 1,
    }
    .trace();
    world.runs.store().remove("run/deleted").unwrap();
    world.facts.store().remove(&trace).unwrap();
    world
        .grades
        .store()
        .remove(&anchor_facts::grade_id("deleted"))
        .unwrap();
    std::fs::remove_file(&evidence_file).unwrap();
    hub.refresh().unwrap();
    let mut cold = Hub::new(vec![world.path().to_path_buf()]);
    cold.refresh().unwrap();
    assert_eq!(hub.roots(), cold.roots());
    assert_eq!(visible(&hub), visible(&cold));
    assert_eq!(hub.query(None).tasks().len(), 0);
    assert_eq!(hub.query(None).invocations().len(), 0);
    assert_eq!(hub.query(None).grades().len(), 0);
    assert!(
        hub.query(None)
            .invocations()
            .iter()
            .all(|i| i.details.is_empty())
    );

    // Removing a nonempty whole source root retires its rows and registration.
    world.grade("root-retired", "task", &[("quality", true)], 3_000);
    hub.refresh().unwrap();
    assert_eq!(hub.query(None).grades().len(), 1);
    std::fs::remove_dir_all(world.root("kernel-grades")).unwrap();
    hub.refresh().unwrap();
    assert_eq!(hub.roots(), 2);
    let mut cold = Hub::new(vec![world.path().to_path_buf()]);
    cold.refresh().unwrap();
    assert_eq!(visible(&hub), visible(&cold));
    assert_eq!(hub.query(None).grades().len(), 0);
    assert!(
        hub.query(None)
            .rows()
            .iter()
            .all(|row| row.key.root != world.root("kernel-grades").display().to_string())
    );
}

#[test]
fn shared_configuration_survives_when_one_root_retires_its_owner() {
    let left = World::new();
    let right = World::new();
    let config = support::configuration("worker");
    left.run("left", 1_000);
    right.run("right", 1_000);
    left.observe(
        "left",
        "worker",
        1_100,
        &config,
        vec![support::lifecycle((1_000, 1_100), Outcome::Completed, 0, 0)],
    );
    right.observe(
        "right",
        "worker",
        1_100,
        &config,
        vec![support::lifecycle((1_000, 1_100), Outcome::Completed, 0, 0)],
    );
    let mut hub = Hub::new(vec![left.path().to_path_buf(), right.path().to_path_buf()]);
    hub.refresh().unwrap();
    let digest = config.digest();
    assert!(hub.query(None).configuration(&digest).is_some());
    assert!(hub.query(None).manifest(&config.envelope_sha256).is_some());

    let left_trace = anchor_facts::Invocation {
        run_id: "left".into(),
        graph_digest: "g".into(),
        node_id: "worker".into(),
        invocation: 1,
    }
    .trace();
    left.facts.store().remove(&left_trace).unwrap();
    left.facts
        .store()
        .remove(&anchor_facts::configuration_id(&digest))
        .unwrap();
    left.facts
        .store()
        .remove(&anchor_facts::configuration_id(&config.envelope_sha256))
        .unwrap();
    hub.refresh().unwrap();
    assert!(hub.query(None).configuration(&digest).is_some());
    assert!(hub.query(None).manifest(&config.envelope_sha256).is_some());
    assert!(
        hub.query(None)
            .rows()
            .iter()
            .any(|row| { row.key.root == right.root("kernel-facts").display().to_string() })
    );
    let mut cold = Hub::new(vec![left.path().to_path_buf(), right.path().to_path_buf()]);
    cold.refresh().unwrap();
    assert_eq!(visible(&hub), visible(&cold));

    // The final active provider going away retires both maps, without leaving
    // a content-addressed entry kept alive just by an old projection.
    std::fs::remove_dir_all(right.root("kernel-facts")).unwrap();
    hub.refresh().unwrap();
    assert!(hub.query(None).configuration(&digest).is_none());
    assert!(hub.query(None).manifest(&config.envelope_sha256).is_none());
}

#[test]
fn replacement_and_corruption_never_leave_a_warm_half_view() {
    let world = World::new();
    world.run("replace", 1_000);
    world.result("replace", "old#1", 1_100, 1, false);
    world.result("replace", "old#2", 1_150, 1, false);
    world.result("replace", "old#3", 1_200, 1, true);
    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    hub.refresh().unwrap();
    let before = hub.query(None).rows().len();

    // Replace the physical resource with the same logical id and a shorter,
    // different history. DataRoot must discard its old cursor and replay.
    world.runs.store().remove("run/replace").unwrap();
    world.clock.set(2_000);
    world
        .runs
        .create(Resource::new("run/replace", "validator/step", Value::Null).task(u64::MAX))
        .unwrap();
    world.result("replace", "new#1", 2_100, 2, true);
    hub.refresh().unwrap();
    let cold = {
        let mut cold = Hub::new(vec![world.path().to_path_buf()]);
        cold.refresh().unwrap();
        cold
    };
    assert!(before > 0);
    assert_eq!(visible(&hub), visible(&cold));
    assert!(
        hub.query(None)
            .rows()
            .iter()
            .any(|row| row.slot.as_deref() == Some("new#1"))
    );
    assert!(hub.query(None).rows().iter().all(|row| {
        !row.slot
            .as_deref()
            .is_some_and(|slot| slot.starts_with("old#"))
    }));

    // A malformed complete history line is an error, not a clean empty root;
    // the last healthy projection remains visible to callers.
    let history = world
        .root("kernel-runs")
        .join("run%2Freplace")
        .join("history.jsonl");
    let healthy_history = std::fs::read(&history).unwrap();
    // Truncate to zero records with the same creation identity and current
    // resource directory. It must not keep the formerly projected final row.
    let state_path = history.with_file_name("resource.json");
    let healthy_state = std::fs::read(&state_path).unwrap();
    let mut initial = world.runs.load("run/replace").unwrap().unwrap();
    initial.version = 0;
    std::fs::write(&state_path, serde_json::to_vec(&initial).unwrap()).unwrap();
    std::fs::write(&history, []).unwrap();
    hub.refresh().unwrap();
    let mut truncated_cold = Hub::new(vec![world.path().to_owned()]);
    truncated_cold.refresh().unwrap();
    assert_eq!(visible(&hub), visible(&truncated_cold));
    assert!(hub.query(None).rows().is_empty());
    std::fs::write(&state_path, healthy_state).unwrap();
    std::fs::write(&history, &healthy_history).unwrap();
    hub.refresh().unwrap();
    assert_eq!(visible(&hub), visible(&cold));
    // Atomic history-file replacement also invalidates the old physical
    // cursor even though resource creation and all fact values are unchanged.
    let replacement = history.with_extension("replacement");
    std::fs::write(&replacement, &healthy_history).unwrap();
    std::fs::rename(&replacement, &history).unwrap();
    hub.refresh().unwrap();
    assert_eq!(visible(&hub), visible(&cold));

    // The facts root sorts before runs. Replace an already projected trace
    // before the later run fails: neither reset markers nor new rows may
    // leak out of the staged refresh.
    let config = support::configuration("worker");
    world.observe(
        "replace",
        "worker",
        2_200,
        &config,
        vec![support::lifecycle((2_000, 2_100), Outcome::Completed, 0, 0)],
    );
    hub.refresh().unwrap();
    world
        .facts
        .store()
        .remove("trace/replace/worker/1")
        .unwrap();
    world.observe(
        "replace",
        "worker",
        2_300,
        &config,
        vec![support::lifecycle((2_200, 2_300), Outcome::Failed, 0, 0)],
    );
    std::fs::write(&history, b"not-json\n").unwrap();
    let visible_before_error = visible(&hub);
    let error = hub
        .refresh()
        .expect_err("corrupt history must fail refresh");
    assert!(!error.is_empty(), "{error}");
    assert_eq!(visible(&hub), visible_before_error);
    let response = handle(
        &mut hub,
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "invocations", "arguments": {}}
        }),
    )
    .unwrap();
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(visible(&hub), visible_before_error);

    std::fs::write(&history, healthy_history).unwrap();
    hub.refresh().unwrap();
    let mut cold = Hub::new(vec![world.path().to_path_buf()]);
    cold.refresh().unwrap();
    assert_eq!(visible(&hub), visible(&cold));
}

#[test]
fn stored_sources_reset_on_history_creation_or_version_regression() {
    fn store(
        created: Option<anchor_kernel::Creation>,
        version: u64,
        count: usize,
        label: &str,
    ) -> MemoryStore {
        let store = MemoryStore::default();
        for id in ["run/a", "run/z"] {
            let mut resource = Resource::new(id, "validator/step", Value::Null).task(10);
            resource.created = created.clone();
            store.create(&resource).unwrap();
            resource.version = version;
            for index in 0..count {
                let record = serde_json::from_value(json!({
                    "stamp": {"principal": {"id": "executor", "kind": "agent"}, "cost": 0},
                    "submission": {"target": id, "seen": 0, "action": "write", "slot": format!("{label}-{index}#1")},
                    "verdict": {"effect": "accepted", "verification": "evidenced", "reason": "ok"},
                    "after": resource,
                })).unwrap();
                store.commit(id, &record).unwrap();
            }
        }
        store
    }
    let creation = Some(anchor_kernel::Creation {
        at: None,
        config: None,
    });
    for (created, version, count) in [
        (None, 4, 4), // Includes Some(empty) -> None, not only projected fields.
        (creation.clone(), 4, 1),
        (creation.clone(), 1, 4),
    ] {
        let old = store(creation.clone(), 4, 4, "old");
        let replacement = store(created, version, count, "new");
        let mut table = MemoryTable::default();
        let mut projector = Projector::default();
        projector
            .pull(&Stored::new(&old, "memory"), &mut table)
            .unwrap();
        projector
            .pull(&Stored::new(&replacement, "memory"), &mut table)
            .unwrap();
        let mut cold = MemoryTable::default();
        Projector::default()
            .pull(&Stored::new(&replacement, "memory"), &mut cold)
            .unwrap();
        assert_eq!(table.facts().rows, cold.facts().rows);
        assert_eq!(table.facts().resources, cold.facts().resources);
        assert!(
            table
                .facts()
                .rows
                .windows(2)
                .all(|rows| rows[0].key < rows[1].key)
        );
    }
}

#[test]
fn listed_resource_loading_absent_retires_entry_rows_and_read_position() {
    struct Absent;
    impl HistorySource for Absent {
        fn root(&self) -> &str {
            "memory"
        }
        fn list(&self) -> Result<Vec<String>, SourceError> {
            Ok(vec!["run/a".into()])
        }
        fn load(&self, _: &str) -> Result<Option<Resource>, SourceError> {
            Ok(None)
        }
        fn history_after(&self, _: &str, _: usize) -> Result<Vec<Record>, SourceError> {
            panic!("absent load must not ask for history");
        }
    }
    let world = World::new();
    world.run("a", 1);
    world.result("a", "worker#1", 2, 1, true);
    let root = world.root("kernel-runs");
    let source = DataRoot::open(&root, "memory").unwrap();
    let mut table = MemoryTable::default();
    let mut projector = Projector::default();
    projector.pull(&source, &mut table).unwrap();
    projector.pull(&Absent, &mut table).unwrap();
    assert!(table.facts().rows.is_empty());
    assert!(table.facts().resources.is_empty());
    assert_eq!(projector.pull(&source, &mut table).unwrap(), 1);
    assert_eq!(table.facts().rows[0].key.place, 1);
}

#[test]
fn optional_evidence_is_current_per_path_and_shared_invocation() {
    let world = World::new();
    let config = support::configuration("worker");
    for run in ["a", "b"] {
        world.observe(
            run,
            "worker",
            10,
            &config,
            vec![support::call(
                "anchor_run",
                1,
                json!({"command": ["printf", "ok"]}),
                (1, 2),
                None,
            )],
        );
    }
    let directory = world.state().join("goose-acp");
    std::fs::create_dir_all(&directory).unwrap();
    let first = directory.join("first.evidence.json");
    let second = directory.join("second.evidence.json");
    let evidence = |run: &str, answer: &str| {
        json!({
            "key": {"run_id": run, "node_id": "worker", "invocation": 1},
            "tool_calls": [{"tool": "anchor_run", "arguments": {"command": ["printf", "ok"]},
                "result": {"ok": true, "output": answer}}],
        })
        .to_string()
    };
    std::fs::write(&first, evidence("a", "first")).unwrap();
    std::fs::write(&second, evidence("a", "second")).unwrap();
    let mut hub = Hub::new(vec![world.path().to_owned()]);
    hub.refresh().unwrap();
    assert_eq!(hub.query(None).invocations()[0].details.len(), 1);

    std::fs::remove_file(&second).unwrap();
    hub.refresh().unwrap();
    assert!(
        hub.query(None).invocations()[0].details[0]
            .answer
            .contains("first")
    );
    std::fs::write(&first, evidence("b", "changed invocation")).unwrap();
    hub.refresh().unwrap();
    let invocations = hub.query(None).invocations();
    assert!(invocations[0].details.is_empty());
    assert_eq!(invocations[1].details.len(), 1);

    std::fs::write(&first, "invalid JSON").unwrap();
    hub.refresh().unwrap();
    assert!(
        hub.query(None)
            .invocations()
            .iter()
            .all(|i| i.details.is_empty())
    );
    let calls =
        anchor_hub::mcp::tool(&hub.query(None), "calls", &json!({"key": "b/worker#1"})).unwrap();
    assert!(calls["items"][0]["answer"].is_null());
    std::fs::write(&first, evidence("b", "restored")).unwrap();
    hub.refresh().unwrap();
    std::fs::remove_file(&first).unwrap();
    hub.refresh().unwrap();
    let mut cold = Hub::new(vec![world.path().to_owned()]);
    cold.refresh().unwrap();
    assert_eq!(visible(&hub), visible(&cold));
}

#[test]
fn unreadable_configured_roots_fail_without_retiring_healthy_facts() {
    let world = World::new();
    world.run("retained", 1);
    world.result("retained", "worker#1", 2, 1, true);
    let root = world.root("kernel-runs");
    let mut hub = Hub::new(vec![root.clone()]);
    hub.refresh().unwrap();
    let before = visible(&hub);
    let moved = world.state().join("unlisted-runs");
    std::fs::rename(&root, &moved).unwrap();
    std::fs::write(&root, b"not a readable directory").unwrap();
    assert!(hub.refresh().is_err());
    assert_eq!(visible(&hub), before);
    assert_eq!(hub.roots(), 1);
    std::fs::remove_file(&root).unwrap();
    std::fs::rename(&moved, &root).unwrap();
    hub.refresh().unwrap();
    assert_eq!(visible(&hub), before);
}

#[test]
fn operators() {
    assert_eq!(
        repeats(&[1, 1, 1, 2, 2, 3, 3, 3, 3], 3, |x| *x),
        vec![0..3, 5..9]
    );
    assert_eq!(repeats::<u8, u8>(&[], 1, |x| *x), vec![]);
    let s = summarize([5, 1, 3, 2, 4, 10, 6, 7, 9, 8]).unwrap();
    assert_eq!((s.n, s.min, s.median, s.p90, s.max), (10, 1, 5, 9, 10));
    assert!((s.sd - 3.0277).abs() < 1e-3, "{}", s.sd);
    assert_eq!(summarize([7]).unwrap().sd, 0.0);
    assert_eq!(summarize(Vec::<u64>::new()), None);
    assert_eq!(covered([0..10, 5..15, 20..25, 21..22]), 20);
}

#[test]
fn grades_join_their_runs_and_failing_checks_are_reported() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    world.run("r3", 300_000);
    world.result("r3", "worker#1", 300_100, 1, true);
    world.grade(
        "r1",
        "ledger",
        &[("unpaid", true), ("total", false)],
        400_000,
    );
    world.grade(
        "r2",
        "ledger",
        &[("unpaid", false), ("total", false)],
        400_001,
    );
    // Regraded: only the latest grade of a Run counts.
    world.grade(
        "r3",
        "ledger",
        &[("unpaid", false), ("total", false)],
        400_002,
    );
    world.grade(
        "r3",
        "ledger",
        &[("unpaid", true), ("total", true)],
        400_003,
    );
    let (mut table, mut projector) = project(&world);
    let grades = DataRoot::open(&world.root("kernel-grades"), "kernel-grades").unwrap();
    projector.pull(&grades, &mut table).unwrap();

    let query = Query::new(table.facts(), None);
    let grades = query.grades();
    assert_eq!(grades.len(), 3);
    assert!(
        grades
            .values()
            .find(|graded| graded.grade.run_id == "r3")
            .unwrap()
            .grade
            .passed()
    );
    assert_eq!(
        grades
            .values()
            .find(|graded| graded.grade.run_id == "r3")
            .unwrap()
            .row
            .key
            .place,
        2
    );

    let report = Health.analyze(&query);
    assert!(report.not_applicable.is_empty());
    let quality = &report.summary["quality"]["ledger v1"];
    assert_eq!(quality["runs"], json!(3));
    assert_eq!(quality["pass_rate"], json!(0.333));
    assert_eq!(quality["mean_score"], json!(0.5));
    assert_eq!(quality["checks"]["total"], json!(0.333));
    assert_eq!(quality["checks"]["unpaid"], json!(0.667));
    // All three grades count, but r1 has an unobserved paid invocation and
    // r3 has no model observations. Neither is a zero-cost sample.
    assert_eq!(quality["tokens"]["n"], json!(1));
    // r1 also observed its ghost invocation, under its own configuration.
    assert_eq!(quality["configurations"], json!(2));
    // Every check that fails in any Run is reported, at its rate, once r3's
    // regrade counts: `total` in 2 of 3, `unpaid` in 1 of 3.
    let failing: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.code == "check_failing")
        .collect();
    assert_eq!(failing.len(), 2, "{failing:#?}");
    assert!(failing.iter().all(|f| f.subject == "task ledger v1"));
    let total = failing
        .iter()
        .find(|f| f.detail.contains("check total failed in 2 of 3"))
        .expect("total is reported");
    assert_eq!(total.evidence.len(), 2);
    assert!(
        failing
            .iter()
            .any(|f| f.detail.contains("check unpaid failed in 1 of 3")),
        "{failing:#?}"
    );
}

#[test]
fn rank_and_proportion_tests_match_known_values() {
    assert!((normal_cdf(1.96) - 0.975).abs() < 1e-4);
    assert!((normal_cdf(-1.0) - 0.158_655).abs() < 1e-5);
    let separated = mann_whitney(&[1.0, 2.0, 3.0], &[4.0, 5.0, 6.0]).unwrap();
    assert_eq!(separated.u, 0.0);
    assert!((separated.p - 0.0495).abs() < 1e-3, "{separated:?}");
    let same = mann_whitney(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]).unwrap();
    assert!((same.p - 1.0).abs() < 1e-6);
    let tied = mann_whitney(&[5.0, 5.0], &[5.0, 5.0]).unwrap();
    assert_eq!(tied.p, 1.0);
    assert_eq!(mann_whitney(&[], &[1.0]), None);
    let p = two_proportions(10, 100, 30, 100).unwrap();
    assert!((p - 0.0004).abs() < 1e-4, "{p}");
    assert_eq!(two_proportions(0, 10, 0, 10), Some(1.0));
}

/// Two harnesses on one task: the variant halves agent time and tool
/// failures, and loses quality on one Run.
fn experiment(world: &World) {
    let mut variant = support::configuration("worker");
    variant.host_sha256 = "host-b".into();
    variant.envelope_sha256 = anchor_facts::text_digest("{\"network\":true}");
    for n in 0..6u64 {
        let (arm, run) = if n % 2 == 0 {
            (support::configuration("worker"), format!("a{n}"))
        } else {
            (variant.clone(), format!("b{n}"))
        };
        let start = 1_000_000 + n * 100_000;
        let length = if n % 2 == 0 {
            10_000 + n * 100
        } else {
            5_000 + n * 100
        };
        world.run(&run, start - 10);
        let failed = (n % 2 == 0).then_some(anchor_facts::Failure::InvalidArguments);
        world.observe(
            &run,
            "worker",
            start + length + 5,
            &arm,
            vec![
                support::turn(1, start + 1_000, 900),
                support::call(
                    "anchor_edit",
                    1,
                    json!({"path": "x"}),
                    (start + 1_000, start + 1_010),
                    failed,
                ),
                support::turn(2, start + 2_000, 900),
                support::call(
                    "final_result",
                    2,
                    json!({}),
                    (start + 2_000, start + 2_001),
                    None,
                ),
                support::lifecycle(
                    (start, start + length),
                    anchor_facts::Outcome::Completed,
                    2,
                    2,
                ),
            ],
        );
        world.result(&run, "worker#1", start + length + 10, 2, true);
        let passed = n != 5;
        world.grade(&run, "ledger", &[("total", passed)], start + length + 20);
    }
}

#[test]
fn contrast_compares_arms_on_quality_first_then_cost() {
    let world = World::new();
    experiment(&world);
    let mut table = MemoryTable::default();
    let mut projector = Projector::default();
    for name in ["kernel-runs", "kernel-facts", "kernel-grades"] {
        projector
            .pull(
                &DataRoot::open(&world.root(name), name).unwrap(),
                &mut table,
            )
            .unwrap();
    }
    let report = Contrast::default().analyze(&Query::new(table.facts(), None));
    assert!(report.not_applicable.is_empty());
    assert_eq!(report.summary["arms"], json!(["host", "host-b"]));
    let task = &report.summary["tasks"]["ledger v1"];
    assert_eq!(task["arms"]["host"]["runs"], json!(3));
    assert_eq!(task["arms"]["host-b"]["mean_score"], json!(0.667));
    let against = &task["against_reference"]["host-b"];
    assert_eq!(against["score_change"], json!(-0.333));
    let ratio = against["agent_ms_ratio"].as_f64().unwrap();
    assert!((ratio - 5_300.0 / 10_200.0).abs() < 0.002, "{ratio}");
    assert!(against["agent_ms_p"].as_f64().unwrap() < 0.1);
    let overall = &report.summary["overall"];
    assert_eq!(overall["host"]["tool_failure_rate"], json!(0.5));
    assert_eq!(overall["host-b"]["tool_failure_rate"], json!(0.0));
    assert!(overall["host-b"]["agent_ms_relative"].as_f64().unwrap() < 0.55);
    let loss = finding(&report, "contrast_quality_loss");
    assert_eq!(loss.subject, "ledger v1: host-b");
    assert_eq!(loss.evidence.len(), 3);

    // The reference may be named; the other arm is then compared to it.
    let named = Contrast {
        reference: Some("host-b".into()),
        ..Contrast::default()
    }
    .analyze(&Query::new(table.facts(), None));
    assert_eq!(named.summary["arms"], json!(["host-b", "host"]));
    let back = &named.summary["tasks"]["ledger v1"]["against_reference"]["host"];
    assert_eq!(back["score_change"], json!(0.333));
    assert!(
        named
            .findings
            .iter()
            .all(|f| f.code != "contrast_quality_loss")
    );

    // Another field names other arms: here every Run shares one.
    let by_model = Contrast {
        by: "model".into(),
        reference: None,
    }
    .analyze(&Query::new(table.facts(), None));
    assert_eq!(by_model.summary["arms"], json!(["flash"]));
    let by_envelope = Contrast {
        by: "envelope_sha256".into(),
        reference: None,
    }
    .analyze(&Query::new(table.facts(), None));
    assert_eq!(
        by_envelope.summary["arms"],
        json!([
            support::configuration("worker").envelope_sha256,
            anchor_facts::text_digest("{\"network\":true}")
        ])
    );
}

/// One observed, accepted worker invocation: `turns` model turns, then the
/// tools called in order.
fn worked(
    world: &World,
    run: &str,
    start: u64,
    length: u64,
    config: &AgentConfiguration,
    turns: u64,
    tools: &[&str],
) {
    world.run(run, start - 10);
    let mut facts: Vec<_> = (1..=turns)
        .map(|i| support::turn(i, start + i * 100, 50))
        .collect();
    for (n, tool) in tools.iter().enumerate() {
        let at = start + 1_000 + n as u64 * 10;
        facts.push(support::call(tool, turns, json!({}), (at, at + 1), None));
    }
    facts.push(support::lifecycle(
        (start, start + length),
        Outcome::Completed,
        turns,
        tools.len() as u64,
    ));
    world.observe(run, "worker", start + length + 5, config, facts);
    world.result(run, "worker#1", start + length + 10, turns, true);
}

/// Two harnesses on one graded task, the second slower at equal tokens and
/// failing its check; and seven ungraded invocations, one far cheaper than
/// the rest and one taking a route nobody else took.
fn harnesses(world: &World) -> (AgentConfiguration, AgentConfiguration) {
    let fast = support::configuration("worker");
    let mut slow = support::configuration("worker");
    slow.host_sha256 = "host-b".into();
    for n in 0..3u64 {
        let start = 1_000_000 + n * 100_000;
        worked(
            world,
            &format!("a{n}"),
            start,
            5_000,
            &fast,
            2,
            &["anchor_edit", "final_result"],
        );
        world.grade(
            &format!("a{n}"),
            "ledger",
            &[("total", true)],
            start + 6_000,
        );
        let start = 2_000_000 + n * 100_000;
        worked(
            world,
            &format!("b{n}"),
            start,
            10_000,
            &slow,
            2,
            &["anchor_edit", "final_result"],
        );
        world.grade(
            &format!("b{n}"),
            "ledger",
            &[("total", false)],
            start + 11_000,
        );
    }
    for n in 0..7u64 {
        let (turns, tools): (u64, &[&str]) = match n {
            5 => (1, &["final_result"]),
            6 => (4, &["anchor_read", "final_result"]),
            _ => (4, &["anchor_edit", "final_result"]),
        };
        worked(
            world,
            &format!("u{n}"),
            3_000_000 + n * 100_000,
            5_000,
            &fast,
            turns,
            tools,
        );
    }
    (fast, slow)
}

#[test]
fn signal_citations_survive_bounded_health_report_support() {
    let world = World::new();
    for index in 0..2001 {
        world.grade(
            &format!("graded-{index:04}"),
            "ledger",
            &[("total", false)],
            100,
        );
    }
    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    hub.refresh().unwrap();
    let query = hub.query(None);
    let health = Health.analyze(&query);
    let health_signal = health
        .findings
        .iter()
        .find(|s| s.code == "check_failing")
        .unwrap();
    assert_eq!(health_signal.evidence.len(), 2000);
    let allowed = |key: &anchor_hub::query::Key| key.resource == "grade/graded-2000";
    assert!(health_signal.evidence.iter().all(|key| !allowed(key)));
    let table = Signals { limit: 20 }.window_table_in(&query, allowed);
    let signal = table
        .signals
        .iter()
        .find(|s| s.code == "check_failing")
        .expect("current evidence must survive upstream Health report bounds");
    assert_eq!(signal.evidence_total, 2001);
    assert_eq!(signal.runs, 2001);
    assert_eq!(signal.evidence.len(), 1);
    assert!(signal.evidence.iter().all(allowed));
}

#[test]
fn window_citation_filter_precedes_bounds_for_all_signal_and_exemplar_sources() {
    let world = World::new();
    let (_, slow) = harnesses(&world);
    let cited = anchor_facts::window::MAX_SIGNAL_CITATIONS;
    for index in 3..cited + 2 {
        let run = format!("b{index}");
        worked(
            &world,
            &run,
            2_000_000 + index as u64 * 100_000,
            10_000,
            &slow,
            2,
            &["anchor_edit", "final_result"],
        );
        world.grade(&run, "ledger", &[("total", false)], 2_900_000);
    }
    world.run("unobserved", 4_000_000);
    for index in 0..cited + 2 {
        world.result(
            "unobserved",
            &format!("op#{}", index + 1),
            4_000_001 + index as u64,
            0,
            false,
        );
    }
    for run in ["u5", "u6"] {
        for index in 0..cited + 1 {
            world.observe(
                run,
                "worker",
                5_000_000 + index as u64,
                &support::configuration("worker"),
                vec![anchor_facts::Fact::InputWait { waiting: false }],
            );
        }
    }
    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    hub.refresh().unwrap();
    let query = hub.query(None);
    let allowed = |key: &anchor_hub::query::Key| match key.resource.as_str() {
        "run/unobserved" | "trace/u5/worker/1" | "trace/u6/worker/1" => key.place > cited,
        resource => resource
            .strip_prefix("grade/b")
            .and_then(|index| index.parse::<usize>().ok())
            .is_some_and(|index| index >= cited),
    };
    let all = Signals { limit: usize::MAX }.table(&query);
    let filtered = Signals { limit: usize::MAX }.window_table_in(&query, allowed);
    assert_eq!(
        filtered.front, all.front,
        "historical grading support is unchanged"
    );
    for code in [
        "check_failing",
        "dominated_harness",
        "runs_without_observations",
        "frugal_route",
        "rare_route",
    ] {
        let original = all
            .signals
            .iter()
            .chain(&all.exemplars)
            .find(|signal| signal.code == code)
            .unwrap_or_else(|| panic!("missing fixture signal {code}"));
        let current = filtered
            .signals
            .iter()
            .chain(&filtered.exemplars)
            .find(|signal| signal.id == original.id)
            .unwrap_or_else(|| panic!("current {code} citations were truncated behind support"));
        assert!(original.evidence_total > cited, "{code}");
        assert!(original.evidence.iter().all(|key| !allowed(key)), "{code}");
        assert!(!current.evidence.is_empty(), "{code}");
        let mut expected = original.clone();
        expected.evidence = current.evidence.clone();
        assert_eq!(
            *current, expected,
            "{code} support statistics or ranking changed"
        );
    }
    for run in ["u5", "u6"] {
        let sample = filtered
            .exemplars
            .iter()
            .find(|signal| {
                signal.code == "ordinary_sample" && signal.subject == format!("{run}/worker#1")
            })
            .unwrap_or_else(|| panic!("missing ordinary sample for {run}"));
        assert!(sample.evidence_total > cited);
        assert_eq!(sample.evidence.len(), 2);
    }
    assert!(
        filtered
            .signals
            .iter()
            .chain(&filtered.exemplars)
            .all(|signal| {
                !signal.evidence.is_empty()
                    && signal.evidence.len() <= cited
                    && signal.evidence.iter().all(allowed)
            })
    );
    let ranks: Vec<_> = filtered
        .signals
        .iter()
        .map(|signal| {
            all.signals
                .iter()
                .position(|original| original.id == signal.id)
                .unwrap()
        })
        .collect();
    assert!(ranks.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn signals_rank_beaten_harnesses_and_exemplars_with_stable_ids() {
    let world = World::new();
    let (fast, slow) = harnesses(&world);
    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    hub.refresh().unwrap();
    let table = Signals::default().table(&hub.query(None));

    // Only the unbeaten harness is on the front.
    let front = &table.front["ledger v1"];
    assert_eq!(front.len(), 1, "{front:#?}");
    assert_eq!(front[0].configs, vec![fast.digest()]);
    assert_eq!(front[0].mean_score, 1.0);
    let beaten = table
        .signals
        .iter()
        .find(|s| s.code == "dominated_harness")
        .expect("a beaten harness");
    assert_eq!(beaten.weight, 3);
    assert_eq!(beaten.evidence_total, 3);
    assert_eq!(beaten.coordinates.len(), 1);
    assert_eq!(beaten.coordinates[0].config, slow.digest());
    assert_eq!(beaten.coordinates[0].host.as_deref(), Some("host-b"));
    assert_eq!(
        beaten.coordinates[0].envelope.as_deref(),
        Some(&slow.envelope_sha256[..12])
    );
    assert!(
        beaten.detail.contains("score 1.000 against 0.000"),
        "{}",
        beaten.detail
    );
    assert_eq!(beaten.id.len(), 12);

    // Ungraded invocations are peers within their Graph.
    let codes: Vec<_> = table
        .exemplars
        .iter()
        .map(|s| (s.code, s.subject.as_str()))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("frugal_route", "graph g node worker"),
            ("rare_route", "graph g node worker")
        ]
    );
    assert!(
        table.exemplars[0]
            .detail
            .contains("u5/worker#1 took 110 tokens against a median of 1040")
    );
    assert!(
        table.exemplars[1]
            .detail
            .contains("u6/worker#1 is the only one of 7")
    );
    assert!(
        table.exemplars[1]
            .detail
            .ends_with("anchor_read → final_result")
    );
    assert!(table.omitted.is_empty());

    // More of the same keeps the signal's id and adds to its weight.
    worked(
        &world,
        "b3",
        2_400_000,
        10_000,
        &slow,
        2,
        &["anchor_edit", "final_result"],
    );
    world.grade("b3", "ledger", &[("total", false)], 2_411_000);
    hub.refresh().unwrap();
    let again = Signals::default().table(&hub.query(None));
    let same = again
        .signals
        .iter()
        .find(|s| s.code == "dominated_harness")
        .unwrap();
    assert_eq!((same.id.as_str(), same.weight), (beaten.id.as_str(), 4));

    // A bound counts what it leaves out.
    let bounded = Signals { limit: 1 }.table(&hub.query(None));
    assert_eq!((bounded.signals.len(), bounded.exemplars.len()), (1, 1));
    assert_eq!(
        bounded.omitted.values().sum::<usize>(),
        again.signals.len() - 1 + again.exemplars.len() - 1
    );
    // As a report, every signal is a finding that names its id.
    let report = Signals::default().analyze(&hub.query(None));
    assert_eq!(
        report.findings.len(),
        again.signals.len() + again.exemplars.len()
    );
    assert!(report.findings[0].detail.starts_with('['));
}

#[test]
fn the_mcp_server_answers_bounded_reads_and_sees_appends() {
    let world = World::new();
    let (fast, _) = harnesses(&world);
    support::r1(&world);
    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    let ask = |hub: &mut Hub, method: &str, params: serde_json::Value| {
        handle(
            hub,
            &json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
        )
        .expect("a reply")
    };
    let call = |hub: &mut Hub, name: &str, arguments: serde_json::Value| {
        let reply = ask(
            hub,
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap().to_owned();
        assert!(!text.contains(SECRET), "{name} leaked business content");
        (result["isError"].as_bool().unwrap(), text)
    };
    let read = |hub: &mut Hub, name: &str, arguments: serde_json::Value| {
        let (failed, text) = call(hub, name, arguments);
        assert!(!failed, "{name}: {text}");
        serde_json::from_str::<serde_json::Value>(&text).unwrap()
    };

    let init = ask(
        &mut hub,
        "initialize",
        json!({"protocolVersion": "2025-03-26"}),
    );
    assert_eq!(init["id"], json!(7));
    assert_eq!(init["result"]["protocolVersion"], json!("2025-03-26"));
    assert_eq!(init["result"]["serverInfo"]["name"], json!("anchor-hub"));
    let notification = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
    assert_eq!(handle(&mut hub, &notification), None);
    let tools = ask(&mut hub, "tools/list", json!({}));
    let tools = tools["result"]["tools"].as_array().unwrap();
    let names: Vec<_> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "signals",
            "report",
            "invocations",
            "invocation",
            "configurations",
            "calls",
            "check",
            "records"
        ]
    );
    assert!(
        tools
            .iter()
            .all(|t| t["annotations"]["readOnlyHint"] == json!(true))
    );

    let signals = read(&mut hub, "signals", json!({}));
    assert!(signals["front"]["ledger v1"].is_array());
    assert!(
        signals["signals"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["code"] == "dominated_harness")
    );

    // Lists are paged and filters combine.
    let first = read(
        &mut hub,
        "invocations",
        json!({"node": "worker", "limit": 2}),
    );
    let total = first["total"].as_u64().unwrap();
    assert!(total >= 13, "{first}");
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    assert_eq!(first["next_offset"], json!(2));
    let routed = read(
        &mut hub,
        "invocations",
        json!({"tool": "anchor_read", "outcome": "completed"}),
    );
    assert_eq!(routed["total"], json!(1));
    assert_eq!(routed["items"][0]["key"], json!("u6/worker#1"));
    let graded = read(
        &mut hub,
        "invocations",
        json!({"task": "ledger v1", "config": &fast.digest()[..8]}),
    );
    assert_eq!(graded["total"], json!(3));
    assert_eq!(graded["items"][0]["grade"]["passed"], json!(true));

    // Drilling down: one invocation, the records it cites, its manifest.
    let one = read(&mut hub, "invocation", json!({"key": "u5/worker#1"}));
    assert_eq!(one["brief"]["route"], json!("final_result"));
    assert_eq!(one["brief"]["tokens"], json!(110));
    assert_eq!(one["calls"]["total"], json!(1));
    let cited = &one["evidence"][0];
    let records = read(
        &mut hub,
        "records",
        json!({"resource": cited["resource"], "place": cited["place"]}),
    );
    assert_eq!(records["total"], json!(1));
    let manifest = read(
        &mut hub,
        "configurations",
        json!({"digest": &fast.digest()[..10]}),
    );
    assert_eq!(manifest["manifest"]["host_sha256"], json!("host"));
    assert_eq!(
        manifest["manifest"]["envelope_sha256"],
        json!(fast.envelope_sha256)
    );
    assert_eq!(manifest["envelope"], json!({"network":false}));
    let listed = read(&mut hub, "configurations", json!({}));
    assert!(listed["total"].as_u64().unwrap() >= 2);

    let health = read(
        &mut hub,
        "report",
        json!({"analyzer": "health", "code": "tool_failures", "limit": 1}),
    );
    assert!(health["codes"]["tool_failures"].as_u64().unwrap() >= 1);
    assert_eq!(health["findings"]["items"].as_array().unwrap().len(), 1);
    let contrast = read(
        &mut hub,
        "report",
        json!({"analyzer": "contrast", "reference": "host"}),
    );
    assert_eq!(contrast["summary"]["arms"][0], json!("host"));

    // Every call reads what was appended since the last one.
    worked(
        &world,
        "late",
        9_000_000,
        5_000,
        &fast,
        1,
        &["final_result"],
    );
    let later = read(&mut hub, "invocations", json!({"node": "worker"}));
    assert_eq!(later["total"].as_u64().unwrap(), total + 1);

    // Mistakes are answered, not fatal.
    assert!(call(&mut hub, "write", json!({})).0);
    assert!(call(&mut hub, "invocation", json!({})).0);
    assert!(call(&mut hub, "invocations", json!({"limit": "ten"})).0);
    let unknown = ask(&mut hub, "resources/list", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32601));
}

#[test]
fn a_rejected_paths_family_is_recorded_and_a_recovery_needs_no_new_shape() {
    let world = World::new();
    let worker = support::configuration("worker");
    world.run("p1", 1_000);
    // Two rejections of an absolute path outside the workspace; the model then
    // asks the same way with a workspace path and is let through.
    world.observe(
        "p1",
        "worker",
        2_000,
        &worker,
        vec![
            support::turn(1, 1_100, 100),
            support::call(
                "anchor_read",
                1,
                json!({"path": "/local-inputs/data"}),
                (1_100, 1_110),
                Some(anchor_facts::Failure::InvalidArguments),
            ),
            support::turn(2, 1_200, 100),
            support::call(
                "anchor_read",
                2,
                json!({"path": "/in/previous/x"}),
                (1_200, 1_210),
                Some(anchor_facts::Failure::InvalidArguments),
            ),
            support::turn(3, 1_300, 100),
            support::call(
                "anchor_read",
                3,
                json!({"path": "/workspace/x"}),
                (1_300, 1_310),
                None,
            ),
            support::lifecycle((1_000, 1_400), anchor_facts::Outcome::Completed, 3, 3),
        ],
    );
    let (table, _) = project(&world);
    let query = Query::new(table.facts(), Some(2_000));
    let calls = query.invocations()[0].calls();
    assert_eq!(
        calls.iter().map(|c| c.path).collect::<Vec<_>>(),
        vec![
            Some(anchor_facts::PathClass::Absolute),
            Some(anchor_facts::PathClass::Absolute),
            Some(anchor_facts::PathClass::Workspace),
        ]
    );
    let report = Health.analyze(&query);
    let tools = &report.summary["tools"]["anchor_read"];
    assert_eq!(tools["paths"]["absolute"], json!({"calls": 2, "failed": 2}));
    assert_eq!(
        tools["paths"]["workspace"],
        json!({"calls": 1, "failed": 0})
    );
    let failed = finding(&report, "tool_failures");
    assert!(
        failed.detail.contains("on absolute path(s)"),
        "{}",
        failed.detail
    );
    // Recovery does not need a new shape: the same shape with another value
    // counts, and the count is of failed calls.
    let corrected = finding(&report, "same_tool_after_failure");
    assert!(
        corrected.detail.contains("2 failed call(s)")
            && corrected
                .detail
                .contains("0 reshaped (different argument shape)")
            && corrected
                .detail
                .contains("2 revalued (same argument shape)")
            && corrected.detail.contains("causal repair or resolution")
            && corrected
                .detail
                .contains("commands/targets may be unrelated"),
        "{}",
        corrected.detail
    );
}

#[test]
fn unrelated_tool_failures_are_weak_clues_not_repair_or_command_retries() {
    let world = World::new();
    let commands = [
        json!({"command": ["find", "/not-granted", "-type", "f"]}),
        json!({"command": ["git", "status"]}),
        json!({"command": ["curl", "https://example.invalid"]}),
        json!({"command": ["printf", "done"]}),
    ];
    let calls = commands.iter().enumerate().map(|(index, arguments)| {
        support::call(
            "anchor_run",
            index as u64 + 1,
            arguments.clone(),
            (1_100 + index as u64 * 100, 1_150 + index as u64 * 100),
            (index < 3).then_some(anchor_facts::Failure::NotAuthorized),
        )
    });
    let mut facts: Vec<_> = calls.collect();
    facts.push(support::lifecycle((1_000, 2_000), Outcome::Completed, 0, 4));
    world.observe(
        "unrelated",
        "worker",
        2_000,
        &support::configuration("worker"),
        facts,
    );
    let (table, _) = project(&world);
    let query = Query::new(table.facts(), Some(3_000));
    let health = Health.analyze(&query);
    let stall = Stall::default().analyze(&query);
    assert!(!health.findings.iter().any(|f| f.code == "call_corrected"));
    assert!(!stall.findings.iter().any(|f| f.code == "repeated_failure"));
    assert_eq!(
        health.summary["tools"]["anchor_run"]["failures"]["not_authorized"],
        json!(3)
    );
    let weak = finding(&health, "same_tool_after_failure");
    assert!(weak.detail.contains("later model request"));
    assert!(weak.detail.contains("causal repair or resolution"));
    assert!(weak.detail.contains("commands/targets may be unrelated"));
    let shape = finding(&stall, "consecutive_same_shape_failures");
    assert!(shape.detail.contains("operations/targets may differ"));
    assert!(shape.detail.contains("retry loop unconfirmed"));
    assert!(shape.detail.contains("program hints: curl, find, git"));
}

#[test]
fn shell_program_is_only_a_hint_and_command_end_counts_stay_authoritative() {
    use anchor_facts::{CommandEnd, CommandStatus, Fact, Failure};
    let world = World::new();
    let mut facts = Vec::new();
    for (index, hint) in ["find", "git", "curl"].into_iter().enumerate() {
        let mut call = support::call(
            "anchor_run",
            index as u64 + 1,
            json!({"command": ["sh", "-lc", "redacted"]}),
            (10 + index as u64, 11 + index as u64),
            Some(Failure::NotAuthorized),
        );
        if let Fact::ToolCall { shell_program, .. } = &mut call {
            *shell_program = Some(hint.into());
        }
        facts.push(call);
    }
    for (request, hint, end) in [
        (
            4,
            "python3",
            CommandEnd {
                status: CommandStatus::NotStarted,
                exit_code: Some(1),
            },
        ),
        (
            5,
            "grep",
            CommandEnd {
                status: CommandStatus::Exited,
                exit_code: Some(2),
            },
        ),
        (
            6,
            "printf",
            CommandEnd {
                status: CommandStatus::Exited,
                exit_code: Some(0),
            },
        ),
    ] {
        let mut call = command(request, "sh", Some(end));
        if let Fact::ToolCall { shell_program, .. } = &mut call {
            *shell_program = Some(hint.into());
        }
        facts.push(call);
    }
    world.observe(
        "shell",
        "worker",
        100,
        &support::configuration("worker"),
        facts,
    );
    let (table, _) = project(&world);
    let query = Query::new(table.facts(), None);
    let calls = query.invocations()[0].calls();
    assert!(calls.iter().all(|call| call.program == Some("sh")));
    assert_eq!(calls[0].shell_program, Some("find"));
    assert_eq!(calls[3].command.unwrap().status, CommandStatus::NotStarted);
    assert!(!calls[3].done());
    assert!(!calls[4].done());
    assert!(calls[5].done());
    let health = Health.analyze(&query);
    let summary = &health.summary["tools"]["anchor_run"];
    assert_eq!(summary["failures"]["not_authorized"], 3);
    assert_eq!(summary["commands"]["sh"]["not_started"], 1);
    assert_eq!(summary["commands"]["sh"]["exit_nonzero"], 1);
    assert_eq!(summary["commands"]["sh"]["clean"], 1);
    assert!(
        finding(&health, "command_failures")
            .subject
            .ends_with("program sh")
    );
    assert!(
        finding(&health, "command_nonzero")
            .subject
            .ends_with("program sh")
    );
    assert!(
        finding(&health, "same_tool_after_failure")
            .detail
            .contains("3 failed call(s)")
    );
    let stall = Stall::default().analyze(&query);
    assert!(
        finding(&stall, "consecutive_same_shape_failures")
            .detail
            .contains("shell-program hints: curl, find, git")
    );
    let response =
        anchor_hub::mcp::tool(&query, "calls", &json!({"key": "shell/worker#1"})).unwrap();
    assert_eq!(response["items"][0]["program"], "sh");
    assert_eq!(response["items"][0]["shell_program"], "find");
}

#[test]
fn parallel_calls_in_one_model_request_are_not_repair_or_retry_signals() {
    let world = World::new();
    let config = support::configuration("worker");
    let mut parallel = Vec::new();
    parallel.push(support::call(
        "anchor_run",
        1,
        json!({"command": ["find", "/nope"]}),
        (1_100, 1_110),
        Some(anchor_facts::Failure::NotAuthorized),
    ));
    parallel.push(support::call(
        "anchor_run",
        1,
        json!({"command": ["printf", "ok"]}),
        (1_110, 1_120),
        None,
    ));
    parallel.push(support::lifecycle((1_000, 1_200), Outcome::Completed, 0, 2));
    world.observe("parallel-success", "worker", 1_200, &config, parallel);

    let failed = (0..3).map(|index| {
        support::call(
            "anchor_read",
            1,
            json!({"path": "/nope", "mode": "read"}),
            (2_100 + index * 10, 2_105 + index * 10),
            Some(anchor_facts::Failure::InvalidArguments),
        )
    });
    let mut parallel_failed: Vec<_> = failed.collect();
    parallel_failed.push(support::lifecycle((2_000, 2_200), Outcome::Completed, 0, 3));
    world.observe("parallel-failed", "worker", 2_200, &config, parallel_failed);

    let (table, _) = project(&world);
    let query = Query::new(table.facts(), Some(3_000));
    let health = Health.analyze(&query);
    let stall = Stall::default().analyze(&query);
    assert!(
        !health
            .findings
            .iter()
            .any(|f| f.code == "same_tool_after_failure")
    );
    assert!(
        !stall
            .findings
            .iter()
            .any(|f| f.code == "consecutive_same_shape_failures")
    );
}

#[test]
fn failure_classes_stay_comparable_and_the_path_class_tells_wrong_places_apart() {
    // Classes keep what earlier experiments recorded; whether the model
    // addressed the wrong place is the path class's to say.
    assert_eq!(
        anchor_facts::classify_failure(
            "anchor_read refused: invalid arguments: absolute read paths must belong to an authorized read-only input. No change was made."
        ),
        anchor_facts::Failure::NotAuthorized
    );
    assert_eq!(
        anchor_facts::classify_failure(
            "anchor_run not_executed: command `find` is not authorized by the host sandbox policy"
        ),
        anchor_facts::Failure::NotAuthorized
    );
    use anchor_facts::PathClass;
    assert_eq!(anchor_facts::path_class("a/b"), PathClass::Relative);
    assert_eq!(anchor_facts::path_class("../x"), PathClass::Parent);
    assert_eq!(
        anchor_facts::path_class("/workspace/a"),
        PathClass::Workspace
    );
    assert_eq!(
        anchor_facts::path_class("/in/previous/a"),
        PathClass::Absolute
    );
}

/// A fixed generator, so the simulation is the same on every machine.
struct Lcg(u64);

impl Lcg {
    fn unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[test]
fn a_bet_watched_after_every_value_stays_within_its_error_rate() {
    // Pass/fail differences of two arms of equal quality, read after every
    // pair as a gate would be: the share of streams that ever reach 1/α
    // stays below α, though no one fixed when to stop.
    let mut random = Lcg(7);
    let (streams, pairs, alpha) = (2_000, 200, 0.05);
    let mut false_alarms = 0;
    for _ in 0..streams {
        let mut worse = Wealth::new(-1.0 - 0.0);
        let mut alarmed = false;
        for _ in 0..pairs {
            let a = (random.unit() < 0.8) as u8 as f64;
            let b = (random.unit() < 0.8) as u8 as f64;
            worse.push(-(b - a));
            alarmed |= worse.value() >= 1.0 / alpha;
        }
        false_alarms += alarmed as usize;
    }
    assert!(
        (false_alarms as f64 / streams as f64) < alpha,
        "{false_alarms}"
    );

    // A real loss of 0.3 in pass rate is found, and early.
    let mut found = Vec::new();
    for _ in 0..200 {
        let mut worse = Wealth::new(-1.0);
        let at = (1..=pairs).find(|_| {
            let a = (random.unit() < 0.9) as u8 as f64;
            let b = (random.unit() < 0.6) as u8 as f64;
            worse.push(a - b);
            worse.value() >= 1.0 / alpha
        });
        found.extend(at);
    }
    assert!(found.len() >= 190, "{}", found.len());
    found.sort();
    assert!(
        found[found.len() / 2] <= 60,
        "median {}",
        found[found.len() / 2]
    );

    // e-BH: the largest set whose k-th largest e-value reaches K/(αk).
    assert_eq!(e_bh(&[100.0, 30.0, 1.0, 0.5], 0.1), vec![0, 1]);
    assert_eq!(e_bh(&[39.0, 39.0], 0.05), vec![0, 1]);
    assert_eq!(e_bh(&[15.0, 15.0], 0.05), Vec::<usize>::new());
    assert_eq!(e_bh(&[], 0.05), Vec::<usize>::new());
}

/// Paired Runs of one task under two Hosts, interleaved, with the given
/// pass/fail outcomes for each arm in order.
fn paired(world: &World, task: &str, start: u64, outcomes: &[(bool, bool)]) {
    let mut variant = support::configuration("worker");
    variant.host_sha256 = "host-b".into();
    for (n, (a, b)) in outcomes.iter().enumerate() {
        for (arm, passed, config) in [
            ("a", a, support::configuration("worker")),
            ("b", b, variant.clone()),
        ] {
            let run = format!("{task}-{start}-{arm}{n}");
            let at = start + n as u64 * 10_000 + if arm == "a" { 0 } else { 5_000 };
            world.run(&run, at);
            world.observe(
                &run,
                "worker",
                at + 1_100,
                &config,
                vec![
                    support::turn(1, at + 500, 400),
                    support::lifecycle((at + 10, at + 1_000), Outcome::Completed, 1, 0),
                ],
            );
            world.result(&run, "worker#1", at + 1_200, 1, true);
            world.grade(&run, task, &[("total", *passed)], at + 1_300);
        }
    }
}

#[test]
fn the_gate_admits_an_arm_no_worse_anywhere_and_vetoes_one_worse_on_a_task() {
    let gate = |world: &World| {
        let mut table = MemoryTable::default();
        let mut projector = Projector::default();
        for name in ["kernel-runs", "kernel-facts", "kernel-grades"] {
            projector
                .pull(
                    &DataRoot::open(&world.root(name), name).unwrap(),
                    &mut table,
                )
                .unwrap();
        }
        Gate {
            delta: 0.1,
            ..Gate::default()
        }
        .analyze(&Query::new(table.facts(), None))
    };

    // Equal on two tasks, every pair passing: admitted once the pooled bet
    // is large enough, and not before.
    let world = World::new();
    paired(&world, "ledger", 1_000_000, &[(true, true); 3]);
    let early = gate(&world);
    assert_eq!(early.summary["arms"]["host-b"]["decision"], json!("open"));
    paired(&world, "ledger", 2_000_000, &[(true, true); 30]);
    paired(&world, "audit", 3_000_000, &[(true, true); 30]);
    let report = gate(&world);
    assert!(report.not_applicable.is_empty());
    assert_eq!(report.summary["reference"], json!("host"));
    assert_eq!(
        report.summary["arms"]["host-b"]["decision"],
        json!("admitted")
    );
    assert_eq!(report.summary["arms"]["host-b"]["pairs"], json!(63));
    assert_eq!(finding(&report, "gate_admits").subject, "host-b");

    // Better on one task does not buy a loss on another: the guard of the
    // task it loses vetoes it.
    let world = World::new();
    paired(&world, "ledger", 1_000_000, &[(false, true); 30]);
    paired(&world, "audit", 2_000_000, &[(true, false); 30]);
    let report = gate(&world);
    assert_eq!(
        report.summary["arms"]["host-b"]["decision"],
        json!("rejected")
    );
    assert_eq!(finding(&report, "gate_worse").subject, "audit v1: host-b");
    assert!(report.findings.iter().all(|f| f.code != "gate_admits"));
}

/// A command fact as the recorder now writes it: the program, and how the
/// command ended.
fn command(
    request: u64,
    program: &str,
    end: Option<anchor_facts::CommandEnd>,
) -> anchor_facts::Fact {
    let mut fact = support::call(
        "anchor_run",
        request,
        json!({"command": [program]}),
        (request * 10, request * 10 + 5),
        None,
    );
    if let anchor_facts::Fact::ToolCall { command, .. } = &mut fact {
        *command = end;
    }
    fact
}

#[test]
fn commands_that_did_not_run_cleanly_are_failures_ranked_by_the_runs_they_reach() {
    use anchor_facts::{CommandEnd, CommandStatus};
    let world = World::new();
    let worker = support::configuration("worker");
    let not_started = Some(CommandEnd {
        status: CommandStatus::NotStarted,
        exit_code: Some(1),
    });
    let clean = Some(CommandEnd {
        status: CommandStatus::Exited,
        exit_code: Some(0),
    });
    let nonzero = Some(CommandEnd {
        status: CommandStatus::Exited,
        exit_code: Some(2),
    });
    // Three Runs each try `python3` once, which never starts, and fall back
    // to `python3.12`; one Run fails a call's arguments three times.
    for (index, run) in ["a", "b", "c"].iter().enumerate() {
        let at = 1000 * index as u64;
        world.run(run, at);
        world.result(run, "worker#1", at + 100, 1, true);
        let mut facts = vec![
            support::turn(1, at + 20, 10),
            command(1, "python3", not_started),
            command(2, "python3.12", clean),
            command(3, "grep", nonzero),
        ];
        if *run == "a" {
            for request in 4..7 {
                facts.push(support::call(
                    "anchor_edit",
                    request,
                    json!({}),
                    (request * 10, request * 10 + 1),
                    Some(anchor_facts::Failure::InvalidArguments),
                ));
            }
        }
        facts.push(support::lifecycle(
            (at, at + 90),
            Outcome::Completed,
            1,
            facts.len() as u64 - 1,
        ));
        world.observe(run, "worker", at + 95, &worker, facts);
        world.grade(
            run,
            "ledger",
            &[("total", true), ("unpaid", *run != "c")],
            at + 110,
        );
    }
    let (mut table, mut projector) = project(&world);
    let grades = DataRoot::open(&world.root("kernel-grades"), "kernel-grades").unwrap();
    projector.pull(&grades, &mut table).unwrap();
    let query = Query::new(table.facts(), Some(10_000));

    let health = Health.analyze(&query);
    let python = health
        .findings
        .iter()
        .find(|f| f.code == "command_failures")
        .expect("python3 is reported");
    assert_eq!(python.subject, "tool anchor_run program python3");
    assert!(
        python
            .detail
            .contains("3 of 3 python3 command(s) never started"),
        "{}",
        python.detail
    );
    assert!(
        health
            .findings
            .iter()
            .any(|f| f.code == "command_nonzero" && f.subject.ends_with("grep"))
    );
    assert_eq!(
        health.summary["tools"]["anchor_run"]["commands"]["python3"],
        json!({"not_started": 3})
    );
    // The call to `python3.12` after `python3` is a recovery only once a
    // command ran cleanly.
    assert_eq!(health.summary["tools"]["anchor_run"]["done"], json!(3));
    // A check failing in one Run of three is a quality gap too.
    let unpaid = finding(&health, "check_failing");
    assert!(
        unpaid.detail.contains("check unpaid failed in 1 of 3"),
        "{}",
        unpaid.detail
    );

    // Signals of one tier rank by the Runs they reach, not by how often:
    // three Runs of python3 come before one Run's three bad edits.
    let table = Signals::default().table(&query);
    let order: Vec<_> = table.signals.iter().map(|s| (s.code, s.runs)).collect();
    let python_at = order
        .iter()
        .position(|(c, _)| *c == "command_failures")
        .unwrap();
    let edits_at = order
        .iter()
        .position(|(c, _)| *c == "tool_failures")
        .unwrap();
    assert!(python_at < edits_at, "{order:?}");
    assert_eq!(order[python_at].1, 3);
    assert_eq!(order[0].0, "check_failing", "quality first: {order:?}");

    // The profile says where each task's cost went.
    let profile = anchor_hub::analyzers::profile::Profile.analyze(&query);
    let ledger = &profile.summary["scopes"]["task ledger v1"];
    assert_eq!(ledger["runs"], json!(3));
    assert_eq!(ledger["turns"]["median"], json!(1));
    assert!(
        ledger["cheapest"]["route"]
            .as_str()
            .unwrap()
            .contains("anchor_run")
    );
}

#[test]
fn the_value_level_of_a_call_comes_from_the_ports_evidence_and_fills_older_facts() {
    let world = World::new();
    let worker = support::configuration("worker");
    world.run("r", 0);
    world.result("r", "worker#1", 100, 1, true);
    // An older record: the fact knows nothing of how the command ended.
    world.observe(
        "r",
        "worker",
        95,
        &worker,
        vec![
            support::call(
                "anchor_run",
                1,
                json!({"command": ["python3", "-c", "x"]}),
                (10, 15),
                None,
            ),
            support::lifecycle((0, 90), Outcome::Completed, 0, 1),
        ],
    );
    let evidence = world.state().join("goose-acp");
    std::fs::create_dir_all(&evidence).unwrap();
    let long = "y".repeat(5000);
    std::fs::write(
        evidence.join("k.evidence.json"),
        json!({
            "key": {"run_id": "r", "node_id": "worker", "invocation": 1},
            "tool_calls": [{"tool": "anchor_run", "tool_call_id": "c1",
                "arguments": {"command": ["python3", "-c", long]},
                "result": {"ok": true, "output": {"ok": true, "output": [{"type": "json", "value":
                    {"status": "completed", "exit_code": 1, "stdout": "", "stderr": "bwrap: execvp python3: No such file or directory\n"}}]}}}]
        })
        .to_string(),
    )
    .unwrap();
    let mut hub = Hub::new(vec![world.path().to_path_buf()]);
    hub.refresh().unwrap();
    let query = hub.query(Some(1000));
    let health = Health.analyze(&query);
    assert!(
        health
            .findings
            .iter()
            .any(|f| f.code == "command_failures" && f.subject.ends_with("python3")),
        "{:#?}",
        health.findings
    );
    let reply = handle(
        &mut hub,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "calls", "arguments": {"ended": "not_started", "group": true}}}),
    )
    .unwrap();
    let page: serde_json::Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    // Grouped, calls that ended alike are one counted row.
    assert_eq!(page["total"], json!(1));
    let row = &page["items"][0];
    assert_eq!(
        (&row["calls"], &row["runs"], &row["ended"]),
        (&json!(1), &json!(1), &json!("not_started"))
    );
    assert_eq!(row["invocations"], json!(["r/worker#1"]));
    assert!(
        row["first_line"]
            .as_str()
            .unwrap()
            .starts_with("bwrap: execvp python3")
    );
    // By default every call is listed with its own values: the shape
    // recorded checks point into.
    let reply = handle(
        &mut hub,
        &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "calls", "arguments": {"ended": "not_started"}}}),
    )
    .unwrap();
    let listed: serde_json::Value =
        serde_json::from_str(reply["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let item = &listed["items"][0];
    assert_eq!(item["invocation"], json!("r/worker#1"));
    assert_eq!(&row["example"], item);
    // Values are cut to a head.
    let arguments = item["arguments"].as_str().unwrap();
    assert!(
        arguments.len() < 1000 && arguments.contains("chars]"),
        "{arguments}"
    );
    // A query with no filter is refused rather than dumping every call.
    let refused = handle(
        &mut hub,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "calls", "arguments": {}}}),
    )
    .unwrap();
    assert_eq!(refused["result"]["isError"], json!(true));
}

#[test]
fn colliding_installations_keep_invocations_grades_and_details_independent() {
    let left = World::new();
    let right = World::new();
    for (world, graph, task, passed) in [
        (&left, "graph-a", "task-a", true),
        (&right, "graph-b", "task-b", false),
    ] {
        let mut config = support::configuration("worker");
        config.graph_digest = graph.into();
        world.run("same-run", 1);
        world.observe(
            "same-run",
            "worker",
            1_000,
            &config,
            vec![
                support::turn(1, 900, 100),
                support::call(
                    "anchor_run",
                    1,
                    json!({"command":["printf",task]}),
                    (910, 920),
                    None,
                ),
                support::lifecycle((800, 1_000), anchor_facts::Outcome::Completed, 1, 1),
            ],
        );
        world.result("same-run", "worker#1", 1_001, 1, true);
        world.grade("same-run", task, &[("quality", passed)], 1_002);
        let directory = world.state().join("goose-acp");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("same.evidence.json"), json!({
            "key":{"run_id":"same-run","node_id":"worker","invocation":1},
            "tool_calls":[{"tool":"anchor_run","arguments":{"command":["printf",task]},"result":{"ok":true,"output":task}}]
        }).to_string()).unwrap();
    }
    let mut hub = anchor_hub::adapters::Hub::new(vec![left.state(), right.state()]);
    hub.refresh().unwrap();
    let query = hub.query(None);
    let invocations = query.invocations();
    assert_eq!(invocations.len(), 2);
    assert_eq!(query.grades().len(), 2);
    assert_eq!(query.tasks().len(), 2);
    assert_ne!(invocations[0].namespace, invocations[1].namespace);
    for invocation in &invocations {
        assert_eq!(invocation.usage().total_tokens(), Some(110));
        assert!(invocation.result.is_some());
        assert!(!invocation.graph_conflict());
        let grade = query.grades()[&invocation.run_key()].grade;
        assert_eq!(invocation.details.len(), 1);
        assert!(invocation.details[0].arguments.contains(&grade.task));
        let item = anchor_hub::mcp::tool(
            &query,
            "invocation",
            &json!({"key":invocation.key(),"namespace":invocation.namespace}),
        )
        .unwrap();
        assert_eq!(item["brief"]["tokens"], 110);
        let qualified = anchor_hub::mcp::tool(
            &query,
            "invocation",
            &json!({"key":invocation.qualified_key()}),
        )
        .unwrap();
        assert_eq!(qualified["brief"]["namespace"], invocation.namespace);
    }
    for tool in ["invocation", "calls"] {
        assert!(
            anchor_hub::mcp::tool(&query, tool, &json!({"key":"same-run/worker#1"}))
                .unwrap_err()
                .contains("ambiguous")
        );
    }
}

#[test]
fn grade_authority_and_graph_conflicts_are_explicit_not_projection_order() {
    use anchor_hub::source::HistorySource;
    let world = World::new();
    let mut config = support::configuration("worker");
    for index in 0..3 {
        let run = format!("r{index}");
        world.run(&run, 1);
        world.observe(
            &run,
            "worker",
            100,
            &config,
            vec![
                support::turn(1, 50, 10),
                support::lifecycle((1, 100), anchor_facts::Outcome::Completed, 1, 0),
            ],
        );
        world.result(&run, "worker#1", 101, 1, true);
        world.grade(&run, "task", &[("quality", true)], 102);
    }
    world.grade("r1", "task", &[("quality", false)], 103);
    let (mut table, mut projector) = project(&world);
    let grade_source = DataRoot::open(&world.root("kernel-grades"), "grades").unwrap();
    projector.pull(&grade_source, &mut table).unwrap();
    let query = Query::new(table.facts(), None);
    assert_eq!(query.grades().len(), 3);
    let namespace = grade_source.namespace().to_owned();
    assert_eq!(query.grades()[&(namespace.as_str(), "r1")].row.key.place, 2);
    assert!(!query.grades()[&(namespace.as_str(), "r1")].grade.passed());
    let other = World::new();
    other.grade("r0", "different-task", &[("quality", false)], 1);
    let source = DataRoot::open(&other.root("kernel-grades"), "other-grades")
        .unwrap()
        .with_namespace(&namespace);
    projector.pull(&source, &mut table).unwrap();
    config.graph_digest = "changed-graph".into();
    world.observe(
        "r2",
        "worker",
        104,
        &config,
        vec![
            support::turn(1, 50, 10),
            support::lifecycle((1, 100), anchor_facts::Outcome::Completed, 1, 0),
        ],
    );
    projector
        .pull(
            &DataRoot::open(&world.root("kernel-facts"), "kernel-facts").unwrap(),
            &mut table,
        )
        .unwrap();
    let query = Query::new(table.facts(), None);
    assert_eq!(query.grades().len(), 2);
    assert_eq!(query.grade_conflicts().len(), 1);
    let invocations = query.invocations();
    let conflict = invocations
        .iter()
        .find(|invocation| invocation.run_id == "r2")
        .unwrap();
    assert!(conflict.graph_conflict());
    assert!(conflict.config().is_none());
    let health = Health.analyze(&query);
    assert!(
        health
            .findings
            .iter()
            .any(|finding| finding.code == "conflicting_grades")
    );
    assert!(
        health
            .findings
            .iter()
            .any(|finding| finding.code == "conflicting_invocation_graph")
    );
    let signals = anchor_hub::analyzers::signals::Signals::default().table(&query);
    assert!(signals.front.is_empty() || signals.front.values().all(Vec::is_empty));
    let applicability = serde_json::to_value(signals.applicability).unwrap();
    assert_eq!(applicability["grade_conflicts"], 1);
    assert_eq!(applicability["graph_conflicts"], 1);
    assert_eq!(applicability["grades"], "conflict");
    assert_eq!(applicability["observations"], "conflict");
}

#[test]
fn stable_v2_accumulates_dynamic_requests_and_v1_is_read_without_merging() {
    use anchor_facts::{Fact, Outcome};
    use anchor_hub::analyzers::signals::Signals;
    for count in [3, 10] {
        let world = World::new();
        let stable = support::configuration("worker");
        let mut contexts = std::collections::BTreeSet::new();
        for index in 0..count {
            let run = format!("dynamic-{index}");
            let task = anchor_facts::text_digest(&format!("Input: {{\"item\":{index}}}"));
            contexts.insert(task.clone());
            world.run(&run, 1);
            world.observe(
                &run,
                "worker",
                100,
                &stable,
                vec![
                    Fact::RequestContext {
                        task_sha256: task,
                        input_sha256: anchor_facts::text_digest(&json!({"item":index}).to_string()),
                        input_guidance_digest: None,
                    },
                    support::turn(1, 50, 10),
                    support::lifecycle((1, 100), Outcome::Completed, 1, 0),
                ],
            );
            world.result(&run, "worker#1", 101, 1, true);
            world.grade(&run, "same-task", &[("quality", true)], 102);
        }
        let mut hub = anchor_hub::adapters::Hub::new(vec![world.state()]);
        hub.refresh().unwrap();
        let query = hub.query(None);
        assert_eq!(query.configurations().count(), 1);
        assert_eq!(contexts.len(), count);
        assert_eq!(
            Signals::default().table(&query).front["same-task v1"][0].runs,
            count
        );
        let mut legacy = stable.clone();
        legacy.version = anchor_facts::LEGACY_CONFIGURATION_VERSION;
        assert_ne!(legacy.digest(), stable.digest());
        world.observe(
            "legacy",
            "worker",
            103,
            &legacy,
            vec![
                support::turn(1, 50, 10),
                support::lifecycle((1, 100), Outcome::Completed, 1, 0),
            ],
        );
        hub.refresh().unwrap();
        let query = hub.query(None);
        assert_eq!(query.configuration(&legacy.digest()).unwrap(), &legacy);
        let table = Signals::default().table(&query);
        assert_eq!(table.applicability.legacy_v1_configurations, 1);
        assert_eq!(table.front["same-task v1"][0].runs, count);
        let health = Health.analyze(&query);
        assert!(
            health.summary["configurations"]
                .to_string()
                .contains("legacy_unstable_config")
        );
    }
}

#[test]
fn accepted_run_metadata_graph_must_agree_with_its_observation() {
    let world = World::new();
    world
        .runs
        .create(
            anchor_kernel::Resource::new(
                "run/disagrees",
                "validator/step",
                json!({"graph_digest":"run-graph"}),
            )
            .task(u64::MAX),
        )
        .unwrap();
    world.result("disagrees", "worker#1", 100, 1, true);
    let mut config = support::configuration("worker");
    config.graph_digest = "trace-graph".into();
    world.observe(
        "disagrees",
        "worker",
        99,
        &config,
        vec![
            support::turn(1, 50, 10),
            support::lifecycle((1, 99), anchor_facts::Outcome::Completed, 1, 0),
        ],
    );
    let mut hub = anchor_hub::adapters::Hub::new(vec![world.state()]);
    hub.refresh().unwrap();
    let query = hub.query(None);
    assert!(query.invocations()[0].graph_conflict());
    assert!(
        Health
            .analyze(&query)
            .findings
            .iter()
            .any(|finding| finding.code == "conflicting_invocation_graph")
    );
}
