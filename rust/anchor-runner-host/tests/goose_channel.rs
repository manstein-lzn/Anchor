#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

#[path = "support/goose_native_gateway.rs"]
mod native_gateway;

use anchor_graph_host::FilePluginCatalog;
use anchor_graph_host::PluginCatalog;
use anchor_runtime::graph::InvocationKey;
use goose::{Host, Provider, Step, command, complete, tool_definition, tool_feedback};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::oneshot,
    task::JoinSet,
};

const SEND_TOOL: &str = "wecom_send_message";
const SEND_FILE_TOOL: &str = "wecom_send_file";
const USER: &str = "fixture-authorized-user";
const CONTENT: &str = "identical fixture message; never delivered to WeCom";
const PDF: &[u8] = b"%PDF-notification-fixture";
const TOKEN: &str = "fixture-private-gateway-token";
const CASE_SOURCE: &str = "tests/goose_channel.rs";

#[derive(Clone, Copy)]
enum Delivery {
    Ack,
    Unknown,
}

struct Gateway {
    root: tempfile::TempDir,
    descriptor: PathBuf,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Option<oneshot::Sender<()>>,
    task: Option<thread::JoinHandle<()>>,
}

impl Gateway {
    fn new(provider: &Provider, delivery: Delivery) -> Self {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let socket = root.path().join("private.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        let descriptor = root.path().join("gateway.json");
        fs::write(
            &descriptor,
            json!({"socket":socket,"token":TOKEN}).to_string(),
        )
        .unwrap();
        fs::set_permissions(&descriptor, fs::Permissions::from_mode(0o600)).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let shared = requests.clone();
        let effects = format!("{}/effects", provider.url);
        let evidence = provider.root.join("gateway.json");
        fs::write(&evidence, "[]").unwrap();
        let (stop, mut stopped) = oneshot::channel();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                    let client = reqwest::Client::builder()
                        .no_proxy()
                        .timeout(Duration::from_secs(5))
                        .build()
                        .unwrap();
                    let mut handlers = JoinSet::new();
                    loop {
                        tokio::select! {
                            _ = &mut stopped => break,
                            Some(result) = handlers.join_next(), if !handlers.is_empty() => result.unwrap(),
                            accepted = listener.accept() => {
                                let (stream, _) = accepted.unwrap();
                                let shared = shared.clone();
                                let evidence = evidence.clone();
                                let effects = effects.clone();
                                let client = client.clone();
                                handlers.spawn(async move {
                                    let mut reader = BufReader::new(stream);
                                    let mut bytes = Vec::new();
                                    let read = tokio::time::timeout(Duration::from_secs(5), (&mut reader).take(65_537).read_until(b'\n', &mut bytes)).await.unwrap().unwrap();
                                    assert!(read > 0 && read <= 65_536 && bytes.last() == Some(&b'\n'));
                                    let payload: Value = serde_json::from_slice(&bytes).unwrap();
                                    // The Host probes its bots' gateways for status.
                                    if payload["operation"] == "status" {
                                        let _ = reader.get_mut().write_all(b"{\"status\":\"authenticated\"}\n").await;
                                        return;
                                    }
                                    assert_eq!(payload["userid"], USER);
                                    assert_eq!(payload["token"], TOKEN);
                                    match payload["operation"].as_str().unwrap() {
                                        "send" => assert_eq!(payload["content"], CONTENT),
                                        "send_file" => {
                                            use base64::{Engine as _, engine::general_purpose::STANDARD};
                                            assert_eq!(payload["name"], "morning.pdf");
                                            assert_eq!(payload["md5"], format!("{:x}", md5::Md5::digest(PDF)));
                                            assert_eq!(STANDARD.decode(payload["data_base64"].as_str().unwrap()).unwrap(), PDF);
                                        }
                                        operation => panic!("unexpected gateway operation: {operation}"),
                                    }
                                    let mut public_payload = payload.clone();
                                    public_payload.as_object_mut().unwrap().remove("token");
                                    let effect: Value = client.post(&effects).json(&public_payload).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
                                    assert!(effect["effect_count"].as_u64().unwrap() >= 1);
                                    {
                                        let mut requests = shared.lock().unwrap();
                                        requests.push(public_payload);
                                        fs::write(&evidence, serde_json::to_vec_pretty(&*requests).unwrap()).unwrap();
                                    }
                                    match delivery {
                                        Delivery::Ack => {
                                            let ack = json!({"accepted":true,"request_id":payload["request_id"]});
                                            reader.get_mut().write_all(format!("{ack}\n").as_bytes()).await.unwrap();
                                        }
                                        Delivery::Unknown => std::future::pending::<()>().await,
                                    }
                                });
                            }
                        }
                    }
                    handlers.abort_all();
                    while let Some(result) = handlers.join_next().await {
                        if let Err(error) = result {
                            assert!(error.is_cancelled(), "gateway handler failed: {error}");
                        }
                    }
                });
        });
        Self {
            root,
            descriptor,
            requests,
            stop: Some(stop),
            task: Some(task),
        }
    }

    fn bind_host(&self, host: Host) -> Host {
        assert!(self.descriptor.starts_with(self.root.path()));
        bind_bot(host, &self.descriptor)
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    fn wait_received(&self, count: usize) {
        goose::wait_until("private gateway received fake delivery", || {
            self.requests().len() == count
        });
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

/// The bot Graph's assistant is the node under test; its recipients come
/// from the bot binding, never from the process environment.
fn bind_bot(host: Host, descriptor: &std::path::Path) -> Host {
    install_resource_plugin(&host, descriptor);
    host.with_bot("fixture", &[USER])
        .with_extra_environment([("ANCHOR_ASSISTANT_AUTO_RESUME", "0")])
}

fn graph() -> Value {
    json!({
        "objective":"Goose channel identity through the real Rust Host and private fake gateway",
        "entry":"wait_input",
        "agents":{"worker":{
            "model":goose::MODEL,
            "instructions":"Use only admitted tools and recipients. An uncertain delivery is not permission to resend; inspect external state before continuing. Write evidence.txt and finish with route reply.",
            "network":true
        }},
        "ops":{
            "wait_input":{"host":{"operation":"session.wait_input"}},
            "reply":{"host":{"operation":"session.reply"}}
        },
        "nodes":[
            {"id":"wait_input","op":"wait_input"},
            {"id":"worker","agent":"worker","plugins":["wecom","network"]},
            {"id":"reply","op":"reply"}
        ],
        "edges":[
            {"from":"wait_input","to":"worker"},
            {"from":"worker","to":"reply"},
            {"from":"reply","to":"wait_input"}
        ]
    })
}

/// A channel Plugin whose gateway process publishes the test's private fake
/// gateway descriptor as this bot Graph's own control descriptor.
fn install_resource_plugin(host: &Host, descriptor: &std::path::Path) -> Value {
    let bundle = host.base.root.path().join("bundle");
    let library = host.base.root.path();
    let plugin = library.join("plugins/wecom");
    fs::create_dir_all(plugin.join("skills/channel")).unwrap();
    fs::create_dir_all(plugin.join("bin")).unwrap();
    fs::write(plugin.join("plugin.json"), json!({
        "name":"WeCom resource-only fixture",
        "description":"Host-owned channel tools without an external MCP process or real messages.",
        "skills":"./skills"
    }).to_string()).unwrap();
    fs::write(plugin.join("skills/channel/SKILL.md"),
        "---\nname: fixture-channel\ndescription: Use host-scoped channel sends.\n---\nUse wecom_send_message for authorized users. Never automatically resend a cancelled or unconfirmed delivery. Inspect external state first.\n").unwrap();
    fs::write(
        plugin.join("channel.json"),
        json!({"platform":"wecom","transport":"websocket","entrypoint":"bin/gateway","required_environment":[]}).to_string(),
    )
    .unwrap();
    let gateway = plugin.join("bin/gateway");
    fs::write(
        &gateway,
        format!(
            "#!/bin/sh\nset -eu\ncat '{}' > \"$ANCHOR_CHANNEL_CONTROL_DESCRIPTOR\"\nexec /bin/sleep 3600\n",
            descriptor.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&gateway, fs::Permissions::from_mode(0o700)).unwrap();
    let binding = FilePluginCatalog::new(library)
        .resolve(&["wecom".into()])
        .unwrap()
        .remove(0);
    let summary = json!({"id":binding.id,"digest":binding.digest,"resources":binding.resources,"mcp_servers":binding.mcp_servers});
    assert_eq!(summary["mcp_servers"], json!([]));
    assert!(!plugin.join(".mcp.json").exists());
    // A persistent assistant reaches the network through the network Plugin.
    let network = library.join("plugins/network");
    fs::create_dir_all(&network).unwrap();
    fs::write(
        network.join("plugin.json"),
        r#"{"name":"Fixture task network","network":true}"#,
    )
    .unwrap();
    fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"wecom"},{"id":"network"}]})
            .to_string(),
    )
    .unwrap();
    summary
}

