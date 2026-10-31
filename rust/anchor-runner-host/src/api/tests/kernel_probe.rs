use super::*;

mod calls;
use anchor_kernel::{Outcome, Phase};

#[tokio::test]
async fn parallel_call_is_rejected_by_shared_admission_without_persisting_parent_or_child() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let definition = json!({"objective":"parallel call admission", "entry":"fan", "agents":{},
        "ops":{"fan":{"fanout":{"join":"join"}},"call":{"call":{"graph":"child","mode":"wait"}},"work":{"run":"true"},"join":{"join":{}}},
        "nodes":[{"id":"fan","op":"fan"},{"id":"nested-call","op":"call"},{"id":"work","op":"work"},{"id":"join","op":"join"}],
        "edges":[{"from":"fan","to":"nested-call"},{"from":"fan","to":"work"},{"from":"nested-call","to":"join"},{"from":"work","to":"join"}]});
    std::fs::write(state.bundle_root.join("graph.json"), definition.to_string()).unwrap();
    let router = router(state.clone());
    for body in [json!({"graph":"fixture"}), json!({"graph":"fixture"})] {
        let (status, response) =
            call(router.clone(), "POST", "/trigger", Some(&body.to_string())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{response}");
        assert!(
            response
                .to_string()
                .contains("Op.call node `nested-call` is unsupported inside a fanout/join region"),
            "{response}"
        );
        assert!(state.application.records().unwrap().is_empty());
        assert!(!state.data_root.join("run-metadata").exists());
        assert!(!state.data_root.join("artifacts").exists());
    }
}

