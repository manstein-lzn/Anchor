#[path = "support/runtime_fixture.rs"]
#[allow(dead_code)]
mod fixture;
#[path = "support/goose_fixture.rs"]
#[allow(dead_code)]
mod goose;

use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use fixture::{Host, read_json};
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
        "Build native tools with cargo build -p anchor-wecom-tools --bins"
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
    }))
    .native()
    // This scenario drives the plugin tools *directly*, and Goose refuses a tool that
    // was not advertised in the turn, so a disclosed node cannot call them that way.
    // The on-demand path is covered by the disclosure scenario instead.
    .with_extra_environment([("ANCHOR_NODE_TOOL_DISCLOSURE", "0")]);
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
