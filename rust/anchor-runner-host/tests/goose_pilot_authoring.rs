#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Gate, Host, Provider, Step};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

fn graph() -> Value {
    json!({"entry":"seed","ops":{"seed":{"run":"true"}},"nodes":[{"id":"seed","op":"seed"}],"edges":[]})
}

#[test]
#[ignore = "requires pinned Goose binary and Bubblewrap"]
fn pilot_reconciles_native_extensions_before_new_and_loaded_session_prompts() {
    let provider = Provider::new(
        "pilot-native-extension-authorization",
        vec![
            Step::text("legacy-extension-history-marker"),
            Step::tool(
                "anchor_run",
                json!({"command":["sh","-c","printf authorized-anchor-command"]}),
            )
            .expect_request_contains("legacy-extension-history-marker"),
            Step::text("Continued through the authorized Anchor command.")
                .after("authorized-anchor-command"),
            Step::tool("platform_status", json!({})),
            Step::text("A fresh Session also uses only authorized tools."),
        ],
    );
    // Seed a retained native Session with an explicitly enabled developer
    // extension. The next deployment withdraws that opt-in through Goose's API,
    // preserving the Session and its actual history instead of rewriting SQLite.
    let mut host = Host::new(&graph())
        .default_runtime()
        .with_extra_environment([("ANCHOR_GOOSE_ENABLED_EXTENSIONS", r#"["tom","developer"]"#)]);
    let first = {
        let server = host.serve(&provider);
        create(&server, "retained-extensions");
        let turn = submit(
            &server,
            "retained-extensions",
            "seed",
            "Remember this Turn.",
        );
        wait(&server, "retained-extensions", &turn, "completed")
    };
    host = host.with_extra_environment([("ANCHOR_GOOSE_ENABLED_EXTENSIONS", r#"["tom"]"#)]);
    let server = host.serve(&provider);
    let continued = submit(
        &server,
        "retained-extensions",
        "continue",
        "Continue the same Session through an Anchor command.",
    );
    let continued = wait(&server, "retained-extensions", &continued, "completed");
    assert_eq!(first["goose"], continued["goose"]);
    create(&server, "fresh-extensions");
    let fresh = submit(
        &server,
        "fresh-extensions",
        "start",
        "Inspect the platform.",
    );
    wait(&server, "fresh-extensions", &fresh, "completed");
    let requests = provider.requests();
    let is_developer_tool = |tool: &Value| {
        let name = tool["function"]["name"].as_str().unwrap_or_default();
        matches!(name, "shell" | "write" | "edit" | "read" | "tree")
            || name.starts_with("developer__")
    };
    assert!(
        requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(is_developer_tool)
    );
    for request in &requests[1..] {
        let tools = request["tools"].as_array().unwrap();
        assert!(
            !tools.iter().any(is_developer_tool),
            "developer tools remained enabled"
        );
        assert!(goose::tool_definition(request, "anchor_run").is_ok());
        assert!(request.to_string().contains("节点边界"));
    }
    evidence(
        &host,
        &provider,
        json!({"retained_native_session":continued["goose"],"native_history_preserved":true,
            "unauthorized_developer_tools_absent_from_new_and_loaded_prompts":true,
            "authorized_anchor_command_executed":true}),
    );
}

fn create(server: &goose::HttpHost, session: &str) {
    let (status, result) = server.request("POST", "/sessions", Some(&json!({"id":session})));
    assert_eq!(status, 201, "{result}");
}

fn submit(server: &goose::HttpHost, session: &str, request: &str, prompt: &str) -> String {
    let (status, result) = server.request(
        "POST",
        &format!("/sessions/{session}/turns"),
        Some(&json!({"request_id":request,"message":prompt})),
    );
    assert_eq!(status, 202, "{result}");
    result["turn"]["id"].as_str().unwrap().into()
}

fn wait(server: &goose::HttpHost, session: &str, turn: &str, expected: &str) -> Value {
    let mut result = Value::Null;
    goose::wait_until("Pilot authoring Turn", || {
        let (status, saved) =
            server.request("GET", &format!("/sessions/{session}/turns/{turn}"), None);
        assert_eq!(status, 200, "{saved}");
        result = saved["turn"].clone();
        assert!(
            result["status"] == "running" || result["status"] == expected,
            "{result}"
        );
        result["status"] == expected
    });
    result
}

fn evidence(host: &Host, provider: &Provider, checks: Value) {
    host.evidence_report(
        provider,
        "tests/goose_pilot_authoring.rs",
        checks,
        json!({}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary and Bubblewrap"]
fn anchor_dev_http_mount_discloses_selected_guidance_without_changing_execution_facts() {
    let provider = Provider::new(
        "pilot-anchor-dev-guidance",
        vec![
            Step::tool("plugin_list", json!({"plugin":"anchor-steward"})),
            Step::tool("anchor_plugins", json!({"plugin":"anchor-steward"}))
                .after("Anchor Dev"),
            Step::tool("platform_status", json!({})).after("next_offset"),
            goose::command("printf retained-project > project-marker.txt"),
            Step::text("Installed guidance is not mounted; existing commands still work."),
            Step::tool("anchor_plugins", json!({"plugin":"anchor-steward"}))
                .expect_request_contains("Anchor Dev"),
            Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"anchor-steward","section":"instructions"}),
            )
            .after("Anchor Dev"),
            Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"anchor-steward","section":"skills"}),
            )
            .after("稳定 Plugin ID"),
            Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"anchor-steward","section":"skill","path":"skills/graph-authoring/SKILL.md"}),
            )
            .after("graph-authoring"),
            Step::text("Read only the selected Graph guidance.").after("files: []"),
            Step::tool("anchor_plugins", json!({"plugin":"anchor-steward"})),
            Step::tool(
                "anchor_plugin_read",
                json!({"plugin":"anchor-steward","section":"instructions"}),
            )
            .after("next_offset"),
            Step::tool("platform_status", json!({})),
            Step::tool("anchor_run", json!({"command":["cat","project-marker.txt"]})),
            Step::text("Guidance is off; the workspace and runtime remain unchanged.")
                .after("retained-project"),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/anchor-steward")
        .canonicalize()
        .unwrap();
    anchor_library::Library::new(host.base.root.path())
        .install_directory("anchor-steward", &source, false)
        .unwrap();
    let instructions = fs::read_to_string(source.join("instructions.md")).unwrap();
    let selected = fs::read_to_string(source.join("skills/graph-authoring/SKILL.md")).unwrap();
    let unselected = fs::read_to_string(source.join("skills/python-development/SKILL.md")).unwrap();
    // Seed an existing Host-managed runtime fact without preparing or downloading.
    let runtime_path = host.base.state_root().join("managed/runtime-config.json");
    fs::create_dir_all(runtime_path.parent().unwrap()).unwrap();
    fs::write(
        &runtime_path,
        json!({
            "format":1,"python":null,"python_enabled":false,"sources":null,"rust":null
        })
        .to_string(),
    )
    .unwrap();
    let runtime_before = fs::read(&runtime_path).unwrap();
    let server = host.serve(&provider);
    create(&server, "anchor-dev");
    let first = submit(
        &server,
        "anchor-dev",
        "unmounted",
        "Find installed development guidance without enabling it; keep a project marker.",
    );
    let first = wait(&server, "anchor-dev", &first, "completed");
    let workspace = workspace_with(&host, "project-marker.txt");
    let workspace_before = goose::file_inventory(&workspace);
    let (status, mounted) = server.request(
        "POST",
        "/sessions/anchor-dev/plugins/anchor-steward",
        Some(&json!({"on":true})),
    );
    assert_eq!(status, 200, "{mounted}");
    let second = submit(
        &server,
        "anchor-dev",
        "mounted",
        "Read Anchor Dev's entry and only its Graph authoring Skill.",
    );
    let second = wait(&server, "anchor-dev", &second, "completed");
    assert_eq!(goose::file_inventory(&workspace), workspace_before);
    let (status, disabled) = server.request(
        "POST",
        "/sessions/anchor-dev/plugins/anchor-steward",
        Some(&json!({"on":false})),
    );
    assert_eq!(status, 200, "{disabled}");
    let third = submit(
        &server,
        "anchor-dev",
        "disabled",
        "Check execution disclosure after disabling guidance, then inspect runtime and the saved project.",
    );
    let third = wait(&server, "anchor-dev", &third, "completed");
    provider.assert_consumed();
    assert_eq!(first["goose"], second["goose"]);
    assert_eq!(first["goose"], third["goose"]);
    assert_eq!(goose::file_inventory(&workspace), workspace_before);
    assert_eq!(fs::read(&runtime_path).unwrap(), runtime_before);

    let requests = provider.requests();
    assert_eq!(requests.len(), 15);
    let system = |request: &Value| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "system")
            .filter_map(|message| message["content"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    for (index, request) in requests.iter().enumerate() {
        let prompt = system(request);
        assert!(prompt.contains("Current Anchor Plugin capabilities for this invocation."));
        if (5..10).contains(&index) {
            assert!(prompt.contains("\"id\":\"anchor-steward\""), "{prompt}");
            assert!(prompt.contains("\"name\":\"Anchor Dev\""));
            assert!(prompt.contains("\"tool_count\":0"));
        } else {
            assert!(!prompt.contains("\"id\":\"anchor-steward\""), "{prompt}");
        }
        assert!(
            !prompt.contains(&instructions),
            "full entry injected into system prompt"
        );
        assert!(
            !prompt.contains(&selected),
            "full Skill injected into system prompt"
        );
        assert!(
            !request
                .to_string()
                .contains(&serde_json::to_string(&unselected).unwrap())
        );
        assert!(goose::tool_definition(request, "anchor_run").is_ok());
        assert!(goose::tool_definition(request, "runtime_prepare").is_ok());
    }
    let feedback =
        |index: usize| goose::tool_feedback(&requests[index]).last().unwrap()["content"].clone();
    let payload = |index: usize| {
        let contents: Value = serde_json::from_str(feedback(index).as_str().unwrap()).unwrap();
        contents[0]["value"].clone()
    };
    assert_eq!(payload(2)["plugins"], json!([]));
    assert_eq!(payload(6)["plugins"][0]["id"], "anchor-steward");
    for (index, body) in [(7, &instructions), (9, &selected)] {
        let read = payload(index);
        assert_eq!(read["text"], body.as_str());
        assert_eq!(read["end_of_file"], true);
    }
    let skills = payload(8);
    assert_eq!(skills["skills"].as_array().unwrap().len(), 6);
    for name in [
        "plugin-authoring",
        "graph-authoring",
        "rust-development",
        "python-development",
        "pixi-development",
        "git-workflow",
    ] {
        assert!(
            skills["skills"]
                .as_array()
                .unwrap()
                .iter()
                .any(|skill| { skill["path"] == format!("skills/{name}/SKILL.md") })
        );
    }
    assert_eq!(payload(11)["plugins"], json!([]));
    assert!(
        feedback(12)
            .to_string()
            .contains("not available in this catalog"),
        "{}",
        feedback(12)
    );
    assert_eq!(
        payload(3),
        payload(13),
        "guidance switch changed runtime capabilities"
    );
    evidence(
        &host,
        &provider,
        json!({
            "installed_default_unmounted":true,
            "http_mount_next_turn_summary":true,
            "real_entry_and_selected_skill_read":true,
            "disabled_execution_read_denied":true,
            "workspace_and_runtime_unchanged":true,
            "scripted_provider_not_model_autonomy":true
        }),
    );
}

struct Sources {
    root: tempfile::TempDir,
    url: String,
    requests: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Sources {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("plugin.json"),
            json!({"name":"Acquired fixture","description":"source-original"}).to_string(),
        )
        .unwrap();
        fs::write(source.join("README.md"), "Source provenance fixture\n").unwrap();
        let run = |arguments: &[&str]| {
            let output = Command::new("git")
                .args(arguments)
                .current_dir(&source)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        run(&["init", "-q"]);
        run(&["add", "."]);
        run(&[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ]);
        let bare = root.path().join("repo.git");
        run(&["clone", "--bare", "-q", ".", bare.to_str().unwrap()]);
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&bare)
            .arg("update-server-info")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(output.status.success());
        // Business tools use a private network namespace whose loopback does
        // not reach the Host. Serve the source on the Host's routable address.
        let route = UdpSocket::bind("0.0.0.0:0").unwrap();
        route.connect("192.0.2.1:9").unwrap();
        let host_address = route.local_addr().unwrap().ip();
        assert!(!host_address.is_loopback() && !host_address.is_unspecified());
        let listener = TcpListener::bind((host_address, 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (files, seen, stop) = (root.path().to_path_buf(), requests.clone(), stopped.clone());
        let thread = thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut input = BufReader::new(stream.try_clone().unwrap());
                        let mut line = String::new();
                        if input.read_line(&mut line).is_err() {
                            continue;
                        }
                        let requested = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                        loop {
                            line.clear();
                            match input.read_line(&mut line) {
                                Ok(0) | Err(_) => break,
                                Ok(_) if line == "\r\n" || line == "\n" => break,
                                Ok(_) => {}
                            }
                        }
                        seen.lock().unwrap().push(requested.clone());
                        let path = requested.split('?').next().unwrap().trim_start_matches('/');
                        let bytes = (!path.split('/').any(|part| part == ".."))
                            .then(|| fs::read(files.join(path)).ok())
                            .flatten();
                        let (status, bytes) = match bytes {
                            Some(bytes) => ("200 OK", bytes),
                            None => ("404 Not Found", Vec::new()),
                        };
                        let header = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        );
                        let _ = stream.write_all(header.as_bytes());
                        let _ = stream.write_all(&bytes);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("fixture HTTP accept: {error}"),
                }
            }
        });
        Self {
            root,
            url,
            requests,
            stopped,
            thread: Some(thread),
        }
    }
}

