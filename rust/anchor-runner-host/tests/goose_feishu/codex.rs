//! Real local Codex; scripted Goose model and loopback Feishu platform only.
use super::*;
use anchor_codex_bridge::{BridgeConfig, RunningBridge, start_server};
use anchor_mcp_host::{McpHost, McpServerConfig, McpTransportConfig, Secret};
use std::{collections::BTreeMap, path::Path, process::Command, time::Duration};

const TOKEN: &str = "codex-runtime-fixture-bearer-32-bytes-minimum";
const MANAGER: &str = "You are the Anchor Codex engineering project manager";
const WATCHER: &str = "You are a single-tick Codex observer";
const RUNNING_NOTICE: &str =
    "Codex is still running; the first scheduled observation is confirmed.";
const DONE_NOTICE: &str =
    "Codex completed the isolated worktree marker; monitoring is now finished.";
const MARKER: &str = "ANCHOR_CODEX_CHANNEL_ACCEPTANCE_OK\n";

struct LocalCodex {
    runtime: tokio::runtime::Runtime,
    bridge: Option<RunningBridge>,
    client: Option<McpHost>,
    root: tempfile::TempDir,
    project: PathBuf,
}

impl LocalCodex {
    fn start() -> Self {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        git(&project, &["init", "--quiet", "--initial-branch=main"]);
        fs::write(
            project.join("README.md"),
            "Disposable acceptance repository.\n",
        )
        .unwrap();
        git(&project, &["add", "README.md"]);
        git(
            &project,
            &["commit", "--quiet", "--no-gpg-sign", "-m", "fixture"],
        );
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let bridge = runtime
            .block_on(start_server(BridgeConfig {
                listen: "127.0.0.1:0".parse().unwrap(),
                bearer_token: TOKEN.into(),
                allow_roots: vec![project.clone()],
                worktree_root: root.path().join("worktrees"),
                codex_bin: "codex".into(),
                codex_args: vec!["app-server".into(), "--stdio".into()],
            }))
            .unwrap();
        let client = runtime
            .block_on(McpHost::connect(McpServerConfig {
                server_id: "codex".into(),
                transport: McpTransportConfig::StreamableHttp {
                    endpoint: bridge.endpoint(),
                    bearer_token: Some(Secret::new(TOKEN)),
                    headers: BTreeMap::new(),
                },
            }))
            .unwrap();
        Self {
            runtime,
            bridge: Some(bridge),
            client: Some(client),
            root,
            project,
        }
    }

    fn call(&self, name: &str, arguments: Value) -> Value {
        let result = self
            .runtime
            .block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(40),
                    self.client.as_ref().unwrap().call(name, arguments),
                )
                .await
            })
            .expect("native bridge request timed out")
            .expect("native MCP transport failed");
        assert!(
            !result.is_error,
            "native bridge tool failed; response withheld"
        );
        result
            .structured_content
            .expect("native bridge omitted structured content")
    }
}

impl Drop for LocalCodex {
    fn drop(&mut self) {
        // Also reap the native app-server if any assertion unwinds mid-task.
        self.runtime.block_on(async {
            if let Some(client) = self.client.take() {
                let _ = tokio::time::timeout(Duration::from_secs(5), client.shutdown()).await;
            }
            if let Some(bridge) = self.bridge.take() {
                let _ = bridge.shutdown().await;
            }
        });
    }
}

fn git(project: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(project)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=Anchor Acceptance",
            "-c",
            "user.email=acceptance@example.invalid",
        ])
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "disposable repository operation failed"
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn manager(steps: Vec<Step>) -> Vec<Step> {
    steps
        .into_iter()
        .map(|step| step.for_request_containing(MANAGER))
        .collect()
}

fn watch(steps: Vec<Step>) -> Vec<Step> {
    steps
        .into_iter()
        .map(|step| step.for_request_containing(WATCHER))
        .collect()
}

fn invoke(name: &str, arguments: Value) -> Step {
    Step::tool(
        "anchor_tools_call",
        json!({"name":name,"arguments":arguments}),
    )
}

