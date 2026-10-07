use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};

use crate::{GatewayConfig, GatewayError, normalize_message, protocol::stream_id};

fn callback() -> Value {
    json!({"cmd":"aibot_msg_callback","headers":{"req_id":"callback-1"},
        "body":{"msgid":"message-1","from":{"userid":"alice"},"msgtype":"text",
            "text":{"content":"hello"}}})
}

#[test]
fn text_and_text_only_mixed_preserve_identity_and_input_order() {
    let mut frame = callback();
    let event = normalize_message(&frame).unwrap().unwrap();
    assert_eq!(event.source, "wecom");
    assert_eq!(event.event_id, "message-1");
    assert_eq!(event.conversation_id, "alice");
    assert_eq!(event.reply_target, "alice");
    assert_eq!(event.metadata["request_id"], "callback-1");
    assert!(event.attachments.is_empty());
    frame["body"] = json!({"from":{"userid":"alice"},"chatid":"conversation-2","msgtype":"mixed",
        "mixed":{"msg_item":[{"msgtype":"text","text":{"content":"first"}},
            {"msgtype":"text","text":{"content":"second"}}]}});
    let mixed = normalize_message(&frame).unwrap().unwrap();
    assert_eq!(mixed.event_id, "callback-1");
    assert_eq!(mixed.text, "first\nsecond");
    assert_eq!(mixed.conversation_id, "conversation-2");
    assert_eq!(stream_id("message-1"), stream_id("message-1"));
    assert_eq!(stream_id("message-1").len(), 39);
    assert_ne!(stream_id("message-1"), stream_id("message-2"));
    assert!(
        normalize_message(&json!({"cmd":"aibot_event_callback"}))
            .unwrap()
            .is_none()
    );
}

#[test]
fn media_is_rejected_instead_of_extracting_only_its_text() {
    for kind in ["image", "file", "voice", "video"] {
        let mut frame = callback();
        frame["body"]["msgtype"] = json!(kind);
        assert_eq!(
            normalize_message(&frame),
            Err(GatewayError::UnsupportedMedia)
        );
        frame["body"]["msgtype"] = json!("text");
        frame["body"][kind] = json!({"url":"https://example.invalid/sensitive"});
        assert_eq!(
            normalize_message(&frame),
            Err(GatewayError::UnsupportedMedia)
        );
    }
    let mut frame = callback();
    frame["body"]["msgtype"] = json!("mixed");
    frame["body"]["mixed"] = json!({"msg_item":[{"msgtype":"text","text":{"content":"first"}},
        {"msgtype":"image","image":{"url":"https://example.invalid/sensitive"}}]});
    assert_eq!(
        normalize_message(&frame),
        Err(GatewayError::UnsupportedMedia)
    );
    let mut hidden = callback();
    hidden["body"]["text"]["image"] = json!({"url":"https://example.invalid/sensitive"});
    assert_eq!(
        normalize_message(&hidden),
        Err(GatewayError::UnsupportedMedia)
    );
    hidden = callback();
    hidden["body"]["mixed"] = json!({"msg_item":[{"msgtype":"image"}]});
    assert_eq!(
        normalize_message(&hidden),
        Err(GatewayError::UnsupportedMedia)
    );
}

#[test]
fn callback_identity_types_controls_and_size_are_validated() {
    let original = callback();
    for path in ["/headers/req_id", "/body/msgid", "/body/from/userid"] {
        for invalid in [json!(47), json!("\nidentity"), json!("")] {
            let mut frame = original.clone();
            *frame.pointer_mut(path).unwrap() = invalid;
            if path != "/body/msgid" || frame.pointer(path) != Some(&json!("")) {
                assert!(normalize_message(&frame).is_err());
            }
        }
    }
    let mut frame = original;
    frame["body"]["text"]["content"] = json!("a".repeat(crate::MAX_TEXT_BYTES + 1));
    assert!(normalize_message(&frame).is_err());
    frame["body"]["text"]["content"] = json!("hello");
    frame["body"]["chattype"] = json!(1);
    assert!(normalize_message(&frame).is_err());
}