fn event(id: &str) -> Value {
    json!({"event":{
        "source":"wecom","event_id":id,"sender_id":USER,
        "conversation_id":"goose-channel-conversation","text":id,
        "reply_target":"goose-channel-conversation","message_type":"text",
        "metadata":{"chat_type":"single","request_id":id},"attachments":[]
    }})
}

/// Send one message to the bot and return the assistant Run that answered it.
fn ask(server: &goose::HttpHost, id: &str) -> String {
    let (status, response) = server.request_with_timeout(
        "POST",
        "/channels/fixture/events",
        Some(&event(id)),
        Duration::from_secs(60),
    );
    assert_eq!(status, 200, "{response}");
    response["run"].as_str().unwrap().to_owned()
}

/// The bot's assistant Run, before any reply names it.
fn assistant_run(host: &Host) -> String {
    let mut run = None;
    goose::wait_until("the bot's assistant Run", || {
        run = fs::read_dir(host.base.state_root().join("run-metadata"))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .find_map(|name| {
                name.strip_suffix(".json")
                    .filter(|run| run.starts_with("assistant-"))
                    .map(str::to_owned)
            });
        run.is_some()
    });
    run.unwrap()
}

/// The assistant answered `replies` Turns and waits for the next message.
fn answered(host: &Host, run: &str, replies: usize) {
    goose::wait_until("assistant answered its Turn", || {
        host.record(run)["results"]["reply"]
            .as_array()
            .is_some_and(|results| results.len() == replies)
    });
}

