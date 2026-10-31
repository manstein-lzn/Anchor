use super::*;
use crate::plugin_library::Catalog;
use anchor_graph_host::PluginCatalog;
use anchor_runtime::graph::RecoveryAttempt;
use std::time::Duration;

fn kernel_resource(state: &ApiState, id: &str) -> Option<anchor_kernel::Resource> {
    use anchor_kernel::Store;
    anchor_kernel::FileStore::new(state.data_root.join("kernel-runs"))
        .unwrap()
        .load(&anchor_runtime::graph::validator::run_resource_id(id))
        .unwrap()
}

fn gate_definition() -> Value {
    json!({
        "objective":"conversation admission fixture","entry":"work","agents":{},
        "input":{"default_input":"frozen"},
        "ops":{
            "work":{"run":"printf ready > gate.ready; while [ ! -f gate.release ]; do sleep 0.02; done; printf once > reply.txt",
                    "writes":["reply.txt"],"wall_time_limit_seconds":10},
            "finish":{"run":"true"},
        },
        "nodes":[{"id":"work","op":"work"},{"id":"finish","op":"finish"}],
        "edges":[{"from":"work","to":"finish"}],
    })
}

fn configure_gate(root: &std::path::Path, state: &ApiState) {
    set_host_env(root);
    unsafe {
        env::set_var("ANCHOR_RUNNER_ALLOWED_COMMANDS", "true,sh,cat,sleep,printf");
    }
    write_graph_bundle(&state.bundle_root, &gate_definition()).unwrap();
}

