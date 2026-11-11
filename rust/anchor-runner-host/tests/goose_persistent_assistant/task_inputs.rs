use super::*;
use anchor_platform_session::{ChannelNotificationFile, TaskResult};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

const TASK_CASE_SOURCE: &str = "tests/goose_persistent_assistant/task_inputs.rs";

fn task(id: &str, source: &str) -> TaskResult {
    TaskResult {
        id: id.into(),
        source_run: Some(source.into()),
        outcome: "completed".into(),
        content: json!({"summary":format!("completed {id}")}),
        files: vec![],
    }
}

fn task_host(definition: &Value) -> Host {
    let host = host_for(definition)
        .with_extra_environment([("ANCHOR_RUNNER_GRAPH_NAME", "deployment-fixture")]);
    // host_for intentionally mounts only wecom; publish the task-enabled bot as
    // a separate library package so this fixture exercises its real grant.
    let bundle = host.base.root.path().join("fixture");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(bundle.join("graph.json"), definition.to_string()).unwrap();
    fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"wecom"},{"id":"graph-tasks"},{"id":"network"}]})
            .to_string(),
    )
    .unwrap();
    fixture::install_system_plugin(
        host.base.root.path(),
        anchor_graph_host::SystemPlugin::GraphTasks,
    );
    host
}

/// This test replaces only the private channel control transport. Gateway
/// ledger replay and partial ACK behavior have their own real-gateway tests.
struct TaskNoticeGateway {
    _root: tempfile::TempDir,
    requests: Arc<Mutex<Vec<Value>>>,
    stopped: Arc<AtomicBool>,
    task: Option<thread::JoinHandle<()>>,
}

