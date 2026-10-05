use super::*;

fn final_result(summary: &str) -> MockTurn {
    MockTurn::tool_call(
        "completion",
        "final_result",
        json!({"summary": summary, "route": "next"}),
    )
}

fn resolver(root: &Path) -> Arc<FixtureResolver> {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    Arc::new(FixtureResolver {
        workspace,
        calls: Arc::new(AtomicUsize::new(0)),
        bindings: vec![],
    })
}

fn registry_port(
    root: &Path,
    models: RigModelRegistry,
    resolver: Arc<FixtureResolver>,
) -> IoHarnessNodePort<FixtureResolver> {
    IoHarnessNodePort::new_with_registry(
        root.join("facts"),
        root.join("io"),
        models,
        resolver,
        fixture_policy(),
    )
}

#[tokio::test]
async fn graph_model_aliases_select_distinct_models_and_unknown_uses_default() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let default =
        MockCompletionModel::from_turns([final_result("default"), final_result("default")]);
    let research = MockCompletionModel::from_turns([final_result("research")]);
    let review = MockCompletionModel::from_turns([final_result("review")]);
    let models = RigModelRegistry::new(default.clone().erase(), "endpoint/responses/default")
        .with_alias(
            "models.research",
            research.clone().erase(),
            "endpoint/responses/research",
        )
        .unwrap()
        .with_alias(
            "models.review",
            review.clone().erase(),
            "endpoint/responses/review",
        )
        .unwrap();
    let port = registry_port(dir.path(), models, resolver.clone());
    for (reference, summary) in [
        ("models.research", "research"),
        ("models.review", "review"),
        ("models.default", "default"),
        ("models.unconfigured", "default"),
    ] {
        let mut req = request(
            &resolver.workspace,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        req.model = Some(reference.into());
        req.key.node_id = reference.into();
        let outcome = port.execute(req).await.unwrap();
        assert!(
            matches!(outcome, NodeExecutionOutcome::Completed(completion) if completion.submission == summary)
        );
    }
    assert_eq!(default.request_count(), 2);
    assert_eq!(research.request_count(), 1);
    assert_eq!(review.request_count(), 1);
}

#[tokio::test]
async fn correction_requests_stay_on_the_selected_alias() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let default = MockCompletionModel::from_turns([]);
    let research = MockCompletionModel::from_turns([
        MockTurn::tool_call(
            "invalid",
            "final_result",
            json!({"summary":"first", "route":"unknown"}),
        ),
        final_result("corrected"),
    ]);
    let models = RigModelRegistry::new(default.clone().erase(), "endpoint/responses/default")
        .with_alias(
            "models.research",
            research.clone().erase(),
            "endpoint/responses/research",
        )
        .unwrap();
    let port = registry_port(dir.path(), models, resolver.clone());
    let mut req = request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    req.model = Some("models.research".into());
    let outcome = port.execute(req.clone()).await.unwrap();
    assert!(
        matches!(outcome, NodeExecutionOutcome::Completed(completion) if completion.submission == "corrected" && completion.model_requests == 2)
    );
    assert_eq!(research.request_count(), 2);
    assert_eq!(default.request_count(), 0);
    assert_eq!(port.provider_request_count(&req.key, 1).unwrap(), 2);
}

