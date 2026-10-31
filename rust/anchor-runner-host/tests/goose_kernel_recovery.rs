#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Host, Provider, command, complete, wait_until};
use serde_json::json;

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn kernel_observed_native_effect_accepts_durable_completed_or_abort_without_replay() {
    for abort in [false, true] {
        let graph = json!({"objective":"Inspect an interrupted native tool effect", "entry":"worker",
            "agents":{"worker":{"model":"models.worker","instructions":"Inspect existing workspace facts before continuing. Never repeat an unknown effect. Finish with final_result route verify after a fresh tool observation."}},
            "ops":{"verify":{"writes":["verified.txt"],"run":"sh -c 'test \"$(cat /in/worker/effects.txt)\" = once; cat /in/worker/effects.txt > verified.txt'"}},
            "nodes":[{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"}], "edges":[{"from":"worker","to":"verify"}]});
        let provider = Provider::new(
            if abort {
                "kernel-native-effect-abort"
            } else {
                "kernel-native-effect-completed"
            },
            vec![command(
                "set -eu; test ! -e effects.txt; printf once >> effects.txt; sleep 3600",
            )],
        );
        let host = Host::new(&graph).default_runtime();
        let mut server = host.serve(&provider);
        let (status, accepted) =
            server.request("POST", "/trigger", Some(&json!({"graph":"fixture"})));
        assert_eq!(status, 202, "{accepted}");
        let run = accepted["run"].as_str().unwrap();
        wait_until("tool effect exists before its result", || {
            !host.base.workspace_files(run, "effects.txt").is_empty()
        });
        let before = host.native_fact(run, "worker", 1);
        assert!(before["completion"].is_null());
        assert_eq!(before["tool_observation"]["tool"], "anchor_run");
        assert!(before["tool_observation"]["result"].is_null());
        server.kill();
        drop(server);
        // The scheduler may already have observed this attempt before the
        // operator arrives. Prepare exactly that Code observation, rather than
        // using an explicit resume (which authorizes Agent inspection).
        host.observe_native_recovery(run, "worker", 1);
        let restarted = host.serve(&provider);
        let mut waiting = serde_json::Value::Null;
        wait_until(
            "Kernel observes unresolved native effect without contacting the model",
            || {
                let (status, detail) = restarted.request("GET", &format!("/runs/{run}"), None);
                assert_eq!(status, 200, "{detail}");
                waiting = detail;
                waiting["projection"]["phase"] == "waiting"
                    && waiting["active"] == false
                    && waiting["state"]["recovery"]
                        .as_array()
                        .is_some_and(|a| a.len() == 1)
            },
        );
        assert_eq!(provider.requests().len(), 1);
        assert_eq!(waiting["projection"]["actions"]["resume"], true);
        let attempt = waiting["state"]["recovery"][0]["attempt"]["attempt_id"]
            .as_i64()
            .unwrap();
        let mut decision = json!({"node_id":"worker","invocation":1,"attempt_id":attempt,"decision":if abort { "abort" } else { "completed" }});
        if !abort {
            decision["observation"] = json!(
                "The operator inspected effects.txt and verified exactly one external effect; continue by reading it."
            );
            provider.append(vec![
                command("set -eu; test \"$(cat effects.txt)\" = once; printf inspected; cat effects.txt").expect_request_contains("The operator inspected effects.txt"),
                complete("verify").files(&["effects.txt"]).after("inspectedonce"),
            ]);
        }
        let (status, response) =
            restarted.request("POST", &format!("/runs/{run}/recovery"), Some(&decision));
        assert_eq!(status, 202, "{response}");
        if abort {
            wait_until("abandonment settles the Kernel Run", || {
                let (status, detail) = restarted.request("GET", &format!("/runs/{run}"), None);
                assert_eq!(status, 200, "{detail}");
                detail["projection"]["outcome"] == "aborted"
            });
            assert_eq!(provider.requests().len(), 1);
        } else {
            restarted.wait_status(run, "completed");
            provider.assert_consumed();
            assert_eq!(
                host.native_fact(run, "worker", 1)["session_id"],
                before["session_id"]
            );
        }
        assert_eq!(
            host.base.workspace_files(run, "effects.txt"),
            vec![b"once".to_vec()]
        );
        let (status, response) =
            restarted.request("POST", &format!("/runs/{run}/recovery"), Some(&decision));
        assert_eq!(status, 202, "{response}");
        let mut changed = json!({"node_id":"worker","invocation":1,"attempt_id":attempt,"decision":if abort { "completed" } else { "abort" }});
        if abort {
            changed["observation"] = json!("a conflicting decision");
        }
        let (status, _) =
            restarted.request("POST", &format!("/runs/{run}/recovery"), Some(&changed));
        assert_eq!(status, 409);
        assert!(
            !host
                .base
                .root
                .path()
                .join(format!("state/runs/{run}.json"))
                .exists()
        );
        host.evidence(&provider, run, json!({"case_source":"tests/goose_kernel_recovery.rs","aborted":abort,"pending_attempt":attempt,"single_effect":true,"human_decision_before_release":true,"no_automatic_tool_replay":true}));
    }
}
