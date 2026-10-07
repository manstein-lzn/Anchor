use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rig_core::test_utils::{MockCompletionModel, MockError, MockStreamEvent};
use sha2::{Digest, Sha256};

mod interactive;

#[derive(Default)]
struct Watching {
    chunks: Mutex<Vec<Value>>,
    cancelled: AtomicBool,
    cancel_on: Option<&'static str>,
    fail_on: Option<&'static str>,
    native_runs: Mutex<Vec<(i64, i64)>>,
    fail_native: bool,
}

impl PilotObserver for Watching {
    fn event(&self, chunk: Value) -> Result<(), String> {
        if chunk["type"] == self.cancel_on.unwrap_or("") {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        let failed = chunk["type"] == self.fail_on.unwrap_or("");
        self.chunks.lock().unwrap().push(chunk);
        if failed {
            Err("fixture observer unavailable".into())
        } else {
            Ok(())
        }
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn native_run(&self, session: i64, run: i64) -> Result<(), String> {
        self.native_runs.lock().unwrap().push((session, run));
        if self.fail_native {
            return Err("fixture native association unavailable".into());
        }
        Ok(())
    }
}

#[derive(Default)]
struct FixturePort {
    calls: AtomicUsize,
    wait: bool,
    indeterminate: bool,
    cancel: Option<Arc<Watching>>,
    entered: Option<std::sync::mpsc::Sender<()>>,
    started: Option<Arc<tokio::sync::Notify>>,
}

impl ToolPort for FixturePort {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition {
            name: "anchor_fixture_read".into(),
            description: "Read a fixture through the explicitly authorized Anchor port".into(),
            parameters: json!({"type":"object", "properties":{"key":{"type":"string"}}, "required":["key"], "additionalProperties":false}),
        }]
    }

    fn is_read_only(&self, _name: &str) -> bool {
        !self.indeterminate
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name != "anchor_fixture_read" {
                return Err(ToolError::Unknown(name.into()));
            }
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = &self.entered {
                entered.send(()).unwrap();
            }
            if let Some(started) = &self.started {
                started.notify_one();
            }
            if let Some(cancel) = &self.cancel {
                cancel.cancelled.store(true, Ordering::SeqCst);
            }
            if self.wait {
                std::future::pending::<()>().await;
            }
            Ok(vec![ToolResultContent::json(
                json!({"found":arguments["key"]}),
            )])
        })
    }
}

fn scope(directory: &tempfile::TempDir) -> PathBuf {
    directory.path().join(format!(
        "{:x}",
        Sha256::digest(b"fixture owner/session/created")
    ))
}

fn request(root: &Path, prompt: &str) -> PilotRequest {
    PilotRequest {
        root: root.into(),
        prompt: prompt.into(),
        instructions: "Answer using only explicitly registered Anchor tools.".into(),
        max_steps: 4,
        max_tokens: 10_000,
        wall_time: Duration::from_secs(10),
    }
}

fn text(reply: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::Text(reply.into()),
        MockStreamEvent::final_response_with_total_tokens(10),
    ]
}

fn read(key: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::tool_call("wire-read", "anchor_fixture_read", json!({"key":key})),
        MockStreamEvent::final_response_with_total_tokens(10),
    ]
}

fn provider(turns: Vec<Vec<MockStreamEvent>>) -> (MockCompletionModel, RigProviderAdapter) {
    let model = MockCompletionModel::from_stream_turns(turns);
    let adapter = RigProviderAdapter::new(model.clone().erase(), false);
    (model, adapter)
}

fn native(root: &Path) -> (Store, Session) {
    PilotPaths::new(root)
        .unwrap()
        .open_existing()
        .unwrap()
        .unwrap()
}

#[test]
fn missing_or_empty_scope_history_does_not_create_any_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    assert!(pilot_messages(&root).unwrap().is_empty());
    assert!(!root.exists());
    let absent = directory
        .path()
        .join("missing")
        .join(root.file_name().unwrap());
    assert!(pilot_messages(&absent).unwrap().is_empty());
    assert!(!directory.path().join("missing").exists());
    fs::create_dir(&root).unwrap();
    assert!(pilot_messages(&root).unwrap().is_empty());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

