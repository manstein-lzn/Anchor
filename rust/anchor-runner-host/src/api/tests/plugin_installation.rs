use super::*;
use anchor_library::{Checkout, GithubSource, InstallError};
use std::{
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const SOURCE: &str = "https://github.com/fixture/plugins/tree/main/demo";

struct FailedCheckout {
    uncertain: bool,
}

impl Checkout for FailedCheckout {
    fn checkout(&self, _source: &GithubSource, _destination: &Path) -> Result<(), InstallError> {
        Err(if self.uncertain {
            InstallError::PublicationUncertain
        } else {
            InstallError::Io(std::io::Error::other("private-checkout-error-sentinel"))
        })
    }
}

#[derive(Default)]
struct FixtureCheckout {
    calls: AtomicUsize,
    text: Mutex<String>,
    malformed: AtomicBool,
    waiting: AtomicBool,
    gate: (Mutex<bool>, Condvar),
}

impl FixtureCheckout {
    fn release(&self) {
        *self.gate.0.lock().unwrap() = true;
        self.gate.1.notify_all();
    }
}

impl Checkout for FixtureCheckout {
    fn checkout(&self, _source: &GithubSource, destination: &Path) -> Result<(), InstallError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.waiting.load(Ordering::SeqCst) {
            let mut released = self.gate.0.lock().unwrap();
            while !*released {
                released = self.gate.1.wait(released).unwrap();
            }
        }
        let plugin = destination.join("demo");
        std::fs::create_dir_all(plugin.join(".codex-plugin")).unwrap();
        std::fs::create_dir_all(plugin.join("skills/demo")).unwrap();
        std::fs::write(
            plugin.join(".codex-plugin/plugin.json"),
            if self.malformed.load(Ordering::SeqCst) {
                r#"{"name":12,"secret":"never-return-this"}"#
            } else {
                r#"{"name":"Fixture Plugin","skills":"skills/"}"#
            },
        )
        .unwrap();
        std::fs::write(
            plugin.join("skills/demo/SKILL.md"),
            self.text.lock().unwrap().as_bytes(),
        )
        .unwrap();
        Ok(())
    }
}

fn install_body(replace_existing: bool) -> String {
    json!({"source":SOURCE,"id":"demo","replace":replace_existing}).to_string()
}

