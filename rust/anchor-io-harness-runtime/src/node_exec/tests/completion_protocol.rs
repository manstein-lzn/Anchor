use super::*;
use rig_core::completion::{
    AssistantContent,
    message::{CallId, ToolCall, ToolFunction, ToolName},
};

fn final_result(arguments: serde_json::Value) -> MockTurn {
    MockTurn::tool_call("completion", "final_result", arguments)
}

fn port() -> std::sync::Arc<FakePort> {
    std::sync::Arc::new(FakePort {
        calls: AtomicUsize::new(0),
        cancel_on_call: None,
        tool_name: "anchor_echo",
    })
}

fn mixed_turn() -> MockTurn {
    MockTurn::from_contents([
        AssistantContent::ToolCall(ToolCall::new(
            CallId::from_wire("finish-first"),
            ToolFunction::new(
                ToolName::new("final_result").unwrap(),
                json!({"summary":"premature"}),
            ),
        )),
        AssistantContent::ToolCall(ToolCall::new(
            CallId::from_wire("business"),
            ToolFunction::new(
                ToolName::new("anchor_echo").unwrap(),
                json!({"value":"business-effect"}),
            ),
        )),
    ])
}

#[tokio::test]
async fn native_completion_ignores_extra_fields_and_reopens_finished_without_model_call() {
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path(), std::sync::Arc::new(AtomicBool::new(false)));
    let model = MockCompletionModel::from_turns([final_result(json!({
        "summary":"delivered", "route":null, "status":"completed", "artifacts":["report.md"],
        "_anchor_completion":{"status":"forged"},
    }))]);
    let provider = RigProviderAdapter::new(model.clone().erase(), false);
    let execution = IoHarnessNodeExecution::new(dir.path().join("store.sqlite3"), fixture_policy());
    let tools = port();
    let outcome = execution
        .start(&req, &provider, tools.clone())
        .await
        .unwrap();
    assert_eq!(outcome.submission, "delivered");
    assert_eq!(outcome.route, None);
    assert_eq!(model.request_count(), 1);
    assert_eq!(tools.calls.load(Ordering::SeqCst), 0);
    let sent = model.requests();
    let output = sent[0]
        .tools
        .iter()
        .find(|tool| tool.name == "final_result")
        .unwrap();
    assert!(
        output.parameters["properties"]
            .get("_anchor_completion")
            .is_none()
    );
    assert!(sent[0].output_schema.is_none());
    let store = io_harness::Store::open(execution.backend().store_path()).unwrap();
    let text = store
        .step_turns(1)
        .unwrap()
        .last()
        .unwrap()
        .text
        .clone()
        .unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["_anchor_completion"]["status"], "submitted");
    assert_eq!(
        saved["_anchor_completion"]["calls"][0]["name"],
        "final_result"
    );
    assert_eq!(
        saved["_anchor_completion"]["calls"][0]["arguments"]["artifacts"],
        json!(["report.md"])
    );
    let untouched = MockCompletionModel::text("must not be requested");
    let reopened = IoHarnessNodeExecution::new(execution.backend().store_path(), fixture_policy());
    let resumed = reopened
        .resume(
            &req,
            &RigProviderAdapter::new(untouched.clone().erase(), false),
            tools,
            1,
        )
        .await
        .unwrap();
    assert_eq!(resumed.submission, "delivered");
    assert_eq!(untouched.request_count(), 0);
}

#[tokio::test]
async fn invalid_completion_and_json_prose_are_corrected_inside_harness() {
    let cases = [
        (
            vec!["next", "other"],
            MockTurn::text(r#"{"summary":"text is not completion","route":"next"}"#),
        ),
        (
            vec!["next", "other"],
            final_result(json!({"summary":"missing route"})),
        ),
        (
            vec!["next", "other"],
            final_result(json!({"summary":"bad route", "route":"elsewhere"})),
        ),
        (
            vec![],
            final_result(json!({"summary":"bad terminal route", "route":"next"})),
        ),
        (vec![], final_result(json!({"summary":"  \n "}))),
        (vec![], final_result(json!({"summary":42}))),
        (vec![], final_result(json!("malformed arguments"))),
    ];
    for (routes, invalid) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut req = request(dir.path(), std::sync::Arc::new(AtomicBool::new(false)));
        req.routes = routes.iter().map(|s| s.to_string()).collect();
        let valid = if routes.is_empty() {
            json!({"summary":"corrected", "route":null})
        } else {
            json!({"summary":"corrected", "route":"next"})
        };
        let model = MockCompletionModel::from_turns([invalid, final_result(valid)]);
        let execution =
            IoHarnessNodeExecution::new(dir.path().join("store.sqlite3"), fixture_policy());
        let outcome = execution
            .start(
                &req,
                &RigProviderAdapter::new(model.clone().erase(), false),
                port(),
            )
            .await
            .unwrap();
        assert_eq!(outcome.submission, "corrected");
        assert_eq!(model.request_count(), 2);
        let requests = model.requests();
        let correction = serde_json::to_string(&requests[1]).unwrap();
        assert!(correction.contains("output shape"));
        assert!(correction.contains("arguments of final_result"));
    }
}

