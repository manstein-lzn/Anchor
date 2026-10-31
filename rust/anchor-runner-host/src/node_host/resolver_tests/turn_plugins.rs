use super::*;
use sha2::{Digest, Sha256};

fn assistant_fixture(
    installed: &[&str],
    declared: &[&str],
) -> (tempfile::TempDir, HostIoResolver, NodeExecutionRequest) {
    let declared = declared
        .iter()
        .map(|id| (*id, false, None))
        .collect::<Vec<_>>();
    let plugins = declared
        .iter()
        .map(|(id, _, _)| json!({"id":id,"required":false}))
        .collect::<Vec<_>>();
    let snapshot = GraphSnapshot::from_authoring(json!({
        "objective":"persistent assistant","entry":"wait",
        "agents":{"worker":{"model":"fixture","network":false}},
        "ops":{"wait":{"host":{"operation":"session.wait_input"}},"reply":{"host":{"operation":"session.reply"}}},
        "nodes":[{"id":"wait","op":"wait"},{"id":"agent","agent":"worker","plugins":plugins},{"id":"reply","op":"reply"}],
        "edges":[{"from":"wait","to":"agent"},{"from":"agent","to":"reply"},{"from":"reply","to":"wait"}]
    })).unwrap();
    let (root, resolver, _, mut request) =
        session_fixture_with_snapshot(installed, &declared, &[], Some(snapshot.clone()));
    let state = resolver.local_inputs.state_root();
    let record = resolver.load_run(&request.key.run_id).unwrap().unwrap();
    let bundle = root.path().join("assistant-graph");
    std::fs::create_dir_all(&bundle).unwrap();
    write_bundle(&bundle, &snapshot);
    let mut metadata = crate::run_metadata::RunMetadata::new(
        record.run_id.clone(),
        "assistant".into(),
        record.graph_digest.clone(),
        &bundle,
    )
    .unwrap();
    metadata.assistant = Some(crate::run_metadata::AssistantSource {
        owner: "alice".into(),
        session: "assistant-session".into(),
        wait_node: "wait".into(),
        work_node: "agent".into(),
        reply_node: "reply".into(),
    });
    crate::run_metadata::save(state, &metadata).unwrap();
    request.key.graph_digest = record.graph_digest;
    request.plugins = record.plugin_bindings.into_values().collect();
    (root, resolver, request)
}

fn write_bundle(bundle: &Path, snapshot: &GraphSnapshot) {
    let mut authoring = serde_json::to_value(snapshot).unwrap();
    for node in authoring["nodes"].as_array_mut().unwrap() {
        node.as_object_mut()
            .unwrap()
            .retain(|_, value| !value.is_null());
    }
    std::fs::write(
        bundle.join("graph.json"),
        serde_json::to_vec(&authoring).unwrap(),
    )
    .unwrap();
    let ids = snapshot
        .nodes
        .iter()
        .flat_map(|node| node.plugins.iter().map(|plugin| plugin.id.clone()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|id| json!({"id":id}))
        .collect::<Vec<_>>();
    std::fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":ids}).to_string(),
    )
    .unwrap();
}

fn desired(
    resolver: &HostIoResolver,
    request: &NodeExecutionRequest,
    ids: &[&str],
    network: bool,
    approve: bool,
) {
    let metadata =
        crate::run_metadata::load(resolver.local_inputs.state_root(), &request.key.run_id)
            .unwrap()
            .unwrap();
    let mut snapshot = resolver
        .load_run(&request.key.run_id)
        .unwrap()
        .unwrap()
        .snapshot;
    snapshot
        .nodes
        .iter_mut()
        .find(|node| node.id == "agent")
        .unwrap()
        .plugins = ids
        .iter()
        .map(|id| anchor_runtime::graph::PluginRef::required(*id))
        .collect();
    snapshot.agents.get_mut("worker").unwrap().network = network;
    if approve {
        crate::assistant_plugins::publish(
            resolver.local_inputs.state_root(),
            &metadata.bundle_source,
            &snapshot,
            resolver.plugin_library_root().unwrap(),
        )
        .unwrap();
    }
    write_bundle(&metadata.bundle_source, &snapshot);
}

