//! Module entry facts are accepted by the Graph, independently of scheduling.
use super::*;
use anchor_kernel::{Authority, Effect, Kernel, MemoryStore, Principal};
use std::sync::Arc;

struct Allow;
impl Authority for Allow {
    fn permits(&self, _: &Principal, _: Action, _: &str) -> bool {
        true
    }
}

fn graph(branches: &[&[&str]], loop_back: bool) -> GraphSnapshot {
    let mut snapshot = super::super::test_support::random_graph(0);
    snapshot.entry = "start".into();
    snapshot.module_rounds.clear();
    snapshot.ops = BTreeMap::from([
        (
            "fanout".into(),
            serde_json::json!({"fanout":{"join":"collect"}}),
        ),
        ("join".into(), serde_json::json!({"join":{}})),
    ]);
    snapshot.nodes = std::iter::once("start")
        .chain(branches.iter().flat_map(|branch| branch.iter().copied()))
        .chain(["collect", "after"])
        .map(|id| {
            let op = match id {
                "start" => Some("fanout".into()),
                "collect" => Some("join".into()),
                _ => None,
            };
            GraphNode {
                id: id.into(),
                agent: op.is_none().then(|| "worker".into()),
                op,
                input: None,
                plugins: vec![],
                max_rounds: None,
            }
        })
        .collect();
    let edge = |from: &str, to: &str| GraphEdge {
        from_node: from.into(),
        to_node: to.into(),
    };
    snapshot.edges = branches
        .iter()
        .flat_map(|branch| {
            std::iter::once(edge("start", branch[0]))
                .chain(branch.windows(2).map(|pair| edge(pair[0], pair[1])))
                .chain(std::iter::once(edge(branch.last().unwrap(), "collect")))
        })
        .chain(std::iter::once(edge("collect", "after")))
        .collect();
    if loop_back {
        snapshot.edges.push(edge("after", "start"));
    }
    snapshot
}

fn kernel(record: GraphRunRecord) -> (Kernel<MemoryStore>, String) {
    let mut kernel = Kernel::new(MemoryStore::default(), Box::new(Allow)).unwrap();
    kernel
        .install(GRAPH_VALIDATOR, Arc::new(GraphValidator))
        .unwrap();
    let resource = run_resource(&record, u64::MAX).unwrap();
    let id = resource.id.clone();
    kernel.create(resource).unwrap();
    (kernel, id)
}

fn submission(resource: &Resource, dispatch: &Dispatch) -> Submission {
    Submission::new(&resource.id, resource.version, Action::Write)
        .slot(dispatch.slot())
        .content(serde_json::json!({
            "summary": "module fixture", "output": {}, "model_requests": 1,
            "commit": {"id": dispatch.slot(), "node_id":dispatch.node.id,
                "invocation":dispatch.invocation}
        }))
        .evidence(Evidence::Resource {
            id: dispatch.slot(),
            version: 0,
        })
}

fn complete(kernel: &Kernel<MemoryStore>, id: &str, node: &str) -> Resource {
    let resource = kernel.load(id).unwrap().unwrap();
    let dispatch = GraphValidator::frontier(&resource)
        .unwrap()
        .into_iter()
        .find(|dispatch| dispatch.node.id == node)
        .unwrap();
    let principal = Principal::new(
        "fixture",
        if dispatch.kind == DispatchKind::Work {
            PrincipalKind::Agent
        } else {
            PrincipalKind::Code
        },
    );
    let verdict = kernel
        .submit(&principal, 1, submission(&resource, &dispatch))
        .unwrap();
    assert!(verdict.is_accepted(), "{node}: {verdict:?}");
    kernel.load(id).unwrap().unwrap()
}

#[test]
fn parallel_region_counts_module_activation_once_and_enforces_reentry_ceiling() {
    // Copy of the old Runner case, with both one and two permitted activations.
    for limit in [1, 2] {
        let mut snapshot = graph(&[&["module/left"], &["module/right"]], true);
        snapshot.module_rounds.insert("module".into(), limit);
        let record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        let (kernel, id) = kernel(record);
        let mut resource = kernel.load(&id).unwrap().unwrap();
        for _ in 0..30 {
            if resource.is_settled() {
                break;
            }
            let dispatch = GraphValidator::frontier(&resource).unwrap().remove(0);
            resource = complete(&kernel, &id, &dispatch.node.id);
        }
        assert!(matches!(resource.phase(), Some(Phase::Settled {
            result: Outcome::Failed { detail, .. }
        }) if detail.contains("module activation ceiling")));
        let record = record_of(&resource).unwrap();
        assert_eq!(record.module_activations["module"], u64::from(limit));
        assert!(record.ceased.contains(&format!("module@{limit}")));
        for node in ["module/left", "module/right"] {
            assert_eq!(record.results[node].len(), limit as usize);
            assert_eq!(record.invocations[node], u64::from(limit));
            assert_eq!(record.passes[node], 1);
        }
    }
}

