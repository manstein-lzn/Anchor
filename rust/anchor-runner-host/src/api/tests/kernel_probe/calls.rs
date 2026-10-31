//! Existing Op.call API acceptance copied onto explicit Kernel admission.
use super::*;

mod recovery;

#[tokio::test]
async fn kernel_op_call_wait_projects_durable_child_run_through_host_api() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(
        child.join("graph.json"),
        r#"{"objective":"child objective","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}"#,
    ).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({
        "objective":"parent",
        "entry":"call",
        "agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"wait","input":{"from":"parent"}}}},
        "nodes":[{"id":"call","op":"call","plugins":[]}],
        "edges":[]
    });
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let mut child_id = String::new();
    for _ in 0..200 {
        let parent_record = fixtures::Reader::new(&state.application)
            .load(&parent_id)
            .unwrap()
            .unwrap();
        if let Some(call) = parent_record.graph_calls.values().next() {
            child_id = call.child_run_id.clone().unwrap_or_default();
        }
        if !child_id.is_empty()
            && fixtures::Reader::new(&state.application)
                .load(&child_id)
                .unwrap()
                .is_some_and(|record| record.has_succeeded())
            && parent_record.has_succeeded()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        !child_id.is_empty(),
        "parent did not admit child: {:?}",
        fixtures::Reader::new(&state.application)
            .load(&parent_id)
            .unwrap()
            .unwrap()
    );
    let store = fixtures::Reader::new(&state.application);
    let child_record = store.load(&child_id).unwrap().unwrap();
    assert!(child_record.has_succeeded());
    assert!(state.application.kernel_owner(&child_id).unwrap().is_some());
    assert!(
        fixtures::Reader::new(&state.application)
            .load(&child_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(child_record.input["from"], "parent");
    let parent_metadata = state.application.metadata(&parent_id).unwrap().unwrap();
    assert_eq!(parent_metadata.oauth_owner.as_deref(), Some("local"));
    let metadata = state.application.metadata(&child_id).unwrap().unwrap();
    assert_eq!(metadata.graph, "child");
    assert_eq!(metadata.trigger_source, "graph_call");
    assert_eq!(metadata.oauth_owner.as_deref(), Some("local"));
    let (status, detail) = call(app.clone(), "GET", &format!("/runs/{child_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["graph"], "child");
    let (status, listing) = call(app, "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        listing["runs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|run| run["run"] == child_id && run["graph"] == "child")
    );
}

#[tokio::test]
async fn kernel_op_call_wait_hands_off_input_map_files_and_selected_result() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(
        child.join("graph.json"),
        serde_json::to_vec(&json!({
            "objective":"child",
            "entry":"answer",
            "input":{"default":"child-default"},
            "agents":{},
            "ops":{"answer":{"run":"sh -c \"cat /in/call/input/report.txt > answer.md; cat answer.md\"","writes":["answer.md"]}},
            "nodes":[{"id":"answer","op":"answer","plugins":[]}],
            "edges":[]
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({
        "objective":"parent",
        "entry":"produce",
        "input":{"topic":"default-topic"},
        "agents":{},
        "ops":{
            "produce":{"run":"sh -c \"printf 'parent-report\\n' > report.md\"","writes":["report.md"]},
            "notify":{"call":{"graph":"child","mode":"wait",
                "input":{"constant":"yes"},
                "input_map":{"topic":"/topic"},
                "files":[{"node":"produce","path":"report.md","as":"input/report.txt"}],
                "result":{"node":"answer","files":["answer.md"]}}},
            "after":{"run":"sh -c \"cat /in/notify/result/answer.md\""}
        },
        "nodes":[
            {"id":"produce","op":"produce","plugins":[]},
            {"id":"notify","op":"notify","plugins":[]},
            {"id":"after","op":"after","plugins":[]}
        ],
        "edges":[{"from":"produce","to":"notify"},{"from":"notify","to":"after"}]
    });
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let store = fixtures::Reader::new(&state.application);
    let mut child_id = String::new();
    for _ in 0..300 {
        let record = store.load(&parent_id).unwrap().unwrap();
        if let Some(id) = record
            .graph_calls
            .values()
            .next()
            .and_then(|call| call.child_run_id.clone())
        {
            child_id = id;
        }
        if record.is_settled() || record.has_stopped_execution() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let parent_record = store.load(&parent_id).unwrap().unwrap();
    assert!(parent_record.has_succeeded(), "{:?}", parent_record.phase);
    assert!(!child_id.is_empty(), "parent did not admit a child Run");
    let child_record = store.load(&child_id).unwrap().unwrap();
    assert!(child_record.has_succeeded(), "{:?}", child_record.phase);
    // `input` constants override and `input_map` pointers select the parent input,
    // all merged under the child's own declared input.
    assert_eq!(child_record.input["constant"], "yes");
    assert_eq!(child_record.input["topic"], "default-topic");
    assert_eq!(child_record.input["default"], "child-default");
    // The child read the selected parent commit through the read-only /in/call mount.
    assert_eq!(
        child_record.results["answer"][0]
            .completion
            .submission
            .trim(),
        "parent-report"
    );
    // The wait call node returns the explicitly selected result descriptor.
    let notify = &parent_record.results["notify"][0].completion.output;
    assert_eq!(notify["summary"], "parent-report");
    assert_eq!(notify["result"]["node"], "answer");
    assert_eq!(notify["result"]["files"], json!(["answer.md"]));
    // The downstream node reads the copied result file from the call node commit.
    assert_eq!(
        parent_record.results["after"][0]
            .completion
            .submission
            .trim(),
        "parent-report"
    );
}

#[tokio::test]
async fn kernel_op_call_wait_fails_closed_on_missing_input_map_pointer() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(
        child.join("graph.json"),
        r#"{"objective":"child","entry":"answer","agents":{},"ops":{"answer":{"run":"true"}},"nodes":[{"id":"answer","op":"answer","plugins":[]}],"edges":[]}"#,
    )
    .unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({
        "objective":"parent",
        "entry":"notify",
        "input":{"topic":"default-topic"},
        "agents":{},
        "ops":{"notify":{"call":{"graph":"child","mode":"wait","input_map":{"missing":"/nope"}}}},
        "nodes":[{"id":"notify","op":"notify","plugins":[]}],
        "edges":[]
    });
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let store = fixtures::Reader::new(&state.application);
    for _ in 0..300 {
        let record = store.load(&parent_id).unwrap().unwrap();
        if record.is_settled() || record.has_stopped_execution() {
            let error = observed_error(&record).unwrap_or_default().to_owned();
            assert!(is_failed(&record), "{error}");
            assert!(error.contains("input_map"), "{error}");
            assert!(
                record.graph_calls.is_empty(),
                "a rejected call must not admit a child"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("parent Run did not settle");
}

#[tokio::test]
async fn kernel_op_call_detach_dispatches_child_and_projects_parent_relation() {
    let (root, state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child = root.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(child.join("graph.json"), r#"{"objective":"child","entry":"work","agents":{},"ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]}"#).unwrap();
    std::fs::write(
        child.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();
    let parent = json!({
        "objective":"parent", "entry":"call", "agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"detach"}}},
        "nodes":[{"id":"call","op":"call","plugins":[]}], "edges":[]
    });
    write_graph_bundle(&state.bundle_root, &parent).unwrap();
    let app = router(state.clone());
    let (status, started) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let parent_id = started["run"].as_str().unwrap().to_owned();
    let mut child_id = None;
    for _ in 0..200 {
        let record = fixtures::Reader::new(&state.application)
            .load(&parent_id)
            .unwrap()
            .unwrap();
        if record.has_succeeded() {
            child_id = record
                .graph_calls
                .values()
                .next()
                .and_then(|call| call.child_run_id.clone());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child_id = child_id.expect("detach should durably admit a child");
    for _ in 0..200 {
        if fixtures::Reader::new(&state.application)
            .load(&child_id)
            .unwrap()
            .is_some_and(|child| child.has_succeeded())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child_record = fixtures::Reader::new(&state.application)
        .load(&child_id)
        .unwrap()
        .unwrap();
    assert!(child_record.has_succeeded());
    assert!(state.application.kernel_owner(&child_id).unwrap().is_some());
    assert!(
        fixtures::Reader::new(&state.application)
            .load(&child_id)
            .unwrap()
            .is_some()
    );
    let metadata = state.application.metadata(&child_id).unwrap().unwrap();
    assert_eq!(metadata.trigger_source, "graph_call");
    let (status, detail) = call(app, "GET", &format!("/runs/{child_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["graph"], "child");
    assert_eq!(detail["state"]["trigger"]["mode"], "detach");
    assert_eq!(detail["state"]["trigger"]["run"], parent_id);
    let parent_detail_app = router(state.clone());
    let (status, parent_detail) = call(
        parent_detail_app,
        "GET",
        &format!("/runs/{parent_id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parent_detail["calls"][0]["run"], child_id);
    assert_eq!(parent_detail["calls"][0]["mode"], "detach");
}

#[tokio::test]
async fn kernel_call_rechecks_a_child_that_settled_before_parent_worker_release() {
    use anchor_graph_host::GraphCatalog;
    use anchor_kernel::{Action, Principal, PrincipalKind, Submission};
    use anchor_runtime::graph::{validator::*, *};
    use std::{future::Future, pin::Pin, sync::atomic::AtomicBool};
    struct WaitingCalls {
        child: String,
        started: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
    }
    impl GraphCallPort for WaitingCalls {
        fn call<'a>(
            &'a self,
            _: &'a CallIdentity,
            _: &'a Value,
            _: &'a Value,
            _: &'a [CommitRef],
            _: anchor_runtime::Cancellation,
        ) -> Pin<Box<dyn Future<Output = Result<GraphCallOutcome, GraphError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.started.notify_one();
                self.release.acquire().await.unwrap().forget();
                Ok(GraphCallOutcome::Waiting {
                    child_run_id: self.child.clone(),
                })
            })
        }
    }
    impl NodeExecutionPort for WaitingCalls {
        fn capabilities(&self) -> NodeExecutionCapabilities {
            NodeExecutionCapabilities::default()
        }
        fn graph_call_port(&self) -> Option<&dyn GraphCallPort> {
            Some(self)
        }
        fn completion_fact<'a>(
            &'a self,
            _: &'a InvocationKey,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
            Box::pin(async { panic!("call must use its port") })
        }
        fn execute<'a>(
            &'a self,
            _: NodeExecutionRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>
        {
            Box::pin(async { panic!("call must use its port") })
        }
    }
    for outcome in ["done", "failed", "unknown"] {
        let (root, state) = fixture();
        let _env_guard = PROCESS_ENV.lock().await;
        set_host_env(root.path());
        let parent_definition = json!({"objective":"parent","entry":"call","agents":{},
        "ops":{"call":{"call":{"graph":"child","mode":"wait"}}},
        "nodes":[{"id":"call","op":"call"}],"edges":[]});
        write_graph_bundle(&state.bundle_root, &parent_definition).unwrap();
        let child_definition = json!({"objective":"child","entry":"work","agents":{},
        "ops":{"work":{"call":{"graph":"unstarted-child","mode":"wait"}}},"nodes":[{"id":"work","op":"work"}],"edges":[]});
        let child_path = root.path().join("child");
        write_graph_bundle(&child_path, &child_definition).unwrap();
        let parent = GraphRunRecord::create_with_id(
            serde_json::from_value(parent_definition).unwrap(),
            json!({}),
            "call-race-parent",
        )
        .unwrap();
        let metadata = crate::run_metadata::RunMetadata::new(
            parent.run_id.clone(),
            "fixture".into(),
            parent.graph_digest.clone(),
            &state.bundle_root,
        )
        .unwrap();
        crate::run_metadata::save(&state.data_root, &metadata).unwrap();
        let runs = state.application.kernel_runs(true).unwrap().unwrap();
        runs.create(&parent, u64::MAX).unwrap();
        let resource = runs.load(&parent.run_id).unwrap().unwrap();
        let dispatch = GraphValidator::frontier(&resource).unwrap().remove(0);
        let identity = call_identity(&parent, &dispatch).unwrap();
        let child_id = anchor_graph_host::child_run_id(&identity);
        let child = GraphRunRecord::create_with_id(
            serde_json::from_value(child_definition).unwrap(),
            json!({}),
            &child_id,
        )
        .unwrap();
        let catalog = crate::execution::HostGraphCatalog {
            root: root.path().into(),
            state: state.data_root.clone(),
            application: Some(state.application.clone()),
        };
        catalog
            .record_child_admission(&child_id, "child", &child.snapshot, &[], &identity, "wait")
            .unwrap();
        runs.create(&child, u64::MAX).unwrap();
        let host = Principal::new(scheduler::HOST_ID, PrincipalKind::Code);
        assert!(
            runs.kernel
                .submit(
                    &host,
                    0,
                    Submission::new(&resource.id, resource.version, Action::Write)
                        .slot(dispatch.slot())
                        .content(
                            serde_json::to_value(CallSubmission::Bound {
                                child_run_id: child_id.clone()
                            })
                            .unwrap()
                        )
                )
                .unwrap()
                .is_accepted()
        );
        let calls = Arc::new(WaitingCalls {
            child: child_id.clone(),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        });
        let worker = {
            let runs = runs.clone();
            let calls = calls.clone();
            let parent_id = parent.run_id.clone();
            tokio::spawn(async move {
                runs.drive(
                    &parent_id,
                    &anchor_runtime::graph::test_support::Artifacts,
                    calls.as_ref(),
                    Arc::new(AtomicBool::new(false)),
                )
                .await
                .unwrap()
            })
        };
        calls.started.notified().await;
        let resource = runs.load(&child_id).unwrap().unwrap();
        let dispatch = GraphValidator::frontier(&resource).unwrap().remove(0);
        let completion = match outcome {
            "done" => CallSubmission::Completed {
                child_run_id: "grandchild".into(),
                output: json!({"proof":"settled before release"}),
                commit: CommitRef {
                    id: "child-commit".into(),
                    node_id: "work".into(),
                    invocation: 1,
                },
            },
            "failed" => CallSubmission::Failed {
                child_run_id: Some("grandchild".into()),
                reason: "known child failure".into(),
            },
            "unknown" => CallSubmission::Uncertain {
                child_run_id: Some("grandchild".into()),
                reason: "child admission unknown".into(),
            },
            _ => unreachable!(),
        };
        assert!(
            runs.kernel
                .submit(
                    &host,
                    0,
                    Submission::new(&resource.id, resource.version, Action::Write)
                        .slot(dispatch.slot())
                        .content(serde_json::to_value(completion).unwrap())
                )
                .unwrap()
                .is_accepted()
        );
        // The first notification arrives while the parent is still active and
        // therefore cannot schedule a duplicate parent worker.
        state
            .application
            .kernel_child_finished(&child_id)
            .await
            .unwrap();
        assert!(runs.active(&parent.run_id));
        calls.release.add_permits(1);
        let waiting = worker.await.unwrap();
        assert!(matches!(waiting.phase(), Some(Phase::Waiting { .. })));
        assert!(!runs.active(&parent.run_id));
        state
            .application
            .kernel_recheck_child(&parent.run_id)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !runs.load(&parent.run_id).unwrap().unwrap().is_settled()
                || runs.active(&parent.run_id)
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let parent = fixtures::Reader::new(&state.application)
            .load(&parent.run_id)
            .unwrap()
            .unwrap();
        let resource = runs.load(&parent.run_id).unwrap().unwrap();
        match outcome {
            "done" => {
                assert!(parent.has_succeeded());
                assert_eq!(
                    parent.results["call"][0].completion.output["proof"],
                    "settled before release"
                );
            }
            "failed" => assert!(matches!(
                resource.phase(),
                Some(Phase::Settled {
                    result: Outcome::Failed { .. }
                })
            )),
            "unknown" => assert!(matches!(
                resource.phase(),
                Some(Phase::Settled {
                    result: Outcome::Unknown { .. }
                })
            )),
            _ => unreachable!(),
        }
        let history = runs
            .kernel
            .history(&run_resource_id(&parent.run_id))
            .unwrap();
        let releases = history
            .iter()
            .filter(|report| {
                report.verdict.is_accepted() && report.submission.content["control"] == "resume"
            })
            .collect::<Vec<_>>();
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].stamp.principal().kind, PrincipalKind::Code);
    }
}
