use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use rig_core::message::{DocumentSourceKind, ImageMediaType, Message, UserContent};
use rig_core::test_utils::MockStreamEvent;

struct ImageResolver {
    workspace: PathBuf,
    calls: Arc<AtomicUsize>,
    images: Vec<NodeImage>,
    conversation: bool,
}

impl NodeHostResolver for ImageResolver {
    fn prompt_images(&self, _request: &NodeExecutionRequest) -> Result<Vec<NodeImage>, String> {
        Ok(self.images.clone())
    }

    fn conversation_hint(
        &self,
        _request: &NodeExecutionRequest,
    ) -> Result<Option<NodeConversationHint>, String> {
        Ok(self.conversation.then(|| NodeConversationHint {
            key: "image-session/graph/agent".into(),
        }))
    }

    fn resolve_plugins(&self, _ids: &[String]) -> Result<Vec<PluginBinding>, String> {
        Ok(vec![])
    }

    fn workspace(&self, _request: &NodeExecutionRequest) -> Result<PathBuf, String> {
        Ok(self.workspace.clone())
    }

    fn tools<'a>(&'a self, _request: &'a NodeExecutionRequest) -> ToolResolution<'a> {
        Box::pin(async move {
            Ok(Arc::new(EchoPort {
                calls: self.calls.clone(),
            }) as Arc<dyn ToolPort>)
        })
    }
}

fn image() -> NodeImage {
    NodeImage {
        data: b"immutable-png-input".to_vec(),
        media_type: "image/png".into(),
    }
}

fn resolver(root: &Path, images: Vec<NodeImage>, conversation: bool) -> Arc<ImageResolver> {
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    Arc::new(ImageResolver {
        workspace,
        calls: Arc::new(AtomicUsize::new(0)),
        images,
        conversation,
    })
}

fn image_port(
    root: &Path,
    model: rig_core::DynModel<Completion>,
    resolver: Arc<ImageResolver>,
    accepts_images: bool,
) -> IoHarnessNodePort<ImageResolver> {
    IoHarnessNodePort::new_with_registry(
        root.join("facts"),
        root.join("io"),
        RigModelRegistry::new_with_image_capability(
            model,
            "endpoint/chat/vision-fixture",
            accepts_images,
        ),
        resolver,
        fixture_policy(),
    )
}

fn final_result(summary: &str) -> MockTurn {
    MockTurn::tool_call(
        "completion",
        "final_result",
        json!({"summary":summary,"route":"next"}),
    )
}

fn stream_final_result(summary: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::tool_call(
            "completion",
            "final_result",
            json!({"summary":summary,"route":"next"}),
        ),
        MockStreamEvent::final_response_with_default_usage(),
    ]
}