#[tokio::test]
async fn plugin_installation_flattens_manifest_and_graphs_keep_frozen_resources() {
    let _environment = PROCESS_ENV.lock().await;
    let (_root, mut state) = fixture();
    let checkout = Arc::new(FixtureCheckout::default());
    *checkout.text.lock().unwrap() = "first frozen instructions".into();
    state.plugin_checkout = Some(checkout.clone());
    let app = router(state.clone());
    let (status, installed) = call(
        app.clone(),
        "POST",
        "/plugins/install",
        Some(&install_body(false)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{installed}");
    assert_eq!(installed["id"], "demo");
    assert!(
        !state
            .catalog_root
            .join("plugins/demo/.codex-plugin")
            .exists()
    );
    let definition = json!({"objective":"installed Plugin","entry":"work",
        "agents":{"work":{"model":"fixture-unused","instructions":"Read the installed Skill"}},
        "ops":{},"nodes":[{"id":"work","agent":"work","plugins":["demo"]}],"edges":[]});
    let (status, created) = call(
        app.clone(),
        "POST",
        "/graphs",
        Some(&json!({"name":"installed","definition":definition}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let graph_root = state.catalog_root.join("installed");
    let before = FileGraphBundleLoader::new(&graph_root).load().unwrap();
    assert_eq!(before.plugins[0].digest, installed["digest"]);
    *checkout.text.lock().unwrap() = "replacement instructions".into();
    let (status, replaced) = call(
        app.clone(),
        "POST",
        "/plugins/install",
        Some(&install_body(true)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{replaced}");
    assert_ne!(installed["digest"], replaced["digest"]);
    let frozen = FileGraphBundleLoader::new(&graph_root).load().unwrap();
    assert_eq!(before.plugins, frozen.plugins);
    assert_eq!(
        std::fs::read_to_string(graph_root.join("plugins/demo/skills/demo/SKILL.md")).unwrap(),
        "first frozen instructions"
    );
    let (status, detail) = call(app, "GET", "/plugins/demo", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["instructions"], "replacement instructions");
    assert_eq!(checkout.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn plugin_installation_rejects_duplicate_and_invalid_replacement_without_mutation() {
    let _environment = PROCESS_ENV.lock().await;
    let (_root, mut state) = fixture();
    let checkout = Arc::new(FixtureCheckout::default());
    *checkout.text.lock().unwrap() = "retained".into();
    state.plugin_checkout = Some(checkout.clone());
    let app = router(state.clone());
    let (status, first) = call(
        app.clone(),
        "POST",
        "/plugins/install",
        Some(&install_body(false)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, _) = call(
        app.clone(),
        "POST",
        "/plugins/install",
        Some(&install_body(false)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    checkout.malformed.store(true, Ordering::SeqCst);
    let (status, failure) = call(
        app.clone(),
        "POST",
        "/plugins/install",
        Some(&install_body(true)),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!failure.to_string().contains("never-return-this"));
    let (status, detail) = call(app, "GET", "/plugins/demo", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["digest"], first["digest"]);
    assert_eq!(detail["instructions"], "retained");
}

#[tokio::test]
async fn plugin_installation_rejects_untrusted_sources_before_checkout() {
    let _environment = PROCESS_ENV.lock().await;
    let (_root, mut state) = fixture();
    let checkout = Arc::new(FixtureCheckout::default());
    state.plugin_checkout = Some(checkout.clone());
    let app = router(state);
    for source in [
        "/etc",
        "file:///etc",
        "http://github.com/a/b/tree/main/c",
        "https://user:secret@github.com/a/b/tree/main/c",
        "https://evil.test/a/b/tree/main/c",
    ] {
        let (status, error) = call(
            app.clone(),
            "POST",
            "/plugins/install",
            Some(&json!({"source":source}).to_string()),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{source}: {error}"
        );
        assert!(!error.to_string().contains("user:secret"));
    }
    let (status, _) = call(
        app,
        "POST",
        "/plugins/install",
        Some(&json!({"source":SOURCE,"id":"../escape"}).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(checkout.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn plugin_installation_authenticates_before_checkout_or_library_mutation() {
    let _environment = PROCESS_ENV.lock().await;
    let (_root, mut state) = fixture();
    let checkout = Arc::new(FixtureCheckout::default());
    state.plugin_checkout = Some(checkout.clone());
    state.loopback = false;
    state.api_keys = vec!["k".repeat(32)];
    let response = router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/plugins/install")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(install_body(false)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(checkout.calls.load(Ordering::SeqCst), 0);
    assert!(!state.catalog_root.join("plugins").exists());
}

#[tokio::test]
async fn plugin_installation_reports_storage_and_uncertain_failures_without_leaking_details() {
    let _environment = PROCESS_ENV.lock().await;
    for uncertain in [false, true] {
        let (_root, mut state) = fixture();
        state.plugin_checkout = Some(Arc::new(FailedCheckout { uncertain }));
        let app = router(state.clone());
        let (status, failure) =
            call(app, "POST", "/plugins/install", Some(&install_body(false))).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{failure}");
        assert!(
            !failure
                .to_string()
                .contains("private-checkout-error-sentinel")
        );
        assert!(!state.catalog_root.join("plugins/demo").exists());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_installation_keeps_catalog_guard_after_http_request_is_cancelled() {
    let _environment = PROCESS_ENV.lock().await;
    let (_root, mut state) = fixture();
    let checkout = Arc::new(FixtureCheckout::default());
    checkout.waiting.store(true, Ordering::SeqCst);
    state.plugin_checkout = Some(checkout.clone());
    let app = router(state.clone());
    let request = tokio::spawn(async move {
        call(app, "POST", "/plugins/install", Some(&install_body(false))).await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while checkout.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    let application = state.application.clone();
    let mutation =
        tokio::spawn(async move { application.create_graph("after-install", None).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let prematurely_finished = mutation.is_finished();
    checkout.release();
    mutation.await.unwrap().unwrap();
    assert!(!prematurely_finished);
    assert!(
        state
            .catalog_root
            .join("plugins/demo/plugin.json")
            .is_file()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn graph_freezing_rejects_an_independent_installation_in_progress() {
    let _environment = PROCESS_ENV.lock().await;
    let (_root, state) = fixture();
    let checkout = Arc::new(FixtureCheckout::default());
    checkout.waiting.store(true, Ordering::SeqCst);
    let installer_checkout = checkout.clone();
    let library_root = state.catalog_root.clone();
    let installer = tokio::task::spawn_blocking(move || {
        anchor_library::Library::new(library_root).install_with_checkout(
            &anchor_library::InstallRequest {
                source: SOURCE.into(),
                id: Some("demo".into()),
                replace_existing: false,
            },
            installer_checkout.as_ref(),
        )
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while checkout.calls.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (status, response) = call(
        router(state.clone()),
        "POST",
        "/graphs",
        Some(&json!({"name":"during-install"}).to_string()),
    )
    .await;
    checkout.release();
    installer.await.unwrap().unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert!(!state.catalog_root.join("during-install").exists());
    assert!(!state.catalog_root.read_dir().unwrap().any(|entry| {
        entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".during-install")
    }));
}
