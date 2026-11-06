use super::*;
use crate::application::run_access::fixtures;
use crate::run_metadata::{self as metadata, RunMetadata};
use anchor_graph_host::PluginCatalog;
use anchor_platform_session::CreateSession;
use anchor_runtime::graph::GraphRunRecord;

pub(super) fn fixture() -> (tempfile::TempDir, RunApplication) {
    let root = tempfile::tempdir().unwrap();
    let application = RunApplication::new(root.path().join("state"), root.path().join("catalog"));
    for (id, network) in [
        ("graph-tasks", false),
        ("offline", false),
        ("other", false),
        ("network", true),
        ("online", true),
    ] {
        let directory = application.library_root().join("plugins").join(id);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("plugin.json"),
            json!({"name":id,"network":network}).to_string(),
        )
        .unwrap();
    }
    let official = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/graph-tasks");
    let installed = application.library_root().join("plugins/graph-tasks");
    std::fs::copy(official.join("plugin.json"), installed.join("plugin.json")).unwrap();
    std::fs::create_dir_all(installed.join("skills/graph-tasks")).unwrap();
    std::fs::copy(
        official.join("skills/graph-tasks/SKILL.md"),
        installed.join("skills/graph-tasks/SKILL.md"),
    )
    .unwrap();
    (root, application)
}

fn agent(plugins: Value) -> Value {
    json!({"objective":"fixture","entry":"work","agents":{"role":{"model":"fixture"}},
        "nodes":[{"id":"work","agent":"role","plugins":plugins}],"edges":[]})
}

fn source(policy: &GraphTaskPolicy) -> Value {
    agent(json!([{"id":"graph-tasks","graph_tasks":policy}]))
}

pub(super) fn command(network: bool) -> Value {
    json!({"objective":"command fixture","entry":"work","ops":{"work":{"run":"true","network":network}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]})
}

fn call(target: &str) -> Value {
    json!({"entry":"work","ops":{"work":{"call":{"graph":target,"mode":"wait"}}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]})
}

pub(super) fn write(application: &RunApplication, name: &str, definition: &Value) {
    graphs::write_graph_bundle_from_catalog(
        &application.graph_bundle_path(name),
        definition,
        application.library_root(),
    )
    .unwrap();
}

fn fixture_origin(
    application: &RunApplication,
    policy: GraphTaskPolicy,
    session: Option<&str>,
) -> TaskOrigin {
    let (_, source) = application.load_graph("source").unwrap();
    let digest = source.snapshot.digest().unwrap();
    let publication = assistant_plugins::publication(
        &application.data_root,
        &application.graph_bundle_path("source"),
        &digest,
    )
    .unwrap()
    .unwrap();
    if let Some(id) = session {
        crate::host_fs::directory(&application.data_root.join("platform")).unwrap();
        let sessions =
            SessionStore::open(application.data_root.join("platform/sessions.sqlite")).unwrap();
        if sessions.get("owner", id).is_err() {
            sessions
                .create(
                    "owner",
                    CreateSession {
                        id: Some(id.into()),
                        ..Default::default()
                    },
                )
                .unwrap();
        }
    }
    TaskOrigin {
        owner: "owner".into(),
        session: session.map(str::to_owned),
        graph: "source".into(),
        source: TaskOriginSource::AgentDelegation {
            key: InvocationKey {
                run_id: "source-run".into(),
                graph_digest: digest.clone(),
                node_id: "work".into(),
                invocation: 1,
            },
        },
        authorization_digest: digest,
        policy,
        bindings: publication.bindings,
        targets: publication.task_graphs,
    }
}

async fn approved(
    application: &RunApplication,
    policy: GraphTaskPolicy,
    session: Option<&str>,
) -> TaskOrigin {
    let definition = source(&policy);
    write(application, "source", &definition);
    application
        .update_graph("source", definition)
        .await
        .unwrap();
    fixture_origin(application, policy, session)
}

