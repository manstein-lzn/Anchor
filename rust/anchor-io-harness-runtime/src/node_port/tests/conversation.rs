use super::*;
use crate::conversation::Admission;
use rig_core::test_utils::MockStreamEvent;

struct ConversationResolver {
    workspace_root: PathBuf,
    scopes: std::collections::BTreeMap<String, String>,
    calls: Arc<AtomicUsize>,
    modes: std::sync::Mutex<std::collections::BTreeMap<String, ToolMode>>,
}

#[derive(Clone, Default)]
enum ToolMode {
    #[default]
    Echo,
    Cancel(anchor_runtime_rig::Cancellation),
    Pending(Arc<tokio::sync::Notify>),
}

struct ConversationTools {
    workspace: PathBuf,
    calls: Arc<AtomicUsize>,
    mode: ToolMode,
}

impl ToolPort for ConversationTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        EchoPort {
            calls: self.calls.clone(),
        }
        .definitions()
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name != "anchor_echo" {
                return Err(ToolError::Unknown(name.to_owned()));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::write(
                self.workspace.join("effect.txt"),
                arguments["value"].as_str().unwrap_or_default(),
            )
            .unwrap();
            match &self.mode {
                ToolMode::Echo => {}
                ToolMode::Cancel(cancellation) => cancellation.store(true, Ordering::SeqCst),
                ToolMode::Pending(entered) => {
                    entered.notify_one();
                    std::future::pending::<()>().await
                }
            }
            Ok(vec![ToolResultContent::json(
                json!({"echo":arguments["value"]}),
            )])
        })
    }
}

impl NodeHostResolver for ConversationResolver {
    fn resume_after_interleaving(&self, request: &NodeExecutionRequest) -> bool {
        request.key.run_id.starts_with("background-")
    }
    fn conversation_hint(
        &self,
        request: &NodeExecutionRequest,
    ) -> Result<Option<NodeConversationHint>, String> {
        Ok(self
            .scopes
            .get(&request.key.run_id)
            .map(|key| NodeConversationHint { key: key.clone() }))
    }

    fn resolve_plugins(&self, _ids: &[String]) -> Result<Vec<PluginBinding>, String> {
        Ok(vec![])
    }

    fn workspace(&self, request: &NodeExecutionRequest) -> Result<PathBuf, String> {
        let path = self.workspace_root.join(&request.key.run_id);
        std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        Ok(path)
    }

    fn tools<'a>(&'a self, request: &'a NodeExecutionRequest) -> ToolResolution<'a> {
        Box::pin(async move {
            let mode = self
                .modes
                .lock()
                .unwrap()
                .get(&request.key.run_id)
                .cloned()
                .unwrap_or_default();
            Ok(Arc::new(ConversationTools {
                workspace: self.workspace(request)?,
                calls: self.calls.clone(),
                mode,
            }) as Arc<dyn ToolPort>)
        })
    }
}

fn resolver(root: &Path, scopes: &[(&str, &str)]) -> Arc<ConversationResolver> {
    Arc::new(ConversationResolver {
        workspace_root: root.join("workspaces"),
        scopes: scopes
            .iter()
            .map(|(run, scope)| (run.to_string(), scope.to_string()))
            .collect(),
        calls: Arc::new(AtomicUsize::new(0)),
        modes: std::sync::Mutex::new(std::collections::BTreeMap::new()),
    })
}

fn conversation_request(run: &str, prompt: &str) -> NodeExecutionRequest {
    let mut req = request(
        Path::new("unused"),
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    req.key.run_id = run.into();
    req.task = prompt.into();
    req
}

fn completion(summary: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::tool_call(
            "completion",
            "final_result",
            json!({"summary": summary, "route": "next"}),
        ),
        MockStreamEvent::final_response_with_default_usage(),
    ]
}