#[tokio::test]
async fn stream_reply_and_multiple_turns_use_native_history_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let (first_model, first) = provider(vec![vec![
        MockStreamEvent::Text("hello ".into()),
        MockStreamEvent::Text("operator".into()),
        MockStreamEvent::final_response_with_total_tokens(10),
    ]]);
    let observer = Arc::new(Watching::default());
    let port = Arc::new(FixturePort::default());
    let result = run_pilot(
        request(&root, "remember original question"),
        &first,
        port.clone(),
        observer.clone(),
    )
    .await
    .unwrap();
    assert_eq!(result.status, PilotStatus::Completed);
    assert_eq!(result.reply.as_deref(), Some("hello operator"));
    assert_eq!(first_model.request_count(), 1);
    let chunks = observer.chunks.lock().unwrap().clone();
    assert!(chunks.iter().any(|chunk| chunk["type"] == "text-start"));
    assert!(chunks.iter().any(|chunk| chunk["type"] == "text-end"));
    assert_eq!(
        chunks
            .iter()
            .filter(|chunk| chunk["type"] == "text-delta")
            .map(|chunk| chunk["delta"].as_str().unwrap())
            .collect::<String>(),
        "hello operator"
    );
    drop(chunks);
    let (second_model, second) = provider(vec![text("continued")]);
    let result = run_pilot(
        request(&root, "continue after restart"),
        &second,
        port.clone(),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    assert_eq!(result.status, PilotStatus::Completed);
    let transported = serde_json::to_string(&second_model.requests()).unwrap();
    assert!(transported.contains("remember original question"));
    assert!(transported.contains("hello operator"));
    assert_eq!(second_model.request_count(), 1);
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        pilot_messages(&root).unwrap(),
        vec![
            json!({"role":"user", "text":"remember original question"}),
            json!({"role":"assistant", "text":"hello operator"}),
            json!({"role":"user", "text":"continue after restart"}),
            json!({"role":"assistant", "text":"continued"}),
        ]
    );
    let (store, session) = native(&root);
    let turns = session.history(&store).unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].parent_turn_id, Some(turns[0].id));
}

#[tokio::test]
async fn anchor_tool_chunks_share_stable_ids_and_results_are_native_facts() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let (model, adapter) = provider(vec![read("lookup"), text("lookup answered")]);
    let observer = Arc::new(Watching::default());
    let port = Arc::new(FixturePort::default());
    let outcome = run_pilot(
        request(&root, "look up fixture"),
        &adapter,
        port.clone(),
        observer.clone(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, PilotStatus::Completed);
    assert_eq!(model.request_count(), 2);
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    let chunks = observer.chunks.lock().unwrap();
    let input = chunks
        .iter()
        .find(|chunk| chunk["type"] == "tool-input-available")
        .unwrap();
    let output = chunks
        .iter()
        .find(|chunk| chunk["type"] == "tool-output-available")
        .unwrap();
    let started = chunks
        .iter()
        .find(|chunk| chunk["type"] == "tool-input-start")
        .unwrap();
    assert_eq!(input["toolCallId"], output["toolCallId"]);
    assert_eq!(input["toolCallId"], started["toolCallId"]);
    assert_eq!(input["input"], json!({"key":"lookup"}));
    let (store, session) = native(&root);
    let turn = &session.history(&store).unwrap()[0];
    assert!(
        store
            .observations(turn.run_id)
            .unwrap()
            .iter()
            .any(|observation| observation.text.contains("lookup"))
    );
    assert!(pilot_messages(&root).unwrap().iter().any(|message| {
        message["commands"].as_array().is_some_and(|commands| {
            commands
                .iter()
                .any(|command| command.as_str().unwrap().contains("anchor_fixture_read"))
        })
    }));
    assert!(
        store
            .events_since(turn.run_id, 0, 1000)
            .unwrap()
            .iter()
            .any(|(_, event)| matches!(event.kind, EventKind::ToolCall { .. }))
    );
}

