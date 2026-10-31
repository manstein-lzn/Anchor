use super::*;
use anchor_platform_session::{ChannelIdentity, ChannelInboundRequest, SessionStore, TurnStatus};
use anchor_runtime::graph::{GraphRunRecord, InvocationKey};

fn event(text: &str) -> Value {
    json!({
        "event": {
            "source":"wecom",
            "event_id":"message-1",
            "sender_id":"user-1",
            "conversation_id":"conversation-1",
            "text":text,
            "reply_target":"conversation-1",
            "message_type":"text",
            "metadata":{"chat_type":"single","request_id":"callback-1"},
            "attachments":[]
        }
    })
}

fn group_event(id: &str, sender: &str, group: &str, text: &str) -> Value {
    let mut body = event(text);
    body["event"]["event_id"] = json!(id);
    body["event"]["sender_id"] = json!(sender);
    body["event"]["conversation_id"] = json!(group);
    body["event"]["reply_target"] = json!(group);
    body["event"]["metadata"] = json!({"chat_type":"group","request_id":format!("callback-{id}")});
    body
}

#[tokio::test]
async fn deleted_previous_channel_run_does_not_block_a_new_instance() {
    let (_root, state) = fixture();
    assert!(
        !super::super::wecom::stop_previous_run(&state, "deleted-previous-run")
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn wecom_event_rejects_untrusted_users_unsupported_chat_types_and_invalid_media() {
    let (root, mut state) = fixture();
    with_bot(&mut state, "work", &["user-1"]);
    let app = router_with_web_root(state, root.path().join("web"));

    let mut body = event("hello");
    body["event"]["sender_id"] = json!("unauthorized");
    let (status, _) = call(
        app.clone(),
        "POST",
        "/channels/fixture/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let mut body = event("hello");
    body["event"]["metadata"]["chat_type"] = json!("broadcast");
    let (status, _) = call(
        app.clone(),
        "POST",
        "/channels/fixture/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let mut body = event("hello");
    body["event"]["attachments"] = json!([{"url":"https://example.invalid/file"}]);
    let (status, _) = call(
        app.clone(),
        "POST",
        "/channels/fixture/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A voice event without attachment content is an invalid handoff.
    let mut body = event("");
    body["event"]["message_type"] = json!("voice");
    let (status, _) = call(
        app,
        "POST",
        "/channels/fixture/events",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn wecom_attachment_validation_rejects_bad_bytes_names_and_counts_before_admission() {
    let (root, mut state) = fixture();
    with_bot(&mut state, "assistant", &["*"]);
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let invalid = vec![
        vec![json!({"name":"bad.txt","data_base64":"not base64!","media_type":null})],
        vec![json!({"name":"../host.txt","data_base64":"aG9zdA==","media_type":"text/plain"})],
        vec![json!({"name":"safe.txt","data_base64":"aG9zdA==","path":"/etc/passwd"})],
        (0..17)
            .map(|index| json!({"name":format!("{index}.txt"),"data_base64":"","media_type":null}))
            .collect::<Vec<_>>(),
    ];

    for (index, attachments) in invalid.into_iter().enumerate() {
        let mut body = event("caption");
        body["event"]["event_id"] = json!(format!("invalid-{index}"));
        body["event"]["message_type"] = json!("mixed");
        body["event"]["attachments"] = json!(attachments);
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    }

    for (message_type, text, attachments) in [
        ("image", "text must not mask missing media", json!([])),
        (
            "text",
            "text events cannot smuggle attachments",
            json!([{"name":"note.txt","data_base64":"aGVsbG8="}]),
        ),
    ] {
        let mut body = event(text);
        body["event"]["event_id"] = json!(format!("mismatch-{message_type}"));
        body["event"]["message_type"] = json!(message_type);
        body["event"]["attachments"] = attachments;
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
    }
    assert!(state.application.records().unwrap().is_empty());
    assert!(!state.data_root.join("channel-inputs").exists());
    assert!(!state.data_root.join("platform/sessions.sqlite").exists());
}

/// A channel only drives a persistent assistant. A message for a Graph without
/// the input, Agent, reply loop is refused before it is admitted, and the
/// per-message admission route no longer exists.
#[tokio::test]
async fn wecom_event_for_a_graph_without_an_input_loop_is_refused_before_admission() {
    let (root, mut state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    with_bot(&mut state, "reply", &["*"]);
    write_graph_bundle(
        &state.bundle_root,
        &json!({
            "objective":"single-node channel Graph","entry":"reply","agents":{},
            "ops":{"reply":{"run":"true"}},
            "nodes":[{"id":"reply","op":"reply","plugins":[]}],"edges":[]
        }),
    )
    .unwrap();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (status, refused) = call(
        app.clone(),
        "POST",
        "/channels/fixture/events",
        Some(&event("hello").to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert!(
        refused["error"]
            .as_str()
            .unwrap()
            .contains("not a persistent assistant"),
        "{refused}"
    );
    assert!(state.application.records().unwrap().is_empty());
    assert!(!state.data_root.join("platform/sessions.sqlite").exists());

    // Like other retired routes, the path now falls through to the static web
    // root, which has no such file and accepts no POST.
    let (status, _) = call_text(app.clone(), "GET", "/conversation-runs").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call_text(app, "POST", "/conversation-runs").await;
    assert!(
        matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "/conversation-runs must stay retired, got {status}"
    );
}

/// A gateway settlement names one admitted message by its receipt key; the
/// actor, conversation and chat type must match that message, a different
/// content digest is refused, and confirming twice is idempotent.
#[tokio::test]
async fn settlement_is_pinned_to_the_admitted_message_and_idempotent() {
    let (root, mut state) = fixture();
    with_bot(&mut state, "assistant", &["*"]);
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let body = group_event("group-bob-1", "bob", "group-a", "Bob adds details");
    let inbound = inbound_id_for("group-bob-1");
    let admission = sessions
        .admit_channel_inbound(
            "local",
            ChannelInboundRequest {
                inbound_id: inbound.clone(),
                identity: ChannelIdentity {
                    conversation_kind: anchor_platform_session::ChannelConversationKind::Group,
                    source: "wecom".into(),
                    account: None,
                    conversation_id: "group-a".into(),
                    sender_id: "bob".into(),
                },
                graph: "fixture".into(),
                reply_node: "assistant".into(),
                text: Some("Bob adds details".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
            },
        )
        .unwrap();
    let session = admission.session.id;
    let turn = admission.turn.id;
    sessions
        .finish_turn("local", &session, &turn, TurnStatus::Completed, None)
        .unwrap();
    let key = format!("channel-wecom-reply:{session}:{inbound}");
    let content_sha256 = format!("{:x}", Sha256::digest(b"group reply"));
    let request = anchor_platform_session::ChannelDeliveryRequest {
        key: key.clone(),
        kind: "wecom_reply".into(),
        content_sha256: content_sha256.clone(),
    };
    sessions
        .admit_completed_channel_delivery("local", &session, &turn, request.clone())
        .unwrap();
    sessions
        .begin_channel_delivery("local", &session, &turn, request)
        .unwrap();

    let app = router_with_web_root(state, root.path().join("web"));
    let settlement = json!({"event":body["event"],"settlement":{
        "key":key,"content_sha256":content_sha256,"status":"confirmed"
    }});
    for (field, value) in [
        ("sender_id", json!("alice")),
        ("conversation_id", json!("group-b")),
        ("chat_type", json!("single")),
    ] {
        let mut forged = settlement.clone();
        if field == "chat_type" {
            forged["event"]["metadata"][field] = value;
        } else {
            forged["event"][field] = value.clone();
            if field == "conversation_id" {
                forged["event"]["reply_target"] = value;
            }
        }
        let (status, _) = call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            Some(&forged.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "forged {field}");
    }
    let mut changed = settlement.clone();
    changed["settlement"]["content_sha256"] = json!("a".repeat(64));
    let (status, _) = call(
        app.clone(),
        "POST",
        "/channels/fixture/events",
        Some(&changed.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    for _ in 0..2 {
        let (status, confirmed) = call(
            app.clone(),
            "POST",
            "/channels/fixture/events",
            Some(&settlement.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{confirmed}");
        assert_eq!(confirmed["delivery"]["status"], "confirmed");
    }
    assert!(
        sessions
            .list_unfinished_channel_deliveries("local", &session)
            .unwrap()
            .is_empty()
    );
}

/// The event route keeps the attachment-sized body limit, above the 2 MiB limit
/// of ordinary routes, and still refuses anything larger.
#[tokio::test]
async fn wecom_event_body_limit_admits_attachment_sized_events_only() {
    let (root, mut state) = fixture();
    with_bot(&mut state, "assistant", &["*"]);
    let app = router_with_web_root(state, root.path().join("web"));
    for length_header in [true, false] {
        for (size, expected) in [
            (2 * 1024 * 1024 + 1, StatusCode::BAD_REQUEST),
            (
                crate::channel_inputs::MAX_BODY_BYTES + 1,
                StatusCode::PAYLOAD_TOO_LARGE,
            ),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/channels/fixture/events")
                .header(header::CONTENT_TYPE, "application/json");
            if length_header {
                request = request.header(header::CONTENT_LENGTH, size);
            }
            let status = app
                .clone()
                .oneshot(request.body(Body::from(vec![b' '; size])).unwrap())
                .await
                .unwrap()
                .status();
            assert_eq!(status, expected, "{size} bytes, length {length_header}");
        }
    }
}

fn inbound_id_for(event_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"anchor-wecom-inbound-v1\0");
    digest.update(b"wecom");
    digest.update([0]);
    digest.update(event_id.as_bytes());
    format!("wecom-{:x}", digest.finalize())
}

/// A persistent assistant has no per-message Run, and its `reply` node is an Op
/// without a Goose trace. The progress stream therefore has to locate the work
/// node of the round that claimed this message through the association the
/// Session store persisted — the old code looked up a *work* invocation binding
/// that never exists (bindings are keyed by the wait invocation) and silently
/// reported nothing.
#[tokio::test]
async fn persistent_assistant_progress_resolves_the_bound_round() {
    use futures_util::StreamExt;

    let (root, mut state) = fixture();
    with_bot(&mut state, "assistant", &["*"]);
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let definition = assistant_definition();
    let (session, turn, expected_work, fact) =
        seed_running_assistant_round(&state, &definition, "message-1", 1);
    let owner = "local";
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let run = expected_work.run_id.as_str();
    let wait_key = format!("{run}:{}:wait_input:1", expected_work.graph_digest);

    // The round is identified by the persisted association, and the work
    // invocation is that round with the node substituted.
    let persisted = sessions
        .channel_assistant_wait_key(owner, &session, &turn)
        .unwrap()
        .expect("the claimed round is persisted for this Turn");
    assert_eq!(persisted, wait_key);
    let work = crate::api::wecom_progress::round_work_invocation(
        &persisted,
        run,
        &expected_work.graph_digest,
        "assistant",
    )
    .expect("a durable wait key names the work invocation of its round");
    assert_eq!(work.node_id, "assistant");
    assert_eq!(work.invocation, 1);
    let binding = crate::assistant::binding(
        &state.data_root,
        &InvocationKey {
            node_id: "wait_input".into(),
            ..work.clone()
        },
    )
    .unwrap()
    .expect("the wait invocation of the round is bound");
    assert_eq!(binding.turn, turn);
    assert_eq!(binding.key.invocation, work.invocation);

    // Seed the work invocation's in-process trace exactly as a running Goose
    // node would: same durable-key stem, same session update shape.
    assert_eq!(work, expected_work);
    let live = crate::goose_acp::LiveTrace::open(&fact).unwrap();
    live.restore("native-fixture", &[]).unwrap();
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"tool_call","toolCallId":"call-1","status":"pending",
        "_meta":{"goose":{"toolCall":{"toolName":"anchor__anchor_run"}}},
        "rawInput":{"command":["sh","-c","ls -la /workspace"]}
    }}}))
    .unwrap();
    // Reasoning and visible narration are never projected.
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"agent_thought_chunk","content":{"text":"secret plan /workspace"}
    }}}))
    .unwrap();

    // The endpoint resolves the assistant Turn instead of answering 404, and a
    // Turn that is still running does not settle. An event that was never
    // admitted is not projectable at all.
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (status, denied) = call(
        app.clone(),
        "GET",
        "/channels/fixture/progress/never-admitted",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{denied}");
    let request = Request::builder()
        .method("GET")
        .uri("/channels/fixture/progress/message-1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut body_text = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, body.next()).await {
        body_text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    assert!(!body_text.contains("event: settled"), "{body_text}");
    assert!(!body_text.contains("no such channel event"), "{body_text}");
    // A real tool call projects to the whitelisted line, category and step.
    assert!(body_text.contains("event: update"), "{body_text}");
    assert!(
        body_text.contains("\"content\":\"正在查看文件\""),
        "{body_text}"
    );
    assert!(
        body_text.contains("\"category\":\"read_file\""),
        "{body_text}"
    );
    assert!(body_text.contains("\"step\":1"), "{body_text}");
    // Arguments, paths, commands, tool names and reasoning never appear.
    for leaked in [
        "/workspace",
        "ls -la",
        "sh",
        "rawInput",
        "anchor__anchor_run",
        "toolCallId",
        "secret plan",
        "agent_thought_chunk",
    ] {
        assert!(!body_text.contains(leaked), "{leaked} leaked: {body_text}");
    }
    drop(live);
}

/// A message whose Run was never admitted — the window a crash leaves behind —
/// stays a running Turn, survives a restart, is admitted only once, and is
/// recorded as superseded rather than lost when a newer message arrives.
#[tokio::test]
async fn an_admitted_message_without_a_run_is_not_finished_lost_or_duplicated() {
    let (root, mut state) = fixture();
    with_bot(&mut state, "assistant", &["*"]);
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let owner = "local";
    let inbound = |event_id: &str, text: &str| ChannelInboundRequest {
        inbound_id: inbound_id_for(event_id),
        identity: ChannelIdentity {
            conversation_kind: Default::default(),
            source: "wecom".into(),
            account: None,
            conversation_id: "conversation-1".into(),
            sender_id: "user-1".into(),
        },
        graph: "fixture".into(),
        reply_node: "reply".into(),
        text: Some(text.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
    };

    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let first = sessions
        .admit_channel_inbound(owner, inbound("crash-1", "先看这个"))
        .unwrap();
    let saved = sessions
        .get_channel_inbound(owner, &first.session.id, &inbound_id_for("crash-1"))
        .unwrap();
    assert!(saved.relation.run_id.is_none());
    assert_eq!(
        saved.turn.status,
        TurnStatus::Running,
        "a message without a Run is not a finished Turn"
    );

    // The fact survives a restart of the platform store.
    drop(sessions);
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    // The same event again admits the same Turn instead of a second one.
    let repeat = sessions
        .admit_channel_inbound(owner, inbound("crash-1", "先看这个"))
        .unwrap();
    assert_eq!(repeat.turn.id, first.turn.id);

    // A newer message supersedes the abandoned Turn and still carries its text.
    let second = sessions
        .admit_channel_inbound(owner, inbound("crash-2", "改成这个"))
        .unwrap();
    let superseded = sessions
        .get_channel_inbound(owner, &first.session.id, &inbound_id_for("crash-1"))
        .unwrap();
    assert_eq!(
        superseded.relation.superseded_by_turn_id.as_deref(),
        Some(second.turn.id.as_str())
    );
    assert_eq!(superseded.request.text.as_deref(), Some("先看这个"));
}

/// The durable shape of one *running* persistent-assistant round.
///
/// The Session claimed the admitted message for a wait invocation, the Run is
/// executing the round, and the work invocation's fact path exists on disk —
/// the runner writes that fact before it opens the invocation's live trace.
/// Returns the work invocation the progress endpoint has to resolve.
fn seed_running_assistant_round(
    state: &ApiState,
    definition: &Value,
    event_id: &str,
    invocation: u64,
) -> (String, String, InvocationKey, std::path::PathBuf) {
    use anchor_runtime::graph::GraphSnapshot;
    use sha2::{Digest, Sha256};

    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    write_graph_bundle(&state.bundle_root, definition).unwrap();
    let owner = "local";
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            owner,
            ChannelInboundRequest {
                inbound_id: inbound_id_for(event_id),
                identity: ChannelIdentity {
                    conversation_kind: Default::default(),
                    source: "wecom".into(),
                    account: Some("corp-a".into()),
                    conversation_id: "conversation-1".into(),
                    sender_id: "user-1".into(),
                },
                graph: "fixture".into(),
                reply_node: "assistant".into(),
                text: Some("看看工作区".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
            },
        )
        .unwrap();
    let session = admission.session.id.clone();
    let turn = admission.turn.id.clone();
    let run = "assistant-progress-fixture";
    let record = GraphRunRecord::create_with_id(
        GraphSnapshot::admit(definition.clone()).unwrap(),
        json!({"session":session}),
        run,
    )
    .unwrap();
    fixtures::create(&state.application, &record);
    sessions
        .bind_channel_assistant(owner, &session, run, "wait_input", "assistant", "reply")
        .unwrap();
    let wait_key = format!("{run}:{}:wait_input:{invocation}", record.graph_digest);
    let claimed = sessions
        .claim_channel_assistant_input(owner, &session, run, &wait_key)
        .unwrap()
        .expect("the running round claimed the admitted message");
    crate::assistant::bind(
        &state.data_root,
        &InvocationKey {
            run_id: run.into(),
            graph_digest: record.graph_digest.clone(),
            node_id: "wait_input".into(),
            invocation,
        },
        wait_key,
        &claimed,
        Vec::new(),
        false,
        None,
    )
    .unwrap();
    let mut saved = RunMetadata::new(
        run.into(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    saved.conversation = Some(metadata::ConversationSource {
        session: session.clone(),
        reply_node: "assistant".into(),
        previous_run: None,
    });
    saved.assistant = Some(metadata::AssistantSource {
        owner: owner.into(),
        session: session.clone(),
        wait_node: "wait_input".into(),
        work_node: "assistant".into(),
        reply_node: "reply".into(),
    });
    metadata::save(&state.data_root, &saved).unwrap();

    let work = InvocationKey {
        run_id: run.into(),
        graph_digest: record.graph_digest.clone(),
        node_id: "assistant".into(),
        invocation,
    };
    let stem = format!("{:x}", Sha256::digest(work.durable_key().as_bytes()));
    let fact = state
        .data_root
        .join("goose-acp")
        .join(format!("{stem}.json"));
    std::fs::create_dir_all(fact.parent().unwrap()).unwrap();
    std::fs::write(&fact, b"{}").unwrap();
    (session, turn, work, fact)
}

/// The assistant Graph of the WeCom product fixture, with its Plugin mounts
/// cleared so the round needs no service.
fn assistant_definition() -> Value {
    let mut definition: Value = serde_json::from_str(include_str!(
        "../../../../../examples/graphs/wecom-persistent-assistant.json"
    ))
    .unwrap();
    definition["nodes"][1]["plugins"] = json!([]);
    definition
}

/// Read the first bytes of a progress stream for `wait`.
async fn progress_prefix(
    state: &ApiState,
    web_root: std::path::PathBuf,
    wait: std::time::Duration,
) -> String {
    use futures_util::StreamExt;
    let app = router_with_web_root(state.clone(), web_root);
    let request = Request::builder()
        .method("GET")
        .uri("/channels/fixture/progress/message-1")
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body().into_data_stream();
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + wait;
    while let Ok(Some(chunk)) = tokio::time::timeout_at(deadline, body.next()).await {
        text.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
    }
    text
}

/// A live invocation that has only produced reasoning and narration is still a
/// running Turn: the user must see the guidance line instead of nothing at all.
#[tokio::test]
async fn persistent_assistant_progress_shows_guidance_from_narration_alone() {
    let (root, mut state) = fixture();
    with_bot(&mut state, "assistant", &["*"]);
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let definition = assistant_definition();
    let (_, _, _, fact) = seed_running_assistant_round(&state, &definition, "message-1", 1);

    let live = crate::goose_acp::LiveTrace::open(&fact).unwrap();
    live.restore("native-fixture", &[]).unwrap();
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"agent_thought_chunk","content":{"text":"secret plan /workspace"}
    }}}))
    .unwrap();
    live.observe(&json!({"method":"session/update","params":{
    "sessionId":"native-fixture","update":{
        "sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"narration"}
    }}}))
    .unwrap();

    let body = progress_prefix(
        &state,
        root.path().join("web"),
        std::time::Duration::from_millis(800),
    )
    .await;
    assert!(body.contains("event: update"), "{body}");
    assert!(body.contains("\"seq\":1"), "{body}");
    assert!(body.contains("\"kind\":\"status\""), "{body}");
    assert!(body.contains("\"content\":\"正在准备…\""), "{body}");
    assert!(body.contains("\"category\":\"preparing\""), "{body}");
    assert!(body.contains("\"settled\":false"), "{body}");
    assert!(
        !body.contains("\"step\""),
        "guidance carries no step: {body}"
    );
    assert!(!body.contains("event: settled"), "{body}");
    for leaked in [
        "secret plan",
        "/workspace",
        "narration",
        "agent_thought_chunk",
        "agent_message_chunk",
    ] {
        assert!(!body.contains(leaked), "{leaked} leaked: {body}");
    }
    drop(live);
}

/// A round that is resolved but whose work invocation has not registered a live
/// trace yet (or has retained nothing yet) must neither claim the Turn is over
/// nor stall silently: it owes the guidance line, and the first real tool step
/// then continues the same stream instead of repeating or renumbering it.
#[tokio::test]
async fn persistent_assistant_progress_reports_before_the_live_trace_registers() {
    let (root, mut state) = fixture();
    with_bot(&mut state, "assistant", &["*"]);
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    std::fs::create_dir_all(state.data_root.join("platform")).unwrap();
    let definition = assistant_definition();
    let (_, _, _, fact) = seed_running_assistant_round(&state, &definition, "message-1", 1);

    // The work node reaches its prompt a little later, exactly as it does in a
    // real round: the fact exists, the live trace does not.
    let registration = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let live = crate::goose_acp::LiveTrace::open(&fact).unwrap();
        live.restore("native-fixture", &[]).unwrap();
        live.observe(&json!({"method":"session/update","params":{
        "sessionId":"native-fixture","update":{
            "sessionUpdate":"tool_call","toolCallId":"call-1","status":"pending",
            "_meta":{"goose":{"toolCall":{"toolName":"anchor__anchor_run"}}},
            "rawInput":{"command":["sh","-c","ls -la /workspace"]}
        }}}))
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        drop(live);
    });

    let body = progress_prefix(
        &state,
        root.path().join("web"),
        std::time::Duration::from_secs(2),
    )
    .await;
    // The guidance line comes from the resolved round alone...
    assert!(body.contains("event: update"), "{body}");
    assert!(body.contains("\"seq\":1"), "{body}");
    assert!(body.contains("\"content\":\"正在准备…\""), "{body}");
    assert!(body.contains("\"category\":\"preparing\""), "{body}");
    assert_eq!(
        body.matches("正在准备…").count(),
        1,
        "the guidance line must not be repeated: {body}"
    );
    // ...and the real step continues the same stream at the next position.
    assert!(body.contains("\"seq\":2"), "{body}");
    assert!(body.contains("\"content\":\"正在查看文件\""), "{body}");
    assert!(body.contains("\"category\":\"read_file\""), "{body}");
    assert!(body.contains("\"step\":1"), "{body}");
    // A running Turn is never reported as settled.
    assert!(!body.contains("event: settled"), "{body}");
    for leaked in ["/workspace", "ls -la", "anchor__anchor_run", "toolCallId"] {
        assert!(!body.contains(leaked), "{leaked} leaked: {body}");
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), registration).await;
}