impl TaskNoticeGateway {
    fn bind(host: &mut Host) -> Self {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("control.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let descriptor = root.path().join("control.json");
        fs::write(
            &descriptor,
            json!({"socket":socket,"token":"private-task-fixture"}).to_string(),
        )
        .unwrap();
        host.set_bot_control_descriptor(&descriptor);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stopping = stopped.clone();
        let task = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) => panic!("private task gateway: {error}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                BufReader::new(&mut stream).read_line(&mut line).unwrap();
                let mut request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["token"], "private-task-fixture");
                let ack = if request["operation"] == "status" {
                    json!({"status":"authenticated"})
                } else {
                    assert_eq!(request["userid"], ALICE);
                    assert!(matches!(
                        request["operation"].as_str(),
                        Some("send" | "send_file")
                    ));
                    request.as_object_mut().unwrap().remove("token");
                    let ack = json!({"accepted":true,"request_id":request["request_id"]});
                    received.lock().unwrap().push(request);
                    ack
                };
                writeln!(stream, "{ack}").unwrap();
            }
        });
        Self {
            _root: root,
            requests,
            stopped,
            task: Some(task),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for TaskNoticeGateway {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn assign_once(server: &HttpHost, session: &str, graph: &str, instruction: &str) -> String {
    let at = (chrono::Utc::now() + chrono::Duration::seconds(4))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();
    let (status, created) = server.request(
        "POST",
        "/schedules",
        Some(&json!({"graph":graph,"rule":{"type":"once","at":at},
            "input":{"bot_graph":"fixture","userid":ALICE}})),
    );
    assert_eq!(status, 201, "{created}");
    let id = created["schedule"]["id"].as_str().unwrap();
    let (status, assigned) = server.request(
        "POST",
        &format!("/schedules/{id}/assign"),
        Some(&json!({"session":session,"instruction":instruction,"input":{}})),
    );
    assert_eq!(status, 200, "{assigned}");
    assert_eq!(assigned["schedule"]["next_at"], at);
    assert_eq!(assigned["schedule"]["input"], json!({}));
    assert_eq!(assigned["schedule"]["assignment"]["session"], session);
    id.into()
}

#[test]
#[ignore = "requires pinned real Goose and Bubblewrap; channel transport is strictly local"]
fn operator_assigned_schedule_wakes_the_original_assistant_to_deliver_success_and_failure() {
    const PDF: &str = "%PDF-local-scheduled-task";
    const INTRODUCTION: &str = "Here is the scheduled report.";
    const FAILURE: &str = "The scheduled report failed before publication; no report was sent.";
    let provider = Provider::new(
        "goose-persistent-assigned-schedule",
        vec![
            command("printf initial-assistant-ready"),
            finish("ready-for-assigned-schedules").after("initial-assistant-ready"),
        ],
    );
    let mut definition = graph();
    definition["nodes"][1]["plugins"] = json!([
        "wecom",{"id":"graph-tasks","graph_tasks":{"graphs":["morning-source","broken-source"]}},
        {"id":"network","network":"denied"}
    ]);
    let mut host = task_host(&definition);
    let gateway = TaskNoticeGateway::bind(&mut host);
    for (name, operation) in [
        (
            "morning-source",
            json!({"run":format!("sh -c 'printf %s {PDF} > report.pdf'"),"writes":["report.pdf"]}),
        ),
        (
            "broken-source",
            json!({"run":"sh -c 'printf producer-fixture-failed >&2; exit 7'"}),
        ),
    ] {
        let package = host.base.root.path().join(name);
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("graph.json"),
            json!({"entry":"produce","ops":{"produce":operation},
                "nodes":[{"id":"produce","op":"produce"}],"edges":[]})
            .to_string(),
        )
        .unwrap();
        fs::write(
            package.join("manifest.json"),
            json!({"format":1,"graph":"graph.json","plugins":[]}).to_string(),
        )
        .unwrap();
    }
    let server = host.serve(&provider);
    let (status, approved) = server.request(
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{approved}");
    let first = submit(
        &server,
        &event(
            ALICE,
            "assigned-initial",
            "please deliver my scheduled reports here",
        ),
        "ready-for-assigned-schedules",
    );
    let run = first["run"].as_str().unwrap();
    let session = first["session"].as_str().unwrap();
    waiting(&host, run, 1);
    let native = host.native_fact(run, "assistant", 1);
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let success_gate = Gate::new();
    provider.append(vec![
        command(&format!(
            "set -eu; test \"$(cat /in/tasks/*/1-report.pdf)\" = {PDF}; printf scheduled-product-read"
        ))
        .expect_request_contains("task_result")
        .gated(&success_gate),
    ]);
    let success_schedule = assign_once(
        &server,
        session,
        "morning-source",
        "Deliver the committed report in this conversation after checking its actual file ACK.",
    );
    success_gate.wait_entered();
    let pending = sessions.pending_task_results("local", session).unwrap();
    assert_eq!(pending.len(), 1);
    let result = &pending[0];
    assert_eq!(result.outcome, "done");
    assert_eq!(result.files.len(), 1);
    let producer = result.source_run.as_deref().unwrap();
    let metadata = fixture::read_json(
        host.base
            .state_root()
            .join("run-metadata")
            .join(format!("{producer}.json")),
    );
    assert_eq!(metadata["schedule"], success_schedule);
    assert_eq!(metadata["task"]["session"], session);
    assert_eq!(metadata["task"]["source"]["kind"], "operator_assignment");
    assert_eq!(host.record(producer)["input"], json!({}));
    let file = &result.files[0];
    provider.append(vec![
        Step::tool("anchor_tools", json!({"name":"wecom_send_file"}))
            .after("scheduled-product-read"),
        Step::tool("anchor_tool_detail", json!({"name":"wecom_send_file"}))
            .after("wecom_send_file"),
        Step::tool(
            "anchor_tools_call",
            json!({"name":"wecom_send_file","arguments":{
                "path":format!("/in/tasks/{}/{}",result.id,file.name),"content":INTRODUCTION}}),
        )
        .after("wecom_send_file"),
        finish("scheduled-product-delivered").after("file_status"),
    ]);
    success_gate.open();
    let delivered = waiting(&host, run, 2);
    assert_eq!(
        delivered["results"]["reply"][1]["completion"]["output"]["task_result_handled"],
        true
    );
    let sent = gateway.requests();
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["operation"], "send_file");
    assert_eq!(sent[0]["name"], "report.pdf");
    assert_eq!(
        STANDARD
            .decode(sent[0]["data_base64"].as_str().unwrap())
            .unwrap(),
        PDF.as_bytes()
    );
    assert_eq!(sent[1]["content"], INTRODUCTION);
    assert!(
        sent[0]["request_id"]
            .as_str()
            .unwrap()
            .starts_with("channel-task-")
    );
    let notice = sessions
        .latest_channel_notification("local", session)
        .unwrap()
        .unwrap();
    assert_eq!(notice.source_run, producer);

    let failure_gate = Gate::new();
    provider.append(vec![
        command("cat /in/wait_input/input.json; printf failed-task-observed")
            .expect_request_contains("broken-source")
            .gated(&failure_gate),
    ]);
    let failure_schedule = assign_once(
        &server,
        session,
        "broken-source",
        "If production fails, tell this user the failure instead of claiming a delivered report.",
    );
    failure_gate.wait_entered();
    let pending = sessions.pending_task_results("local", session).unwrap();
    assert_eq!(pending.len(), 1);
    let failure = &pending[0];
    assert_eq!(failure.outcome, "failed");
    assert!(failure.files.is_empty());
    assert_eq!(failure.content["event"], "run.completed");
    let failed_producer = failure.source_run.as_deref().unwrap();
    let failed_metadata = fixture::read_json(
        host.base
            .state_root()
            .join("run-metadata")
            .join(format!("{failed_producer}.json")),
    );
    assert_eq!(failed_metadata["schedule"], failure_schedule);
    assert_eq!(failed_metadata["task"]["session"], session);
    provider.append(vec![
        Step::tool("anchor_tools", json!({"name":"wecom_send_message"}))
            .after("failed-task-observed"),
        Step::tool("anchor_tool_detail", json!({"name":"wecom_send_message"}))
            .after("wecom_send_message"),
        Step::tool(
            "anchor_tools_call",
            json!({"name":"wecom_send_message","arguments":{"content":FAILURE}}),
        )
        .after("wecom_send_message"),
        finish("scheduled-failure-reported").after("confirmed"),
    ]);
    failure_gate.open();
    let completed = waiting(&host, run, 3);
    assert_eq!(
        completed["results"]["reply"][2]["completion"]["output"]["suppressed"],
        true
    );
    assert_eq!(gateway.requests().len(), 3);
    assert_eq!(gateway.requests()[2]["content"], FAILURE);
    assert!(
        sessions
            .pending_task_results("local", session)
            .unwrap()
            .is_empty()
    );
    assert_eq!(sessions.list_turns("local", session).unwrap().len(), 3);
    assert_eq!(
        host.native_fact(run, "assistant", 3)["session_id"],
        native["session_id"]
    );
    provider.assert_consumed();
    idle(&provider, 12);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({"case_source":TASK_CASE_SOURCE,"operator_assignment":true,
            "real_ticker_and_producer":true,"success_run":producer,"failed_run":failed_producer,
            "same_session_and_native_history":true,"gateway_requests":gateway.requests(),
            "real_model_requests":0,"real_wecom_requests":0,"stopped":stopped}),
    );
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn agent_delegates_an_authorized_graph_and_its_real_completion_wakes_the_same_session() {
    let provider = Provider::new("goose-persistent-task-delegation", vec![
        Step::tool("anchor_tools", json!({"plugin":"graph-tasks"})).expect_request_contains("start-my-authorized-report"),
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_list"})).after("graph_tasks_list"),
        Step::tool("anchor_tools_call", json!({"name":"graph_tasks_list","arguments":{}})).after("graph_tasks_list"),
        Step::tool("anchor_tool_detail", json!({"name":"graph_tasks_run"})).after("morning-source"),
        Step::tool("anchor_tools_call", json!({"name":"graph_tasks_run","arguments":{"graph":"morning-source","input":{"request":"fixture-report"}}})).after("graph_tasks_run"),
        finish("authorized-report-started").after("accepted"),
        command("set -eu; test \"$(cat /in/tasks/*/1-report.md)\" = actual-delegated-report; cp /in/tasks/*/1-report.md delegated-report.md; printf delegated-result-observed")
            .expect_request_contains("task_result"),
        finish("private-result-processed-no-automatic-send").after("delegated-result-observed"),
    ]);
    let mut definition = graph();
    definition["nodes"][1]["plugins"] = json!([
        "wecom",{"id":"graph-tasks","graph_tasks":{"graphs":["morning-source"]}},
        {"id":"network","network":"denied"}
    ]);
    let host = task_host(&definition);
    let source = host.base.root.path().join("morning-source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("graph.json"), json!({
        "objective":"One deterministic report", "entry":"produce",
        "ops":{"produce":{"run":"sh -c 'printf actual-delegated-report > report.md'","writes":["report.md"]}},
        "nodes":[{"id":"produce","op":"produce"}],"edges":[],
    }).to_string()).unwrap();
    fs::write(
        source.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[]}).to_string(),
    )
    .unwrap();
    let server = host.serve(&provider);
    let (status, saved) = server.request(
        "PUT",
        "/graphs/fixture",
        Some(&json!({"definition":definition})),
    );
    assert_eq!(status, 200, "{saved}");
    let first = submit(
        &server,
        &event(ALICE, "delegation-user", "start-my-authorized-report"),
        "authorized-report-started",
    );
    let run = first["run"].as_str().unwrap();
    let session = first["session"].as_str().unwrap();
    let completed = waiting(&host, run, 2);
    provider.assert_consumed();
    let input = &completed["results"]["wait_input"][1]["completion"]["output"];
    assert_eq!(input["source"], "task_result");
    assert!(input.get("channel").is_none());
    let result = &input["task_result"];
    assert_eq!(result["outcome"], "done");
    assert_eq!(result["content"]["graph"], "morning-source");
    let child = result["source_run"].as_str().unwrap();
    assert_ne!(child, run);
    let child_record = host.record(child);
    assert_eq!(child_record["input"]["request"], "fixture-report");
    assert_eq!(
        child_record["results"]["produce"].as_array().unwrap().len(),
        1
    );
    let child_metadata = fixture::read_json(
        host.base
            .state_root()
            .join("run-metadata")
            .join(format!("{child}.json")),
    );
    assert_eq!(child_metadata["task"]["session"], session);
    assert_eq!(child_metadata["task"]["source"]["kind"], "agent_delegation");
    assert_eq!(child_metadata["task"]["source"]["key"]["run_id"], run);
    assert_eq!(
        completed["results"]["reply"][1]["completion"]["output"]["suppressed"],
        true
    );
    assert_eq!(
        completed["results"]["reply"][1]["completion"]["output"]["task_result_handled"],
        true
    );
    assert_eq!(
        host.native_fact(run, "assistant", 1)["session_id"],
        host.native_fact(run, "assistant", 2)["session_id"]
    );
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    assert!(
        sessions
            .pending_task_results("local", session)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        sessions
            .task_result("local", session, result["id"].as_str().unwrap())
            .unwrap()
            .unwrap()
            .source_run
            .as_deref(),
        Some(child)
    );
    idle(&provider, 8);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({"case_source":TASK_CASE_SOURCE,"actual_native_graph_tasks_run":true,"child_run":child,
        "terminal_completion_pump":true,"same_session_and_native_history":true,"silent_result_ack":true,"stopped":stopped}),
    );
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn queued_task_results_wake_the_existing_assistant_silently_with_exact_readonly_files() {
    let provider = Provider::new("goose-persistent-task-results", vec![
        command("printf ready > state.txt; printf initial-ready"),
        finish("ready-for-background-tasks").after("initial-ready"),
        command("set -eu; test \"$(cat /in/tasks/morning-one/briefing.md)\" = selected-report; test ! -e /in/tasks/morning-one/unrelated.txt; if printf bad > /in/tasks/morning-one/briefing.md 2>/dev/null; then exit 9; fi; case \"$(cat /in/wait_input/input.json)\" in *'\"channel\"'*) exit 10;; esac; cp /in/tasks/morning-one/briefing.md background.txt; printf first-task-readonly-ok")
            .expect_request_contains("morning-one"),
        finish("internal-note-must-not-be-delivered").after("first-task-readonly-ok"),
        command("set -eu; test \"$(cat background.txt)\" = selected-report; test ! -e /in/tasks/morning-one/briefing.md; printf second-task-fifo-ok")
            .expect_request_contains("morning-two"),
        finish("second-internal-note-must-not-be-delivered").after("second-task-fifo-ok"),
    ]);
    let host = host();
    let source = host.base.root.path().join("task-source");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("graph.json"), json!({
        "entry":"produce", "ops":{"produce":{"run":"sh -c 'printf selected-report > report.md; printf unrelated > unrelated.txt'","writes":["report.md","unrelated.txt"]}},
        "nodes":[{"id":"produce","op":"produce"}],"edges":[],
    }).to_string()).unwrap();
    fs::write(
        source.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[]}).to_string(),
    )
    .unwrap();
    let server = host.serve(&provider);
    let first = submit(
        &server,
        &event(ALICE, "background-initial", "please wait"),
        "ready-for-background-tasks",
    );
    let run = first["run"].as_str().unwrap();
    waiting(&host, run, 1);
    let native = host.native_fact(run, "assistant", 1);
    let (status, accepted) = server.request(
        "POST",
        "/trigger",
        Some(&json!({"graph":"task-source","input":{}})),
    );
    assert_eq!(status, 202, "{accepted}");
    let source_run = accepted["run"].as_str().unwrap();
    server.wait_status(source_run, "completed");
    let source_record = host.record(source_run);
    let commit = source_record["results"]["produce"][0]["commit"]["id"]
        .as_str()
        .unwrap();
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let session = first["session"].as_str().unwrap();
    let mut first_task = task("morning-one", source_run);
    first_task.files.push(ChannelNotificationFile {
        artifact_id: commit.into(),
        path: "report.md".into(),
        name: "briefing.md".into(),
    });
    let second_task = task("morning-two", source_run);
    for result in [&first_task, &second_task, &first_task] {
        sessions
            .record_task_result("local", session, result)
            .unwrap();
    }
    let completed = waiting(&host, run, 3);
    provider.assert_consumed();
    for index in [1, 2] {
        let reply = &completed["results"]["reply"][index]["completion"];
        assert_eq!(reply["output"]["suppressed"], true);
        assert_eq!(reply["output"]["task_result_handled"], true);
        assert!(
            !reply["submission"]
                .as_str()
                .unwrap()
                .contains("internal-note")
        );
    }
    assert!(
        sessions
            .pending_task_results("local", session)
            .unwrap()
            .is_empty()
    );
    assert_eq!(sessions.list_turns("local", session).unwrap().len(), 3);
    assert!(
        sessions
            .get_channel_inbound("local", session, "morning-one")
            .is_err()
    );
    assert_eq!(
        host.native_fact(run, "assistant", 3)["session_id"],
        native["session_id"]
    );
    assert_eq!(
        fs::read(workspace(&host, run).join("background.txt")).unwrap(),
        b"selected-report"
    );
    idle(&provider, 6);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({"case_source":TASK_CASE_SOURCE,"task_results":2,"same_native_session":true,
        "silent_replies":true,"selected_file_readonly":true,"source_run":source_run,"stopped":stopped}),
    );
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn user_input_interrupts_task_processing_and_the_same_result_is_retried_without_repeating_effects()
{
    let old_gate = Gate::new();
    let user_gate = Gate::new();
    let provider = Provider::new("goose-persistent-task-interruption", vec![
        command("printf initial-ready"), finish("ready").after("initial-ready"),
        command("set -eu; test ! -e effect.txt; printf performed-once > effect.txt; printf task-effect-recorded")
            .expect_request_contains("background-interrupted"),
        finish("never-ack-the-interrupted-attempt").after("task-effect-recorded").gated(&old_gate),
        command("set -eu; test \"$(cat effect.txt)\" = performed-once; printf urgent-user-inspected")
            .expect_request_contains("urgent-user-input").gated(&user_gate),
        finish("urgent-user-answered").after("urgent-user-inspected"),
        command("set -eu; test \"$(cat effect.txt)\" = performed-once; printf task-retry-inspected-existing-effect")
            .expect_request_contains("background-interrupted"),
        finish("background-finished-silently").after("task-retry-inspected-existing-effect"),
    ]);
    let host = host();
    let server = host.serve(&provider);
    let first = submit(
        &server,
        &event(ALICE, "task-interrupt-initial", "wait for background work"),
        "ready",
    );
    let run = first["run"].as_str().unwrap();
    let session = first["session"].as_str().unwrap();
    waiting(&host, run, 1);
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let result = task("background-interrupted", "completed-source-fixture");
    sessions
        .record_task_result("local", session, &result)
        .unwrap();
    old_gate.wait_entered();
    assert_eq!(
        sessions.pending_task_results("local", session).unwrap(),
        vec![result.clone()]
    );
    let urgent = event(ALICE, "task-interrupt-user", "urgent-user-input");
    let answered = thread::scope(|scope| {
        let request = scope.spawn(|| submit(&server, &urgent, "urgent-user-answered"));
        entered(&provider, 5, &user_gate, &request);
        let current = host.record(run);
        assert_eq!(
            current["results"]["reply"][1]["completion"]["output"]["suppressed"],
            true
        );
        assert!(
            current["results"]["reply"][1]["completion"]["output"]
                .get("task_result_handled")
                .is_none()
        );
        assert_eq!(
            sessions.pending_task_results("local", session).unwrap(),
            vec![result.clone()]
        );
        old_gate.open();
        user_gate.open();
        request.join().unwrap()
    });
    assert_eq!(answered["run"], first["run"]);
    let completed = waiting(&host, run, 4);
    assert_eq!(
        completed["results"]["reply"][3]["completion"]["output"]["task_result_handled"],
        true
    );
    assert!(
        sessions
            .pending_task_results("local", session)
            .unwrap()
            .is_empty()
    );
    for (index, status) in [(1, TurnStatus::Interrupted), (3, TurnStatus::Completed)] {
        let wait = &completed["results"]["wait_input"][index]["completion"]["output"];
        assert_eq!(wait["source"], "task_result");
        assert_eq!(
            sessions
                .get_turn("local", session, wait["turn"].as_str().unwrap())
                .unwrap()
                .status,
            status
        );
    }
    assert_eq!(
        host.native_fact(run, "assistant", 2)["session_id"],
        host.native_fact(run, "assistant", 4)["session_id"]
    );
    let requests = provider.requests();
    assert!(
        requests[6]["messages"]
            .to_string()
            .contains("task-effect-recorded")
    );
    idle(&provider, 8);
    let stopped = stop(&server, &provider, run);
    evidence(
        &host,
        &provider,
        run,
        json!({"case_source":TASK_CASE_SOURCE,"user_priority":true,"task_result_preserved":true,
        "source_graph_not_restarted":true,"prior_effect_inspected_not_repeated":true,"same_native_session":true,"stopped":stopped}),
    );
}