#[tokio::test]
async fn all_builtin_fs_and_exec_calls_are_masked_even_when_model_requests_them() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let marker = directory.path().join("must-not-exist");
    let masked = [
        (
            "exec",
            json!({"command":format!("touch {}", marker.display())}),
        ),
        ("write_file", json!({"path":marker, "content":"forbidden"})),
        ("read_file", json!({"path":"session.json"})),
        ("shell", json!({"command":"printf forbidden"})),
    ];
    for (name, arguments) in masked {
        let (model, adapter) = provider(vec![
            vec![
                MockStreamEvent::tool_call("forbidden", name, arguments),
                MockStreamEvent::final_response_with_total_tokens(10),
            ],
            text("refused"),
        ]);
        let port = Arc::new(FixturePort::default());
        let result = run_pilot(
            request(&root, "test builtin mask"),
            &adapter,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap();
        assert_eq!(result.status, PilotStatus::Completed);
        assert_eq!(model.request_count(), 2);
        for transported in model.requests() {
            assert_eq!(
                transported
                    .tools
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>(),
                vec!["anchor_fixture_read"]
            );
        }
        assert_eq!(port.calls.load(Ordering::SeqCst), 0);
        assert!(!marker.exists());
    }
}

#[tokio::test]
async fn cancel_at_native_start_does_not_call_provider() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let (model, adapter) = provider(vec![text("must not answer")]);
    let observer = Arc::new(Watching {
        cancelled: AtomicBool::new(true),
        ..Default::default()
    });
    let result = run_pilot(
        request(&root, "cancel before model"),
        &adapter,
        Arc::new(FixturePort::default()),
        observer,
    )
    .await
    .unwrap();
    assert_eq!(result.status, PilotStatus::Stopped);
    assert_eq!(model.request_count(), 0);
    assert_eq!(
        pilot_messages(&root).unwrap()[0]["text"],
        "cancel before model"
    );
}

#[tokio::test]
async fn cancellation_is_rechecked_after_native_announcement_and_before_port_dispatch() {
    for cancel_on in ["tool-input-start", "tool-input-available"] {
        let directory = tempfile::tempdir().unwrap();
        let root = scope(&directory);
        let (model, adapter) = provider(vec![read("must-not-dispatch"), text("must not answer")]);
        let observer = Arc::new(Watching {
            cancel_on: Some(cancel_on),
            ..Default::default()
        });
        let port = Arc::new(FixturePort::default());
        let result = run_pilot(
            request(&root, "stop before tool"),
            &adapter,
            port.clone(),
            observer,
        )
        .await
        .unwrap();
        assert_eq!(result.status, PilotStatus::Stopped, "{cancel_on}");
        assert_eq!(port.calls.load(Ordering::SeqCst), 0, "{cancel_on}");
        assert_eq!(model.request_count(), 1, "{cancel_on}");
    }
}

#[tokio::test]
async fn stopping_mid_tool_records_result_then_next_input_does_not_replay() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let observer = Arc::new(Watching::default());
    let port = Arc::new(FixturePort {
        cancel: Some(observer.clone()),
        ..Default::default()
    });
    let (model, adapter) = provider(vec![read("known-result"), text("must not answer")]);
    let outcome = run_pilot(
        request(&root, "stopped earlier prompt"),
        &adapter,
        port.clone(),
        observer,
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, PilotStatus::Stopped);
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.request_count(), 1);
    assert!(
        pilot_messages(&root)
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["text"].as_str().unwrap().contains("known-result"))
    );
    let (next_model, next) = provider(vec![text("continue without replay")]);
    assert_eq!(
        run_pilot(
            request(&root, "new input"),
            &next,
            port.clone(),
            Arc::new(Watching::default())
        )
        .await
        .unwrap()
        .status,
        PilotStatus::Completed
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    assert_eq!(next_model.request_count(), 1);
    assert!(
        serde_json::to_string(&next_model.requests())
            .unwrap()
            .contains("stopped earlier prompt")
    );
}