#[tokio::test]
async fn resumed_alias_keeps_binding_and_does_not_replay_open_tool() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let mut req = request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    req.model = Some("models.research".into());
    let initial = registry_port(
        dir.path(),
        RigModelRegistry::new(MockCompletionModel::from_turns([]).erase(), "default")
            .with_alias(
                "models.research",
                MockCompletionModel::from_turns([]).erase(),
                "endpoint/responses/research",
            )
            .unwrap(),
        resolver.clone(),
    );
    initial.pin_model(&req).unwrap();
    let binding_before = std::fs::read(initial.model_binding_path(&req.key)).unwrap();
    let store = Store::open(initial.io_run_path(&req.key)).unwrap();
    let run_id = store
        .start_run("legacy", resolver.workspace.to_str().unwrap())
        .unwrap();
    let attempt_id = store
        .open_attempt(
            run_id,
            1,
            "anchor_echo",
            io_harness::ToolRecovery::Indeterminate,
        )
        .unwrap()
        .unwrap();
    drop(store);
    initial.store_run_id(&req.key, run_id).unwrap();

    let resumed_model = MockCompletionModel::from_turns([final_result("resumed")]);
    let fallback = MockCompletionModel::from_turns([]);
    let resumed = registry_port(
        dir.path(),
        RigModelRegistry::new(
            fallback.clone().erase(),
            "changed-default-does-not-affect-alias",
        )
        .with_alias(
            "models.research",
            resumed_model.clone().erase(),
            "endpoint/responses/research",
        )
        .unwrap(),
        resolver.clone(),
    );
    resumed
        .record_recovery_decision(
            &req.key,
            attempt_id,
            RecoveryDecision::Completed {
                observation: "verified existing business result".into(),
            },
        )
        .unwrap();
    let outcome = resumed.execute(req.clone()).await.unwrap();
    assert!(
        matches!(outcome, NodeExecutionOutcome::Completed(completion) if completion.submission == "resumed")
    );
    assert_eq!(
        std::fs::read(resumed.model_binding_path(&req.key)).unwrap(),
        binding_before
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    assert_eq!(resumed_model.request_count(), 1);
    assert_eq!(fallback.request_count(), 0);
}

#[tokio::test]
async fn model_drift_is_rejected_before_any_provider_request() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let req = request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let original = registry_port(
        dir.path(),
        RigModelRegistry::new(
            MockCompletionModel::from_turns([]).erase(),
            "original-model",
        ),
        resolver.clone(),
    );
    assert!(original.execute(req.clone()).await.is_err());
    let fingerprint = std::fs::read(original.model_binding_path(&req.key)).unwrap();

    let changed = MockCompletionModel::from_turns([final_result("must not run")]);
    let reopened = registry_port(
        dir.path(),
        RigModelRegistry::new(changed.clone().erase(), "changed-model"),
        resolver,
    );
    let error = reopened
        .validate_model_binding(&req.key, req.model.as_deref())
        .unwrap_err();
    assert!(error.to_string().contains("model binding changed"));
    assert_eq!(changed.request_count(), 0);
    assert!(!reopened.failed_path(&req.key).exists());
    // Restoring the original binding remains possible after a failed preflight;
    // neither the invocation nor its pinned fact was rewritten.
    original
        .validate_model_binding(&req.key, req.model.as_deref())
        .unwrap();
    let error = reopened.execute(req.clone()).await.unwrap_err();
    assert!(error.to_string().contains("model binding changed"));
    assert_eq!(changed.request_count(), 0);
    assert_eq!(
        std::fs::read(reopened.model_binding_path(&req.key)).unwrap(),
        fingerprint
    );
}

#[tokio::test]
async fn old_unfinished_store_first_binds_current_model_and_preserves_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let model = MockCompletionModel::from_turns([final_result("legacy resumed")]);
    let calls = Arc::new(AtomicUsize::new(0));
    let (port, req, attempt_id) = seed_recovery(dir.path(), model.clone().erase(), calls.clone());
    std::fs::remove_file(port.model_binding_path(&req.key)).unwrap();
    port.validate_model_binding(&req.key, req.model.as_deref())
        .unwrap();
    assert!(!port.model_binding_path(&req.key).exists());
    assert_eq!(model.request_count(), 0);
    let run_id = port.stored_run_id(&req.key).unwrap().unwrap();
    let store = Store::open(port.io_run_path(&req.key)).unwrap();
    store
        .record_provider_call(
            run_id,
            &io_harness::ProviderCall {
                step: 1,
                provider: "gateway".into(),
                model: Some("normalized-response-model".into()),
                ..Default::default()
            },
        )
        .unwrap();
    drop(store);
    port.record_recovery_decision(
        &req.key,
        attempt_id,
        RecoveryDecision::Completed {
            observation: "verified legacy business result".into(),
        },
    )
    .unwrap();
    let outcome = port.execute(req.clone()).await.unwrap();
    assert!(
        matches!(outcome, NodeExecutionOutcome::Completed(completion) if completion.submission == "legacy resumed")
    );
    assert_eq!(model.request_count(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let binding: serde_json::Value =
        serde_json::from_slice(&std::fs::read(port.model_binding_path(&req.key)).unwrap()).unwrap();
    assert_eq!(binding["binding_origin"], "legacy_first_binding");
}

#[test]
fn old_store_response_model_does_not_prove_a_different_request_binding() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let models = RigModelRegistry::new(
        anchor_runtime_rig::RigCompletionPort::openai_compatible(
            "secret-not-persisted",
            "http://127.0.0.1:1/v1",
            "current-wire-model",
            "chat",
        )
        .unwrap()
        .dyn_model(),
        "endpoint/chat/current-wire-model",
    );
    let port = registry_port(dir.path(), models, resolver.clone());
    let req = request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    std::fs::create_dir_all(&port.io_store_root).unwrap();
    let store = Store::open(port.io_run_path(&req.key)).unwrap();
    let run_id = store
        .start_run("legacy", resolver.workspace.to_str().unwrap())
        .unwrap();
    store
        .record_provider_call(
            run_id,
            &io_harness::ProviderCall {
                step: 1,
                provider: "openai".into(),
                model: Some("previous-wire-model".into()),
                ..Default::default()
            },
        )
        .unwrap();
    drop(store);
    port.pin_model(&req).unwrap();
    let bytes = std::fs::read(port.model_binding_path(&req.key)).unwrap();
    let binding: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(binding["binding_origin"], "legacy_first_binding");
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains("secret-not-persisted")
    );
}

