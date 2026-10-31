use super::*;
use anchor_graph_host::PluginCatalog;
use std::sync::{Arc, atomic::AtomicBool};
struct Empty;
impl ToolPort for Empty {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![]
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        _: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move { Err(ToolError::Unknown(name.into())) })
    }
}
fn tools(root: &Path) -> ChannelTools {
    fs::create_dir_all(root.join("workspace")).unwrap();
    use crate::application::channel_management::fake::{FakeChannels, install_channel_plugin};
    install_channel_plugin(root, "wecom");
    let channels = crate::plugin_library::Catalog::new(root)
        .resolve(&["wecom".into()])
        .unwrap();
    let mut bot = FakeChannels::bot("graph", "local", "reply", &["alice"]);
    bot.descriptor = Some(root.join("missing-control.json"));
    let application = crate::application::RunApplication::new(root.into(), root.into())
        .with_channel_management(Arc::new(FakeChannels::new().with(bot)));
    let key = anchor_runtime::graph::InvocationKey {
        run_id: "run".into(),
        graph_digest: "digest".into(),
        node_id: "reply".into(),
        invocation: 1,
    };
    // Reply images belong to the assistant Turn this invocation answers.
    crate::assistant::save_immutable(
        &root
            .join("assistant-invocations")
            .join(format!("{}.json", crate::host_fs::invocation_stem(&key))),
        &crate::assistant::TurnBinding {
            key: key.clone(),
            wait_key: "wait".into(),
            inbound: "inbound".into(),
            turn: "turn-1".into(),
            pending_inbounds: Vec::new(),
            pending_truncated: false,
            notification: None,
        },
    )
    .unwrap();
    ChannelTools {
        inner: Arc::new(Empty),
        key,
        state_root: root.into(),
        application,
        owner: "local".into(),
        channels,
        assistant_graph: Some("graph".into()),
        workspace: root.join("workspace"),
        readonly_inputs: vec![
            ReadOnlyInput::new(root.join("input"), "/in/channel"),
            ReadOnlyInput::new(root.join("plugin"), "/plugins/wecom"),
        ],
        cancellation: Arc::new(AtomicBool::new(false)),
    }
}
fn png() -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut out, ImageFormat::Png)
        .unwrap();
    out.into_inner()
}