fn observer(project: &Path, thread: &str, status: &'static str) -> Vec<Step> {
    let report = json!({"observations":[{"project":project,"thread_id":thread,"status":status}],"issues":[]});
    watch(vec![
        Step::tool("anchor_tool_detail", json!({"name":"codex-codex_board"}))
            .after("codex-codex_inspect"),
        invoke("codex-codex_board", json!({"project":project})).after("codex-codex_board"),
        Step::tool("anchor_tool_detail", json!({"name":"codex-codex_inspect"})).after("threads"),
        invoke("codex-codex_inspect", json!({"thread_id":thread})).after("thread_id"),
        Step::tool(
            "anchor_edit",
            json!({"path":"codex-watch.json","content":report.to_string()}),
        )
        .after(status),
        Step::tool(
            "final_result",
            json!({"summary":status,"files":["codex-watch.json"]}),
        )
        .after("sha256_after"),
    ])
}

fn await_replies(host: &Host, run: &str, count: usize) -> Value {
    let mut record = Value::Null;
    wait_until("manager handled input and returned to wait_input", || {
        record = host.record(run);
        assert_ne!(record["projection"]["outcome"], "failed", "manager failed");
        record["results"]["reply"]
            .as_array()
            .is_some_and(|rows| rows.len() == count)
            && record["frontier"][0]["node_id"] == "wait_input"
    });
    record
}

fn pending_report(sessions: &SessionStore, session: &str) -> String {
    let pending = sessions.pending_task_results("local", session).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].files.len(), 1);
    format!("/in/tasks/{}/{}", pending[0].id, pending[0].files[0].name)
}

