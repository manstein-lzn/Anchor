use super::*;

fn feedback(request: &Value) -> Vec<Value> {
    request["input"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| item["type"] == "function_call_output")
        .cloned()
        .collect()
}

fn definition<'a>(request: &'a Value, suffix: &str) -> Result<&'a Value, String> {
    let tools = request["tools"]
        .as_array()
        .ok_or("missing actual Responses tools schema")?;
    let matches = tools
        .iter()
        .filter(|tool| {
            tool["type"] == "function"
                && tool["name"]
                    .as_str()
                    .is_some_and(|name| name == suffix || name.ends_with(&format!("__{suffix}")))
        })
        .collect::<Vec<_>>();
    if matches.len() != 1 {
        return Err(format!(
            "expected one actual Responses {suffix} definition, got {}: {tools:?}",
            matches.len()
        ));
    }
    Ok(matches[0])
}

/// Compare against outputs this Provider actually emitted, including opaque fields.
/// Retained history must preserve both reasoning order and each tool exchange.
fn retained_reasoning(request: &Value, exchanges: &[Value]) -> Result<(), String> {
    let input = request["input"]
        .as_array()
        .ok_or("missing actual Responses input array")?;
    let outputs = exchanges
        .iter()
        .filter_map(|exchange| exchange["response_output"].as_array())
        .collect::<Vec<_>>();
    let expected = outputs
        .iter()
        .flat_map(|output| output.iter())
        .filter(|item| item["type"] == "reasoning")
        .collect::<Vec<_>>();
    let actual = input
        .iter()
        .filter(|item| item["type"] == "reasoning")
        .collect::<Vec<_>>();
    if actual != expected {
        return Err(format!(
            "complete reasoning items must occur exactly once in original order; expected {expected:?}, got {actual:?}"
        ));
    }
    for output in outputs {
        for (offset, reasoning) in output.iter().enumerate() {
            if reasoning["type"] != "reasoning" {
                continue;
            }
            let call = output[offset + 1..]
                .iter()
                .find(|item| item["type"] == "function_call")
                .ok_or("reasoning fixture must precede a function call")?;
            let positions = |kind: &str| {
                input
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| item["type"] == kind && item["call_id"] == call["call_id"])
                    .map(|(index, _)| index)
                    .collect::<Vec<_>>()
            };
            let calls = positions("function_call");
            let results = positions("function_call_output");
            let reasoning_index = input.iter().position(|item| item == reasoning).unwrap();
            if calls.len() != 1
                || results.len() != 1
                || reasoning_index >= calls[0]
                || calls[0] >= results[0]
            {
                return Err(format!(
                    "reasoning must precede its single function call and output: reasoning={reasoning_index}, calls={calls:?}, outputs={results:?}, call_id={}",
                    call["call_id"]
                ));
            }
        }
    }
    Ok(())
}

