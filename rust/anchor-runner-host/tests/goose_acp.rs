#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[path = "support/goose_fixture.rs"]
#[allow(dead_code)]
mod goose;

use goose::{Gate, Host, Provider, Step, command, complete, tool_definition, tool_feedback};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;

fn graph() -> Value {
    json!({
        "objective":"real Goose native loop with deterministic local transport", "entry":"worker",
        "agents":{"worker":{"model":"fixture-goose","instructions":"Use the provided Anchor tools. Write evidence.txt, inspect tool feedback, then final_result with route verify.","wall_time_limit_seconds":40}},
        "ops":{
            "verify":{"run":"sh -c 'set -eu; cat /in/worker/evidence.txt > verified.txt'"},
            "unused":{"run":"sh -c 'printf wrong-branch > wrong.txt'"}
        },
        "nodes":[{"id":"worker","agent":"worker"},{"id":"verify","op":"verify"},{"id":"unused","op":"unused"}],
        "edges":[{"from":"worker","to":"verify"},{"from":"worker","to":"unused"}]
    })
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_direct_model_transport_requires_observed_tool_receipt() {
    let provider = Provider::new(
        "goose-native-direct-receipt",
        vec![
            command("printf 'native-goose-evidence\\n' > evidence.txt; cat evidence.txt"),
            complete("verify").after("native-goose-evidence"),
            Step::text("native task complete"),
        ],
    );
    let mut definition = graph();
    definition["agents"]["worker"]
        .as_object_mut()
        .unwrap()
        .remove("wall_time_limit_seconds");
    let host = Host::new(&definition).default_runtime();
    let server = host.serve(&provider);
    let run = server.trigger();
    let response = server.wait_status(&run, "completed");
    assert_eq!(response["state"]["status"], "completed", "{response}");
    provider.assert_consumed();
    let saved = host.record(&run);
    assert_artifact(
        &host,
        &saved,
        "worker",
        "evidence.txt",
        b"native-goose-evidence\n",
    );
    assert_artifact(
        &host,
        &saved,
        "verify",
        "verified.txt",
        b"native-goose-evidence\n",
    );
    assert_eq!(provider.requests().len(), 3);
    for request in provider.requests() {
        assert_eq!(request["model"], goose::MODEL);
        assert!(tool_definition(&request, "final_result").unwrap()["function"]["parameters"]["properties"]["observed_receipt"].is_object());
    }
    let (fact, native, _) = host.native_record(&run, "worker", 1);
    assert_eq!(fact["version"], 2);
    assert!(
        fact["model_binding"]
            .as_str()
            .is_some_and(|value| value.len() == 64)
    );
    assert_eq!(native["runtime"], "goose");
    assert!(native["fixture_transport"].is_null());
    assert_eq!(native["budget_enforcement"], false);
    assert!(native["provider_requests"].is_null());
    assert_eq!(native["observed_model_requests"], 3);
    // The usage tally observes the custom notifications as they arrive, so it
    // must agree with the count taken from the retained notification tail and
    // expose the structured accounting alongside it.
    let usage = &native["usage"];
    assert_eq!(usage["messages"], 3, "{usage}");
    assert_eq!(
        usage["samples"].as_array().map(Vec::len),
        Some(3),
        "{usage}"
    );
    assert!(usage["total_tokens"].as_u64().is_some(), "{usage}");
    assert!(usage["elapsed_ms"].as_u64().is_some(), "{usage}");
    assert_eq!(native["compaction_messages"], 0, "{native}");
    let (status, detail) = server.request("GET", &format!("/runs/{run}"), None);
    assert_eq!(status, 200, "{detail}");
    let trace = detail["traces"]["[\"worker\",1]"]
        .as_array()
        .expect("Goose trace projection");
    assert!(
        trace
            .iter()
            .any(|message| message["commands"].to_string().contains("anchor_run"))
    );
    assert!(trace.iter().any(|message| {
        message["role"] == "tool"
            && message["text"]
                .as_str()
                .is_some_and(|text| text.contains("native-goose-evidence"))
    }));
    assert!(
        trace
            .iter()
            .any(|message| message["text"] == "native task complete")
    );
    assert_eq!(provider.requests().len(), 3);
    assert!(
        native
            .to_string()
            .find("fixture-only-not-a-secret")
            .is_none()
    );
    host.evidence(&provider, &run, json!({"native_direct_model_transport":true,"receipt_bound_completion":true,"no_arbitrary_wall_time":true,"default_runtime":true,"public_goose_trace":true}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_disclosed_tools_round_trip_keeps_the_completion_contract() {
    // With on-demand disclosure the model sees Anchor's own tools plus the two
    // disclosure tools, and a call made through them still reaches the bridge, so
    // the receipt-bound completion contract is unchanged.
    let provider = Provider::new(
        "goose-native-disclosure",
        vec![
            Step::tool("anchor_tools", json!({"query": "run"})),
            Step::tool(
                "anchor_tools_call",
                json!({
                    "name": "anchor_run",
                    "arguments": {"command": ["sh", "-c", "printf 'disclosed' > evidence.txt"]}
                }),
            )
            .after("anchor_run"),
            complete("verify").after("anchor_receipt"),
            Step::text("disclosure complete"),
        ],
    );
    let host = Host::new(&graph())
        .native()
        .with_extra_environment([("ANCHOR_NODE_TOOL_DISCLOSURE", "1")]);
    let server = host.serve(&provider);
    let run = server.trigger();
    let response = server.wait_status(&run, "completed");
    assert_eq!(response["state"]["status"], "completed", "{response}");
    provider.assert_consumed();
    let requests = provider.requests();
    let names = requests
        .iter()
        .flat_map(|request| {
            request["tools"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|tool| tool["function"]["name"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let mut surface = names.clone();
    surface.sort_unstable();
    surface.dedup();
    assert_eq!(
        surface,
        [
            "anchor__anchor_edit",
            "anchor__anchor_read",
            "anchor__anchor_run",
            "anchor__anchor_tools",
            "anchor__anchor_tools_call",
            "anchor__final_result"
        ],
        "disclosure keeps Anchor's own tools and the two disclosure tools, nothing else"
    );
    let saved = host.record(&run);
    assert_artifact(&host, &saved, "verify", "verified.txt", b"disclosed");
    let history = host.native_conversation(&run, "worker", 1);
    assert_native_tool_response(&history, "anchor__anchor_tools", "anchor_run");
    assert_native_tool_response(&history, "anchor__anchor_tools_call", "exit_code");
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":"tests/goose_acp.rs",
            "disclosure_tools_advertised":true,
            "invoke_reached_the_bridge":true,
            "receipt_bound_completion_after_indirection":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_turn_context_carries_the_host_boundary() {
    // The host states the node boundary every turn through Goose's persistent
    // instructions, so the agent learns its limits instead of discovering them.
    let provider = Provider::new(
        "goose-native-boundary",
        vec![
            command("printf 'boundary-check' > evidence.txt"),
            complete("verify"),
            Step::text("boundary complete"),
        ],
    );
    let host = Host::new(&graph()).native();
    let server = host.serve(&provider);
    let run = server.trigger();
    let response = server.wait_status(&run, "completed");
    assert_eq!(response["state"]["status"], "completed", "{response}");
    provider.assert_consumed();
    let boundary = provider
        .requests()
        .into_iter()
        .filter(|request| request.to_string().contains("节点边界（宿主声明"))
        .collect::<Vec<_>>();
    assert!(
        boundary.len() >= 2,
        "the boundary must ride every turn, saw {} of {} requests",
        boundary.len(),
        provider.requests().len()
    );
    for (index, request) in boundary.iter().enumerate() {
        let text = request.to_string();
        assert!(text.contains("/workspace 可写"), "turn {index}: {text}");
        assert!(text.contains("需要用户明确授权"), "turn {index}: {text}");
        assert!(text.contains("anchor_run"), "turn {index}: {text}");
        let isolated = text.contains("没有外网访问");
        let shared = text.contains("共享宿主网络");
        assert!(
            isolated ^ shared,
            "turn {index} must state exactly one network mode: {text}"
        );
    }
    let saved = host.record(&run);
    assert_artifact(&host, &saved, "worker", "evidence.txt", b"boundary-check");
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":"tests/goose_acp.rs",
            "boundary_injected_every_turn":true,
            "network_mode_stated_exactly_once":true,
            "host_authorization_rule_stated":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_file_tools_read_and_edit_through_the_bridge() {
    // One variable: the structured file tools. Everything else matches the
    // existing native scenario, and the verify node proves the edit reached the
    // real workspace rather than a model-side copy.
    let provider = Provider::new(
        "goose-native-file-tools",
        vec![
            // `after` asserts what the *previous* tool result showed the model.
            command("printf 'one\\ntwo\\n' > evidence.txt"),
            Step::tool("anchor_read", json!({"path":"evidence.txt"})).after(""),
            Step::tool(
                "anchor_edit",
                json!({"path":"evidence.txt","old_string":"one","new_string":"ONE"}),
            )
            .after("two"),
            command("grep -c ONE evidence.txt").after("sha256_after"),
            complete("verify").after("1"),
            Step::text("file tools complete"),
        ],
    );
    let host = Host::new(&graph()).native();
    let server = host.serve(&provider);
    let run = server.trigger();
    let response = server.wait_status(&run, "completed");
    assert_eq!(response["state"]["status"], "completed", "{response}");
    provider.assert_consumed();
    let saved = host.record(&run);
    // The edit reached the real workspace: the verify node copies the edited file.
    assert_artifact(&host, &saved, "verify", "verified.txt", b"ONE\ntwo\n");
    let history = host.native_conversation(&run, "worker", 1);
    assert_native_tool_response(&history, "anchor__anchor_read", "two");
    assert_native_tool_response(&history, "anchor__anchor_edit", "sha256_after");
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":"tests/goose_acp.rs",
            "structured_file_tools_advertised":true,
            "read_returned_numbered_lines_and_hash":true,
            "edit_reached_the_real_workspace":true,
            "downstream_node_saw_the_edited_file":true,
            "receipt_bound_completion":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_unknown_external_effect_reopens_same_session_and_checks_before_continuing() {
    let provider = Provider::new("goose-native-unknown-effect-resume", vec![]);
    let payload = json!({"effect_key":"native-effect-once"});
    provider.append(vec![
        Step::tool("anchor_run", json!({"command":["sh","-c",format!(
            "curl --silent --show-error --fail --max-time 5 --noproxy '*' --request POST --header 'Content-Type: application/json' --data '{}' '{}/effects'; printf ready > effect-ready; sleep 60",
            payload, provider.url
        )]})),
        complete("verify"),
        Step::tool("anchor_run", json!({"command":["sh","-c",format!(
            "curl --silent --show-error --fail --max-time 5 --noproxy '*' '{}/effects' > evidence.txt; cat evidence.txt", provider.url
        )]})).after("after observing business results"),
        complete("verify").after("effect_count"),
        Step::text("checked external state and completed without repeating POST"),
    ]);
    let mut definition = graph();
    definition["agents"]["worker"]["network"] = json!(true);
    definition["agents"]["worker"]
        .as_object_mut()
        .unwrap()
        .remove("wall_time_limit_seconds");
    let host = Host::new(&definition)
        .native()
        .with_allowed_commands("sh,cat,curl,sleep,true");
    let mut server = host.serve(&provider);
    let run = server.trigger();
    fixture::wait_until("external effect before native ToolResponse", || {
        provider.effects().len() == 1
    });
    let before = host.native_conversation(&run, "worker", 1);
    assert!(
        !before.to_string().contains("toolResponse"),
        "must kill before a ToolResponse is saved: {before}"
    );
    let before_fact = host.native_fact(&run, "worker", 1);
    let session = before_fact["session_id"].clone();
    assert_eq!(before_fact["tool_observation"]["tool"], "anchor_run");
    assert!(
        before_fact["tool_observation"]["arguments"]
            .to_string()
            .contains("native-effect-once")
    );
    assert!(before_fact["tool_observation"]["result"].is_null());
    fs::write(
        provider.root.join("before-kill-anchor.json"),
        serde_json::to_vec_pretty(&before_fact).unwrap(),
    )
    .unwrap();
    fs::write(
        provider.root.join("before-kill-native.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    server.kill();
    drop(server);
    assert_eq!(provider.effects(), vec![payload.clone()]);
    assert_eq!(provider.requests().len(), 1);
    let restarted = host.serve(&provider);
    let (status, accepted) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{accepted}");
    restarted.wait_status(&run, "completed");
    provider.assert_consumed();
    assert_eq!(provider.effects(), vec![payload]);
    assert_eq!(host.native_fact(&run, "worker", 1)["session_id"], session);
    let saved = host.record(&run);
    assert_eq!(saved["invocations"]["worker"], 1);
    let report: Value =
        serde_json::from_slice(&host.base.file(&saved, "worker", "evidence.txt")).unwrap();
    assert_eq!(report["effect_count"], 1);
    assert_eq!(
        host.base.file(&saved, "verify", "verified.txt"),
        host.base.file(&saved, "worker", "evidence.txt")
    );
    let resumed = &provider.requests()[1];
    assert!(
        resumed["messages"]
            .to_string()
            .contains("do not blindly repeat")
    );
    assert!(
        resumed["messages"]
            .to_string()
            .contains("native-effect-once")
    );
    let (_, native, _) = host.native_record(&run, "worker", 1);
    assert_eq!(native["resume"], true);
    assert!(
        native["restored_history"]
            .as_array()
            .is_some_and(|events| !events.is_empty())
    );
    host.evidence(&provider, &run, json!({"kill_after_effect_before_tool_response":true,"same_invocation":true,"same_native_session":true,"effect_count":1,"agent_checked_before_continuing":true,"real_model_requests":0}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_rejects_guessed_receipt_then_accepts_observed_result() {
    let provider = Provider::new(
        "goose-native-guessed-receipt",
        vec![
            command("printf native > evidence.txt; cat evidence.txt"),
            Step::tool(
                "final_result",
                json!({"summary":"must reject","route":"verify","observed_receipt":"guessed-before-tool-result"}),
            ),
            complete("verify").after("after observing business results"),
            Step::text("finished after correcting receipt"),
        ],
    );
    let host = Host::new(&graph()).native();
    let response = host.run(&provider);
    assert_eq!(response["status"], "completed", "{response}");
    provider.assert_consumed();
    let (_, native, _) = host.native_record("fixture", "worker", 1);
    assert_eq!(native["tool_calls"][1]["result"]["ok"], false);
    assert_eq!(native["tool_calls"][2]["result"]["ok"], true);
    host.evidence(
        &provider,
        "fixture",
        json!({"guessed_receipt_rejected":true,"observed_receipt_accepted":true}),
    );
}

fn assert_catalog(request: &Value) {
    let run = tool_definition(request, "anchor_run").unwrap();
    assert_eq!(
        run["function"]["parameters"]["properties"]["command"]["type"],
        "array"
    );
    let final_result = tool_definition(request, "final_result").unwrap();
    let parameters = &final_result["function"]["parameters"];
    assert!(
        parameters["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "summary")
    );
    assert_eq!(parameters["properties"]["summary"]["type"], "string");
    assert_eq!(parameters["properties"]["summary"]["minLength"], 1);
    let routes = parameters["properties"]["route"]["enum"]
        .as_array()
        .unwrap();
    assert!(routes.iter().any(|route| route == "verify"));
    assert!(routes.iter().any(|route| route == "unused"));
    assert!(!routes.iter().any(|route| route == "unknown"));
    let mut names = request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "anchor__anchor_edit",
            "anchor__anchor_read",
            "anchor__anchor_run",
            "anchor__final_result"
        ],
        "model tools must be exactly the Anchor tools, with nothing from Goose's own extensions"
    );
}

fn assert_artifact(host: &Host, saved: &Value, node: &str, name: &str, expected: &[u8]) {
    assert_eq!(host.base.file(saved, node, name), expected);
    let manifest = fixture::read_json(host.base.artifact(saved, node).join("manifest.json"));
    assert_eq!(
        manifest["files"][name]["sha256"],
        format!("{:x}", Sha256::digest(expected))
    );
    assert_eq!(manifest["files"][name]["bytes"], expected.len());
}

fn assert_native_record(host: &Host, provider: &Provider, run: &str, completed: bool) {
    let (fact, native, process) = host.native_record(run, "worker", 1);
    assert_eq!(fact["binary_sha256"], goose::GOOSE_SHA256);
    assert_eq!(native["binary_sha256"], goose::GOOSE_SHA256);
    assert_eq!(native["initialize"]["agentInfo"]["version"], "1.53.0");
    assert_eq!(native["session"]["_meta"]["workingDir"], "/workspace");
    assert_eq!(native["provider_requests"], provider.requests().len());
    assert!(native["real_model_calls"].is_null());
    assert_eq!(native["fixture_transport"], "loopback");
    let controlled = fixture::read_json(provider.root.join("provider.json"));
    assert_eq!(controlled["real_model_requests"], 0);
    assert_eq!(controlled["requests"], json!(provider.requests()));
    assert!(
        provider
            .requests()
            .iter()
            .all(|request| request["model"] == goose::MODEL)
    );
    assert_eq!(fact["completion"].is_object(), completed);
    if completed {
        assert_eq!(
            fact["completion"]["model_requests"],
            provider.requests().len()
        );
        assert_eq!(fact["completion"]["route"], "verify");
    }
    assert!(
        process.join("data/sessions/sessions.db").is_file(),
        "missing native Goose session history under isolated process data root"
    );
    assert!(process.join("config/config.yaml").is_file());
    let extensions = native["session"]["_meta"]["extensionResults"]
        .as_array()
        .unwrap();
    // Exactly the Anchor MCP server plus Goose's builtin per-turn context provider
    // (`tom`, listed in `enabledExtensions`); anything else loading here is a
    // regression in the session's extension surface.
    let mut loaded = extensions
        .iter()
        .map(|extension| {
            assert_eq!(extension["success"], true, "{extension}");
            extension["name"].as_str().unwrap()
        })
        .collect::<Vec<_>>();
    loaded.sort_unstable();
    assert_eq!(
        loaded,
        ["anchor", "tom"],
        "only Anchor MCP and the builtin context provider may load"
    );
    let conversation = host.native_conversation(run, "worker", 1);
    let (_, run_response) =
        assert_native_tool_response(&conversation, "anchor__anchor_run", "fixture-written");
    let output = native_response_value(&run_response);
    assert_eq!(output[0]["value"]["exit_code"], 0);
    assert_eq!(output[0]["value"]["stdout"], "fixture-written");
    if completed {
        let (request, response) =
            assert_native_tool_response(&conversation, "anchor__final_result", "fixture complete");
        assert_eq!(request["toolCall"]["value"]["arguments"]["route"], "verify");
        assert_eq!(
            native_response_value(&response),
            json!({"summary":"fixture complete","route":"verify"})
        );
    }
    for request in provider.requests() {
        assert_eq!(request["parallel_tool_calls"], false);
    }
}

fn native_response_value(response: &Value) -> Value {
    let contents = response["toolResult"]["value"]["content"]
        .as_array()
        .expect("native tool response has no actual MCP content");
    let text = contents
        .iter()
        .filter_map(|content| content["text"].as_str())
        .collect::<String>();
    serde_json::from_str(&text)
        .expect("native Anchor tool response must contain its actual JSON result")
}

fn assert_native_tool_response(conversation: &Value, name: &str, expected: &str) -> (Value, Value) {
    let blocks = conversation
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .collect::<Vec<_>>();
    for request in blocks.iter().filter(|block| {
        block["type"] == "toolRequest" && block["toolCall"]["value"]["name"] == name
    }) {
        assert_eq!(request["toolCall"]["status"], "success");
        if let Some(response) = blocks.iter().find(|block| {
            block["type"] == "toolResponse"
                && block["id"] == request["id"]
                && block["toolResult"]["status"] == "success"
                && block["toolResult"]["value"]["isError"] != true
                && block["toolResult"]["value"]["content"]
                    .to_string()
                    .contains(expected)
        }) {
            return ((**request).clone(), (**response).clone());
        }
    }
    panic!(
        "native Goose SQLite has no matched real ToolRequest/ToolResponse for {name} containing {expected:?}: {conversation}"
    );
}

#[test]
fn provider_selects_actual_namespaced_schema_and_rejects_ambiguous_suffixes() {
    let schema = json!({"tools":[{"type":"function","function":{"name":"anchor__anchor_run","parameters":{"required":["command"]}}}]});
    assert_eq!(
        tool_definition(&schema, "anchor_run").unwrap()["function"]["name"],
        "anchor__anchor_run"
    );
    assert!(tool_definition(&schema, "final_result").is_err());
    assert!(tool_definition(&json!({"tools":[{"function":{"name":"anchor_run"}},{"function":{"name":"anchor__anchor_run"}}]}), "anchor_run").is_err());
}

#[test]
fn provider_streams_fragmented_arguments_usage_and_records_real_tool_feedback() {
    let provider = Provider::new(
        "provider-wire",
        vec![
            command("printf test"),
            Step::text("done").after("fixture-feedback"),
        ],
    );
    let request = json!({"model":"fixture-goose","stream":true,"messages":[{"role":"user","content":"test"}],"tools":[{"type":"function","function":{"name":"anchor__anchor_run","parameters":{"type":"object","properties":{"command":{"type":"array","items":{"type":"string"}}}}}}]});
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .post(format!("{}/v1/chat/completions", provider.url))
            .json(&request)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        let stream = response.text().await.unwrap();
        let chunks = stream
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|payload| *payload != "[DONE]")
            .map(|payload| serde_json::from_str::<Value>(payload).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            chunks[1]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "anchor__anchor_run"
        );
        let arguments = chunks
            .iter()
            .filter_map(|chunk| {
                chunk["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .collect::<String>();
        assert_eq!(
            serde_json::from_str::<Value>(&arguments).unwrap(),
            json!({"command":["sh","-c","printf test"]})
        );
        assert_eq!(chunks.last().unwrap()["usage"]["total_tokens"], 18);
        assert!(stream.ends_with("data: [DONE]\n\n"));
        let mut next = request.clone();
        next["messages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"role":"tool","tool_call_id":"call-1","content":"fixture-feedback"}));
        assert_eq!(
            client
                .post(format!("{}/v1/chat/completions", provider.url))
                .json(&next)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    });
    provider.assert_consumed();
    assert_eq!(tool_feedback(&provider.requests()[1]).len(), 1);
    let saved = fixture::read_json(provider.root.join("provider.json"));
    assert_eq!(saved["real_model_requests"], 0);
    assert!(saved["exchanges"].to_string().contains("fixture-feedback"));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn real_goose_tools_route_artifacts_and_completed_restart_without_provider_replay() {
    let provider = Provider::new(
        "goose-success-reopen",
        vec![
            command(
                "set -eu; printf goose-evidence > evidence.txt; printf once > effects.txt; printf fixture-written",
            ),
            complete("verify").after("fixture-written"),
            Step::text("Fixture finished.").after(""),
        ],
    );
    let host = Host::new(&graph());
    assert_eq!(host.run(&provider)["status"], "completed");
    let saved = host.record("fixture");
    assert_eq!(saved["results"]["worker"].as_array().unwrap().len(), 1);
    assert_eq!(saved["results"]["verify"].as_array().unwrap().len(), 1);
    assert!(saved["results"].get("unused").is_none());
    assert_eq!(saved["results"]["worker"][0]["key"]["invocation"], 1);
    assert_artifact(&host, &saved, "worker", "evidence.txt", b"goose-evidence");
    assert_artifact(&host, &saved, "worker", "effects.txt", b"once");
    assert_artifact(&host, &saved, "verify", "verified.txt", b"goose-evidence");
    assert_eq!(
        host.base.workspace_files("fixture", "evidence.txt"),
        vec![b"goose-evidence".to_vec()]
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_native_record(&host, &provider, "fixture", true);
    for request in &requests {
        assert_catalog(request);
    }
    assert!(
        tool_feedback(&requests[1]).last().unwrap()["content"]
            .to_string()
            .contains("exit_code")
    );
    assert_eq!(host.run(&provider)["status"], "completed");
    assert_eq!(host.record("fixture"), saved);
    assert_eq!(provider.requests(), requests);
    host.evidence(&provider, "fixture", json!({"nodes_recorded":2,"branch":"verify","tool_result_roundtrip":true,"workspace_checked":true,"artifact_hashes_checked":true,"completed_restart_model_requests":0,"tool_effects":1}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn real_goose_invalid_route_feedback_is_corrected_before_branch_commit() {
    let provider = Provider::new(
        "goose-route-correction",
        vec![
            command("printf goose-evidence > evidence.txt; printf fixture-written"),
            complete("unknown").after("fixture-written"),
            complete("verify").after(""),
            Step::text("Corrected fixture finished.").after(""),
        ],
    );
    let host = Host::new(&graph());
    assert_eq!(host.run(&provider)["status"], "completed");
    let saved = host.record("fixture");
    assert!(saved["results"].get("unused").is_none());
    assert_eq!(saved["results"]["worker"].as_array().unwrap().len(), 1);
    assert_artifact(&host, &saved, "verify", "verified.txt", b"goose-evidence");
    let requests = provider.requests();
    assert_eq!(requests.len(), 4);
    assert_native_record(&host, &provider, "fixture", true);
    assert_catalog(&requests[1]);
    let rejection = tool_feedback(&requests[2]).last().unwrap()["content"]
        .to_string()
        .to_lowercase();
    assert!(
        rejection.contains("unknown")
            || rejection.contains("invalid")
            || rejection.contains("error")
            || rejection.contains("not an allowed"),
        "invalid route was not rejected: {rejection}"
    );
    host.evidence(&provider, "fixture", json!({"invalid_route":"unknown","corrected_route":"verify","invalid_branch_commits":0,"invocations":1,"rejection":rejection}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn real_goose_denied_command_and_absolute_path_do_not_escape_sandbox() {
    denied_command_and_absolute_path(false);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_denied_command_and_absolute_path_do_not_escape_sandbox() {
    denied_command_and_absolute_path(true);
}

fn denied_command_and_absolute_path(native: bool) {
    let outside = tempfile::tempdir().unwrap();
    let protected = outside.path().join("protected.txt");
    fs::write(&protected, "operator-data").unwrap();
    let provider = Provider::new(
        if native {
            "goose-native-sandbox-denials"
        } else {
            "goose-sandbox-denials"
        },
        vec![
            Step::tool(
                "anchor_run",
                json!({"command":["touch",protected.to_str().unwrap()]}),
            ),
            command(&format!(
                "printf stolen > '{}'; printf path-denied",
                protected.display()
            ))
            .after(""),
            command("printf goose-evidence > evidence.txt; printf fixture-written").after(""),
            complete("verify").after("fixture-written"),
            Step::text("Sandbox fixture finished.").after(""),
        ],
    );
    let host = Host::new(&graph()).with_allowed_commands("sh,cat,true");
    let host = if native { host.native() } else { host };
    assert_eq!(host.run(&provider)["status"], "completed");
    assert_eq!(fs::read(&protected).unwrap(), b"operator-data");
    let requests = provider.requests();
    assert_eq!(requests.len(), 5);
    if !native {
        assert_native_record(&host, &provider, "fixture", true);
    }
    let denied = tool_feedback(&requests[1]).last().unwrap()["content"]
        .to_string()
        .to_lowercase();
    assert!(
        denied.contains("not_executed") && denied.contains("not authorized"),
        "command was not rejected: {denied}"
    );
    let path_denied = tool_feedback(&requests[2]).last().unwrap()["content"]
        .to_string()
        .to_lowercase();
    assert!(
        path_denied.contains("permission denied")
            || path_denied.contains("read-only")
            || path_denied.contains("nonexistent")
            || path_denied.contains("no such file")
            || path_denied.contains("cannot create"),
        "missing real sandbox path rejection: {path_denied}"
    );
    assert_artifact(
        &host,
        &host.record("fixture"),
        "verify",
        "verified.txt",
        b"goose-evidence",
    );
    host.evidence(&provider, "fixture", json!({"unauthorized_command":"touch","absolute_path_rejected":true,"protected_file_unchanged":true,"command_feedback":denied,"path_feedback":path_denied}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn real_goose_cancel_settles_and_restart_does_not_invoke_provider() {
    cancel_settles_and_restart_does_not_invoke_provider(false);
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_cancel_without_a_wall_time_settles_and_restart_stays_stopped() {
    cancel_settles_and_restart_does_not_invoke_provider(true);
}

fn cancel_settles_and_restart_does_not_invoke_provider(native: bool) {
    let gate = Gate::new();
    let provider = Provider::new(
        if native {
            "goose-native-cancel-restart"
        } else {
            "goose-cancel-restart"
        },
        vec![
            command(
                "printf goose-evidence > evidence.txt; printf once > effects.txt; printf fixture-written",
            ),
            Step::text("Cancelled fixture.")
                .after("fixture-written")
                .gated(&gate),
        ],
    );
    let mut definition = graph();
    if native {
        definition["agents"]["worker"]
            .as_object_mut()
            .unwrap()
            .remove("wall_time_limit_seconds");
    }
    let host = Host::new(&definition);
    let host = if native { host.native() } else { host };
    let server = host.serve(&provider);
    let run = server.trigger();
    gate.wait_entered();
    let (status, accepted) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{accepted}");
    gate.open();
    server.wait_status(&run, "stopped");
    let stopped = host.record(&run);
    assert!(stopped["results"].get("verify").is_none());
    assert!(stopped["results"].get("unused").is_none());
    assert_eq!(
        host.base.workspace_files(&run, "effects.txt"),
        vec![b"once".to_vec()]
    );
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    if native {
        assert!(host.native_fact(&run, "worker", 1)["completion"].is_null());
        assert!(
            host.native_conversation(&run, "worker", 1)
                .to_string()
                .contains("fixture-written")
        );
    } else {
        assert_native_record(&host, &provider, &run, false);
    }
    drop(server);
    let restarted = host.serve(&provider);
    restarted.wait_status(&run, "stopped");
    assert_eq!(host.record(&run), stopped);
    assert_eq!(provider.requests(), requests);
    host.evidence(&provider, &run, json!({"stop_requested":true,"stop_settled":true,"downstream_started":false,"restart_requests":0,"tool_effects":1}));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn real_goose_business_after_final_result_is_rejected_without_commit() {
    let provider = Provider::new(
        "goose-post-final-rejection",
        vec![
            command("printf goose-evidence > evidence.txt; printf fixture-written"),
            complete("verify").after("fixture-written"),
            command("printf forbidden > after-final.txt").after("fixture complete"),
            Step::text("Post-completion business call was refused.")
                .after("tools after node completion are refused"),
        ],
    );
    let host = Host::new(&graph());
    assert_eq!(host.run(&provider)["status"], "failed");
    let saved = host.record("fixture");
    assert!(saved["error"].as_str().unwrap().contains("final_result"));
    assert!(saved["results"].get("worker").is_none());
    assert!(saved["results"].get("verify").is_none());
    assert!(
        host.base
            .workspace_files("fixture", "after-final.txt")
            .is_empty()
    );
    assert_eq!(provider.requests().len(), 4);
    assert_native_record(&host, &provider, "fixture", false);
    for request in provider.requests() {
        assert_catalog(&request);
    }
    let conversation = host.native_conversation("fixture", "worker", 1);
    assert_native_tool_response(&conversation, "anchor__final_result", "fixture complete");
    let (_, native, _) = host.native_record("fixture", "worker", 1);
    let calls = native["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2]["tool"], "anchor_run");
    assert_eq!(calls[2]["result"]["ok"], false);
    assert_eq!(
        calls[2]["result"]["error"],
        "tools after node completion are refused"
    );
    host.evidence(
        &provider,
        "fixture",
        json!({
            "post_final_business_refused":true,"provider_requests":4,
            "after_final_effects":0,"committed_agent_results":0,"downstream_started":false,
            "parallel_tool_calls_false_is_not_security_proof":true
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn real_goose_external_effect_then_host_kill_is_spike_failclosed_not_native_safe_recovery() {
    let gate = Gate::new();
    let provider = Provider::new("goose-external-effect-kill-failclosed", vec![]);
    let payload = json!({"effect_key":"fixture-external-once"});
    provider.append(vec![
        Step::tool(
            "anchor_run",
            json!({"command":[
                "curl","--silent","--show-error","--fail","--max-time","5","--noproxy","*",
                "--request","POST","--header","Content-Type: application/json",
                "--data",payload.to_string(),format!("{}/effects",provider.url)
            ]}),
        ),
        Step::text("Unfinished fixture response.")
            .after("external_effect")
            .gated(&gate),
    ]);
    let mut definition = graph();
    definition["agents"]["worker"]["network"] = json!(true);
    let host = Host::new(&definition).with_allowed_commands("curl,sh,cat,true");
    let mut server = host.serve(&provider);
    let run = server.trigger();
    gate.wait_entered();
    assert_eq!(provider.effects(), vec![payload.clone()]);
    assert_eq!(provider.requests().len(), 2);
    for request in provider.requests() {
        assert_catalog(&request);
    }
    let before = host.native_conversation(&run, "worker", 1);
    let (request, response) =
        assert_native_tool_response(&before, "anchor__anchor_run", "external_effect");
    assert_eq!(
        request["toolCall"]["value"]["arguments"]["command"][0],
        "curl"
    );
    let result = native_response_value(&response);
    assert_eq!(result[0]["value"]["exit_code"], 0);
    let external_result: Value =
        serde_json::from_str(result[0]["value"]["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(
        external_result,
        json!({"external_effect":"once","effect_count":1})
    );
    let fact = host.native_fact(&run, "worker", 1);
    assert!(fact["completion"].is_null());
    assert_eq!(fact["binary_sha256"], goose::GOOSE_SHA256);
    fs::write(
        provider.root.join("native-conversation-before-kill.json"),
        serde_json::to_vec_pretty(&before).unwrap(),
    )
    .unwrap();
    server.kill();
    drop(server);
    gate.open();
    assert_eq!(host.native_conversation(&run, "worker", 1), before);
    let requests = provider.requests();
    let restarted = host.serve(&provider);
    let (status, detail) = restarted.request("GET", &format!("/runs/{run}"), None);
    assert_eq!(status, 200, "{detail}");
    assert_eq!(detail["active"], false);
    assert_ne!(detail["state"]["status"], "completed");
    let (status, accepted) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{accepted}");
    let failed = restarted.wait_status(&run, "failed");
    let reason = failed["state"]["error"].as_str().unwrap();
    assert!(
        reason.contains("Goose") && reason.contains("replay") && reason.contains("refused"),
        "expected explicit spike failclosed limitation: {reason}"
    );
    assert_eq!(provider.requests(), requests);
    assert_eq!(provider.effects(), vec![payload]);
    assert_eq!(host.native_fact(&run, "worker", 1), fact);
    assert_eq!(host.native_conversation(&run, "worker", 1), before);
    let saved = host.record(&run);
    assert!(saved["results"].get("worker").is_none());
    assert!(saved["results"].get("verify").is_none());
    assert!(saved["results"].get("unused").is_none());
    host.evidence(
        &provider,
        &run,
        json!({
            "acceptance":"Anchor Goose spike failclosed only, not Goose native safe recovery",
            "external_fake_effects":1,"blocked_after_effect":true,"host_killed":true,
            "restart_provider_requests":0,"native_conversation_preserved":true,
            "automatic_load_or_replay":false,"safe_native_recovery_proven":false,
            "spike_failure_reason":reason
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn real_goose_missing_shared_network_authorization_rejects_before_any_model_request() {
    let provider = Provider::new("goose-shared-network-not-authorized", vec![]);
    // This negative case is about *sharing* the host network, so pin that mode.
    let host = Host::new(&graph()).with_extra_environment([("ANCHOR_GOOSE_LOCAL_NETWORK", "0")]);
    let response = host.run_without_env(&provider, "ANCHOR_GOOSE_ALLOW_SHARED_NETWORK");
    let reason = response["reason"]
        .as_str()
        .expect("expected Host configuration rejection");
    assert!(reason.contains("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK=1"));
    assert!(provider.requests().is_empty());
    assert!(provider.effects().is_empty());
    assert!(
        !host
            .base
            .root
            .path()
            .join("state/runs/fixture.json")
            .exists()
    );
    assert!(!host.base.root.path().join("work/.goose-process").exists());
    provider.assert_consumed();
    fs::write(
        provider.root.join("evidence.json"),
        serde_json::to_vec_pretty(&json!({
            "status":"passed","runtime":"goose-acp-spike","response":response,
            "goose_binary_sha256":goose::GOOSE_SHA256,"provider_requests":0,"real_model_requests":0,
            "runtime_shared_network_explicitly_authorized":false,
            "shared_network_is_not_loopback_only_os_isolation":true,
            "business_sandbox_network_policy_is_independent":true,
            "graph_run_admitted":false,"goose_process_started":false
        }))
        .unwrap(),
    )
    .unwrap();
    println!(
        "evidence: {}",
        provider.root.join("evidence.json").display()
    );
}