#[tokio::test]
async fn explicit_kernel_probe_runs_real_host_ops_and_projects_existing_queries() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let definition = json!({"objective":"kernel probe","entry":"a","agents":{},
        "ops":{"work":{"run":"sh -c 'printf kernel-artifact > report.txt'","writes":["report.txt"]}},
        "nodes":[{"id":"a","op":"work"},{"id":"b","op":"work"},{"id":"c","op":"work"}],
        "edges":[{"from":"a","to":"b"},{"from":"b","to":"c"}]});
    std::fs::write(state.bundle_root.join("graph.json"), definition.to_string()).unwrap();
    let router = router(state.clone());
    let (status, body) = call(
        router.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture","input":{}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let id = body["run"].as_str().unwrap();
    let runs = state.application.kernel_owner(id).unwrap().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(15), async {
        loop {
            if runs.load(id).unwrap().unwrap().is_settled() && !runs.active(id) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        runs.load(id).unwrap().unwrap().phase(),
        Some(&Phase::Settled {
            result: Outcome::Done
        })
    );
    assert!(
        fixtures::Reader::new(&state.application)
            .load(id)
            .unwrap()
            .is_some()
    );
    let (status, detail) = call(router.clone(), "GET", &format!("/runs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(detail["projection"]["phase"], "settled");
    assert_eq!(detail["projection"]["outcome"], "done");
    for node in ["a", "b", "c"] {
        assert_eq!(detail["state"]["nodes"][node]["files"][0], "report.txt");
    }
    let (status, list) = call(router.clone(), "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["runs"].as_array().unwrap().len(), 1);
    assert_eq!(list["runs"][0]["projection"]["outcome"], "done");
    let (status, late) = call(router, "POST", &format!("/runs/{id}/stop"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{late}");
}

#[tokio::test]
async fn kernel_read_view_http_tracks_running_human_wait_and_settlement_with_host_time() {
    use anchor_runtime::graph::*;
    use std::{future::Future, pin::Pin};
    struct Gate {
        started: tokio::sync::Notify,
        release: tokio::sync::Semaphore,
    }
    impl NodeExecutionPort for Gate {
        fn capabilities(&self) -> NodeExecutionCapabilities {
            NodeExecutionCapabilities {
                agent: true,
                exact_provider_request_budget: true,
                ..Default::default()
            }
        }
        fn completion_fact<'a>(
            &'a self,
            _: &'a InvocationKey,
        ) -> Pin<Box<dyn Future<Output = Result<CompletionFact, GraphError>> + Send + 'a>> {
            Box::pin(async { Ok(CompletionFact::NotStarted) })
        }
        fn execute<'a>(
            &'a self,
            _: NodeExecutionRequest,
        ) -> Pin<Box<dyn Future<Output = Result<NodeExecutionOutcome, GraphError>> + Send + 'a>>
        {
            Box::pin(async move {
                self.started.notify_one();
                self.release.acquire().await.unwrap().forget();
                Ok(NodeExecutionOutcome::Completed(NodeCompletion {
                    submission: "late".into(),
                    route: None,
                    output: json!({"files":[]}),
                    model_requests: 1,
                }))
            })
        }
    }
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let id = "r-http-view";
    let record=GraphRunRecord::create_with_id(serde_json::from_value(json!({"objective":"read","entry":"work","agents":{"role":{"model":"fixture","instructions":"work"}},"nodes":[{"id":"work","agent":"role"}],"edges":[]})).unwrap(),json!({}),id).unwrap();
    let metadata = crate::run_metadata::RunMetadata::new(
        id.into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root.join("graph.json"),
    )
    .unwrap();
    crate::run_metadata::save(&state.data_root, &metadata).unwrap();
    let runs = state.application.kernel_runs(true).unwrap().unwrap();
    runs.create(&record, 50).unwrap();
    let gate = Arc::new(Gate {
        started: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    let worker = {
        let runs = runs.clone();
        let gate = gate.clone();
        tokio::spawn(async move {
            runs.drive(
                id,
                &anchor_runtime::graph::test_support::Artifacts,
                gate.as_ref(),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .await
            .unwrap()
        })
    };
    tokio::time::timeout(std::time::Duration::from_secs(10), gate.started.notified())
        .await
        .unwrap();
    let router = router(state.clone());
    let observation = state
        .data_root
        .join("kernel-run-observations")
        .join(format!("{id}.json"));
    let sentinel = "2001-02-03T04:05:06Z";
    std::fs::write(&observation, json!({"updated":sentinel}).to_string()).unwrap();
    let (status, running) = call(router.clone(), "GET", &format!("/runs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "{running}");
    assert_eq!(running["projection"]["phase"], "executing");
    assert_eq!(
        running["projection"]["actions"],
        json!({"pause":true,"stop":true,"resume":false,"abandon":true})
    );
    assert_eq!(running["state"]["updated"], sentinel);
    let (status, body) = call(router.clone(), "POST", &format!("/runs/{id}/stop"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (_, waiting) = call(router.clone(), "GET", &format!("/runs/{id}"), None).await;
    assert_eq!(waiting["projection"]["phase"], "waiting");
    assert_eq!(
        waiting["projection"]["actions"],
        json!({"pause":false,"stop":false,"resume":false,"abandon":true})
    );
    let updated: Value = serde_json::from_slice(&std::fs::read(&observation).unwrap()).unwrap();
    assert_ne!(updated["updated"], sentinel);
    assert_eq!(waiting["state"]["updated"], updated["updated"]);
    gate.release.add_permits(1);
    worker.await.unwrap();
    let (_, waiting) = call(router.clone(), "GET", &format!("/runs/{id}"), None).await;
    assert_eq!(waiting["projection"]["actions"]["resume"], true);
    assert_eq!(
        waiting["state"]["updated"], updated["updated"],
        "refused late results must not stamp an accepted-write observation"
    );
    let (status, body) = call(
        router.clone(),
        "POST",
        &format!("/runs/{id}/abandon"),
        Some(r#"{"reason":"operator"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, settled) = call(router.clone(), "GET", &format!("/runs/{id}"), None).await;
    assert_eq!(settled["projection"]["phase"], "settled");
    assert_eq!(settled["projection"]["outcome"], "aborted");
    assert_eq!(
        settled["projection"]["actions"],
        json!({"pause":false,"stop":false,"resume":false,"abandon":false})
    );
    let before = std::fs::read(&observation).unwrap();
    let (status, _) = call(router, "POST", &format!("/runs/{id}/stop"), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(std::fs::read(&observation).unwrap(), before);
}

#[tokio::test]
async fn legacy_and_kernel_http_share_executing_paused_and_settled_projections() {
    use anchor_runtime::graph::InvocationKey;
    use std::time::Duration;

    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,sleep,printf");
    }
    write_graph_bundle(&state.bundle_root, &json!({
        "objective":"HTTP lifecycle comparison","entry":"work","agents":{},
        "ops":{
            "work":{"run":"printf ready > gate.ready; while [ ! -f gate.release ]; do sleep 0.02; done; printf once > report.txt","writes":["report.txt"],"wall_time_limit_seconds":15},
            "finish":{"run":"true"}
        },
        "nodes":[{"id":"work","op":"work"},{"id":"finish","op":"finish"}],
        "edges":[{"from":"work","to":"finish"}]
    })).unwrap();
    let app = router(state.clone());
    let mut projections = Vec::new();
    for _attempt in 0..2 {
        let request = json!({"graph":"fixture","input":{}});
        let (status, accepted) =
            call(app.clone(), "POST", "/trigger", Some(&request.to_string())).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        let id = accepted["run"].as_str().unwrap();
        let record = fixtures::Reader::new(&state.application)
            .load(id)
            .unwrap()
            .unwrap();
        let artifacts = HostArtifacts::new(
            state.data_root.join("artifacts"),
            state.workspace_root.clone(),
        );
        let workspace = artifacts
            .workspace_path(&InvocationKey {
                run_id: id.into(),
                graph_digest: record.graph_digest.clone(),
                node_id: "work".into(),
                invocation: 1,
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !workspace.join("gate.ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("real Op must reach the same gate on both backends");
        let (status, executing) = call(app.clone(), "GET", &format!("/runs/{id}"), None).await;
        assert_eq!(status, StatusCode::OK, "{executing}");
        let (status, receipt) = call(app.clone(), "POST", &format!("/runs/{id}/pause"), None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{receipt}");
        assert_eq!(receipt["operation"], "pause");
        assert!(receipt.get("alias").is_none());
        let (_, pending) = call(app.clone(), "GET", &format!("/runs/{id}"), None).await;
        assert_eq!(pending["projection"]["phase"], "executing");
        assert_eq!(pending["projection"]["active"], true);
        std::fs::write(workspace.join("gate.release"), "complete current node").unwrap();
        let paused = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (status, detail) = call(app.clone(), "GET", &format!("/runs/{id}"), None).await;
                assert_eq!(status, StatusCode::OK, "{detail}");
                if detail["projection"]["phase"] == "waiting"
                    && detail["projection"]["active"] == false
                {
                    break detail;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pause must drain the current node before waiting");
        let paused_record = fixtures::Reader::new(&state.application)
            .load(id)
            .unwrap()
            .unwrap();
        assert_eq!(paused_record.results.len(), 1);
        assert_eq!(paused_record.results["work"].len(), 1);
        let first = &paused_record.results["work"][0];
        assert_eq!(
            std::fs::read(
                artifacts
                    .files_path(&first.commit)
                    .unwrap()
                    .join("report.txt")
            )
            .unwrap(),
            b"once"
        );
        let (status, receipt) =
            call(app.clone(), "POST", &format!("/runs/{id}/resume"), None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{receipt}");
        let settled = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (status, detail) = call(app.clone(), "GET", &format!("/runs/{id}"), None).await;
                assert_eq!(status, StatusCode::OK, "{detail}");
                if detail["projection"]["phase"] == "settled"
                    && detail["projection"]["active"] == false
                {
                    break detail;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("resume must finish the same Run");
        let finished = fixtures::Reader::new(&state.application)
            .load(id)
            .unwrap()
            .unwrap();
        assert_eq!(finished.results.len(), 2);
        assert_eq!(finished.results["work"].len(), 1);
        assert_eq!(finished.results["work"][0].commit, first.commit);
        let mut stages = Vec::new();
        for detail in [executing, paused, settled] {
            assert!(detail["state"].get("status").is_none());
            let mut projection = detail["projection"].clone();
            assert!(projection["updated"].is_string());
            projection.as_object_mut().unwrap().remove("updated");
            stages.push(projection);
        }
        assert_eq!(
            stages,
            vec![
                json!({"phase":"executing","reason":null,"active":true,"actions":{"pause":true,"stop":true,"resume":false,"abandon":true}}),
                json!({"phase":"waiting","waiting_for":"human","reason":"paused","active":false,"actions":{"pause":false,"stop":false,"resume":true,"abandon":true}}),
                json!({"phase":"settled","outcome":"done","reason":null,"active":false,"actions":{"pause":false,"stop":false,"resume":false,"abandon":false}}),
            ]
        );
        projections.push(stages);
    }
    assert_eq!(projections[0], projections[1]);
}
