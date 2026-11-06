#![allow(dead_code)]
mod support;

use anchor_facts::{Fact, ModelUsage, Outcome, UsageReportedTurns};
use anchor_hub::{
    adapters::Hub,
    analyzers::{
        Analyzer, contrast::Contrast, gate::Gate, health::Health, profile::Profile,
        signals::Signals,
    },
    mcp,
    query::{UsageCoverage, agent_ms, usage},
};
use serde_json::json;
use support::World;

fn accounting(messages: u64, sampled: u64, start: u64, complete: bool) -> Vec<Fact> {
    let mut facts: Vec<_> = (1..=sampled)
        .map(|index| Fact::ModelTurn {
            index,
            message_id: Some(format!("m{start}-{index}")),
            observed_at_ms: Some(start + 10 + index * 8),
            elapsed_ms: Some(7),
            time_to_first_token_ms: None,
            input_tokens: Some(10),
            output_tokens: Some(2),
            total_tokens: Some(12),
            cache_read_tokens: Some(5),
            cache_write_tokens: None,
            cost_usd: None,
            compaction: false,
        })
        .collect();
    facts.push(Fact::ModelUsage {
        usage: ModelUsage {
            complete,
            messages,
            sampled_turns: sampled,
            omitted_turns: messages - sampled,
            input_tokens: Some(messages * 10),
            output_tokens: Some(messages * 2),
            total_tokens: Some(messages * 12),
            cache_read_tokens: Some(messages * 5),
            elapsed_ms: Some(messages * 7),
            reported_turns: UsageReportedTurns {
                input_tokens: messages,
                output_tokens: messages,
                total_tokens: messages,
                cache_read_tokens: messages,
                elapsed_ms: messages,
                ..Default::default()
            },
            ..Default::default()
        },
    });
    facts.push(support::lifecycle(
        (start, start + 1_000),
        if complete {
            Outcome::Completed
        } else {
            Outcome::Interrupted
        },
        messages,
        0,
    ));
    facts
}

fn hub(world: &World) -> Hub {
    let mut hub = Hub::new(vec![world.state()]);
    hub.refresh().unwrap();
    hub
}

#[test]
fn full_accounting_not_bounded_samples_drives_reports_and_mcp() {
    let world = World::new();
    world.run("long", 1);
    world.observe(
        "long",
        "worker",
        1_100,
        &support::configuration("worker"),
        accounting(70, 64, 100, true),
    );
    world.result("long", "op#1", 1_200, 0, false);
    world.result("long", "worker#1", 1_200, 70, true);
    world.observe(
        "long",
        "idle",
        1_200,
        &support::configuration("idle"),
        vec![Fact::InputWait { waiting: true }],
    );
    let hub = hub(&world);
    let query = hub.query(None);
    let invocations = query.invocations();
    let measured = invocations
        .iter()
        .find(|i| i.node_id == "worker")
        .unwrap()
        .usage();
    assert_eq!(measured.coverage, UsageCoverage::Complete);
    assert_eq!(measured.total_tokens(), Some(840));
    assert_eq!(measured.elapsed_ms(), Some(490));
    assert_eq!(measured.totals.sampled_turns, 64);
    assert_eq!(measured.totals.omitted_turns, 6);
    assert!(!measured.details_complete());
    assert_eq!(agent_ms(invocations.iter()), Some(1_000));

    let health = Health.analyze(&query);
    assert!(
        !health
            .findings
            .iter()
            .any(|f| f.code == "counts_disagree" || f.code == "usage_incomplete")
    );
    assert_eq!(health.summary["turns"]["per_invocation"]["max"], 70);
    assert_eq!(health.summary["turns"]["cache_read_share"], 0.5);
    assert_eq!(health.summary["time"]["model_share"], 0.49);
    assert_eq!(health.summary["time"]["timeline_complete"], false);
    assert!(health.summary["time"]["unobserved_share"].is_null());
    let profile = Profile.analyze(&query);
    assert_eq!(profile.summary["all"]["tokens"], 840);
    let one = mcp::tool(&query, "invocation", &json!({"key":"long/worker#1"})).unwrap();
    assert_eq!(one["brief"]["tokens"], 840);
    assert_eq!(one["brief"]["turns"], 70);
    assert_eq!(one["brief"]["usage"]["totals"]["omitted_turns"], 6);
    assert_eq!(one["turns"]["total"], 64);
}

