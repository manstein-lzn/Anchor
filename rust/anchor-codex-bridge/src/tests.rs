use crate::{BridgeConfig, RunningBridge, start_server};
use anchor_mcp_host::{McpHost, McpServerConfig, McpTransportConfig, Secret, ToolCallResult};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, Write},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use tempfile::TempDir;

const TOKEN: &str = "deterministic-bridge-http-test-token-32-plus";
const DEADLINE: Duration = Duration::from_secs(10);

struct Fixture {
    _temp: TempDir,
    projects: PathBuf,
    alpha: PathBuf,
    beta: PathBuf,
    outside: PathBuf,
    worktrees: PathBuf,
    bridge: RunningBridge,
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with(
            std::env::current_exe().unwrap(),
            [
                "--ignored",
                "--exact",
                "tests::fake_app_server",
                "--nocapture",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        )
        .await
    }

    async fn start_with(codex_bin: PathBuf, codex_args: Vec<String>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let projects = temp.path().join("projects");
        let alpha = projects.join("alpha");
        let beta = projects.join("beta");
        let outside = temp.path().join("outside");
        for repo in [&alpha, &beta, &outside] {
            std::fs::create_dir_all(repo).unwrap();
            git(repo, &["init", "--quiet", "--initial-branch=main"]);
            std::fs::write(repo.join("README.md"), "Committed fixture checkout.\n").unwrap();
            git(repo, &["add", "README.md"]);
            git(
                repo,
                &["commit", "--quiet", "--no-gpg-sign", "-m", "fixture"],
            );
        }
        let worktrees = temp.path().join("worktrees");
        let bridge = start_server(BridgeConfig {
            listen: "127.0.0.1:0".parse().unwrap(),
            bearer_token: TOKEN.into(),
            allow_roots: vec![projects.clone()],
            worktree_root: worktrees.clone(),
            codex_bin,
            codex_args,
        })
        .await
        .unwrap();
        Self {
            _temp: temp,
            projects,
            alpha,
            beta,
            outside,
            worktrees,
            bridge,
        }
    }

    fn connection(&self, token: Option<&str>, scope: Option<&Path>) -> McpServerConfig {
        let mut headers = BTreeMap::new();
        if let Some(scope) = scope {
            headers.insert(
                "x-codex-project-scope".into(),
                scope.to_str().unwrap().into(),
            );
        }
        McpServerConfig {
            server_id: "codex".into(),
            transport: McpTransportConfig::StreamableHttp {
                endpoint: self.bridge.endpoint(),
                bearer_token: token.map(Secret::new),
                headers,
            },
        }
    }

    async fn connect(&self, scope: Option<&Path>) -> McpHost {
        tokio::time::timeout(
            DEADLINE,
            McpHost::connect(self.connection(Some(TOKEN), scope)),
        )
        .await
        .expect("HTTP MCP initialization timed out")
        .expect("HTTP MCP initialization failed")
    }

    async fn shutdown(self, hosts: Vec<McpHost>) {
        for host in hosts {
            tokio::time::timeout(DEADLINE, host.shutdown())
                .await
                .unwrap()
                .unwrap();
        }
        tokio::time::timeout(DEADLINE, self.bridge.shutdown())
            .await
            .unwrap()
            .unwrap();
    }
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "user.name=Bridge Fixture",
            "-c",
            "user.email=bridge@example.invalid",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn call(host: &McpHost, tool: &str, args: Value) -> Value {
    let result = tokio::time::timeout(Duration::from_secs(40), host.call(tool, args))
        .await
        .unwrap_or_else(|_| panic!("{tool} timed out"))
        .unwrap_or_else(|error| panic!("{tool}: {error}"));
    assert!(!result.is_error, "{tool}: {result:?}");
    result_json(&result).unwrap_or_else(|| panic!("{tool} returned no JSON: {result:?}"))
}

fn result_json(result: &ToolCallResult) -> Option<Value> {
    result.structured_content.clone().or_else(|| {
        result.content.iter().find_map(|part| {
            part["text"]
                .as_str()
                .and_then(|text| serde_json::from_str(text).ok())
        })
    })
}

async fn rejected(host: &McpHost, tool: &str, args: Value, reason: &str) {
    let result = tokio::time::timeout(DEADLINE, host.call(tool, args))
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_error, "{tool} unexpectedly succeeded: {result:?}");
    assert!(
        format!("{result:?}").contains(reason),
        "{tool} missing {reason:?}: {result:?}"
    );
}

fn spawn_args(project: &Path, prompt: &str) -> Value {
    json!({"project": project, "prompt": prompt, "mode": "read-only",
        "approval_policy": "never", "wait_seconds": 0})
}

fn thread_id(value: &Value) -> &str {
    value["thread_id"]
        .as_str()
        .expect("thread_id must be a string")
}

async fn inspect(host: &McpHost, id: &str) -> Value {
    call(host, "inspect", json!({"thread_id": id})).await
}

async fn wait_status(host: &McpHost, id: &str, status: &str) -> Value {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let detail = inspect(host, id).await;
            if detail["status"] == status {
                return detail;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{id} never reached {status}"))
}

fn receipt(detail: &Value) -> Value {
    serde_json::from_str(detail["latest_agent_message"].as_str().unwrap()).unwrap()
}

fn board_ids(board: &Value) -> Vec<&str> {
    board["threads"]
        .as_array()
        .unwrap()
        .iter()
        .map(thread_id)
        .collect()
}

async fn interrupt(host: &McpHost, id: &str) {
    let result = call(
        host,
        "control",
        json!({"thread_id": id, "action": "interrupt"}),
    )
    .await;
    assert_eq!(result["action"], "interrupt_requested");
    wait_status(host, id, "interrupted").await;
}

#[tokio::test]
async fn http_authentication_and_raw_tool_inventory() {
    let fixture = Fixture::start().await;
    assert!(fixture.bridge.address.ip().is_loopback());
    assert_ne!(fixture.bridge.address.port(), 0);
    for token in [None, Some("incorrect-http-bearer-token-32-plus-characters")] {
        let result =
            tokio::time::timeout(DEADLINE, McpHost::connect(fixture.connection(token, None)))
                .await
                .unwrap();
        assert!(
            result.is_err(),
            "missing or incorrect bearer token was accepted"
        );
    }
    let host = fixture.connect(None).await;
    let mut names: Vec<_> = host.tools().into_iter().map(|tool| tool.name).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "board",
            "btw",
            "control",
            "history",
            "inspect",
            "models",
            "native_call",
            "native_discover",
            "native_events",
            "native_respond",
            "projects",
            "session",
            "spawn",
            "steer_or_resume"
        ]
    );
    let projects = call(&host, "projects", json!({})).await;
    assert_eq!(projects["roots"], json!([fixture.projects]));
    assert_eq!(projects["projects"].as_array().unwrap().len(), 2);
    fixture.shutdown(vec![host]).await;
}

#[tokio::test]
async fn inline_completion_keeps_native_setup_and_streamed_projection() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    let mut args = spawn_args(&fixture.alpha, "fast");
    args["title"] = json!("Native named task");
    args["mode"] = json!("workspace-write");
    args["network_access"] = json!(true);
    let spawned = call(&host, "spawn", args).await;
    assert_eq!(
        spawned["status"], "completed",
        "inline completion must precede the RPC reply"
    );
    assert!(spawned["active_turn_id"].is_null());
    let detail = inspect(&host, thread_id(&spawned)).await;
    assert_eq!(detail["title"], "Native named task");
    assert_eq!(detail["plan"][0]["step"], "Inspect checkout");
    assert_eq!(detail["latest_command_output"], "fixture command output");
    assert_eq!(detail["token_usage"]["total"]["totalTokens"], 7);
    let receipt: Value =
        serde_json::from_str(detail["latest_agent_message"].as_str().unwrap()).unwrap();
    assert_eq!(receipt["thread_start"]["cwd"], json!(fixture.alpha));
    assert_eq!(receipt["thread_start"]["sandbox"], "workspace-write");
    assert_eq!(receipt["thread_start"]["approvalPolicy"], "never");
    assert_eq!(receipt["thread_start"]["approvalsReviewer"], "user");
    assert!(receipt["thread_start"].get("config").is_none());
    assert_eq!(receipt["title"], "Native named task");
    assert_eq!(receipt["turn_start"]["approvalPolicy"], "never");
    assert_eq!(receipt["turn_start"]["approvalsReviewer"], "user");
    assert_eq!(
        receipt["turn_start"]["sandboxPolicy"]["type"],
        "workspaceWrite"
    );
    assert_eq!(
        receipt["turn_start"]["sandboxPolicy"]["networkAccess"],
        true
    );
    assert_eq!(
        receipt["turn_start"]["sandboxPolicy"]["writableRoots"],
        json!([fixture.alpha])
    );
    assert_eq!(receipt["turn_start"]["input"][0]["text"], "fast");
    let board = call(&host, "board", json!({})).await;
    assert_eq!(board["threads"][0]["thread_id"], spawned["thread_id"]);
    assert_eq!(board["threads"][0]["status"], "completed");
    assert!(board["history_next_cursor"].is_null());
    fixture.shutdown(vec![host]).await;
}

