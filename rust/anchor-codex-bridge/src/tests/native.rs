use super::*;

const MODEL: &str = "gpt-6.1-sol";
const ECHO_TOOL: &str = "anchor_native_echo";

fn check(condition: bool, reason: &'static str) -> Result<(), &'static str> {
    if condition { Ok(()) } else { Err(reason) }
}

async fn native(host: &McpHost, method: &str, params: Value) -> Result<Value, &'static str> {
    eprintln!("native live probe: {method}");
    live_call(
        host,
        "native_call",
        json!({"method": method, "params": params}),
    )
    .await
}

async fn deny_native(host: &McpHost, method: &str, params: Value) -> Result<(), &'static str> {
    let response = host
        .call("native_call", json!({"method": method, "params": params}))
        .await
        .map_err(|_| "rejection probe transport failed (details withheld)")?;
    check(
        response.is_error,
        "native boundary probe unexpectedly succeeded",
    )
}

async fn queued_while_waiting(host: &McpHost, thread: &str) -> Result<(), &'static str> {
    // Native queue/add starts work when idle. Keep the echo request pending
    // until this submission is deleted so the probe cannot add an inference.
    let schema = live_call(
        host,
        "native_discover",
        json!({"direction": "request", "method": "thread/queue/list"}),
    )
    .await?;
    let validator = jsonschema::validator_for(&schema["response_schema"])
        .map_err(|_| "discovered queue/list response schema is invalid")?;
    let queued = native(host, "thread/queue/add", json!({
        "threadId": thread, "clientUserMessageId": format!("native-queue-{thread}"),
        "input": [{"type": "text", "text": "Queue fixture to delete before execution; do not use tools, commands, files, network or subagents.", "text_elements": []}]
    })).await?;
    let queued_id = queued["queuedSubmission"]["id"]
        .as_str()
        .ok_or("native queue/add returned no id")?;
    let queue = native(
        host,
        "thread/queue/list",
        json!({"threadId": thread, "limit": 10}),
    )
    .await;
    // Attempt deletion even if listing failed; never release the echo first.
    let deleted = native(
        host,
        "thread/queue/delete",
        json!({"threadId": thread, "queuedSubmissionId": queued_id}),
    )
    .await?;
    check(
        deleted["deleted"] == true,
        "native queue/delete did not delete submission",
    )?;
    let queue = queue?;
    check(
        validator.is_valid(&queue),
        "queue/list response differs from installed schema",
    )?;
    check(
        queue["data"]
            .as_array()
            .is_some_and(|entries| entries.len() == 1 && entries[0] == queued["queuedSubmission"]),
        "native queue did not retain submission while echo was pending",
    )?;
    let empty = native(
        host,
        "thread/queue/list",
        json!({"threadId": thread, "limit": 10}),
    )
    .await?;
    check(
        validator.is_valid(&empty) && empty["data"] == json!([]),
        "native queue was not emptied",
    )?;
    eprintln!("native live probe: queue add/list/delete verified before echo response");
    Ok(())
}