#[test]
fn configuration_rejects_insecure_transports_and_redacts_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = GatewayConfig::new(
        directory.path(),
        "bot-sensitive".into(),
        "secret-sensitive".into(),
        "control-sensitive-000000000000000000".into(),
    );
    assert!(config.validate().is_ok());
    let debug = format!("{config:?}");
    for secret in [&config.bot_id, &config.secret, &config.control_token] {
        assert!(!debug.contains(secret));
    }
    for url in [
        "ws://example.com",
        "wss://user:secret@example.com",
        "wss://example.com?secret=value",
        "wss://example.com#fragment",
        "http://127.0.0.1",
        "ws://localhost",
    ] {
        config.ws_url = url.into();
        assert!(config.validate().is_err());
    }
    config.ws_url = "ws://127.0.0.1:1234".into();
    assert!(config.validate().is_ok());
    config.webhook = Some(crate::WebhookConfig {
        url: "http://127.0.0.1:8080/events".into(),
        api_key: "webhook-key".into(),
        timeout: std::time::Duration::from_secs(2),
    });
    assert!(config.validate().is_ok());
    config.webhook.as_mut().unwrap().url = "http://localhost:8080/events".into();
    assert!(config.validate().is_err());
    config.webhook.as_mut().unwrap().url = "https://hooks.example/events?token=x".into();
    assert!(config.validate().is_err());
    config.webhook.as_mut().unwrap().url = "https://hooks.example/events".into();
    config.webhook.as_mut().unwrap().api_key = "k".repeat(4097);
    assert!(config.validate().is_err());
    config.control_token = "short".into();
    assert!(config.validate().is_err());
}

#[test]
fn private_state_requires_private_regular_files_and_exclusive_lease() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("state");
    let config = GatewayConfig::new(&state_dir, "bot".into(), "secret".into(), "c".repeat(40));
    let state = crate::private_state::PrivateState::acquire(&config).unwrap();
    assert_eq!(
        std::fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(matches!(
        crate::private_state::PrivateState::acquire(&config),
        Err(GatewayError::AlreadyRunning)
    ));
    drop(state);
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        crate::private_state::PrivateState::acquire(&config),
        Err(GatewayError::PrivateState)
    ));
}

#[test]
fn ledger_binds_identity_content_wire_ack_and_profile_across_reopen() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ledger.sqlite");
    let ledger = crate::ledger::Ledger::open(&path, "profile").unwrap();
    assert!(
        ledger
            .claim("send", "request-1", "content-digest", "wire-1")
            .unwrap()
    );
    assert_eq!(
        ledger.claim("send", "request-1", "content-digest", "wire-2"),
        Err(GatewayError::PreviousUnconfirmed)
    );
    assert!(matches!(
        ledger.claim("send", "request-1", "different", "wire-3"),
        Err(GatewayError::Conflict(_))
    ));
    assert_eq!(ledger.confirm("uncorrelated"), Err(GatewayError::Ledger));
    ledger.confirm("wire-1").unwrap();
    assert!(
        !ledger
            .claim("send", "request-1", "content-digest", "wire-4")
            .unwrap()
    );
    assert!(
        ledger
            .claim("send", "request-2", "content-digest", "wire-2")
            .unwrap()
    );
    drop(ledger);
    let reopened = crate::ledger::Ledger::open(&path, "profile").unwrap();
    assert!(
        reopened
            .previous("send", "request-1", "content-digest")
            .unwrap()
    );
    assert_eq!(
        reopened.previous("send", "request-2", "content-digest"),
        Err(GatewayError::PreviousUnconfirmed)
    );
    assert_eq!(reopened.facts().unwrap().len(), 2);
    drop(reopened);
    assert!(matches!(
        crate::ledger::Ledger::open(&path, "different"),
        Err(GatewayError::Conflict(_))
    ));
}