#[tokio::test]
async fn btw_preserves_running_parent_and_forwards_model_and_effort() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(Some(&fixture.alpha)).await;
    let outside = fixture.connect(Some(&fixture.beta)).await;
    let mut args = spawn_args(&fixture.alpha, "slow");
    args["model"] = json!("gpt-6.1-sol");
    args["reasoning_effort"] = json!("max");
    let parent = call(&host, "spawn", args.clone()).await;
    assert_eq!(parent["model"], "gpt-6.1-sol");
    assert_eq!(parent["reasoning_effort"], "max");
    args["reasoning_effort"] = json!(" ");
    rejected(&host, "spawn", args, "nonempty").await;
    let question = json!({"thread_id": thread_id(&parent), "prompt": "What is the current plan?", "wait_seconds": 0});
    rejected(&outside, "btw", question.clone(), "scope").await;
    let side = call(&host, "btw", question).await;
    assert_eq!(side["parent_thread_id"], parent["thread_id"]);
    assert_eq!(side["status"], "completed");
    assert_eq!(side["model"], "gpt-6.1-sol");
    assert_eq!(side["reasoning_effort"], "max");
    let receipt = receipt(&side);
    assert_eq!(receipt["fork"]["config"]["model_reasoning_effort"], "max");
    assert_eq!(receipt["turn_start"]["sandboxPolicy"]["type"], "readOnly");
    assert_eq!(
        receipt["turn_start"]["sandboxPolicy"]["networkAccess"],
        false
    );
    assert_eq!(receipt["turn_start"]["approvalPolicy"], "never");
    let continued = call(
        &host,
        "steer_or_resume",
        json!({"thread_id": thread_id(&side), "prompt": "fast", "wait_seconds": 0}),
    )
    .await;
    assert_eq!(continued["status"], "completed");
    call(
        &host,
        "control",
        json!({"thread_id": thread_id(&side), "action": "close"}),
    )
    .await;
    let after = inspect(&host, thread_id(&parent)).await;
    assert_eq!(after["status"], "running");
    assert_eq!(after["active_turn_id"], parent["active_turn_id"]);
    interrupt(&host, thread_id(&parent)).await;
    fixture.shutdown(vec![host, outside]).await;
}

#[tokio::test]
async fn scoped_session_lifecycle_does_not_start_work_until_requested() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(Some(&fixture.alpha)).await;
    let other = fixture.connect(Some(&fixture.beta)).await;
    call(&host, "spawn", spawn_args(&fixture.alpha, "seed-history")).await;
    let models = call(&host, "models", json!({})).await;
    assert_eq!(
        models["data"][0]["supportedReasoningEfforts"][0]["reasoningEffort"],
        "max"
    );
    let id = "history-alpha";
    let before = inspect(&host, id).await;
    let turns = call(&host, "history", json!({"thread_id":id})).await;
    assert_eq!(turns["data"][0]["id"], before["last_turn_id"]);
    let items = call(
        &host,
        "history",
        json!({"thread_id":id,"view":"items","turn_id":before["last_turn_id"]}),
    )
    .await;
    assert_eq!(items["data"][0]["item"]["type"], "agentMessage");
    rejected(&other, "history", json!({"thread_id":id}), "scope").await;
    rejected(
        &other,
        "session",
        json!({"thread_id":id,"action":"load"}),
        "scope",
    )
    .await;
    rejected(
        &host,
        "session",
        json!({"thread_id":id,"action":"load","model":"ignored"}),
        "do not apply",
    )
    .await;
    let side = call(
        &host,
        "btw",
        json!({"thread_id":id,"prompt":"What was done?","wait_seconds":0}),
    )
    .await;
    assert_eq!(side["status"], "completed");
    let after = inspect(&host, id).await;
    assert_eq!(after["loaded"], false);
    assert_eq!(after["last_turn_id"], before["last_turn_id"]);
    call(
        &host,
        "control",
        json!({"thread_id":side["thread_id"],"action":"close"}),
    )
    .await;
    let loaded = call(&host, "session", json!({"thread_id":id,"action":"load"})).await;
    assert_eq!(loaded["loaded"], true);
    assert!(loaded["active_turn_id"].is_null());
    assert_eq!(loaded["last_turn_id"], before["last_turn_id"]);
    let configured = call(&host, "session", json!({"thread_id":id,"action":"configure","model":"fixture-model","reasoning_effort":"max"})).await;
    assert_eq!(configured["model"], "fixture-model");
    assert_eq!(configured["reasoning_effort"], "max");
    call(
        &host,
        "session",
        json!({"thread_id":id,"action":"rename","title":"Project research"}),
    )
    .await;
    let fork = call(
        &host,
        "session",
        json!({"thread_id":id,"action":"fork","worktree":"next-task","title":"Implementation"}),
    )
    .await;
    assert_eq!(fork["status"], "idle");
    assert!(fork["active_turn_id"].is_null());
    assert_eq!(fork["source_thread_id"], id);
    assert_eq!(fork["worktree"], "next-task");
    assert_ne!(fork["cwd"], before["cwd"]);
    let compact = call(
        &host,
        "session",
        json!({"thread_id":fork["thread_id"],"action":"compact"}),
    )
    .await;
    assert_eq!(compact["action"], "compaction_requested");
    let unloaded = call(&host, "session", json!({"thread_id":id,"action":"unload"})).await;
    assert_eq!(unloaded["subscribed"], false);
    call(&host, "control", json!({"thread_id":id,"action":"archive"})).await;
    call(
        &host,
        "session",
        json!({"thread_id":id,"action":"unarchive"}),
    )
    .await;
    let resumed = call(
        &host,
        "steer_or_resume",
        json!({"thread_id":id,"prompt":"slow","wait_seconds":0}),
    )
    .await;
    assert_eq!(resumed["status"], "running");
    assert_eq!(resumed["model"], "fixture-model");
    rejected(
        &host,
        "session",
        json!({"thread_id":id,"action":"unload"}),
        "busy",
    )
    .await;
    interrupt(&host, id).await;
    fixture.shutdown(vec![host, other]).await;
}