#[tokio::test]
async fn distinct_graph_runs_share_native_history_and_keep_exact_trace_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(
        dir.path(),
        &[
            ("run-1", "session-a/graph/agent"),
            ("run-2", "session-a/graph/agent"),
        ],
    );
    let model = MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::tool_call(
                "echo",
                "anchor_echo",
                json!({"value":"first-tool-evidence"}),
            ),
            MockStreamEvent::final_response_with_default_usage(),
        ],
        completion("first-reply-evidence"),
        completion("second-reply"),
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver.clone(),
    );
    let first = conversation_request("run-1", "first-prompt-evidence");
    let second = conversation_request("run-2", "second-prompt-evidence");
    assert!(matches!(
        port.execute(first.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert!(matches!(
        port.execute(second.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let sent = serde_json::to_value(model.requests()).unwrap();
    assert!(sent[2].to_string().contains("first-prompt-evidence"));
    assert!(sent[2].to_string().contains("first-reply-evidence"));
    assert!(
        !sent[2]
            .to_string()
            .contains("did not complete successfully")
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    let paths = ConversationPaths::new(&port.io_store_root, "session-a/graph/agent", &first.key);
    let (store, session) = paths.open_or_create().unwrap();
    let turns = store.session_turns(session.id()).unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].parent_turn_id, Some(turns[0].id));
    assert_eq!(
        store.run_file(turns[0].run_id).unwrap(),
        store.run_file(turns[1].run_id).unwrap()
    );
    assert_ne!(paths.root_path(), resolver.workspace_root.join("run-1"));
    assert_eq!(
        std::fs::read_to_string(resolver.workspace_root.join("run-1/effect.txt")).unwrap(),
        "first-tool-evidence"
    );
    assert!(!paths.root_path().join("effect.txt").exists());
    let advertised = sent[2]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(advertised.contains(&"anchor_echo"));
    assert!(advertised.contains(&"final_result"));
    let first_trace = trace_messages(&port.io_store_root, &first.key).unwrap();
    assert!(
        serde_json::to_string(&first_trace)
            .unwrap()
            .contains("first-tool-evidence")
    );
    assert!(
        serde_json::to_string(&first_trace)
            .unwrap()
            .contains("first-reply-evidence")
    );
    drop(store);
    drop(port);
    let fresh = MockCompletionModel::from_turns([]);
    let reopened = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        fresh.clone().erase(),
        resolver,
    );
    assert_eq!(
        trace_messages(&reopened.io_store_root, &first.key).unwrap(),
        first_trace
    );
    assert!(matches!(
        reopened.execute(first.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert_eq!(fresh.request_count(), 0);
    assert_ne!(
        crate::recording::directory(&reopened.io_store_root, &first.key),
        crate::recording::directory(&reopened.io_store_root, &second.key)
    );
    assert_eq!(
        std::fs::read_dir(crate::recording::directory(
            &reopened.io_store_root,
            &first.key
        ))
        .unwrap()
        .count(),
        2
    );
    assert_eq!(
        std::fs::read_dir(crate::recording::directory(
            &reopened.io_store_root,
            &second.key
        ))
        .unwrap()
        .count(),
        1
    );
}

#[tokio::test]
async fn same_node_in_distinct_sessions_is_isolated() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(
        dir.path(),
        &[
            ("run-1", "session-a/graph/agent"),
            ("run-2", "session-b/graph/agent"),
        ],
    );
    let model = MockCompletionModel::from_stream_turns([
        completion("private-first-reply"),
        completion("other-session-reply"),
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver,
    );
    let first = conversation_request("run-1", "private-first-prompt");
    let second = conversation_request("run-2", "other-session-prompt");
    assert!(matches!(
        port.execute(first.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert!(matches!(
        port.execute(second.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let requests = serde_json::to_value(model.requests()).unwrap();
    assert!(!requests[1].to_string().contains("private-first"));
    assert_ne!(
        port.checked_io_run_path(&first.key).unwrap(),
        port.checked_io_run_path(&second.key).unwrap()
    );
}

#[tokio::test]
async fn cancelled_turn_keeps_actual_outcome_and_new_message_sees_its_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), &[("run-1", "same"), ("run-2", "same")]);
    let first = conversation_request("run-1", "cancelled-prompt-evidence");
    resolver
        .modes
        .lock()
        .unwrap()
        .insert("run-1".into(), ToolMode::Cancel(first.cancellation.clone()));
    let model = MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::tool_call("echo", "anchor_echo", json!({"value":"cancelled-effect"})),
            MockStreamEvent::final_response_with_default_usage(),
        ],
        completion("new-turn-reply"),
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver.clone(),
    );
    assert_eq!(
        port.execute(first.clone()).await.unwrap(),
        NodeExecutionOutcome::Cancelled
    );
    let second = conversation_request("run-2", "new-prompt");
    assert!(matches!(
        port.execute(second).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let requests = serde_json::to_value(model.requests()).unwrap();
    assert!(
        requests[1]
            .to_string()
            .contains("cancelled-prompt-evidence")
    );
    assert!(
        requests[1]
            .to_string()
            .contains("did not complete successfully")
    );
    let paths = ConversationPaths::new(&port.io_store_root, "same", &first.key);
    let (store, session) = paths.open_or_create().unwrap();
    let turns = store.session_turns(session.id()).unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].outcome.as_deref(), Some("cancelled"));
    assert_eq!(turns[1].parent_turn_id, Some(turns[0].id));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn abandoned_tool_keeps_unknown_effect_and_new_turn_never_replays_it() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), &[("run-1", "same"), ("run-2", "same")]);
    let entered = Arc::new(tokio::sync::Notify::new());
    resolver
        .modes
        .lock()
        .unwrap()
        .insert("run-1".into(), ToolMode::Pending(entered.clone()));
    let model = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::tool_call("echo", "anchor_echo", json!({"value":"unknown-effect"})),
        MockStreamEvent::final_response_with_default_usage(),
    ]]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.erase(),
        resolver.clone(),
    );
    let first = conversation_request("run-1", "unfinished-prompt-evidence");
    tokio::select! {
        result = port.execute_agent(first.clone()) => panic!("unexpected outcome: {result:?}"),
        _ = entered.notified() => {},
        _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("tool did not begin"),
    }
    let paths = ConversationPaths::new(&port.io_store_root, "same", &first.key);
    let locator = paths.read_locator().unwrap().unwrap();
    let (store, session) = paths.open_or_create().unwrap();
    assert_eq!(session.head(), None);
    assert_eq!(
        store
            .session_turn(locator.turn_id)
            .unwrap()
            .unwrap()
            .outcome,
        None
    );
    let attempt = store.open_attempts(locator.run_id).unwrap().remove(0);
    assert!(
        trace_messages(&port.io_store_root, &first.key)
            .unwrap()
            .iter()
            .any(|message| message["unknown_external_outcome"] == true)
    );
    drop(store);
    drop(port);
    let fresh = MockCompletionModel::from_stream_turns([completion("continued-work")]);
    let reopened = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        fresh.clone().erase(),
        resolver.clone(),
    );
    assert!(matches!(
        reopened
            .execute(conversation_request("run-2", "inspect-before-continuing"))
            .await
            .unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let sent = serde_json::to_value(fresh.requests()).unwrap().to_string();
    assert!(sent.contains("unfinished-prompt-evidence"));
    assert!(sent.contains("External outcome is unknown"));
    assert!(sent.contains(&format!("attempt={}", attempt.id)));
    let (store, session) = paths.open_or_create().unwrap();
    let turns = store.session_turns(session.id()).unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].outcome, None);
    assert_eq!(turns[1].parent_turn_id, Some(locator.turn_id));
    assert_eq!(store.open_attempts(locator.run_id).unwrap().len(), 1);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert!(
        reopened
            .execute(first)
            .await
            .unwrap_err()
            .to_string()
            .contains("earlier native conversation turn")
    );
    assert_eq!(fresh.request_count(), 1);
}

#[tokio::test]
async fn same_invocation_recovery_resumes_exact_run_without_a_second_session_turn() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), &[("run-1", "same")]);
    let entered = Arc::new(tokio::sync::Notify::new());
    resolver
        .modes
        .lock()
        .unwrap()
        .insert("run-1".into(), ToolMode::Pending(entered.clone()));
    let model = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::tool_call("echo", "anchor_echo", json!({"value":"once"})),
        MockStreamEvent::final_response_with_default_usage(),
    ]]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.erase(),
        resolver.clone(),
    );
    let req = conversation_request("run-1", "resume-this-invocation");
    tokio::select! {
        result = port.execute_agent(req.clone()) => panic!("unexpected outcome: {result:?}"),
        _ = entered.notified() => {},
        _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("tool did not begin"),
    }
    let paths = ConversationPaths::new(&port.io_store_root, "same", &req.key);
    let locator = paths.read_locator().unwrap().unwrap();
    let (store, _) = paths.open_or_create().unwrap();
    let attempt = store.open_attempts(locator.run_id).unwrap().remove(0);
    drop(store);
    drop(port);
    let fresh = MockCompletionModel::from_turns([MockTurn::tool_call(
        "finish",
        "final_result",
        json!({"summary":"resumed", "route":"next"}),
    )]);
    let reopened = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        fresh.clone().erase(),
        resolver.clone(),
    );
    assert!(matches!(
        reopened.execute(req.clone()).await.unwrap(),
        NodeExecutionOutcome::WaitingRecovery { .. }
    ));
    assert_eq!(fresh.request_count(), 0);
    reopened.record_recovery_decision(&req.key, attempt.id, RecoveryDecision::Completed { observation:"Tool result was not recorded; external outcome is unknown. Inspect the real state before deciding whether to act.".into() }).unwrap();
    assert!(matches!(
        reopened.execute(req.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let (store, session) = paths.open_or_create().unwrap();
    assert_eq!(store.runs().unwrap(), vec![locator.run_id]);
    assert_eq!(store.session_turns(session.id()).unwrap().len(), 1);
    assert_eq!(session.head(), Some(locator.turn_id));
    assert_eq!(
        store
            .session_turn(locator.turn_id)
            .unwrap()
            .unwrap()
            .outcome
            .as_deref(),
        Some("finished")
    );
    assert_eq!(paths.read_locator().unwrap().unwrap(), locator);
    assert_eq!(fresh.request_count(), 1);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    let trace = trace_messages(&reopened.io_store_root, &req.key).unwrap();
    assert!(trace.iter().any(|message| {
        message["text"]
            .as_str()
            .is_some_and(|text| text.contains("external outcome is unknown"))
    }));
}

#[tokio::test]
async fn provider_failure_retains_native_history_for_new_turn_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), &[("run-1", "same"), ("run-2", "same")]);
    let model = MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::tool_call(
                "echo",
                "anchor_echo",
                json!({"value":"recorded-before-error"}),
            ),
            MockStreamEvent::final_response_with_default_usage(),
        ],
        vec![MockStreamEvent::error("provider unavailable")],
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.erase(),
        resolver.clone(),
    );
    let req = conversation_request("run-1", "provider-failure-prompt");
    assert!(port.execute(req.clone()).await.is_err());
    let paths = ConversationPaths::new(&port.io_store_root, "same", &req.key);
    let first_locator = paths.read_locator().unwrap().unwrap();
    let (store, _) = paths.open_or_create().unwrap();
    let original_turn = store.session_turn(first_locator.turn_id).unwrap().unwrap();
    assert!(!matches!(
        original_turn.outcome.as_deref(),
        Some("finished" | "success")
    ));
    drop(store);
    let fresh = MockCompletionModel::from_stream_turns([completion("after-error")]);
    let reopened = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        fresh.clone().erase(),
        resolver.clone(),
    );
    assert!(reopened.execute(req.clone()).await.is_err());
    assert_eq!(fresh.request_count(), 0);
    assert!(matches!(
        reopened
            .execute(conversation_request("run-2", "new-message-after-error"))
            .await
            .unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert!(
        serde_json::to_value(fresh.requests())
            .unwrap()
            .to_string()
            .contains("provider-failure-prompt")
    );
    let (store, session) = paths.open_or_create().unwrap();
    let turns = store.session_turns(session.id()).unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0], original_turn);
    assert_eq!(turns[1].parent_turn_id, Some(first_locator.turn_id));
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert!(
        serde_json::to_string(&trace_messages(&reopened.io_store_root, &req.key).unwrap())
            .unwrap()
            .contains("recorded-before-error")
    );
}