fn assert_image(request: &rig_core::completion::CompletionRequest, expected: &NodeImage) {
    let images = request
        .chat_history
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(content),
            _ => None,
        })
        .flat_map(|content| content.iter())
        .filter_map(|part| match part {
            UserContent::Image(image) => Some(image),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(images.len(), 1);
    assert_eq!(images[0].media_type, Some(ImageMediaType::PNG));
    assert_eq!(
        images[0].data,
        DocumentSourceKind::Base64(STANDARD.encode(&expected.data))
    );
}

fn node_request(resolver: &ImageResolver) -> NodeExecutionRequest {
    request(
        &resolver.workspace,
        Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

#[tokio::test]
async fn every_provider_turn_and_native_record_carries_the_frozen_image_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), vec![image()], false);
    let model = MockCompletionModel::from_turns([
        MockTurn::tool_call(
            "business",
            "anchor_echo",
            json!({"value":"inspected-image"}),
        ),
        final_result("image-result"),
    ]);
    let port = image_port(dir.path(), model.clone().erase(), resolver.clone(), true);
    let req = node_request(&resolver);
    assert!(matches!(
        port.execute(req.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert_eq!(model.request_count(), 2);
    for sent in model.requests() {
        assert_image(&sent, &image());
    }
    let expected =
        serde_json::to_value(io_harness::Media::image("image/png", &image().data).unwrap())
            .unwrap();
    for sequence in 1..=2 {
        let path = crate::recording::directory(&port.io_store_root, &req.key)
            .join(format!("{sequence:020}/request.json"));
        let recorded: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(recorded["media"], json!([expected]));
    }
    let binding: serde_json::Value =
        serde_json::from_slice(&std::fs::read(port.model_binding_path(&req.key)).unwrap()).unwrap();
    assert_eq!(binding["accepts_images"], true);
}

#[tokio::test]
async fn native_session_turns_receive_current_images_without_changing_completion_protocol() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), vec![image()], true);
    let model = MockCompletionModel::from_stream_turns([
        stream_final_result("first"),
        stream_final_result("second"),
    ]);
    let port = image_port(dir.path(), model.clone().erase(), resolver.clone(), true);
    let first = node_request(&resolver);
    let mut second = first.clone();
    second.key.run_id = "run-2".into();
    assert!(matches!(
        port.execute(first.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert!(matches!(
        port.execute(second).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    for sent in model.requests() {
        assert_image(&sent, &image());
    }
    let paths =
        ConversationPaths::new(&port.io_store_root, "image-session/graph/agent", &first.key);
    let (store, session) = paths.open_or_create().unwrap();
    assert_eq!(store.session_turns(session.id()).unwrap().len(), 2);
    assert_eq!(model.request_count(), 2);
}

#[tokio::test]
async fn image_input_without_explicit_capability_fails_before_model_or_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(dir.path(), vec![image()], false);
    let model = MockCompletionModel::from_turns([final_result("must-not-run")]);
    let port = image_port(dir.path(), model.clone().erase(), resolver.clone(), false);
    let req = node_request(&resolver);
    assert!(
        matches!(port.execute(req.clone()).await.unwrap(), NodeExecutionOutcome::Failed { reason } if reason.contains("does not accept images"))
    );
    assert_eq!(model.request_count(), 0);
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    assert!(!port.io_run_path(&req.key).exists());
    assert!(!port.started_path(&req.key).exists());
}

#[tokio::test]
async fn unsupported_image_mime_is_rejected_before_provider_call() {
    let dir = tempfile::tempdir().unwrap();
    let resolver = resolver(
        dir.path(),
        vec![NodeImage {
            data: b"svg".to_vec(),
            media_type: "image/svg+xml".into(),
        }],
        false,
    );
    let model = MockCompletionModel::from_turns([final_result("must-not-run")]);
    let port = image_port(dir.path(), model.clone().erase(), resolver.clone(), true);
    let req = node_request(&resolver);
    assert!(
        matches!(port.execute(req.clone()).await.unwrap(), NodeExecutionOutcome::Failed { reason } if reason.contains("unsupported image media type"))
    );
    assert_eq!(model.request_count(), 0);
    assert!(!port.started_path(&req.key).exists());
}

#[tokio::test]
async fn image_capability_is_part_of_frozen_model_binding_in_both_directions() {
    for original in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let resolver = resolver(dir.path(), vec![], false);
        let model = MockCompletionModel::from_turns([]);
        let port = image_port(dir.path(), model.erase(), resolver.clone(), original);
        let req = node_request(&resolver);
        port.pin_model(&req).unwrap();
        let changed_model = MockCompletionModel::from_turns([final_result("must-not-run")]);
        let changed = image_port(
            dir.path(),
            changed_model.clone().erase(),
            resolver,
            !original,
        );
        assert!(
            changed
                .validate_model_binding(&req.key, req.model.as_deref())
                .unwrap_err()
                .to_string()
                .contains("model binding changed")
        );
        assert!(
            changed
                .execute(req)
                .await
                .unwrap_err()
                .to_string()
                .contains("model binding changed")
        );
        assert_eq!(changed_model.request_count(), 0);
    }
}

#[tokio::test]
async fn legacy_binding_without_image_field_remains_text_only_and_resumes_old_history() {
    let dir = tempfile::tempdir().unwrap();
    let model = MockCompletionModel::from_turns([final_result("legacy-image-free-resume")]);
    let (port, req, attempt_id) = seed_recovery(
        dir.path(),
        model.clone().erase(),
        Arc::new(AtomicUsize::new(0)),
    );
    let path = port.model_binding_path(&req.key);
    let mut fact: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    fact.as_object_mut().unwrap().remove("accepts_images");
    IoHarnessNodePort::<FixtureResolver>::write_atomic(&path, &serde_json::to_vec(&fact).unwrap())
        .unwrap();
    port.validate_model_binding(&req.key, req.model.as_deref())
        .unwrap();
    port.record_recovery_decision(
        &req.key,
        attempt_id,
        RecoveryDecision::Completed {
            observation: "verified legacy result".into(),
        },
    )
    .unwrap();
    assert!(matches!(
        port.execute(req).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert_eq!(model.request_count(), 1);
    assert!(
        !serde_json::to_value(model.requests())
            .unwrap()
            .to_string()
            .contains("media_type")
    );
}

#[tokio::test]
async fn exact_run_reopen_keeps_images_and_does_not_replay_committed_tool() {
    use rig_core::driver::{Exchange, Model, Opening, Transport};
    use rig_core::test_utils::{MockFrame, MockRuntime, MockScript};

    #[derive(Clone)]
    struct InterruptSecond {
        inner: MockRuntime,
        sends: Arc<AtomicUsize>,
        entered: Arc<tokio::sync::Notify>,
    }
    impl Transport<MockScript> for InterruptSecond {
        fn send(
            &self,
            payload: rig_core::completion::CompletionRequest,
            exchange: Exchange,
        ) -> Opening<MockFrame> {
            if self.sends.fetch_add(1, Ordering::SeqCst) == 1 {
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
    let resolver = resolver(dir.path(), vec![image()], false);
    let mock = MockCompletionModel::from_turns([MockTurn::tool_call(
        "business",
        "anchor_echo",
        json!({"value":"exact-once"}),
    )]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let model = Model::new(
        mock.wire.clone(),
        InterruptSecond {
            inner: mock.transport.clone(),
            sends: Arc::new(AtomicUsize::new(0)),
            entered: entered.clone(),
        },
    );
    let port = image_port(dir.path(), model.erase(), resolver.clone(), true);
    let req = node_request(&resolver);
    tokio::select! {
        result = port.execute_agent(req.clone()) => panic!("unexpected outcome {result:?}"),
        _ = entered.notified() => {},
        _ = tokio::time::sleep(Duration::from_secs(15)) => panic!("second provider request did not begin"),
    }
    assert_image(&mock.requests()[0], &image());
    let recorded_path = crate::recording::directory(&port.io_store_root, &req.key)
        .join("00000000000000000001/request.json");
    let original = std::fs::read(&recorded_path).unwrap();
    let run_id = port.discover_io_run_id(&req.key).unwrap().unwrap();
    drop(port);
    let fresh = MockCompletionModel::from_turns([final_result("resumed-image-result")]);
    let reopened = image_port(dir.path(), fresh.clone().erase(), resolver.clone(), true);
    assert!(matches!(
        reopened.execute(req.clone()).await.unwrap(),
        NodeExecutionOutcome::Completed(_)
    ));
    assert_image(&fresh.requests()[0], &image());
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    let store = Store::open(reopened.io_run_path(&req.key)).unwrap();
    assert_eq!(store.runs().unwrap(), vec![run_id]);
    assert_eq!(std::fs::read(recorded_path).unwrap(), original);
    assert_eq!(fresh.request_count(), 1);
}

#[tokio::test]
async fn completed_recovery_keeps_image_input_for_one_shot_and_session_runs() {
    for conversation in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let resolver = resolver(dir.path(), vec![image()], conversation);
        let model = MockCompletionModel::from_turns([final_result("image-after-recovery")]);
        let port = image_port(dir.path(), model.clone().erase(), resolver.clone(), true);
        let req = node_request(&resolver);
        port.pin_model(&req).unwrap();
        port.mark_started(&req.key).unwrap();
        let (store, run_id) = if conversation {
            let paths =
                ConversationPaths::new(&port.io_store_root, "image-session/graph/agent", &req.key);
            let _lease = paths.acquire(&port.io_store_root).unwrap();
            let prepared = port.prepare_conversation(&paths, &req).unwrap();
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
            crate::conversation::resolve_locator(&paths, &prepared.store, &prepared.admission)
                .unwrap();
            (prepared.store, run_id)
        } else {
            let store = Store::open(port.io_run_path(&req.key)).unwrap();
            let run_id = store
                .start_run(&req.task, resolver.workspace.to_str().unwrap())
                .unwrap();
            port.store_run_id(&req.key, run_id).unwrap();
            (store, run_id)
        };
        let attempt = store
            .open_attempt(
                run_id,
                1,
                "anchor_echo",
                io_harness::ToolRecovery::Indeterminate,
            )
            .unwrap()
            .unwrap();
        drop(store);
        port.record_recovery_decision(&req.key, attempt, RecoveryDecision::Completed { observation:"Tool result was unrecorded; verify the current external state before proceeding.".into() }).unwrap();
        assert!(matches!(
            port.execute(req.clone()).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        assert_image(&model.requests()[0], &image());
        assert_eq!(model.request_count(), 1);
        assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
        let store = Store::open(port.checked_io_run_path(&req.key).unwrap()).unwrap();
        assert_eq!(store.runs().unwrap(), vec![run_id]);
    }
}

#[tokio::test]
async fn contract_image_bytes_reach_actual_chat_and_responses_http_wires() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    for (wire, image_size) in [
        ("chat", 0),
        ("responses", 0),
        ("chat", 6 * 1024 * 1024),
        ("responses", 6 * 1024 * 1024),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut frozen_image = image();
        if image_size != 0 {
            frozen_image.data.resize(image_size, 255);
        }
        let resolver = resolver(dir.path(), vec![frozen_image.clone()], false);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (capture, captured) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            let mut bytes = Vec::new();
            let body_offset = loop {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                bytes.push(byte[0]);
                if bytes.ends_with(b"\r\n\r\n") {
                    break bytes.len();
                }
                assert!(bytes.len() < 16 * 1024);
            };
            let header = String::from_utf8(bytes.clone()).unwrap();
            let length = header
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            bytes.resize(body_offset + length, 0);
            socket.read_exact(&mut bytes[body_offset..]).unwrap();
            capture
                .send(serde_json::from_slice::<serde_json::Value>(&bytes[body_offset..]).unwrap())
                .unwrap();
            let arguments = json!({"summary":"wire-image-result", "route":"next"}).to_string();
            let response = if wire == "chat" {
                json!({
                    "id":"chat-fixture","object":"chat.completion","created":1,"model":"vision-fixture",
                    "choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{
                        "id":"completion","type":"function","function":{"name":"final_result","arguments":arguments}
                    }]},"finish_reason":"tool_calls"}],
                    "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
                })
            } else {
                json!({
                    "id":"responses-fixture","object":"response","created_at":1,"status":"completed","model":"vision-fixture",
                    "error":null,"incomplete_details":null,"instructions":null,"max_output_tokens":null,"tools":[],
                    "output":[{"id":"fc-fixture","type":"function_call","status":"completed","call_id":"completion","name":"final_result","arguments":arguments}],
                    "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
                })
            }.to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
        });
        let transport = anchor_runtime_rig::RigCompletionPort::openai_compatible(
            "fixture-secret",
            format!("http://{address}/v1"),
            "vision-fixture",
            wire,
        )
        .unwrap();
        let port = image_port(dir.path(), transport.dyn_model(), resolver.clone(), true);
        assert!(matches!(
            port.execute(node_request(&resolver)).await.unwrap(),
            NodeExecutionOutcome::Completed(_)
        ));
        server.join().unwrap();
        let request = captured.recv().unwrap();
        let uri = format!(
            "data:image/png;base64,{}",
            STANDARD.encode(&frozen_image.data)
        );
        let content = if wire == "chat" {
            &request["messages"]
        } else {
            &request["input"]
        };
        assert!(
            content.as_array().unwrap().iter().any(|message| {
                message["content"].as_array().is_some_and(|parts| {
                    parts.iter().any(|part| {
                        if wire == "chat" {
                            part["type"] == "image_url" && part["image_url"]["url"] == uri
                        } else {
                            part["type"] == "input_image" && part["image_url"] == uri
                        }
                    })
                })
            }),
            "image missing from {wire} wire: {request}"
        );
    }
}