#[tokio::test]
async fn project_scope_fences_discovery_threads_and_managed_worktrees() {
    let fixture = Fixture::start().await;
    let all = fixture.connect(None).await;
    let alpha = fixture.connect(Some(Path::new("alpha"))).await;
    let beta = fixture.connect(Some(&fixture.beta)).await;
    let mut alpha_threads = Vec::new();
    let mut beta_threads = Vec::new();
    for (project, threads) in [
        (&fixture.alpha, &mut alpha_threads),
        (&fixture.beta, &mut beta_threads),
    ] {
        for worktree in [None, Some("scoped")] {
            let mut args = spawn_args(project, "fast");
            if let Some(slug) = worktree {
                args["worktree"] = json!(slug);
                args["base_ref"] = json!("main");
            }
            threads.push(call(&all, "spawn", args).await);
        }
    }
    let projects = call(&alpha, "projects", json!({})).await;
    assert_eq!(projects["projects"].as_array().unwrap().len(), 1);
    assert_eq!(projects["projects"][0]["path"], json!(fixture.alpha));
    assert_eq!(
        projects["projects"][0]["managed_worktrees"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    for (host, visible, hidden) in [
        (&alpha, &alpha_threads, &beta_threads),
        (&beta, &beta_threads, &alpha_threads),
    ] {
        let board = call(host, "board", json!({})).await;
        assert_eq!(board_ids(&board).len(), 2);
        for thread in visible {
            assert!(board_ids(&board).contains(&thread_id(thread)));
            assert_eq!(
                inspect(host, thread_id(thread)).await["status"],
                "completed"
            );
        }
        for thread in hidden {
            let id = thread_id(thread);
            rejected(host, "inspect", json!({"thread_id": id}), "scope").await;
            rejected(
                host,
                "steer_or_resume",
                json!({"thread_id": id, "prompt": "fast", "wait_seconds": 0}),
                "scope",
            )
            .await;
            rejected(
                host,
                "control",
                json!({"thread_id": id, "action": "remove_worktree", "force": true}),
                "scope",
            )
            .await;
            rejected(
                host,
                "spawn",
                spawn_args(Path::new(thread["cwd"].as_str().unwrap()), "fast"),
                "scope",
            )
            .await;
        }
    }
    assert_ne!(alpha_threads[1]["cwd"], beta_threads[1]["cwd"]);
    assert!(Path::new(alpha_threads[1]["cwd"].as_str().unwrap()).starts_with(&fixture.worktrees));
    let resumed = call(
        &alpha,
        "steer_or_resume",
        json!({
            "thread_id": thread_id(&alpha_threads[1]), "prompt": "fast", "wait_seconds": 0
        }),
    )
    .await;
    assert_eq!(resumed["status"], "completed");
    assert_eq!(board_ids(&call(&all, "board", json!({})).await).len(), 4);
    rejected(&all, "spawn", spawn_args(&fixture.outside, "fast"), "scope").await;
    let invalid_scope = fixture.connect(Some(&fixture.outside)).await;
    rejected(&invalid_scope, "projects", json!({}), "allow-root").await;
    let worktree_scope = fixture.connect(Some(&fixture.worktrees)).await;
    rejected(&worktree_scope, "board", json!({}), "allow-root").await;
    fixture
        .shutdown(vec![all, alpha, beta, invalid_scope, worktree_scope])
        .await;
}

#[tokio::test]
async fn waiting_spawn_does_not_serialize_parallel_worktree_admission() {
    let fixture = Fixture::start().await;
    let waiter = fixture.connect(None).await;
    let observer = fixture.connect(None).await;
    let mut first_args = spawn_args(&fixture.alpha, "slow");
    first_args["worktree"] = json!("first");
    first_args["base_ref"] = json!("main");
    first_args["mode"] = json!("workspace-write");
    first_args["wait_seconds"] = json!(30);
    {
        let first = call(&waiter, "spawn", first_args);
        tokio::pin!(first);
        // Observe admission, rather than guessing when the long wait began with a sleep.
        let progress = tokio::time::timeout(DEADLINE, async {
            let first_id = loop {
                let board = call(&observer, "board", json!({})).await;
                if let Some(thread) = board["threads"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|t| t["worktree"] == "first" && t["status"] == "running")
                {
                    break thread_id(thread).to_owned();
                }
                tokio::task::yield_now().await;
            };
            let mut args = spawn_args(&fixture.alpha, "slow");
            args["worktree"] = json!("second");
            args["mode"] = json!("workspace-write");
            let second = call(&observer, "spawn", args).await;
            assert_eq!(second["status"], "running");
            let board = call(&observer, "board", json!({})).await;
            assert_eq!(
                board["threads"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|t| t["status"] == "running")
                    .count(),
                2
            );
            (first_id, second)
        });
        let (first_id, second) = tokio::select! {
            result = &mut first => panic!("slow spawn returned before being steered: {result}"),
            result = progress => result.expect("one worker's wait blocked independent admission"),
        };
        let steered = call(
            &observer,
            "steer_or_resume",
            json!({
                "thread_id": first_id, "prompt": "finish first", "wait_seconds": 0
            }),
        )
        .await;
        assert_eq!(steered["action"], "steered");
        let completed = tokio::time::timeout(DEADLINE, &mut first).await.unwrap();
        assert_eq!(completed["status"], "completed");
        assert_ne!(completed["cwd"], second["cwd"]);
        let first_receipt = receipt(&inspect(&observer, &first_id).await);
        assert_eq!(
            first_receipt["steer"]["expectedTurnId"],
            completed["last_turn_id"]
        );
        assert_eq!(first_receipt["steer"]["input"][0]["text"], "finish first");
        assert_eq!(
            first_receipt["turn_start"]["sandboxPolicy"]["writableRoots"],
            json!([completed["cwd"], fixture.alpha.join(".git")])
        );
        interrupt(&observer, thread_id(&second)).await;
    }
    fixture.shutdown(vec![waiter, observer]).await;
}

#[tokio::test]
async fn same_cwd_writer_is_rejected_while_readers_can_share_workspace() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    let mut args = spawn_args(&fixture.alpha, "slow");
    args["mode"] = json!("workspace-write");
    let writer = call(&host, "spawn", args.clone()).await;
    assert_eq!(writer["status"], "running");
    assert_eq!(
        anchor_file_lease::ProjectLease::acquire(&fixture.alpha)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(
        anchor_file_lease::ProjectLease::acquire(&fixture.projects)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    let unrelated = anchor_file_lease::ProjectLease::acquire(&fixture.beta).unwrap();
    rejected(&host, "spawn", args.clone(), "writer").await;
    let reader = call(&host, "spawn", spawn_args(&fixture.alpha, "slow")).await;
    let another = call(&host, "spawn", spawn_args(&fixture.alpha, "slow")).await;
    let board = call(&host, "board", json!({})).await;
    assert_eq!(
        board_ids(&board).len(),
        3,
        "rejected writer must not create a native thread"
    );
    for thread in [&writer, &reader, &another] {
        assert_eq!(thread["cwd"], json!(fixture.alpha));
        assert_eq!(thread["status"], "running");
    }
    interrupt(&host, thread_id(&writer)).await;
    let direct = anchor_file_lease::ProjectLease::acquire(&fixture.alpha).unwrap();
    rejected(&host, "spawn", args.clone(), "writer lease").await;
    rejected(
        &host,
        "steer_or_resume",
        json!({"thread_id": thread_id(&writer), "prompt": "fast", "wait_seconds": 0}),
        "writer lease",
    )
    .await;
    drop(direct);
    drop(unrelated);
    args["prompt"] = json!("fast");
    assert_eq!(call(&host, "spawn", args).await["status"], "completed");
    interrupt(&host, thread_id(&reader)).await;
    interrupt(&host, thread_id(&another)).await;
    fixture.shutdown(vec![host]).await;
}

#[tokio::test]
async fn native_writer_entrypoints_share_host_lease_until_terminal() {
    let fixture = Fixture::start().await;
    let manager = crate::manager::Manager::new(BridgeConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        bearer_token: TOKEN.into(),
        allow_roots: vec![fixture.projects.clone()],
        worktree_root: fixture.worktrees.clone(),
        codex_bin: std::env::current_exe().unwrap(),
        codex_args: [
            "--ignored",
            "--exact",
            "tests::fake_app_server",
            "--nocapture",
        ]
        .map(str::to_owned)
        .to_vec(),
    });
    manager.install_writer_fixture();
    let direct = anchor_file_lease::ProjectLease::acquire(&fixture.alpha).unwrap();
    let create =
        json!({"method":"thread/start","params":{"cwd":fixture.alpha,"sandbox":"workspace-write"}});
    assert!(
        manager
            .dispatch("native_call", create.clone(), None)
            .await
            .unwrap_err()
            .contains("writer lease")
    );
    drop(direct);
    let created = manager.dispatch("native_call", create, None).await.unwrap();
    let id = created["thread"]["id"].as_str().unwrap();
    let direct = anchor_file_lease::ProjectLease::acquire(&fixture.alpha).unwrap();
    let start = json!({"method":"turn/start","params":{"threadId":id,"input":[{"type":"text","text":"slow"}]}});
    assert!(
        manager
            .dispatch("native_call", start.clone(), None)
            .await
            .unwrap_err()
            .contains("writer lease")
    );
    drop(direct);
    manager.dispatch("native_call", start, None).await.unwrap();
    assert_eq!(
        anchor_file_lease::ProjectLease::acquire(&fixture.alpha)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::WouldBlock
    );
    manager
        .dispatch(
            "control",
            json!({"thread_id":id,"action":"interrupt"}),
            None,
        )
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, async {
        loop {
            if let Ok(guard) = anchor_file_lease::ProjectLease::acquire(&fixture.alpha) {
                break guard;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    manager
        .dispatch("session", json!({"thread_id":id,"action":"unload"}), None)
        .await
        .unwrap();
    let direct = anchor_file_lease::ProjectLease::acquire(&fixture.alpha).unwrap();
    let resume = json!({"method":"thread/resume","params":{"threadId":id}});
    assert!(
        manager
            .dispatch("native_call", resume.clone(), None)
            .await
            .unwrap_err()
            .contains("writer lease")
    );
    drop(direct);
    manager.dispatch("native_call", resume, None).await.unwrap();
    anchor_file_lease::ProjectLease::acquire(&fixture.alpha).unwrap();
    manager.shutdown().await;
    fixture.shutdown(vec![]).await;
}

#[tokio::test]
async fn pending_approval_and_user_question_require_explicit_correlated_answers() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    for (prompt, status, method, answer, native_answer) in [
        (
            "approval",
            "waiting_approval",
            "item/commandExecution/requestApproval",
            json!({"approval_decision": "accept"}),
            json!({"decision": "accept"}),
        ),
        (
            "question",
            "waiting_input",
            "item/tool/requestUserInput",
            json!({"answers": {"choice": ["Proceed"]}}),
            json!({"answers": {"choice": {"answers": ["Proceed"]}}}),
        ),
    ] {
        let mut args = spawn_args(&fixture.alpha, prompt);
        args["approval_policy"] = json!("on-request");
        args["mode"] = json!("workspace-write");
        let spawned = call(&host, "spawn", args).await;
        assert_eq!(spawned["status"], status);
        assert_eq!(
            anchor_file_lease::ProjectLease::acquire(&fixture.alpha)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        let id = thread_id(&spawned);
        let detail = inspect(&host, id).await;
        let pending = detail["pending_requests"].as_array().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["method"], method);
        assert_eq!(pending[0]["params"]["threadId"], id);
        let key = pending[0]["request_key"].as_str().unwrap();
        let board = call(&host, "board", json!({})).await;
        let entry = board["threads"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| thread_id(t) == id)
            .unwrap();
        assert_eq!(entry["status"], status);
        assert_eq!(entry["pending_requests"][0]["request_key"], key);
        rejected(
            &host,
            "steer_or_resume",
            json!({"thread_id": id, "prompt": "finish", "wait_seconds": 0}),
            "pending",
        )
        .await;
        let mut response = answer;
        response["thread_id"] = json!(id);
        response["wait_seconds"] = json!(2);
        rejected(&host, "steer_or_resume", response.clone(), "request_key").await;
        response["request_key"] = json!("not-the-pending-request");
        rejected(
            &host,
            "steer_or_resume",
            response.clone(),
            "pending request not found",
        )
        .await;
        assert_eq!(inspect(&host, id).await["status"], status);
        response["request_key"] = json!(key);
        let answered = call(&host, "steer_or_resume", response.clone()).await;
        assert_eq!(answered["action"], "answered");
        let completed = wait_status(&host, id, "completed").await;
        assert_eq!(completed["pending_requests"], json!([]));
        assert_eq!(receipt(&completed)["answer"], native_answer);
        assert_eq!(
            receipt(&completed)["turn_start"]["approvalPolicy"],
            "on-request"
        );
        rejected(
            &host,
            "steer_or_resume",
            response,
            "pending request not found",
        )
        .await;
    }
    fixture.shutdown(vec![host]).await;
}

#[tokio::test]
async fn native_history_is_scoped_paginated_inspected_resumed_and_archived() {
    let fixture = Fixture::start().await;
    let all = fixture.connect(None).await;
    let scoped = fixture.connect(Some(&fixture.alpha)).await;
    call(&all, "spawn", spawn_args(&fixture.alpha, "seed-history")).await;
    // Inspect before board: discovery must use native thread/read, not a cached projection.
    let history = inspect(&scoped, "history-alpha").await;
    assert_eq!(history["loaded"], false);
    assert_eq!(history["title"], "Stored alpha task");
    assert_eq!(history["cwd"], json!(fixture.alpha));
    assert_eq!(history["mode"], "read-only");
    assert_eq!(history["status"], "completed");
    assert_eq!(
        history["latest_agent_message"],
        "stored answer for history-alpha"
    );
    assert_eq!(history["latest_command_output"], "stored command output");
    let first = call(&scoped, "board", json!({})).await;
    assert_eq!(first["history_next_cursor"], "fixture-next");
    assert!(board_ids(&first).contains(&"history-alpha"));
    assert!(!board_ids(&first).contains(&"history-alpha-next"));
    assert!(!board_ids(&first).contains(&"history-beta"));
    assert!(!board_ids(&first).contains(&"history-outside"));
    let second = call(
        &scoped,
        "board",
        json!({"cursor": first["history_next_cursor"]}),
    )
    .await;
    assert!(second["history_next_cursor"].is_null());
    assert!(board_ids(&second).contains(&"history-alpha-next"));
    rejected(
        &scoped,
        "inspect",
        json!({"thread_id": "history-beta"}),
        "scope",
    )
    .await;
    rejected(
        &all,
        "inspect",
        json!({"thread_id": "history-outside"}),
        "scope",
    )
    .await;
    let broad = call(&all, "board", json!({})).await;
    assert!(board_ids(&broad).contains(&"history-beta"));
    assert!(!board_ids(&broad).contains(&"history-outside"));
    let resumed = call(
        &scoped,
        "steer_or_resume",
        json!({
            "thread_id": "history-alpha", "prompt": "fast", "wait_seconds": 0
        }),
    )
    .await;
    assert_eq!(resumed["thread_id"], "history-alpha");
    assert_eq!(resumed["action"], "resumed");
    assert_eq!(resumed["status"], "completed");
    assert_ne!(resumed["last_turn_id"], history["last_turn_id"]);
    let detail = inspect(&scoped, "history-alpha").await;
    assert_eq!(detail["loaded"], true);
    let receipt = receipt(&detail);
    assert_eq!(
        receipt["resume"],
        json!({"threadId": "history-alpha", "sandbox": "read-only",
            "cwd": fixture.alpha, "approvalPolicy": "on-request", "approvalsReviewer": "user"})
    );
    assert_eq!(
        receipt["turn_start"]["sandboxPolicy"],
        json!({"type": "readOnly", "networkAccess": false})
    );
    assert_eq!(
        call(
            &scoped,
            "control",
            json!({"thread_id": "history-alpha", "action": "archive"})
        )
        .await["action"],
        "archived"
    );
    assert!(!board_ids(&call(&scoped, "board", json!({})).await).contains(&"history-alpha"));
    let archived = call(&scoped, "board", json!({"archived": true})).await;
    assert_eq!(board_ids(&archived), ["history-alpha"]);
    rejected(
        &scoped,
        "steer_or_resume",
        json!({"thread_id": "history-alpha", "prompt": "fast", "wait_seconds": 0}),
        "archived",
    )
    .await;
    fixture.shutdown(vec![all, scoped]).await;
}

#[tokio::test]
async fn worktree_removal_requires_idle_clean_checkout_or_explicit_force() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    let mut args = spawn_args(&fixture.alpha, "fast");
    args["worktree"] = json!("clean");
    args["base_ref"] = json!("main");
    let clean = call(&host, "spawn", args.clone()).await;
    let clean_path = PathBuf::from(clean["cwd"].as_str().unwrap());
    let direct = anchor_file_lease::ProjectLease::acquire(&clean_path).unwrap();
    rejected(
        &host,
        "control",
        json!({"thread_id": thread_id(&clean), "action": "remove_worktree", "force": true}),
        "writer lease",
    )
    .await;
    assert!(clean_path.exists());
    drop(direct);
    assert_eq!(
        git(&clean_path, &["rev-parse", "HEAD"]),
        git(&fixture.alpha, &["rev-parse", "main"])
    );
    assert_eq!(
        inspect(&host, thread_id(&clean)).await["git"]["clean"],
        true
    );
    let removed = call(
        &host,
        "control",
        json!({"thread_id": thread_id(&clean), "action": "remove_worktree"}),
    )
    .await;
    assert_eq!(removed["action"], "worktree_removed");
    assert_eq!(removed["path"], json!(clean_path));
    assert_eq!(removed["branch_retained"], true);
    assert!(!clean_path.exists());
    git(
        &fixture.alpha,
        &["show-ref", "--verify", "refs/heads/codex/clean"],
    );
    args["worktree"] = json!("dirty");
    args["prompt"] = json!("slow");
    args["mode"] = json!("workspace-write");
    let dirty = call(&host, "spawn", args).await;
    let dirty_path = PathBuf::from(dirty["cwd"].as_str().unwrap());
    let mut removal =
        json!({"thread_id": thread_id(&dirty), "action": "remove_worktree", "force": true});
    rejected(&host, "control", removal.clone(), "active sessions").await;
    std::fs::write(dirty_path.join("README.md"), "Changed only in worktree.\n").unwrap();
    std::fs::write(dirty_path.join("untracked.txt"), "keep until force").unwrap();
    interrupt(&host, thread_id(&dirty)).await;
    let detail = inspect(&host, thread_id(&dirty)).await;
    assert_eq!(detail["git"]["clean"], false);
    assert!(
        detail["git"]["status_short"]
            .as_str()
            .unwrap()
            .contains("untracked.txt")
    );
    assert!(
        detail["git"]["diff_stat"]
            .as_str()
            .unwrap()
            .contains("README.md")
    );
    removal["force"] = json!(false);
    rejected(&host, "control", removal.clone(), "uncommitted").await;
    assert_eq!(
        std::fs::read_to_string(dirty_path.join("untracked.txt")).unwrap(),
        "keep until force"
    );
    removal["force"] = json!(true);
    assert_eq!(
        call(&host, "control", removal).await["branch_retained"],
        true
    );
    assert!(!dirty_path.exists());
    git(
        &fixture.alpha,
        &["show-ref", "--verify", "refs/heads/codex/dirty"],
    );
    let worktrees = git(&fixture.alpha, &["worktree", "list", "--porcelain"]);
    assert!(!worktrees.contains(dirty_path.to_str().unwrap()));
    assert!(!worktrees.contains(clean_path.to_str().unwrap()));
    assert_eq!(
        std::fs::read_to_string(fixture.alpha.join("README.md")).unwrap(),
        "Committed fixture checkout.\n"
    );
    fixture.shutdown(vec![host]).await;
}

#[tokio::test]
async fn removal_through_child_history_checks_the_entire_managed_worktree() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    let mut args = spawn_args(&fixture.alpha, "seed-worktree-history");
    args["worktree"] = json!("child-history");
    let created = call(&host, "spawn", args).await;
    let checkout = PathBuf::from(created["cwd"].as_str().unwrap());
    std::fs::create_dir(checkout.join("nested")).unwrap();
    let history = inspect(&host, "history-worktree-child").await;
    assert_eq!(history["cwd"], json!(checkout.join("nested")));
    assert_eq!(history["worktree"], "child-history");
    let mut args = spawn_args(&checkout, "slow");
    args["mode"] = json!("workspace-write");
    let running = call(&host, "spawn", args).await;
    assert_eq!(running["status"], "running");
    let removal = host
        .call(
            "control",
            json!({
                "thread_id": "history-worktree-child", "action": "remove_worktree", "force": true
            }),
        )
        .await
        .unwrap();
    let still_exists = checkout.is_dir();
    fixture.shutdown(vec![host]).await;
    assert!(
        removal.is_error,
        "removal through a descendant deleted an active worker's root: {removal:?}"
    );
    assert!(
        still_exists,
        "active worker checkout must survive rejected removal"
    );
}

#[tokio::test]
async fn uncertain_turn_admission_does_not_release_the_writer_reservation() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    for action in ["steer", "interrupt"] {
        let mut args = spawn_args(&fixture.alpha, "uncertain-admission");
        args["mode"] = json!("workspace-write");
        args["worktree"] = json!("uncertain-admission");
        rejected(&host, "spawn", args, "admission uncertain").await;
        let board = call(&host, "board", json!({})).await;
        let unknown: Vec<_> = board["threads"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|thread| thread["status"] == "unknown")
            .collect();
        assert_eq!(unknown.len(), 1, "uncertain admission must remain fenced");
        let id = thread_id(unknown[0]);
        assert!(unknown[0]["active_turn_id"].is_null());
        let checkout = Path::new(unknown[0]["cwd"].as_str().unwrap());
        assert_eq!(
            anchor_file_lease::ProjectLease::acquire(checkout)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::WouldBlock
        );
        let mut replacement = spawn_args(checkout, "fast");
        replacement["mode"] = json!("workspace-write");
        rejected(&host, "spawn", replacement, "writer is already active").await;
        rejected(
            &host,
            "steer_or_resume",
            json!({"thread_id": id, "prompt": "fast", "wait_seconds": 0}),
            "inspect before",
        )
        .await;
        rejected(
            &host,
            "control",
            json!({"thread_id": id, "action": "archive"}),
            "interrupt",
        )
        .await;
        rejected(
            &host,
            "control",
            json!({"thread_id": id, "action": "remove_worktree", "force": true}),
            "active sessions",
        )
        .await;
        assert!(checkout.exists());

        // Only thread/read + turns/list can identify this accepted native turn.
        let reconciled = inspect(&host, id).await;
        assert_eq!(reconciled["status"], "running");
        assert_eq!(reconciled["loaded"], true);
        assert!(reconciled["active_turn_id"].is_string());
        assert_eq!(reconciled["active_turn_id"], reconciled["last_turn_id"]);
        if action == "steer" {
            let steered = call(
                &host,
                "steer_or_resume",
                json!({"thread_id": id, "prompt": "finish reconciled turn", "wait_seconds": 1}),
            )
            .await;
            assert_eq!(steered["action"], "steered");
            let completed = wait_status(&host, id, "completed").await;
            assert_eq!(
                receipt(&completed)["steer"]["expectedTurnId"],
                reconciled["active_turn_id"]
            );
        } else {
            interrupt(&host, id).await;
        }
        anchor_file_lease::ProjectLease::acquire(checkout).unwrap();
    }
    fixture.shutdown(vec![host]).await;
}

#[tokio::test]
async fn stale_unknown_inspection_cannot_release_a_new_uncertain_writer() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    let mut args = spawn_args(&fixture.alpha, "uncertain-unloaded");
    args["mode"] = json!("workspace-write");
    rejected(&host, "spawn", args, "admission uncertain").await;
    let board = call(&host, "board", json!({})).await;
    let id = thread_id(&board["threads"][0]).to_owned();
    std::fs::write(fixture.alpha.join(".fixture-hold-reconcile"), "hold").unwrap();
    let stale;
    {
        let inspection = inspect(&host, &id);
        tokio::pin!(inspection);
        let barrier = tokio::time::timeout(DEADLINE, async {
            while !fixture.alpha.join(".fixture-reconcile-pending").exists() {
                tokio::task::yield_now().await;
            }
        });
        tokio::select! {
            early = &mut inspection => panic!("inspection bypassed native barrier: {early}"),
            result = barrier => result.expect("native reconciliation never reached turns/list"),
        }
        // A second observation resolves the old admission before a new turn starts.
        assert_eq!(inspect(&host, &id).await["status"], "completed");
        rejected(
            &host,
            "steer_or_resume",
            json!({"thread_id": id, "prompt": "uncertain-admission", "wait_seconds": 0}),
            "no turn id",
        )
        .await;
        // The fake releases the old read only after this new admission is unknown.
        call(&host, "board", json!({})).await;
        stale = tokio::time::timeout(DEADLINE, &mut inspection)
            .await
            .unwrap();
    }
    let board = call(&host, "board", json!({})).await;
    let mut replacement = spawn_args(&fixture.alpha, "fast");
    replacement["mode"] = json!("workspace-write");
    let competing = host.call("spawn", replacement).await.unwrap();
    fixture.shutdown(vec![host]).await;
    assert_eq!(
        board["threads"][0]["status"], "unknown",
        "old inspection erased the new uncertain admission: {stale}"
    );
    assert!(
        competing.is_error,
        "stale reconciliation admitted a competing writer"
    );
}