#[tokio::test]
async fn native_session_preserves_tool_mask_and_rejects_plain_json_completion() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), &[("run-1", "same")]);
    let forbidden = dir.path().join("forbidden.txt");
    let model = MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::tool_call(
                "native-write",
                "write_file",
                json!({"path":forbidden, "content":"forbidden"}),
            ),
            MockStreamEvent::final_response_with_default_usage(),
        ],
        vec![
            MockStreamEvent::text(
                "{\"summary\":\"plain-json-is-not-completion\",\"route\":\"next\"}",
            ),
            MockStreamEvent::final_response_with_default_usage(),
        ],
        completion("native-completion-only"),
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver,
    );
    let outcome = port
        .execute(conversation_request(
            "run-1",
            "complete-only-via-final-result",
        ))
        .await
        .unwrap();
    assert!(
        matches!(outcome, NodeExecutionOutcome::Completed(ref completion) if completion.submission == "native-completion-only")
    );
    assert!(!forbidden.exists());
    assert_eq!(model.request_count(), 3);
    let requests = serde_json::to_value(model.requests()).unwrap();
    assert!(requests[1].to_string().contains("withholds that tool"));
    assert!(requests[2].to_string().contains("call final_result"));
}

#[tokio::test]
async fn crash_before_started_locator_binds_the_only_admitted_run_without_new_turn() {
    for recorded_turn in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let resolver = resolver(dir.path(), &[("run-1", "same")]);
        let model = MockCompletionModel::from_turns([MockTurn::tool_call(
            "finish",
            "final_result",
            json!({"summary":"after-gap", "route":"next"}),
        )]);
        let port = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            model.clone().erase(),
            resolver,
        );
        let req = conversation_request("run-1", "exact-gap-prompt");
        port.ensure_roots().unwrap();
        port.mark_started(&req.key).unwrap();
        let paths = ConversationPaths::new(&port.io_store_root, "same", &req.key);
        let lease = paths.acquire(&port.io_store_root).unwrap();
        let prepared = port.prepare_conversation(&paths, &req).unwrap();
        let run_id = prepared
            .store
            .start_run(&req.task, paths.root_path().to_str().unwrap())
            .unwrap();
        if recorded_turn {
            prepared
                .store
                .record_turn(
                    prepared.session.id(),
                    prepared.admission.parent_turn_id,
                    run_id,
                    &req.task,
                )
                .unwrap();
        }
        drop(prepared);
        drop(lease);
        assert!(matches!(
            port.completion_fact(&req.key).await.unwrap(),
            CompletionFact::Resumable
        ));
        assert!(matches!(
            port.execute(req).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        let locator = paths.read_locator().unwrap().unwrap();
        assert_eq!(locator.run_id, run_id);
        let (store, session) = paths.open_or_create().unwrap();
        assert_eq!(store.runs().unwrap(), vec![run_id]);
        assert_eq!(store.session_turns(session.id()).unwrap().len(), 1);
        assert_eq!(session.head(), Some(locator.turn_id));
        assert_eq!(model.request_count(), 1);
    }
}