#[tokio::test]
async fn completed_fact_precedes_new_model_configuration_and_keeps_history_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let req = request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let original = registry_port(
        dir.path(),
        RigModelRegistry::new(
            MockCompletionModel::from_turns([final_result("saved")]).erase(),
            "original",
        ),
        resolver.clone(),
    );
    let before = original.execute(req.clone()).await.unwrap();
    let fact = std::fs::read(original.completion_path(&req.key)).unwrap();
    let binding = std::fs::read(original.model_binding_path(&req.key)).unwrap();
    let changed = MockCompletionModel::from_turns([]);
    let reopened = registry_port(
        dir.path(),
        RigModelRegistry::new(changed.clone().erase(), "changed"),
        resolver,
    );
    reopened
        .validate_model_binding(&req.key, req.model.as_deref())
        .unwrap();
    let after = reopened.execute(req.clone()).await.unwrap();
    assert_eq!(after, before);
    assert_eq!(changed.request_count(), 0);
    assert_eq!(
        std::fs::read(reopened.completion_path(&req.key)).unwrap(),
        fact
    );
    assert_eq!(
        std::fs::read(reopened.model_binding_path(&req.key)).unwrap(),
        binding
    );
    // Completed invocations written before model pinning also return directly.
    std::fs::remove_file(reopened.model_binding_path(&req.key)).unwrap();
    reopened
        .validate_model_binding(&req.key, req.model.as_deref())
        .unwrap();
    assert_eq!(reopened.execute(req.clone()).await.unwrap(), before);
    assert_eq!(changed.request_count(), 0);
    assert!(!reopened.model_binding_path(&req.key).exists());
    assert_eq!(
        std::fs::read(reopened.completion_path(&req.key)).unwrap(),
        fact
    );
}

#[test]
fn model_preflight_without_existing_facts_does_not_create_state() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let model = MockCompletionModel::from_turns([]);
    let port = registry_port(
        dir.path(),
        RigModelRegistry::new(model.clone().erase(), "test"),
        resolver.clone(),
    );
    let req = request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    port.validate_model_binding(&req.key, req.model.as_deref())
        .unwrap();
    assert!(!port.facts_root.exists());
    assert!(!port.io_store_root.exists());
    assert_eq!(model.request_count(), 0);
}

#[tokio::test]
async fn provider_budget_stays_explicitly_unsupported_and_sends_zero_requests() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path());
    let model = MockCompletionModel::from_turns([]);
    let port = registry_port(
        dir.path(),
        RigModelRegistry::new(model.clone().erase(), "test"),
        resolver.clone(),
    );
    let mut req = request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    req.max_provider_requests = Some(2);
    assert!(!port.capabilities().exact_provider_request_budget);
    let outcome = port.execute(req.clone()).await.unwrap();
    assert!(
        matches!(outcome, NodeExecutionOutcome::Failed { reason } if reason.contains("no durable pre-call request limit"))
    );
    assert_eq!(model.request_count(), 0);
    assert!(!port.io_run_path(&req.key).exists());
}