#[tokio::test]
async fn concurrent_history_hydration_returns_the_resumed_live_turn() {
    let fixture = Fixture::start().await;
    let host = fixture.connect(None).await;
    call(
        &host,
        "spawn",
        spawn_args(&fixture.alpha, "seed-hydration-race"),
    )
    .await;
    let detail;
    {
        let hydration = inspect(&host, "history-hydration");
        tokio::pin!(hydration);
        let barrier = tokio::time::timeout(DEADLINE, async {
            while !fixture.alpha.join(".fixture-items-pending").exists() {
                tokio::task::yield_now().await;
            }
        });
        tokio::select! {
            early = &mut hydration => panic!("history read bypassed native barrier: {early}"),
            result = barrier => result.expect("native items/list was never requested"),
        }
        let resumed = call(
            &host,
            "steer_or_resume",
            json!({
                "thread_id": "history-hydration", "prompt": "slow", "wait_seconds": 0
            }),
        )
        .await;
        assert_eq!(resumed["status"], "running");
        detail = tokio::time::timeout(DEADLINE, &mut hydration)
            .await
            .unwrap();
        assert_eq!(
            detail["last_turn_id"], resumed["last_turn_id"],
            "inspect returned the old stored turn after native resume"
        );
    }
    let current = inspect(&host, "history-hydration").await;
    interrupt(&host, "history-hydration").await;
    fixture.shutdown(vec![host]).await;
    assert_eq!(detail["loaded"], true);
    assert_eq!(detail["status"], "running");
    assert_eq!(detail["active_turn_id"], current["active_turn_id"]);
}

