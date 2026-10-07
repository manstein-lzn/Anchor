use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

struct HttpFixture {
    provider: RigProviderAdapter,
    requests: Arc<Mutex<Vec<Value>>>,
    stopped: Arc<AtomicBool>,
    server: Option<std::thread::JoinHandle<()>>,
}

impl HttpFixture {
    fn new(responses: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let stopped = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::clone(&stopped);
        let server = std::thread::spawn(move || {
            let mut responses = VecDeque::from(responses);
            while !shutdown.load(Ordering::SeqCst) {
                let (mut socket, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                    assert!(header.len() < 16 * 1024);
                }
                let length = String::from_utf8(header)
                    .unwrap()
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                assert!(length < 1024 * 1024);
                let mut bytes = vec![0; length];
                socket.read_exact(&mut bytes).unwrap();
                let body: Value = serde_json::from_slice(&bytes).unwrap();
                captured.lock().unwrap().push(body.clone());
                let message = responses.pop_front().unwrap_or_else(
                    || json!({"role":"assistant", "content":"unexpected extra request"}),
                );
                if message["fixture_hold"] == true {
                    while !shutdown.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    break;
                }
                let finish = if message["tool_calls"].is_array() {
                    "tool_calls"
                } else {
                    "stop"
                };
                let usage = json!({"prompt_tokens":10,"completion_tokens":5,"total_tokens":15});
                let (content_type, response) = if body["stream"] == true {
                    let mut delta = message;
                    if let Some(calls) = delta["tool_calls"].as_array_mut() {
                        for (index, call) in calls.iter_mut().enumerate() {
                            call["index"] = json!(index);
                        }
                    }
                    let chunk = json!({"id":"fixture-response","object":"chat.completion.chunk","created":1,"model":"fixture-model","choices":[{"index":0,"delta":delta,"finish_reason":finish}],"usage":usage});
                    (
                        "text/event-stream",
                        format!("data: {chunk}\n\ndata: [DONE]\n\n"),
                    )
                } else {
                    let response = json!({"id":"fixture-response","object":"chat.completion","created":1,"model":"fixture-model","choices":[{"index":0,"message":message,"finish_reason":finish}],"usage":usage});
                    ("application/json", response.to_string())
                };
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
        });
        let transport = anchor_runtime_rig::RigCompletionPort::openai_compatible(
            "local-fixture-not-a-real-key",
            format!("http://{address}/v1"),
            "fixture-model",
            "chat",
        )
        .unwrap();
        Self {
            provider: RigProviderAdapter::new(transport.dyn_model(), false),
            requests,
            stopped,
            server: Some(server),
        }
    }

    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        self.server.take().unwrap().join().unwrap();
    }
}

fn tool_call(name: &str, arguments: Value) -> Value {
    json!({"id":format!("wire-{name}"),"type":"function","function":{"name":name,"arguments":arguments.to_string()}})
}

fn calls(calls: Vec<Value>) -> Value {
    let calls = calls
        .into_iter()
        .enumerate()
        .map(|(index, mut call)| {
            call["id"] = json!(format!("wire-call-{index}"));
            call
        })
        .collect::<Vec<_>>();
    json!({"role":"assistant","content":null,"tool_calls":calls})
}

fn ask() -> Value {
    calls(vec![tool_call(
        "session_ask",
        json!({"question":"Which destination?","context":"Choose before proceeding.","choices":["alpha","beta"]}),
    )])
}

fn reply(text: &str) -> Value {
    json!({"role":"assistant","content":text})
}

fn waiting(outcome: InteractivePilotOutcome) -> PilotPendingQuestion {
    match outcome {
        InteractivePilotOutcome::AwaitingAnswer(question) => question,
        other => panic!("expected native waiting, got {other:?}"),
    }
}

fn answer(question: &PilotPendingQuestion, text: &str) -> PilotAnswer {
    PilotAnswer {
        question: question.identity.clone(),
        answer: text.into(),
    }
}

fn assert_catalog(body: &Value, interactive: bool) {
    let names = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let mut expected = BTreeSet::from(["anchor_fixture_read"]);
    if interactive {
        expected.insert("session_ask");
    }
    assert_eq!(names, expected);
}