#[test]
fn independent_modules_preview_without_entry_and_reset_only_the_submitted_module() {
    let mut snapshot = graph(&[&["m/a", "m/b"], &["n/a", "n/b"]], false);
    snapshot.module_rounds = BTreeMap::from([("m".into(), 1), ("n".into(), 1)]);
    for node in &mut snapshot.nodes {
        if node.id.contains('/') {
            node.max_rounds = Some(1);
        }
    }
    let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
    record.passes = BTreeMap::from([("m/a".into(), 9), ("m/b".into(), 9), ("n/a".into(), 9)]);
    record.module_activations.insert("m/nested".into(), 9);
    record.module_activations.insert("n/nested".into(), 9);
    let (kernel, id) = kernel(record);
    let initial = complete(&kernel, &id, "start");
    let record = record_of(&initial).unwrap();
    for _ in 0..3 {
        let frontier = GraphValidator::frontier(&initial).unwrap();
        assert_eq!(frontier.len(), 2);
        assert_eq!(
            GraphValidator::frontier_from_record(&record).unwrap(),
            frontier
        );
        assert_eq!(record_of(&initial).unwrap(), record);
        assert_eq!(kernel.load(&id).unwrap().unwrap(), initial);
    }
    let resource = complete(&kernel, &id, "m/a");
    let record = record_of(&resource).unwrap();
    assert_eq!(record.module_activations.get("m"), Some(&1));
    assert!(!record.module_activations.contains_key("n"));
    assert!(!record.module_activations.contains_key("m/nested"));
    assert_eq!(record.module_activations["n/nested"], 9);
    assert_eq!(record.passes["m/a"], 1);
    assert!(!record.passes.contains_key("m/b"));
    assert_eq!(record.passes["n/a"], 9);
    // Finish the fast branch before the other branch submits its first fact.
    complete(&kernel, &id, "m/b");
    complete(&kernel, &id, "n/a");
    complete(&kernel, &id, "n/b");
    complete(&kernel, &id, "collect");
    let resource = complete(&kernel, &id, "after");
    assert_eq!(
        resource.phase(),
        Some(&Phase::Settled {
            result: Outcome::Done
        })
    );
    let record = record_of(&resource).unwrap();
    assert_eq!(
        record.module_activations,
        BTreeMap::from([("m".into(), 1), ("n".into(), 1)])
    );
    for node in ["m/a", "m/b", "n/a", "n/b"] {
        assert_eq!(record.passes[node], 1);
    }
}

#[test]
fn shared_module_counts_once_when_a_slow_branch_enters_after_its_sibling() {
    let mut snapshot = graph(&[&["outside", "m/left"], &["m/right"]], false);
    snapshot.module_rounds.insert("m".into(), 1);
    let (kernel, id) = kernel(GraphRunRecord::create(snapshot, Value::Null).unwrap());
    for node in ["start", "m/right", "outside", "m/left", "collect", "after"] {
        complete(&kernel, &id, node);
    }
    let resource = kernel.load(&id).unwrap().unwrap();
    assert_eq!(
        resource.phase(),
        Some(&Phase::Settled {
            result: Outcome::Done
        })
    );
    assert_eq!(record_of(&resource).unwrap().module_activations["m"], 1);
}

#[test]
fn module_ceiling_settles_once_and_preserves_late_results_and_failures() {
    for late_failure in [false, true] {
        let mut snapshot = graph(&[&["outside", "m/left"], &["other"]], false);
        snapshot.module_rounds.insert("m".into(), 1);
        let mut record = GraphRunRecord::create(snapshot, Value::Null).unwrap();
        record.module_activations.insert("m".into(), 1);
        let (kernel, id) = kernel(record);
        let dispatched = complete(&kernel, &id, "start");
        let other = GraphValidator::frontier(&dispatched)
            .unwrap()
            .into_iter()
            .find(|dispatch| dispatch.node.id == "other")
            .unwrap();
        let settled = complete(&kernel, &id, "outside");
        assert!(matches!(settled.phase(), Some(Phase::Settled {
            result: Outcome::Failed { detail, .. }
        }) if detail == "parallel branch `m/left` exceeded module activation ceiling `m@1`"));
        let mut late = submission(&dispatched, &other);
        let principal = if late_failure {
            late.action = Action::Settle;
            late.content = serde_json::json!({"reason":"late sibling failure"});
            PrincipalKind::Code
        } else {
            PrincipalKind::Agent
        };
        let verdict = super::super::scheduler::submit_with_retry(
            &kernel,
            &Principal::new("fixture", principal),
            3,
            late,
            None,
        )
        .unwrap();
        assert_eq!(verdict.effect, Effect::Refused);
        assert_eq!(verdict.reason, Reason::TaskSettled);
        let after = kernel.load(&id).unwrap().unwrap();
        assert_eq!(after.version, settled.version);
        assert_eq!(after.phase(), settled.phase());
        assert_eq!(after.state, settled.state);
        assert_eq!(
            after.task.as_ref().unwrap().budget.spent,
            settled.task.as_ref().unwrap().budget.spent + 3
        );
        let history = kernel.history(&id).unwrap();
        let late = history.last().unwrap();
        assert_eq!(late.submission.slot.as_deref(), Some("other#1"));
        assert!(!late.submission.evidence.is_empty());
        assert_eq!(
            history
                .iter()
                .filter(|record| record.after.as_ref().is_some_and(Resource::is_settled))
                .count(),
            1
        );
    }
}