#[tokio::test]
async fn cached_history_revalidates_scope_after_native_cwd_changes() {
    let fixture = Fixture::start().await;
    let all = fixture.connect(None).await;
    let scoped = fixture.connect(Some(&fixture.alpha)).await;
    call(&all, "spawn", spawn_args(&fixture.alpha, "seed-history")).await;
    assert_eq!(inspect(&scoped, "history-alpha").await["loaded"], false);
    // Simulate a separate native CLI resuming this stored thread in another project.
    call(&all, "spawn", spawn_args(&fixture.beta, "relocate-history")).await;
    let board = call(&scoped, "board", json!({})).await;
    let detail = scoped
        .call("inspect", json!({"thread_id": "history-alpha"}))
        .await
        .unwrap();
    fixture.shutdown(vec![all, scoped]).await;
    assert!(
        detail.is_error,
        "cached cwd authorized reading history now owned by another scope: {detail:?}"
    );
    assert!(
        !board_ids(&board).contains(&"history-alpha"),
        "board retained out-of-scope native history"
    );
}

// This test intentionally uses the operator's existing Codex provider configuration.
// Never include native responses/history in its assertion messages.
#[tokio::test]
#[ignore = "requires explicitly authorized local Codex provider access"]
async fn live_codex_readonly_smoke() {
    const MARKER: &str = "ANCHOR_BRIDGE_SMOKE_OK";
    let fixture =
        Fixture::start_with("codex".into(), vec!["app-server".into(), "--stdio".into()]).await;
    std::fs::write(fixture.alpha.join("smoke-marker.txt"), MARKER).unwrap();
    git(&fixture.alpha, &["add", "smoke-marker.txt"]);
    git(
        &fixture.alpha,
        &["commit", "--quiet", "--no-gpg-sign", "-m", "smoke marker"],
    );
    let host = fixture.connect(Some(&fixture.alpha)).await;
    let mut active_thread = None;
    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        let projects = live_call(&host, "projects", json!({})).await?;
        if projects["projects"].as_array().map(Vec::len) != Some(1) {
            return Err("live project scope did not contain exactly the fixture repository");
        }
        let board = live_call(&host, "board", json!({})).await?;
        if !board["threads"].is_array() || board.get("history_next_cursor").is_none() || !board["warning"].is_null() {
            return Err("live board/history query failed (details withheld)");
        }
        let spawned = live_call(&host, "spawn", spawn_args(&fixture.alpha,
            "Read only smoke-marker.txt in the current directory. Reply with exactly its contents, no explanation or formatting. Do not modify files, access the network, or read any other file.")).await?;
        let id = spawned["thread_id"].as_str().ok_or("live spawn returned no thread id")?.to_owned();
        active_thread = Some(id.clone());
        loop {
            let detail = live_call(&host, "inspect", json!({"thread_id": id})).await?;
            match detail["status"].as_str() {
                Some("completed") => {
                    if detail["latest_agent_message"].as_str().map(str::trim) != Some(MARKER) {
                        return Err("live marker reply did not match (response withheld)");
                    }
                    if detail["git"]["clean"] != true {
                        return Err("read-only live task left checkout changes");
                    }
                    return Ok(());
                }
                Some("running") | Some("idle") => tokio::time::sleep(Duration::from_millis(250)).await,
                _ => return Err("live task failed or requires intervention (details withheld)"),
            }
        }
    }).await;
    if !matches!(outcome, Ok(Ok(())))
        && let Some(id) = active_thread
    {
        let _ = tokio::time::timeout(
            DEADLINE,
            live_call(
                &host,
                "control",
                json!({
                    "thread_id": id, "action": "interrupt"
                }),
            ),
        )
        .await;
    }
    fixture.shutdown(vec![host]).await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => panic!("{reason}"),
        Err(_) => {
            panic!("live Codex smoke exceeded 120 seconds; interrupt attempted before shutdown")
        }
    }
}

async fn live_call(host: &McpHost, tool: &str, args: Value) -> Result<Value, &'static str> {
    let result = host
        .call(tool, args)
        .await
        .map_err(|_| "live MCP transport failed (details withheld)")?;
    if result.is_error {
        return Err("live tool failed (details withheld)");
    }
    result_json(&result).ok_or("live tool returned no JSON (response withheld)")
}

