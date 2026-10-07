#[path = "support/runtime_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "support/goose_fixture.rs"]
#[allow(dead_code)]
mod goose;

use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::HeaderMap,
    routing::{get, post},
};
use fixture::{Host, read_json};
#[cfg(feature = "legacy-regression")]
use fixture::{Provider, Reply, command, complete, evidence};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex, mpsc},
    thread,
};
use tokio::sync::oneshot;

const PAGE: &str = "11111111-1111-4111-8111-111111111111";
const ATTACHMENT: &str = "22222222-2222-4222-8222-222222222222";

#[derive(Clone, Default)]
struct Calls(Arc<Mutex<Vec<Value>>>);

struct BusinessFixture {
    endpoint: String,
    calls: Calls,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

async fn token(
    State(calls): State<Calls>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    assert_eq!(params["corpid"], "fixture-corp");
    assert_eq!(params["corpsecret"], "fixture-secret");
    calls.0.lock().unwrap().push(json!({"kind":"token"}));
    Json(json!({"errcode":0,"access_token":"fixture-access-token","expires_in":7200}))
}

async fn member(
    State(calls): State<Calls>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    assert_eq!(params["access_token"], "fixture-access-token");
    assert_eq!(params["userid"], "member-fixture");
    calls.0.lock().unwrap().push(json!({"kind":"member"}));
    Json(json!({"errcode":0,"userid":"member-fixture","name":"Native fixture user"}))
}

async fn upload(State(calls): State<Calls>, headers: HeaderMap, body: Bytes) -> Json<Value> {
    assert_eq!(headers["authorization"], "Bearer fixture-docmost-key");
    assert!(
        headers["content-type"]
            .to_str()
            .unwrap()
            .starts_with("multipart/form-data;")
    );
    let multipart = String::from_utf8(body.to_vec()).unwrap();
    assert!(multipart.contains("name=\"pageId\""));
    assert!(multipart.contains(PAGE));
    assert!(multipart.contains("filename=\"panel.png\""));
    assert!(multipart.contains("fixture-image"));
    assert!(multipart.to_lowercase().contains("content-type: image/png"));
    calls
        .0
        .lock()
        .unwrap()
        .push(json!({"kind":"upload","bytes":body.len()}));
    Json(json!({"id":ATTACHMENT,"fileName":"panel.png","mimeType":"image/png","pageId":PAGE}))
}

impl BusinessFixture {
    fn new() -> Self {
        let calls = Calls::default();
        let state = calls.clone();
        let (address_tx, address_rx) = mpsc::channel();
        let (stop, stopped) = oneshot::channel();
        let thread = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    address_tx.send(listener.local_addr().unwrap()).unwrap();
                    let app = Router::new()
                        .route("/cgi-bin/gettoken", get(token))
                        .route("/cgi-bin/user/get", get(member))
                        .route("/api/files/upload", post(upload))
                        .with_state(state);
                    axum::serve(listener, app)
                        .with_graceful_shutdown(async {
                            let _ = stopped.await;
                        })
                        .await
                        .unwrap();
                });
        });
        Self {
            endpoint: format!("http://{}", address_rx.recv().unwrap()),
            calls,
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}