#[test]
#[ignore = "explicit live Codex provider access; pinned Goose and real Host/Gateway with loopback model/channel fixtures"]
fn live_codex_scheduled_progress_and_completion_keep_the_manager_conversation_usable() {
    assert_eq!(
        std::env::var("ANCHOR_CODEX_LIVE_ACCEPTANCE").as_deref(),
        Ok("1"),
        "set ANCHOR_CODEX_LIVE_ACCEPTANCE=1 explicitly to authorize local Codex provider use"
    );
    let native = LocalCodex::start();
    let after_spawn = Gate::new();
    let second_watch = Gate::new();
    let first_result = Gate::new();
    let final_result = Gate::new();
    let provider = Provider::new("goose-feishu-live-codex", manager(vec![
        Step::tool("anchor_plugins", json!({"query":"codex"})).expect_request_contains(FIRST_INPUT),
        Step::tool("anchor_plugin_read", json!({"plugin":"codex","section":"skills"})).after("codex"),
        Step::tool("anchor_tools", json!({"name":"codex-codex_spawn"})),
        Step::tool("anchor_tool_detail", json!({"name":"codex-codex_spawn"})).after("codex-codex_spawn"),
        invoke("codex-codex_spawn", json!({
            "project":native.project,"worktree":"channel-acceptance","mode":"workspace-write","wait_seconds":0,
            "prompt":"This is an authorized disposable acceptance test. In this worktree only, run a shell command that waits until the file acceptance-release exists, then writes exactly ANCHOR_CODEX_CHANNEL_ACCEPTANCE_OK followed by one newline into result.txt. The test harness will create acceptance-release after observing progress. Keep waiting until it exists; do not create it yourself. Do not modify other files, commit, push, use the network, read other directories, or change configuration. After result.txt exists reply with exactly WRITTEN."
        })).after("wait_seconds"),
        Step::tool("anchor_tools", json!({"name":"graph_tasks_schedule_create"})).after("thread_id").gated(&after_spawn),
    ]));
    let platform = Platform::new();
    let host = Host::new(&json!({"entry":"work","ops":{"work":{"run":"true"}},"nodes":[{"id":"work","op":"work"}],"edges":[]}))
        .default_runtime().with_extra_environment([("ANCHOR_FEISHU_API_BASE_URL", platform.api_base_url.as_str())]);
    let (gateway, _) = install_plugin(&host);
    for system in [
        anchor_graph_host::SystemPlugin::GraphTasks,
        anchor_graph_host::SystemPlugin::Network,
    ] {
        fixture::install_system_plugin(host.base.root.path(), system);
    }
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/codex")
        .canonicalize()
        .unwrap();
    anchor_library::Library::new(host.base.root.path())
        .install_directory("codex", &source, false)
        .unwrap();
    let server = host.serve(&provider);
    let (status, config) = server.request("GET", "/plugins/codex/configuration", None);
    assert_eq!(status, 200, "{config}");
    let (status, saved) = server.request("PUT", "/plugins/codex/configuration", Some(&json!({
        "authorization_digest":config["authorization_digest"],"revision":config["revision"],"fields":{
            "bridge_url":{"action":"set","value":format!("http://{}", native.bridge.as_ref().unwrap().address)},
            "api_key":{"action":"set","value":TOKEN}
        }
    })));
    assert_eq!(status, 200, "{saved}");
    assert!(!saved.to_string().contains(TOKEN));
    let watcher: Value = serde_json::from_str(include_str!(
        "../../../../examples/graphs/codex-watch-worker.json"
    ))
    .unwrap();
    let (status, created) = server.request(
        "POST",
        "/graphs",
        Some(&json!({"name":"codex-watch-worker","definition":watcher})),
    );
    assert_eq!(status, 201, "{created}");
    let definition: Value = serde_json::from_str(include_str!(
        "../../../../examples/graphs/feishu-codex-project-manager.json"
    ))
    .unwrap();
    configure_bot_definition(&server, &definition);
    let (status, approved) = server.request(
        "PUT",
        &format!("/graphs/{GRAPH}"),
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{approved}");
    let started = std::time::Instant::now();
    platform.send(1, FIRST_MESSAGE, FIRST_INPUT);
    platform.wait_ack(1);
    after_spawn.wait_entered();
    let feedback = goose::tool_feedback(provider.requests().last().unwrap());
    let spawn: Value =
        serde_json::from_str(feedback.last().unwrap()["content"].as_str().unwrap()).unwrap();
    let spawned = &spawn["output"][0]["value"];
    let thread = spawned["thread_id"].as_str().unwrap().to_owned();
    assert_eq!(spawned["project"], json!(native.project));
    let board = native.call("board", json!({"project":native.project}));
    let threads = board["threads"].as_array().unwrap();
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0]["thread_id"], thread);
    let detail = native.call("inspect", json!({"thread_id":thread}));
    assert_eq!(detail["status"], "running");
    let cwd = PathBuf::from(detail["cwd"].as_str().unwrap());
    assert!(cwd.starts_with(native.root.path().join("worktrees")));
    assert_ne!(cwd, native.project);
    let targets = json!({"targets":[{"project":native.project,"thread_id":thread}]});
    provider.append(manager(vec![
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_schedule_create"})).after("graph_tasks_schedule_create"),
        invoke("graph_tasks_schedule_create", json!({"graph":"codex-watch-worker","rule":{"type":"interval","seconds":15},"remaining":2,
            "input":targets})).after("graph_tasks_schedule_create"),
        Step::tool("final_result", json!({"summary":FIRST_REPLY,"route":"reply","files":[]})).after("next_at"),
        Step::tool("anchor_tools", json!({"name":"feishu_send_message"})).expect_request_contains("task_result").gated(&first_result),
    ]));
    provider.append(watch(vec![Step::tool(
        "anchor_tools",
        json!({"name":"codex-codex_inspect"}),
    )]));
    provider.append(observer(&native.project, &thread, "running"));
    provider.append(watch(vec![
        Step::tool("anchor_tools", json!({"name":"codex-codex_inspect"})).gated(&second_watch),
    ]));
    after_spawn.open();
    wait_until("initial channel reply while Codex is running", || {
        platform.snapshot()["replies"].as_array().unwrap().len() == 1
    });
    let first_reply_ms = started.elapsed().as_millis();
    assert!(!cwd.join("result.txt").exists());
    first_result.wait_entered();
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let session = serde_json::to_value(&sessions.list("local").unwrap()[0]).unwrap();
    let session_id = session["id"].as_str().unwrap();
    let run = session["run_ids"][0].as_str().unwrap();
    provider.append(manager(vec![
        Step::tool(
            "anchor_read",
            json!({"path":pending_report(&sessions, session_id)}),
        )
        .after("feishu_send_message"),
        Step::tool("anchor_tool_detail", json!({"name":"feishu_send_message"})).after("running"),
        invoke("feishu_send_message", json!({"content":RUNNING_NOTICE}))
            .after("feishu_send_message"),
        Step::tool("task_result_resolve", json!({"action":"delivered"})).after("confirmed"),
        Step::tool(
            "final_result",
            json!({"summary":"Running observation delivered.","route":"reply","files":[]}),
        )
        .after("resolved"),
    ]));
    first_result.open();
    await_replies(&host, run, 2);
    assert_eq!(
        platform.snapshot()["notifications"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    provider.append(manager(vec![
        Step::tool("anchor_tools", json!({"name":"codex-codex_board"}))
            .expect_request_contains(SECOND_INPUT),
        Step::tool(
            "final_result",
            json!({"summary":SECOND_REPLY,"route":"reply","files":[]}),
        )
        .after("codex-codex_board"),
    ]));
    platform.send(2, SECOND_MESSAGE, SECOND_INPUT);
    platform.wait_ack(2);
    await_replies(&host, run, 3);
    wait_until("followup delivered before native task completion", || {
        confirmed(&sessions, session_id).len() == 2
    });
    assert!(!cwd.join("result.txt").exists());
    second_watch.wait_entered();
    fs::write(cwd.join("acceptance-release"), "continue\n").unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    let terminal = loop {
        let detail = native.call("inspect", json!({"thread_id":thread}));
        if detail["status"] == "completed" {
            break detail;
        }
        assert_eq!(
            detail["status"], "running",
            "native task requires intervention; response withheld"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "native task exceeded acceptance deadline"
        );
        thread::sleep(Duration::from_millis(250));
    };
    assert_eq!(fs::read(cwd.join("result.txt")).unwrap(), MARKER.as_bytes());
    assert!(!native.project.join("result.txt").exists());
    assert!(git(&native.project, &["status", "--porcelain"]).is_empty());
    provider.append(observer(&native.project, &thread, "completed"));
    provider.append(manager(vec![
        Step::tool("anchor_tools", json!({"name":"feishu_send_message"}))
            .expect_request_contains("task_result")
            .gated(&final_result),
    ]));
    second_watch.open();
    final_result.wait_entered();
    let (status, schedules) = server.request("GET", "/schedules", None);
    assert_eq!(status, 200, "{schedules}");
    let rows: Vec<_> = schedules["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["graph"] == "codex-watch-worker")
        .collect();
    assert_eq!(rows.len(), 1);
    let schedule = rows[0]["id"].clone();
    assert_eq!(rows[0]["input"], targets);
    assert_eq!(rows[0]["remaining"], 0);
    provider.append(manager(vec![
        Step::tool("anchor_read", json!({"path":pending_report(&sessions, session_id)})).after("feishu_send_message"),
        Step::tool("anchor_tool_detail", json!({"name":"feishu_send_message"})).after("completed"),
        invoke("feishu_send_message", json!({"content":DONE_NOTICE})).after("feishu_send_message"),
        Step::tool("anchor_tools", json!({"name":"graph_tasks_schedule_delete"})).after("confirmed"),
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_schedule_delete"})).after("graph_tasks_schedule_delete"),
        invoke("graph_tasks_schedule_delete", json!({"id":schedule})).after("graph_tasks_schedule_delete"),
        Step::tool("task_result_resolve", json!({"action":"delivered"})),
        Step::tool("final_result", json!({"summary":"Completion delivered; saved monitoring schedule removed.","route":"reply","files":[]})).after("resolved"),
    ]));
    final_result.open();
    let completed = await_replies(&host, run, 4);
    provider.assert_consumed();
    let (status, schedules) = server.request("GET", "/schedules", None);
    assert_eq!(status, 200);
    assert!(
        schedules["schedules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["graph"] != "codex-watch-worker")
    );
    assert!(
        sessions
            .pending_task_results("local", session_id)
            .unwrap()
            .is_empty()
    );
    let facts = platform.snapshot();
    assert_eq!(facts["replies"].as_array().unwrap().len(), 2);
    assert_eq!(facts["notifications"].as_array().unwrap().len(), 2);
    for (index, notice) in [RUNNING_NOTICE, DONE_NOTICE].into_iter().enumerate() {
        assert_eq!(
            markdown(&facts["notifications"][index])["body"]["elements"][0]["content"],
            notice
        );
    }
    let database = host
        .base
        .state_root()
        .join("channels/graphs")
        .join(&format!("{:x}", Sha256::digest(GRAPH.as_bytes()))[..16])
        .join("delivery.sqlite");
    let ledger =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let notifications: Vec<(String, String)> = ledger
        .prepare("SELECT request_id,status FROM deliveries WHERE kind='send'")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(notifications.len(), 2);
    assert!(
        notifications
            .iter()
            .all(|(id, status)| id.starts_with("channel-task-") && status == "confirmed")
    );
    let first_native = host.native_fact(run, "manager", 1);
    for invocation in 2..=4 {
        assert_eq!(
            first_native["session_id"],
            host.native_fact(run, "manager", invocation)["session_id"]
        );
    }
    let mut producer_runs = Vec::new();
    for (invocation, status) in [(1, "running"), (3, "completed")] {
        let input = &completed["results"]["wait_input"][invocation]["completion"]["output"];
        assert_eq!(input["source"], "task_result");
        assert_eq!(input["task_result"]["outcome"], "done");
        let producer = input["task_result"]["source_run"].as_str().unwrap();
        producer_runs.push(producer.to_owned());
        let child = host.record(producer);
        assert_eq!(child["input"], targets);
        let metadata = fixture::read_json(
            host.base
                .state_root()
                .join("run-metadata")
                .join(format!("{producer}.json")),
        );
        assert_eq!(metadata["task"]["session"], session_id);
        assert_eq!(metadata["task"]["source"]["kind"], "agent_delegation");
        assert_eq!(metadata["schedule"], schedule);
        let report: Value = serde_json::from_slice(&fixture::artifact_file(
            &host.base.artifact(&child, "watch"),
            "codex-watch.json",
        ))
        .unwrap();
        assert_eq!(
            report,
            json!({"observations":[{"project":native.project,"thread_id":thread,"status":status}],"issues":[]})
        );
        let result = &completed["results"]["reply"][invocation]["completion"]["output"];
        assert_eq!(result["task_result_handled"], true);
        assert_eq!(result["suppressed"], true);
    }
    assert_ne!(producer_runs[0], producer_runs[1]);
    for request in provider.requests() {
        assert!(!request.to_string().contains(TOKEN));
        assert!(!request.to_string().contains(APP_SECRET));
    }
    let (status, stopped) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{stopped}");
    server.wait_status(run, "stopped");
    let cleanup = native.call(
        "control",
        json!({"thread_id":thread,"action":"remove_worktree","force":true}),
    );
    assert_eq!(cleanup["action"], "worktree_removed");
    assert!(!cwd.exists());
    host.evidence_report(&provider, "tests/goose_feishu/codex.rs", json!({
        "actual_codex_app_server":true,"actual_local_codex_provider":true,
        "shipped_plugin_and_graphs":true,"real_scheduler_ticks":2,"running_and_completed_notifications":true,
        "initial_reply_before_completion":true,"followup_before_completion":true,"first_reply_ms":first_reply_ms,
        "same_manager_session_and_native_history":true,"schedule_removed":true,"main_checkout_clean":true,
        "real_feishu_requests":0,"real_manager_model_requests":0
    }), json!({"real_model_requests":null,"model_scope":"real Codex provider; deterministic Goose provider; loopback Feishu",
        "gateway_binary_sha256":goose::digest(&gateway),"platform":facts,"run":completed,
        "codex_outcome":{"thread_id":thread,"status":terminal["status"],"marker_sha256":format!("{:x}",Sha256::digest(MARKER.as_bytes()))}}));
}