#[tokio::test]
async fn timeout_preserves_unknown_native_tool_facts_and_continuation_never_replays() {
    for indeterminate in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = scope(&directory);
        let port = Arc::new(FixturePort {
            wait: true,
            indeterminate,
            ..Default::default()
        });
        let (model, adapter) = provider(vec![read("pending"), text("must not answer")]);
        let mut first = request(&root, "previous interrupted prompt");
        first.wall_time = Duration::from_secs(2);
        let outcome = run_pilot(first, &adapter, port.clone(), Arc::new(Watching::default()))
            .await
            .unwrap();
        assert_eq!(outcome.status, PilotStatus::Interrupted);
        assert_eq!(model.request_count(), 1);
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        let (store, session) = native(&root);
        let history = session.history(&store).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].outcome, None);
        assert!(store.run_summary(history[0].run_id).unwrap().is_none());
        assert!(
            !store
                .events_since(history[0].run_id, 0, 1000)
                .unwrap()
                .iter()
                .any(|(_, event)| matches!(event.kind, EventKind::Finished { .. }))
        );
        assert_eq!(
            store.open_attempts(history[0].run_id).unwrap().len(),
            usize::from(indeterminate)
        );
        let messages = pilot_messages(&root).unwrap();
        assert!(messages.iter().any(|message| {
            message["role"] == "tool"
                && message["text"]
                    .as_str()
                    .unwrap()
                    .contains("anchor_fixture_read")
                && message["text"].as_str().unwrap().contains("unknown")
        }));
        drop(store);
        let (next_model, next) = provider(vec![text("checked unknown outcome")]);
        let result = run_pilot(
            request(&root, "new input after timeout"),
            &next,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap();
        assert_eq!(result.status, PilotStatus::Completed);
        let transported = serde_json::to_string(&next_model.requests()).unwrap();
        assert!(transported.contains("previous interrupted prompt"));
        assert!(transported.contains("anchor_fixture_read"));
        assert!(transported.contains("External outcome is unknown"));
        assert_eq!(model.request_count(), 1);
        assert_eq!(next_model.request_count(), 1);
        assert_eq!(port.calls.load(Ordering::SeqCst), 1);
        let (store, session) = native(&root);
        let history = session.history(&store).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].outcome, None);
        assert_eq!(history[1].parent_turn_id, Some(history[0].id));
    }
}

#[tokio::test]
async fn abandoned_future_keeps_unfinished_turn_off_head_until_new_input_branches() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let started = Arc::new(tokio::sync::Notify::new());
    let port = Arc::new(FixturePort {
        wait: true,
        started: Some(started.clone()),
        ..Default::default()
    });
    let (model, adapter) = provider(vec![read("unknown-after-abandonment")]);
    {
        let future = run_pilot(
            request(&root, "previous abandoned prompt"),
            &adapter,
            port.clone(),
            Arc::new(Watching::default()),
        );
        tokio::pin!(future);
        tokio::select! {
            result = &mut future => panic!("pending fixture unexpectedly returned: {result:?}"),
            _ = started.notified() => {}
        }
    }
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.request_count(), 1);
    let (store, session) = native(&root);
    assert_eq!(session.head(), None);
    let original = store.session_turns(session.id()).unwrap();
    assert_eq!(original.len(), 1);
    assert_eq!(original[0].outcome, None);
    drop(store);
    let (next_model, next) = provider(vec![text("continued with native facts")]);
    assert_eq!(
        run_pilot(
            request(&root, "new input after external interruption"),
            &next,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap()
        .status,
        PilotStatus::Completed
    );
    let transported = serde_json::to_string(&next_model.requests()).unwrap();
    assert!(transported.contains("previous abandoned prompt"));
    assert!(transported.contains("External outcome is unknown"));
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.request_count(), 1);
    assert_eq!(next_model.request_count(), 1);
    let (store, session) = native(&root);
    let history = session.history(&store).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].outcome, None);
    assert_eq!(history[1].parent_turn_id, Some(original[0].id));
}