impl Drop for BusinessFixture {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn package(host: &Host, name: &str) -> PathBuf {
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_anchor-runner-host"))
        .with_file_name(format!("anchor-{name}-tools"));
    assert!(
        binary.is_file(),
        "Build native tools with cargo build -p anchor-wecom-tools -p anchor-docmost-tools --bins"
    );
    let destination = host.root.path().join("bundle/plugins").join(name);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    let result = Command::new(&binary)
        .arg("package-plugin")
        .arg(&destination)
        .env_clear()
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Native package failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    for path in ["plugin.json", "skills"].map(|path| destination.join(path)) {
        assert!(path.exists());
    }
    assert!(!destination.join("server.py").exists());
    assert!(!destination.join("upload_server.py").exists());
    destination
}

fn bind(host: &Host, plugin: &str) {
    let binding = FilePluginCatalog::new(host.root.path().join("bundle"))
        .resolve(&[plugin.to_owned()])
        .unwrap()
        .remove(0);
    fs::write(host.root.path().join("bundle/manifest.json"), json!({
        "format":1,"graph":"graph.json","plugins":[{
            "id":binding.id,"digest":binding.digest,"resources":binding.resources,"mcp_servers":binding.mcp_servers
        }]
    }).to_string()).unwrap();
}

#[test]
#[ignore = "requires pinned Goose binary and native WeCom tools"]
fn native_goose_reads_plugin_skill_and_calls_rust_stdio_mcp() {
    let business = BusinessFixture::new();
    let provider = goose::Provider::new("goose-native-wecom-plugin", vec![
        goose::command("set -eu; cat /plugins/wecom/skills/wecom/SKILL.md > skill.txt; if printf corrupt >> /plugins/wecom/skills/wecom/SKILL.md 2>/dev/null; then exit 7; fi; printf readonly-package-confirmed"),
        goose::Step::tool("wecom-wecom_wecom_get_user", json!({"userid":"member-fixture"})).after("readonly-package-confirmed"),
        goose::command("printf 'member-fixture: Native fixture user\n' > report.txt; cp report.txt evidence.txt; cat report.txt").after("Native fixture user"),
        goose::complete("verify").after("member-fixture: Native fixture user"),
        goose::Step::text("Plugin instructions and member lookup verified"),
    ]);
    let host = goose::Host::new(&json!({
        "entry":"worker","agents":{"worker":{"model":goose::MODEL,"network":true,
            "instructions":"Read the mounted WeCom SKILL, query the fixture member, save the returned member in report.txt and evidence.txt, then finish with route verify."}},
        "ops":{"verify":{"run":"sh -c 'cat /in/worker/evidence.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["wecom"]},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })).native();
    let plugin = package(&host.base, "wecom");
    let path = plugin.join("plugin.json");
    let original_skill = fs::read(plugin.join("skills/wecom/SKILL.md")).unwrap();
    let mut manifest = read_json(&path);
    manifest["mcpServers"]["wecom"]["env"] = json!({
        "WECOM_API_BASE_URL":business.endpoint,"WECOM_CORP_ID":"fixture-corp",
        "WECOM_AGENT_ID":"1","WECOM_SECRET":"fixture-secret"
    });
    manifest["mcpServers"]["wecom"]
        .as_object_mut()
        .unwrap()
        .remove("optional_env_vars");
    fs::write(path, manifest.to_string()).unwrap();
    bind(&host.base, "wecom");
    let response = host.run(&provider);
    assert_eq!(response["status"], "completed", "{response}");
    provider.assert_consumed();
    let saved = host.record("fixture");
    assert_eq!(
        host.base.file(&saved, "worker", "report.txt"),
        b"member-fixture: Native fixture user\n"
    );
    assert_eq!(
        host.base.file(&saved, "verify", "verified.txt"),
        host.base.file(&saved, "worker", "evidence.txt")
    );
    assert_eq!(
        host.base.file(&saved, "worker", "skill.txt"),
        original_skill
    );
    assert_eq!(
        fs::read(plugin.join("skills/wecom/SKILL.md")).unwrap(),
        original_skill
    );
    let calls = business.calls.0.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![json!({"kind":"token"}), json!({"kind":"member"})]
    );
    let conversation = host.native_conversation("fixture", "worker", 1);
    assert!(
        conversation
            .to_string()
            .contains("wecom-wecom_wecom_get_user")
    );
    assert!(conversation.to_string().contains("Native fixture user"));
    assert!(
        provider.requests()[0]["messages"]
            .to_string()
            .contains("/plugins/wecom/skills/wecom/SKILL.md")
    );
    host.evidence(
        &provider,
        "fixture",
        json!({
            "native_rust_plugin":true,"plugin_skill_readonly":true,"business_calls":calls,
            "python_required":false,"production_calls":0,"case_source":"tests/native_plugins.rs"
        }),
    );
}

#[test]
#[cfg(feature = "legacy-regression")]
fn rust_wecom_stdio_plugin_runs_through_host_harness_and_readonly_package() {
    let business = BusinessFixture::new();
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            Reply::Tool(
                "wecom-wecom_wecom_get_user",
                json!({"userid":"member-fixture"}),
            ),
            command("printf 'member-fixture\n' > report.txt; printf once > effects.txt"),
            complete(None),
        ],
    )]);
    let host = Host::new(&json!({
        "entry":"worker","agents":{"worker":{"model":"models.worker","network":true,"instructions":"Read one local fixture member with the Plugin and save its userid", "wall_time_limit_seconds":30}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["wecom"]}],"edges":[]
    }));
    let plugin = package(&host, "wecom");
    let path = plugin.join("plugin.json");
    let mut manifest = read_json(&path);
    manifest["mcpServers"]["wecom"]["env"] = json!({
        "WECOM_API_BASE_URL":business.endpoint,"WECOM_CORP_ID":"fixture-corp",
        "WECOM_AGENT_ID":"1","WECOM_SECRET":"fixture-secret"
    });
    manifest["mcpServers"]["wecom"]
        .as_object_mut()
        .unwrap()
        .remove("optional_env_vars");
    fs::write(path, manifest.to_string()).unwrap();
    bind(&host, "wecom");
    let response = host.run(&provider);
    assert_eq!(response["status"], "completed", "{response}");
    let saved = host.record();
    assert_eq!(
        host.file(&saved, "worker", "report.txt"),
        b"member-fixture\n"
    );
    assert_eq!(host.file(&saved, "worker", "effects.txt"), b"once");
    let history = host.history("fixture", "worker", 1);
    assert!(history.iter().any(|message| {
        message["commands"]
            .to_string()
            .contains("wecom-wecom_wecom_get_user")
    }));
    assert!(history.iter().any(|message| message["role"] == "tool"
        && message["text"].to_string().contains("Native fixture user")));
    let calls = business.calls.0.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![json!({"kind":"token"}), json!({"kind":"member"})]
    );
    assert!(
        provider.requests()[1]["messages"]
            .to_string()
            .contains("Native fixture user")
    );
    provider.assert_consumed();
    evidence(
        "native-wecom-stdio-plugin",
        &host,
        &provider,
        json!({"native_tool_binary":true,"business_calls":calls,"python_required":false,"production_calls":0}),
    );
}