#[tokio::test]
async fn http_ask_reopens_resumes_original_turn_and_never_replays_tools_or_answers() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let recording = directory.path().join("recordings");
    let fixture = HttpFixture::new(vec![
        calls(vec![tool_call(
            "anchor_fixture_read",
            json!({"key":"before-ask"}),
        )]),
        ask(),
        reply("Destination alpha selected."),
        reply("A later turn remembers alpha."),
    ]);
    let provider = fixture.provider.clone().with_recording(&recording);
    let port = Arc::new(FixturePort::default());
    let observer = Arc::new(Watching::default());
    let question = waiting(
        run_interactive_pilot(
            request(&root, "Choose a destination"),
            &provider,
            port.clone(),
            observer.clone(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(fixture.count(), 2);
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    assert_eq!(question.question, "Which destination?");
    assert_eq!(
        question.context.as_deref(),
        Some("Choose before proceeding.")
    );
    assert_eq!(question.choices.len(), 2);
    assert!(
        observer
            .chunks
            .lock()
            .unwrap()
            .iter()
            .any(|chunk| chunk["type"] == "tool-input-start" && chunk["toolName"] == "session_ask")
    );
    {
        let (store, session) = native(&root);
        let turns = session.history(&store).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].id, question.identity.turn_id);
        assert_eq!(turns[0].outcome.as_deref(), Some("awaiting_answer"));
        let pending = store
            .question(question.identity.question_id)
            .unwrap()
            .unwrap();
        assert!(!pending.resolved);
        assert!(
            store
                .step_turns(question.identity.run_id)
                .unwrap()
                .iter()
                .flat_map(|step| &step.calls)
                .any(|call| call.name == "ask_question")
        );
    }
    let reopened = pilot_pending_question(&root).unwrap().unwrap();
    assert_eq!(question.identity, reopened.identity);
    let history = pilot_messages(&root).unwrap();
    assert!(
        history
            .iter()
            .any(|message| message["role"] == "assistant" && message["text"] == question.question)
    );
    let resumed = resume_pilot_with_answer(
        request(&root, "Choose a destination"),
        answer(&reopened, "alpha"),
        &provider,
        port.clone(),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    match resumed {
        InteractivePilotOutcome::Finished(outcome) => {
            assert_eq!(outcome.status, PilotStatus::Completed, "{outcome:?}");
            assert_eq!(
                outcome.reply.as_deref(),
                Some("Destination alpha selected.")
            );
        }
        other => panic!("expected completion: {other:?}"),
    }
    assert_eq!(fixture.count(), 3);
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    assert!(pilot_pending_question(&root).unwrap().is_none());
    let (store, session) = native(&root);
    let turns = session.history(&store).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, question.identity.turn_id);
    assert_eq!(
        turns[0].reply.as_deref(),
        Some("Destination alpha selected.")
    );
    assert_eq!(
        turns[0].outcome,
        store.outcome(question.identity.run_id).unwrap()
    );
    let accepted = store
        .question(question.identity.question_id)
        .unwrap()
        .unwrap();
    assert!(accepted.resolved);
    assert_eq!(store.spent_tokens(question.identity.run_id).unwrap(), 45);
    assert_eq!(accepted.answer.as_deref(), Some("alpha"));
    for text in ["alpha", "beta"] {
        let error = resume_pilot_with_answer(
            request(&root, "Choose a destination"),
            answer(&question, text),
            &provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap_err();
        assert!(error.contains("already answered"), "{error}");
    }
    assert_eq!(fixture.count(), 3);
    let history = pilot_messages(&root).unwrap();
    assert!(
        history
            .iter()
            .any(|message| message["role"] == "user" && message["text"] == "alpha")
    );
    assert!(
        history
            .iter()
            .any(|message| message["text"] == "Destination alpha selected.")
    );
    assert!(
        history
            .iter()
            .filter_map(|message| message["commands"].as_array())
            .flatten()
            .filter_map(Value::as_str)
            .any(|command| command.starts_with("session_ask "))
    );
    run_interactive_pilot(
        request(&root, "Remember the answer"),
        &provider,
        port.clone(),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    assert_eq!(session.history(&store).unwrap().len(), 1);
    let (_, refreshed) = native(&root);
    assert_eq!(refreshed.history(&store).unwrap().len(), 2);
    let bodies = fixture.requests.lock().unwrap();
    for body in bodies.iter() {
        assert_catalog(body, true);
        let encoded = serde_json::to_string(&body["messages"]).unwrap();
        assert!(!encoded.contains("\"name\":\"ask_question\""));
    }
    let resumed_body = serde_json::to_string(&bodies[2]).unwrap();
    assert!(resumed_body.contains("Which destination?"));
    assert!(resumed_body.contains("alpha"));
    assert!(
        resumed_body.contains("\"name\":\"session_ask\""),
        "{resumed_body}"
    );
    assert!(
        serde_json::to_string(&bodies[3])
            .unwrap()
            .contains("Destination alpha selected.")
    );
    drop(bodies);
    let recorded: Value = serde_json::from_slice(
        &fs::read(recording.join("00000000000000000002/recording.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        recorded["exchanges"][0]["response"]["tool_calls"][0]["name"],
        "session_ask"
    );
}

#[tokio::test]
async fn http_invalid_question_identity_and_both_leases_refuse_before_answer_or_provider() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let fixture = HttpFixture::new(vec![ask()]);
    let port = Arc::new(FixturePort::default());
    let question = waiting(
        run_interactive_pilot(
            request(&root, "Choose a destination"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap(),
    );
    for field in ["question", "run", "session", "turn", "scope"] {
        let mut invalid = answer(&question, "alpha");
        match field {
            "question" => invalid.question.question_id += 100,
            "run" => invalid.question.run_id += 100,
            "session" => invalid.question.session_id += 100,
            "turn" => invalid.question.turn_id += 100,
            "scope" => invalid.question.scope = directory.path().join("a".repeat(64)),
            _ => unreachable!(),
        }
        assert!(
            resume_pilot_with_answer(
                request(&root, "Choose a destination"),
                invalid,
                &fixture.provider,
                port.clone(),
                Arc::new(Watching::default())
            )
            .await
            .is_err(),
            "{field}"
        );
    }
    assert!(
        run_interactive_pilot(
            request(&root, "Do not abandon the waiting turn"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default())
        )
        .await
        .is_err()
    );
    {
        let (store, _) = native(&root);
        let _lease = store.acquire_lease(question.identity.run_id, 60).unwrap();
        let error = resume_pilot_with_answer(
            request(&root, "Choose a destination"),
            answer(&question, "alpha"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap_err();
        assert!(error.contains("held by another owner"), "{error}");
        assert!(
            !store
                .question(question.identity.question_id)
                .unwrap()
                .unwrap()
                .resolved
        );
    }
    {
        let paths = PilotPaths::new(&root).unwrap();
        let _lease = paths.acquire().unwrap();
        assert!(
            resume_pilot_with_answer(
                request(&root, "Choose a destination"),
                answer(&question, "alpha"),
                &fixture.provider,
                port.clone(),
                Arc::new(Watching::default())
            )
            .await
            .is_err()
        );
    }
    assert_eq!(fixture.count(), 1);
    let (store, _) = native(&root);
    assert!(
        !store
            .question(question.identity.question_id)
            .unwrap()
            .unwrap()
            .resolved
    );
    assert_eq!(
        store
            .session_turns(question.identity.session_id)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn http_observer_and_native_association_fail_closed_before_model_and_answer_acceptance() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let fixture = HttpFixture::new(vec![ask()]);
    let port = Arc::new(FixturePort::default());
    let failed = run_interactive_pilot(
        request(&root, "Association must precede transport"),
        &fixture.provider,
        port.clone(),
        Arc::new(Watching {
            fail_native: true,
            ..Watching::default()
        }),
    )
    .await
    .unwrap();
    assert!(matches!(
        failed,
        InteractivePilotOutcome::Finished(PilotOutcome {
            status: PilotStatus::Failed,
            ..
        })
    ));
    assert_eq!(fixture.count(), 0);
    let question = waiting(
        run_interactive_pilot(
            request(&root, "Choose a destination"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap(),
    );
    for (index, observer) in [
        Watching {
            fail_native: true,
            ..Watching::default()
        },
        Watching {
            fail_on: Some("resume-start"),
            ..Watching::default()
        },
        Watching {
            fail_on: Some("start-step"),
            ..Watching::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let result = resume_pilot_with_answer(
            request(&root, "Choose a destination"),
            answer(&question, "alpha"),
            &fixture.provider,
            port.clone(),
            Arc::new(observer),
        )
        .await;
        assert!(
            result.is_err()
                || matches!(
                    result.unwrap(),
                    InteractivePilotOutcome::Finished(PilotOutcome {
                        status: PilotStatus::Failed,
                        ..
                    })
                )
        );
        assert_eq!(fixture.count(), 1);
        let (store, _) = native(&root);
        assert_eq!(
            store
                .question(question.identity.question_id)
                .unwrap()
                .unwrap()
                .resolved,
            index == 2
        );
    }
}

#[tokio::test]
async fn http_builtin_filesystem_exec_and_batch_questions_stay_masked() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let forbidden = root.join("forbidden.txt");
    let fixture = HttpFixture::new(vec![
        calls(vec![
            tool_call(
                "write_file",
                json!({"path":"forbidden.txt","content":"should not write"}),
            ),
            tool_call(
                "exec",
                json!({"command":format!("touch {}", forbidden.display())}),
            ),
            tool_call(
                "ask_questions",
                json!({"questions":[{"question":"not allowed"}]}),
            ),
        ]),
        reply("No builtin privileges were granted."),
    ]);
    let outcome = run_interactive_pilot(
        request(&root, "Use only authorized tools"),
        &fixture.provider,
        Arc::new(FixturePort::default()),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome,
            InteractivePilotOutcome::Finished(PilotOutcome {
                status: PilotStatus::Completed,
                ..
            })
        ),
        "{outcome:?}"
    );
    assert!(!forbidden.exists());
    assert!(pilot_pending_question(&root).unwrap().is_none());
    let (store, session) = native(&root);
    let run_id = session.history(&store).unwrap()[0].run_id;
    assert!(store.questions(run_id).unwrap().is_empty());
    let observations = serde_json::to_string(&store.observations(run_id).unwrap()).unwrap();
    assert!(observations.contains("withholds"), "{observations}");
    assert_eq!(fixture.count(), 2);
    for body in fixture.requests.lock().unwrap().iter() {
        assert_catalog(body, true);
    }
}

#[tokio::test]
async fn http_same_reply_runs_tools_before_ask_skips_tools_after_and_resume_does_not_replay() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let fixture = HttpFixture::new(vec![
        calls(vec![
            tool_call("anchor_fixture_read", json!({"key":"before"})),
            tool_call("session_ask", json!({"question":"Which destination?"})),
            tool_call("anchor_fixture_read", json!({"key":"after"})),
        ]),
        reply("Continued after the committed mixed step."),
    ]);
    let port = Arc::new(FixturePort::default());
    let question = waiting(
        run_interactive_pilot(
            request(&root, "Mixed tool reply"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap(),
    );
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    {
        let (store, _) = native(&root);
        let observations = store.observations(question.identity.run_id).unwrap();
        assert!(
            observations
                .iter()
                .any(|observation| observation.text.contains("before"))
        );
        assert!(
            !observations
                .iter()
                .any(|observation| observation.text.contains("\"found\":\"after\""))
        );
        let names = store
            .events_since(question.identity.run_id, 0, 512)
            .unwrap()
            .into_iter()
            .filter_map(|(_, event)| match event.kind {
                EventKind::ToolCall { name, .. } => Some(name),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["anchor_fixture_read", "ask_question"]);
        assert_eq!(store.last_step(question.identity.run_id).unwrap(), 1);
    }
    let outcome = resume_pilot_with_answer(
        request(&root, "Mixed tool reply"),
        answer(&question, "alpha"),
        &fixture.provider,
        port.clone(),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            outcome,
            InteractivePilotOutcome::Finished(PilotOutcome {
                status: PilotStatus::Completed,
                ..
            })
        ),
        "{outcome:?}"
    );
    assert_eq!(fixture.count(), 2);
    assert_eq!(port.calls.load(Ordering::SeqCst), 1);
    let (store, _) = native(&root);
    assert_eq!(store.questions(question.identity.run_id).unwrap().len(), 1);
    assert_eq!(store.last_step(question.identity.run_id).unwrap(), 2);
}

#[tokio::test]
async fn http_answer_persisted_before_incomplete_provider_reopen_refuses_duplicate_without_replay()
{
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let fixture = HttpFixture::new(vec![ask(), json!({"fixture_hold":true})]);
    let port = Arc::new(FixturePort::default());
    let question = waiting(
        run_interactive_pilot(
            request(&root, "Choose a destination"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap(),
    );
    {
        let mut resumed = Box::pin(resume_pilot_with_answer(
            request(&root, "Choose a destination"),
            answer(&question, "alpha"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        ));
        tokio::select! {
            outcome = &mut resumed => panic!("expected unfinished transport, got {outcome:?}"),
            reached = tokio::time::timeout(Duration::from_secs(5), async {
                while fixture.count() < 2 { tokio::time::sleep(Duration::from_millis(5)).await; }
            }) => reached.unwrap(),
        }
    }
    assert_eq!(fixture.count(), 2);
    {
        let (store, session) = native(&root);
        let saved = store
            .question(question.identity.question_id)
            .unwrap()
            .unwrap();
        assert!(saved.resolved);
        assert_eq!(saved.answer.as_deref(), Some("alpha"));
        assert_eq!(saved.answered_by.as_deref(), Some("human"));
        assert!(
            store
                .observations(question.identity.run_id)
                .unwrap()
                .iter()
                .any(|observation| observation.text.contains("[answer] alpha"))
        );
        assert_eq!(store.last_step(question.identity.run_id).unwrap(), 1);
        assert_eq!(session.head(), Some(question.identity.turn_id));
        assert_eq!(session.history(&store).unwrap().len(), 1);
        assert_eq!(session.history(&store).unwrap()[0].reply, None);
    }
    assert!(
        pilot_pending_question(&root)
            .unwrap_err()
            .contains("inspect recovery")
    );
    for text in ["alpha", "beta"] {
        let error = resume_pilot_with_answer(
            request(&root, "Choose a destination"),
            answer(&question, text),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap_err();
        assert!(error.contains("already answered"), "{error}");
    }
    assert!(
        run_interactive_pilot(
            request(&root, "Do not replace unknown execution"),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default())
        )
        .await
        .is_err()
    );
    assert_eq!(fixture.count(), 2);
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    let history = pilot_messages(&root).unwrap();
    assert!(
        history
            .iter()
            .any(|message| message["role"] == "user" && message["text"] == "alpha")
    );
}

#[tokio::test]
async fn http_native_step_budget_is_not_reset_when_answer_resumes_existing_run() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let fixture = HttpFixture::new(vec![ask()]);
    let port = Arc::new(FixturePort::default());
    let mut bounded = request(&root, "Budgeted question");
    bounded.max_steps = 1;
    let question = waiting(
        run_interactive_pilot(
            bounded.clone(),
            &fixture.provider,
            port.clone(),
            Arc::new(Watching::default()),
        )
        .await
        .unwrap(),
    );
    let outcome = resume_pilot_with_answer(
        bounded,
        answer(&question, "alpha"),
        &fixture.provider,
        port.clone(),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    match outcome {
        InteractivePilotOutcome::Finished(outcome) => {
            assert_eq!(outcome.status, PilotStatus::Stopped);
            assert!(outcome.error.unwrap().contains("StepCapReached"));
        }
        other => panic!("expected step cap: {other:?}"),
    }
    assert_eq!(fixture.count(), 1);
    let (store, session) = native(&root);
    assert_eq!(store.last_step(question.identity.run_id).unwrap(), 1);
    assert_eq!(store.spent_tokens(question.identity.run_id).unwrap(), 15);
    assert_eq!(session.history(&store).unwrap().len(), 1);
    assert_eq!(
        session.history(&store).unwrap()[0].outcome.as_deref(),
        Some("step_cap_reached")
    );
}

#[tokio::test]
async fn http_opt_in_does_not_mutate_plain_pilot_provider_catalog_or_recorded_facts() {
    let directory = tempfile::tempdir().unwrap();
    let root = scope(&directory);
    let other_root = directory.path().join("b".repeat(64));
    let recording = directory.path().join("recordings");
    let fixture = HttpFixture::new(vec![ask(), reply("Plain Pilot without questions.")]);
    let provider = fixture.provider.clone().with_recording(&recording);
    run_interactive_pilot(
        request(&root, "Ask only when opted in"),
        &provider,
        Arc::new(FixturePort::default()),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    let outcome = run_pilot(
        request(&other_root, "No added capability"),
        &provider,
        Arc::new(FixturePort::default()),
        Arc::new(Watching::default()),
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, PilotStatus::Completed);
    let bodies = fixture.requests.lock().unwrap();
    assert_catalog(&bodies[0], true);
    assert_catalog(&bodies[1], false);
    let plain_record: Value = serde_json::from_slice(
        &fs::read(recording.join("00000000000000000002/recording.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        plain_record["exchanges"][0]["request"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        !serde_json::to_string(&plain_record)
            .unwrap()
            .contains("session_ask")
    );
}