async fn wait_gate(state: &ApiState, run: &str) -> PathBuf {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = fixtures::Reader::new(&state.application)
                .load(run)
                .unwrap()
                .unwrap();
            assert!(
                !(is_failed(&record) || record.has_succeeded()),
                "{record:?}"
            );
            if let Some(dispatch) = record.view.dispatches().first() {
                let workspace = HostArtifacts::new(
                    state.data_root.join("artifacts"),
                    state.workspace_root.clone(),
                )
                .workspace_path(&dispatch.key)
                .unwrap();
                if workspace.join("gate.ready").is_file() {
                    return workspace;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real Op did not enter its fixture gate")
}

async fn wait_run_status(
    state: &ApiState,
    run: &str,
    expected: impl Fn(&fixtures::Observation) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = fixtures::Reader::new(&state.application)
                .load(run)
                .unwrap()
                .unwrap();
            if expected(&record)
                && !state
                    .application
                    .active_runs(None)
                    .await
                    .iter()
                    .any(|id| id == run)
            {
                return;
            }
            assert!(!is_failed(&record), "{record:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the real Run did not settle")
}

async fn stop_run(app: &Router, state: &ApiState, run: &str) {
    let (status, value) = call(app.clone(), "POST", &format!("/runs/{run}/stop"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    wait_run_status(state, run, |run| run.waits_reason("stopped")).await;
}

/// One turn of `session` handed over from `previous`: the conversation Run an
/// assistant handover or a Session call leaves behind, parked until resumed.
fn seed_turn(
    state: &ApiState,
    graph: &str,
    run: &str,
    session: &str,
    reply_node: &str,
    previous: Option<&str>,
) {
    let path = state.application.graph_bundle_path(graph);
    let bundle = FileGraphBundleLoader::new(&path).load().unwrap();
    let mut record = anchor_runtime::graph::GraphRunRecord::create_with_id(
        bundle.snapshot,
        json!({"session":session}),
        run,
    )
    .unwrap();
    record.plugin_bindings_initialized = true;
    fixtures::create(&state.application, &record);
    fixtures::control(&state.application, run, "stop", "stopped");
    let mut metadata =
        RunMetadata::new(run.into(), graph.into(), record.graph_digest, &path).unwrap();
    metadata.trigger_source = "channel".into();
    metadata.conversation = Some(crate::run_metadata::ConversationSource {
        session: session.into(),
        reply_node: reply_node.into(),
        previous_run: previous.map(str::to_owned),
    });
    metadata::save(&state.data_root, &metadata).unwrap();
}

async fn resume(app: &Router, run: &str) -> (StatusCode, Value) {
    call(app.clone(), "POST", &format!("/runs/{run}/resume"), None).await
}

/// Seed a turn of the fixture Graph and run it to completion.
async fn completed_turn(app: &Router, state: &ApiState, run: &str, previous: Option<&str>) {
    seed_turn(state, "fixture", run, "alice", "work", previous);
    let (status, resumed) = resume(app, run).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    wait_run_status(state, run, fixtures::Observation::has_succeeded).await;
}

#[tokio::test]
async fn sessions_share_a_graph_and_a_handed_over_turn_protects_its_predecessor() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    let app = router(state.clone());
    let (alice_run, bob_run, next_run) = ("alice-1", "bob-1", "alice-2");
    seed_turn(&state, "fixture", alice_run, "alice", "work", None);
    seed_turn(&state, "fixture", bob_run, "bob", "work", None);
    for run in [alice_run, bob_run] {
        let (status, resumed) = resume(&app, run).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    }
    wait_gate(&state, alice_run).await;
    let bob_workspace = wait_gate(&state, bob_run).await;
    assert_eq!(
        state.application.active_runs(Some("fixture")).await.len(),
        2
    );
    let (status, rejected) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{rejected}");

    stop_run(&app, &state, alice_run).await;
    seed_turn(
        &state,
        "fixture",
        next_run,
        "alice",
        "work",
        Some(alice_run),
    );
    let (status, resumed) = resume(&app, next_run).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    wait_gate(&state, next_run).await;
    let original = kernel_resource(&state, alice_run).unwrap();
    for (method, path) in [
        ("POST", format!("/runs/{alice_run}/resume")),
        ("DELETE", format!("/runs/{alice_run}")),
    ] {
        let (status, rejected) = call(app.clone(), method, &path, None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
        assert!(
            rejected["error"]
                .as_str()
                .unwrap()
                .contains("previous turn"),
            "{rejected}"
        );
    }
    assert_eq!(kernel_resource(&state, alice_run).unwrap(), original);
    // The taken-over turn reads as replaced, not as waiting for a person, and
    // offers no control: resuming and abandoning it are both refused.
    let (_, previous) = call(app.clone(), "GET", &format!("/runs/{alice_run}"), None).await;
    let projection = &previous["projection"];
    assert_eq!(projection["phase"], "waiting", "{previous}");
    assert_eq!(projection["superseded"], true, "{previous}");
    assert_eq!(
        projection["actions"],
        json!({"pause":false,"stop":false,"resume":false,"abandon":false}),
        "{previous}"
    );
    let (_, current) = call(app.clone(), "GET", &format!("/runs/{next_run}"), None).await;
    assert!(
        current["projection"].get("superseded").is_none(),
        "{current}"
    );
    let (_, detail) = call(app.clone(), "GET", &format!("/runs/{next_run}"), None).await;
    assert_eq!(detail["state"]["trigger"]["session"], "alice");
    assert_eq!(detail["state"]["trigger"]["reply_node"], "work");
    assert_eq!(detail["state"]["trigger"]["previous_run"], alice_run);
    assert!(detail["state"]["trigger"].get("channel").is_none());
    let (_, listed) = call(app.clone(), "GET", "/runs", None).await;
    assert_eq!(
        listed["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["run"] == next_run)
            .unwrap()["trigger"],
        detail["state"]["trigger"]
    );

    stop_run(&app, &state, next_run).await;
    std::fs::write(bob_workspace.join("gate.release"), "release").unwrap();
    wait_run_status(&state, bob_run, fixtures::Observation::has_succeeded).await;
    let shared = state.data_root.join("io-harness/sessions/node.sqlite3");
    std::fs::create_dir_all(shared.parent().unwrap()).unwrap();
    std::fs::write(&shared, "Session-owned record").unwrap();
    // A completed Session turn with nothing newer may be deleted, and that
    // cleanup must still leave Session-owned shared records alone.
    let (status, deleted) = call(app.clone(), "DELETE", &format!("/runs/{bob_run}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert_eq!(
        std::fs::read_to_string(&shared).unwrap(),
        "Session-owned record"
    );
    assert!(
        crate::run_deletions::load(&state.data_root, bob_run)
            .unwrap()
            .is_some()
    );
    assert!(kernel_resource(&state, bob_run).is_none());
    let (status, deleted) = call(app, "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(state.application.records().unwrap().is_empty());
}

/// A conversation Run is one link of a Session's lineage. A link may be deleted
/// once nothing newer still reads it, in any order: its tombstone keeps the place
/// it held, so the surviving lineage stays a single chain.
#[tokio::test]
async fn conversation_lineage_deletes_any_link_once_nothing_newer_reads_it() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let app = router(state.clone());

    // first → second → third, each turn completed before the next one runs.
    let runs = ["first", "second", "third"];
    for (index, run) in runs.iter().enumerate() {
        let previous = index.checked_sub(1).map(|index| runs[index]);
        seed_turn(&state, "fixture", run, "alice", "work", previous);
        // A Run the handed-over turn has not read yet must stay.
        if let Some(previous) = previous {
            let (status, rejected) =
                call(app.clone(), "DELETE", &format!("/runs/{previous}"), None).await;
            assert_eq!(status, StatusCode::CONFLICT, "{rejected}");
            assert!(
                rejected["error"]
                    .as_str()
                    .unwrap()
                    .contains("previous turn")
            );
        }
        let (status, resumed) = resume(&app, run).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
        wait_run_status(&state, run, fixtures::Observation::has_succeeded).await;
    }

    // A middle link goes once its successor is settled, and the surviving chain
    // still resolves past the tombstone onto the older link that survives.
    let (status, deleted) = call(app.clone(), "DELETE", "/runs/second", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{deleted}");
    assert!(
        crate::run_deletions::load(&state.data_root, "second")
            .unwrap()
            .unwrap()
            .previous_run()
            .is_some()
    );
    completed_turn(&app, &state, "fourth", Some("third")).await;

    // The newest link may go while older ones survive, and those inert older
    // links can still be cleaned up, in any order.
    for run in ["fourth", "third", "first"] {
        let (status, deleted) = call(app.clone(), "DELETE", &format!("/runs/{run}"), None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{run}: {deleted}");
    }
    assert!(state.application.records().unwrap().is_empty());
}

#[tokio::test]
async fn superseded_wait_ancestry_stops_at_a_detach_boundary() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let app = router(state.clone());
    let first_run = "ancestry-first";
    completed_turn(&app, &state, first_run, None).await;
    write_graph_bundle(
        &state.catalog_root.join("child"),
        &json!({"entry":"work","objective":"child","agents":{},
        "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]}),
    )
    .unwrap();
    seed_call_fixture(&state, "child", "wait-child", first_run, first_run, "wait");
    seed_call_fixture(
        &state,
        "child",
        "wait-grandchild",
        "wait-child",
        first_run,
        "wait",
    );
    seed_call_fixture(
        &state,
        "child",
        "detached-child",
        first_run,
        first_run,
        "detach",
    );
    seed_call_fixture(
        &state,
        "child",
        "detached-wait-grandchild",
        "detached-child",
        first_run,
        "wait",
    );
    completed_turn(&app, &state, "ancestry-next", Some(first_run)).await;
    for run in ["wait-child", "wait-grandchild"] {
        let before = kernel_resource(&state, run).unwrap();
        for (path, body) in [
            (format!("/runs/{run}/resume"), None),
            (
                format!("/runs/{run}/recovery"),
                Some(r#"{"node_id":"work","invocation":1,"attempt_id":1,"decision":"retry"}"#),
            ),
        ] {
            let (status, value) = call(app.clone(), "POST", &path, body).await;
            assert_eq!(status, StatusCode::CONFLICT, "{value}");
            assert!(value["error"].as_str().unwrap().contains("previous turn"));
        }
        assert_eq!(kernel_resource(&state, run).unwrap(), before);
    }
    for run in ["detached-child", "detached-wait-grandchild"] {
        let (status, value) = call(app.clone(), "POST", &format!("/runs/{run}/resume"), None).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
        wait_run_status(&state, run, fixtures::Observation::has_succeeded).await;
    }
}

fn seed_call_fixture(
    state: &ApiState,
    graph: &str,
    run: &str,
    parent: &str,
    root: &str,
    mode: &str,
) {
    let store = fixtures::Reader::new(&state.application);
    let parent_record = store.load(parent).unwrap().unwrap();
    let parent_metadata = state.application.metadata(parent).unwrap().unwrap();
    let path = state.application.graph_bundle_path(graph);
    let bundle = FileGraphBundleLoader::new(&path).load().unwrap();
    let mut record =
        anchor_runtime::graph::GraphRunRecord::create_with_id(bundle.snapshot, Value::Null, run)
            .unwrap();
    // A bundle references Plugins by id; the code lives in the Library.
    record.plugin_bindings = Catalog::new(&state.catalog_root)
        .resolve(&bundle.plugins)
        .unwrap()
        .into_iter()
        .map(|plugin| (plugin.id.clone(), plugin))
        .collect();
    record.plugin_bindings_initialized = true;
    let metadata = RunMetadata::graph_call_child(
        run.into(),
        graph.into(),
        record.graph_digest.clone(),
        &path,
        crate::run_metadata::GraphCallSource {
            parent_run: parent.into(),
            parent_graph: parent_metadata.graph,
            parent_graph_digest: parent_record.graph_digest.clone(),
            node: "work".into(),
            invocation: 1,
            mode: mode.into(),
            root_run: root.into(),
        },
    )
    .unwrap();
    metadata::save(&state.data_root, &metadata).unwrap();
    fixtures::create(&state.application, &record);
    fixtures::control(&state.application, run, "stop", "stopped");
}

#[tokio::test]
async fn publishing_graph_preserves_plugin_conversation_history_and_child_runs() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/example")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/example/SKILL.md"), "demo skill").unwrap();
    let definition = json!({"objective":"plugin history","entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},"ops":{},
        "nodes":[{"id":"work","agent":"worker","plugins":["demo"]}],"edges":[]});
    let app = router(state.clone());
    assert_eq!(
        call(
            app.clone(),
            "PUT",
            "/graphs/fixture",
            Some(&json!({"definition":definition}).to_string())
        )
        .await
        .0,
        StatusCode::OK
    );
    let bundle = FileGraphBundleLoader::new(&state.bundle_root)
        .load()
        .unwrap();
    let store = fixtures::Reader::new(&state.application);
    let mut first = anchor_runtime::graph::GraphRunRecord::create_with_id(
        bundle.snapshot.clone(),
        Value::Null,
        "plugin-first",
    )
    .unwrap();
    first.plugin_bindings = Catalog::new(&state.catalog_root)
        .resolve(&bundle.plugins)
        .unwrap()
        .into_iter()
        .map(|plugin| (plugin.id.clone(), plugin))
        .collect();
    first.plugin_bindings_initialized = true;
    let mut first_metadata = RunMetadata::new(
        first.run_id.clone(),
        "fixture".into(),
        first.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    first_metadata.conversation = Some(crate::run_metadata::ConversationSource {
        session: "alice".into(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata::save(&state.data_root, &first_metadata).unwrap();
    fixtures::create(&state.application, &first);
    fixtures::control(&state.application, &first.run_id, "stop", "stopped");
    let first_bytes = kernel_resource(&state, "plugin-first").unwrap();
    let mut updated = definition;
    updated["objective"] = json!("next Graph version");
    updated["agents"]["worker"]["network"] = json!(true);
    let update = json!({"definition":updated}).to_string();
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::OK
    );
    let mut next = anchor_runtime::graph::GraphRunRecord::create_with_id(
        first.snapshot.clone(),
        Value::Null,
        "plugin-next",
    )
    .unwrap();
    next.plugin_bindings = first.plugin_bindings.clone();
    next.plugin_bindings_initialized = true;
    let mut next_metadata = RunMetadata::new(
        next.run_id.clone(),
        "fixture".into(),
        next.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    next_metadata.conversation = Some(crate::run_metadata::ConversationSource {
        session: "alice".into(),
        reply_node: "work".into(),
        previous_run: Some(first.run_id.clone()),
    });
    metadata::save(&state.data_root, &next_metadata).unwrap();
    fixtures::create(&state.application, &next);
    fixtures::control(&state.application, &next.run_id, "stop", "stopped");
    let (status, value) = call(app.clone(), "PUT", "/graphs/fixture", Some(&update)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    seed_call_fixture(
        &state,
        "fixture",
        "plugin-wait-child",
        &first.run_id,
        &first.run_id,
        "wait",
    );
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::OK
    );
    fixtures::control(&state.application, "plugin-wait-child", "resume", "resumed");
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::OK
    );
    fixtures::control(&state.application, "plugin-wait-child", "stop", "stopped");
    seed_call_fixture(
        &state,
        "fixture",
        "plugin-detached-child",
        &first.run_id,
        &first.run_id,
        "detach",
    );
    assert_eq!(
        call(app.clone(), "PUT", "/graphs/fixture", Some(&update))
            .await
            .0,
        StatusCode::OK
    );
    fixtures::control(
        &state.application,
        "plugin-detached-child",
        "resume",
        "resumed",
    );
    fixtures::finish(&state.application, "plugin-detached-child");
    assert_eq!(
        call(app, "PUT", "/graphs/fixture", Some(&update)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        kernel_resource(&state, "plugin-first").unwrap(),
        first_bytes
    );
    for run in ["plugin-first", "plugin-next"] {
        let record = store.load(run).unwrap().unwrap();
        assert!(!record.snapshot.agents["worker"].network);
    }
    // Children admitted from the newly published definition use its permission;
    // their stopped/running states do not prevent later publication either.
    for run in ["plugin-wait-child", "plugin-detached-child"] {
        let record = store.load(run).unwrap().unwrap();
        assert!(record.snapshot.agents["worker"].network);
    }
}

fn seed_legacy_scope(state: &ApiState, run: &str, node: &str) -> (PathBuf, PathBuf, PathBuf) {
    let metadata = state.application.metadata(run).unwrap().unwrap();
    let hint = metadata.node_conversation_key(node);
    let scope = format!("nc1-{:x}", Sha256::digest(hint.as_bytes()));
    let io_root = state.data_root.join("io-harness/store");
    let directory = io_root.join("conversations").join(&scope);
    let framework_root = io_root.join("conversation-roots").join(&scope);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::create_dir_all(&framework_root).unwrap();
    let (session_id, native_run, turn) = {
        std::fs::write(
            directory.join("framework.sqlite3"),
            b"opaque legacy cleanup fixture",
        )
        .unwrap();
        (1, 1, 1)
    };
    std::fs::write(
        directory.join("session.json"),
        json!({"version":1,"session_id":session_id}).to_string(),
    )
    .unwrap();
    let key = anchor_runtime::graph::InvocationKey {
        run_id: run.into(),
        graph_digest: metadata.graph_digest,
        node_id: node.into(),
        invocation: 1,
    };
    let stem = format!("np1-{:x}", Sha256::digest(key.durable_key().as_bytes()));
    let pointer = io_root.join(format!("{stem}.conversation.json"));
    std::fs::write(
        &pointer,
        json!({"version":1,"scope":scope,"invocation":key.durable_key(),
        "session_id":session_id,"run_id":native_run,"turn_id":turn})
        .to_string(),
    )
    .unwrap();
    (directory, framework_root, pointer)
}

#[tokio::test]
async fn graph_delete_cleans_legacy_scopes_and_preserves_other_graph_history() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    write_graph_bundle(&state.bundle_root, &two_node_definition("true")).unwrap();
    let app = router(state.clone());
    let turn = |graph: &'static str, run: &'static str, session: &'static str, previous| {
        let (app, state) = (app.clone(), state.clone());
        async move {
            seed_turn(&state, graph, run, session, "first", previous);
            let (status, resumed) = resume(&app, run).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
            wait_run_status(&state, run, fixtures::Observation::has_succeeded).await;
        }
    };
    turn("fixture", "scope-first", "alice", None).await;
    let new_definition = json!({"objective":"new graph","entry":"first","agents":{},
        "ops":{"first":{"run":"true"}},"nodes":[{"id":"first","op":"first"}],"edges":[]});
    assert_eq!(
        call(
            app.clone(),
            "PUT",
            "/graphs/fixture",
            Some(&json!({"definition":new_definition}).to_string())
        )
        .await
        .0,
        StatusCode::OK
    );
    turn("fixture", "scope-next", "alice", Some("scope-first")).await;
    write_graph_bundle(&state.catalog_root.join("other"), &new_definition).unwrap();
    turn("other", "scope-other", "bob", None).await;
    let first_scope = seed_legacy_scope(&state, "scope-first", "first");
    let same_scope = seed_legacy_scope(&state, "scope-next", "first");
    let old_node_scope = seed_legacy_scope(&state, "scope-first", "second");
    let other_scope = seed_legacy_scope(&state, "scope-other", "first");
    assert_eq!(first_scope.0, same_scope.0);
    seed_call_fixture(
        &state,
        "other",
        "orphan-wait-child",
        "scope-first",
        "scope-first",
        "wait",
    );
    fixtures::control(&state.application, "orphan-wait-child", "resume", "resumed");
    let (status, value) = call(app.clone(), "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert!(first_scope.0.join("framework.sqlite3").is_file());
    stop_run(&app, &state, "orphan-wait-child").await;
    // A single conversation Run may now be deleted, and that cleanup must still
    // leave the Session-owned legacy scopes to the Graph cascade.
    let (status, value) = call(app.clone(), "DELETE", "/runs/scope-next", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
    assert!(first_scope.0.exists());
    let (status, value) = call(app.clone(), "DELETE", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
    for (directory, framework_root, pointer) in [first_scope, same_scope, old_node_scope] {
        assert!(!directory.exists());
        assert!(!framework_root.exists());
        assert!(!pointer.exists());
    }
    assert!(other_scope.0.join("framework.sqlite3").is_file());
    assert!(other_scope.1.is_dir());
    assert!(other_scope.2.is_file());
    let remaining = state.application.records().unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().any(|(id, _)| id == "scope-other"));
    let (status, value) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(&json!({"name":"fixture","definition":new_definition}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{value}");
    turn("fixture", "scope-fresh", "alice", None).await;
    let native = seed_legacy_scope(&state, "scope-fresh", "first");
    assert_eq!(
        std::fs::read(native.0.join("framework.sqlite3")).unwrap(),
        b"opaque legacy cleanup fixture"
    );
}

#[tokio::test]
async fn graph_delete_allows_a_completed_caller_after_its_callee_was_deleted() {
    let _env = PROCESS_ENV.lock().await;
    for conversation in [false, true] {
        let (root, state) = fixture();
        set_host_env(root.path());
        let completed = json!({"objective":"completed node","entry":"work","agents":{},
            "ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]});
        write_graph_bundle(&state.catalog_root.join("child"), &completed).unwrap();
        write_graph_bundle(
            &state.bundle_root,
            &json!({"objective":"completed caller","entry":"work","agents":{},
            "ops":{"work":{"call":{"graph":"child","mode":"wait"}}},
            "nodes":[{"id":"work","op":"work"}],"edges":[]}),
        )
        .unwrap();
        let app = router(state.clone());
        let run = if conversation {
            completed_turn(&app, &state, "caller", None).await;
            "caller".to_owned()
        } else {
            let (status, value) = call(
                app.clone(),
                "POST",
                "/trigger",
                Some(r#"{"graph":"fixture"}"#),
            )
            .await;
            assert_eq!(status, StatusCode::ACCEPTED, "{value}");
            let run = value["run"].as_str().unwrap().to_owned();
            wait_run_status(&state, &run, fixtures::Observation::has_succeeded).await;
            run
        };
        let scope = conversation.then(|| seed_legacy_scope(&state, &run, "work"));
        assert_eq!(
            call(
                app.clone(),
                "PUT",
                "/graphs/fixture",
                Some(&json!({"definition":completed}).to_string())
            )
            .await
            .0,
            StatusCode::OK
        );
        let (status, value) = call(app.clone(), "DELETE", "/graphs/child", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
        let (status, value) = call(app, "DELETE", "/graphs/fixture", None).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{value}");
        assert!(state.application.records().unwrap().is_empty());
        if let Some((directory, framework_root, pointer)) = scope {
            assert!(!directory.exists());
            assert!(!framework_root.exists());
            assert!(!pointer.exists());
        }
    }
}

#[tokio::test]
async fn conversation_orphan_running_requires_explicit_stop_before_a_new_turn() {
    let (root, mut state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let run = "orphan-first";
    let record = anchor_runtime::graph::GraphRunRecord::create_with_id(
        state.application.load_graph("fixture").unwrap().1.snapshot,
        json!({"session":"alice"}),
        run,
    )
    .unwrap();
    fixtures::create(&state.application, &record);
    let mut saved = RunMetadata::new(
        run.into(),
        "fixture".into(),
        record.graph_digest,
        &state.bundle_root,
    )
    .unwrap();
    saved.trigger_source = "channel".into();
    saved.conversation = Some(crate::run_metadata::ConversationSource {
        session: "alice".into(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata::save(&state.data_root, &saved).unwrap();
    state.application = RunApplication::new(state.data_root.clone(), state.catalog_root.clone())
        .with_configured_graph("fixture".into(), state.bundle_root.clone());
    let restarted = state.clone();
    let app = router(restarted.clone());
    let (_, detail) = call(app.clone(), "GET", &format!("/runs/{run}"), None).await;
    assert_eq!(detail["projection"]["phase"], "executing");
    assert_eq!(detail["active"], false);
    seed_turn(
        &restarted,
        "fixture",
        "orphan-next",
        "alice",
        "work",
        Some(run),
    );
    assert_eq!(resume(&app, "orphan-next").await.0, StatusCode::CONFLICT);
    assert!(restarted.application.active_runs(None).await.is_empty());
    // A durable Running orphan is never silently replayed; the trusted host must
    // settle it explicitly before the next conversation turn executes.
    stop_run(&app, &restarted, run).await;
    let (status, resumed) = resume(&app, "orphan-next").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    wait_run_status(
        &restarted,
        "orphan-next",
        fixtures::Observation::has_succeeded,
    )
    .await;
}

#[tokio::test]
async fn an_unfinished_manual_run_blocks_conversation_execution_on_its_graph() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    configure_gate(root.path(), &state);
    let app = router(state.clone());
    let (status, manual) = call(
        app.clone(),
        "POST",
        "/trigger",
        Some(r#"{"graph":"fixture"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{manual}");
    let run = manual["run"].as_str().unwrap();
    wait_gate(&state, run).await;
    seed_turn(&state, "fixture", "blocked-turn", "alice", "work", None);
    let before = kernel_resource(&state, "blocked-turn").unwrap();
    assert_eq!(resume(&app, "blocked-turn").await.0, StatusCode::CONFLICT);
    stop_run(&app, &state, run).await;
    assert_eq!(resume(&app, "blocked-turn").await.0, StatusCode::CONFLICT);
    assert_eq!(kernel_resource(&state, "blocked-turn").unwrap(), before);
    assert_eq!(
        call(app.clone(), "DELETE", &format!("/runs/{run}"), None)
            .await
            .0,
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn conversation_waiting_recovery_stop_keeps_unknown_attempts_without_execution() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let snapshot = anchor_runtime::graph::GraphSnapshot::admit(json!({
        "objective":"unknown-effect fixture","entry":"work",
        "agents":{"worker":{"model":"fixture","instructions":"fixture"}},
        "ops":{},"nodes":[{"id":"work","agent":"worker"}],"edges":[],
    }))
    .unwrap();
    let record = anchor_runtime::graph::GraphRunRecord::create_with_id(
        snapshot,
        Value::Null,
        "recovery-first",
    )
    .unwrap();
    let store = fixtures::Reader::new(&state.application);
    fixtures::create(&state.application, &record);
    fixtures::recovery(
        &state.application,
        &record.run_id,
        "work",
        vec![RecoveryAttempt {
            attempt_id: 17,
            step: 1,
            tool: "unknown-effect-fixture".into(),
            started_at: "fixture".into(),
        }],
    );
    let before = store.load(&record.run_id).unwrap().unwrap();
    let mut metadata = RunMetadata::new(
        record.run_id.clone(),
        "fixture".into(),
        record.graph_digest.clone(),
        &state.bundle_root,
    )
    .unwrap();
    metadata.trigger_source = "channel".into();
    metadata.conversation = Some(crate::run_metadata::ConversationSource {
        session: "alice".into(),
        reply_node: "work".into(),
        previous_run: None,
    });
    metadata::save(&state.data_root, &metadata).unwrap();
    let app = router(state.clone());
    stop_run(&app, &state, &record.run_id).await;
    let stopped = store.load(&record.run_id).unwrap().unwrap();
    assert_eq!(stopped.recovery(), before.recovery());
    assert_eq!(
        stopped
            .view
            .dispatches()
            .iter()
            .map(|d| &d.key)
            .collect::<Vec<_>>(),
        before
            .view
            .dispatches()
            .iter()
            .map(|d| &d.key)
            .collect::<Vec<_>>()
    );
    assert!(stopped.results.is_empty());
    assert!(!state.data_root.join("io-harness").exists());
    fixtures::control(
        &state.application,
        &record.run_id,
        "abandon",
        "operator abandoned unknown effects",
    );
    let abandoned = store.load(&record.run_id).unwrap().unwrap();
    assert!(is_aborted(&abandoned));
    // Only the abandoned Run can hand the Session over; the next turn executes
    // without touching its retained attempts.
    seed_turn(
        &state,
        "fixture",
        "recovery-next",
        "alice",
        "work",
        Some(&record.run_id),
    );
    let (status, resumed) = resume(&app, "recovery-next").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{resumed}");
    wait_run_status(
        &state,
        "recovery-next",
        fixtures::Observation::has_succeeded,
    )
    .await;
    assert_eq!(
        store.load(&record.run_id).unwrap().unwrap().record,
        abandoned.record
    );
}

#[tokio::test]
async fn graph_node_plugins_project_expanded_nodes_and_legacy_metadata_remains_readable() {
    let (root, state) = fixture();
    let _env = PROCESS_ENV.lock().await;
    set_host_env(root.path());
    let plugin = state.catalog_root.join("plugins/demo");
    std::fs::create_dir_all(plugin.join("skills/example")).unwrap();
    std::fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"Demo","skills":"skills/"}"#,
    )
    .unwrap();
    std::fs::write(plugin.join("skills/example/SKILL.md"), "demo skill").unwrap();
    let authoring = json!({"entry":"stage","objective":"projection","agents":{"worker":{"model":"fixture","instructions":"fixture"}},"ops":{"work":{"run":"true"}},
        "nodes":[{"id":"stage","graph":"inner"}],"edges":[],
        "graphs":{"inner":{"entry":"inside","exit":"inside","nodes":[{"id":"inside","agent":"worker","plugins":["demo"]}],"edges":[]}}});
    let app = router(state.clone());
    let (status, changed) = call(
        app.clone(),
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":authoring}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    let (status, graph) = call(app, "GET", "/graphs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(graph["definition"], authoring);
    // The projection carries the declaration: a required reference with no
    // default is written as the bare id it always was.
    assert_eq!(graph["node_plugins"], json!({"stage/inside":["demo"]}));
    let metadata = RunMetadata::new(
        "legacy-run".into(),
        "fixture".into(),
        "digest".into(),
        &state.bundle_root,
    )
    .unwrap();
    let mut old = serde_json::to_value(&metadata).unwrap();
    old.as_object_mut().unwrap().remove("conversation");
    for format in [1, 2] {
        old["format"] = json!(format);
        let directory = state.data_root.join("run-metadata");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("legacy-run.json"), old.to_string()).unwrap();
        assert!(
            metadata::load(&state.data_root, "legacy-run")
                .unwrap()
                .unwrap()
                .conversation
                .is_none()
        );
    }
}
