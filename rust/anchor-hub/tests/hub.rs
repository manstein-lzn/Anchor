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
use anchor_hub::source::{DataRoot, Stored};
use anchor_hub::table::{Body, FactTable, MemoryTable, Projector};
use anchor_kernel::{MemoryStore, Record, Resource, Store};
use serde_json::json;
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
            ("tool_failures", "tool anchor_run".into()),
            ("tool_failures", "tool anchor_read".into()),
            ("call_corrected", "tool anchor_run".into()),
        ])
    );
    let refused = finding(&report, "not_accepted");
    assert!(refused.detail.contains("task_settled"));
    assert_eq!(refused.evidence[0].place, 5);
    assert_eq!(finding(&report, "unobserved_invocation").evidence.len(), 1);
    assert_eq!(finding(&report, "call_corrected").evidence.len(), 2);

    let time = &report.summary["time"];
    let shares = ["model_share", "tool_share", "unobserved_share"]
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
fn stall_finds_stalled_runs_silences_slow_calls_and_loops() {
    let world = World::new();
    support::r1(&world);
    support::r2(&world);
    let (table, _) = project(&world);
    let now = 136_200 + 16 * 60_000;
    let report = Stall::default().analyze(&Query::new(table.facts(), Some(now)));
    assert_eq!(
        codes(&report),
        BTreeSet::from([
            ("stalled_task", "run/r2".into()),
            ("idle_gap", "r2/worker#1".into()),
            ("slow_tool_call", "tool anchor_run".into()),
            ("repeated_failure", "r2/worker#1".into()),
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
        finding(&report, "repeated_failure")
            .detail
            .contains("3 consecutive failed anchor_read")
    );

    // Not yet idle long enough: no task finding.
    let early = Stall::default().analyze(&Query::new(table.facts(), Some(136_200 + 60_000)));
    assert!(!early.findings.iter().any(|f| f.code == "stalled_task"));
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
    assert!(grades["r3"].grade.passed());
    assert_eq!(grades["r3"].row.key.place, 2);

    let report = Health.analyze(&query);
    assert!(report.not_applicable.is_empty());
    let quality = &report.summary["quality"]["ledger v1"];
    assert_eq!(quality["runs"], json!(3));
    assert_eq!(quality["pass_rate"], json!(0.333));
    assert_eq!(quality["mean_score"], json!(0.5));
    assert_eq!(quality["checks"]["total"], json!(0.333));
    assert_eq!(quality["checks"]["unpaid"], json!(0.667));
    assert_eq!(quality["tokens"]["n"], json!(3));
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
    let corrected = finding(&report, "call_corrected");
    assert!(
        corrected.detail.contains("2 failed call(s)")
            && corrected
                .detail
                .contains("0 with arguments of another shape")
            && corrected
                .detail
                .contains("2 with arguments of the same shape"),
        "{}",
        corrected.detail
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