#[tokio::test]
#[ignore = "requires explicitly authorized local Codex provider access"]
async fn live_codex_btw_and_reasoning_effort() {
    let fixture =
        Fixture::start_with("codex".into(), vec!["app-server".into(), "--stdio".into()]).await;
    let host = fixture.connect(Some(&fixture.alpha)).await;
    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        let mut args = spawn_args(&fixture.alpha,
            "Remember this side-chat marker: ANCHOR_BTW_CONTEXT_OK. Run exactly `printf 'MAIN_WAITING\\n'; sleep 300`, then reply MAIN_DONE. Do not modify files, access the network, or do any other work.");
        args["model"] = json!("gpt-6.1-sol");
        args["reasoning_effort"] = json!("max");
        let parent = live_call(&host, "spawn", args).await?;
        let id = parent["thread_id"].as_str().ok_or("no parent id")?;
        if parent["model"] != "gpt-6.1-sol" || parent["reasoning_effort"] != "max" {
            return Err("native model/effort did not match requested settings");
        }
        loop {
            let detail = live_call(&host, "inspect", json!({"thread_id": id})).await?;
            if detail["recent_activities"].as_array().is_some_and(|items| items.iter().any(|item| item["kind"] == "command" && item["status"] == "in_progress")) { break; }
            if detail["status"] != "running" { return Err("main task stopped before side question"); }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        println!("main command started; asking native side question");
        let mut side = live_call(&host, "btw", json!({"thread_id": id,
            "prompt": "What side-chat marker did the original user ask you to remember? Reply with that token only, using inherited context, without tools.", "wait_seconds": 0})).await?;
        while side["status"] == "running" {
            tokio::time::sleep(Duration::from_millis(250)).await;
            side = live_call(&host, "inspect", json!({"thread_id": side["thread_id"]})).await?;
        }
        if side["status"] != "completed" || side["latest_agent_message"].as_str().map(str::trim) != Some("ANCHOR_BTW_CONTEXT_OK") {
            return Err("native side answer failed inherited-context check (response withheld)");
        }
        let after = live_call(&host, "inspect", json!({"thread_id": id})).await?;
        if after["status"] != "running" || after["active_turn_id"] != parent["active_turn_id"] {
            return Err("main task did not remain active in the original turn");
        }
        live_call(&host, "control", json!({"thread_id": side["thread_id"], "action": "close"})).await?;
        live_call(&host, "control", json!({"thread_id": id, "action": "interrupt"})).await?;
        println!("{}", json!({"status":"passed", "parent_thread_id":id, "side_thread_id":side["thread_id"],
            "model":parent["model"], "reasoning_effort":parent["reasoning_effort"], "parent_kept_running":true, "inherited_context_verified":true}));
        Ok::<(), &'static str>(())
    }).await;
    fixture.shutdown(vec![host]).await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => panic!("{reason}"),
        Err(_) => panic!("live side-chat probe timed out; isolated Bridge was shut down"),
    }
}

#[tokio::test]
#[ignore = "requires explicitly authorized local Codex provider access"]
async fn live_codex_project_session_lifecycle() {
    let fixture = Fixture::start_with(
        "codex".into(),
        vec![
            "-c".into(),
            "thread_unload_delay_secs=0".into(),
            "app-server".into(),
            "--stdio".into(),
        ],
    )
    .await;
    let host = fixture.connect(Some(&fixture.alpha)).await;
    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        let mut args = spawn_args(&fixture.alpha, "Remember the token ANCHOR_HISTORY_CONTEXT_OK. Reply READY only. Do not use tools.");
        args["model"] = json!("gpt-6.1-sol");
        args["reasoning_effort"] = json!("max");
        args["wait_seconds"] = json!(30);
        println!("native: seed persisted context");
        let parent = call(&host, "spawn", args).await;
        let id = thread_id(&parent);
        let before = wait_status(&host, id, "completed").await;
        call(&host, "session", json!({"thread_id":id,"action":"unload"})).await;
        tokio::time::timeout(DEADLINE, async {
            while inspect(&host, id).await["loaded"] != false {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }).await.expect("native session did not unload");
        println!("native: old history btw without parent resume");
        let side = call(&host, "btw", json!({"thread_id":id,"prompt":"What token did the original user ask you to remember? Reply with that token only, without tools.","wait_seconds":30})).await;
        let answer = wait_status(&host, thread_id(&side), "completed").await;
        assert_eq!(answer["latest_agent_message"].as_str().map(str::trim), Some("ANCHOR_HISTORY_CONTEXT_OK"));
        let after = inspect(&host, id).await;
        assert_eq!(after["last_turn_id"], before["last_turn_id"]);
        assert_eq!(after["subscribed"], false);
        assert_eq!(after["loaded"], false);
        call(&host, "control", json!({"thread_id":side["thread_id"],"action":"close"})).await;
        println!("native: load, configure and fork without work");
        let loaded = call(&host, "session", json!({"thread_id":id,"action":"load"})).await;
        assert!(loaded["active_turn_id"].is_null());
        assert_eq!(loaded["last_turn_id"], before["last_turn_id"]);
        call(&host, "session", json!({"thread_id":id,"action":"configure","model":"gpt-6.1-sol","reasoning_effort":"max"})).await;
        let settings = inspect(&host, id).await;
        assert_eq!(settings["model"], "gpt-6.1-sol");
        assert_eq!(settings["reasoning_effort"], "max");
        let fork = call(&host, "session", json!({"thread_id":id,"action":"fork","title":"Disposable lifecycle fork","worktree":"lifecycle"})).await;
        assert!(fork["active_turn_id"].is_null());
        assert_eq!(fork["status"], "idle");
        let history = call(&host, "history", json!({"thread_id":id,"view":"items"})).await;
        assert!(!history["data"].as_array().unwrap().is_empty());
        let models = call(&host, "models", json!({})).await;
        assert!(!models["data"].as_array().unwrap().is_empty());
        call(&host, "control", json!({"thread_id":fork["thread_id"],"action":"archive"})).await;
        call(&host, "session", json!({"thread_id":fork["thread_id"],"action":"unarchive"})).await;
        call(&host, "control", json!({"thread_id":fork["thread_id"],"action":"archive"})).await;
        call(&host, "control", json!({"thread_id":id,"action":"archive"})).await;
        println!("{}", json!({"status":"passed","main_thread_id":id,"side_thread_id":side["thread_id"],"fork_thread_id":fork["thread_id"],"historical_context_verified":true,"main_turn_unchanged":true}));
    }).await;
    fixture.shutdown(vec![host]).await;
    assert!(
        outcome.is_ok(),
        "native lifecycle exceeded 120 seconds; isolated Bridge shut down"
    );
}