#[test]
#[cfg(feature = "legacy-regression")]
fn rust_docmost_stdio_plugin_uploads_only_frozen_input_through_real_sandbox() {
    let business = BusinessFixture::new();
    let provider = Provider::new([(
        "fixture-worker",
        vec![
            Reply::Tool(
                "docmost-attachments_upload_page_image",
                json!({"path":"/in/publish/assets/panel.png","pageId":PAGE}),
            ),
            command(&format!(
                "printf '{ATTACHMENT}' > report.txt; printf once > effects.txt"
            )),
            complete(None),
        ],
    )]);
    let host = Host::new(&json!({
        "entry":"publish","agents":{"worker":{"model":"models.worker","network":true,"instructions":"Upload the one frozen image to the local fixture, then save its returned attachment id", "wall_time_limit_seconds":30}},
        "ops":{"seed":{"run":"sh -c 'mkdir assets; printf fixture-image > assets/panel.png'"}},
        "nodes":[{"id":"publish","op":"seed"},{"id":"worker","agent":"worker","plugins":["docmost"]}],
        "edges":[{"from":"publish","to":"worker"}]
    })).with_allowed_commands("sh,cat,git,true,mkdir");
    let plugin = package(&host, "docmost");
    let path = plugin.join("plugin.json");
    let mut manifest = read_json(&path);
    manifest["mcpServers"]
        .as_object_mut()
        .unwrap()
        .retain(|name, _| name == "attachments");
    manifest["mcpServers"]["attachments"]["args"] = json!([
        "--endpoint",
        format!("{}/api/files/upload", business.endpoint)
    ]);
    manifest["mcpServers"]["attachments"]["env"] = json!({"DOCMOST_API_KEY":"fixture-docmost-key"});
    manifest["mcpServers"]["attachments"]
        .as_object_mut()
        .unwrap()
        .remove("env_vars");
    fs::write(path, manifest.to_string()).unwrap();
    bind(&host, "docmost");
    let response = host.run(&provider);
    assert_eq!(response["status"], "completed", "{response}");
    let saved = host.record();
    assert_eq!(
        host.file(&saved, "worker", "report.txt"),
        ATTACHMENT.as_bytes()
    );
    assert_eq!(
        host.file(&saved, "publish", "assets/panel.png"),
        b"fixture-image"
    );
    let history = host.history("fixture", "worker", 1);
    assert!(history.iter().any(|message| {
        message["commands"]
            .to_string()
            .contains("docmost-attachments_upload_page_image")
    }));
    assert!(history.iter().any(
        |message| message["role"] == "tool" && message["text"].to_string().contains(ATTACHMENT)
    ));
    let calls = business.calls.0.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["kind"], "upload");
    assert!(
        provider.requests()[1]["messages"]
            .to_string()
            .contains(ATTACHMENT)
    );
    provider.assert_consumed();
    evidence(
        "native-docmost-stdio-plugin",
        &host,
        &provider,
        json!({"native_tool_binary":true,"business_calls":calls,"python_required":false,"production_calls":0,"upload_effects":1}),
    );
}