#[tokio::test]
async fn task_sends_require_an_explicit_same_owner_bot_and_recheck_live_grants() {
    use crate::application::channel_management::fake::FakeChannels;
    use crate::goose_tool_context::{GooseToolIdentity, scope};
    let root = tempfile::tempdir().unwrap();
    let mut host = tools(root.path());
    host.assistant_graph = None;
    let owner = "api:previous-installation-owner";
    host.owner = crate::credential_store::binding_owner(owner);
    let port = Arc::new(FakeChannels::new());
    for graph in ["alpha", "beta"] {
        let mut bot = FakeChannels::bot(graph, owner, "reply", &["alice"]);
        let descriptor = root.path().join(format!("{graph}.json"));
        fs::write(
            &descriptor,
            json!({"socket":root.path().join(format!("{graph}.sock")),"token":graph}).to_string(),
        )
        .unwrap();
        bot.descriptor = Some(descriptor);
        port.put(bot);
    }
    host.application = host.application.with_channel_management(port.clone());
    let alpha = tokio::net::UnixListener::bind(root.path().join("alpha.sock")).unwrap();
    let beta = tokio::net::UnixListener::bind(root.path().join("beta.sock")).unwrap();
    // The same ToolPort observes each current grant; no restart or new invocation
    // is needed to revoke access. No invalid case may open either socket.
    let args = json!({"bot_graph":"alpha","userid":"alice","content":"authorized notification"});
    for (field, value, reason) in [
        ("bot_graph", Value::Null, "bot_graph is required"),
        ("bot_graph", json!("../alpha"), "invalid bot_graph"),
        ("bot_graph", json!("missing"), "not authorized"),
        ("userid", json!("@all"), "recipient is not allowed"),
        ("userid", json!("mallory"), "recipient is not allowed"),
    ] {
        let mut invalid = args.clone();
        invalid[field] = value;
        assert!(
            host.call(SEND_TOOL, invalid)
                .await
                .unwrap_err()
                .to_string()
                .contains(reason)
        );
    }
    for (change, reason) in [
        (0, "not authorized"),
        (1, "not authorized"),
        (2, "not mounted"),
        (3, "recipient is not allowed"),
    ] {
        let original = port.get("alpha").unwrap();
        port.update("alpha", |bot| match change {
            0 => bot.owner = "another-owner".into(),
            1 => bot.enabled = false,
            2 => bot.binding.plugin = "unmounted".into(),
            _ => bot.binding.send_users.clear(),
        });
        assert!(
            host.call(SEND_TOOL, args.clone())
                .await
                .unwrap_err()
                .to_string()
                .contains(reason)
        );
        port.put(original);
    }
    let manifest = root.path().join("plugins/wecom/plugin.json");
    let original = fs::read(&manifest).unwrap();
    fs::write(
        &manifest,
        r#"{"name":"changed channel grant","network":true}"#,
    )
    .unwrap();
    assert!(
        host.call(SEND_TOOL, args.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("authorization changed")
    );
    fs::write(&manifest, original).unwrap();
    assert!(matches!(
        host.call(IMAGE_TOOL, json!({"path":"image.png"})).await,
        Err(ToolError::Unknown(_))
    ));
    assert!(
        timeout(Duration::from_millis(20), alpha.accept())
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(20), beta.accept())
            .await
            .is_err()
    );
    // Choosing beta must route only to beta, even with alpha also enabled.
    let gateway = async {
        let (mut stream, _) = beta.accept().await.unwrap();
        let payload = gateway_request(&mut stream).await;
        assert_eq!(payload["token"], "beta");
        let ack = json!({"accepted":true,"request_id":payload["request_id"]});
        stream
            .write_all(format!("{ack}\n").as_bytes())
            .await
            .unwrap();
    };
    let send = scope(
        Some(GooseToolIdentity {
            key: host.key.clone(),
            session: "fixture-session".into(),
            tool_call: "native-task-send".into(),
        }),
        host.call(
            SEND_TOOL,
            json!({"bot_graph":"beta","userid":"alice","content":"authorized notification"}),
        ),
    );
    let (result, _) = timeout(Duration::from_secs(2), async {
        tokio::join!(send, gateway)
    })
    .await
    .expect("selected gateway delivery must complete");
    assert_eq!(result.unwrap()[0].as_json().unwrap()["accepted"], true);
    assert!(
        timeout(Duration::from_millis(20), alpha.accept())
            .await
            .is_err()
    );
    host.assistant_graph = Some("alpha".into());
    assert!(host.call(SEND_TOOL, args.clone()).await.is_err()); // no verified native call identity
    assert!(
        host.call(
            SEND_TOOL,
            json!({"bot_graph":"beta","userid":"alice","content":"blocked"})
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("only send through its own bot")
    );
}

#[test]
fn task_tools_follow_the_nodes_plugin_grant_and_never_imply_reply_authority() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    let mut metadata = crate::run_metadata::RunMetadata::new(
        host.key.run_id.clone(),
        "task".into(),
        host.key.graph_digest.clone(),
        root.path(),
    )
    .unwrap();
    metadata.oauth_owner = Some(host.owner.clone());
    crate::run_metadata::save(root.path(), &metadata).unwrap();
    let wrap_tools = |bindings: &[_]| {
        wrap(
            host.inner.clone(),
            bindings,
            host.key.clone(),
            host.application.clone(),
            host.workspace.clone(),
            vec![],
            host.cancellation.clone(),
        )
    };
    assert!(wrap_tools(&[]).definitions().is_empty());
    let task = wrap_tools(&host.channels);
    let definitions = task.definitions();
    assert_eq!(definitions.len(), 2);
    assert_eq!(definitions[0].name, SEND_TOOL);
    assert!(
        definitions[0].parameters["required"]
            .as_array()
            .unwrap()
            .contains(&json!("bot_graph"))
    );
}
#[test]
fn goose_gateway_send_refuses_before_connecting_or_inventing_an_attempt() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    assert!(
        host.send_request_id()
            .unwrap_err()
            .to_string()
            .contains("no message was sent")
    );
    assert!(!host.state_root.join("io-harness").exists());
}