fn admit_source(
    application: &RunApplication,
    run: &str,
    owner: &str,
) -> (InvocationKey, Vec<PluginBinding>) {
    let (_, bundle) = application.load_graph("source").unwrap();
    let mounted = crate::plugin_library::Catalog::new(application.library_root())
        .resolve(&["graph-tasks".into()])
        .unwrap();
    let mut record = GraphRunRecord::create_with_id(bundle.snapshot, json!({}), run).unwrap();
    record.plugin_bindings = mounted
        .iter()
        .map(|binding| (binding.id.clone(), binding.clone()))
        .collect();
    record.plugin_bindings_initialized = true;
    fixtures::create(application, &record);
    let mut metadata = RunMetadata::new(
        record.run_id.clone(),
        "source".into(),
        record.graph_digest.clone(),
        &application.graph_bundle_path("source"),
    )
    .unwrap();
    metadata.oauth_owner = Some(owner.into());
    metadata.task_grants = assistant_plugins::publication(
        &application.data_root,
        &metadata.bundle_source,
        &metadata.graph_digest,
    )
    .unwrap()
    .map(|receipt| receipt.task_grants());
    metadata::save(&application.data_root, &metadata).unwrap();
    let key = InvocationKey {
        run_id: record.run_id.clone(),
        graph_digest: record.graph_digest.clone(),
        node_id: "work".into(),
        invocation: 1,
    };
    (key, mounted)
}

#[tokio::test]
async fn mounted_plugin_requires_an_explicit_operator_receipt() {
    let (_root, application) = fixture();
    let policy = GraphTaskPolicy {
        graphs: vec!["selected".into()],
        develop_plugins: true,
        ..Default::default()
    };
    write(&application, "selected", &agent(json!([])));
    let definition = source(&policy);
    write(&application, "source", &definition);
    let owner = crate::credential_store::binding_owner("api:fixture");
    let (key, mounted) = admit_source(&application, "source-run", &owner);
    assert!(application.graph_task_origin(&key, &mounted).is_err());
    application
        .update_graph_from_agent("source", definition.clone())
        .await
        .unwrap();
    assert!(application.graph_task_origin(&key, &mounted).is_err());
    application
        .update_graph("source", definition.clone())
        .await
        .unwrap();
    assert!(
        application.graph_task_origin(&key, &mounted).is_err(),
        "a save cannot upgrade an already admitted Run"
    );
    let (next_key, _) = admit_source(&application, "next-source-run", &owner);
    let authorized = application.graph_task_origin(&next_key, &mounted).unwrap();
    assert_eq!(authorized.policy, policy);
    assert_eq!(authorized.owner, owner);
    assert_eq!(authorized.credential_owner(), owner);
    let stale_key = InvocationKey {
        invocation: next_key.invocation + 1,
        ..next_key.clone()
    };
    assert!(
        application.graph_task_origin(&stale_key, &mounted).is_err(),
        "only the current dispatch can delegate"
    );
    assert!(application.graph_task_origin(&next_key, &[]).is_err());
    assert!(
        application
            .authorize_task_plugin_development(&authorized)
            .is_ok()
    );
    assert!(
        !authorized.policy.create_graphs,
        "candidate testing does not implicitly enable Graph creation"
    );
    let mut changed = agent(json!([]));
    changed["objective"] = json!("new approved contract");
    write(&application, "selected", &changed);
    application
        .update_graph("source", definition)
        .await
        .unwrap();
    let rebuilt = application.graph_task_origin(&next_key, &mounted).unwrap();
    assert_eq!(
        rebuilt.targets, authorized.targets,
        "rebuilding tools retains the admission's frozen target grants"
    );
    assert!(
        application
            .authorize_delegated_graph(&rebuilt, "selected")
            .is_err()
    );
}

#[tokio::test]
async fn cancelled_task_tools_cannot_publish_after_waiting_for_the_catalog_guard() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let (root, application) = fixture();
    let origin = approved(
        &application,
        GraphTaskPolicy {
            create_graphs: true,
            ..Default::default()
        },
        None,
    )
    .await;
    let (key, mounted) = admit_source(&application, "source-run", "owner");
    let definition = agent(json!([]));
    let created = application
        .create_task_graph(&origin, definition.clone(), "create".into())
        .await
        .unwrap();
    let graph = created["graph"].as_str().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let cancellation = Arc::new(AtomicBool::new(false));
    let context = crate::application::plugin_candidates::CandidateTestContext {
        sandbox: Arc::new(
            anchor_sandbox_bwrap::BubblewrapSandbox::new(
                anchor_sandbox_bwrap::BubblewrapPolicy::new("bwrap", ["sh"])
                    .authorize_workspace_root(&workspace),
            )
            .unwrap(),
        ),
        workspace,
        access: crate::dependency_provision::DependencyAccess {
            network: false,
            cancellation: cancellation.clone(),
        },
    };
    let inner = crate::tool_host::PluginToolHost::new(std::iter::empty::<String>())
        .for_bindings(&[])
        .unwrap();
    let tools = crate::graph_task_tools::wrap(
        Arc::new(inner),
        &mounted,
        key,
        application.clone(),
        context,
        &mut crate::plugin_disclosure::PluginDisclosureCatalog::default(),
    );
    let mut changed = definition.clone();
    changed["objective"] = json!("must not be saved after cancellation");
    let guard = application.graph_catalog_mutation_guard().await;
    let mut call = tools.call(
        "graph_tasks_update",
        json!({"graph":graph,"definition":changed}),
    );
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(call.as_mut().poll(cx)))
            .await
            .is_pending()
    );
    cancellation.store(true, Ordering::Release);
    drop(guard);
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), call)
        .await
        .unwrap();
    assert!(
        matches!(result, Err(anchor_runtime::ToolError::Failed(reason)) if reason.contains("cancelled"))
    );
    assert_eq!(
        application.read_task_graph(&origin, graph).unwrap()["definition"],
        definition
    );
}