impl Drop for Sources {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn workspace_with(host: &Host, file: &str) -> PathBuf {
    fs::read_dir(host.base.root.path().join("work/pilot"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|root| root.join(file).is_file())
        .expect("Session workspace file not found")
}

#[test]
#[ignore = "requires pinned Goose binary and Bubblewrap"]
fn ordinary_turn_acquires_http_and_git_sources_and_freezes_network_across_followup() {
    let sources = Sources::new();
    let gate = Gate::new();
    let provider = Provider::new("pilot-authoring-acquisition", vec![
        goose::command("mkdir -p http-candidate"),
        Step::tool("anchor_run", json!({"command":["curl","--fail","--silent","--show-error",format!("{}/source/plugin.json", sources.url),"-o","http-candidate/plugin.json"]})).gated(&gate),
        Step::tool("anchor_read", json!({"path":"http-candidate/plugin.json"})),
        Step::tool("anchor_edit", json!({"path":"http-candidate/plugin.json","old_string":"source-original","new_string":"http-adapted"})).after("source-original"),
        Step::tool("plugin_review", json!({"directory":"http-candidate"})).after("sha256_after"),
        Step::tool("anchor_run", json!({"command":["git","clone",format!("{}/repo.git", sources.url),"git-candidate"]})).after("http-adapted"),
        Step::tool("anchor_read", json!({"path":"git-candidate/plugin.json"})),
        Step::tool("anchor_edit", json!({"path":"git-candidate/plugin.json","old_string":"source-original","new_string":"git-adapted"})).after("source-original"),
        Step::tool("plugin_review", json!({"directory":"git-candidate"})).after("sha256_after"),
        Step::text("HTTP and Git candidates are reviewed; nothing is installed.").after("git-adapted"),
        Step::tool("anchor_read", json!({"path":"http-candidate/plugin.json"})),
        Step::tool("anchor_edit", json!({"path":"http-candidate/plugin.json","old_string":"http-adapted","new_string":"followup-adapted"})).after("http-adapted"),
        Step::tool("plugin_review", json!({"directory":"http-candidate"})),
        goose::command(&format!("if curl --fail --silent --max-time 1 '{}/source/plugin.json' -o denied-download; then printf unexpected-network; else printf frozen-network-denied; fi", sources.url)).after("followup-adapted"),
        Step::text("The next Turn kept the candidate and denied the download.").after("frozen-network-denied"),
        Step::tool("anchor_read", json!({"path":"http-candidate/plugin.json"})),
        Step::tool("anchor_edit", json!({"path":"../outside","content":"escape"})).after("No such file or directory"),
        Step::tool("plugin_review", json!({"directory":"../http-candidate"})).after("relative to /workspace"),
        Step::text("Another Session cannot access this candidate or escape its workspace.").after("workspace-relative"),
    ]);
    let host = Host::new(&graph())
        .default_runtime()
        .with_allowed_commands("sh,cat,git,true,curl,python3");
    let network = host.base.root.path().join("plugins/network");
    fs::create_dir_all(&network).unwrap();
    fs::write(
        network.join("plugin.json"),
        r#"{"name":"Fixture task network","network":true}"#,
    )
    .unwrap();
    let server = host.serve(&provider);
    create(&server, "authoring");
    let (status, mounted) = server.request(
        "POST",
        "/sessions/authoring/plugins/network",
        Some(&json!({"on":true})),
    );
    assert_eq!(status, 200, "{mounted}");
    let first = submit(
        &server,
        "authoring",
        "get-sources",
        &format!(
            "Fetch {}/source/plugin.json and clone {}/repo.git. Adapt and review each candidate without installing.",
            sources.url, sources.url
        ),
    );
    gate.wait_entered();
    let (status, denied) = server.request(
        "POST",
        "/sessions/authoring/network",
        Some(&json!({"allowed":false})),
    );
    assert_eq!(status, 200, "{denied}");
    gate.open();
    let first = wait(&server, "authoring", &first, "completed");
    let requests = sources.requests.lock().unwrap().clone();
    assert!(
        requests
            .iter()
            .any(|request| request == "/source/plugin.json")
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("/repo.git/objects/"))
    );
    let workspace = workspace_with(&host, "http-candidate/plugin.json");
    assert!(
        fs::read_to_string(workspace.join("git-candidate/plugin.json"))
            .unwrap()
            .contains("git-adapted")
    );
    let second = submit(
        &server,
        "authoring",
        "followup",
        "Continue the saved HTTP candidate, review it and check the current network denial.",
    );
    let second = wait(&server, "authoring", &second, "completed");
    assert_eq!(first["goose"], second["goose"]);
    assert!(
        fs::read_to_string(workspace.join("http-candidate/plugin.json"))
            .unwrap()
            .contains("followup-adapted")
    );
    assert_eq!(
        *sources.requests.lock().unwrap(),
        requests,
        "denied Turn reached the HTTP source"
    );
    create(&server, "isolated");
    let isolated = submit(
        &server,
        "isolated",
        "isolation",
        "Check Session isolation and reject attempts to write or review outside your own workspace.",
    );
    let isolated = wait(&server, "isolated", &isolated, "completed");
    assert_ne!(first["goose"]["scope"], isolated["goose"]["scope"]);
    assert!(!host.base.root.path().join("work/pilot/outside").exists());
    assert!(
        !host
            .base
            .root
            .path()
            .join("plugins/http-candidate")
            .exists()
    );
    evidence(
        &host,
        &provider,
        json!({"first":first,"followup":second,"isolated":isolated,"http_git_requests":requests,"source_files":goose::file_inventory(sources.root.path()),"workspace_files":goose::file_inventory(&workspace),"frozen_network_and_next_turn_denial":true}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary and Bubblewrap"]
fn responses_authoring_previous_response_continues_and_an_independent_response_is_isolated() {
    let provider = Provider::new("pilot-authoring-responses", vec![
        goose::command("mkdir -p candidate"),
        Step::tool("anchor_edit", json!({"path":"candidate/plugin.json","content":"{\"name\":\"Responses fixture\",\"description\":\"response-first\"}"})),
        Step::tool("plugin_review", json!({"directory":"candidate"})).after("sha256_after"),
        Step::text("response candidate reviewed").after("response-first"),
        Step::tool("anchor_read", json!({"path":"candidate/plugin.json"})).expect_request_contains("response candidate reviewed"),
        Step::tool("anchor_edit", json!({"path":"candidate/plugin.json","old_string":"response-first","new_string":"response-followup"})).after("response-first"),
        Step::tool("plugin_review", json!({"directory":"candidate"})),
        Step::text("response candidate continued").after("response-followup"),
        Step::tool("anchor_read", json!({"path":"candidate/plugin.json"})),
        Step::tool("anchor_edit", json!({"path":"/etc/anchor-escape","content":"escape"})).after("No such file or directory"),
        Step::text("independent response is isolated").after("relative to /workspace"),
    ]);
    let host = Host::new(&graph()).default_runtime();
    let server = host.serve(&provider);
    let (status, first) = server.request_with_timeout("POST", "/v1/responses", Some(&json!({"model":"anchor-copilot","input":"Develop and review a candidate in this workspace.","stream":false})), Duration::from_secs(30));
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["status"], "completed");
    let workspace = workspace_with(&host, "candidate/plugin.json");
    let (status, next) = server.request_with_timeout("POST", "/v1/responses", Some(&json!({"model":"anchor-copilot","input":"Continue the saved candidate and review the change.","previous_response_id":first["id"],"stream":false})), Duration::from_secs(30));
    assert_eq!(status, 200, "{next}");
    assert_eq!(next["status"], "completed");
    assert!(
        fs::read_to_string(workspace.join("candidate/plugin.json"))
            .unwrap()
            .contains("response-followup")
    );
    let (status, isolated) = server.request_with_timeout("POST", "/v1/responses", Some(&json!({"model":"anchor-copilot","input":"Check isolation from other Responses sessions and reject absolute paths.","stream":false})), Duration::from_secs(30));
    assert_eq!(status, 200, "{isolated}");
    assert_eq!(isolated["status"], "completed");
    let (status, sessions) = server.request("GET", "/sessions", None);
    assert_eq!(status, 200, "{sessions}");
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 2);
    evidence(
        &host,
        &provider,
        json!({"first":first,"continued":next,"independent":isolated,"sessions":sessions,"workspace_files":goose::file_inventory(&workspace)}),
    );
}

const SOCKET_SERVER: &str = r#"import json, sys, socket
for line in sys.stdin:
    message = json.loads(line)
    if 'id' not in message: continue
    method = message.get('method')
    if method == 'initialize':
        result = {'protocolVersion':'2025-03-26','capabilities':{'tools':{}},'serverInfo':{'name':'socket-probe','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':'probe','description':'observe task network','inputSchema':{'type':'object'}}]}
    elif method == 'tools/call':
        address = message['params']['arguments']['address']
        try:
            socket.create_connection(tuple(address), timeout=0.1).close()
            network = 'connected'
        except OSError: network = 'denied'
        result = {'content':[],'structuredContent':{'network':network}}
    else: result = {}
    print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}), flush=True)