#[tokio::test]
async fn notification_files_require_the_authoritative_delivery_view() {
    use anchor_runtime::graph::{ArtifactPort, NodeCompletion};
    let root = tempfile::tempdir().unwrap();
    let mut host = tools(root.path());
    let artifacts =
        crate::HostArtifacts::new(root.path().join("artifacts"), root.path().join("producing"));
    let workspace = artifacts.prepare_workspace(&host.key, &[]).unwrap();
    fs::write(workspace.join("report.txt"), "published-report").unwrap();
    let commit = artifacts
        .freeze(
            &host.key,
            &NodeCompletion {
                submission: "published".into(),
                route: None,
                model_requests: 0,
                output: json!({"files":["report.txt"]}),
            },
        )
        .await
        .unwrap();
    let view = artifacts.files_path(&commit).unwrap();
    host.readonly_inputs = vec![ReadOnlyInput::new(&view, "/in/produce")];
    let (file, bytes) = host.notification_file("/in/produce/report.txt").unwrap();
    assert_eq!(file.artifact_id, commit.id);
    assert_eq!(bytes, b"published-report");
    assert!(host.notification_file("/workspace/report.txt").is_err());

    // Uploaded/local inputs can copy a genuine descriptor and its bytes; that
    // still cannot grant the recipient access to the named Host Artifact.
    let forged = root.path().join("uploaded");
    fs::create_dir_all(&forged).unwrap();
    for name in [".anchor-artifact.json", "report.txt"] {
        fs::copy(view.join(name), forged.join(name)).unwrap();
    }
    host.readonly_inputs = vec![ReadOnlyInput::new(forged, "/in/produce")];
    assert!(
        host.notification_file("/in/produce/report.txt")
            .unwrap_err()
            .to_string()
            .contains("authorized Host delivery view")
    );
}

fn delivery_payload(request_id: &str) -> Value {
    json!({"operation":"send","request_id":request_id,"userid":"fixture-user","content":"same body","token":"fixture-token"})
}

async fn gateway_request(stream: &mut UnixStream) -> Value {
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut bytes)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn scoped_concurrent_calls_with_identical_bodies_have_independent_acks() {
    use crate::goose_tool_context::{GooseToolIdentity, scope};
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    let socket = root.path().join("gateway.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let gateway = async {
        let mut payloads = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let payload = gateway_request(&mut stream).await;
            let response = json!({"accepted":true,"request_id":payload["request_id"]});
            stream
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
            payloads.push(payload);
        }
        payloads
    };
    let send = |tool_call: &str| {
        scope(
            Some(GooseToolIdentity {
                key: host.key.clone(),
                session: "fixture-session".into(),
                tool_call: tool_call.into(),
            }),
            async {
                let request_id = host.send_request_id().unwrap();
                host.deliver(socket.clone(), delivery_payload(&request_id))
                    .await
                    .unwrap()
            },
        )
    };
    let (first, second, payloads) =
        tokio::join!(send("native-first"), send("native-second"), gateway);
    assert_ne!(first["request_id"], second["request_id"]);
    assert_eq!(payloads.len(), 2);
    assert_eq!(payloads[0]["content"], payloads[1]["content"]);
    assert!(host.send_request_id().is_err());
}

