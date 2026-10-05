use super::*;
use crate::adapter::RigProviderAdapter;
use io_harness::{CompletionRequest, Provider, provider::Replay, schema::OutputSchema};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent, MockTurn};

fn request() -> CompletionRequest {
    CompletionRequest {
        system: "record the whole task".into(),
        user: "native output tool evidence".into(),
        output_schema: Some(
            OutputSchema::new(json!({
                "type":"object","properties":{"summary":{"type":"string"}},"required":["summary"]
            }))
            .unwrap(),
        ),
        ..Default::default()
    }
}

fn read(path: impl AsRef<Path>) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[tokio::test]
async fn native_record_is_saved_before_completion_projection_and_replay_needs_that_projection() {
    let root = tempfile::tempdir().unwrap();
    let model = MockCompletionModel::from_turns([MockTurn::tool_call(
        "native-call-id",
        "final_result",
        json!({"summary":"recorded", "route":null,"extra":"retained-native"}),
    )
    .with_raw(json!({"transport_secret":"must-not-be-recorded"}))]);
    let provider = RigProviderAdapter::new(model.clone().erase(), false)
        .with_recording(root.path().join("recordings"));
    let request = request();
    let response = provider.complete(request.clone()).await.unwrap();
    let directory = root.path().join("recordings/00000000000000000001");
    let recording = read(directory.join("recording.json"));
    assert_eq!(recording["harness"], "0.86.0");
    assert_eq!(recording["exchanges"].as_array().unwrap().len(), 1);
    let exchange = &recording["exchanges"][0];
    assert_eq!(exchange["request"], serde_json::to_value(&request).unwrap());
    assert_eq!(
        exchange["response"]["tool_calls"][0]["name"],
        "final_result"
    );
    assert_eq!(
        exchange["response"]["tool_calls"][0]["arguments"]["extra"],
        "retained-native"
    );
    assert_eq!(
        read(directory.join("rig-request.json"))["tools"][0]["name"],
        "final_result"
    );
    assert_eq!(
        read(directory.join("rig-request.json")),
        serde_json::to_value(&model.requests()[0]).unwrap()
    );
    let raw_response = read(directory.join("rig-response.json"));
    assert_eq!(
        raw_response["choice"][0]["function"]["name"],
        "final_result"
    );
    assert!(raw_response.get("raw").is_none());
    assert!(
        !fs::read_to_string(directory.join("rig-response.json"))
            .unwrap()
            .contains("must-not-be-recorded")
    );
    assert_eq!(read(directory.join("outcome.json"))["status"], "succeeded");

    let replay = Replay::load(directory.join("recording.json")).unwrap();
    let native = replay.complete(request).await.unwrap();
    assert_eq!(native.tool_calls[0].name, "final_result");
    let projected = crate::completion::response(native);
    assert_eq!(
        serde_json::to_value(projected).unwrap(),
        serde_json::to_value(response).unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for entry in fs::read_dir(&directory).unwrap() {
            assert_eq!(
                fs::metadata(entry.unwrap().path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[tokio::test]
async fn provider_errors_leave_request_and_classification_without_fabricating_an_exchange() {
    let root = tempfile::tempdir().unwrap();
    let model = MockCompletionModel::from_turns([MockTurn::error(
        "bad endpoint https://secret:password@private.example with sk-sensitive-header",
    )]);
    let provider = RigProviderAdapter::new(model.clone().erase(), false)
        .with_recording(root.path().join("recordings"));
    assert!(provider.complete(request()).await.is_err());
    assert_eq!(model.request_count(), 1);
    let directory = root.path().join("recordings/00000000000000000001");
    assert!(directory.join("request.json").is_file());
    assert!(directory.join("rig-request.json").is_file());
    assert!(!directory.join("recording.json").exists());
    let outcome = read(directory.join("outcome.json"));
    assert_eq!(outcome["status"], "failed");
    assert_eq!(outcome["error_kind"], "provider_Request");
    let body = fs::read_to_string(directory.join("outcome.json")).unwrap();
    assert!(!body.contains("secret"));
    assert!(!body.contains("private.example"));
}

#[tokio::test]
async fn streaming_keeps_incremental_tokens_and_saves_the_complete_native_exchange() {
    let root = tempfile::tempdir().unwrap();
    let model = MockCompletionModel::from_stream_turns([[
        MockStreamEvent::text("first "),
        MockStreamEvent::text("second"),
        MockStreamEvent::final_response_with_default_usage(),
    ]]);
    let provider = RigProviderAdapter::new(model.erase(), false).with_recording(root.path());
    let text = std::sync::Mutex::new(Vec::new());
    let response = provider
        .complete_streaming(
            CompletionRequest {
                user: "stream".into(),
                ..Default::default()
            },
            &|delta| text.lock().unwrap().push(delta.to_owned()),
        )
        .await
        .unwrap();
    assert_eq!(*text.lock().unwrap(), vec!["first ", "second"]);
    assert_eq!(response.text.as_deref(), Some("first second"));
    assert_eq!(
        read(root.path().join("00000000000000000001/recording.json"))["exchanges"][0]["response"]["text"],
        "first second"
    );
}

#[tokio::test]
async fn post_response_recording_failure_does_not_reclassify_or_retry_the_completion() {
    let root = tempfile::tempdir().unwrap();
    let destination = root.path().join("00000000000000000001/recording.json");
    // Block the diagnostic publication after transport starts, using a token
    // callback rather than modifying runtime completion/recovery facts.
    let model = MockCompletionModel::from_stream_turns([[
        MockStreamEvent::text("returned"),
        MockStreamEvent::final_response_with_default_usage(),
    ]]);
    let provider =
        RigProviderAdapter::new(model.clone().erase(), false).with_recording(root.path());
    let response = provider
        .complete_streaming(
            CompletionRequest {
                user: "diagnostic failure".into(),
                ..Default::default()
            },
            &|_| {
                fs::create_dir(&destination).unwrap();
            },
        )
        .await
        .unwrap();
    assert_eq!(response.text.as_deref(), Some("returned"));
    assert_eq!(model.request_count(), 1);
    assert_eq!(
        read(root.path().join("00000000000000000001/outcome.json"))["status"],
        "recording_incomplete"
    );
}

#[test]
fn abandoned_request_and_reopened_recorder_append_without_overwriting_history() {
    let root = tempfile::tempdir().unwrap();
    let request = request();
    let first = Attempt::begin(root.path(), &request).unwrap();
    let first_path = first.path.clone();
    let bytes = fs::read(first_path.join("request.json")).unwrap();
    drop(first);
    let next = Attempt::begin(root.path(), &request).unwrap();
    assert_ne!(first_path, next.path);
    assert_eq!(fs::read(first_path.join("request.json")).unwrap(), bytes);
    assert!(!first_path.join("outcome.json").exists());
    assert!(next.path.ends_with("00000000000000000002"));
}

#[tokio::test]
async fn recording_storage_failure_before_send_never_calls_the_model() {
    let root = tempfile::tempdir().unwrap();
    let denied = root.path().join("file");
    fs::write(&denied, b"not a directory").unwrap();
    let model = MockCompletionModel::text("should not run");
    let provider = RigProviderAdapter::new(model.clone().erase(), false).with_recording(denied);
    assert!(provider.complete(request()).await.is_err());
    assert_eq!(model.request_count(), 0);
}

#[tokio::test]
async fn actual_http_provider_request_is_recorded_with_native_tool_and_without_transport_credentials()
 {
    use std::{io::Read, net::TcpListener, sync::mpsc, time::Duration};
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (captured, request_body) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
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
                let (key, value) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        assert!(length < 1024 * 1024);
        bytes.resize(body_offset + length, 0);
        socket.read_exact(&mut bytes[body_offset..]).unwrap();
        captured
            .send(serde_json::from_slice::<Value>(&bytes[body_offset..]).unwrap())
            .unwrap();
        let response = json!({
            "id":"fixture-response", "object":"chat.completion", "created":1,"model":"fixture-model",
            "choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{
                "id":"fixture-completion","type":"function","function":{"name":"final_result","arguments":"{\"summary\":\"http-recorded\"}"}
            }]},"finish_reason":"tool_calls"}],
            "usage":{"prompt_tokens":17,"completion_tokens":5,"total_tokens":22}
        }).to_string();
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", response.len(), response).unwrap();
    });
    let transport_secret = "recording-fixture-transport-secret";
    let transport = anchor_runtime_rig::RigCompletionPort::openai_compatible(
        transport_secret,
        format!("http://{address}/v1"),
        "fixture-model",
        "chat",
    )
    .unwrap();
    let provider =
        RigProviderAdapter::new(transport.dyn_model(), false).with_recording(root.path());
    let response = provider.complete(request()).await.unwrap();
    server.join().unwrap();
    let wire = request_body.recv().unwrap();
    assert_eq!(wire["model"], "fixture-model");
    assert_eq!(wire["tools"][0]["function"]["name"], "final_result");
    assert_eq!(
        serde_json::from_str::<Value>(response.text.as_deref().unwrap()).unwrap()["summary"],
        "http-recorded"
    );
    let directory = root.path().join("00000000000000000001");
    let recording = read(directory.join("recording.json"));
    assert_eq!(
        recording["exchanges"][0]["response"]["model"],
        "fixture-model"
    );
    assert_eq!(
        recording["exchanges"][0]["response"]["usage"]["prompt_tokens"],
        17
    );
    for entry in fs::read_dir(directory).unwrap() {
        assert!(
            !fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .contains(transport_secret)
        );
    }
}
