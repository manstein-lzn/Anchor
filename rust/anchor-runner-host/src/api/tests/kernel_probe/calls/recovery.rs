//! Detached child admission survives Host restart without replaying unknown effects.
use super::*;

#[tokio::test]
async fn kernel_restart_dispatches_detach_child_only_with_staged_call_inputs() {
    use anchor_runtime::graph::{
        ArtifactPort, CallFileSelection, GraphRunRecord, GraphSnapshot, InvocationKey,
        NodeCompletion,
    };
    let (root, mut state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child_bundle = root.path().join("child");
    std::fs::create_dir_all(&child_bundle).unwrap();
    let definition = json!({
        "objective":"child","entry":"answer","agents":{},
        "ops":{"answer":{"run":"sh -c \"cat /in/call/input/report.txt\""}},
        "nodes":[{"id":"answer","op":"answer","plugins":[]}],"edges":[]
    });
    std::fs::write(child_bundle.join("graph.json"), definition.to_string()).unwrap();
    std::fs::write(
        child_bundle.join("manifest.json"),
        r#"{"format":1,"graph":"graph.json","plugins":[]}"#,
    )
    .unwrap();

    // The staged read-only `/in/call` bundle is written before the child is
    // durable, exactly as the live Op.call path orders it.
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    let producer = InvocationKey {
        run_id: "parent-run".into(),
        graph_digest: "parent-digest".into(),
        node_id: "produce".into(),
        invocation: 1,
    };
    let producer_workspace = artifacts.workspace_path(&producer).unwrap();
    std::fs::create_dir_all(&producer_workspace).unwrap();
    std::fs::write(producer_workspace.join("report.txt"), "parent-report").unwrap();
    let producer_commit = artifacts
        .freeze(
            &producer,
            &NodeCompletion {
                submission: "done".into(),
                route: None,
                model_requests: 0,
                output: json!({"files":["report.txt"]}),
            },
        )
        .await
        .unwrap();
    let child_id = "detach-restart";
    artifacts
        .stage_call_inputs(
            child_id,
            std::slice::from_ref(&producer_commit),
            &[CallFileSelection {
                node: "produce".into(),
                path: "report.txt".into(),
                alias: "input/report.txt".into(),
            }],
        )
        .await
        .unwrap();

    let snapshot = GraphSnapshot::admit(definition).unwrap();
    let mut record = GraphRunRecord::create_with_id(snapshot, json!({}), child_id).unwrap();
    record.plugin_bindings_initialized = true;
    let runs = state.application.kernel_runs(true).unwrap().unwrap();
    runs.create(&record, u64::MAX).unwrap();
    drop(runs);
    let metadata = RunMetadata::graph_call_child(
        child_id.to_owned(),
        "child".into(),
        record.graph_digest.clone(),
        &child_bundle,
        crate::run_metadata::GraphCallSource {
            parent_run: "missing-parent".into(),
            parent_graph: "parent".into(),
            parent_graph_digest: "parent-digest".into(),
            node: "notify".into(),
            invocation: 1,
            mode: "detach".into(),
            root_run: "missing-parent".into(),
        },
    )
    .unwrap();
    metadata::save(&state.data_root, &metadata).unwrap();

    // Release the original writer and reopen from durable admission facts.
    state.application = RunApplication::new(state.data_root.clone(), root.path().to_path_buf())
        .with_configured_graph("fixture".into(), state.bundle_root.clone());
    state
        .application
        .recover_detached_at_startup()
        .await
        .unwrap();
    for _ in 0..200 {
        if fixtures::Reader::new(&state.application)
            .load(child_id)
            .unwrap()
            .is_some_and(|record| record.has_succeeded())
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let child = fixtures::Reader::new(&state.application)
        .load(child_id)
        .unwrap()
        .unwrap();
    assert!(state.application.kernel_owner(child_id).unwrap().is_some());
    assert!(
        fixtures::Reader::new(&state.application)
            .load(child_id)
            .unwrap()
            .is_some()
    );
    assert!(child.has_succeeded(), "{:?}", child.phase);
    assert_eq!(
        child.results["answer"][0].completion.submission.trim(),
        "parent-report"
    );
}

#[tokio::test]
async fn kernel_restart_parks_started_detach_child_without_replaying_effects() {
    use anchor_kernel::{Evidence, Waiter};
    use anchor_runtime::graph::{CompletionFact, GraphRunRecord, GraphSnapshot, InvocationKey};

    let (root, mut state) = fixture();
    let _env_guard = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let child_bundle = root.path().join("child");
    let definition = json!({
        "objective":"child","entry":"work","agents":{},
        "ops":{"work":{"run":"sh -c \"printf replayed >> executions.txt\""}},
        "nodes":[{"id":"work","op":"work","plugins":[]}],"edges":[]
    });
    write_graph_bundle(&child_bundle, &definition).unwrap();
    let child_id = "detach-started-restart";
    let mut record = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition).unwrap(),
        json!({}),
        child_id,
    )
    .unwrap();
    record.plugin_bindings_initialized = true;
    let key = InvocationKey {
        run_id: child_id.into(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    let workspace = artifacts.workspace_path(&key).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let effect = workspace.join("executions.txt");
    std::fs::write(&effect, "already-executed\n").unwrap();
    let facts_root = state.data_root.join("facts");
    std::fs::create_dir_all(&facts_root).unwrap();
    let stem = format!("nf1-{:x}", Sha256::digest(key.durable_key().as_bytes()));
    std::fs::write(facts_root.join(format!("{stem}.started")), b"started\n").unwrap();
    assert!(matches!(
        crate::load_host_completion_fact(&facts_root, &key).unwrap(),
        CompletionFact::Uncertain(_)
    ));
    let metadata = RunMetadata::graph_call_child(
        child_id.into(),
        "child".into(),
        record.graph_digest.clone(),
        &child_bundle,
        crate::run_metadata::GraphCallSource {
            parent_run: "missing-parent".into(),
            parent_graph: "parent".into(),
            parent_graph_digest: "parent-digest".into(),
            node: "notify".into(),
            invocation: 1,
            mode: "detach".into(),
            root_run: "missing-parent".into(),
        },
    )
    .unwrap();
    metadata::save(&state.data_root, &metadata).unwrap();
    let runs = state.application.kernel_runs(true).unwrap().unwrap();
    runs.create(&record, u64::MAX).unwrap();
    drop(runs);
    state.application = RunApplication::new(state.data_root.clone(), root.path().to_path_buf())
        .with_configured_graph("fixture".into(), state.bundle_root.clone());

    state
        .application
        .recover_detached_at_startup()
        .await
        .unwrap();
    let runs = state.application.kernel_owner(child_id).unwrap().unwrap();
    let resource = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let resource = runs.load(child_id).unwrap().unwrap();
            if matches!(
                resource.phase(),
                Some(Phase::Waiting {
                    waiter: Waiter::Human { .. }
                })
            ) && !runs.active(child_id)
            {
                break resource;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let child = fixtures::Reader::new(&state.application)
        .load(child_id)
        .unwrap()
        .unwrap();
    assert!(child.results.is_empty());
    assert_eq!(
        std::fs::read_to_string(&effect).unwrap(),
        "already-executed\n"
    );
    let history = runs.kernel.history(&resource.id).unwrap();
    let observation = history
        .iter()
        .filter(|report| report.verdict.is_accepted())
        .collect::<Vec<_>>();
    assert_eq!(observation.len(), 1);
    assert_eq!(observation[0].submission.content["control"], "stop");
    assert_eq!(
        observation[0].submission.evidence,
        vec![Evidence::Observation {
            id: format!("node-completion/{}", key.durable_key())
        }]
    );
    state
        .application
        .recover_detached_at_startup()
        .await
        .unwrap();
    assert_eq!(runs.load(child_id).unwrap().unwrap(), resource);
    assert_eq!(runs.kernel.history(&resource.id).unwrap(), history);
    assert_eq!(
        std::fs::read_to_string(&effect).unwrap(),
        "already-executed\n"
    );
    assert!(matches!(
        crate::load_host_completion_fact(&facts_root, &key).unwrap(),
        CompletionFact::Uncertain(_)
    ));
}