#[tokio::test]
async fn selected_graphs_pin_static_call_closures_and_operator_revocation_narrows_old_tasks() {
    let (_root, application) = fixture();
    write(&application, "leaf", &agent(json!(["offline"])));
    write(&application, "selected", &call("leaf"));
    write(&application, "unselected", &agent(json!([])));
    let policy = GraphTaskPolicy {
        graphs: vec!["selected".into()],
        ..Default::default()
    };
    let approved = approved(&application, policy.clone(), None).await;
    assert_eq!(approved.targets.len(), 2);
    assert!(
        application
            .authorize_delegated_graph(&approved, "selected")
            .is_ok()
    );
    assert!(
        application
            .authorize_delegated_graph(&approved, "unselected")
            .is_err()
    );
    let (_, leaf) = application.load_graph("leaf").unwrap();
    assert!(
        application
            .authorize_task_call_snapshot(&approved, "leaf", &leaf.snapshot)
            .is_ok()
    );
    let mut changed = agent(json!(["offline"]));
    changed["objective"] = json!("changed execution contract");
    write(&application, "leaf", &changed);
    let (_, changed_leaf) = application.load_graph("leaf").unwrap();
    assert!(
        application
            .authorize_task_call_snapshot(&approved, "leaf", &changed_leaf.snapshot)
            .is_err()
    );
    // Reapproving the same source digest cannot enlarge an already frozen task.
    application
        .update_graph("source", source(&policy))
        .await
        .unwrap();
    assert!(
        application
            .authorize_delegated_graph(&approved, "selected")
            .is_err()
    );
    let renewed = fixture_origin(&application, policy, None);
    assert!(
        application
            .authorize_delegated_graph(&renewed, "selected")
            .is_ok()
    );
    application
        .update_graph("source", source(&GraphTaskPolicy::default()))
        .await
        .unwrap();
    assert!(
        application
            .authorize_delegated_graph(&renewed, "selected")
            .is_err()
    );
    assert!(application.read_task_graph(&renewed, "selected").is_err());
    assert!(
        application.list_task_graphs(&renewed).await.unwrap()["graphs"]
            .as_array()
            .unwrap()
            .is_empty(),
        "revoked Graphs are no longer discoverable"
    );
}