"#;

fn process_with_marker(marker: &str) -> Vec<u32> {
    fs::read_dir("/proc")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let command = fs::read(entry.path().join("cmdline")).ok()?;
            command
                .windows(marker.len())
                .any(|part| part == marker.as_bytes())
                .then_some(pid)
        })
        .collect()
}

#[test]
#[ignore = "requires pinned Goose binary and Bubblewrap"]
fn candidate_declaration_cannot_enable_network_or_credentials_and_command_cancel_can_continue() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let python = [
        "/usr/bin/python3.12",
        "/usr/bin/python3.11",
        "/usr/bin/python3.10",
        "/usr/bin/python3.9",
        "/usr/bin/python3.8",
    ]
    .into_iter()
    .find(|path| Path::new(path).is_file())
    .expect("requires a system Python 3.8+ interpreter");
    let marker = format!(
        "pilot-authoring-cancel-{}-{}",
        std::process::id(),
        address.port()
    );
    let manifest = json!({"name":"Socket candidate","network":true,"mcpServers":{"probe":{"command":python,"args":["server.py"]}}}).to_string();
    let provider = Provider::new("pilot-authoring-denial-cancel", vec![
        goose::command("mkdir -p socket-candidate"),
        Step::tool("anchor_edit", json!({"path":"socket-candidate/plugin.json","content":manifest})),
        Step::tool("anchor_edit", json!({"path":"socket-candidate/server.py","content":SOCKET_SERVER})),
        Step::tool("plugin_prepare", json!({"directory":"socket-candidate"})),
        Step::tool("plugin_test", json!({"directory":"socket-candidate","server":"probe","tool":"probe","arguments":{"address":["127.0.0.1",address.port()]}})).after("prepared"),
        Step::text("The candidate declaration did not enable network.").after("denied"),
        Step::tool("anchor_run", json!({"command":["sh","-c",format!("printf started > command-started; sleep 30; printf unexpected > command-finished # {marker}")]})),
    ]);
    let commands = format!(
        "sh,cat,git,true,{}",
        Path::new(python).file_name().unwrap().to_str().unwrap()
    );
    let host = Host::new(&graph())
        .default_runtime()
        .with_allowed_commands(&commands);
    let server = host.serve(&provider);
    create(&server, "cancel-authoring");
    let first = submit(
        &server,
        "cancel-authoring",
        "candidate-network",
        "Create, prepare and really call a socket candidate with no task network grant; its own network declaration is not authorization.",
    );
    let first = wait(&server, "cancel-authoring", &first, "completed");
    let workspace = workspace_with(&host, "socket-candidate/plugin.json");
    let cancelled = submit(
        &server,
        "cancel-authoring",
        "cancel-command",
        "Run a cancellable command in this same workspace.",
    );
    goose::wait_until("sandbox command started", || {
        workspace.join("command-started").is_file()
    });
    assert!(
        !process_with_marker(&marker).is_empty(),
        "the real command was never running"
    );
    let (status, ack) = server.request("POST", "/sessions/cancel-authoring/stop", Some(&json!({})));
    assert_eq!(status, 202, "{ack}");
    let stopped = wait(&server, "cancel-authoring", &cancelled, "stopped");
    goose::wait_until("sandbox command reaped", || {
        process_with_marker(&marker).is_empty()
    });
    assert!(!workspace.join("command-finished").exists());
    provider.append(vec![
        Step::tool("anchor_read", json!({"path":"command-started"})),
        Step::tool("anchor_edit", json!({"path":"socket-candidate/plugin.json","old_string":format!("\"command\":{}", json!(python)),"new_string":format!("\"command\":{},\"env_vars\":[\"HOST_PRIVATE_CREDENTIAL\"]", json!(python))})).after("started"),
        Step::tool("plugin_test", json!({"directory":"socket-candidate"})),
        Step::text("The command stopped, facts were checked, and the candidate cannot request host credentials.").after("does not grant host credentials"),
    ]);
    let recovered = submit(
        &server,
        "cancel-authoring",
        "inspect-and-continue",
        "Inspect the saved command fact and continue this candidate; verify host credential declarations are rejected.",
    );
    let recovered = wait(&server, "cancel-authoring", &recovered, "completed");
    assert_eq!(first["goose"], recovered["goose"]);
    let (status, session) = server.request("GET", "/sessions/cancel-authoring/plugins", None);
    assert_eq!(status, 200, "{session}");
    let plugins = session["plugins"].as_array().unwrap();
    assert_eq!(
        plugins.len(),
        1,
        "candidate changed installed/mounted plugins: {session}"
    );
    assert_eq!(
        plugins[0]["id"], "network",
        "candidate changed the default plugin: {session}"
    );
    evidence(
        &host,
        &provider,
        json!({"first":first,"stopped":stopped,"continued":recovered,"session":session,"workspace_files":goose::file_inventory(&workspace),"actual_command_reaped":true,"candidate_network_and_credential_self_authorization_rejected":true}),
    );
}
