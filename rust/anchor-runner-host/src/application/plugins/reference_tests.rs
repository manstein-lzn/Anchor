use super::*;
use crate::application::AdmissionOptions;
use crate::application::run_access::fixtures;
use anchor_platform_session::{CreateSession, SessionStore};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{fs, time::Duration};

use crate::test_support::Environment;

fn environment(root: &Path) -> Environment {
    let names = [
        "ANCHOR_RUNNER_WORKSPACE_ROOT",
        "ANCHOR_RUNNER_ALLOWED_COMMANDS",
        "ANCHOR_RUNNER_LOCAL_INPUTS_ROOT",
        "ANCHOR_MODEL_API_KEY",
        "ANCHOR_MODEL_URL",
        "ANCHOR_MODEL_NAME",
        "ANCHOR_MODEL_WIRE_API",
        "ANCHOR_MODEL_DIRECT",
        "ANCHOR_MODEL_ALIASES",
        "ANCHOR_MODEL_CONTEXT_WINDOW",
        "ANCHOR_RUNNER_AGENT_RUNTIME",
        "ANCHOR_GOOSE_BINARY",
        "ANCHOR_GOOSE_BINARY_SHA256",
        "ANCHOR_GOOSE_LOCAL_NETWORK",
        "ANCHOR_GOOSE_ALLOW_SHARED_NETWORK",
    ];
    let mut environment = Environment::cleared(&names);
    // Admission needs a valid provider configuration for the Agent that
    // owns this Plugin reference. The preceding real Op gate is cancelled
    // before that Agent can execute, so Goose/model transport must not run.
    let binary = root.join("unstarted-goose");
    let bytes = b"Goose must not execute in this reference admission fixture";
    fs::write(&binary, bytes).unwrap();

    environment.set("ANCHOR_RUNNER_WORKSPACE_ROOT", root.join("workspaces"));

    environment.set("ANCHOR_RUNNER_ALLOWED_COMMANDS", "sh,sleep,printf");
    environment.set("ANCHOR_RUNNER_AGENT_RUNTIME", "goose");
    environment.set("ANCHOR_GOOSE_BINARY", &binary);
    environment.set(
        "ANCHOR_GOOSE_BINARY_SHA256",
        format!("{:x}", Sha256::digest(bytes)),
    );
    environment.set("ANCHOR_GOOSE_LOCAL_NETWORK", "0");
    environment.set("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK", "1");
    environment.set("ANCHOR_MODEL_DIRECT", "0");
    crate::model_configuration::ModelConfiguration::from_env(&root.join("state"))
            .unwrap()
            .save(serde_json::from_value(json!({"endpoint":"http://127.0.0.1:1/v1","wire_api":"chat","model":"admission-fixture","api_key":{"action":"set","value":"admission-fixture-only"}})).unwrap())
            .unwrap();
    environment
}

fn fixture(root: &Path) -> (RunApplication, SessionStore) {
    let candidate = root.join("candidate");
    fs::create_dir_all(candidate.join("state")).unwrap();
    fs::write(
        candidate.join("plugin.json"),
        json!({"name":"Demo","mutable":["state/"]}).to_string(),
    )
    .unwrap();
    crate::plugin_library::PluginLibrary::new(root.join("library"))
        .install_directory("demo", &candidate, false)
        .unwrap();
    fs::write(
        root.join("library/plugins/demo/state/data.bin"),
        b"precious",
    )
    .unwrap();
    let bundle = root.join("bundle");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(
        bundle.join("graph.json"),
        json!({
            "objective":"Plugin reference publication fixture","entry":"work",
            "agents":{"worker":{"model":"admission-fixture","instructions":"not reached"}},
            "ops":{"work":{
                "run":"printf ready > gate.ready; while [ ! -f gate.release ]; do sleep 0.02; done",
                "wall_time_limit_seconds":10
            }},
            "nodes":[{"id":"work","op":"work"},{"id":"agent","agent":"worker","plugins":["demo"]}],
            "edges":[{"from":"work","to":"agent"}]
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"demo"}]}).to_string(),
    )
    .unwrap();
    fs::create_dir_all(root.join("state/platform")).unwrap();
    let sessions = SessionStore::open(root.join("state/platform/sessions.sqlite")).unwrap();
    sessions.create("local", CreateSession::default()).unwrap();
    (
        RunApplication::new(root.join("state"), root.to_path_buf())
            .with_host_configuration(Arc::new(crate::HostConfiguration {
                state_root: Some(root.join("state")),
                library_root: Some(root.join("library")),
                ..Default::default()
            }))
            .with_configured_graph("fixture".into(), bundle),
        sessions,
    )
}

