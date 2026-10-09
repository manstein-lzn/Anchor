use super::*;
use anchor_runtime::graph::InvocationKey;
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{DynamicImage, ImageFormat};
use std::{io::Cursor, time::Duration};

fn attachment(name: &str, bytes: &[u8], media_type: Option<&str>) -> Value {
    json!({"name":name,"data_base64":STANDARD.encode(bytes),"media_type":media_type})
}

fn request(serial: u64, session: &str, attachments: Vec<Value>) -> Value {
    json!({
        "graph":"fixture",
        "run":format!("channel-00000000-0000-4000-8000-{serial:012x}"),
        "session":session,"reply_node":"work","input":{"message":"read my files"},
        "attachments":attachments,
    })
}

fn png_bytes() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

async fn wait_completed(state: &ApiState, run: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = FileRunStore::new(state.data_root.join("runs"))
                .load(run)
                .unwrap()
                .unwrap();
            assert_ne!(record.status, RunStatus::Failed, "{record:?}");
            if record.status == RunStatus::Completed
                && !state
                    .application
                    .active_runs(None)
                    .await
                    .iter()
                    .any(|id| id == run)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("attachment Run did not settle");
}

#[tokio::test]
async fn attachment_admission_retry_restart_and_public_manifest_use_frozen_facts() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let png = png_bytes();
    let mut jpeg = Cursor::new(Vec::new());
    DynamicImage::new_rgb8(1, 1)
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .unwrap();
    let jpeg = jpeg.into_inner();
    let original = request(
        1,
        "alice",
        vec![
            attachment("notes.txt", b"original", Some("text/plain")),
            attachment("z-diagram.png", &png, None),
            attachment("a-photo.jpg", &jpeg, None),
        ],
    );
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&original.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = original["run"].as_str().unwrap();
    wait_completed(&state, run).await;
    let (status, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    let manifest = detail["attachments"].as_array().unwrap();
    assert_eq!(manifest.len(), 3);
    assert_eq!(
        manifest[1],
        json!({
            "name":"z-diagram.png", "sha256":format!("{:x}", Sha256::digest(&png)),
            "size":png.len(), "media_type":"image/png",
        })
    );
    assert_eq!(
        manifest[0],
        json!({
            "name":"notes.txt", "sha256":format!("{:x}", Sha256::digest(b"original")),
            "size":8, "media_type":"text/plain",
        })
    );
    assert_eq!(
        manifest[2],
        json!({
            "name":"a-photo.jpg", "sha256":format!("{:x}", Sha256::digest(&jpeg)),
            "size":jpeg.len(), "media_type":"image/jpeg",
        })
    );
    let record_path = state.data_root.join("runs").join(format!("{run}.json"));
    let record_before = std::fs::read(&record_path).unwrap();
    let directory = crate::channel_inputs::input_directory(&state.data_root, run).unwrap();
    let manifest_before = std::fs::read(directory.join("input.json")).unwrap();
    let mut equivalent = original.clone();
    equivalent["attachments"][1]["media_type"] = json!("image/png");
    for same in [&original, &equivalent] {
        let (status, repeated) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&same.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{repeated}");
        assert_eq!(repeated["run"], run);
    }
    let mut reordered = original.clone();
    reordered["attachments"].as_array_mut().unwrap().reverse();
    let (status, rejected) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&reordered.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    for field in ["data_base64", "name", "media_type"] {
        let mut changed = original.clone();
        changed["attachments"][0][field] = match field {
            "data_base64" => json!(STANDARD.encode(b"replaced")),
            "name" => json!("renamed.txt"),
            _ => json!("application/octet-stream"),
        };
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&changed.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    }
    let mut restarted = state.clone();
    restarted.application =
        RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
            .with_configured_graph("fixture".into(), state.bundle_root.clone());
    let app = router(restarted.clone());
    let (status, repeated) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&equivalent.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{repeated}");
    assert!(restarted.application.active_runs(None).await.is_empty());
    assert_eq!(std::fs::read(&record_path).unwrap(), record_before);
    assert_eq!(
        std::fs::read(directory.join("input.json")).unwrap(),
        manifest_before
    );
    let trusted = restarted.application.metadata(run).unwrap().unwrap();
    let key = InvocationKey {
        run_id: run.into(),
        graph_digest: trusted.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    let inputs = crate::channel_inputs::node_inputs(&state.data_root, &trusted, &key).unwrap();
    assert_eq!(inputs.images.len(), 2);
    assert_eq!(inputs.images[0].bytes, png);
    assert_eq!(inputs.images[1].bytes, jpeg);
    let text = directory.join("files/notes.txt");
    std::fs::write(&text, b"tampered").unwrap();
    let (status, rejected) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&original.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert_eq!(std::fs::read(&text).unwrap(), b"tampered");
    assert!(crate::channel_inputs::node_inputs(&state.data_root, &trusted, &key).is_err());
    std::fs::remove_file(&text).unwrap();
    let (status, rejected) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&original.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert!(!text.exists());
    std::fs::remove_dir_all(&directory).unwrap();
    let (status, rejected) = call(
        app,
        "POST",
        "/conversation-runs",
        Some(&original.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
    assert!(!directory.exists());
    assert_eq!(std::fs::read(record_path).unwrap(), record_before);
}

#[tokio::test]
async fn invalid_attachments_leave_no_admitted_run_metadata_or_input_directory() {
    let (_root, state) = fixture();
    let app = router(state.clone());
    let png = png_bytes();
    let invalid = vec![
        vec![attachment("../host.txt", b"text", None)],
        vec![
            attachment("same", b"one", None),
            attachment("same", b"two", None),
        ],
        vec![json!({"name":"bad.txt","data_base64":"invalid!","media_type":null})],
        vec![attachment("image.png", &png, Some("image/jpeg"))],
        vec![attachment("image.png", &png[..12], None)],
        vec![attachment("image.gif", b"GIF89a\x01\x00\x01\x00", None)],
        vec![json!({"name":"host.txt","path":"/etc/passwd","data_base64":""})],
    ];
    for (serial, attachments) in invalid.into_iter().enumerate() {
        let body = request(serial as u64, "alice", attachments);
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&body.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{rejected}");
        let run = body["run"].as_str().unwrap();
        assert!(state.application.metadata(run).unwrap().is_none());
        assert!(
            !crate::channel_inputs::input_directory(&state.data_root, run)
                .unwrap()
                .exists()
        );
        assert!(state.application.records().unwrap().is_empty());
    }
    assert!(!state.data_root.join("channel-inputs").exists());
}

#[tokio::test]
async fn attachments_are_isolated_by_run_and_removed_by_run_and_graph_deletion() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let alice = request(1, "alice", vec![attachment("same.txt", b"alice", None)]);
    let bob = request(2, "bob", vec![attachment("same.txt", b"bob", None)]);
    let alice_body = alice.to_string();
    let bob_body = bob.to_string();
    let (a, b) = tokio::join!(
        call(app.clone(), "POST", "/conversation-runs", Some(&alice_body)),
        call(app.clone(), "POST", "/conversation-runs", Some(&bob_body)),
    );
    assert_eq!(a.0, StatusCode::ACCEPTED, "{}", a.1);
    assert_eq!(b.0, StatusCode::ACCEPTED, "{}", b.1);
    let alice_run = alice["run"].as_str().unwrap();
    let bob_run = bob["run"].as_str().unwrap();
    wait_completed(&state, alice_run).await;
    wait_completed(&state, bob_run).await;
    let alice_dir = crate::channel_inputs::input_directory(&state.data_root, alice_run).unwrap();
    let bob_dir = crate::channel_inputs::input_directory(&state.data_root, bob_run).unwrap();
    assert_eq!(
        std::fs::read(alice_dir.join("files/same.txt")).unwrap(),
        b"alice"
    );
    assert_eq!(
        std::fs::read(bob_dir.join("files/same.txt")).unwrap(),
        b"bob"
    );
    // Deleting one completed turn removes exactly that Run's frozen inputs; the
    // other Session's Run keeps its own.
    let (status, deleted) = call(app.clone(), "DELETE", &format!("/runs/{alice_run}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(!alice_dir.exists());
    assert!(bob_dir.exists());
    let mut legacy = request(3, "legacy", Vec::new());
    legacy.as_object_mut().unwrap().remove("attachments");
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&legacy.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let legacy_run = legacy["run"].as_str().unwrap();
    wait_completed(&state, legacy_run).await;
    let (status, detail) = call(app.clone(), "GET", &format!("/runs/{legacy_run}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["attachments"], json!([]));
    assert!(
        !crate::channel_inputs::input_directory(&state.data_root, legacy_run)
            .unwrap()
            .exists()
    );
    let (status, deleted) = call(app, "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(!bob_dir.exists());
    assert!(state.application.records().unwrap().is_empty());
}

#[tokio::test]
async fn published_inputs_reuse_original_bytes_and_incomplete_metadata_stays_closed() {
    let _env = PROCESS_ENV.lock().await;
    for metadata_saved in [false, true] {
        let (root, state) = fixture();
        set_host_env(root.path(), &state.data_root);
        let original = request(1, "alice", vec![attachment("notes.txt", b"original", None)]);
        let run = original["run"].as_str().unwrap();
        let bundle = FileGraphBundleLoader::new(&state.bundle_root)
            .load()
            .unwrap();
        let record = anchor_runtime::graph::GraphRunRecord::create_with_id(
            bundle.snapshot,
            original["input"].clone(),
            run,
        )
        .unwrap();
        let uploads = serde_json::from_value::<Vec<crate::channel_inputs::UploadedAttachment>>(
            original["attachments"].clone(),
        )
        .unwrap();
        let prepared = crate::channel_inputs::prepare(&uploads).unwrap();
        let mut trusted = RunMetadata::new(
            run.into(),
            "fixture".into(),
            record.graph_digest,
            &state.bundle_root,
        )
        .unwrap();
        trusted.conversation = Some(crate::application::ConversationSource {
            session: "alice".into(),
            reply_node: "work".into(),
            previous_run: None,
        });
        trusted.attachments = prepared.manifest();
        crate::channel_inputs::freeze(&state.data_root, &trusted, &prepared).unwrap();
        let directory = crate::channel_inputs::input_directory(&state.data_root, run).unwrap();
        let file = directory.join("files/notes.txt");
        let modified = std::fs::metadata(&file).unwrap().modified().unwrap();
        let manifest = std::fs::read(directory.join("input.json")).unwrap();
        if metadata_saved {
            metadata::save(&state.data_root, &trusted).unwrap();
        }
        let app = router(state.clone());
        let mut changed = original.clone();
        changed["attachments"][0]["data_base64"] = json!(STANDARD.encode(b"replaced"));
        let (status, rejected) = call(
            app.clone(),
            "POST",
            "/conversation-runs",
            Some(&changed.to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert!(state.application.records().unwrap().is_empty());
        assert_eq!(std::fs::read(&file).unwrap(), b"original");
        let (status, value) = call(
            app,
            "POST",
            "/conversation-runs",
            Some(&original.to_string()),
        )
        .await;
        if metadata_saved {
            assert_eq!(status, StatusCode::CONFLICT, "{value}");
            assert!(
                value["error"]
                    .as_str()
                    .unwrap()
                    .contains("without a complete admission")
            );
            assert!(state.application.records().unwrap().is_empty());
            assert!(state.application.active_runs(None).await.is_empty());
        } else {
            assert_eq!(status, StatusCode::ACCEPTED, "{value}");
            wait_completed(&state, run).await;
        }
        assert_eq!(std::fs::read(file).unwrap(), b"original");
        assert_eq!(
            std::fs::metadata(directory.join("files/notes.txt"))
                .unwrap()
                .modified()
                .unwrap(),
            modified
        );
        assert_eq!(
            std::fs::read(directory.join("input.json")).unwrap(),
            manifest
        );
    }
}

async fn body_status(app: Router, uri: &str, bytes: Vec<u8>, content_length: bool) -> StatusCode {
    let mut request = Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if content_length {
        request = request.header(header::CONTENT_LENGTH, bytes.len());
    }
    app.oneshot(request.body(Body::from(bytes)).unwrap())
        .await
        .unwrap()
        .status()
}

#[tokio::test]
async fn conversation_body_limit_accepts_large_attachments_and_preserves_other_route_limits() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let bytes = vec![b'a'; 2 * 1024 * 1024];
    let body = request(
        1,
        "alice",
        vec![attachment("large.txt", &bytes, Some("text/plain"))],
    );
    let encoded = body.to_string();
    assert!(encoded.len() > 2 * 1024 * 1024);
    let (status, accepted) = call(app.clone(), "POST", "/conversation-runs", Some(&encoded)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let run = body["run"].as_str().unwrap();
    wait_completed(&state, run).await;
    assert_eq!(
        std::fs::read(
            crate::channel_inputs::input_directory(&state.data_root, run)
                .unwrap()
                .join("files/large.txt")
        )
        .unwrap(),
        bytes
    );
    for length_header in [true, false] {
        let trigger =
            json!({"graph":"fixture","input":{"text":"a".repeat(2 * 1024 * 1024)}}).to_string();
        assert_eq!(
            body_status(app.clone(), "/trigger", trigger.into_bytes(), length_header).await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert_eq!(
            body_status(
                app.clone(),
                "/conversation-runs",
                vec![b' '; crate::channel_inputs::MAX_BODY_BYTES + 1],
                length_header
            )
            .await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }
    assert_eq!(state.application.records().unwrap().len(), 1);
}
