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
fn inbound_media_callbacks_become_download_descriptors() {
    let url = "https://media.example.invalid/signed-ciphertext-url";
    let key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    for kind in ["image", "file", "voice"] {
        let mut frame = callback();
        frame["body"]["msgtype"] = json!(kind);
        frame["body"].as_object_mut().unwrap().remove("text");
        frame["body"][kind] = json!({"url":url, "aeskey":key, "filename":"report.pdf"});
        let event = normalize_message(&frame).unwrap().unwrap();
        assert_eq!(event.message_type, kind);
        assert_eq!(event.text, "");
        assert_eq!(event.attachments.len(), 1);
        assert_eq!(event.attachments[0]["kind"], kind);
        assert_eq!(event.attachments[0]["url"], url);
        assert_eq!(event.attachments[0]["aeskey"], key);
        assert_eq!(event.attachments[0]["name"], "report.pdf");
    }

    let mut mixed = callback();
    mixed["body"]["msgtype"] = json!("mixed");
    mixed["body"].as_object_mut().unwrap().remove("text");
    mixed["body"]["mixed"] = json!({"msg_item":[
        {"msgtype":"text","text":{"content":"caption"}},
        {"msgtype":"image","image":{"url":url,"aes_key":key}},
        {"msgtype":"file","file":{"url":url,"aeskey":key}}
    ]});
    let event = normalize_message(&mixed).unwrap().unwrap();
    assert_eq!(event.message_type, "mixed");
    assert_eq!(event.text, "caption");
    assert_eq!(event.attachments.len(), 2);
    assert_eq!(event.attachments[1]["kind"], "file");
}

#[test]
fn unsupported_or_malformed_media_shapes_are_rejected() {
    let url = "https://media.example.invalid/signed-ciphertext-url";
    let key = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

    // Media attached to a text message would reach the Host as a text event
    // carrying attachments, which its contract forbids.
    for kind in ["image", "file", "voice"] {
        let mut frame = callback();
        frame["body"][kind] = json!({"url":url,"aeskey":key});
        assert_eq!(
            normalize_message(&frame),
            Err(GatewayError::UnsupportedMedia)
        );
    }

    // Kinds this transport cannot fetch stay unsupported.
    for kind in ["video", "audio", "media"] {
        let mut frame = callback();
        frame["body"]["msgtype"] = json!(kind);
        frame["body"][kind] = json!({"url":url,"aeskey":key});
        assert_eq!(
            normalize_message(&frame),
            Err(GatewayError::UnsupportedMedia)
        );
    }

    // A descriptor must carry a fetchable URL and the platform AES key.
    for payload in [
        json!({"url":"http://media.example.invalid/plain","aeskey":key}),
        json!({"url":"file:///etc/passwd","aeskey":key}),
        json!({"url":url}),
        json!({"url":url,"aeskey":""}),
        json!({"aeskey":key}),
    ] {
        let mut frame = callback();
        frame["body"]["msgtype"] = json!("image");
        frame["body"].as_object_mut().unwrap().remove("text");
        frame["body"]["image"] = payload.clone();
        let result = normalize_message(&frame);
        assert!(
            matches!(result, Err(GatewayError::Invalid(_))),
            "{payload} produced {result:?}"
        );
    }

    // Inline bytes in a platform payload are never trusted: only the URL is
    // downloaded, so a frame cannot smuggle unverified content into the Host.
    let mut inline = callback();
    inline["body"]["msgtype"] = json!("image");
    inline["body"].as_object_mut().unwrap().remove("text");
    inline["body"]["image"] = json!({"url":url,"aeskey":key,"data_base64":"aGVsbG8="});
    let event = normalize_message(&inline).unwrap().unwrap();
    assert_eq!(event.attachments.len(), 1);
    assert_eq!(event.attachments[0]["url"], url);
    assert!(event.attachments[0].get("data_base64").is_none());

    // A hidden or malformed mixed media item is never silently dropped.
    let mut hidden = callback();
    hidden["body"]["text"]["image"] = json!({"url":url,"aeskey":key});
    assert_eq!(
        normalize_message(&hidden),
        Err(GatewayError::UnsupportedMedia)
    );
    for items in [
        json!([{"msgtype":"image"}]),
        json!([{"msgtype":"image","image":{"url":url},"extra":true}]),
        json!([{"msgtype":"template_card"}]),
    ] {
        let mut mixed = callback();
        mixed["body"]["msgtype"] = json!("mixed");
        mixed["body"]["mixed"] = json!({"msg_item":items});
        let result = normalize_message(&mixed);
        assert!(result.is_err(), "{items} produced {result:?}");
    }
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
    frame["body"]["chattype"] = json!("group");
    assert_eq!(
        normalize_message(&frame),
        Err(GatewayError::Invalid(
            "only private WeCom callbacks are supported"
        ))
    );
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
        timeout: Some(std::time::Duration::from_secs(2)),
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
        .ready(&changed.event_id, "saved reply", None, None)
        .unwrap();
    assert_eq!(ledger.ready_replies(8).unwrap()[0].text, "saved reply");
    drop(ledger);
    let reopened = crate::ledger::Ledger::open(&path, "profile").unwrap();
    assert!(!reopened.admit(&event).unwrap());
    assert!(!reopened.admit(&changed).unwrap());
    assert_eq!(reopened.ready_replies(8).unwrap().len(), 1);
}