#[tokio::test]
async fn ambiguous_shared_store_rows_never_bind_the_latest_run_or_bill_again() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), &[("run-1", "same")]);
    let model = MockCompletionModel::from_turns([]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver,
    );
    let req = conversation_request("run-1", "same-prompt");
    port.ensure_roots().unwrap();
    port.mark_started(&req.key).unwrap();
    let paths = ConversationPaths::new(&port.io_store_root, "same", &req.key);
    let lease = paths.acquire(&port.io_store_root).unwrap();
    let prepared = port.prepare_conversation(&paths, &req).unwrap();
    for _ in 0..2 {
        let run_id = prepared
            .store
            .start_run(&req.task, paths.root_path().to_str().unwrap())
            .unwrap();
        prepared
            .store
            .record_turn(
                prepared.session.id(),
                prepared.admission.parent_turn_id,
                run_id,
                &req.task,
            )
            .unwrap();
    }
    drop(prepared);
    drop(lease);
    let error = port.execute(req).await.unwrap_err();
    assert!(error.to_string().contains("multiple native runs"));
    assert_eq!(model.request_count(), 0);
    assert!(paths.read_locator().unwrap().is_none());
    let (store, session) = paths.open_or_create().unwrap();
    assert_eq!(store.runs().unwrap().len(), 2);
    assert_eq!(session.head(), None);
}