#[test]
fn legacy_truncation_is_visible_but_never_ranked_as_cheap() {
    let world = World::new();
    world.run("legacy", 1);
    let facts = accounting(70, 64, 100, true)
        .into_iter()
        .filter(|f| !matches!(f, Fact::ModelUsage { .. }))
        .collect();
    world.observe(
        "legacy",
        "worker",
        1_100,
        &support::configuration("worker"),
        facts,
    );
    world.result("legacy", "worker#1", 1_200, 70, true);
    let hub = hub(&world);
    let query = hub.query(None);
    let invocations = query.invocations();
    let measured = invocations[0].usage();
    assert_eq!(measured.coverage, UsageCoverage::Partial);
    assert_eq!(measured.total_tokens(), None);
    assert_eq!(measured.totals.total_tokens, Some(768));
    assert_eq!(measured.totals.omitted_turns, 6);
    assert_eq!(agent_ms(invocations.iter()), Some(1_000));
    let profile = Profile.analyze(&query);
    assert!(profile.summary["all"]["tokens"].is_null());
    assert_eq!(profile.summary["all"]["runs_with_incomplete_tokens"], 1);
    let scope = profile.summary["scopes"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert!(scope["cheapest"].is_null());
    assert!(scope["dearest"].is_null());
    assert!(scope["model_share"].is_null());
    assert!(mcp::tool(&query, "invocations", &json!({})).unwrap()["items"][0]["tokens"].is_null());
}

#[test]
fn legacy_complete_counts_do_not_fill_unreported_optional_fields() {
    let world = World::new();
    world.run("old", 1);
    let mut facts: Vec<_> = accounting(2, 2, 100, true)
        .into_iter()
        .filter(|f| !matches!(f, Fact::ModelUsage { .. }))
        .collect();
    if let Fact::ModelTurn {
        input_tokens,
        elapsed_ms,
        cache_read_tokens,
        ..
    } = &mut facts[1]
    {
        *input_tokens = None;
        *elapsed_ms = None;
        *cache_read_tokens = None;
    }
    world.observe(
        "old",
        "worker",
        1_100,
        &support::configuration("worker"),
        facts,
    );
    let hub = hub(&world);
    let query = hub.query(None);
    let measured = query.invocations()[0].usage();
    assert_eq!(measured.coverage, UsageCoverage::LegacyComplete);
    assert_eq!(measured.total_tokens(), Some(24));
    assert_eq!(measured.input_tokens(), None);
    assert_eq!(measured.elapsed_ms(), None);
    assert_eq!(measured.cache_read_tokens(), None);
    assert_eq!(measured.cost_usd(), None);
    assert_eq!(measured.totals.elapsed_ms, Some(7));
    assert_eq!(measured.totals.reported_turns.elapsed_ms, 1);
}

#[test]
fn missing_token_fields_and_timing_stay_unknown_in_facts_reports_and_mcp() {
    let world = World::new();
    world.run("fields", 1);
    let mut facts = accounting(1, 1, 100, true);
    for fact in &mut facts {
        match fact {
            Fact::ModelTurn {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                elapsed_ms,
                ..
            } => {
                *input_tokens = None;
                *output_tokens = None;
                *cache_read_tokens = None;
                *elapsed_ms = None;
            }
            Fact::ModelUsage { usage } => {
                usage.input_tokens = None;
                usage.output_tokens = None;
                usage.cache_read_tokens = None;
                usage.elapsed_ms = None;
                usage.reported_turns = UsageReportedTurns {
                    total_tokens: 1,
                    ..Default::default()
                };
            }
            _ => {}
        }
    }
    world.observe(
        "fields",
        "worker",
        1_100,
        &support::configuration("worker"),
        facts,
    );
    world.result("fields", "worker#1", 1_200, 1, true);
    let hub = hub(&world);
    let query = hub.query(None);
    let health = Health.analyze(&query);
    assert_eq!(health.summary["turns"]["total_tokens"]["min"], 12);
    assert!(health.summary["turns"]["input_tokens"].is_null());
    assert!(health.summary["turns"]["output_tokens"].is_null());
    assert!(health.summary["turns"]["elapsed_ms"].is_null());
    assert!(!health.findings.iter().any(|f| f.code == "startup_heavy"));
    assert_eq!(health.summary["time"]["timeline_complete"], false);
    let profile = Profile.analyze(&query);
    let scope = profile.summary["scopes"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert!(scope["context_growth"].is_null());
    let one = mcp::tool(&query, "invocation", &json!({"key":"fields/worker#1"})).unwrap();
    assert_eq!(one["brief"]["tokens"], 12);
    assert_eq!(one["brief"]["timeline_complete"], false);
    assert!(one["turns"]["items"][0]["input_tokens"].is_null());
    assert!(one["turns"]["items"][0]["output_tokens"].is_null());
    assert_eq!(one["turns"]["items"][0]["total_tokens"], 12);
}

#[test]
fn resumed_attempts_sum_their_accounting_once_and_do_not_reuse_turn_indexes() {
    let world = World::new();
    world.run("resume", 1);
    let configuration = support::configuration("worker");
    for (start, complete) in [(100, false), (2_000, true)] {
        let mut facts = accounting(1, 1, start, complete);
        facts.push(support::call(
            "anchor_read",
            1,
            json!({}),
            (start + 17, start + 18),
            None,
        ));
        if let Some(Fact::Lifecycle {
            tool_calls,
            resumed,
            ..
        }) = facts
            .iter_mut()
            .find(|f| matches!(f, Fact::Lifecycle { .. }))
        {
            *tool_calls = 1;
            *resumed = start == 2_000;
        }
        world.observe("resume", "worker", start + 1_100, &configuration, facts);
    }
    let hub = hub(&world);
    let query = hub.query(None);
    let invocations = query.invocations();
    let measured = invocations[0].usage();
    assert_eq!(measured.attempts, 2);
    assert_eq!(measured.totals.messages, 2);
    assert_eq!(measured.totals.total_tokens, Some(24));
    assert_eq!(measured.total_tokens(), None);
    assert_eq!(measured.coverage, UsageCoverage::Partial);
    assert_eq!(agent_ms(invocations.iter()), Some(2_000));
    assert_eq!(
        invocations[0].turn_spans(),
        [(1, 110..117), (1, 2_010..2_017)]
    );
    assert_eq!(Health.analyze(&query).summary["invocations"]["resumed"], 1);
}

#[test]
fn first_saved_attempt_resumed_keeps_prior_cost_and_time_unknown() {
    let world = World::new();
    world.run("lost", 1);
    let configuration = support::configuration("worker");
    world.observe(
        "lost",
        "worker",
        50,
        &configuration,
        vec![Fact::InputWait { waiting: false }],
    );
    let mut facts = accounting(1, 1, 100, true);
    if let Some(Fact::Lifecycle { resumed, .. }) = facts
        .iter_mut()
        .find(|fact| matches!(fact, Fact::Lifecycle { .. }))
    {
        *resumed = true;
    }
    world.observe("lost", "worker", 1_100, &configuration, facts);
    world.result("lost", "worker#1", 1_200, 1, true);
    let hub = hub(&world);
    let query = hub.query(None);
    let invocations = query.invocations();
    let one = &invocations[0];
    let measured = one.usage();
    assert_eq!(measured.attempts, 1);
    assert_eq!(measured.unknown_attempts, 1);
    assert_eq!(measured.totals.total_tokens, Some(12));
    assert_eq!(measured.coverage, UsageCoverage::Partial);
    assert_eq!(measured.total_tokens(), None);
    assert_eq!(one.observed_agent_ms(), 1_000);
    assert_eq!(one.agent_ms(), None);
    assert_eq!(agent_ms(invocations.iter()), None);
    let one = mcp::tool(&query, "invocation", &json!({"key":"lost/worker#1"})).unwrap();
    assert!(one["brief"]["agent_ms"].is_null());
    assert_eq!(one["brief"]["observed_agent_ms"], 1_000);
}

#[test]
fn unobserved_model_work_makes_run_cost_unknown_not_zero() {
    let world = World::new();
    world.run("missing", 1);
    world.observe(
        "missing",
        "worker",
        1_100,
        &support::configuration("worker"),
        accounting(1, 1, 100, true),
    );
    world.result("missing", "worker#1", 1_200, 1, false);
    world.result("missing", "critic#1", 1_300, 1, true);
    world.grade("missing", "ledger", &[("correct", true)], 1_400);
    let hub = hub(&world);
    let query = hub.query(None);
    let invocations = query.invocations();
    let measured = usage(invocations.iter());
    assert_eq!(measured.total_tokens(), None);
    assert_eq!(measured.unknown_attempts, 1);
    assert_eq!(measured.totals.total_tokens, Some(12));
    assert_eq!(agent_ms(invocations.iter()), None);
    let profile = Profile.analyze(&query);
    assert!(profile.summary["all"]["tokens"].is_null());
    let scope = &profile.summary["scopes"]["task ledger v1"];
    assert!(scope["agent_ms"].is_null());
    assert!(scope["model_share"].is_null());
    assert!(scope["tool_share"].is_null());
    assert_eq!(scope["observed_agent_ms"]["min"], 1_000);
    assert_eq!(scope["runs_with_incomplete_agent_time"], 1);
    assert!(Health.analyze(&query).summary["quality"]["ledger v1"]["agent_ms"].is_null());
    let one = mcp::tool(&query, "invocation", &json!({"key":"missing/critic#1"})).unwrap();
    assert!(one["brief"]["agent_ms"].is_null());
    assert_eq!(one["brief"]["observed_agent_ms"], 0);
}

#[test]
fn unknown_cost_does_not_remove_quality_evidence_or_create_a_frontier() {
    let world = World::new();
    for arm in ["old", "new"] {
        for index in 0..3 {
            let run = format!("{arm}-{index}");
            world.run(&run, 1);
            let mut configuration = support::configuration("worker");
            configuration.host_sha256 = arm.into();
            let facts = accounting(70, 64, 100, true)
                .into_iter()
                .filter(|f| !matches!(f, Fact::ModelUsage { .. }))
                .collect();
            world.observe(&run, "worker", 1_100, &configuration, facts);
            world.result(&run, "worker#1", 1_200, 70, arm == "old");
            if arm == "new" {
                world.result(&run, "critic#1", 1_250, 1, true);
            }
            world.grade(&run, "ledger", &[("correct", true)], 1_300);
        }
    }
    let hub = hub(&world);
    let query = hub.query(None);
    let contrast = Contrast {
        reference: Some("old".into()),
        ..Default::default()
    }
    .analyze(&query);
    assert!(contrast.summary["tasks"]["ledger v1"]["arms"]["old"]["tokens"].is_null());
    assert_eq!(
        contrast.summary["tasks"]["ledger v1"]["arms"]["old"]["mean_score"],
        1.0
    );
    assert!(contrast.summary["overall"]["new"]["tokens_relative"].is_null());
    assert!(contrast.summary["overall"]["new"]["agent_ms_relative"].is_null());
    assert_eq!(contrast.summary["overall"]["new"]["runs"], 3);
    assert_eq!(
        contrast.summary["overall"]["new"]["runs_with_complete_agent_time"],
        0
    );
    assert!(contrast.summary["tasks"]["ledger v1"]["arms"]["new"]["agent_ms"].is_null());
    assert_eq!(
        contrast.summary["tasks"]["ledger v1"]["arms"]["old"]["agent_ms"]["min"],
        1_000
    );
    assert!(
        contrast.summary["tasks"]["ledger v1"]["against_reference"]["new"]["agent_ms_ratio"]
            .is_null()
    );
    assert_eq!(
        contrast.summary["tasks"]["ledger v1"]["arms"]["new"]["mean_score"],
        1.0
    );
    assert_eq!(
        Gate {
            reference: Some("old".into()),
            ..Default::default()
        }
        .analyze(&query)
        .summary["arms"]["new"]["pairs"],
        3
    );
    assert!(Signals::default().table(&query).front.is_empty());
}