async fn freeze(
    resolver: &HostIoResolver,
    request: &NodeExecutionRequest,
    turn: &str,
) -> Result<(), String> {
    let wait = InvocationKey {
        node_id: "wait".into(),
        ..request.key.clone()
    };
    resolver
        .freeze_assistant_plugins(&wait, turn, true, request.cancellation.clone())
        .await?;
    let fact = crate::assistant::TurnBinding {
        key: request.key.clone(),
        wait_key: wait.durable_key(),
        inbound: format!("in-{turn}"),
        turn: turn.into(),
        pending_inbounds: vec![],
        pending_truncated: false,
        notification: None,
    };
    let path = resolver
        .local_inputs
        .state_root()
        .join("assistant-invocations")
        .join(format!(
            "{:x}.json",
            Sha256::digest(request.key.durable_key().as_bytes())
        ));
    crate::assistant::save_immutable(&path, &fact)
}

#[tokio::test]
async fn assistant_turn_add_remove_and_retry_preserve_original_run_and_current_turn() {
    let (_root, resolver, mut request) = assistant_fixture(&["old", "new"], &["old"]);
    let original = resolver.load_run(&request.key.run_id).unwrap().unwrap();
    freeze(&resolver, &request, "one").await.unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["old"]
    );
    desired(&resolver, &request, &["new"], false, true);
    freeze(&resolver, &request, "one").await.unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["old"]
    );
    request.key.invocation = 2;
    freeze(&resolver, &request, "two").await.unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["new"]
    );
    // A reconstructed resolver path uses the same durable fact, despite a newer save.
    desired(&resolver, &request, &[], false, true);
    resolver
        .freeze_assistant_plugins(&request.key, "two", false, request.cancellation.clone())
        .await
        .unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["new"]
    );
    request.key.invocation = 3;
    freeze(&resolver, &request, "three").await.unwrap();
    assert!(
        resolver
            .mounted_plugins(&request)
            .unwrap()
            .bindings
            .is_empty()
    );
    assert_eq!(
        resolver.load_run(&request.key.run_id).unwrap().unwrap(),
        original
    );
}

#[tokio::test]
async fn same_turn_policy_survives_repeated_handover_before_replacement_prepares() {
    let (_root, resolver, mut request) = assistant_fixture(&["old", "new"], &["old"]);
    let state = resolver.local_inputs.state_root();
    std::fs::create_dir_all(state.join("platform")).unwrap();
    let sessions = crate::assistant::sessions(state).unwrap();
    let admission = sessions
        .admit_channel_inbound(
            "alice",
            anchor_platform_session::ChannelInboundRequest {
                inbound_id: "one".into(),
                identity: anchor_platform_session::ChannelIdentity {
                    conversation_kind: Default::default(),
                    source: "wecom".into(),
                    account: None,
                    conversation_id: "fixture".into(),
                    sender_id: "alice".into(),
                },
                graph: "assistant".into(),
                reply_node: "agent".into(),
                text: Some("one".into()),
                attachments: Default::default(),
                run_id: None,
                replace_running: true,
            },
        )
        .unwrap();
    let session = admission.session.id;
    let carried_turn = admission.turn.id;
    let mut original_meta = crate::run_metadata::load(state, &request.key.run_id)
        .unwrap()
        .unwrap();
    original_meta.assistant.as_mut().unwrap().session = session.clone();
    original_meta.conversation = Some(crate::run_metadata::ConversationSource {
        session: session.clone(),
        reply_node: "agent".into(),
        previous_run: None,
    });
    crate::run_metadata::save(state, &original_meta).unwrap();
    sessions
        .bind_channel_assistant(
            "alice",
            &session,
            &request.key.run_id,
            "wait",
            "agent",
            "reply",
        )
        .unwrap();
    let wait = InvocationKey {
        node_id: "wait".into(),
        ..request.key.clone()
    };
    sessions
        .claim_channel_assistant_input("alice", &session, &request.key.run_id, &wait.durable_key())
        .unwrap()
        .unwrap();
    freeze(&resolver, &request, &carried_turn).await.unwrap();
    desired(&resolver, &request, &["new"], false, true);
    let original = resolver.load_run(&request.key.run_id).unwrap().unwrap();
    let mut previous = request.key.run_id.clone();
    for next in ["replacement-empty", "replacement-current"] {
        let mut record =
            GraphRunRecord::create_with_id(original.snapshot.clone(), json!({}), next).unwrap();
        record.plugin_bindings = original.plugin_bindings.clone();
        record.plugin_bindings_initialized = true;
        test_runs::seed(resolver.state_root(), &record);
        let mut metadata = original_meta.clone();
        metadata.run_id = next.into();
        metadata.conversation.as_mut().unwrap().previous_run = Some(previous.clone());
        crate::run_metadata::save(state, &metadata).unwrap();
        sessions
            .handover_channel_assistant("alice", &session, &previous, next, &format!("key-{next}"))
            .unwrap();
        previous = next.into();
    }
    request.key.run_id = previous;
    freeze(&resolver, &request, &carried_turn).await.unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["old"]
    );
    // Later Turns resolve desired policy directly and do not revisit a retired
    // instance that can already have been deleted after successful handover.
    std::fs::remove_file(
        state
            .join("run-metadata")
            .join(format!("{}.json", original.run_id)),
    )
    .unwrap();
    request.key.invocation = 2;
    freeze(&resolver, &request, "two").await.unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["new"]
    );
}