#[tokio::test]
async fn terminal_turn_repairs_locator_and_head_publication_windows_without_billing() {
    for scope_locator_persisted in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let resolver = resolver(dir.path(), &[("run-1", "same")]);
        let model = MockCompletionModel::from_stream_turns([completion("already-finished")]);
        let port = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            model.clone().erase(),
            resolver,
        );
        let req = conversation_request("run-1", "published-native-turn");
        assert!(matches!(
            port.execute(req.clone()).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        let paths = ConversationPaths::new(&port.io_store_root, "same", &req.key);
        let locator = paths.read_locator().unwrap().unwrap();
        let admission = paths.read_admission().unwrap().unwrap();
        let (store, session) = paths.open_or_create().unwrap();
        store
            .set_session_head_if(
                session.id(),
                Some(locator.turn_id),
                admission.parent_turn_id,
            )
            .unwrap();
        drop(store);
        if !scope_locator_persisted {
            std::fs::remove_file(
                port.io_store_root
                    .join("conversations")
                    .join(&paths.scope)
                    .join("invocations")
                    .join(format!(
                        "{}.json",
                        IoHarnessNodePort::<ConversationResolver>::stem(&req.key)
                    )),
            )
            .unwrap();
        }
        paths.publish_admission(&admission).unwrap();
        std::fs::remove_file(port.completion_path(&req.key)).unwrap();
        port.mark_started(&req.key).unwrap();
        assert!(matches!(
            port.execute(req.clone()).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        assert_eq!(model.request_count(), 1);
        assert_eq!(paths.read_locator().unwrap().unwrap(), locator);
        let (store, session) = paths.open_or_create().unwrap();
        assert_eq!(store.runs().unwrap(), vec![locator.run_id]);
        assert_eq!(store.session_turns(session.id()).unwrap().len(), 1);
        assert_eq!(session.head(), Some(locator.turn_id));
        assert!(
            serde_json::to_string(&trace_messages(&port.io_store_root, &req.key).unwrap())
                .unwrap()
                .contains("already-finished")
        );
    }
}

#[tokio::test]
async fn scope_removal_is_locked_isolated_and_recreated_without_old_history() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(
        dir.path(),
        &[
            ("run-1", "scope-a"),
            ("run-2", "scope-b"),
            ("run-3", "scope-a"),
        ],
    );
    let model = MockCompletionModel::from_stream_turns([
        completion("old-a-reply"),
        completion("retained-b-reply"),
        completion("fresh-a-reply"),
    ]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver,
    );
    let first = conversation_request("run-1", "old-a-prompt");
    let other = conversation_request("run-2", "retained-b-prompt");
    assert!(matches!(
        port.execute(first.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert!(matches!(
        port.execute(other.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let paths = ConversationPaths::new(&port.io_store_root, "scope-a", &first.key);
    let lease = paths.acquire(&port.io_store_root).unwrap();
    let hint = NodeConversationHint {
        key: "scope-a".into(),
    };
    assert!(
        remove_conversation(&port.io_store_root, &hint)
            .unwrap_err()
            .contains("already active")
    );
    assert!(paths.store_path().exists());
    drop(lease);
    remove_conversation(&port.io_store_root, &hint).unwrap();
    assert!(!paths.store_path().exists());
    assert!(!paths.root_path().exists());
    assert!(
        trace_messages(&port.io_store_root, &first.key)
            .unwrap()
            .is_empty()
    );
    assert!(!crate::recording::directory(&port.io_store_root, &first.key).exists());
    assert!(
        serde_json::to_string(&trace_messages(&port.io_store_root, &other.key).unwrap())
            .unwrap()
            .contains("retained-b-reply")
    );
    remove_conversation(&port.io_store_root, &hint).unwrap();
    assert!(matches!(
        port.execute(conversation_request("run-3", "fresh-a-prompt"))
            .await
            .unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let requests = serde_json::to_value(model.requests()).unwrap();
    assert!(!requests[2].to_string().contains("old-a-prompt"));
    assert!(!requests[2].to_string().contains("old-a-reply"));
}

#[tokio::test]
async fn admission_publication_reentry_repairs_second_turn_and_allows_third_turn() {
    for window in 0..3 {
        let dir = tempfile::tempdir().unwrap();
        let resolver = resolver(
            dir.path(),
            &[("run-1", "same"), ("run-2", "same"), ("run-3", "same")],
        );
        let model = MockCompletionModel::from_stream_turns([
            completion("first"),
            completion("second"),
            completion("third"),
        ]);
        let port = IoHarnessNodePort::fixture(
            dir.path().join("facts"),
            dir.path().join("io"),
            model.clone().erase(),
            resolver,
        );
        let first = conversation_request("run-1", "first-prompt");
        let second = conversation_request("run-2", "second-prompt");
        let third = conversation_request("run-3", "third-prompt");
        assert!(matches!(
            port.execute(first.clone()).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        let first_paths = ConversationPaths::new(&port.io_store_root, "same", &first.key);
        let paths = ConversationPaths::new(&port.io_store_root, "same", &second.key);
        if window == 2 {
            assert!(matches!(
                port.execute(second.clone()).await.unwrap(),
                NodeExecutionOutcome::Completed(_)
            ));
            let first_admission = first_paths.read_admission().unwrap().unwrap();
            first_paths.publish_admission(&first_admission).unwrap();
            std::fs::remove_file(port.completion_path(&second.key)).unwrap();
            port.mark_started(&second.key).unwrap();
        } else {
            let lease = paths.acquire(&port.io_store_root).unwrap();
            let (store, session) = paths.open_or_create().unwrap();
            let admission = Admission {
                invocation: second.key.durable_key(),
                session_id: session.id(),
                parent_turn_id: session.head(),
                prompt: second.task.clone(),
                runs_before: store.runs().unwrap(),
            };
            paths.write_admission(&admission).unwrap();
            if window == 1 {
                IoHarnessNodePort::<ConversationResolver>::write_atomic(
                    &port.conversation_sidecar_path(&second.key),
                    &serde_json::to_vec(&json!({"version":1,"scope":paths.scope,"invocation":admission.invocation,"session_id":admission.session_id})).unwrap(),
                ).unwrap();
            }
            port.mark_started(&second.key).unwrap();
            drop(store);
            drop(lease);
        }
        assert_eq!(
            paths.read_pending().unwrap().unwrap().invocation,
            first.key.durable_key()
        );
        assert!(matches!(
            port.completion_fact(&second.key).await.unwrap(),
            CompletionFact::Resumable
        ));
        assert!(matches!(
            port.execute(second.clone()).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        assert_eq!(
            paths.read_pending().unwrap().unwrap().invocation,
            second.key.durable_key()
        );
        assert!(!port.io_run_path(&second.key).exists());
        assert!(matches!(
            port.execute(third).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        let (store, session) = paths.open_or_create().unwrap();
        let turns = store.session_turns(session.id()).unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1].parent_turn_id, Some(turns[0].id));
        assert_eq!(turns[2].parent_turn_id, Some(turns[1].id));
        assert_eq!(model.request_count(), 3);
    }
}

#[tokio::test]
async fn background_yields_to_foreground_and_resumes_without_rewinding_native_head() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(
        dir.path(),
        &[
            ("background-1", "same"),
            ("foreground-1", "same"),
            ("foreground-2", "same"),
        ],
    );
    let entered = Arc::new(tokio::sync::Notify::new());
    resolver
        .modes
        .lock()
        .unwrap()
        .insert("background-1".into(), ToolMode::Pending(entered.clone()));
    let model = MockCompletionModel::from_stream_turns([vec![
        MockStreamEvent::tool_call("echo", "anchor_echo", json!({"value":"once"})),
        MockStreamEvent::final_response_with_default_usage(),
    ]]);
    let port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.erase(),
        resolver.clone(),
    );
    let req = conversation_request("background-1", "complete background report");
    let cancellation = req.cancellation.clone();
    let stop = async {
        entered.notified().await;
        cancellation.store(true, Ordering::SeqCst);
    };
    let (result, _) = tokio::join!(port.execute(req.clone()), stop);
    assert!(matches!(result.unwrap(), NodeExecutionOutcome::Cancelled));
    let paths = ConversationPaths::new(&port.io_store_root, "same", &req.key);
    let locator = paths.read_locator().unwrap().unwrap();
    let (store, _) = paths.open_or_create().unwrap();
    let attempt = store.open_attempts(locator.run_id).unwrap().remove(0);
    drop(store);
    drop(port);
    let model = MockCompletionModel::from_stream_turns([completion("foreground answer")]);
    let foreground = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.erase(),
        resolver.clone(),
    );
    assert!(matches!(
        foreground
            .execute(conversation_request(
                "foreground-1",
                "New requirement: use purple in the report"
            ))
            .await
            .unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let (_, session) = paths.open_or_create().unwrap();
    let head = session.head();
    assert_ne!(head, Some(locator.turn_id));
    drop(foreground);
    let model = MockCompletionModel::from_turns([MockTurn::tool_call(
        "finish",
        "final_result",
        json!({"summary":"purple report done", "route":"next"}),
    )]);
    let resumed = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        model.clone().erase(),
        resolver.clone(),
    );
    req.cancellation.store(false, Ordering::SeqCst);
    assert!(matches!(
        resumed.execute(req.clone()).await.unwrap(),
        NodeExecutionOutcome::WaitingRecovery { .. }
    ));
    resumed
        .record_recovery_decision(
            &req.key,
            attempt.id,
            RecoveryDecision::Completed {
                observation: "Effect unknown; inspect before repeating".into(),
            },
        )
        .unwrap();
    assert!(matches!(
        resumed.execute(req.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let sent = serde_json::to_value(model.requests()).unwrap().to_string();
    assert!(sent.contains("use purple in the report"), "{sent}");
    let (store, session) = paths.open_or_create().unwrap();
    assert_eq!(session.head(), head);
    assert_eq!(store.session_turns(session.id()).unwrap().len(), 2);
    assert_eq!(paths.read_locator().unwrap().unwrap(), locator);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    let observations = store.observations(locator.run_id).unwrap();
    assert_eq!(
        observations
            .iter()
            .filter(
                |item| item.text.starts_with("[Session turn") && item.text.contains("use purple")
            )
            .count(),
        1
    );
    drop(store);
    let next = MockCompletionModel::from_stream_turns([completion("next answer")]);
    let next_port = IoHarnessNodePort::fixture(
        dir.path().join("facts"),
        dir.path().join("io"),
        next.clone().erase(),
        resolver,
    );
    assert!(matches!(
        next_port
            .execute(conversation_request(
                "foreground-2",
                "what is the report status?"
            ))
            .await
            .unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    let sent = serde_json::to_value(next.requests()).unwrap().to_string();
    assert!(sent.contains("purple report done") && sent.contains("use purple in the report"));
}
