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
const USER: &str = "fixture-authorized-user";
const CONTENT: &str = "identical fixture message; never delivered to WeCom";
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
                                    assert_eq!(payload["operation"], "send");
                                    assert_eq!(payload["userid"], USER);
                                    assert_eq!(payload["content"], CONTENT);
                                    assert_eq!(payload["token"], TOKEN);
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
        host.with_extra_environment([
            (
                "ANCHOR_CHANNEL_CONTROL_DESCRIPTOR",
                self.descriptor.as_os_str().to_owned(),
            ),
            ("ANCHOR_WECOM_SEND_USERS", USER.into()),
        ])
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

fn graph() -> Value {
    json!({
        "objective":"Goose channel identity through the real Rust Host and private fake gateway",
        "entry":"worker",
        "agents":{"worker":{
            "model":goose::MODEL,
            "instructions":"Use only admitted tools and recipients. An uncertain delivery is not permission to resend; inspect external state before continuing. Write evidence.txt and finish with route verify.",
            "network":true
        }},
        "ops":{"verify":{"run":"sh -c 'set -eu; cat /in/worker/evidence.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["wecom"]},{"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })
}

fn install_resource_plugin(host: &Host) -> Value {
    let bundle = host.base.root.path().join("bundle");
    let plugin = bundle.join("plugins/wecom");
    fs::create_dir_all(plugin.join("skills/channel")).unwrap();
    fs::write(plugin.join("plugin.json"), json!({
        "name":"WeCom resource-only fixture",
        "description":"Host-owned channel tools without an external MCP process or real messages.",
        "skills":"./skills"
    }).to_string()).unwrap();
    fs::write(plugin.join("skills/channel/SKILL.md"),
        "---\nname: fixture-channel\ndescription: Use host-scoped channel sends.\n---\nUse wecom_send_message for authorized users. Never automatically resend a cancelled or unconfirmed delivery. Inspect external state first.\n").unwrap();
    let binding = FilePluginCatalog::new(&bundle)
        .resolve(&["wecom".into()])
        .unwrap()
        .remove(0);
    let summary = json!({"id":binding.id,"digest":binding.digest,"resources":binding.resources,"mcp_servers":binding.mcp_servers});
    assert_eq!(summary["mcp_servers"], json!([]));
    assert!(!plugin.join(".mcp.json").exists());
    fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[summary]}).to_string(),
    )
    .unwrap();
    summary
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
    assert_eq!(host.base.file(&record, "verify", "verified.txt"), expected);
}

fn assert_no_legacy_attempts(host: &Host) {
    let state = host.base.root.path().join("state");
    assert!(!state.join("io-harness").exists());
    assert!(!state.join("harness").exists());
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
            complete("verify").after("channel-two-confirmed"),
            Step::text("two distinct native sends confirmed"),
        ],
    );
    let gateway = Gateway::new(&provider, Delivery::Ack);
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    let plugin = install_resource_plugin(&host);
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
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
            complete("verify").after("channel-unauthorized-rejected"),
            Step::text("unauthorized channel sends were rejected"),
        ],
    );
    let gateway = Gateway::new(&provider, Delivery::Ack);
    let host = gateway.bind_host(Host::new(&graph()).default_runtime());
    install_resource_plugin(&host);
    let server = host.serve(&provider);
    let run = server.trigger();
    server.wait_status(&run, "completed");
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
    let gateway = Gateway::new(&provider, Delivery::Unknown);
    let host = gateway.bind_host(
        Host::new(&graph())
            .default_runtime()
            .with_allowed_commands("sh,cat,curl,true"),
    );
    install_resource_plugin(&host);
    let mut server = host.serve(&provider);
    let run = server.trigger();
    gateway.wait_received(1);
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
    assert_eq!(provider.requests().len(), 1);
    provider.append(vec![
        complete("verify"),
        command(&format!("curl --silent --show-error --fail --max-time 5 --noproxy '*' '{}/effects' > evidence.txt; cat evidence.txt", provider.url)).after("after observing business results"),
        complete("verify").after("effect_count"),
        Step::text("inspected unconfirmed delivery and did not resend"),
    ]);
    let restarted = host.serve(&provider);
    let (status, accepted) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{accepted}");
    restarted.wait_status(&run, "completed");
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
    assert_eq!(
        host.base.file(&host.record(&run), "worker", "evidence.txt"),
        host.base.file(&host.record(&run), "verify", "verified.txt")
    );
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