#[tokio::test]
async fn provider_exception_preserves_native_prompt_and_next_input_is_new_transport() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let port = Arc::new(FixturePort::default());
    let (model, adapter) = provider(vec![vec![MockStreamEvent::Error(MockError::request(
        "fixture rejected request",
    ))]]);
    let result = run_pilot(
        request(&root, "previous failed prompt"),
        &adapter,
        port.clone(),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    assert_eq!(result.status, PilotStatus::Failed);
    assert_eq!(model.request_count(), 1);
    let (next_model, next) = provider(vec![text("new response")]);
    assert_eq!(
        run_pilot(
            request(&root, "new prompt after error"),
            &next,
            port.clone(),
            Arc::new(Watching::default())
        )
        .await
        .unwrap()
        .status,
        PilotStatus::Completed
    );
    assert!(
        serde_json::to_string(&next_model.requests())
            .unwrap()
            .contains("previous failed prompt")
    );
    assert_eq!(next_model.request_count(), 1);
    assert_eq!(model.request_count(), 1);
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn observer_failure_cancels_before_tool_without_losing_native_prompt() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let (model, adapter) = provider(vec![read("not-dispatched")]);
    let observer = Arc::new(Watching {
        fail_on: Some("tool-input-available"),
        ..Default::default()
    });
    let port = Arc::new(FixturePort::default());
    let result = run_pilot(
        request(&root, "observer failed prompt"),
        &adapter,
        port.clone(),
        observer,
    )
    .await
    .unwrap();
    assert_eq!(result.status, PilotStatus::Failed);
    assert_eq!(
        result.error.as_deref(),
        Some("fixture observer unavailable")
    );
    assert_eq!(model.request_count(), 1);
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        pilot_messages(&root).unwrap()[0]["text"],
        "observer failed prompt"
    );
}

#[tokio::test]
async fn native_identity_is_reported_before_provider_and_failure_does_not_execute() {
    for fail_native in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = scope(&directory);
        let (model, adapter) = provider(vec![text("linked reply")]);
        let observer = Arc::new(Watching {
            fail_native,
            ..Default::default()
        });
        let result = run_pilot(
            request(&root, "link native execution"),
            &adapter,
            Arc::new(FixturePort::default()),
            observer.clone(),
        )
        .await
        .unwrap();
        let (store, session) = native(&root);
        let runs = observer.native_runs.lock().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, session.id());
        let turn = store
            .session_turn(store.turn_for_run(runs[0].1).unwrap().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(turn.session_id, session.id());
        assert_eq!(turn.prompt, "link native execution");
        if fail_native {
            assert_eq!(result.status, PilotStatus::Failed);
            assert_eq!(model.request_count(), 0);
            assert_eq!(
                result.error.as_deref(),
                Some("fixture native association unavailable")
            );
        } else {
            assert_eq!(result.status, PilotStatus::Completed);
            assert_eq!(model.request_count(), 1);
        }
    }
}

#[tokio::test]
async fn native_step_and_token_budgets_are_not_reported_as_completion() {
    for token_cap in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let root = scope(&directory);
        let (model, adapter) = provider(vec![read("budget"), text("must not answer")]);
        let mut bounded = request(&root, "budgeted prompt");
        if token_cap {
            bounded.max_tokens = 1;
        } else {
            bounded.max_steps = 1;
        }
        let result = run_pilot(
            bounded,
            &adapter,
            Arc::new(FixturePort::default()),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap();
        assert_eq!(result.status, PilotStatus::Stopped);
        assert_eq!(model.request_count(), 1);
    }
}

#[tokio::test]
async fn invalid_admission_is_rejected_before_store_or_transport() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let (model, adapter) = provider(vec![text("must not answer")]);
    for choice in 0..5 {
        let mut invalid = request(&root, "invalid budgets");
        match choice {
            0 => invalid.max_steps = 0,
            1 => invalid.max_steps = u32::MAX as usize + 1,
            2 => invalid.max_tokens = 0,
            3 => invalid.wall_time = Duration::ZERO,
            _ => invalid.prompt.clear(),
        }
        assert!(
            run_pilot(
                invalid,
                &adapter,
                Arc::new(FixturePort::default()),
                Arc::new(Watching::default())
            )
            .await
            .is_err()
        );
    }
    assert!(!root.exists());
    assert_eq!(model.request_count(), 0);
}