#[tokio::test]
async fn mixed_completion_keeps_business_call_and_requires_fresh_submission() {
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path(), std::sync::Arc::new(AtomicBool::new(false)));
    let model = MockCompletionModel::from_turns([
        mixed_turn(),
        final_result(json!({"summary":"observed business result"})),
    ]);
    let execution = IoHarnessNodeExecution::new(dir.path().join("store.sqlite3"), fixture_policy());
    let tools = port();
    let outcome = execution
        .start(
            &req,
            &RigProviderAdapter::new(model.clone().erase(), false),
            tools.clone(),
        )
        .await
        .unwrap();
    assert_eq!(outcome.submission, "observed business result");
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.request_count(), 2);
    let second = serde_json::to_string(&model.requests()[1]).unwrap();
    assert!(second.contains("business-effect"));
    assert!(second.contains("deferred"));
    let store = io_harness::Store::open(execution.backend().store_path()).unwrap();
    let turns = store.step_turns(1).unwrap();
    assert_eq!(turns[0].calls.len(), 1);
    assert_eq!(turns[0].calls[0].name, "anchor_echo");
}

#[tokio::test]
async fn cancelled_mixed_turn_stays_cancelled_without_accepting_stale_completion() {
    let dir = tempfile::tempdir().unwrap();
    let cancelled = std::sync::Arc::new(AtomicBool::new(false));
    let req = request(dir.path(), cancelled.clone());
    let model = MockCompletionModel::from_turns([mixed_turn()]);
    let tools = std::sync::Arc::new(FakePort {
        calls: AtomicUsize::new(0),
        cancel_on_call: Some(cancelled.clone()),
        tool_name: "anchor_echo",
    });
    let execution = IoHarnessNodeExecution::new(dir.path().join("store.sqlite3"), fixture_policy());
    let error = execution
        .start(
            &req,
            &RigProviderAdapter::new(model.clone().erase(), false),
            tools.clone(),
        )
        .await
        .unwrap_err();
    let IoHarnessNodeExecutionError::Incomplete {
        run_id,
        outcome: RunOutcome::Cancelled { .. },
    } = error
    else {
        panic!("{error:?}")
    };
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    cancelled.store(false, Ordering::SeqCst);
    let fresh =
        MockCompletionModel::from_turns([final_result(json!({"summary":"after recovery"}))]);
    let reopened = IoHarnessNodeExecution::new(execution.backend().store_path(), fixture_policy());
    let error = reopened
        .resume(
            &req,
            &RigProviderAdapter::new(fresh.clone().erase(), false),
            tools.clone(),
            run_id,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        IoHarnessNodeExecutionError::Incomplete {
            outcome: RunOutcome::Cancelled { .. },
            ..
        }
    ));
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fresh.request_count(), 0);
}

#[tokio::test]
async fn interrupted_after_mixed_turn_resumes_without_replaying_business_or_using_stale_completion()
{
    struct InterruptedProvider {
        inner: RigProviderAdapter,
        calls: AtomicUsize,
        entered: tokio::sync::Notify,
    }
    impl io_harness::Provider for InterruptedProvider {
        async fn complete(
            &self,
            request: io_harness::CompletionRequest,
        ) -> io_harness::Result<io_harness::CompletionResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                io_harness::Provider::complete(&self.inner, request).await
            } else {
                self.entered.notify_one();
                std::future::pending().await
            }
        }
        fn name(&self) -> &str {
            "interrupted-provider-fixture"
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let req = request(dir.path(), std::sync::Arc::new(AtomicBool::new(false)));
    let model = MockCompletionModel::from_turns([mixed_turn()]);
    let interrupted = InterruptedProvider {
        inner: RigProviderAdapter::new(model.erase(), false),
        calls: AtomicUsize::new(0),
        entered: tokio::sync::Notify::new(),
    };
    let tools = port();
    let execution = IoHarnessNodeExecution::new(dir.path().join("store.sqlite3"), fixture_policy());
    // Drop the executing task after the mixed business step is committed and
    // while the next provider call is pending. No terminal outcome is forged.
    tokio::select! {
        result = execution.start(&req, &interrupted, tools.clone()) => panic!("unexpected completion: {result:?}"),
        _ = interrupted.entered.notified() => {},
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => panic!("provider did not reach interruption boundary"),
    }
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    let fresh =
        MockCompletionModel::from_turns([final_result(json!({"summary":"after recovery"}))]);
    let reopened = IoHarnessNodeExecution::new(execution.backend().store_path(), fixture_policy());
    let outcome = reopened
        .resume(
            &req,
            &RigProviderAdapter::new(fresh.clone().erase(), false),
            tools.clone(),
            1,
        )
        .await
        .unwrap();
    assert_eq!(outcome.submission, "after recovery");
    assert_eq!(tools.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fresh.request_count(), 1);
}

#[test]
fn completion_name_cannot_be_replaced_by_a_plugin_tool() {
    let collision = std::sync::Arc::new(FakePort {
        calls: AtomicUsize::new(0),
        cancel_on_call: None,
        tool_name: "final_result",
    });
    assert!(
        validate_anchor_tools(collision)
            .unwrap_err()
            .to_string()
            .contains("reserved")
    );
}