#[test]
fn callback_claims_do_not_rebind_or_reexecute_and_suppress_stale_replies() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ledger.sqlite");
    let ledger = crate::ledger::Ledger::open(&path, "profile").unwrap();
    let event = normalize_message(&callback()).unwrap().unwrap();
    assert!(ledger.admit(&event).unwrap());
    assert!(!ledger.admit(&event).unwrap());
    let mut changed = event.clone();
    changed.sender_id = "bob".into();
    assert!(matches!(
        ledger.admit(&changed),
        Err(GatewayError::Conflict(_))
    ));
    changed = event.clone();
    changed
        .metadata
        .insert("request_id".into(), json!("different-callback"));
    assert!(matches!(
        ledger.admit(&changed),
        Err(GatewayError::Conflict(_))
    ));
    changed = event.clone();
    changed.event_id = "message-2".into();
    assert!(matches!(
        ledger.admit(&changed),
        Err(GatewayError::Conflict(_))
    ));
    changed
        .metadata
        .insert("request_id".into(), json!("callback-2"));
    assert!(ledger.admit(&changed).unwrap());
    assert!(!ledger.is_latest(&event).unwrap());
    assert!(ledger.is_latest(&changed).unwrap());
    ledger
        .ready(&changed.event_id, "saved reply", None)
        .unwrap();
    assert_eq!(ledger.ready_replies(8).unwrap()[0].text, "saved reply");
    drop(ledger);
    let reopened = crate::ledger::Ledger::open(&path, "profile").unwrap();
    assert!(!reopened.admit(&event).unwrap());
    assert!(!reopened.admit(&changed).unwrap());
    assert_eq!(reopened.ready_replies(8).unwrap().len(), 1);
}

#[test]
fn webhook_receipt_has_bounded_stable_key_digest_and_exact_fields() {
    use serde_json::json;

    let valid = json!({"key":"channel-turn-123", "content_sha256":"a".repeat(64)});
    assert!(crate::webhook::parse_receipt(&valid).is_ok());
    for invalid in [
        json!({"key":"turn-123", "content_sha256":"a".repeat(64)}),
        json!({"key":"channel-", "content_sha256":"a".repeat(64)}),
        json!({"key":format!("channel-{}", "a".repeat(249)), "content_sha256":"a".repeat(64)}),
        json!({"key":"channel-turn", "content_sha256":"A".repeat(64)}),
        json!({"key":"channel-turn", "content_sha256":"a".repeat(63)}),
        json!({"key":"channel-turn", "content_sha256":"a".repeat(64), "extra":true}),
    ] {
        assert!(crate::webhook::parse_receipt(&invalid).is_err());
    }
}

#[test]
fn version_one_delivery_ledger_migrates_without_changing_at_most_once_facts() {
    use rusqlite::Connection;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("delivery.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE inbound (event_id TEXT PRIMARY KEY, digest TEXT NOT NULL,
              callback_id TEXT NOT NULL UNIQUE, sender_id TEXT NOT NULL, conversation_id TEXT NOT NULL,
              state TEXT NOT NULL CHECK(state IN ('processing','ready','completed','unknown')), reply TEXT);
             CREATE TABLE deliveries (kind TEXT NOT NULL, request_id TEXT NOT NULL,
              digest TEXT NOT NULL, wire_id TEXT NOT NULL UNIQUE,
              status TEXT NOT NULL CHECK(status IN ('confirmed','unknown')),
              PRIMARY KEY(kind, request_id));
             CREATE TABLE gateway_profile (id INTEGER PRIMARY KEY CHECK(id=1), digest TEXT NOT NULL);
             INSERT INTO gateway_profile(id,digest) VALUES(1,'profile');
             INSERT INTO deliveries(kind,request_id,digest,wire_id,status)
              VALUES('send','confirmed-request','d1','wire-confirmed','confirmed'),
                    ('send','unknown-request','d2','wire-unknown','unknown');
             PRAGMA user_version=1;",
        )
        .unwrap();
    drop(connection);

    let ledger = crate::ledger::Ledger::open(&path, "profile").unwrap();
    assert!(ledger.previous("send", "confirmed-request", "d1").unwrap());
    assert_eq!(
        ledger.previous("send", "unknown-request", "d2"),
        Err(GatewayError::PreviousUnconfirmed)
    );
    assert_eq!(ledger.facts().unwrap().len(), 2);
    let version: i64 = Connection::open(&path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2);
}