#[tokio::test]
async fn new_turn_after_idle_handover_ignores_earlier_explicit_instance_replacement() {
    let (_root, resolver, mut request) = assistant_fixture(&["old", "new"], &["old"]);
    let state = resolver.local_inputs.state_root();
    std::fs::create_dir_all(state.join("platform")).unwrap();
    let sessions = crate::assistant::sessions(state).unwrap();
    let inbound = |id: &str| anchor_platform_session::ChannelInboundRequest {
        inbound_id: id.into(),
        identity: anchor_platform_session::ChannelIdentity {
            conversation_kind: Default::default(),
            source: "wecom".into(),
            account: None,
            conversation_id: "fixture".into(),
            sender_id: "alice".into(),
        },
        graph: "assistant".into(),
        reply_node: "agent".into(),
        text: Some(id.into()),
        attachments: Default::default(),
        run_id: None,
        replace_running: true,
    };
    let history = sessions
        .admit_channel_inbound("alice", inbound("history"))
        .unwrap();
    let session = history.session.id;
    sessions
        .finish_turn(
            "alice",
            &session,
            &history.turn.id,
            anchor_platform_session::TurnStatus::Completed,
            None,
        )
        .unwrap();
    let original = resolver.load_run(&request.key.run_id).unwrap().unwrap();
    let mut metadata = crate::run_metadata::load(state, &request.key.run_id)
        .unwrap()
        .unwrap();
    metadata.assistant.as_mut().unwrap().session = session.clone();
    let admit = |run: &str, previous: Option<&str>, snapshot: GraphSnapshot| {
        let mut record = GraphRunRecord::create_with_id(snapshot, json!({}), run).unwrap();
        record.plugin_bindings = original.plugin_bindings.clone();
        record.plugin_bindings_initialized = true;
        test_runs::seed(resolver.state_root(), &record);
        let mut meta = metadata.clone();
        meta.run_id = run.into();
        meta.graph_digest = record.graph_digest;
        meta.conversation = Some(crate::run_metadata::ConversationSource {
            session: session.clone(),
            reply_node: "agent".into(),
            previous_run: previous.map(str::to_owned),
        });
        crate::run_metadata::save(state, &meta).unwrap();
    };
    let mut historical_graph = original.snapshot.clone();
    historical_graph
        .agents
        .get_mut("worker")
        .unwrap()
        .instructions = "historical instructions".into();
    admit("history-origin", None, historical_graph.clone());
    admit(
        "history-abandoned",
        Some("history-origin"),
        historical_graph,
    );
    sessions
        .bind_channel_assistant(
            "alice",
            &session,
            "history-origin",
            "wait",
            "agent",
            "reply",
        )
        .unwrap();
    sessions
        .handover_channel_assistant(
            "alice",
            &session,
            "history-origin",
            "history-abandoned",
            "history-key",
        )
        .unwrap();
    sessions
        .retire_channel_assistant("alice", &session, "history-abandoned")
        .unwrap();
    // Explicitly recreate from an older conversation scene. Its conversation
    // predecessor differs from the most recently retired assistant instance.
    let mut recreated = metadata.clone();
    recreated.conversation = Some(crate::run_metadata::ConversationSource {
        session: session.clone(),
        reply_node: "agent".into(),
        previous_run: Some("history-origin".into()),
    });
    crate::run_metadata::save(state, &recreated).unwrap();
    sessions
        .bind_channel_assistant(
            "alice",
            &session,
            &request.key.run_id,
            "wait",
            "agent",
            "reply",
        )
        .unwrap();
    let previous_turn = sessions
        .admit_channel_inbound("alice", inbound("completed-round"))
        .unwrap()
        .turn
        .id;
    let wait = InvocationKey {
        node_id: "wait".into(),
        ..request.key.clone()
    };
    sessions
        .claim_channel_assistant_input("alice", &session, &request.key.run_id, &wait.durable_key())
        .unwrap()
        .unwrap();
    freeze(&resolver, &request, &previous_turn).await.unwrap();
    sessions
        .finish_turn(
            "alice",
            &session,
            &previous_turn,
            anchor_platform_session::TurnStatus::Completed,
            None,
        )
        .unwrap();
    let predecessor = request.key.run_id.clone();
    desired(&resolver, &request, &["new"], false, true);
    admit(
        "replacement-idle",
        Some(&predecessor),
        original.snapshot.clone(),
    );
    sessions
        .handover_channel_assistant(
            "alice",
            &session,
            &predecessor,
            "replacement-idle",
            "replacement-key",
        )
        .unwrap();
    request.key.run_id = "replacement-idle".into();
    let next_turn = sessions
        .admit_channel_inbound("alice", inbound("new-round"))
        .unwrap()
        .turn
        .id;
    let wait = InvocationKey {
        node_id: "wait".into(),
        ..request.key.clone()
    };
    sessions
        .claim_channel_assistant_input("alice", &session, &request.key.run_id, &wait.durable_key())
        .unwrap()
        .unwrap();
    assert_eq!(
        sessions
            .get_turn("alice", &session, &next_turn)
            .unwrap()
            .runs,
        [request.key.run_id.clone()]
    );
    assert_eq!(
        sessions
            .channel_assistant_predecessor("alice", &session, &predecessor)
            .unwrap()
            .unwrap()
            .run_id,
        "history-abandoned"
    );
    freeze(&resolver, &request, &next_turn).await.unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["new"]
    );
    let old = crate::assistant_plugins::load(state, &predecessor, &session, &previous_turn)
        .unwrap()
        .unwrap();
    assert_eq!(old.bindings.keys().cloned().collect::<Vec<_>>(), ["old"]);
}