#[test]
fn retryable_events_require_unknown_state_without_reply_claim() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ledger.sqlite");
    let ledger = crate::ledger::Ledger::open(&path, "profile").unwrap();
    let event = normalize_message(&callback()).unwrap().unwrap();
    let now = crate::ledger::current_time_millis().unwrap();
    let window = crate::DEFAULT_RECOVERY_WINDOW_SECS as i64 * 1000;
    assert!(ledger.admit(&event).unwrap());
    assert!(ledger.retryable_events(now, window).unwrap().is_empty());
    ledger.finish_event(&event.event_id, false).unwrap();
    assert_eq!(
        ledger.retryable_events(now, window).unwrap(),
        vec![event.clone()]
    );

    assert!(
        ledger
            .claim("reply", &event.event_id, "reply-digest", "reply-wire")
            .unwrap()
    );
    assert!(ledger.retryable_events(now, window).unwrap().is_empty());
    ledger.confirm("reply-wire").unwrap();
    assert!(ledger.retryable_events(now, window).unwrap().is_empty());
}

#[test]
fn stale_undelivered_inbounds_are_retired_instead_of_replayed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("ledger.sqlite");
    let ledger = crate::ledger::Ledger::open(&path, "profile").unwrap();
    let event = normalize_message(&callback()).unwrap().unwrap();
    assert!(ledger.admit(&event).unwrap());
    ledger.finish_event(&event.event_id, false).unwrap();
    let now = crate::ledger::current_time_millis().unwrap();
    let window = 60_000;

    // Inside the window the event is still recoverable.
    assert_eq!(ledger.retryable_events(now, window).unwrap().len(), 1);
    assert!(ledger.is_recent(&event.event_id, now, window).unwrap());

    // A restart long afterwards retires it instead of replaying it.
    let later = now + window * 10;
    assert!(!ledger.is_recent(&event.event_id, later, window).unwrap());
    assert_eq!(ledger.retire_stale_inbounds(later, window).unwrap(), 1);
    assert!(ledger.retryable_events(later, window).unwrap().is_empty());
    assert_eq!(ledger.retire_stale_inbounds(later, window).unwrap(), 0);

    // A Host refusal retires one event immediately.
    let mut refused_frame = callback();
    refused_frame["body"]["msgid"] = json!("message-refused");
    refused_frame["headers"]["req_id"] = json!("callback-refused");
    let refused = normalize_message(&refused_frame).unwrap().unwrap();
    assert!(ledger.admit(&refused).unwrap());
    ledger.reject(&refused.event_id).unwrap();
    assert!(ledger.retryable_events(now, window).unwrap().is_empty());
}

