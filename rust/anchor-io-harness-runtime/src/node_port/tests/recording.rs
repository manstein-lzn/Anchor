use super::*;

fn directory(port: &IoHarnessNodePort<FixtureResolver>, request: &NodeExecutionRequest) -> PathBuf {
    crate::recording::directory(&port.io_store_root, &request.key)
}

fn read(path: impl AsRef<Path>) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn production_node_port_records_correction_requests_and_completed_reopen_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let resolver = Arc::new(FixtureResolver {
        workspace: workspace.clone(),
        calls: Arc::new(AtomicUsize::new(0)),
        bindings: vec![],
    });
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call("business", "anchor_echo", json!({"value":"full result"})),
        MockTurn::tool_call(
            "invalid",
            "final_result",
            json!({"summary":"invalid route","route":"unknown"}),
        ),
        MockTurn::tool_call(
            "corrected",
            "final_result",
            json!({"summary":"finished","route":"next"}),
        ),
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver.clone(),
    );
    let request = request(
        &workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    assert!(matches!(
        port.execute(request.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let recording_root = directory(&port, &request);
    assert_eq!(std::fs::read_dir(&recording_root).unwrap().count(), 3);
    let first_path = recording_root.join("00000000000000000001/recording.json");
    let original = std::fs::read(&first_path).unwrap();
    let third = read(recording_root.join("00000000000000000003/recording.json"));
    assert!(
        third["exchanges"][0]["request"]
            .to_string()
            .contains("output shape")
    );
    assert!(
        third["exchanges"][0]["request"]
            .to_string()
            .contains("full result")
    );
    assert_eq!(
        third["exchanges"][0]["response"]["tool_calls"][0]["name"],
        "final_result"
    );
    let fresh_model = MockCompletionModel::from_turns([]);
    let reopened = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        fresh_model.clone().erase(),
        resolver,
    );
    assert!(matches!(
        reopened.execute(request).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert_eq!(fresh_model.request_count(), 0);
    assert_eq!(std::fs::read(first_path).unwrap(), original);
    assert_eq!(std::fs::read_dir(recording_root).unwrap().count(), 3);
}

#[tokio::test]
async fn recorded_provider_error_does_not_enable_tool_replay_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let resolver = Arc::new(FixtureResolver {
        workspace: workspace.clone(),
        calls: calls.clone(),
        bindings: vec![],
    });
    let req = request(
        &workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let first_model = MockCompletionModel::from_turns([
        MockTurn::tool_call("business", "anchor_echo", json!({"value":"only-once"})),
        MockTurn::error("provider unavailable"),
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        first_model.erase(),
        resolver.clone(),
    );
    assert!(port.execute(req.clone()).await.is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let recording_root = directory(&port, &req);
    let original =
        std::fs::read(recording_root.join("00000000000000000001/recording.json")).unwrap();
    assert_eq!(
        read(recording_root.join("00000000000000000002/outcome.json"))["status"],
        "failed"
    );
    // A terminal provider error follows native Harness escalation, so reopening
    // cannot make it resumable by reading or deleting diagnostic records.
    let fresh = MockCompletionModel::from_turns([MockTurn::tool_call(
        "finish",
        "final_result",
        json!({"summary":"done","route":"next"}),
    )]);
    let reopened = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        fresh.clone().erase(),
        resolver,
    );
    assert!(reopened.execute(req).await.is_err());
    assert_eq!(fresh.request_count(), 0);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(recording_root.join("00000000000000000001/recording.json")).unwrap(),
        original
    );
}

#[tokio::test]
async fn interrupted_production_invocation_reopens_and_appends_without_replaying_completed_tool() {
    use rig_core::{
        driver::{Exchange, Model, Opening, Transport},
        test_utils::{MockFrame, MockRuntime, MockScript},
    };

    #[derive(Clone)]
    struct InterruptSecondRequest {
        inner: MockRuntime,
        calls: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
    }
    impl Transport<MockScript> for InterruptSecondRequest {
        fn send(
            &self,
            payload: rig_core::completion::CompletionRequest,
            exchange: Exchange,
        ) -> Opening<MockFrame> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 1 {
                let entered = self.entered.clone();
                Opening::new(async move {
                    entered.notify_one();
                    std::future::pending().await
                })
            } else {
                self.inner.send(payload, exchange)
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let resolver = Arc::new(FixtureResolver {
        workspace: workspace.clone(),
        calls: calls.clone(),
        bindings: vec![],
    });
    let mock = MockCompletionModel::from_turns([MockTurn::tool_call(
        "business",
        "anchor_echo",
        json!({"value":"committed"}),
    )]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let model = Model::new(
        mock.wire,
        InterruptSecondRequest {
            inner: mock.transport,
            calls: Arc::new(AtomicUsize::new(0)),
            entered: entered.clone(),
        },
    );
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.erase(),
        resolver.clone(),
    );
    let req = request(
        &workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    // Dropping the direct worker future models process loss without leaving
    // NodePort's dedicated blocking thread running in this test process.
    tokio::select! {
        result = port.execute_agent(req.clone()) => panic!("unexpected terminal outcome: {result:?}"),
        _ = entered.notified() => {},
        _ = tokio::time::sleep(Duration::from_secs(10)) => panic!("second request did not begin"),
    }
    let recording_root = directory(&port, &req);
    let first = recording_root.join("00000000000000000001/recording.json");
    let original = std::fs::read(&first).unwrap();
    let unknown = recording_root.join("00000000000000000002");
    assert!(unknown.join("request.json").is_file());
    assert!(unknown.join("rig-request.json").is_file());
    assert!(!unknown.join("outcome.json").exists());
    assert!(!unknown.join("recording.json").exists());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let fresh = MockCompletionModel::from_turns([MockTurn::tool_call(
        "finish",
        "final_result",
        json!({"summary":"resumed","route":"next"}),
    )]);
    let reopened = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        fresh.clone().erase(),
        resolver,
    );
    let result = reopened.execute(req).await.unwrap();
    assert!(
        matches!(result, NodeExecutionOutcome::Completed(completion) if completion.submission == "resumed")
    );
    assert_eq!(fresh.request_count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(first).unwrap(), original);
    assert!(
        recording_root
            .join("00000000000000000003/recording.json")
            .is_file()
    );
    assert!(!unknown.join("outcome.json").exists());
}