// Poll both the projection and the raw wire stream: a synthesized completion is
// insufficient. Only the explicitly requested dynamic tool may receive a reply.
async fn completed_turn(
    host: &McpHost,
    thread: &str,
    turn: &str,
    expected: &str,
    echo: Option<(&str, &Value)>,
) -> Result<(), &'static str> {
    tokio::time::timeout(Duration::from_secs(90), async {
        let mut cursor = 0;
        let mut epoch = None;
        let mut wire_completed = false;
        let mut wire_request = None;
        let mut answered_request = None;
        loop {
            loop {
                let page = live_call(host, "native_events", json!({
                    "thread_id": thread, "after": cursor, "limit": 100
                })).await?;
                check(page["gap"] == false, "native event stream lost events")?;
                let current_epoch = page["stream_epoch"].as_str().ok_or("native stream has no epoch")?;
                check(epoch.as_deref().is_none_or(|old| old == current_epoch), "native stream epoch changed")?;
                epoch = Some(current_epoch.to_owned());
                for event in page["events"].as_array().ok_or("native events is not an array")? {
                    let params = &event["params"];
                    if event["method"] == "turn/completed" && params["turn"]["id"] == turn {
                        check(params["threadId"] == thread && params["turn"]["status"] == "completed",
                            "raw native turn did not complete successfully")?;
                        wire_completed = true;
                    }
                    if event["method"] == "item/tool/call" && params["turnId"] == turn {
                        let (marker, _) = echo.ok_or("unexpected dynamic tool request in text-only turn")?;
                        check(params["threadId"] == thread && params["tool"] == ECHO_TOOL
                            && params["arguments"] == json!({"marker": marker}), "unexpected raw tool request")?;
                        wire_request = Some(event["request_key"].as_str().ok_or("raw tool request has no key")?.to_owned());
                    }
                    if matches!(event["method"].as_str(), Some("item/started" | "item/completed"))
                        && params["turnId"] == turn
                    {
                        let kind = params["item"]["type"].as_str().unwrap_or_default();
                        check(!matches!(kind, "commandExecution" | "fileChange" | "collabAgentToolCall" | "mcpToolCall" | "webSearch"),
                            "marker-only turn attempted unrelated tools")?;
                    }
                }
                let next = page["next_cursor"].as_u64().ok_or("native stream has no next cursor")?;
                let last = page["last_cursor"].as_u64().ok_or("native stream has no last cursor")?;
                check(next >= cursor && (next > cursor || next == last), "native cursor did not advance")?;
                cursor = next;
                if cursor == last { break; }
            }
            let detail = live_call(host, "inspect", json!({"thread_id": thread})).await?;
            for pending in detail["pending_requests"].as_array().ok_or("inspect has no pending request array")? {
                let (marker, response_schema) = echo.ok_or("text-only turn requires intervention")?;
                let params = &pending["params"];
                check(answered_request.is_none() && pending["method"] == "item/tool/call"
                    && params["threadId"] == thread && params["turnId"] == turn
                    && params["tool"] == ECHO_TOOL && params["arguments"] == json!({"marker": marker}),
                    "unexpected pending request; no approval was granted")?;
                let key = pending["request_key"].as_str().ok_or("pending tool request has no key")?;
                let result = json!({"contentItems": [{"type": "inputText", "text": expected}], "success": true});
                check(jsonschema::is_valid(response_schema, &result), "echo response does not match discovered native schema")?;
                queued_while_waiting(host, thread).await?;
                let sent = live_call(host, "native_respond", json!({
                    "thread_id": thread, "request_key": key, "result": result
                })).await?;
                check(sent["status"] == "response_written" && sent["completed"] == false
                    && sent["request_key"] == key, "native response receipt mismatch")?;
                answered_request = Some(key.to_owned());
            }
            match detail["status"].as_str() {
                Some("completed") if detail["last_turn_id"] == turn => {
                    check(detail["latest_agent_message"].as_str().map(str::trim) == Some(expected),
                        "native marker reply mismatch (response withheld)")?;
                    if wire_completed {
                        check(echo.is_none() || (answered_request.is_some() && answered_request == wire_request),
                            "dynamic response did not correlate with the raw request")?;
                        check(detail["pending_requests"] == json!([]), "completed turn retained pending requests")?;
                        return Ok(());
                    }
                }
                Some("failed" | "interrupted" | "unknown") => return Err("native turn failed or became uncertain (details withheld)"),
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }).await.map_err(|_| "native turn exceeded 90 seconds")?
}

// Uses only temporary repositories and the operator's existing provider. Never
// dump native payloads/history or write global configuration, even on failure.
#[tokio::test]
#[ignore = "requires explicitly authorized local Codex provider access"]
async fn live_codex_native_surface() {
    if std::env::var("ANCHOR_CODEX_BRIDGE_REAL_CODEX").as_deref() != Ok("1") {
        eprintln!("skipped: set ANCHOR_CODEX_BRIDGE_REAL_CODEX=1 to authorize provider access");
        return;
    }
    let fixture = Fixture::start_with(
        PathBuf::from("codex"),
        vec!["app-server".into(), "--stdio".into()],
    )
    .await;
    let host = fixture.connect(Some(&fixture.alpha)).await;
    let initial_head = git(&fixture.alpha, &["rev-parse", "HEAD"]);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let marker = format!("ANCHOR_NATIVE_TEXT_{nonce}");
    let tool_marker = format!("ANCHOR_NATIVE_ECHO_{nonce}");
    let tool_reply = format!("ANCHOR_NATIVE_RECEIPT_{nonce}");
    let mut active_thread = None;
    let outcome = tokio::time::timeout(Duration::from_secs(240), async {
        let schema = live_call(&host, "native_discover", json!({"direction": "request", "method": "thread/start"})).await?;
        check(schema["method"] == "thread/start" && schema["callable"] == true
            && schema["bridge_policy"] == "project_thread", "thread/start discovery mismatch")?;
        let params = json!({
            "cwd": fixture.alpha, "model": MODEL, "sandbox": "read-only",
            "approvalPolicy": "never", "config": {"model_reasoning_effort": "max"},
            "dynamicTools": [{
                "type": "function", "name": ECHO_TOOL, "deferLoading": false,
                "description": "Echo a test marker with a runtime-issued receipt. Return the tool's text verbatim. No commands, files, network or subagents.",
                "inputSchema": {"type": "object", "properties": {"marker": {"type": "string"}},
                    "required": ["marker"], "additionalProperties": false}
            }]
        });
        let validator = jsonschema::validator_for(&schema["params_schema"]).map_err(|_| "discovered thread/start schema is invalid")?;
        check(validator.is_valid(&params), "thread/start parameters do not match installed schema")?;
        for cwd in [&fixture.beta, &fixture.outside] {
            let mut denied = params.clone();
            denied["cwd"] = json!(cwd);
            check(validator.is_valid(&denied), "outside-cwd probe is not schema-valid")?;
            deny_native(&host, "thread/start", denied).await?;
        }
        let mut override_config = params.clone();
        override_config["config"]["sandbox_mode"] = json!("danger-full-access");
        check(validator.is_valid(&override_config), "config probe is not schema-valid")?;
        deny_native(&host, "thread/start", override_config).await?;
        let mut unsafe_sandbox = params.clone();
        unsafe_sandbox["sandbox"] = json!("danger-full-access");
        check(validator.is_valid(&unsafe_sandbox), "sandbox probe is not schema-valid")?;
        deny_native(&host, "thread/start", unsafe_sandbox).await?;

        let started = native(&host, "thread/start", params).await?;
        let id = started["thread"]["id"].as_str().ok_or("native thread/start returned no id")?.to_owned();
        active_thread = Some(id.clone());
        check(started["cwd"] == json!(fixture.alpha) && started["model"] == MODEL
            && started["reasoningEffort"] == "max" && started["sandbox"]["type"] == "readOnly",
            "native thread execution settings mismatch")?;
        let blocked_config = fixture._temp.path().join("blocked-config.toml");
        for (method, denied) in [
            ("config/value/write", json!({"keyPath": "model_reasoning_effort", "value": "max", "mergeStrategy": "replace", "filePath": blocked_config})),
            ("thread/shellCommand", json!({"threadId": id, "command": "printf ANCHOR_NATIVE_BLOCKED"})),
        ] {
            let blocked = live_call(&host, "native_discover", json!({"direction": "request", "method": method})).await?;
            check(blocked["callable"] == false, "host-wide native operation advertised as callable")?;
            check(jsonschema::is_valid(&blocked["params_schema"], &denied), "blocked method probe is not schema-valid")?;
            deny_native(&host, method, denied).await?;
        }
        check(!blocked_config.exists(), "blocked config operation created a file")?;

        let turn_schema = live_call(&host, "native_discover", json!({"direction": "request", "method": "turn/start"})).await?;
        let text_turn = json!({
            "threadId": id, "effort": "max", "approvalPolicy": "never",
            "input": [{"type": "text",
                "text": format!("{marker}\nReply with exactly the marker on the first line. Do not use any tools, commands, files, network or subagents. Do not do any other work."),
                "text_elements": [{"byteRange": {"start": 0, "end": marker.len()}, "placeholder": marker}]
            }]
        });
        check(jsonschema::is_valid(&turn_schema["params_schema"], &text_turn), "rich text input does not match native schema")?;
        let first = native(&host, "turn/start", text_turn).await?;
        let first_id = first["turn"]["id"].as_str().ok_or("native turn/start returned no id")?;
        completed_turn(&host, &id, first_id, &marker, None).await?;
        eprintln!("native live probe: text marker and raw completion verified");

        let tool_schema = live_call(&host, "native_discover", json!({"direction": "server_request", "method": "item/tool/call"})).await?;
        check(tool_schema["response_schema"].is_object(), "dynamic tool response schema missing")?;
        let second = native(&host, "turn/start", json!({
            "threadId": id, "effort": "max", "approvalPolicy": "never",
            "input": [{"type": "text", "text_elements": [],
                "text": format!("Call the dynamic tool {ECHO_TOOL} exactly once now with arguments {{\"marker\":\"{tool_marker}\"}}. Wait for its result, then reply with exactly the text returned by that tool. The runtime receipt is not known until the tool responds; do not invent it. Do not use any other tools, commands, files, network or subagents. Do not do any other work.")} ]
        })).await?;
        let second_id = second["turn"]["id"].as_str().ok_or("second native turn returned no id")?;
        completed_turn(&host, &id, second_id, &tool_reply, Some((&tool_marker, &tool_schema["response_schema"]))).await?;
        eprintln!("native live probe: dynamic tool request/response roundtrip verified");

        let name = format!("Native fixture {nonce}");
        native(&host, "thread/name/set", json!({"threadId": id, "name": name})).await?;
        native(&host, "thread/settings/update", json!({"threadId": id, "model": MODEL, "effort": "max"})).await?;
        let models = native(&host, "model/list", json!({"limit": 10})).await?;
        check(models["data"].is_array(), "native model catalog returned no data array")?;
        let turns = native(&host, "thread/turns/list", json!({"threadId": id, "limit": 10, "sortDirection": "desc"})).await?;
        let data = turns["data"].as_array().ok_or("native turns/list returned no data array")?;
        check(data.len() == 2 && [first_id, second_id].iter().all(|turn| data.iter().any(|entry| entry["id"] == *turn
            && entry["status"] == "completed")), "native history does not contain exactly the two completed turns")?;
        let detail = live_call(&host, "inspect", json!({"thread_id": id})).await?;
        check(detail["title"] == name && detail["model"] == MODEL && detail["reasoning_effort"] == "max"
            && detail["mode"] == "read-only" && detail["last_turn_id"] == second_id
            && detail["status"] == "completed" && detail["active_turn_id"].is_null(),
            "idle metadata operations changed execution state or failed to persist")?;
        check(git(&fixture.alpha, &["rev-parse", "HEAD"]) == initial_head, "native probe changed fixture HEAD")?;
        for repo in [&fixture.alpha, &fixture.beta, &fixture.outside] {
            check(git(repo, &["status", "--porcelain", "--untracked-files=all"]).is_empty(),
                "marker-only native probe changed a fixture checkout")?;
        }
        Ok::<(), &'static str>(())
    }).await;
    if !matches!(outcome, Ok(Ok(())))
        && let Some(id) = active_thread
    {
        let _ = tokio::time::timeout(
            DEADLINE,
            live_call(
                &host,
                "control",
                json!({"thread_id": id, "action": "interrupt"}),
            ),
        )
        .await;
    }
    fixture.shutdown(vec![host]).await;
    match outcome {
        Ok(Ok(())) => eprintln!(
            "native live probe passed: two turns, dynamic response, raw events, idle metadata and boundaries"
        ),
        Ok(Err(reason)) => panic!("{reason}"),
        Err(_) => {
            panic!("native live probe exceeded 240 seconds; interrupt attempted before shutdown")
        }
    }
}