#[test]
fn the_hosts_cancellation_decides_which_turn_is_live() {
    let root = tempfile::tempdir().unwrap();
    let ledger =
        crate::ledger::Ledger::open(&root.path().join("ledger.sqlite"), "profile").unwrap();
    let first = normalize_message(&callback()).unwrap().unwrap();
    let mut frame = callback();
    frame["body"]["msgid"] = json!("message-2");
    frame["headers"]["req_id"] = json!("callback-2");
    let second = normalize_message(&frame).unwrap().unwrap();
    assert!(ledger.admit(&first).unwrap());
    assert!(ledger.admit(&second).unwrap());

    // Arrival order alone says the second message took the conversation over.
    assert!(ledger.is_latest(&second).unwrap());
    assert!(!ledger.is_latest(&first).unwrap());

    // The Host cancelled the second turn, so the first one is the live turn
    // again: it keeps its progress bubble and its reply.
    ledger.supersede(&second.event_id).unwrap();
    assert!(ledger.is_latest(&first).unwrap());
    // A cancelled message is still the newest row, but it no longer shadows
    // the running turn; its own reply is suppressed by the Host's refusal.
    assert!(ledger.is_latest(&second).unwrap());
}

#[test]
fn version_three_ledger_gains_the_recovery_window_column() {
    use rusqlite::Connection;

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("delivery.sqlite");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE inbound (event_id TEXT PRIMARY KEY, digest TEXT NOT NULL,
              callback_id TEXT NOT NULL UNIQUE, sender_id TEXT NOT NULL, conversation_id TEXT NOT NULL,
              state TEXT NOT NULL CHECK(state IN ('processing','ready','completed','unknown')), reply TEXT,
              event_json TEXT, receipt_key TEXT, receipt_digest TEXT, reply_items TEXT);
             CREATE TABLE deliveries (kind TEXT NOT NULL, request_id TEXT NOT NULL,
              digest TEXT NOT NULL, wire_id TEXT NOT NULL UNIQUE,
              status TEXT NOT NULL CHECK(status IN ('confirmed','unknown')),
              PRIMARY KEY(kind, request_id));
             CREATE TABLE gateway_profile (id INTEGER PRIMARY KEY CHECK(id=1), digest TEXT NOT NULL);
             CREATE UNIQUE INDEX inbound_receipt_key_unique ON inbound(receipt_key);
             CREATE TABLE settlement_outbox (receipt_key TEXT PRIMARY KEY, content_sha256 TEXT NOT NULL,
              status TEXT NOT NULL CHECK(status IN ('confirmed','unknown','suppressed')),
              event_id TEXT NOT NULL, event_json TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
              retry_at INTEGER NOT NULL DEFAULT 0, delivered INTEGER NOT NULL DEFAULT 0
              CHECK(delivered IN (0,1)));
             INSERT INTO gateway_profile(id,digest) VALUES(1,'profile');
             INSERT INTO inbound(event_id,digest,callback_id,sender_id,conversation_id,state)
              VALUES('saved','d','callback-saved','alice','alice','unknown');
             PRAGMA user_version=3;",
        )
        .unwrap();
    drop(connection);

    let ledger = crate::ledger::Ledger::open(&path, "profile").unwrap();
    let connection = Connection::open(&path).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
    let columns: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_table_info('inbound') WHERE name='created_at'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 1);
    // The rebuilt table keeps its rows and accepts the new terminal state.
    let saved: i64 = connection
        .query_row(
            "SELECT count(*) FROM inbound WHERE event_id='saved'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, 1);
    ledger.reject("saved").unwrap();
    drop(ledger);
    // A migrated database is recognised on reopen.
    let reopened = crate::ledger::Ledger::open(&path, "profile").unwrap();
    assert!(reopened.reject("saved").is_err());
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
    let connection = Connection::open(&path).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5);
    // The v3/v4/v5 columns must exist on a database migrated from v1.
    let columns: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_table_info('inbound') WHERE name IN ('reply_items','created_at','superseded')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(columns, 3);
}

#[test]
fn reply_images_require_the_exact_validated_shape() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use md5::{Digest as _, Md5};

    let bytes = STANDARD
        .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
        .unwrap();
    let digest = format!("{:x}", Md5::digest(&bytes));
    let encoded = STANDARD.encode(&bytes);
    let valid = json!([{"msgtype":"image","image":{"base64":encoded,"md5":digest}}]);
    let items = crate::media::reply_images(Some(&valid)).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].chunk_count(), 1);
    assert_eq!(items[0].name, "reply.png");

    // A wrong digest, a foreign shape, an unsupported format and an empty list
    // are all refused instead of being forwarded to the platform.
    for invalid in [
        json!([{"msgtype":"image","image":{"base64":encoded,"md5":"0".repeat(32)}}]),
        json!([{"msgtype":"file","image":{"base64":encoded,"md5":digest}}]),
        json!([{"msgtype":"image","image":{"base64":encoded,"md5":digest,"name":"x"}}]),
        json!([{"msgtype":"image","image":{"base64":STANDARD.encode(b"not an image"),"md5":digest}}]),
        json!([]),
    ] {
        assert!(crate::media::reply_images(Some(&invalid)).is_err());
    }
    assert!(crate::media::reply_images(None).unwrap().is_empty());
}