pub(super) async fn completion(
    State(shared): State<Arc<Mutex<ProviderState>>>,
    Json(request): Json<Value>,
) -> Response {
    let (sequence, step) = {
        let mut state = shared.lock().unwrap();
        state.requests.push(request.clone());
        let sequence = state.requests.len();
        state
            .exchanges
            .push(json!({"method":"POST","path":"/v1/responses",
            "request":request,"tool_feedback":feedback(&request)}));
        if request["model"] != state.model || request["stream"] != true {
            let model = state.model.clone();
            return state.fail(format!(
                "expected Responses {model} with stream=true: {request}"
            ));
        }
        if let Err(message) = retained_reasoning(&request, &state.exchanges) {
            return state.fail(message);
        }
        let Some(step) = state.take_step(&request) else {
            return state.fail("unscripted Responses request".into());
        };
        if step.summary || step.preface.is_some() {
            return state.fail("Responses fixture does not script compaction or prefaces".into());
        }
        if let Some(expected) = &step.request_contains
            && !request["input"].to_string().contains(expected)
        {
            return state.fail(format!(
                "actual Responses history must contain {expected:?}: {request}"
            ));
        }
        if let Some(expected) = step.feedback
            && !feedback(&request)
                .last()
                .is_some_and(|item| item["output"].to_string().contains(expected))
        {
            return state.fail(format!(
                "latest actual Responses tool output must contain {expected:?}: {request}"
            ));
        }
        state.save();
        (sequence, step)
    };
    let mut output = Vec::new();
    let mut reply = step.reply;
    loop {
        match reply {
            Reply::Gated(gate, next) => {
                gate.entered.store(true, Ordering::Release);
                gate.release.acquire().await.unwrap().forget();
                reply = *next;
            }
            Reply::Reasoning(item, next) => {
                output.push(item);
                reply = *next;
            }
            _ => break,
        }
    }
    if let Reply::HttpError(status, message) = reply {
        let response = json!({"error": {
            "message": message, "type": "invalid_request_error", "code": "fixture_error"
        }});
        let mut state = shared.lock().unwrap();
        state.exchanges.push(json!({
            "sequence": sequence, "response_status": status, "response": response
        }));
        state.save();
        return (StatusCode::from_u16(status).unwrap(), Json(response)).into_response();
    }
    let mut selected_tool = Value::Null;
    let mut text = None;
    match reply {
        Reply::Batch(calls) => {
            let mut definitions = Vec::new();
            for (index, (suffix, mut arguments)) in calls.into_iter().enumerate() {
                let tool = match definition(&request, suffix) {
                    Ok(tool) => tool,
                    Err(message) => return shared.lock().unwrap().fail(message),
                };
                definitions.push(tool.clone());
                if suffix == "final_result"
                    && arguments.get("observed_receipt").is_none()
                    && let Some(receipt) = feedback(&request)
                        .iter()
                        .rev()
                        .filter_map(observed_receipt)
                        .nth(step.receipt_back)
                {
                    arguments["observed_receipt"] = json!(receipt);
                }
                output.push(json!({"type":"function_call",
                    "id":format!("fc_fixture_{sequence}_{index}"),"status":"completed",
                    "call_id":format!("call_fixture_{sequence}_{index}"),
                    "name":tool["name"],"arguments":arguments.to_string()}));
            }
            selected_tool = json!(definitions);
        }
        Reply::Tool(suffix, mut arguments) => {
            let tool = match definition(&request, suffix) {
                Ok(tool) => tool,
                Err(message) => return shared.lock().unwrap().fail(message),
            };
            selected_tool = tool.clone();
            if suffix == "final_result"
                && tool["parameters"]["properties"]
                    .get("observed_receipt")
                    .is_some()
                && arguments.get("observed_receipt").is_none()
                && let Some(receipt) = feedback(&request)
                    .iter()
                    .rev()
                    .filter_map(observed_receipt)
                    .nth(step.receipt_back)
            {
                arguments["observed_receipt"] = json!(receipt);
            }
            output.push(
                json!({"type":"function_call","id":format!("fc_fixture_{sequence}"),
                "status":"completed","call_id":format!("call_fixture_{sequence}"),
                "name":tool["name"],"arguments":arguments.to_string()}),
            );
        }
        Reply::Text(value) => {
            text = Some(value);
            output.push(
                json!({"type":"message","id":format!("msg_fixture_{sequence}"),
                "status":"completed","role":"assistant",
                "content":[{"type":"output_text","text":value,"annotations":[]}]}),
            );
        }
        _ => {
            return shared
                .lock()
                .unwrap()
                .fail("unsupported Responses fixture reply".into());
        }
    }
    let response = |status: &str, items: Value| {
        json!({"id":format!("resp_fixture_{sequence}"),"object":"response","created_at":1,
            "status":status,"model":request["model"],"output":items})
    };
    let mut chunks = vec![
        // Some compatible endpoints send a payload-free heartbeat without the
        // fields required for a real text delta, even before response.created.
        json!({"type":"response.output_text.delta","delta":""}),
        json!({"type":"response.created","sequence_number":1,
            "response":response("in_progress",json!([]))}),
    ];
    if let Some(text) = text {
        chunks.push(
            json!({"type":"response.output_text.delta","sequence_number":chunks.len()+1,
            "item_id":format!("msg_fixture_{sequence}"),"output_index":output.len()-1,
            "content_index":0,"delta":text}),
        );
    }
    for (index, item) in output.iter().enumerate() {
        chunks.push(
            json!({"type":"response.output_item.done","sequence_number":chunks.len()+1,
            "output_index":index,"item":item}),
        );
    }
    let (input, completion) = step.usage;
    let usage =
        json!({"input_tokens":input,"output_tokens":completion,"total_tokens":input+completion});
    let mut completed = response("completed", json!(output));
    completed["usage"] = usage.clone();
    chunks.push(
        json!({"type":"response.completed","sequence_number":chunks.len()+1,"response":completed}),
    );
    {
        let mut state = shared.lock().unwrap();
        state
            .exchanges
            .push(json!({"sequence":sequence,"response_chunks":chunks,
            "response_output":output,"usage":usage,"selected_actual_tool_schema":selected_tool}));
        state.save();
    }
    let frames = chunks
        .into_iter()
        .map(|chunk| Ok::<_, Infallible>(format!("data: {chunk}\n\n")));
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(futures_util::stream::iter(frames)),
    )
        .into_response()
}

#[test]
fn reasoning_validator_rejects_loss_duplication_and_reordering() {
    let reasoning = json!({"type":"reasoning","id":"rs_fixture","status":"completed",
        "summary":[{"type":"summary_text","text":"summary"}],
        "content":[{"type":"reasoning_text","text":"body"}],"encrypted_content":"opaque"});
    let call = json!({"type":"function_call","call_id":"call_fixture"});
    let result = json!({"type":"function_call_output","call_id":"call_fixture","output":"actual"});
    let exchanges = vec![json!({"response_output":[reasoning,call]})];
    assert!(retained_reasoning(&json!({"input":[reasoning,call,result]}), &exchanges).is_ok());
    for input in [
        json!([call, result]),
        json!([reasoning, reasoning, call, result]),
        json!([call, reasoning, result]),
        json!([reasoning, result, call]),
    ] {
        assert!(retained_reasoning(&json!({"input":input}), &exchanges).is_err());
    }
    for field in ["id", "status", "summary", "content", "encrypted_content"] {
        let mut changed = reasoning.clone();
        changed.as_object_mut().unwrap().remove(field);
        assert!(retained_reasoning(&json!({"input":[changed,call,result]}), &exchanges).is_err());
    }
}