#[tokio::test]
async fn generated_graph_creation_is_scope_owned_idempotent_and_does_not_install_plugins() {
    let (_root, application) = fixture();
    write(&application, "shared", &agent(json!([])));
    let policy = GraphTaskPolicy {
        create_graphs: true,
        plugins: vec!["offline".into()],
        ..Default::default()
    };
    let alice = approved(&application, policy.clone(), Some("alice")).await;
    let bob = fixture_origin(&application, policy, Some("bob"));
    let skill = std::fs::read_to_string(
        application
            .library_root()
            .join("plugins/graph-tasks/skills/graph-tasks/SKILL.md"),
    )
    .unwrap();
    let example = skill
        .split_once("```json\n")
        .unwrap()
        .1
        .split_once("\n```")
        .unwrap()
        .0;
    let example: Value = serde_json::from_str(example).unwrap();
    application
        .validate_task_graph(&alice, &example["definition"])
        .unwrap();
    let definition = agent(json!(["offline"]));
    let created = application
        .create_task_graph(&alice, definition.clone(), "request".into())
        .await
        .unwrap();
    let name = created["graph"].as_str().unwrap();
    let repeated = application
        .create_task_graph(&alice, definition.clone(), "request".into())
        .await
        .unwrap();
    assert_eq!(repeated["graph"], name);
    assert!(application.authorize_delegated_graph(&alice, name).is_ok());
    assert!(application.read_task_graph(&bob, name).is_err());
    assert!(
        application
            .update_task_graph(&bob, name, definition.clone())
            .await
            .is_err()
    );
    assert!(
        application
            .update_task_graph(&alice, "shared", definition.clone())
            .await
            .is_err()
    );
    let mut edited = definition.clone();
    edited["objective"] = json!("edited generated task");
    assert!(
        application
            .create_task_graph(&alice, edited.clone(), "request".into())
            .await
            .is_err()
    );
    application
        .update_task_graph(&alice, name, edited.clone())
        .await
        .unwrap();
    assert_eq!(
        application.read_task_graph(&alice, name).unwrap()["definition"],
        edited
    );
    let bob_created = application
        .create_task_graph(&bob, definition, "request".into())
        .await
        .unwrap();
    assert_ne!(bob_created["graph"], name);
    let alice_list = application.list_task_graphs(&alice).await.unwrap();
    assert_eq!(
        alice_list["graphs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["graph"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![name]
    );
    let catalog = crate::plugin_library::Catalog::new(application.library_root());
    assert!(catalog.definition("offline").is_ok());
    assert!(
        !application
            .library_root()
            .join("plugins/candidate")
            .exists()
    );
    assert!(
        !application.graph_bundle_path(name).join("plugins").exists(),
        "generated Graphs reuse explicit Library references, not a copied tool installation"
    );
    let ownership = task_graph_ownership_path(&application, name).unwrap();
    assert!(ownership.is_file());
    application
        .delete_graph(
            name,
            &application.graph_bundle_path(name),
            &_root.path().join("workspaces"),
        )
        .await
        .unwrap();
    assert!(
        !ownership.exists(),
        "deletion releases the generated asset reservation"
    );
}

#[tokio::test]
async fn generated_graphs_stay_within_the_intersection_of_original_and_current_capabilities() {
    let (_root, application) = fixture();
    write(&application, "selected", &agent(json!([])));
    let policy = GraphTaskPolicy {
        graphs: vec!["selected".into()],
        create_graphs: true,
        plugins: vec!["offline".into()],
        ..Default::default()
    };
    let origin = approved(&application, policy.clone(), None).await;
    assert!(
        application
            .validate_task_graph(&origin, &agent(json!(["offline"])))
            .is_ok()
    );
    assert!(
        application
            .validate_task_graph(&origin, &call("selected"))
            .is_ok()
    );
    let mut network = agent(json!([]));
    network["agents"]["role"]["network"] = json!(true);
    let mut session_call = call("selected");
    session_call["ops"]["work"]["call"]["session"] = json!("another-session");
    let host = json!({"entry":"work","ops":{"work":{"host":{"operation":"session.wait_input"}}},"nodes":[{"id":"work","op":"work"}],"edges":[]});
    for definition in [
        command(false),
        network,
        agent(json!(["other"])),
        agent(json!(["graph-tasks"])),
        session_call,
        host,
        call("source"),
        call("unselected"),
    ] {
        assert!(
            application
                .validate_task_graph(&origin, &definition)
                .is_err(),
            "{definition}"
        );
    }
    let broader = GraphTaskPolicy {
        commands: true,
        network: true,
        plugins: vec!["offline".into(), "other".into()],
        ..policy.clone()
    };
    application
        .update_graph("source", source(&broader))
        .await
        .unwrap();
    assert!(
        application
            .validate_task_graph(&origin, &command(false))
            .is_err(),
        "a later broad grant cannot enlarge an old task"
    );
    let renewed = fixture_origin(&application, broader, None);
    assert!(
        application
            .validate_task_graph(&renewed, &command(true))
            .is_ok()
    );
    application
        .update_graph("source", source(&policy))
        .await
        .unwrap();
    assert!(
        application
            .validate_task_graph(&renewed, &command(false))
            .is_err(),
        "a current narrower grant revokes future work"
    );
}

#[tokio::test]
async fn network_required_selected_plugins_are_approved_without_granting_generated_network_access()
{
    let (_root, application) = fixture();
    write(&application, "online", &agent(json!(["online"])));
    let policy = GraphTaskPolicy {
        graphs: vec!["online".into()],
        create_graphs: true,
        commands: true,
        ..Default::default()
    };
    let origin = approved(&application, policy, None).await;
    assert!(
        application
            .authorize_delegated_graph(&origin, "online")
            .is_ok(),
        "operator-selected Graphs carry their own approved Plugin/network closure"
    );
    assert!(
        application
            .validate_task_graph(&origin, &command(true))
            .is_err(),
        "Op.network cannot bypass the generated networking limit"
    );
}