#[test]
fn platform_media_decryption_follows_the_official_aes_conventions() {
    use aes::Aes256;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use cbc::cipher::{BlockEncryptMut as _, KeyIvInit as _, block_padding::Pkcs7};

    let key = [0x5au8; 32];
    let encoded_key = STANDARD.encode(key);
    assert_eq!(encoded_key.len(), 44);
    let plaintext = b"platform media payload";
    let mut buffer = vec![0u8; plaintext.len() + 16];
    buffer[..plaintext.len()].copy_from_slice(plaintext);
    let encrypted = cbc::Encryptor::<Aes256>::new_from_slices(&key, &key[..16])
        .unwrap()
        .encrypt_padded_mut::<Pkcs7>(&mut buffer, plaintext.len())
        .unwrap()
        .to_vec();
    assert_eq!(
        crate::media::decrypt_media(&encrypted, &encoded_key).unwrap(),
        plaintext
    );
    // The platform omits base64 padding; the transport restores it.
    let unpadded = encoded_key.trim_end_matches('=');
    assert_eq!(
        crate::media::decrypt_media(&encrypted, unpadded).unwrap(),
        plaintext
    );

    // A wrong key, a short key and corrupted padding must all fail closed.
    let wrong = STANDARD.encode([0x11u8; 32]);
    assert_eq!(
        crate::media::decrypt_media(&encrypted, &wrong),
        Err(GatewayError::Media)
    );
    assert_eq!(
        crate::media::decrypt_media(&encrypted, &STANDARD.encode([0u8; 16])),
        Err(GatewayError::Media)
    );
    let mut corrupted = encrypted.clone();
    let last = corrupted.len() - 1;
    corrupted[last] = 0xff;
    assert_eq!(
        crate::media::decrypt_media(&corrupted, &encoded_key),
        Err(GatewayError::Media)
    );
}

#[test]
fn media_urls_filenames_and_fallbacks_stay_inside_the_transport_contract() {
    use crate::media::{attachment_name, disposition_filename, permitted_media_url};
    use serde_json::json;

    assert!(permitted_media_url(
        "https://media.example.invalid/a?sign=1"
    ));
    assert!(permitted_media_url("http://127.0.0.1:8080/a"));
    assert!(!permitted_media_url("http://media.example.invalid/a"));
    assert!(!permitted_media_url("file:///etc/passwd"));
    assert!(!permitted_media_url(
        "https://user:secret@media.example.invalid/a"
    ));
    assert!(!permitted_media_url(
        "https://media.example.invalid/a#fragment"
    ));

    assert_eq!(
        disposition_filename("attachment; filename=\"report.pdf\""),
        Some("report.pdf".into())
    );
    assert_eq!(
        disposition_filename("attachment; filename*=UTF-8''%E6%8A%A5%E5%91%8A.pdf"),
        Some("报告.pdf".into())
    );
    assert_eq!(disposition_filename("inline"), None);

    let png = b"\x89PNG\r\n\x1a\nrest";
    assert_eq!(
        attachment_name(
            Some("../../etc/passwd"),
            crate::media::InboundKind::File,
            1,
            b"x"
        ),
        "passwd"
    );
    assert_eq!(
        attachment_name(Some(".."), crate::media::InboundKind::Image, 2, png),
        "attachment-2.png"
    );
    assert_eq!(
        attachment_name(None, crate::media::InboundKind::Image, 3, png),
        "attachment-3.png"
    );
    assert_eq!(
        attachment_name(
            Some("voice.amr"),
            crate::media::InboundKind::Voice,
            4,
            b"audio"
        ),
        "voice.amr"
    );
    let long = attachment_name(
        Some(&"a".repeat(400)),
        crate::media::InboundKind::File,
        5,
        b"x",
    );
    assert_eq!(long.chars().count(), 180);
    let _ = json!({"probe": long});
}
