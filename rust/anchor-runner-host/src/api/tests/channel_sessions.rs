use super::*;
use anchor_platform_session::{SessionStore, TurnStatus};
use axum::body::Body;
use sha2::{Digest, Sha256};
use std::time::Duration;

async fn authorized_call(
    app: Router,
    method: &str,
    uri: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::AUTHORIZATION, format!("Bearer {key}"))
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn inbound(id: &str, conversation: &str) -> Value {
    json!({
        "inbound_id": id,
        "identity": {
            "source": "wecom",
            "account": "corp-a",
            "conversation_id": conversation,
            "sender_id": "user-1"
        },
        "graph": "fixture",
        "reply_node": "work",
        "text": "hello"
    })
}

#[tokio::test]
async fn channel_admission_is_authenticated_idempotent_and_owner_scoped() {
    let (root, mut state) = fixture();
    let first_key = "a".repeat(32);
    let second_key = "b".repeat(32);
    state.loopback = false;
    state.api_keys = vec![first_key.clone(), second_key.clone()];
    let app = router_with_web_root(state.clone(), root.path().join("web"));

    let (status, first) = authorized_call(
        app.clone(),
        "POST",
        "/channel-sessions/inbound",
        &first_key,
        inbound("message-1", "conversation-1"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{first}");
    assert_eq!(first["session"]["graph"], "fixture");
    assert_eq!(first["relation"]["run_id"], Value::Null);
    let session = first["session"]["id"].as_str().unwrap().to_owned();
    let turn = first["turn"]["id"].as_str().unwrap().to_owned();

    let (status, duplicate) = authorized_call(
        app.clone(),
        "POST",
        "/channel-sessions/inbound",
        &first_key,
        inbound("message-1", "conversation-1"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{duplicate}");
    assert_eq!(duplicate["session"]["id"], session);
    assert_eq!(duplicate["turn"]["id"], turn);

    let (status, other) = authorized_call(
        app.clone(),
        "POST",
        "/channel-sessions/inbound",
        &second_key,
        inbound("message-1", "conversation-1"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{other}");
    assert_ne!(other["session"]["id"], session);

    let (status, relation) = authorized_call(
        app.clone(),
        "GET",
        &format!("/channel-sessions/{session}/inbounds/message-1"),
        &first_key,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{relation}");
    assert_eq!(relation["relation"]["turn_id"], turn);

    let (status, bound) = authorized_call(
        app.clone(),
        "POST",
        &format!("/channel-sessions/{session}/inbounds/message-1/run"),
        &first_key,
        json!({"run_id":"channel-run-1"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bound}");
    assert_eq!(bound["relation"]["run_id"], "channel-run-1");

    let (status, forbidden) = authorized_call(
        app.clone(),
        "GET",
        &format!("/channel-sessions/{session}/inbounds/message-1"),
        &second_key,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{forbidden}");

    let (status, _) = authorized_call(
        app,
        "POST",
        "/channel-sessions/inbound",
        &first_key,
        json!({"owner":"forged", "inbound_id":"message-2"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn channel_delivery_routes_preserve_the_store_state_machine() {
    let (root, state) = fixture();
    let app = router_with_web_root(state.clone(), root.path().join("web"));
    let (_, admitted) = call(
        app.clone(),
        "POST",
        "/channel-sessions/inbound",
        Some(&inbound("message-1", "conversation-1").to_string()),
    )
    .await;
    let session = admitted["session"]["id"].as_str().unwrap();
    let turn = admitted["turn"]["id"].as_str().unwrap();
    let hash = "b".repeat(64);
    let delivery = json!({"key":"reply-1","kind":"text","content_sha256":hash});

    let (status, pending) = call(
        app.clone(),
        "POST",
        &format!("/channel-sessions/{session}/turns/{turn}/deliveries"),
        Some(&delivery.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{pending}");
    assert_eq!(pending["delivery"]["status"], "pending");

    let (status, sending) = call(
        app.clone(),
        "POST",
        &format!("/channel-sessions/{session}/turns/{turn}/deliveries/reply-1/begin"),
        Some(&json!({"kind":"text","content_sha256":hash}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sending}");
    assert_eq!(sending["delivery"]["status"], "sending");

    let (status, unfinished) = call(
        app.clone(),
        "GET",
        &format!("/channel-sessions/{session}/deliveries/unresolved"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unfinished}");
    assert_eq!(unfinished["deliveries"].as_array().unwrap().len(), 1);

    let (status, confirmed) = call(
        app.clone(),
        "POST",
        &format!("/channel-sessions/{session}/deliveries/reply-1/settle"),
        Some(
            &json!({
                "kind":"text",
                "content_sha256":hash,
                "status":"confirmed"
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert_eq!(confirmed["delivery"]["status"], "confirmed");

    let (status, unfinished) = call(
        app.clone(),
        "GET",
        &format!("/channel-sessions/{session}/deliveries/unresolved"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{unfinished}");
    assert!(unfinished["deliveries"].as_array().unwrap().is_empty());

    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    assert_eq!(sessions.list("local").unwrap().len(), 1);
}

#[tokio::test]
async fn channel_admission_integrates_with_conversation_runs_and_settles_graph_rejection() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path(), &state.data_root);
    let app = router(state.clone());
    let attachment_hash = format!("{:x}", Sha256::digest(b"hello"));
    let (status, admitted) = call(
        app.clone(),
        "POST",
        "/channel-sessions/inbound",
        Some(
            &json!({
                "inbound_id":"message-1",
                "identity":{"source":"wecom","account":"corp-a","conversation_id":"conversation-1","sender_id":"user-1"},
                "graph":"fixture","reply_node":"work","text":"hello",
                "attachments":{"format":1,"files":[{"name":"note.txt","path":"/in/channel/note.txt","sha256":attachment_hash,"size":5,"media_type":"text/plain"}]}
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{admitted}");
    let session = admitted["session"]["id"].as_str().unwrap().to_owned();
    let run = "channel-00000000-0000-4000-8000-000000000001";
    let body = json!({
        "graph":"fixture","run":run,"session":session,"reply_node":"work",
        "input":{"message":"hello"},"channel_inbound":"message-1",
        "attachments":[{"name":"note.txt","data_base64":"aGVsbG8=","media_type":"text/plain"}]
    });
    let (status, accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let relation = sessions
        .get_channel_relation("local", &session, "message-1")
        .unwrap();
    assert_eq!(relation.run_id.as_deref(), Some(run));
    let turn = sessions
        .get_turn("local", &session, &relation.turn_id)
        .unwrap();
    assert_eq!(turn.runs, vec![run]);
    assert_eq!(sessions.get("local", &session).unwrap().run_ids, vec![run]);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = sessions
                .get_turn("local", &session, &relation.turn_id)
                .unwrap()
                .status;
            if status != TurnStatus::Running {
                assert_eq!(status, TurnStatus::Completed);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("channel Turn did not settle");
    assert_eq!(
        std::fs::read(
            crate::channel_inputs::input_directory(&state.data_root, run)
                .unwrap()
                .join("files/note.txt")
        )
        .unwrap(),
        b"hello"
    );

    let (status, second_admitted) = call(
        app.clone(),
        "POST",
        "/channel-sessions/inbound",
        Some(
            &json!({
                "inbound_id":"message-2",
                "identity":{"source":"wecom","account":"corp-a","conversation_id":"conversation-1","sender_id":"user-1"},
                "graph":"fixture","reply_node":"work","text":"second"
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{second_admitted}");
    let second_run = "channel-00000000-0000-4000-8000-000000000003";
    let second_body = json!({
        "graph":"fixture","run":second_run,"session":session,"reply_node":"work",
        "input":{"message":"second"},"previous_run":run,"channel_inbound":"message-2"
    });
    let (status, second_accepted) = call(
        app.clone(),
        "POST",
        "/conversation-runs",
        Some(&second_body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{second_accepted}");
    let second_relation = sessions
        .get_channel_relation("local", &session, "message-2")
        .unwrap();
    assert_eq!(second_relation.run_id.as_deref(), Some(second_run));
    assert_eq!(
        sessions
            .get_turn("local", &session, &second_relation.turn_id)
            .unwrap()
            .runs,
        vec![second_run]
    );
    let (status, listed) = call(app.clone(), "GET", "/runs", None).await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    let listed_run = listed["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["run"] == second_run)
        .unwrap();
    assert_eq!(
        listed_run["trigger"]["channel"],
        json!({"session":session,"inbound":"message-2","turn":second_relation.turn_id})
    );
    assert!(listed_run["trigger"]["channel"].get("owner").is_none());
    let (status, detail) = call(app.clone(), "GET", &format!("/runs/{second_run}"), None).await;
    assert_eq!(status, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["state"]["trigger"]["channel"],
        json!({"session":session,"inbound":"message-2","turn":second_relation.turn_id})
    );

    let (status, rejected_admission) = call(
        app.clone(),
        "POST",
        "/channel-sessions/inbound",
        Some(
            &json!({
                "inbound_id":"message-fail",
                "identity":{"source":"wecom","account":"corp-a","conversation_id":"conversation-2","sender_id":"user-1"},
                "graph":"fixture","reply_node":"missing","text":"fail"
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{rejected_admission}");
    let failed_session = rejected_admission["session"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let failed_run = "channel-00000000-0000-4000-8000-000000000002";
    let failed_body = json!({
        "graph":"fixture","run":failed_run,"session":failed_session,"reply_node":"missing",
        "input":{"message":"fail"},"channel_inbound":"message-fail"
    });
    let (status, rejected) = call(
        app,
        "POST",
        "/conversation-runs",
        Some(&failed_body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{rejected}");
    let sessions = SessionStore::open(state.data_root.join("platform/sessions.sqlite")).unwrap();
    let failed_relation = sessions
        .get_channel_relation("local", &failed_session, "message-fail")
        .unwrap();
    assert_eq!(failed_relation.run_id, None);
    assert_eq!(
        sessions
            .get_turn("local", &failed_session, &failed_relation.turn_id)
            .unwrap()
            .status,
        anchor_platform_session::TurnStatus::Failed
    );
}