#[tokio::test]
async fn unapproved_or_structural_graph_and_corrupt_turn_policy_never_grant_tools() {
    let (_root, resolver, request) = assistant_fixture(&["new"], &[]);
    desired(&resolver, &request, &["new"], false, false);
    assert!(
        freeze(&resolver, &request, "one")
            .await
            .unwrap_err()
            .contains("operator authorization")
    );
    desired(&resolver, &request, &["new"], false, true);
    freeze(&resolver, &request, "one").await.unwrap();
    let path = crate::assistant_plugins::turn_path(
        resolver.local_inputs.state_root(),
        &request.key.run_id,
        "assistant-session",
        "one",
    );
    std::fs::write(&path, "{bad").unwrap();
    assert!(resolver.mounted_plugins(&request).is_err());
    assert!(
        resolver
            .freeze_assistant_plugins(&request.key, "one", false, request.cancellation.clone())
            .await
            .is_err()
    );
    std::fs::remove_file(path).unwrap();
    assert!(
        resolver
            .freeze_assistant_plugins(&request.key, "one", false, request.cancellation.clone())
            .await
            .unwrap_err()
            .contains("policy is missing")
    );
    let metadata =
        crate::run_metadata::load(resolver.local_inputs.state_root(), &request.key.run_id)
            .unwrap()
            .unwrap();
    let mut changed = resolver
        .load_run(&request.key.run_id)
        .unwrap()
        .unwrap()
        .snapshot;
    changed.agents.get_mut("worker").unwrap().instructions = "new instruction".into();
    write_bundle(&metadata.bundle_source, &changed);
    assert!(
        freeze(&resolver, &request, "two")
            .await
            .unwrap_err()
            .contains("restart_required")
    );
}

