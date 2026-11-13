use super::Fact;
use crate::resource_read::open_resource;
use anchor_runtime::graph::{GraphError, InvocationKey};
use serde_json::{Value, json};
use std::{io::Read, path::Path};

mod live;
pub(crate) use live::LiveTrace;

const MAX_PROJECTION_BYTES: u64 = 160 * 1024 * 1024;

pub(super) fn store_evidence(path: &Path, evidence: &Value) -> Result<(), GraphError> {
    crate::write_durable(
        path,
        &serde_json::to_vec_pretty(evidence)
            .map_err(|error| GraphError::CorruptRun(error.to_string()))?,
    )?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(root: &Path, name: &str) -> Result<Option<T>, String> {
    let file = match open_resource(root, name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Goose trace resource cannot be read safely".into()),
    };
    let mut bytes = Vec::new();
    file.take(MAX_PROJECTION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Goose trace resource is unreadable")?;
    if bytes.len() as u64 > MAX_PROJECTION_BYTES {
        return Err("Goose trace resource exceeds the projection limit".into());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "Goose trace resource is malformed".into())
}

/// Notifications retained by a currently running invocation, or `None` when
/// that invocation is not live. Read-only and best-effort: a missing or
/// unregistered invocation is never an error for the caller.
pub(crate) fn live_notifications(
    state: &Path,
    key: &InvocationKey,
) -> Result<Option<Vec<Value>>, String> {
    let stem = crate::host_fs::invocation_stem(key);
    for directory in ["goose-acp", "goose-acp-spike"] {
        let fact = state.join(directory).join(format!("{stem}.json"));
        if let Some(notifications) = live::notifications(&fact)? {
            return Ok(Some(notifications));
        }
    }
    Ok(None)
}

pub(crate) fn trace_messages(state: &Path, key: &InvocationKey) -> Result<Vec<Value>, String> {
    let stem = crate::host_fs::invocation_stem(key);
    let mut selected = None;
    for (directory, version, runtime) in [
        ("goose-acp", 2, "goose"),
        ("goose-acp-spike", 1, "goose-acp-spike"),
    ] {
        let root = state.join(directory);
        let fact = read_json::<Value>(&root, &format!("{stem}.json"))?;
        let evidence = read_json::<Value>(&root, &format!("{stem}.evidence.json"))?;
        if fact.is_none() && evidence.is_none() {
            continue;
        }
        if selected.is_some() {
            return Err("Goose trace has ambiguous invocation facts".into());
        }
        let fact: Fact = serde_json::from_value(fact.ok_or("Goose trace has no invocation fact")?)
            .map_err(|_| "Goose trace invocation fact is malformed")?;
        if fact.key != *key || fact.version != version {
            return Err("Goose trace invocation identity changed".into());
        }
        if let Some(evidence) = &evidence
            && (evidence["version"] != 1
                || evidence["runtime"] != runtime
                || evidence["key"] != json!(key))
        {
            return Err("Goose trace evidence identity changed".into());
        }
        selected = Some((fact, evidence, root.join(format!("{stem}.json"))));
    }
    let Some((fact, evidence, fact_path)) = selected else {
        return Ok(Vec::new());
    };
    if let Some(messages) = live::snapshot(&fact_path, fact.session_id.as_deref())? {
        return Ok(messages);
    }
    let mut messages = Vec::new();
    if let Some(evidence) = evidence {
        for field in ["restored_history", "notifications"] {
            if let Some(events) = evidence.get(field).and_then(Value::as_array) {
                for event in events {
                    project_update(&mut messages, event, fact.session_id.as_deref())?;
                }
            }
        }
    }
    if let Some(observation) = fact.tool_observation {
        project_tool_observation(&mut messages, observation);
    }
    Ok(messages)
}

/// Run detail entries contain only this invocation's updates. Repeating the
/// restored Session history for every pass makes long-running details quadratic.
pub(crate) fn run_trace_messages(state: &Path, key: &InvocationKey) -> Result<Vec<Value>, String> {
    let stem = crate::host_fs::invocation_stem(key);
    let mut selected = None;
    for (directory, version, runtime) in [
        ("goose-acp", 2, "goose"),
        ("goose-acp-spike", 1, "goose-acp-spike"),
    ] {
        let root = state.join(directory);
        let fact = read_json::<Fact>(&root, &format!("{stem}.json"))?;
        let evidence = read_json::<InvocationEvidence>(&root, &format!("{stem}.evidence.json"))?;
        if fact.is_none() && evidence.is_none() {
            continue;
        }
        if selected.is_some() {
            return Err("Goose trace has ambiguous invocation facts".into());
        }
        let fact = fact.ok_or("Goose trace has no invocation fact")?;
        if fact.key != *key || fact.version != version {
            return Err("Goose trace invocation identity changed".into());
        }
        if let Some(evidence) = &evidence
            && (evidence.version != 1 || evidence.runtime != runtime || evidence.key != *key)
        {
            return Err("Goose trace evidence identity changed".into());
        }
        selected = Some((fact, evidence));
    }
    let Some((fact, evidence)) = selected else {
        return Ok(Vec::new());
    };
    if let Some(events) = live_notifications(state, key)? {
        let mut messages = Vec::new();
        for event in events {
            project_update(&mut messages, &event, fact.session_id.as_deref())?;
        }
        return Ok(messages);
    }
    let mut messages = Vec::new();
    if let Some(events) = evidence
        .as_ref()
        .and_then(|evidence| evidence.notifications.as_array())
    {
        for event in events {
            project_update(&mut messages, event, fact.session_id.as_deref())?;
        }
    }
    if let Some(observation) = fact.tool_observation {
        project_tool_observation(&mut messages, observation);
    }
    Ok(messages)
}

fn project_tool_observation(messages: &mut Vec<Value>, observation: Value) {
    if observation["result"].is_null() {
        messages.push(json!({
            "role":"assistant","text":"",
            "commands":[format!("{} {}", observation["tool"].as_str().unwrap_or("unknown"), observation["arguments"])],
            "source":"anchor_tool_observation"
        }));
        messages.push(json!({
            "role":"tool","text":"The tool result was not recorded. External outcome is unknown; inspect actual state before acting.",
            "tool":observation["tool"],"unrecorded":true,"unknown_external_outcome":true,
            "source":"anchor_tool_observation"
        }));
    } else if messages.is_empty() {
        messages.push(json!({"role":"tool","text":observation.to_string(),"source":"anchor_tool_observation"}));
    }
}

// Serde skips restored_history without allocating its nested JSON Values. The
// reader still scans the persisted bytes and validates the evidence identity.
#[derive(serde::Deserialize)]
struct InvocationEvidence {
    version: u64,
    runtime: String,
    key: InvocationKey,
    #[serde(default)]
    notifications: Value,
}

fn project_update(
    messages: &mut Vec<Value>,
    event: &Value,
    session: Option<&str>,
) -> Result<(), String> {
    if event["source"] == "anchor.acp.transport" && event["kind"] == "notification_tail_truncated" {
        messages.push(json!({"role":"system","text":"Earlier live display updates were omitted. Inspect the native Goose history for the full conversation.","truncated":true,"source":"anchor.acp.transport"}));
        return Ok(());
    }
    if event["method"] != "session/update" {
        return Ok(());
    }
    if event["params"]["sessionId"].as_str() != session || session.is_none() {
        return Err("Goose trace update belongs to another Session".into());
    }
    let update = &event["params"]["update"];
    match update["sessionUpdate"].as_str() {
        Some("agent_message_chunk" | "agent_thought_chunk" | "user_message_chunk") => {
            let (role, thinking) = match update["sessionUpdate"].as_str() {
                Some("user_message_chunk") => ("user", false),
                Some("agent_thought_chunk") => ("assistant", true),
                _ => ("assistant", false),
            };
            if update["content"]["type"] == "image" {
                messages.push(json!({"role":role,"text":"","contents":[{"type":"content","content":update["content"]}]}));
                return Ok(());
            }
            let Some(text) = update["content"]["text"]
                .as_str()
                .filter(|text| !text.is_empty())
            else {
                return Ok(());
            };
            if let Some(previous) = messages.last_mut()
                && previous["role"] == role
                && previous["thinking"].as_bool().unwrap_or(false) == thinking
                && previous["commands"].is_null()
                && previous["contents"].is_null()
                && let Some(previous_text) = previous["text"].as_str()
            {
                previous["text"] = json!(format!("{previous_text}{text}"));
            } else {
                let mut message = json!({"role":role,"text":text});
                if thinking {
                    message["thinking"] = json!(true);
                }
                messages.push(message);
            }
        }
        Some("tool_call") => {
            let tool = update["_meta"]["goose"]["toolCall"]["toolName"]
                .as_str()
                .or_else(|| update["title"].as_str())
                .unwrap_or("Goose tool");
            messages.push(json!({
                "role":"assistant","text":"","commands":[format!("{tool} {}",update["rawInput"])],
                "tool_call_id":update["toolCallId"],"status":update["status"]
            }));
        }
        Some("tool_call_update") => {
            if let Some(status) = update["status"].as_str()
                && let Some(request) = messages.iter_mut().rev().find(|message| {
                    !message["commands"].is_null()
                        && message["tool_call_id"] == update["toolCallId"]
                })
            {
                request["status"] = json!(status);
            }
            let text = update["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|part| part["content"]["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let contents = update["content"]
                .as_array()
                .filter(|parts| parts.iter().any(|part| part["content"]["type"] == "image"));
            if !text.is_empty() || contents.is_some() {
                let mut message = json!({
                    "role":"tool","text":text,"tool_call_id":update["toolCallId"],"status":update["status"]
                });
                if let Some(contents) = contents {
                    message["contents"] = json!(contents);
                }
                messages.push(message);
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_reader_retains_complete_snapshot_during_evidence_publication() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("trace.evidence.json");
        let previous = json!({"notifications":["previous".repeat(8192)]});
        let current = json!({"notifications":["current".repeat(8192)]});
        store_evidence(&path, &previous).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let snapshot = std::fs::File::open(&path).unwrap();
        store_evidence(&path, &current).unwrap();
        // A reader can retain the old inode while publication replaces the
        // pathname. It must see one complete document throughout that read.
        assert_eq!(
            serde_json::from_reader::<_, Value>(snapshot).unwrap(),
            previous
        );
        assert_eq!(
            read_json(directory.path(), "trace.evidence.json").unwrap(),
            Some(current)
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn trace_reader_retains_complete_snapshots_during_repeated_evidence_publication() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("goose-acp");
        fact(&root, &key(), Value::Null);
        let name = format!("{}.evidence.json", crate::host_fs::invocation_stem(&key()));
        let path = root.join(&name);
        let versions = ["previous".repeat(8192), "current".repeat(4096)].map(|text| {
            json!({"version":1,"runtime":"goose","key":key(),"notifications":[
                update(json!({"sessionUpdate":"agent_message_chunk","content":{"text":text}}))
            ]})
        });
        store_evidence(&path, &versions[0]).unwrap();
        std::thread::scope(|scope| {
            let (opened, ready) = std::sync::mpsc::sync_channel(0);
            let (published, committed) = std::sync::mpsc::sync_channel(0);
            let published_path = &path;
            let published_versions = &versions;
            let writer = scope.spawn(move || {
                for sequence in 1..=32 {
                    if ready.recv().is_err() {
                        return;
                    }
                    store_evidence(published_path, &published_versions[sequence % 2]).unwrap();
                    published.send(()).unwrap();
                }
            });
            for sequence in 1..=32 {
                let mut snapshot = open_resource(&root, &name).unwrap();
                let mut bytes = vec![0; 1024];
                snapshot.read_exact(&mut bytes).unwrap();
                opened.send(()).unwrap();
                committed.recv().unwrap();
                snapshot.read_to_end(&mut bytes).unwrap();
                assert!(
                    serde_json::from_slice::<Value>(&bytes).unwrap()
                        == versions[(sequence - 1) % 2],
                    "retained reader changed during publication {sequence}"
                );
                assert_eq!(
                    read_json(&root, &name).unwrap(),
                    Some(versions[sequence % 2].clone())
                );
                let messages = trace_messages(directory.path(), &key()).unwrap();
                assert_eq!(messages.len(), 1);
                assert_eq!(
                    messages[0]["text"],
                    versions[sequence % 2]["notifications"][0]["params"]["update"]["content"]["text"]
                );
            }
            writer.join().unwrap();
        });
    }

    #[test]
    fn trace_reader_observes_only_complete_versions_while_evidence_is_rewritten() {
        let directory = tempfile::tempdir().unwrap();
        let name = "trace.evidence.json";
        let path = directory.path().join(name);
        let versions = [
            json!({"notifications":["previous".repeat(8192)]}),
            json!({"notifications":["current".repeat(16384)]}),
        ];
        store_evidence(&path, &versions[0]).unwrap();
        std::thread::scope(|scope| {
            let (ready, started) = std::sync::mpsc::sync_channel(0);
            let (stop, stopped) = std::sync::mpsc::channel::<()>();
            let root = directory.path();
            let snapshots = &versions;
            let reader = scope.spawn(move || {
                let initial = read_json(root, name).unwrap().unwrap();
                assert!(snapshots.contains(&initial));
                let mut reads = 1;
                ready.send(()).unwrap();
                while matches!(
                    stopped.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty)
                ) {
                    let snapshot = read_json(root, name).unwrap().unwrap();
                    assert!(
                        snapshots.contains(&snapshot),
                        "partial evidence was published"
                    );
                    reads += 1;
                }
                reads
            });
            started.recv().unwrap();
            for sequence in 1..=64 {
                store_evidence(&path, &versions[sequence % 2]).unwrap();
            }
            drop(stop);
            assert!(reader.join().unwrap() > 0);
        });
    }

    #[test]
    fn image_only_tool_updates_keep_their_native_content_projection() {
        let mut messages = Vec::new();
        let contents = json!([{"type":"content","content":{"type":"image","mimeType":"image/png","data":"AAEC"}}]);
        project_update(&mut messages, &json!({"method":"session/update","params":{
            "sessionId":"native-session","update":{"sessionUpdate":"tool_call_update","toolCallId":"image-tool","status":"completed","content":contents}
        }}), Some("native-session")).unwrap();
        assert_eq!(
            messages,
            vec![
                json!({"role":"tool","text":"","tool_call_id":"image-tool","status":"completed","contents":contents})
            ]
        );
    }

    #[test]
    fn message_images_remain_ordered_and_text_does_not_swallow_media() {
        let mut messages = Vec::new();
        for content in [
            json!({"type":"text","text":"before"}),
            json!({"type":"image","mimeType":"image/png","data":"AAEC"}),
            json!({"type":"text","text":"after"}),
        ] {
            project_update(
                &mut messages,
                &update(json!({"sessionUpdate":"agent_message_chunk","content":content})),
                Some("native-session"),
            )
            .unwrap();
        }
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0]["text"], "before");
        assert_eq!(messages[1]["contents"][0]["content"]["type"], "image");
        assert_eq!(messages[2]["text"], "after");
    }

    #[test]
    fn status_only_updates_bind_to_the_native_tool_identity() {
        let mut messages = Vec::new();
        for event in [
            json!({"sessionUpdate":"tool_call","toolCallId":"first","title":"read","rawInput":{}}),
            json!({"sessionUpdate":"tool_call","toolCallId":"second","title":"write","rawInput":{}}),
            json!({"sessionUpdate":"tool_call_update","toolCallId":"first","status":"failed"}),
        ] {
            project_update(&mut messages, &update(event), Some("native-session")).unwrap();
        }
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["status"], "failed");
        assert!(messages[1]["status"].is_null());
    }

    fn key() -> InvocationKey {
        InvocationKey {
            run_id: "run".into(),
            graph_digest: "digest".into(),
            node_id: "node".into(),
            invocation: 1,
        }
    }

    fn write(root: &Path, name: &str, value: &Value) {
        std::fs::create_dir_all(root).unwrap();
        std::fs::write(root.join(name), serde_json::to_vec(value).unwrap()).unwrap();
    }

    fn fact(root: &Path, key: &InvocationKey, observation: Value) {
        write(
            root,
            &format!("{}.json", crate::host_fs::invocation_stem(key)),
            &json!({
                "version":2,"key":key,"binary_sha256":"fixture","session_id":"native-session",
                "completion":null,"reason":null,"model_binding":null,"tool_observation":observation
            }),
        );
    }

    fn update(value: Value) -> Value {
        json!({"method":"session/update","params":{"sessionId":"native-session","update":value}})
    }

    #[test]
    fn projects_public_acp_text_commands_and_results_without_native_database() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("goose-acp");
        fact(&root, &key(), Value::Null);
        write(
            &root,
            &format!("{}.evidence.json", crate::host_fs::invocation_stem(&key())),
            &json!({
                "version":1,"runtime":"goose","key":key(),"restored_history":[],"notifications":[
                    update(json!({"sessionUpdate":"agent_message_chunk","content":{"text":"Inspect "}})),
                    update(json!({"sessionUpdate":"agent_message_chunk","content":{"text":"workspace"}})),
                    update(json!({"sessionUpdate":"tool_call","title":"anchor_run","rawInput":{"command":["cat","report.txt"]},"toolCallId":"call-1"})),
                    update(json!({"sessionUpdate":"tool_call_update","toolCallId":"call-1","status":"completed","content":[{"content":{"text":"verified"}}]})),
                    update(json!({"sessionUpdate":"agent_message_chunk","content":{"text":"Done"}}))
                ]
            }),
        );
        let messages = trace_messages(directory.path(), &key()).unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["text"], "Inspect workspace");
        assert!(
            messages[1]["commands"][0]
                .as_str()
                .unwrap()
                .contains("report.txt")
        );
        assert_eq!(messages[2]["text"], "verified");
        assert_eq!(messages[3]["text"], "Done");
        assert!(!root.join("framework.sqlite3").exists());
    }

    #[test]
    fn run_projection_contains_each_turn_once_without_repeating_restored_history() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("goose-acp");
        let mut history = Vec::new();
        let mut all = Vec::new();
        for invocation in 1..=32 {
            let key = InvocationKey {
                invocation,
                ..key()
            };
            fact(&root, &key, Value::Null);
            let event = update(json!({"sessionUpdate":"agent_message_chunk",
                "content":{"text":format!("turn {invocation}\n")}}));
            write(
                &root,
                &format!("{}.evidence.json", crate::host_fs::invocation_stem(&key)),
                &json!({"version":1,"runtime":"goose","key":key,
                    "restored_history":history,"notifications":[event]}),
            );
            let current = run_trace_messages(directory.path(), &key).unwrap();
            assert_eq!(current.len(), 1);
            assert_eq!(current[0]["text"], format!("turn {invocation}\n"));
            all.extend(current);
            history.push(event);
        }
        assert_eq!(all.len(), 32);
        let full = trace_messages(
            directory.path(),
            &InvocationKey {
                invocation: 32,
                ..key()
            },
        )
        .unwrap();
        assert!(full[0]["text"].as_str().unwrap().starts_with("turn 1\n"));
        assert!(full[0]["text"].as_str().unwrap().ends_with("turn 32\n"));
    }

    #[test]
    fn run_projection_retains_current_live_tool_updates_and_validates_identity() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("goose-acp");
        assert!(
            run_trace_messages(directory.path(), &key())
                .unwrap()
                .is_empty()
        );
        fact(&root, &key(), Value::Null);
        let stem = crate::host_fs::invocation_stem(&key());
        let live = LiveTrace::open(&root.join(format!("{stem}.json"))).unwrap();
        live.restore(
            "native-session",
            &[update(json!({
                "sessionUpdate":"agent_message_chunk","content":{"text":"old history"}
            }))],
        )
        .unwrap();
        live.observe(&update(json!({"sessionUpdate":"tool_call_update",
            "toolCallId":"current","status":"in_progress",
            "content":[{"content":{"text":"current progress"}}]})))
            .unwrap();
        let messages = run_trace_messages(directory.path(), &key()).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["text"], "current progress");
        assert_eq!(messages[0]["status"], "in_progress");
        drop(live);
        write(
            &root,
            &format!("{stem}.evidence.json"),
            &json!({"version":1,"runtime":"goose","key":{"run_id":"other"},"notifications":[]}),
        );
        assert!(
            run_trace_messages(directory.path(), &key())
                .unwrap_err()
                .contains("malformed")
        );
        write(
            &root,
            &format!("{stem}.evidence.json"),
            &json!({"version":1,"runtime":"goose","key":InvocationKey { run_id:"other".into(), ..key() },"notifications":[]}),
        );
        assert!(
            run_trace_messages(directory.path(), &key())
                .unwrap_err()
                .contains("identity")
        );
    }

    #[test]
    fn run_projection_preserves_tool_observation_fallback_contract() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("goose-acp");
        let unresolved = json!({
            "tool":"external_send","arguments":{"message":"once"},"result":null
        });
        fact(&root, &key(), unresolved);
        let messages = run_trace_messages(directory.path(), &key()).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "assistant");
        assert!(
            messages[0]["commands"][0]
                .as_str()
                .unwrap()
                .contains("external_send")
        );
        assert_eq!(messages[1]["tool"], "external_send");
        assert_eq!(messages[1]["unrecorded"], true);
        assert_eq!(messages[1]["unknown_external_outcome"], true);

        let completed_key = InvocationKey {
            invocation: 2,
            ..key()
        };
        let completed = json!({
            "tool":"lookup","arguments":{"id":7},"result":{"found":true}
        });
        fact(&root, &completed_key, completed.clone());
        let messages = run_trace_messages(directory.path(), &completed_key).unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["text"], completed.to_string());
    }

    #[test]
    fn projects_unresolved_tool_without_claiming_failure_or_replaying_it() {
        let directory = tempfile::tempdir().unwrap();
        fact(
            &directory.path().join("goose-acp"),
            &key(),
            json!({"tool":"external_send","arguments":{"message":"once"},"result":null}),
        );
        let messages = trace_messages(directory.path(), &key()).unwrap();
        assert_eq!(messages[1]["unrecorded"], true);
        assert_eq!(messages[1]["unknown_external_outcome"], true);
        assert_eq!(messages[1]["tool"], "external_send");
    }

    #[test]
    fn missing_projection_is_empty_but_corrupt_or_foreign_evidence_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        assert!(trace_messages(directory.path(), &key()).unwrap().is_empty());
        let root = directory.path().join("goose-acp");
        fact(&root, &key(), Value::Null);
        let name = format!("{}.evidence.json", crate::host_fs::invocation_stem(&key()));
        write(
            &root,
            &name,
            &json!({"version":1,"runtime":"goose","key":key(),"notifications":[{"method":"session/update","params":{"sessionId":"foreign"}}]}),
        );
        assert!(
            trace_messages(directory.path(), &key())
                .unwrap_err()
                .contains("another Session")
        );
        write(
            &root,
            &name,
            &json!({"version":1,"runtime":"goose","key":{"run_id":"foreign"}}),
        );
        assert!(
            trace_messages(directory.path(), &key())
                .unwrap_err()
                .contains("identity")
        );
    }

    #[test]
    fn refuses_symlinks_and_oversized_projection_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("goose-acp");
        fact(&root, &key(), Value::Null);
        let path = root.join(format!(
            "{}.evidence.json",
            crate::host_fs::invocation_stem(&key())
        ));
        std::os::unix::fs::symlink(root.join("missing"), &path).unwrap();
        assert!(trace_messages(directory.path(), &key()).is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::File::create(path)
            .unwrap()
            .set_len(MAX_PROJECTION_BYTES + 1)
            .unwrap();
        assert!(
            trace_messages(directory.path(), &key())
                .unwrap_err()
                .contains("limit")
        );
    }
}
