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
        // Task egress deliberately cannot reach Host loopback. Like the network
        // boundary fixture, expose only this business mock on a routable address.
        let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let business_address = route.local_addr().unwrap().ip();
        let (address_tx, address_rx) = mpsc::channel();
        let (stop, stopped) = oneshot::channel();
        let thread = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let listener = tokio::net::TcpListener::bind((business_address, 0))
                        .await
                        .unwrap();
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
    let binary = fixture::host_binary().with_file_name(format!("anchor-{name}-tools"));
    assert!(
        binary.is_file(),
        "Build native tools with cargo build -p anchor-wecom-tools --bins"
    );
    let destination = host.root.path().join("checkout").join(name);
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
    let binding = FilePluginCatalog::new(host.root.path())
        .resolve(&[plugin.to_owned()])
        .unwrap()
        .remove(0);
    fs::write(
        host.root.path().join("bundle/manifest.json"),
        json!({
            "format":1,"graph":"graph.json","plugins":[{"id":binding.id}]
        })
        .to_string(),
    )
    .unwrap();
}

fn graph() -> Value {
    json!({
        "entry":"worker","agents":{"worker":{"model":goose::MODEL,"network":true,
            "instructions":"Read the mounted WeCom SKILL, query the fixture member, save the returned member in report.txt and evidence.txt, then finish with route verify."}},
        "ops":{"verify":{"writes":["verified.txt"],"run":"sh -c 'cat /in/worker/evidence.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["wecom"]},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })
}

/// Package the WeCom plugin against the fixture endpoint and bind it to the Run.
fn install_plugin(host: &goose::Host, business: &BusinessFixture) -> PathBuf {
    let plugin = package(&host.base, "wecom");
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
    anchor_library::Library::new(host.base.root.path())
        .install_directory("wecom", &plugin, false)
        .unwrap();
    bind(&host.base, "wecom");
    host.base.root.path().join("plugins/wecom")
}

#[test]
#[ignore = "requires pinned Goose binary and native WeCom tools"]
fn native_goose_reads_plugin_skill_and_calls_rust_stdio_mcp() {
    let business = BusinessFixture::new();
    let provider = goose::Provider::new("goose-native-wecom-plugin", vec![
        goose::command("set -eu; cat /plugins/wecom/skills/wecom/SKILL.md > skill.txt; if printf corrupt >> /plugins/wecom/skills/wecom/SKILL.md 2>/dev/null; then exit 7; fi; printf readonly-package-confirmed"),
        goose::Step::tool("wecom-wecom_wecom_get_user", json!({"userid":"member-fixture"})).after("readonly-package-confirmed"),
        goose::command("printf 'member-fixture: Native fixture user\n' > report.txt; cp report.txt evidence.txt; cat report.txt").after("Native fixture user"),
        goose::complete("verify").files(&["report.txt", "evidence.txt", "skill.txt"]).after("member-fixture: Native fixture user"),

    ]);
    let host = goose::Host::new(&graph())
        .native()
        // This scenario drives the plugin tools *directly*, and Goose refuses a tool
        // that was not advertised in the turn, so a disclosed node cannot call them
        // that way. The on-demand path has its own scenario below.
        .with_extra_environment([("ANCHOR_NODE_TOOL_DISCLOSURE", "0")]);
    let plugin = install_plugin(&host, &business);
    let original_skill = fs::read(plugin.join("skills/wecom/SKILL.md")).unwrap();
    let response = host.run(&provider);
    assert_eq!(response["projection"]["outcome"], "done", "{response}");
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
            .contains("anchor_plugins")
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
#[ignore = "requires pinned Goose binary and native WeCom tools"]
fn native_goose_reaches_a_hidden_plugin_tool_through_disclosure() {
    // The plugin's tools are not advertised in this node: a direct call must be
    // refused by the agent, and the only way in is the disclosure tool.
    let business = BusinessFixture::new();
    let provider = goose::Provider::new(
        "goose-native-wecom-disclosure",
        vec![
            goose::Step::named(
                "anchor__wecom-wecom_wecom_get_user",
                json!({"userid":"member-fixture"}),
            ),
            goose::Step::tool("anchor_tools", json!({"query":"get_user"})).after("not advertised"),
            goose::Step::tool("anchor_plugins", json!({"plugin":"wecom"}))
                .after("wecom-wecom_wecom_get_user"),
            goose::Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"wecom","section":"skills"}),
            )
            .after("wecom"),
            goose::Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"wecom","section":"skill","path":"skills/wecom/SKILL.md"}),
            )
            .after("skills/wecom/SKILL.md"),
            goose::Step::tool(
                "anchor_tool_detail",
                json!({"name":"wecom-wecom_wecom_get_user"}),
            )
            .after("sha256"),
            goose::Step::tool(
                "anchor_tools_call",
                json!({
                    "name":"wecom-wecom_wecom_get_user",
                    "arguments":{"userid":"member-fixture"}
                }),
            )
            .after("wecom-wecom_wecom_get_user"),
            goose::command(
                "printf 'member-fixture: Native fixture user\n' > evidence.txt; cat evidence.txt",
            )
            .after("member-fixture"),
            goose::complete("verify")
                .files(&["evidence.txt"])
                .after("member-fixture: Native fixture user"),
        ],
    );
    let host = goose::Host::new(&graph())
        .native()
        .with_extra_environment([("ANCHOR_NODE_TOOL_DISCLOSURE", "1")]);
    install_plugin(&host, &business);
    let response = host.run(&provider);
    assert_eq!(response["projection"]["outcome"], "done", "{response}");
    provider.assert_consumed();

    let mut advertised = provider.requests()[0]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    advertised.sort();
    assert_eq!(
        advertised,
        [
            "anchor__anchor_edit",
            "anchor__anchor_plugin_read",
            "anchor__anchor_plugins",
            "anchor__anchor_read",
            "anchor__anchor_run",
            "anchor__anchor_tool_detail",
            "anchor__anchor_tools",
            "anchor__anchor_tools_call",
            "anchor__final_result"
        ],
        "plugin tools stay hidden: the model must discover them"
    );

    let saved = host.record("fixture");
    assert_eq!(
        host.base.file(&saved, "worker", "evidence.txt"),
        b"member-fixture: Native fixture user\n"
    );
    assert_eq!(
        host.base.file(&saved, "verify", "verified.txt"),
        b"member-fixture: Native fixture user\n"
    );
    let calls = business.calls.0.lock().unwrap().clone();
    assert_eq!(
        calls,
        vec![json!({"kind":"token"}), json!({"kind":"member"})],
        "the refused direct call must not reach the plugin"
    );
    let conversation = host.native_conversation("fixture", "worker", 1);
    assert!(
        conversation.to_string().contains("not advertised"),
        "the refusal must be visible to the model"
    );
    host.evidence(
        &provider,
        "fixture",
        json!({
            "case_source":"tests/native_plugins.rs",
            "plugin_tools_hidden":true,
            "direct_call_refused_before_the_bridge":true,
            "reached_the_tool_through_disclosure":true,
            "business_calls":calls,
            "production_calls":0
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary, native Git tools and local Bubblewrap"]
fn native_goose_maintains_a_git_worktree_shared_with_node_tools() {
    let git = |cwd: &str, args: &[&str]| {
        goose::Step::tool(
            "anchor_tools_call",
            json!({
                "name":"git-git_git_run","arguments":{"cwd":cwd,"args":args}
            }),
        )
    };
    let provider = goose::Provider::new("goose-native-git-worktree", vec![
        goose::Step::tool("anchor_tools", json!({"query":"git_run"})),
        goose::Step::tool("anchor_tool_detail", json!({"name":"git-git_git_run"})).after("git-git_git_run"),
        goose::Step::tool("anchor_plugin_read", json!({"plugin":"git","section":"skill","path":"skills/git/SKILL.md"})),
        git(".", &["--version"]).after("git_run"),
        git(".", &["init","--bare","remote.git"]).after("git version"),
        git(".", &["clone","remote.git","project"]).after("exit_code"),
        goose::Step::tool("anchor_edit", json!({"path":"project/answer.txt","content":"42\n"})).after("exit_code"),
        goose::command("set -eu; cd project; test \"$(cat answer.txt)\" = 42; git status --short; printf 'project-test-passed\\n' > /workspace/report.txt; cat /workspace/report.txt").after("sha256_after"),
        git("project", &["switch","-c","fix/answer"]).after("project-test-passed"),
        git("project", &["add","--","answer.txt"]).after("exit_code"),
        git("project", &["commit","-m","Fix answer from AgentNode"]).after("exit_code"),
        git("project", &["push","-u","origin","fix/answer"]).after("Fix answer"),
        // Git itself invokes the packaged helper using the Host-injected
        // configuration; raw credentials must never reach native history.
        goose::Step::tool("anchor_tool_detail", json!({"name":"git-https_git_run"})).after("exit_code"),
        goose::Step::tool("anchor_tools_call", json!({"name":"git-https_git_run","arguments":{"cwd":"project","args":["-c", "alias.credential-check=!printf 'protocol=https\\nhost=git.example\\npath=team/project.git\\n\\n' | git credential fill", "credential-check"]}})).after("git-https_git_run"),
        goose::Step::tool("anchor_tool_detail", json!({"name":"git-host_git_run"})).after("[REDACTED]"),
        goose::Step::tool("anchor_tools_call", json!({"name":"git-host_git_run","arguments":{"args":["-c", "alias.ssh-fixture=!sh -c \"$GIT_SSH_COMMAND -G anchor-fixture\"", "ssh-fixture"]}})).after("git-host_git_run"),
        git(".", &["--git-dir=remote.git","show","fix/answer:answer.txt"]).after("fixture.example"),
        goose::command("set -eu; git -C project diff --exit-code; test -z \"$(git -C project status --porcelain)\"; git --git-dir=remote.git log fix/answer --format='%s|%an|%ae'; cat report.txt").after("42"),
        goose::complete("verify").files(&["report.txt", "project/answer.txt"]).after("project-test-passed"),
    ]);
    let host = goose::Host::new(&json!({
        "entry":"worker","agents":{"worker":{"model":goose::MODEL,"network":false,
            "instructions":"Use the Git Plugin with native Git and the node workspace to develop, test, commit and push a fixture change. Never print configured credentials."}},
        "ops":{"verify":{"writes":["verified.txt"],"run":"sh -c 'cat /in/worker/report.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["git"]},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })).native().with_extra_environment([("ANCHOR_NODE_TOOL_DISCLOSURE","1")]);
    let library_root = host.base.root.path().to_path_buf();
    let host = host.with_extra_environment([("ANCHOR_RUNNER_LIBRARY_ROOT", library_root)]);
    let source = host.base.root.path().join("checkout/git");
    fs::create_dir_all(source.join("bin")).unwrap();
    fs::create_dir_all(source.join("skills/git")).unwrap();
    fs::write(
        source.join("plugin.json"),
        include_bytes!("../../../plugins/git/plugin.json"),
    )
    .unwrap();
    fs::write(
        source.join("skills/git/SKILL.md"),
        include_bytes!("../../../plugins/git/skills/git/SKILL.md"),
    )
    .unwrap();
    let binary = fixture::host_binary().with_file_name("anchor-git-tools");
    assert!(binary.is_file(), "build anchor-git-tools --bins first");
    fs::copy(binary, source.join("bin/anchor-git-tools")).unwrap();
    anchor_library::Library::new(host.base.root.path())
        .install_directory("git", &source, false)
        .unwrap();
    bind(&host.base, "git");
    let ssh = host.base.root.path().join("operator-ssh");
    fs::create_dir(&ssh).unwrap();
    fs::write(ssh.join("config"), format!("Host anchor-fixture\n  HostName fixture.example\n  User fixture-user\n  IdentityFile {}/identity\n  UserKnownHostsFile {}/known_hosts\n", ssh.display(), ssh.display())).unwrap();
    fs::create_dir_all(host.base.root.path().join("tools/git-host-ssh")).unwrap();
    fs::write(
        host.base.root.path().join("tools/git-host-ssh/tool.json"),
        json!({
            "plugin":"git", "entrypoint":"/usr/bin/ssh", "imports":[ssh]
        })
        .to_string(),
    )
    .unwrap();
    let server = host.serve(&provider);
    let (status, configuration) = server.request("GET", "/plugins/git/configuration", None);
    assert_eq!(status, 200, "{configuration}");
    let (status, configured) = server.request("PUT", "/plugins/git/configuration", Some(&json!({
        "authorization_digest":configuration["authorization_digest"],"revision":configuration["revision"],
        "fields":{
            "author_name":{"action":"set","value":"Anchor fixture"},
            "author_email":{"action":"set","value":"fixture@example.invalid"},
            "https_url":{"action":"set","value":"https://git.example/team/project.git"},
            "https_username":{"action":"set","value":"oauth2"},
            "https_token":{"action":"set","value":"fixture-private-git-token"},
            "ssh_config":{"action":"set","value":ssh.join("config")}
        }
    })));
    assert_eq!(status, 200, "{configured}");
    let run = server.trigger();
    server.wait_status(&run, "completed");
    provider.assert_consumed();
    let record = host.record(&run);
    assert_eq!(
        host.base.file(&record, "worker", "project/answer.txt"),
        b"42\n"
    );
    assert_eq!(
        host.base.file(&record, "verify", "verified.txt"),
        b"project-test-passed\n"
    );
    let history = host.native_conversation(&run, "worker", 1).to_string();
    assert!(history.contains("Fix answer from AgentNode|Anchor fixture|fixture@example.invalid"));
    assert!(!history.contains("fixture-private-git-token"));
    assert!(
        !provider
            .requests()
            .iter()
            .any(|request| request.to_string().contains("fixture-private-git-token"))
    );
    host.evidence(
        &provider,
        &run,
        json!({"case_source":"tests/native_plugins.rs",
        "native_git":true,"shared_workspace":true,"clone_edit_test_commit_push":true,
        "git_credential_helper":true,"credentials_absent_from_history":true,
        "host_ssh_configuration":true,"operator_plugin_scoped_imports":true,
        "real_git_service_requests":0,"real_model_requests":0}),
    );
}