struct PolicySource {
    network: AtomicBool,
    switches: BTreeMap<String, bool>,
}
impl PluginMountSource for PolicySource {
    fn session_mounts(&self, _: &str) -> Result<BTreeMap<String, bool>, String> {
        Ok(self.switches.clone())
    }
    fn session_policy(&self, _: &str) -> Result<(BTreeMap<String, bool>, bool), String> {
        Ok((self.switches.clone(), self.network.load(Ordering::Acquire)))
    }
}

#[tokio::test]
async fn graph_network_and_session_denial_are_frozen_instead_of_stale_request_network() {
    let (_root, mut resolver, mut request) = assistant_fixture(&["network", "online"], &[]);
    let library = resolver.plugin_library_root().unwrap().to_path_buf();
    for id in ["network", "online"] {
        std::fs::write(
            library.join("plugins").join(id).join("plugin.json"),
            json!({"name":id,"network":true}).to_string(),
        )
        .unwrap();
    }
    let switches = std::sync::Arc::new(PolicySource {
        network: AtomicBool::new(true),
        switches: BTreeMap::new(),
    });
    resolver.mounts = switches.clone();
    desired(&resolver, &request, &["online"], true, true);
    assert!(!request.network);
    freeze(&resolver, &request, "one").await.unwrap();
    let mounted = resolver.mounted_plugins(&request).unwrap();
    assert!(mounted.network);
    assert_eq!(mounted_ids(&mounted), ["network", "online"]);
    switches.network.store(false, Ordering::Release);
    assert!(resolver.mounted_plugins(&request).unwrap().network);
    request.key.invocation = 2;
    request.network = true;
    freeze(&resolver, &request, "two").await.unwrap();
    let denied = resolver.mounted_plugins(&request).unwrap();
    assert!(!denied.network);
    assert!(denied.bindings.is_empty());
    assert!(
        denied
            .reasons
            .iter()
            .any(|reason| reason.contains("禁止联网"))
    );
    switches.network.store(true, Ordering::Release);
    desired(&resolver, &request, &["online"], false, true);
    request.key.invocation = 3;
    freeze(&resolver, &request, "three").await.unwrap();
    assert!(!resolver.mounted_plugins(&request).unwrap().network);
}

#[tokio::test]
async fn assistant_session_switch_off_wins_and_frozen_authority_drift_fails_closed() {
    let (_root, mut resolver, mut request) = assistant_fixture(&["off", "on"], &[]);
    resolver.mounts = std::sync::Arc::new(PolicySource {
        network: AtomicBool::new(true),
        switches: BTreeMap::from([("off".into(), false)]),
    });
    desired(&resolver, &request, &["off", "on"], false, true);
    freeze(&resolver, &request, "one").await.unwrap();
    assert_eq!(
        mounted_ids(&resolver.mounted_plugins(&request).unwrap()),
        ["on"]
    );
    std::fs::write(
        resolver
            .plugin_library_root()
            .unwrap()
            .join("plugins/on/plugin.json"),
        r#"{"name":"on","network":true}"#,
    )
    .unwrap();
    assert!(
        resolver
            .mounted_plugins(&request)
            .unwrap_err()
            .contains("authorization changed")
    );
    request.key.invocation = 2;
    assert!(
        freeze(&resolver, &request, "two")
            .await
            .unwrap_err()
            .contains("authorization changed")
    );
}
