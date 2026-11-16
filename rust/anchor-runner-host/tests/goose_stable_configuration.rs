//! Dynamic task inputs must not fragment the stable harness configuration.
#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_facts::{AgentConfiguration, Fact, Invocation, Observation};
use goose::{Host, Provider, Step};
use serde_json::json;
use std::collections::BTreeSet;

#[test]
#[ignore = "requires pinned Goose binary"]
fn ten_host_inputs_share_one_stable_configuration_and_keep_distinct_request_contexts() {
    let graph = json!({
        "objective":"Complete each supplied fixture input",
        "entry":"worker",
        "agents":{"worker":{"model":"fixture-goose",
            "instructions":"Submit a checked final_result for the supplied input.",
            "wall_time_limit_seconds":40}},
        "ops":{"done":{"run":"true"}},
        "nodes":[{"id":"worker","agent":"worker"},{"id":"done","op":"done"}],
        "edges":[{"from":"worker","to":"done"}]
    });
    let inputs = (0..10)
        .map(|index| json!({"item":index,"message":format!("distinct-input-{index:02}")}))
        .collect::<Vec<_>>();
    let provider = Provider::new(
        "ten-input-stable-configuration",
        inputs
            .iter()
            .map(|input| {
                Step::tool(
                    "final_result",
                    json!({"files":[],"summary":"input complete","route":"done"}),
                )
                .expect_request_contains(input["message"].as_str().unwrap())
            })
            .collect(),
    );
    let host = Host::new(&graph).default_runtime();
    let server = host.serve(&provider);
    let mut runs = Vec::new();
    let mut configuration_digests = BTreeSet::new();
    let mut task_digests = BTreeSet::new();
    let mut input_digests = BTreeSet::new();
    let mut contexts = Vec::new();
    for input in &inputs {
        let (status, accepted) = server.request(
            "POST",
            "/trigger",
            Some(&json!({"graph":"fixture","input":input})),
        );
        assert_eq!(status, 202, "{accepted}");
        let run = accepted["run"].as_str().unwrap().to_owned();
        server.wait_status(&run, "completed");
        let saved = host.record(&run);
        assert_eq!(&saved["input"], input);
        let reader =
            anchor_kernel::HistoryReader::open(host.base.state_root().join("kernel-facts"))
                .unwrap();
        let invocation = Invocation {
            run_id: run.clone(),
            graph_digest: saved["graph_digest"].as_str().unwrap().into(),
            node_id: "worker".into(),
            invocation: 1,
        };
        let mut found = Vec::new();
        for record in reader.history(&invocation.trace()).unwrap() {
            assert!(record.verdict.is_accepted());
            let configuration = record.stamp.config().expect("configured Agent observation");
            configuration_digests.insert(configuration.to_owned());
            let observation: Observation =
                serde_json::from_value(record.submission.content).unwrap();
            assert_eq!(observation.invocation, invocation);
            for fact in observation.facts {
                if let Fact::RequestContext {
                    task_sha256,
                    input_sha256,
                    input_guidance_digest,
                } = fact
                {
                    assert_eq!(
                        input_sha256,
                        anchor_facts::text_digest(
                            &anchor_facts::canonical(&json!({
                                "input":input,"committed_inputs":[]
                            }))
                            .to_string()
                        )
                    );
                    assert!(input_guidance_digest.is_none());
                    task_digests.insert(task_sha256.clone());
                    input_digests.insert(input_sha256.clone());
                    found.push(json!({"run":run,"task_sha256":task_sha256,"input_sha256":input_sha256,"configuration":configuration}));
                }
            }
        }
        assert_eq!(found.len(), 1, "one per-attempt context: {found:?}");
        contexts.extend(found);
        runs.push(run);
    }
    provider.assert_consumed();
    assert_eq!(provider.requests().len(), 10);
    assert_eq!(configuration_digests.len(), 1);
    assert_eq!(task_digests.len(), 10);
    assert_eq!(input_digests.len(), 10);
    let reader =
        anchor_kernel::HistoryReader::open(host.base.state_root().join("kernel-facts")).unwrap();
    let configurations = reader
        .list("config/")
        .unwrap()
        .into_iter()
        .filter_map(|id| {
            let resource = reader.load(&id).unwrap().unwrap();
            serde_json::from_value::<AgentConfiguration>(resource.state)
                .ok()
                .map(|configuration| (id, configuration))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        configurations.len(),
        1,
        "envelopes are not Agent configurations"
    );
    let (id, configuration) = &configurations[0];
    assert!(configuration.is_stable());
    assert_eq!(id, &anchor_facts::configuration_id(&configuration.digest()));
    assert_eq!(
        configuration_digests.first().unwrap(),
        &configuration.digest()
    );
    host.evidence(
        &provider,
        &runs[0],
        json!({
            "case_source":"tests/goose_stable_configuration.rs",
            "runs":runs,"contexts":contexts,"stable_configuration":configuration,
            "distinct_inputs":10,"distinct_request_contexts":10,"stable_configuration_count":1,
            "host_runner_goose_acp":true,"external_provider_requests":0
        }),
    );
}