#[tokio::test]
async fn existing_native_locator_is_validated_before_provider_or_new_turn() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let (_, adapter) = provider(vec![text("initial")]);
    run_pilot(
        request(&root, "initial"),
        &adapter,
        Arc::new(FixturePort::default()),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    let original = fs::read(root.join("session.json")).unwrap();
    for field in ["version", "framework_version", "session_id", "root"] {
        let mut locator: Value = serde_json::from_slice(&original).unwrap();
        locator[field] = match field {
            "version" => json!(2),
            "framework_version" => json!("future"),
            "session_id" => json!(99999),
            _ => json!(directory.path()),
        };
        fs::write(
            root.join("session.json"),
            serde_json::to_vec(&locator).unwrap(),
        )
        .unwrap();
        assert!(pilot_messages(&root).is_err(), "{field}");
        let (model, untouched) = provider(vec![text("must not answer")]);
        assert!(
            run_pilot(
                request(&root, "must not admit"),
                &untouched,
                Arc::new(FixturePort::default()),
                Arc::new(Watching::default())
            )
            .await
            .is_err(),
            "{field}"
        );
        assert_eq!(model.request_count(), 0);
    }
    fs::write(root.join("session.json"), original).unwrap();
    assert_eq!(native(&root).1.history(&native(&root).0).unwrap().len(), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn scope_store_locator_and_sqlite_sidecars_reject_symlinks_and_hardlinks() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    let outside = directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let root = scope(&directory);
    symlink(&outside, &root).unwrap();
    assert!(pilot_messages(&root).is_err());
    let (model, adapter) = provider(vec![text("must not answer")]);
    assert!(
        run_pilot(
            request(&root, "unsafe"),
            &adapter,
            Arc::new(FixturePort::default()),
            Arc::new(Watching::default())
        )
        .await
        .is_err()
    );
    assert_eq!(model.request_count(), 0);
    fs::remove_file(&root).unwrap();
    fs::create_dir(&root).unwrap();
    let target = outside.join("untouched");
    fs::write(&target, "do not modify").unwrap();
    for name in [
        "framework.sqlite3",
        "session.json",
        "execution.lock",
        "framework.sqlite3-wal",
        "framework.sqlite3-shm",
        "session.json.tmp",
    ] {
        let linked = root.join(name);
        symlink(&target, &linked).unwrap();
        assert!(pilot_messages(&root).is_err(), "{name}");
        assert!(
            run_pilot(
                request(&root, "unsafe"),
                &adapter,
                Arc::new(FixturePort::default()),
                Arc::new(Watching::default())
            )
            .await
            .is_err(),
            "{name}"
        );
        assert_eq!(fs::read_to_string(&target).unwrap(), "do not modify");
        fs::remove_file(&linked).unwrap();
        fs::hard_link(&target, &linked).unwrap();
        assert!(pilot_messages(&root).is_err(), "{name}");
        fs::remove_file(linked).unwrap();
    }
    assert_eq!(model.request_count(), 0);
}

#[tokio::test]
async fn same_scope_writer_lease_is_exclusive_but_history_reader_coexists() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let (entered, receiving) = std::sync::mpsc::channel();
    let writer_root = root.clone();
    let port = Arc::new(FixturePort {
        wait: true,
        entered: Some(entered),
        ..Default::default()
    });
    let writer_port = port.clone();
    let writer = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (_, adapter) = provider(vec![read("pending")]);
            let mut bounded = request(&writer_root, "active native prompt");
            bounded.wall_time = Duration::from_millis(500);
            run_pilot(
                bounded,
                &adapter,
                writer_port,
                Arc::new(Watching::default()),
            )
            .await
            .unwrap()
        })
    });
    receiving.recv_timeout(Duration::from_secs(5)).unwrap();
    let messages = pilot_messages(&root).unwrap();
    assert_eq!(messages[0]["text"], "active native prompt");
    let (model, adapter) = provider(vec![text("must not answer")]);
    let error = run_pilot(
        request(&root, "conflicting prompt"),
        &adapter,
        Arc::new(FixturePort::default()),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap_err();
    assert!(error.contains("already active"));
    assert_eq!(model.request_count(), 0);
    assert_eq!(writer.join().unwrap().status, PilotStatus::Interrupted);
    let (_, adapter) = provider(vec![text("lease released")]);
    assert_eq!(
        run_pilot(
            request(&root, "next writer"),
            &adapter,
            Arc::new(FixturePort::default()),
            Arc::new(Watching::default())
        )
        .await
        .unwrap()
        .status,
        PilotStatus::Completed
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
}