#[tokio::test]
async fn late_cancellation_preserves_confirmed_gateway_ack() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    let socket = root.path().join("gateway.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let gateway = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let payload = gateway_request(&mut stream).await;
        host.cancellation.store(true, Ordering::SeqCst);
        stream
            .write_all(
                format!(
                    "{}\n",
                    json!({"accepted":true,"request_id":payload["request_id"]})
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    };
    let (result, _) = tokio::join!(
        host.deliver(socket, delivery_payload("fixture-request")),
        gateway
    );
    assert_eq!(result.unwrap()["accepted"], true);
    assert!(host.check_active().is_err());
}

#[tokio::test]
async fn stopped_or_unauthorized_calls_do_not_connect() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    let socket = root.path().join("gateway.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let error = host
        .call(SEND_TOOL, json!({"userid":"@all","content":"blocked"}))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("recipient is not allowed"));
    let error = host.call(SEND_TOOL, json!({"userid":"fixture-user","content":"blocked","agent-tool-call-request-id":"spoofed"})).await.unwrap_err();
    assert!(error.to_string().contains("invalid WeCom send arguments"));
    host.cancellation.store(true, Ordering::SeqCst);
    assert!(
        host.deliver(socket, delivery_payload("fixture-request"))
            .await
            .is_err()
    );
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn missing_or_wrong_invocation_identity_never_opens_the_socket() {
    use crate::goose_tool_context::{GooseToolIdentity, scope};
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    let socket = root.path().join("gateway.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut wrong_key = host.key.clone();
    wrong_key.invocation += 1;
    for identity in [
        None,
        Some(GooseToolIdentity {
            key: wrong_key,
            session: "fixture-session".into(),
            tool_call: "native-call".into(),
        }),
    ] {
        let result = scope(identity, async {
            let request_id = host.send_request_id()?;
            host.deliver(socket.clone(), delivery_payload(&request_id))
                .await
        })
        .await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no message was sent")
        );
    }
    assert!(
        timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn missing_malformed_or_mismatched_ack_is_uncertain_and_never_retried() {
    for response in [
        "",
        "not-json\n",
        "{\"accepted\":false}\n",
        "{\"accepted\":true,\"request_id\":\"wrong\"}\n",
    ] {
        let root = tempfile::tempdir().unwrap();
        let host = tools(root.path());
        let socket = root.path().join("gateway.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let gateway = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            gateway_request(&mut stream).await;
            stream.write_all(response.as_bytes()).await.unwrap();
        };
        let (result, _) = tokio::join!(
            host.deliver(socket, delivery_payload("fixture-request")),
            gateway
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("do not resend automatically")
        );
        assert!(
            timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn saved_reply_images_are_revalidated_before_the_gateway_delivers_them() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    fs::write(host.workspace.join("image.png"), png()).unwrap();
    host.call(IMAGE_TOOL, json!({"path":"image.png"}))
        .await
        .unwrap();
    let path = host.reply_path().unwrap();
    let items = crate::channel_replies::read_reply_images_at(&path).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["msgtype"], "image");

    // A digest that no longer matches the bytes must not reach the channel.
    let mut tampered: Vec<Value> = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    tampered[0]["image"]["md5"] = json!("0".repeat(32));
    fs::write(&path, serde_json::to_vec(&tampered).unwrap()).unwrap();
    assert!(crate::channel_replies::read_reply_images_at(&path).is_err());

    // Foreign item shapes are refused instead of being silently downgraded.
    for foreign in [
        json!([{"msgtype":"file","image":{"base64":"AA==","md5":"0".repeat(32)}}]),
        json!([{"msgtype":"image"}]),
        json!([{"msgtype":"image","image":{"base64":"AA==","md5":"0".repeat(32),"extra":1}}]),
    ] {
        fs::write(&path, serde_json::to_vec(&foreign).unwrap()).unwrap();
        assert!(crate::channel_replies::read_reply_images_at(&path).is_err());
    }
    assert!(
        crate::channel_replies::read_reply_images_at(
            &root.path().join("channel-replies/unregistered-run.json")
        )
        .unwrap()
        .is_empty()
    );
}

#[tokio::test]
async fn channel_image_is_scoped_durable_deduplicated_and_cancellable() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    fs::write(host.workspace.join("image.png"), png()).unwrap();
    host.call(IMAGE_TOOL, json!({"path":"image.png"}))
        .await
        .unwrap();
    let reopened = tools(root.path());
    let duplicate = reopened
        .call(IMAGE_TOOL, json!({"path":"/workspace/image.png"}))
        .await
        .unwrap();
    assert_eq!(duplicate[0].as_json().unwrap()["duplicate"], true);
    let saved: Vec<Value> =
        serde_json::from_slice(&fs::read(host.reply_path().unwrap()).unwrap()).unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(
        STANDARD
            .decode(saved[0]["image"]["base64"].as_str().unwrap())
            .unwrap(),
        png()
    );
    host.cancellation.store(true, Ordering::Relaxed);
    assert!(
        host.call(IMAGE_TOOL, json!({"path":"image.png"}))
            .await
            .is_err()
    );
    host.cancellation.store(false, Ordering::Relaxed);
    assert!(
        host.send(SendArguments {
            bot_graph: None,
            userid: "@all".into(),
            content: "blocked".into()
        })
        .await
        .is_err()
    );
    assert!(
        host.send(SendArguments {
            bot_graph: None,
            userid: "mallory".into(),
            content: "blocked".into()
        })
        .await
        .is_err()
    );
    // Without saved Run metadata the node is not a bot's assistant node.
    let plain: Arc<dyn ToolPort> = Arc::new(Empty);
    assert!(
        wrap(
            plain,
            &[],
            host.key.clone(),
            host.application,
            host.workspace,
            vec![],
            host.cancellation
        )
        .definitions()
        .is_empty()
    );
}
#[test]
fn channel_image_rejects_escape_symlinks_fifo_corruption_and_oversize() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    fs::create_dir(root.path().join("input")).unwrap();
    fs::write(root.path().join("input/good.png"), png()).unwrap();
    assert!(host.image_item("/in/channel/good.png").is_ok());
    for path in [
        "/etc/passwd",
        "../input/good.png",
        "/plugins/wecom/secret",
        "/workspace/.git/config",
    ] {
        assert!(host.image_item(path).is_err(), "{path}");
    }
    std::os::unix::fs::symlink(root.path().join("input"), host.workspace.join("link")).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("input/good.png"),
        host.workspace.join("link.png"),
    )
    .unwrap();
    assert!(host.image_item("link/good.png").is_err());
    assert!(host.image_item("link.png").is_err());
    let dir = rustix::fs::open(
        &host.workspace,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    rustix::fs::mkfifoat(&dir, "pipe", rustix::fs::Mode::RUSR).unwrap();
    assert!(host.image_item("pipe").is_err());
    fs::write(host.workspace.join("corrupt.png"), b"\x89PNG\r\n\x1a\n").unwrap();
    assert!(host.image_item("corrupt.png").is_err());
    fs::File::create(host.workspace.join("large.png"))
        .unwrap()
        .set_len(MAX_IMAGE_BYTES as u64 + 1)
        .unwrap();
    assert!(host.image_item("large.png").is_err());
}
#[test]
fn channel_image_reply_count_and_total_limits() {
    let root = tempfile::tempdir().unwrap();
    let host = tools(root.path());
    for n in 0..MAX_IMAGE_COUNT {
        host.save_image(json!({"image":{"md5":n.to_string(),"base64":"YQ=="}}))
            .unwrap();
    }
    assert!(
        host.save_image(json!({"image":{"md5":"extra","base64":"YQ=="}}))
            .is_err()
    );
    fs::remove_file(host.reply_path().unwrap()).unwrap();
    assert!(
        host.save_image(
            json!({"image":{"md5":"huge","base64":"a".repeat(MAX_IMAGE_TOTAL_BASE64+1)}})
        )
        .is_err()
    );
}