#[tokio::test]
#[ignore = "requires explicitly authorized local Codex provider access and disposable worktree writes"]
async fn live_codex_worktree_write_smoke() {
    const FILE: &str = "anchor-write-smoke.txt";
    const CONTENT: &str = "ANCHOR_WORKTREE_WRITE_OK\n";
    let fixture =
        Fixture::start_with("codex".into(), vec!["app-server".into(), "--stdio".into()]).await;
    let host = fixture.connect(Some(&fixture.alpha)).await;
    let original_head = git(&fixture.alpha, &["rev-parse", "HEAD"]);
    let mut active_thread = None;
    let mut checkout = None;
    let mut terminal = false;
    let outcome = tokio::time::timeout(Duration::from_secs(120), async {
        let mut args = spawn_args(&fixture.alpha,
            "In the current worktree only, create anchor-write-smoke.txt containing exactly ANCHOR_WORKTREE_WRITE_OK followed by one newline. Do not modify any other file. Do not commit, push, access the network, or change configuration. Reply with exactly WRITTEN after creating the file.");
        args["worktree"] = json!("marker-task");
        args["mode"] = json!("workspace-write");
        let spawned = live_call(&host, "spawn", args).await?;
        let id = spawned["thread_id"].as_str().ok_or("live write returned no thread id")?.to_owned();
        active_thread = Some(id.clone());
        let cwd = PathBuf::from(spawned["cwd"].as_str().ok_or("live write returned no cwd")?);
        if !cwd.starts_with(&fixture.worktrees) || cwd == fixture.alpha {
            return Err("live write was not isolated in the managed worktree");
        }
        checkout = Some(cwd.clone());
        loop {
            let detail = live_call(&host, "inspect", json!({"thread_id": id})).await?;
            match detail["status"].as_str() {
                Some("completed" | "failed" | "interrupted") => {
                    terminal = true;
                    if detail["status"] != "completed" {
                        return Err("live write did not complete (response withheld)");
                    }
                    if std::fs::read(cwd.join(FILE)).ok().as_deref() != Some(CONTENT.as_bytes()) {
                        return Err("live worktree marker bytes did not match (contents withheld)");
                    }
                    if fixture.alpha.join(FILE).exists() {
                        return Err("live worktree marker leaked into main checkout");
                    }
                    if detail["git"]["clean"] != false
                        || !detail["git"]["status_short"].as_str().is_some_and(|s| s.contains(FILE))
                    {
                        return Err("live inspect did not report the uncommitted worktree marker");
                    }
                    if git(&cwd, &["rev-parse", "HEAD"]) != original_head
                        || git(&fixture.alpha, &["rev-parse", "HEAD"]) != original_head
                        || !git(&fixture.alpha, &["status", "--porcelain"]).is_empty()
                    {
                        return Err("live write changed committed history or the main checkout");
                    }
                    return Ok(());
                }
                Some("running" | "idle") => tokio::time::sleep(Duration::from_millis(250)).await,
                _ => return Err("live write requires intervention (details withheld)"),
            }
        }
    }).await;
    if !matches!(outcome, Ok(Ok(())))
        && !terminal
        && let Some(id) = active_thread.as_deref()
    {
        terminal = tokio::time::timeout(DEADLINE, async {
            live_call(
                &host,
                "control",
                json!({"thread_id": id, "action": "interrupt"}),
            )
            .await?;
            loop {
                let detail = live_call(&host, "inspect", json!({"thread_id": id})).await?;
                if matches!(
                    detail["status"].as_str(),
                    Some("completed" | "failed" | "interrupted")
                ) {
                    return Ok::<_, &'static str>(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .is_ok_and(|result| result.is_ok());
    }
    let removed = if terminal && let Some(id) = active_thread {
        tokio::time::timeout(
            DEADLINE,
            live_call(
                &host,
                "control",
                json!({
                    "thread_id": id, "action": "remove_worktree", "force": true
                }),
            ),
        )
        .await
        .is_ok_and(|r| r.is_ok_and(|v| v["action"] == "worktree_removed"))
    } else {
        false
    };
    let checkout_removed = checkout.as_ref().is_some_and(|path| !path.exists());
    fixture.shutdown(vec![host]).await;
    match outcome {
        Ok(Ok(())) => {
            assert!(
                removed && checkout_removed,
                "terminal live worktree was not removed through control"
            );
        }
        Ok(Err(reason)) => panic!("{reason}; terminal={terminal}, control_cleanup={removed}"),
        Err(_) => panic!(
            "live write exceeded 120 seconds; interrupt attempted, terminal={terminal}, control_cleanup={removed}"
        ),
    }
}

struct FakeThread {
    metadata: Value,
    loaded: bool,
    archived: bool,
    turn: Value,
    receipt: Value,
}

impl FakeThread {
    fn new(id: &str, cwd: &Path, loaded: bool) -> Self {
        Self {
            metadata: json!({"id": id, "cwd": cwd, "source": "appServer", "name": null, "createdAt": 1700000000,
                "updatedAt": 1700000001, "preview": "fixture history",
                "status": {"type": if loaded { "idle" } else { "notLoaded" }}}),
            loaded,
            archived: false,
            turn: json!({"id": format!("stored-{id}"), "status": "completed", "error": null}),
            receipt: json!({}),
        }
    }
}

fn send(output: &mut impl Write, frame: Value) {
    writeln!(output, "{frame}").unwrap();
    output.flush().unwrap();
}

fn notify(output: &mut impl Write, method: &str, params: Value) {
    send(
        output,
        json!({"jsonrpc": "2.0", "method": method, "params": params}),
    );
}

fn finish(output: &mut impl Write, thread: &mut FakeThread, status: &str) {
    thread.metadata["status"] = json!({"type": "idle"});
    let id = &thread.metadata["id"];
    let turn = &thread.turn["id"];
    let text = thread.receipt.to_string();
    notify(
        output,
        "item/agentMessage/delta",
        json!({
            "threadId": id, "turnId": turn, "itemId": "message", "delta": text
        }),
    );
    notify(
        output,
        "item/completed",
        json!({
            "threadId": id, "turnId": turn,
            "item": {"id": "message", "type": "agentMessage", "text": text}
        }),
    );
    thread.turn["status"] = json!(status);
    notify(
        output,
        "turn/completed",
        json!({"threadId": id, "turn": thread.turn}),
    );
}

fn seed_history(threads: &mut BTreeMap<String, FakeThread>, alpha: &Path) {
    let projects = alpha.parent().unwrap();
    for (id, cwd, title) in [
        ("history-alpha", alpha.to_path_buf(), "Stored alpha task"),
        (
            "history-alpha-next",
            alpha.to_path_buf(),
            "Stored next page",
        ),
        ("history-beta", projects.join("beta"), "Stored beta task"),
        (
            "history-outside",
            projects.parent().unwrap().join("outside"),
            "Outside grant",
        ),
    ] {
        let mut thread = FakeThread::new(id, &cwd, false);
        thread.metadata["name"] = json!(title);
        threads.insert(id.into(), thread);
    }
}

#[test]
#[ignore = "stdio subprocess fixture; launched only by the HTTP E2E tests"]
fn fake_app_server() {
    let input = std::io::stdin();
    let mut output = std::io::stdout().lock();
    // A fresh line separates frames from the Rust harness's test-name prefix.
    writeln!(output, "\nfixture app-server ready").unwrap();
    output.flush().unwrap();
    let mut threads = BTreeMap::<String, FakeThread>::new();
    let mut pending = BTreeMap::<String, (String, Value)>::new();
    let mut held_items = None;
    let mut held_turns = None;
    let mut initialized = false;
    let mut next_thread = 0;
    let mut next_turn = 0;
    for line in input.lock().lines() {
        let message: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = message["id"].clone();
        let params = &message["params"];
        let Some(method) = message["method"].as_str() else {
            let (thread_id, expected) = pending
                .remove(id.as_str().unwrap())
                .expect("response must match a pending request");
            assert_eq!(message["result"], expected);
            let thread = threads.get_mut(&thread_id).unwrap();
            thread.receipt["answer"] = message["result"].clone();
            notify(
                &mut output,
                "serverRequest/resolved",
                json!({"threadId": thread_id, "requestId": id}),
            );
            finish(&mut output, thread, "completed");
            continue;
        };
        if method == "initialized" {
            initialized = true;
            continue;
        }
        assert!(
            method == "initialize" || initialized,
            "native initialization must precede calls"
        );
        let result = match method {
            "initialize" => {
                assert_eq!(params["clientInfo"]["name"], "anchor-codex-bridge");
                assert_eq!(params["capabilities"]["experimentalApi"], true);
                json!({"userAgent": "anchor-rust-fixture"})
            }
            "thread/start" => {
                next_thread += 1;
                let thread_id = format!("fixture-{next_thread}");
                let mut thread =
                    FakeThread::new(&thread_id, Path::new(params["cwd"].as_str().unwrap()), true);
                thread.receipt["thread_start"] = params.clone();
                notify(
                    &mut output,
                    "thread/started",
                    json!({"thread": thread.metadata}),
                );
                let result = json!({"thread": thread.metadata, "model": params["model"],
                    "reasoningEffort": params["config"]["model_reasoning_effort"]});
                threads.insert(thread_id, thread);
                result
            }
            "thread/fork" => {
                let parent = threads.get(params["threadId"].as_str().unwrap()).unwrap();
                assert_eq!(params["sandbox"], "read-only");
                if params["ephemeral"] != true {
                    assert_eq!(params["deferGoalContinuation"], true);
                }
                let source_model = parent.metadata["model"].clone();
                next_thread += 1;
                let thread_id = format!("fixture-{next_thread}");
                let mut thread =
                    FakeThread::new(&thread_id, Path::new(params["cwd"].as_str().unwrap()), true);
                thread.receipt["fork"] = params.clone();
                thread.metadata["model"] = params.get("model").cloned().unwrap_or(source_model);
                thread.metadata["reasoningEffort"] =
                    params["config"]["model_reasoning_effort"].clone();
                let result = json!({"thread": thread.metadata, "model": params["model"],
                    "reasoningEffort": params["config"]["model_reasoning_effort"]});
                threads.insert(thread_id, thread);
                result
            }
            "thread/unsubscribe" => {
                let thread_id = params["threadId"].as_str().unwrap();
                let thread = threads.get_mut(thread_id).unwrap();
                thread.loaded = false;
                thread.metadata["status"] = json!({"type": "notLoaded"});
                if thread.receipt["fork"]["ephemeral"] == true {
                    threads.remove(thread_id);
                }
                json!({"status": "unsubscribed"})
            }
            "model/list" => {
                json!({"data":[{"id":"fixture-model","model":"fixture-model","supportedReasoningEfforts":[{"reasoningEffort":"max"}]}],"nextCursor":null})
            }
            "thread/settings/update" => {
                let thread = threads
                    .get_mut(params["threadId"].as_str().unwrap())
                    .unwrap();
                assert!(thread.loaded);
                if let Some(model) = params.get("model") {
                    thread.metadata["model"] = model.clone();
                }
                if let Some(effort) = params.get("effort") {
                    thread.metadata["reasoningEffort"] = effort.clone();
                }
                notify(
                    &mut output,
                    "thread/settings/updated",
                    json!({"threadId":params["threadId"],"threadSettings":{"model":thread.metadata["model"],"effort":thread.metadata["reasoningEffort"]}}),
                );
                json!({})
            }
            "thread/unarchive" => {
                let thread = threads
                    .get_mut(params["threadId"].as_str().unwrap())
                    .unwrap();
                thread.archived = false;
                json!({"thread":thread.metadata})
            }
            "thread/compact/start" => {
                let thread = threads
                    .get_mut(params["threadId"].as_str().unwrap())
                    .unwrap();
                next_turn += 1;
                thread.turn = json!({"id":format!("compact-{next_turn}"),"status":"inProgress"});
                notify(
                    &mut output,
                    "turn/started",
                    json!({"threadId":params["threadId"],"turn":thread.turn}),
                );
                finish(&mut output, thread, "completed");
                json!({})
            }
            "thread/name/set" => {
                let thread = threads
                    .get_mut(params["threadId"].as_str().unwrap())
                    .unwrap();
                thread.metadata["name"] = params["name"].clone();
                thread.receipt["title"] = params["name"].clone();
                notify(
                    &mut output,
                    "thread/name/updated",
                    json!({"threadId": params["threadId"], "threadName": params["name"]}),
                );
                json!({})
            }
            "turn/start" => {
                let thread_id = params["threadId"].as_str().unwrap();
                let prompt = params["input"][0]["text"].as_str().unwrap();
                if prompt == "seed-history" {
                    seed_history(&mut threads, Path::new(params["cwd"].as_str().unwrap()));
                }
                if prompt == "seed-hydration-race" {
                    threads.insert(
                        "history-hydration".into(),
                        FakeThread::new(
                            "history-hydration",
                            Path::new(params["cwd"].as_str().unwrap()),
                            false,
                        ),
                    );
                }
                if prompt == "relocate-history" {
                    threads.get_mut("history-alpha").unwrap().metadata["cwd"] =
                        params["cwd"].clone();
                }
                if prompt == "seed-worktree-history" {
                    threads.insert(
                        "history-worktree-child".into(),
                        FakeThread::new(
                            "history-worktree-child",
                            &Path::new(params["cwd"].as_str().unwrap()).join("nested"),
                            false,
                        ),
                    );
                }
                let thread = threads.get_mut(thread_id).unwrap();
                assert!(
                    thread.loaded,
                    "history must be natively resumed before turn/start"
                );
                assert!(!thread.archived);
                assert_eq!(params["cwd"], thread.metadata["cwd"]);
                next_turn += 1;
                let turn_id = format!("turn-{next_turn}");
                thread.turn =
                    json!({"id": turn_id, "status": "inProgress", "items": [], "error": null});
                thread.metadata["status"] = json!({"type": "active", "activeFlags": []});
                thread.receipt["turn_start"] = params.clone();
                let result = json!({"turn": thread.turn});
                if matches!(prompt, "uncertain-admission" | "uncertain-unloaded") {
                    // Native admission happened, but the reply cannot identify its turn.
                    // No completion/start notification has reached the bridge yet.
                    if prompt == "uncertain-unloaded" {
                        thread.turn["status"] = json!("completed");
                        thread.metadata["status"] = json!({"type": "notLoaded"});
                        thread.loaded = false;
                    }
                    send(
                        &mut output,
                        json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    );
                    continue;
                }
                notify(
                    &mut output,
                    "turn/started",
                    json!({"threadId": thread_id, "turn": thread.turn}),
                );
                notify(
                    &mut output,
                    "turn/plan/updated",
                    json!({"threadId": thread_id, "turnId": turn_id,
                    "explanation": "Deterministic fixture", "plan": [{"step": "Inspect checkout", "status": "in_progress"}]}),
                );
                notify(
                    &mut output,
                    "item/commandExecution/outputDelta",
                    json!({
                    "threadId": thread_id, "turnId": turn_id, "itemId": "command", "delta": "fixture command output"}),
                );
                notify(
                    &mut output,
                    "thread/tokenUsage/updated",
                    json!({
                    "threadId": thread_id, "tokenUsage": {"total": {"totalTokens": 7}}}),
                );
                if thread_id == "history-hydration"
                    && let Some(frame) = held_items.take()
                {
                    send(&mut output, frame);
                }
                match prompt {
                    "slow" => {}
                    "approval" | "question" => {
                        assert_eq!(params["approvalPolicy"], "on-request");
                        let key = format!("{prompt}-{turn_id}");
                        let (method, details, expected) = if prompt == "approval" {
                            (
                                "item/commandExecution/requestApproval",
                                json!({"command": "echo fixture", "reason": "Explicit decision required"}),
                                json!({"decision": "accept"}),
                            )
                        } else {
                            (
                                "item/tool/requestUserInput",
                                json!({"questions": [{"id": "choice",
                                "header": "Fixture", "question": "Continue?", "options": [{"label": "Proceed", "description": "Finish fixture"}]}]}),
                                json!({"answers": {"choice": {"answers": ["Proceed"]}}}),
                            )
                        };
                        let mut request = details;
                        request["threadId"] = json!(thread_id);
                        request["turnId"] = json!(turn_id);
                        request["itemId"] = json!("pending-item");
                        pending.insert(key.clone(), (thread_id.into(), expected));
                        send(
                            &mut output,
                            json!({"jsonrpc": "2.0", "id": key, "method": method, "params": request}),
                        );
                    }
                    "fast"
                    | "seed-history"
                    | "seed-worktree-history"
                    | "seed-hydration-race"
                    | "relocate-history" => finish(&mut output, thread, "completed"),
                    other if other.starts_with("Side conversation boundary.") => {
                        finish(&mut output, thread, "completed")
                    }
                    other => panic!("unexpected fixture prompt: {other}"),
                }
                // Deliberately stale inProgress reply arrives after inline completion.
                result
            }
            "turn/steer" | "turn/interrupt" => {
                let thread = threads
                    .get_mut(params["threadId"].as_str().unwrap())
                    .unwrap();
                assert_eq!(thread.turn["status"], "inProgress");
                let turn_id = thread.turn["id"].clone();
                if method == "turn/steer" {
                    assert_eq!(params["expectedTurnId"], turn_id);
                    thread.receipt["steer"] = params.clone();
                    finish(&mut output, thread, "completed");
                } else {
                    assert_eq!(params["turnId"], turn_id);
                    pending.retain(|_, (id, _)| id != params["threadId"].as_str().unwrap());
                    finish(&mut output, thread, "interrupted");
                }
                json!({"turnId": turn_id})
            }
            "thread/read" => {
                let thread = threads.get(params["threadId"].as_str().unwrap()).unwrap();
                json!({"thread": thread.metadata})
            }
            "thread/list" => {
                if let Some((thread_id, frame)) = held_turns.take() {
                    let thread: &FakeThread = threads.get(&thread_id).unwrap();
                    if thread.loaded && thread.turn["status"] == "inProgress" {
                        send(&mut output, frame);
                    } else {
                        held_turns = Some((thread_id, frame));
                    }
                }
                let archived = params["archived"].as_bool().unwrap_or(false);
                let next_page = !params["cursor"].is_null();
                if next_page {
                    assert_eq!(params["cursor"], "fixture-next");
                }
                let data: Vec<_> = threads
                    .iter()
                    .filter(|(id, t)| {
                        t.archived == archived
                            && (next_page == (id.as_str() == "history-alpha-next"))
                    })
                    .map(|(_, t)| t.metadata.clone())
                    .collect();
                let cursor =
                    if !next_page && !archived && threads.contains_key("history-alpha-next") {
                        json!("fixture-next")
                    } else {
                        Value::Null
                    };
                json!({"data": data, "nextCursor": cursor})
            }
            "thread/turns/list" => {
                let thread_id = params["threadId"].as_str().unwrap();
                let thread = threads.get(thread_id).unwrap();
                let result = json!({"data": [thread.turn], "nextCursor": null});
                let cwd = Path::new(thread.metadata["cwd"].as_str().unwrap());
                let pending = cwd.join(".fixture-reconcile-pending");
                if cwd.join(".fixture-hold-reconcile").exists() && !pending.exists() {
                    held_turns = Some((
                        thread_id.to_owned(),
                        json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    ));
                    std::fs::write(pending, "ready").unwrap();
                    continue;
                }
                result
            }
            "thread/items/list" => {
                let thread_id = params["threadId"].as_str().unwrap();
                let thread = threads.get(thread_id).unwrap();
                if !params["turnId"].is_null() {
                    assert_eq!(params["turnId"], thread.turn["id"]);
                }
                let result = json!({"data": [
                    {"item": {"id": "stored-message", "type": "agentMessage", "text": format!("stored answer for {thread_id}")}},
                    {"item": {"id": "stored-command", "type": "commandExecution", "aggregatedOutput": "stored command output"}}
                ], "nextCursor": null});
                if thread_id == "history-hydration" && !thread.loaded {
                    held_items = Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
                    std::fs::write(
                        Path::new(thread.metadata["cwd"].as_str().unwrap())
                            .join(".fixture-items-pending"),
                        "ready",
                    )
                    .unwrap();
                    continue;
                }
                result
            }
            "thread/goal/get" => json!({"goal":null}),
            "thread/queue/list" => json!({"data":[],"nextCursor":null}),
            "thread/resume" => {
                let thread = threads
                    .get_mut(params["threadId"].as_str().unwrap())
                    .unwrap();
                assert!(!thread.loaded);
                thread.loaded = true;
                thread.metadata["status"] = json!({"type":"idle"});
                thread.receipt["resume"] = params.clone();
                json!({"thread": thread.metadata, "model":thread.metadata["model"], "reasoningEffort":thread.metadata["reasoningEffort"]})
            }
            "thread/archive" => {
                let thread = threads
                    .get_mut(params["threadId"].as_str().unwrap())
                    .unwrap();
                thread.archived = true;
                notify(
                    &mut output,
                    "thread/archived",
                    json!({"threadId": params["threadId"]}),
                );
                json!({})
            }
            other => panic!("unexpected native method: {other}"),
        };
        send(
            &mut output,
            json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }
}

mod native;
