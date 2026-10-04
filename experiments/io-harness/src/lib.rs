//! Provider-free feasibility evidence for the io-harness 0.86.0 boundary.
//!
//! The fake provider and tools deliberately model the narrow adapter that a
//! future Anchor/Rig integration would own. No network provider is used.

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use io_harness::provider::{CompletionRequest, CompletionResponse, ToolCall, Usage};
    use io_harness::tools::{Tool, ToolEffect, ToolFuture, ToolRecovery, Toolbox};
    use io_harness::{
        ApproveAll, Compaction, ContextBudget, Media, Policy, Provider, RecoveryDecision,
        RunOutcome, Store, TaskContract, ToolSpec, resume, resume_with, run_with,
    };
    use serde_json::json;

    fn policy() -> Policy {
        Policy::default()
            .layer("fake-anchor-boundary")
            .allow_read("*")
            .allow_write("*")
            .allow_exec("*")
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            name: name.to_owned(),
            arguments,
        }
    }

    struct FakeAnchorTool {
        name: &'static str,
        calls: Arc<AtomicUsize>,
        recovery: ToolRecovery,
        effect: ToolEffect,
        result: &'static str,
        padding: usize,
        started: Option<Arc<AtomicUsize>>,
        block: bool,
    }

    impl Tool for FakeAnchorTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "Fake Anchor Tool adapter for a JSON payload.".into(),
                parameters: json!({
                    "type": "object",
                    "properties": { "index": { "type": "integer" } }
                }),
            }
        }

        fn invoke<'a>(&'a self, _arguments: &'a serde_json::Value) -> ToolFuture<'a> {
            let calls = Arc::clone(&self.calls);
            let started = self.started.clone();
            let block = self.block;
            let result = self.result.to_owned();
            let padding = self.padding;
            Box::pin(async move {
                if let Some(started) = started {
                    started.store(1, Ordering::SeqCst);
                }
                calls.fetch_add(1, Ordering::SeqCst);
                if block {
                    std::future::pending::<()>().await;
                }
                Ok(format!("{result}{}", "x".repeat(padding)))
            })
        }

        fn effect(&self) -> ToolEffect {
            self.effect
        }

        fn recovery(&self) -> ToolRecovery {
            self.recovery
        }
    }

    /// A script with one ordinary tool call per turn. Fold requests are
    /// identified by io-harness's documented compaction marker and answer with
    /// text rather than consuming a tool-call slot.
    struct LongScript {
        at: AtomicUsize,
        seen: Arc<Mutex<Vec<CompletionRequest>>>,
        calls: usize,
    }

    impl Provider for LongScript {
        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> io_harness::Result<CompletionResponse> {
            let is_fold = request
                .system
                .contains("compacting an agent's own working notes")
                || request
                    .user
                    .contains("compacting an agent's own working notes");
            self.seen.lock().unwrap().push(request);
            let usage = Some(Usage {
                prompt_tokens: 40,
                completion_tokens: 8,
                total_tokens: 48,
                ..Default::default()
            });
            if is_fold {
                return Ok(CompletionResponse {
                    text: Some("summary: fake Anchor calls were folded".into()),
                    usage,
                    ..Default::default()
                });
            }
            let index = self.at.fetch_add(1, Ordering::SeqCst);
            if index < self.calls {
                return Ok(CompletionResponse {
                    tool_calls: vec![call("anchor_read", json!({ "index": index }))],
                    usage,
                    ..Default::default()
                });
            }
            Ok(CompletionResponse {
                text: Some("all scripted Anchor calls observed".into()),
                usage,
                ..Default::default()
            })
        }

        fn name(&self) -> &str {
            "scripted-provider"
        }
    }

    #[tokio::test]
    async fn thirty_tool_calls_trigger_compaction_and_finish() {
        let workspace = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = LongScript {
            at: AtomicUsize::new(0),
            seen: Arc::new(Mutex::new(Vec::new())),
            calls: 30,
        };
        let contract =
            TaskContract::workspace("exercise the Anchor tool boundary", workspace.path())
                .with_tools(Toolbox::new().with(FakeAnchorTool {
                    name: "anchor_read",
                    calls: Arc::clone(&calls),
                    recovery: ToolRecovery::Replayable,
                    effect: ToolEffect::ReadOnly,
                    result: "{\"ok\":true}",
                    padding: 1800,
                    started: None,
                    block: false,
                }))
                .with_max_steps(40)
                .with_context_budget(ContextBudget {
                    max_tokens: 700,
                    share: 0.5,
                })
                .with_compaction(Compaction {
                    at_share: 0.45,
                    keep_recent: 2,
                });
        let store = Store::memory().unwrap();
        let result = run_with(&contract, &provider, &store, &policy(), &ApproveAll)
            .await
            .unwrap();

        assert!(matches!(result.outcome, RunOutcome::Finished { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 30);
        let summaries = store.summaries(result.run_id).unwrap();
        assert!(
            !summaries.is_empty(),
            "the long transcript must have folded"
        );
        assert!(provider.seen.lock().unwrap().iter().any(|r| {
            r.system.contains("compacting an agent's own working notes")
                || r.user.contains("compacting an agent's own working notes")
        }));
        assert!(store.steps(result.run_id).unwrap().len() >= 30);
    }

    struct ResumeScript {
        calls: Arc<AtomicUsize>,
    }

    impl Provider for ResumeScript {
        async fn complete(
            &self,
            request: CompletionRequest,
        ) -> io_harness::Result<CompletionResponse> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n == 0 && request.messages.is_empty() {
                return Ok(CompletionResponse {
                    tool_calls: vec![call("anchor_read", json!({ "index": 1 }))],
                    ..Default::default()
                });
            }
            Ok(CompletionResponse {
                text: Some("resumed from the durable checkpoint".into()),
                ..Default::default()
            })
        }
    }

    fn resume_contract(root: &Path, max_steps: u32) -> TaskContract {
        TaskContract::workspace("checkpoint then resume", root)
            .with_tools(Toolbox::new().with(FakeAnchorTool {
                name: "anchor_read",
                calls: Arc::new(AtomicUsize::new(0)),
                recovery: ToolRecovery::Replayable,
                effect: ToolEffect::ReadOnly,
                result: "checkpointed-result",
                padding: 0,
                started: None,
                block: false,
            }))
            .with_max_steps(max_steps)
    }

    #[tokio::test]
    async fn sqlite_checkpoint_reopens_and_resume_does_not_repeat_step() {
        let workspace = tempfile::tempdir().unwrap();
        let db = workspace.path().join("trace.sqlite3");
        let first_provider = ResumeScript {
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let first_store = Store::open(&db).unwrap();
        let first = run_with(
            &resume_contract(workspace.path(), 1),
            &first_provider,
            &first_store,
            &policy(),
            &ApproveAll,
        )
        .await
        .unwrap();
        assert!(matches!(
            first.outcome,
            RunOutcome::StepCapReached { steps: 1 }
        ));
        drop(first_store);

        let second_provider = ResumeScript {
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let reopened = Store::open(&db).unwrap();
        let resumed = resume_with(
            &resume_contract(workspace.path(), 2),
            &second_provider,
            &reopened,
            first.run_id,
            &policy(),
            &ApproveAll,
        )
        .await
        .unwrap();
        assert!(matches!(resumed.outcome, RunOutcome::Finished { .. }));
        assert_eq!(reopened.steps(first.run_id).unwrap().len(), 2);
        assert_eq!(second_provider.calls.load(Ordering::SeqCst), 1);
    }

    struct OneCallProvider {
        calls: AtomicUsize,
    }

    impl Provider for OneCallProvider {
        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> io_harness::Result<CompletionResponse> {
            if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
                return Ok(CompletionResponse {
                    text: Some("recovery observation accepted".into()),
                    ..Default::default()
                });
            }
            Ok(CompletionResponse {
                tool_calls: vec![call("anchor_unknown", json!({ "index": 9 }))],
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn unknown_effect_tool_pauses_for_recovery() {
        let workspace = tempfile::tempdir().unwrap();
        let db = workspace.path().join("recovery.sqlite3");
        let started = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let contract =
            TaskContract::workspace("exercise an indeterminate effect", workspace.path())
                .with_tools(Toolbox::new().with(FakeAnchorTool {
                    name: "anchor_unknown",
                    calls: Arc::clone(&calls),
                    recovery: ToolRecovery::Indeterminate,
                    effect: ToolEffect::Mutating,
                    result: "never returned",
                    padding: 0,
                    started: Some(Arc::clone(&started)),
                    block: true,
                }))
                .with_max_steps(3);
        let store = Store::open(&db).unwrap();
        let provider = OneCallProvider {
            calls: AtomicUsize::new(0),
        };
        let run_policy = policy();
        let mut running = Box::pin(run_with(
            &contract,
            &provider,
            &store,
            &run_policy,
            &ApproveAll,
        ));
        for _ in 0..10_000 {
            tokio::select! {
                result = &mut running => {
                    panic!("the interrupted tool unexpectedly completed: {:?}", result);
                }
                _ = tokio::task::yield_now() => {
                    if started.load(Ordering::SeqCst) == 1 {
                        break;
                    }
                }
            }
        }
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "tool call must have started"
        );
        drop(running);

        let resumed = resume(&contract, &provider, &store, 1).await.unwrap();
        let attempt_id = match resumed.outcome {
            RunOutcome::AwaitingRecovery { attempt_id, .. } => attempt_id,
            other => panic!("expected AwaitingRecovery, got {other:?}"),
        };
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "resume must not replay an unknown effect"
        );
        let completed = io_harness::resume_with_recovery(
            &contract,
            &provider,
            &store,
            1,
            attempt_id,
            RecoveryDecision::Completed {
                observation: "confirmed by Anchor operator".into(),
            },
            &policy(),
            &ApproveAll,
        )
        .await
        .unwrap();
        assert!(!matches!(
            completed.outcome,
            RunOutcome::AwaitingRecovery { .. }
        ));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "recovery confirmation must not invoke the tool again"
        );
    }

    #[tokio::test]
    async fn read_only_effect_is_replayable_and_can_complete() {
        let workspace = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = LongScript {
            at: AtomicUsize::new(0),
            seen: Arc::new(Mutex::new(Vec::new())),
            calls: 1,
        };
        let contract = TaskContract::workspace("read only fake Anchor data", workspace.path())
            .with_tools(Toolbox::new().with(FakeAnchorTool {
                name: "anchor_read",
                calls: Arc::clone(&calls),
                recovery: ToolRecovery::Replayable,
                effect: ToolEffect::ReadOnly,
                result: "read-only-json",
                padding: 0,
                started: None,
                block: false,
            }))
            .with_max_steps(3);
        let result = run_with(
            &contract,
            &provider,
            &Store::memory().unwrap(),
            &policy(),
            &ApproveAll,
        )
        .await
        .unwrap();
        assert!(matches!(result.outcome, RunOutcome::Finished { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    struct StreamScript;

    impl Provider for StreamScript {
        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> io_harness::Result<CompletionResponse> {
            Ok(CompletionResponse {
                text: Some("fallback".into()),
                ..Default::default()
            })
        }

        async fn complete_streaming(
            &self,
            _request: CompletionRequest,
            on_token: &(dyn Fn(&str) + Send + Sync),
        ) -> io_harness::Result<CompletionResponse> {
            for chunk in ["Anchor ", "stream ", "capture"] {
                on_token(chunk);
                tokio::task::yield_now().await;
            }
            Ok(CompletionResponse {
                text: Some("Anchor stream capture".into()),
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn image_fixture_and_streaming_capture_cross_provider_boundary() {
        // A complete 1×1 RGBA PNG. `Media::image` validates the media type and
        // byte bound; decoding/vision quality remains a provider concern.
        let png = [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 31, 0, 3, 3, 1, 0, 24, 221, 141, 181, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66,
            96, 130,
        ];
        let image = Media::image("image/png", &png).unwrap();
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.byte_len(), png.len());
        assert_eq!(Media::media_type_for("fixture.png"), Some("image/png"));

        let seen = Arc::new(Mutex::new(String::new()));
        let captured = Arc::clone(&seen);
        let response = StreamScript
            .complete_streaming(CompletionRequest::default(), &move |chunk| {
                captured.lock().unwrap().push_str(chunk)
            })
            .await
            .unwrap();
        assert_eq!(
            seen.lock().unwrap().as_str(),
            response.text.as_deref().unwrap()
        );
    }
}