fn send(userid: &str) -> Step {
    Step::tool(SEND_TOOL, json!({"userid":userid,"content":CONTENT}))
}

fn native_sends(history: &Value) -> Vec<Value> {
    history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .filter(|block| {
            block["type"] == "toolRequest"
                && block["toolCall"]["value"]["name"]
                    .as_str()
                    .is_some_and(|name| {
                        name == SEND_TOOL || name.ends_with(&format!("__{SEND_TOOL}"))
                    })
        })
        .cloned()
        .collect()
}

fn expected_request_id(key: &InvocationKey, session: &str, tool_call: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"anchor-goose-channel-send-v1\0");
    digest.update(serde_json::to_vec(&(key, session, tool_call, SEND_TOOL)).unwrap());
    format!("channel-send-{:x}", digest.finalize())
}

fn assert_artifact(host: &Host, run: &str, expected: &[u8]) {
    let record = host.record(run);
    assert_eq!(host.base.file(&record, "worker", "evidence.txt"), expected);
}

fn assert_no_legacy_attempts(host: &Host) {
    let state = host.base.root.path().join("state");
    assert!(!state.join("io-harness").exists());
    assert!(!state.join("harness").exists());
}

#[test]
#[ignore = "requires pinned Goose binary; channel delivery is strictly local"]
fn confirmed_file_notification_joins_the_existing_assistant_with_exact_selected_context() {
    use anchor_platform_session::SessionStore;

    let provider = Provider::new(
        "goose-channel-file-notification",
        vec![
            command("printf assistant-ready > state.txt; cat state.txt"),
            complete("reply").after("assistant-ready"),
        ],
    );
    let gateway = Gateway::new(&provider, Delivery::Ack);
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    let task = host.base.root.path().join("notification-task");
    fs::create_dir_all(&task).unwrap();
    fs::write(task.join("graph.json"), json!({
        "objective":"Deliver one committed report and preserve its exact discussion context",
        "entry":"produce",
        "ops":{
            "produce":{"run":"sh -c 'printf %s %PDF-notification-fixture > morning.pdf; printf original-report-evidence > report.txt; printf unrelated-delivery-file > unrelated.txt'","writes":["morning.pdf","report.txt","unrelated.txt"]},
            "deliver":{"call":{
                "graph":"notification-delivery","mode":"wait",
                "input_map":{"bot_graph":"/bot_graph","userid":"/userid"},
                "files":[{"node":"produce","path":"morning.pdf","as":"morning.pdf"},{"node":"produce","path":"report.txt","as":"report.txt"}],
                "result":{"node":"worker","files":["evidence.txt"]}
            }}
        },
        "nodes":[{"id":"produce","op":"produce"},{"id":"deliver","op":"deliver"}],
        "edges":[{"from":"produce","to":"deliver"}]
    }).to_string()).unwrap();
    fs::write(
        task.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[]}).to_string(),
    )
    .unwrap();
    let delivery = host.base.root.path().join("notification-delivery");
    fs::create_dir_all(&delivery).unwrap();
    fs::write(delivery.join("graph.json"), json!({
        "entry":"prepare",
        "agents":{"worker":{"model":goose::MODEL,"instructions":"Send only the prepared PDF and report to the explicit input bot_graph and userid, inspect the ACK, then finish with route done.","network":false}},
        "ops":{
            "prepare":{"run":"cp /in/call/morning.pdf morning.pdf; cp /in/call/report.txt report.txt","writes":["morning.pdf","report.txt"]},
            "done":{"run":"true"}
        },
        "nodes":[{"id":"prepare","op":"prepare"},{"id":"worker","agent":"worker","plugins":["wecom"]},{"id":"done","op":"done"}],
        "edges":[{"from":"prepare","to":"worker"},{"from":"worker","to":"done"}]
    }).to_string()).unwrap();
    fs::write(
        delivery.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"wecom"}]}).to_string(),
    )
    .unwrap();
    let server = host.serve(&provider);
    let mut first_event = event("notification-existing-conversation");
    first_event["event"]["conversation_id"] = json!(USER);
    first_event["event"]["reply_target"] = json!(USER);
    let (status, first) = server.request("POST", "/channels/fixture/events", Some(&first_event));
    assert_eq!(status, 200, "{first}");
    let assistant = first["run"].as_str().unwrap();
    answered(&host, assistant, 1);
    let native_before = host.native_fact(assistant, "worker", 1);
    provider.assert_consumed();

    provider.append(vec![
        Step::tool(
            SEND_FILE_TOOL,
            json!({
                "bot_graph":"fixture","userid":USER,"path":"/in/prepare/morning.pdf",
                "content":CONTENT,"context_files":["/in/prepare/report.txt"]
            }),
        ),
        command("printf confirmed-report-delivery > evidence.txt; cat evidence.txt")
            .after("accepted"),
        complete("done")
            .files(&["evidence.txt"])
            .after("confirmed-report-delivery"),
    ]);
    let (status, accepted) = server.request(
        "POST",
        "/trigger",
        Some(&json!({"graph":"notification-task","input":{"bot_graph":"fixture","userid":USER}})),
    );
    assert_eq!(status, 202, "{accepted}");
    let source_run = accepted["run"].as_str().unwrap();
    let parent = server.wait_status(source_run, "completed");
    let delivery_run = parent["calls"][0]["run"].as_str().unwrap();
    assert_eq!(parent["calls"][0]["graph"], "notification-delivery");
    provider.assert_consumed();
    let requests = gateway.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["operation"], "send_file");
    assert_eq!(requests[1]["operation"], "send");
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let session = first["session"].as_str().unwrap();
    let notification = sessions
        .latest_channel_notification("local", session)
        .unwrap()
        .unwrap();
    assert_eq!(notification.source_run, delivery_run);
    assert_eq!(host.record(delivery_run)["input"]["userid"], USER);
    assert_eq!(host.record(delivery_run)["input"]["bot_graph"], "fixture");
    assert_eq!(
        host.base
            .file(&host.record(source_run), "deliver", "result/evidence.txt"),
        b"confirmed-report-delivery"
    );
    assert_eq!(notification.content, CONTENT);
    assert_eq!(notification.files.len(), 2);
    assert_eq!(notification.files[0].name, "morning.pdf");
    assert_eq!(notification.files[1].name, "report.txt");
    assert_eq!(
        host.record(assistant)["results"]["worker"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // The source workspace may change or disappear; discussion must still
    // use only the selected immutable delivery, never the current workspace.
    fs::write(
        host.base
            .workspace(source_run, "produce")
            .join("report.txt"),
        "changed-after-delivery",
    )
    .unwrap();
    let prefix = format!("/in/notifications/{}", notification.id);
    provider.append(vec![
        command(&format!("set -eu; test \"$(cat state.txt)\" = assistant-ready; test \"$(cat '{prefix}/report.txt')\" = original-report-evidence; test \"$(cat '{prefix}/morning.pdf')\" = %PDF-notification-fixture; test ! -e '{prefix}/unrelated.txt'; if printf changed > '{prefix}/report.txt'; then exit 91; fi; cat '{prefix}/report.txt' > evidence.txt; printf selected-report-readonly"))
            .expect_request_contains("notification-report-followup"),
        complete("reply")
            .files(&["evidence.txt"])
            .after("selected-report-readonly"),
    ]);
    let mut followup = first_event.clone();
    followup["event"]["event_id"] = json!("notification-report-followup");
    followup["event"]["text"] = json!("notification-report-followup");
    followup["event"]["metadata"]["request_id"] = json!("notification-report-followup");
    let (status, reply) = server.request("POST", "/channels/fixture/events", Some(&followup));
    assert_eq!(status, 200, "{reply}");
    assert_eq!(reply["run"], first["run"]);
    assert_eq!(reply["session"], first["session"]);
    answered(&host, assistant, 2);
    provider.assert_consumed();
    let native_after = host.native_fact(assistant, "worker", 2);
    assert_eq!(native_after["session_id"], native_before["session_id"]);
    let saved = host.record(assistant);
    let projected =
        &saved["results"]["wait_input"][1]["completion"]["output"]["latest_notification"];
    assert_eq!(projected["id"], notification.id);
    assert_eq!(
        projected["files"][1]["artifact_id"],
        notification.files[1].artifact_id
    );
    assert_eq!(
        fixture::artifact_file(
            &host.base.state_root().join("artifacts").join(
                saved["results"]["worker"][1]["commit"]["id"]
                    .as_str()
                    .unwrap()
            ),
            "evidence.txt",
        ),
        b"original-report-evidence"
    );
    assert_eq!(gateway.requests(), requests);
    host.evidence(
        &provider,
        assistant,
        json!({
            "case_source":CASE_SOURCE,"notification":notification,"gateway_requests":requests,
            "same_assistant_run":true,"same_host_session":true,"same_native_session":true,
            "no_synthetic_user_turn":true,"selected_context_readonly":true,
            "source_workspace_changed_without_changing_report":true,"real_channel_deliveries":0
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_identical_channel_messages_have_distinct_native_ids_and_gateway_acks() {
    let provider = Provider::new(
        "goose-channel-identical-acks",
        vec![
            send(USER),
            send(USER).after("accepted"),
            command("printf channel-two-confirmed > evidence.txt; cat evidence.txt")
                .after("accepted"),
            complete("reply")
                .files(&["evidence.txt"])
                .after("channel-two-confirmed"),
        ],
    );
    let gateway = Gateway::new(&provider, Delivery::Ack);
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    let plugin = install_resource_plugin(&host, &gateway.descriptor);
    let server = host.serve(&provider);
    let run = ask(&server, "goose-channel-identical");
    provider.assert_consumed();
    let requests = gateway.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(provider.effects().len(), 2);
    assert_ne!(requests[0]["request_id"], requests[1]["request_id"]);
    assert_eq!(requests[0]["content"], requests[1]["content"]);
    let history = host.native_conversation(&run, "worker", 1);
    let sends = native_sends(&history);
    assert_eq!(sends.len(), 2, "missing native send history: {history}");
    assert_ne!(sends[0]["id"], sends[1]["id"]);
    let fact = host.native_fact(&run, "worker", 1);
    let key: InvocationKey = serde_json::from_value(fact["key"].clone()).unwrap();
    let session = fact["session_id"].as_str().unwrap();
    for (native, request) in sends.iter().zip(&requests) {
        let native_id = native["id"].as_str().unwrap();
        assert_eq!(
            request["request_id"],
            expected_request_id(&key, session, native_id)
        );
        let response = history
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["content"].as_array().unwrap())
            .find(|block| block["type"] == "toolResponse" && block["id"] == native["id"])
            .expect("missing actual Goose send response");
        assert_eq!(response["toolResult"]["status"], "success");
        assert!(
            response
                .to_string()
                .contains(request["request_id"].as_str().unwrap())
        );
        assert!(response.to_string().contains("accepted"));
    }
    let first = &provider.requests()[0];
    let schema = tool_definition(first, SEND_TOOL).unwrap();
    assert_eq!(
        schema["function"]["parameters"]["properties"]
            .as_object()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        schema["function"]["parameters"]["additionalProperties"],
        false
    );
    assert_no_legacy_attempts(&host);
    assert_artifact(&host, &run, b"channel-two-confirmed");
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":CASE_SOURCE,"pure_resource_plugin":plugin,
            "identical_bodies_distinct_native_ids":true,"gateway_requests":requests,
            "native_send_requests":sends,"request_id_bound_to_fact_and_goose_ids":true,
            "gateway_acks_observed":2,"real_channel_deliveries":0,"no_harness_attempts":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_unauthorized_channel_recipient_is_rejected_before_gateway_connection() {
    let provider = Provider::new(
        "goose-channel-unauthorized",
        vec![
            send("@all"),
            send("fixture-other-user").after("recipient is not allowed"),
            command("printf channel-unauthorized-rejected > evidence.txt; cat evidence.txt")
                .after("recipient is not allowed"),
            complete("reply")
                .files(&["evidence.txt"])
                .after("channel-unauthorized-rejected"),
        ],
    );
    let gateway = Gateway::new(&provider, Delivery::Ack);
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    let server = host.serve(&provider);
    let run = ask(&server, "goose-channel-unauthorized");
    provider.assert_consumed();
    assert!(gateway.requests().is_empty());
    assert!(provider.effects().is_empty());
    for request in &provider.requests()[1..3] {
        assert!(
            tool_feedback(request).last().unwrap()["content"]
                .to_string()
                .contains("recipient is not allowed")
        );
    }
    assert_eq!(
        native_sends(&host.native_conversation(&run, "worker", 1)).len(),
        2
    );
    assert_artifact(&host, &run, b"channel-unauthorized-rejected");
    assert_no_legacy_attempts(&host);
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":CASE_SOURCE,"broadcast_and_other_user_rejected":true,
            "gateway_requests":gateway.requests(),"gateway_deliveries":0,"real_channel_deliveries":0
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_unknown_channel_result_resumes_same_session_and_inspects_without_resending() {
    let provider = Provider::new("goose-channel-unknown-recovery", vec![send(USER)]);
    let business = provider.serve_effects();
    let gateway = Gateway::new(&provider, Delivery::Unknown);
    let host = gateway.bind_host(
        Host::new(&graph())
            .default_runtime()
            .with_allowed_commands("sh,cat,curl,true"),
    );
    let mut server = host.serve(&provider);
    // The send is never acknowledged, so the message's reply never arrives;
    // the Run is observed through the Host's facts instead.
    let pending = {
        let url = format!("{}/channels/fixture/events", server.url);
        thread::spawn(move || {
            let _ = reqwest::blocking::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap()
                .post(url)
                .json(&event("goose-channel-unknown"))
                .send();
        })
    };
    gateway.wait_received(1);
    let run = assistant_run(&host);
    let before = host.native_fact(&run, "worker", 1);
    assert_eq!(before["tool_observation"]["tool"], SEND_TOOL);
    assert!(before["tool_observation"]["result"].is_null());
    assert!(!before["tool_observation"]["native_tool_call"].is_null());
    let session = before["session_id"].clone();
    let gateway_requests = gateway.requests();
    assert_eq!(gateway_requests.len(), 1);
    fs::write(
        provider.root.join("before-kill-anchor.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    server.kill();
    drop(server);
    pending.join().unwrap();
    assert_eq!(provider.requests().len(), 1);
    provider.append(vec![
        complete("reply").files(&["evidence.txt"]),
        command(&format!("set -eu; curl --silent --show-error --fail --max-time 5 --noproxy '*' '{}/effects' > evidence.txt; cat evidence.txt", business.url)).after("final_result.observed_receipt must match"),
        complete("reply").files(&["evidence.txt"]).after("effect_count"),
    ]);
    let restarted = host.serve(&provider);
    let (status, accepted) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{accepted}");
    answered(&host, &run, 1);
    provider.assert_consumed();
    assert_eq!(gateway.requests(), gateway_requests);
    assert_eq!(provider.effects().len(), 1);
    let fact = host.native_fact(&run, "worker", 1);
    assert_eq!(fact["session_id"], session);
    assert_eq!(host.record(&run)["invocations"]["worker"], 1);
    let report: Value =
        serde_json::from_slice(&host.base.file(&host.record(&run), "worker", "evidence.txt"))
            .unwrap();
    assert_eq!(report["effect_count"], 1);
    assert_eq!(report["effects"], json!(gateway_requests));
    let resumed = &provider.requests()[1];
    assert!(
        resumed["messages"]
            .to_string()
            .contains("do not blindly repeat")
    );
    assert!(resumed["messages"].to_string().contains(SEND_TOOL));
    let (_, native, _) = host.native_record(&run, "worker", 1);
    assert_eq!(native["resume"], true);
    assert_no_legacy_attempts(&host);
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":CASE_SOURCE,"kill_after_gateway_effect_before_ack":true,
            "same_invocation":true,"same_native_session":true,"gateway_requests":gateway.requests(),
            "completion_without_current_inspection_rejected":true,"external_state_inspected":report,
            "blind_resends":0,"real_channel_deliveries":0
        }),
    );
}