fn admission_options() -> AdmissionOptions {
    AdmissionOptions {
        objective: None,
        trigger: Default::default(),
        pilot: None,
        oauth_owner: None,
        workspace_bindings: Default::default(),
        workspace_root: None,
    }
}

async fn wait_for_library_reader(library: &Path) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match crate::plugin_library::PluginLibrary::new(library).try_install_lease() {
                Err(InstallError::CatalogBusy) => return,
                Ok(lease) => drop(lease),
                Err(error) => panic!("cannot inspect the Library lease: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("admission must acquire its Library lease before waiting for active Runs");
}

async fn stop_gate(application: &RunApplication, root: &Path, run: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = fixtures::view(application, run);
            assert!(!record.is_settled(), "{record:?}");
            if let Some(dispatch) = record.dispatches().first() {
                let workspace = crate::HostArtifacts::new(
                    root.join("state/artifacts"),
                    root.join("workspaces"),
                )
                .workspace_path(&dispatch.key)
                .unwrap();
                if workspace.join("gate.ready").is_file() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the real Op did not reach its fixture gate");
    // The short publication protection has ended while execution is running.
    drop(
        crate::plugin_library::PluginLibrary::new(root.join("library"))
            .try_install_lease()
            .expect("model/Op execution must not retain the publication lease"),
    );
    application.control(run, "stop").await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let record = fixtures::view(application, run);
            assert!(!record.is_settled(), "{record:?}");
            if record.has_stopped_execution() && !application.run_is_active(run).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the fixture Run did not stop before Agent execution");
}

#[tokio::test]
async fn concurrent_run_admission_publishes_its_reference_before_uninstall_rechecks() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let _environment = environment(root.path());
    let (application, sessions) = fixture(root.path());
    let library = root.path().join("library");
    let precondition = application
        .plugin_uninstall_precondition(&library, "demo", &sessions)
        .await
        .unwrap();
    assert!(precondition.refusal().is_none());
    let (source, bundle) = application.load_graph("fixture").unwrap();
    let graph_lease = application.graph_admission_lease(&source).unwrap();
    let held_active = application.admission.lock().await;
    let admitting = tokio::spawn({
        let application = application.clone();
        async move {
            application
                .admit(
                    "fixture".into(),
                    &source,
                    bundle,
                    json!({}),
                    admission_options(),
                    graph_lease,
                )
                .await
        }
    });
    wait_for_library_reader(&library).await;
    let deleting = tokio::spawn({
        let (application, sessions, library) =
            (application.clone(), sessions.clone(), library.clone());
        async move {
            application
                .uninstall_plugin_if_unchanged(&library, &precondition, &sessions)
                .await
        }
    });
    assert!(!admitting.is_finished());
    drop(held_active);
    let (run, deletion) = tokio::time::timeout(Duration::from_secs(5), async {
        (admitting.await.unwrap().unwrap(), deleting.await.unwrap())
    })
    .await
    .expect("Run admission and uninstall deadlocked");
    let error = deletion.unwrap_err();
    assert!(
        matches!(error, ApplicationError::Conflict(ref reason) if reason.contains(&run)),
        "{error:?}"
    );
    assert!(
        fixtures::record(&application, &run)
            .plugin_bindings
            .contains_key("demo")
    );
    assert_eq!(
        fs::read(library.join("plugins/demo/state/data.bin")).unwrap(),
        b"precious"
    );
    stop_gate(&application, root.path(), &run).await;
}

#[tokio::test]
async fn an_uninstalled_plugin_cannot_be_admitted_from_an_already_loaded_bundle() {
    let _env_guard = crate::PROCESS_ENV_LOCK.lock().await;
    let root = tempfile::tempdir().unwrap();
    let _environment = environment(root.path());
    let (application, sessions) = fixture(root.path());
    let library = root.path().join("library");
    let (source, bundle) = application.load_graph("fixture").unwrap();
    let graph_lease = application.graph_admission_lease(&source).unwrap();
    let precondition = application
        .plugin_uninstall_precondition(&library, "demo", &sessions)
        .await
        .unwrap();
    application
        .uninstall_plugin_if_unchanged(&library, &precondition, &sessions)
        .await
        .unwrap();
    let error = application
        .admit(
            "fixture".into(),
            &source,
            bundle,
            json!({}),
            admission_options(),
            graph_lease,
        )
        .await
        .unwrap_err();
    assert!(format!("{error:?}").contains("demo"), "{error:?}");
    assert!(application.records().unwrap().is_empty());
    assert!(application.active_runs(None).await.is_empty());
    assert!(!library.join("plugins/demo").exists());
}
