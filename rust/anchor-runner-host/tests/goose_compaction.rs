#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use goose::{
    Gate, Host, HttpHost, Provider, Step, command, complete, is_summary_request, tool_feedback,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io::Read};

const CASE_SOURCE: &str = "tests/goose_compaction.rs";
const PLUGIN: &str = "compaction";
const SKILL: &str = "/plugins/compaction/skills/inspect/SKILL.md";
const CONTINUATION: &str = "Your context was compacted.";

fn random_value(label: &str) -> String {
    let mut bytes = [0; 32];
    fs::File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut bytes)
        .unwrap();
    format!("g2f-{label}-{:x}", Sha256::digest(bytes))
}

fn graph() -> Value {
    json!({
        "objective":"Inspect real Plugin and workspace facts after native Goose compaction",
        "entry":"worker",
        "agents":{"worker":{"model":"models.worker","instructions":
            "Read the mounted Plugin instructions and workspace with authorized Anchor tools. After compaction or interruption, inspect current facts instead of repeating effects. Finish with final_result, route verify, and a newly observed receipt."}},
        "ops":{"verify":{"run":
            "sh -c 'set -eu; test \"$(cat /in/worker/effects.txt)\" = once; cat /in/worker/evidence.txt > verified.txt; cat /in/worker/plugin-after.txt > verified-plugin.txt; cat /in/worker/effects.txt > verified-effects.txt'"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":[PLUGIN]},
                 {"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })
}

struct Facts {
    workspace: String,
    skill: String,
    summary: String,
}

impl Facts {
    fn new() -> Self {
        Self {
            workspace: random_value("workspace"),
            skill: format!(
                "---\nname: inspect-compaction\ndescription: Inspect durable facts after interruption.\n---\nPlugin marker: {}\nRead workspace.txt and effects.txt before continuing. Do not replay a recorded effect.\n",
                random_value("plugin")
            ),
            summary: format!(
                "{}: The workspace has an effect to inspect. Re-read the mounted Plugin instructions and current workspace before completing. This summary is not a tool observation or completion receipt.",
                random_value("summary")
            ),
        }
    }

    fn seed(&self) -> Step {
        command(&format!(
            "set -eu; test ! -e effects.txt; cat {SKILL} > plugin-before.txt; if printf corrupt >> {SKILL} 2>/dev/null; then exit 91; fi; printf '%s' '{}' > workspace.txt; printf once >> effects.txt; cat plugin-before.txt workspace.txt effects.txt",
            self.workspace
        ))
        .with_usage(120_000, 7)
    }

    fn reread(&self) -> Step {
        command(&format!(
            "set -eu; test \"$(cat effects.txt)\" = once; cat {SKILL} > plugin-after.txt; if printf corrupt >> {SKILL} 2>/dev/null; then exit 92; fi; cat workspace.txt > evidence.txt; cat plugin-after.txt evidence.txt effects.txt; printf '\\nplugin-workspace-reread\\n'"
        ))
    }
}

fn install_plugin(host: &Host, facts: &Facts) -> Value {
    let bundle = host.base.root.path().join("bundle");
    let plugin = bundle.join("plugins").join(PLUGIN);
    fs::create_dir_all(plugin.join("skills/inspect")).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        json!({"name":"Compaction fixture","description":"Inspect durable facts.","skills":"./skills"}).to_string(),
    )
    .unwrap();
    fs::write(plugin.join("skills/inspect/SKILL.md"), &facts.skill).unwrap();
    let binding = FilePluginCatalog::new(&bundle)
        .resolve(&[PLUGIN.into()])
        .unwrap()
        .remove(0);
    let summary = json!({
        "id":binding.id,"digest":binding.digest,
        "resources":binding.resources,"mcp_servers":binding.mcp_servers
    });
    assert_eq!(
        summary["resources"],
        json!(["plugin.json", "skills/inspect/SKILL.md"])
    );
    assert_eq!(summary["mcp_servers"], json!([]));
    fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[summary]}).to_string(),
    )
    .unwrap();
    summary
}

fn checkpoint(host: &Host, provider: &Provider, run: &str, label: &str) {
    fs::write(
        provider.root.join(format!("{label}.json")),
        serde_json::to_vec_pretty(&json!({
            "run":host.record(run),
            "fact":host.native_fact(run,"worker",1),
            "history":host.native_conversation(run,"worker",1),
            "workspace":goose::file_inventory(&host.base.root.path().join("work").join(run))
        }))
        .unwrap(),
    )
    .unwrap();
}

fn receipt(value: &Value) -> Option<String> {
    if let Some(receipt) = value.get("anchor_receipt").and_then(Value::as_str) {
        return Some(receipt.into());
    }
    match value {
        Value::String(text) => serde_json::Deserializer::from_str(text)
            .into_iter::<Value>()
            .next()
            .and_then(Result::ok)
            .and_then(|value| receipt(&value)),
        Value::Array(values) => values.iter().rev().find_map(receipt),
        Value::Object(values) => values.values().rev().find_map(receipt),
        _ => None,
    }
}

fn assert_compaction(history: &Value, facts: &Facts) -> Value {
    let messages = history.as_array().unwrap();
    let old = messages
        .iter()
        .find(|message| {
            message["content"].to_string().contains("plugin-before.txt")
                && message["content"].to_string().contains("toolRequest")
        })
        .unwrap_or_else(|| panic!("missing original native tool request: {history}"));
    assert_eq!(old["metadata"]["agentVisible"], false, "{old}");
    assert_eq!(old["metadata"]["userVisible"], true, "{old}");
    let old_result = messages
        .iter()
        .find(|message| {
            message["metadata"]["agentVisible"] == false
                && message["content"].to_string().contains("toolResponse")
                && message["content"].to_string().contains(&facts.workspace)
        })
        .unwrap_or_else(|| panic!("missing preserved old native result: {history}"));
    assert_eq!(old_result["metadata"]["userVisible"], true);
    let summaries = messages
        .iter()
        .filter(|message| {
            message["metadata"]["agentVisible"] == true
                && message["content"].to_string().contains(&facts.summary)
        })
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 1, "{history}");
    assert_eq!(summaries[0]["metadata"]["userVisible"], false);
    assert!(
        messages.iter().any(|message| {
            message["metadata"]["agentVisible"] == true
                && message["content"].to_string().contains(CONTINUATION)
        }),
        "missing native continuation: {history}"
    );
    json!({"old_request":old,"old_result":old_result,"summary":summaries[0]})
}

fn assert_artifacts(host: &Host, run: &str, facts: &Facts) -> Value {
    let saved = host.record(run);
    assert_eq!(saved["status"], "completed", "{saved}");
    for (node, name, expected) in [
        ("worker", "evidence.txt", facts.workspace.as_bytes()),
        ("worker", "workspace.txt", facts.workspace.as_bytes()),
        ("worker", "plugin-before.txt", facts.skill.as_bytes()),
        ("worker", "plugin-after.txt", facts.skill.as_bytes()),
        ("worker", "effects.txt", b"once".as_slice()),
        ("verify", "verified.txt", facts.workspace.as_bytes()),
        ("verify", "verified-plugin.txt", facts.skill.as_bytes()),
        ("verify", "verified-effects.txt", b"once".as_slice()),
    ] {
        assert_eq!(saved["invocations"][node], 1);
        assert_eq!(saved["results"][node].as_array().unwrap().len(), 1);
        assert_eq!(host.base.file(&saved, node, name), expected);
        assert_eq!(
            host.base.workspace_files(run, name),
            vec![expected.to_vec()]
        );
        let manifest = fixture::read_json(host.base.artifact(&saved, node).join("manifest.json"));
        assert_eq!(
            manifest["files"][name]["sha256"],
            format!("{:x}", Sha256::digest(expected))
        );
        assert_eq!(manifest["files"][name]["bytes"], expected.len());
    }
    assert_eq!(
        fs::read(
            host.base
                .root
                .path()
                .join("bundle/plugins/compaction/skills/inspect/SKILL.md")
        )
        .unwrap(),
        facts.skill.as_bytes()
    );
    saved
}

fn stop_after_seed(
    host: &Host,
    server: &HttpHost,
    provider: &Provider,
    facts: &Facts,
    gate: &Gate,
) -> (String, Value) {
    let run = server.trigger();
    gate.wait_entered();
    assert_eq!(provider.requests().len(), 2);
    assert_eq!(
        host.base.workspace_files(&run, "workspace.txt"),
        vec![facts.workspace.as_bytes().to_vec()]
    );
    assert_eq!(
        host.base.workspace_files(&run, "effects.txt"),
        vec![b"once".to_vec()]
    );
    let before = host.native_fact(&run, "worker", 1);
    assert_eq!(before["tool_observation"]["result"]["ok"], true);
    checkpoint(host, provider, &run, "before-initial-stop");
    let (status, accepted) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{accepted}");
    server.wait_status(&run, "stopped");
    gate.open();
    assert!(host.native_fact(&run, "worker", 1)["completion"].is_null());
    assert!(host.record(&run)["results"].get("verify").is_none());
    assert_eq!(provider.requests().len(), 2);
    checkpoint(host, provider, &run, "initial-stopped");
    (run, before)
}

fn resume(server: &HttpHost, run: &str) {
    let (status, accepted) = server.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{accepted}");
}

fn assert_identity(host: &Host, run: &str, before: &Value) -> Value {
    let after = host.native_fact(run, "worker", 1);
    for name in ["key", "session_id", "binary_sha256", "model_binding"] {
        assert_eq!(
            after[name], before[name],
            "Goose identity changed at {name}"
        );
    }
    assert_eq!(host.record(run)["invocations"]["worker"], 1);
    after
}

fn stale_completion(old_receipt: &str) -> Step {
    Step::tool(
        "final_result",
        json!({
            "summary":"An old receipt must not complete this resumed invocation.",
            "route":"verify","observed_receipt":old_receipt
        }),
    )
}

fn assert_resumed_completion(host: &Host, run: &str, old_receipt: &str) -> (Value, String) {
    let (fact, native, _) = host.native_record(run, "worker", 1);
    assert!(fact["completion"].is_object(), "{fact}");
    assert_eq!(native["resume"], true);
    let calls = native["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3, "{native}");
    assert_eq!(calls[0]["tool"], "final_result");
    assert_eq!(calls[0]["arguments"]["observed_receipt"], old_receipt);
    assert_eq!(calls[0]["result"]["ok"], false);
    assert_eq!(calls[1]["tool"], "anchor_run");
    assert_eq!(calls[1]["result"]["ok"], true);
    let fresh_receipt = receipt(&calls[1]["result"]).unwrap();
    assert_ne!(old_receipt, fresh_receipt);
    assert_eq!(calls[2]["tool"], "final_result");
    assert_eq!(calls[2]["arguments"]["observed_receipt"], fresh_receipt);
    assert_eq!(calls[2]["result"]["ok"], true);
    (native, fresh_receipt)
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_entry_compaction_rereads_readonly_plugin_and_workspace_before_graph_completion() {
    let facts = Facts::new();
    let gate = Gate::new();
    let provider = Provider::new(
        "goose-compaction-plugin-workspace",
        vec![
            facts.seed(),
            Step::text("A cancelled model reply is not a node completion.")
                .after("once")
                .gated(&gate),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let binding = install_plugin(&host, &facts);
    let server = host.serve(&provider);
    let (run, before) = stop_after_seed(&host, &server, &provider, &facts, &gate);
    let old_receipt = receipt(&before["tool_observation"]["result"]).unwrap();
    provider.append(vec![
        Step::summary(&facts.summary).expect_request_contains(&facts.workspace),
        Step::tool(
            "final_result",
            json!({
                "summary":"The previous receipt must not complete the resumed invocation.",
                "route":"verify","observed_receipt":old_receipt
            }),
        )
        .expect_request_contains(&facts.summary),
        facts.reread().after("after observing business results"),
        complete("verify").after("plugin-workspace-reread"),
        Step::text("Verified current Plugin and workspace after native entry compaction."),
    ]);
    resume(&server, &run);
    server.wait_status(&run, "completed");
    checkpoint(&host, &provider, &run, "completed");
    provider.assert_consumed();
    assert_artifacts(&host, &run, &facts);
    let requests = provider.requests();
    assert_eq!(requests.len(), 7);
    assert_eq!(
        requests
            .iter()
            .filter(|request| is_summary_request(request))
            .count(),
        1
    );
    assert!(requests[0]["messages"].to_string().contains(SKILL));
    assert!(is_summary_request(&requests[2]));
    assert!(
        tool_feedback(&requests[3]).is_empty(),
        "old tool results must be hidden after compaction"
    );
    let fresh_receipt = receipt(tool_feedback(&requests[5]).last().unwrap()).unwrap();
    let (fact, native, _) = host.native_record(&run, "worker", 1);
    assert_identity(&host, &run, &before);
    assert!(fact["completion"].is_object(), "{fact}");
    assert_eq!(native["resume"], true);
    let calls = native["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3, "{native}");
    assert_eq!(calls[0]["tool"], "final_result");
    assert_eq!(calls[0]["result"]["ok"], false);
    assert_eq!(calls[1]["tool"], "anchor_run");
    assert_eq!(calls[2]["tool"], "final_result");
    assert_eq!(calls[2]["arguments"]["observed_receipt"], fresh_receipt);
    assert_eq!(calls[2]["result"]["ok"], true);
    let history = host.native_conversation(&run, "worker", 1);
    let compaction = assert_compaction(&history, &facts);
    assert_eq!(receipt(&compaction["old_result"]).unwrap(), old_receipt);
    assert_ne!(old_receipt, fresh_receipt);
    host.evidence(&provider, &run, json!({
        "case_source":CASE_SOURCE,"native_entry_compaction":true,"stop_resume_same_invocation":true,
        "same_native_session":true,"mid_turn_compaction":false,
        "summary_requests":1,"plugin_binding":binding,"plugin_readonly_before_and_after":true,
        "random_workspace":facts.workspace,"compaction_history":compaction,
        "old_receipt":old_receipt,"fresh_receipt":fresh_receipt,
        "summary_cannot_complete":true,"artifact_hashes_checked":true,
        "effects":1,"real_model_requests":0
    }));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_compacted_effect_survives_host_kill_and_is_inspected_without_replay() {
    let facts = Facts::new();
    let effect = random_value("post-compaction-effect");
    let seed_gate = Gate::new();
    let reply_gate = Gate::new();
    let provider = Provider::new(
        "goose-compaction-effect-host-restart",
        vec![
            facts.seed(),
            Step::text("The initial interrupted reply cannot finish the Graph.")
                .after("once")
                .gated(&seed_gate),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let binding = install_plugin(&host, &facts);
    let mut server = host.serve(&provider);
    let (run, original) = stop_after_seed(&host, &server, &provider, &facts, &seed_gate);
    provider.append(vec![
        Step::summary(&facts.summary).expect_request_contains(&facts.workspace),
        command(&format!(
            "set -eu; test \"$(cat effects.txt)\" = once; cat {SKILL}; cat workspace.txt; test ! -e post-effects.txt; printf '%s' '{effect}' >> post-effects.txt; cat post-effects.txt; printf '\\npost-compaction-effect-written\\n'"
        )).expect_request_contains(&facts.summary),
        complete("verify").after("post-compaction-effect-written").gated(&reply_gate),
    ]);
    resume(&server, &run);
    reply_gate.wait_entered();
    let compacted = host.native_conversation(&run, "worker", 1);
    let compaction = assert_compaction(&compacted, &facts);
    let before_kill = assert_identity(&host, &run, &original);
    assert!(before_kill["completion"].is_null());
    assert_eq!(before_kill["tool_observation"]["tool"], "anchor_run");
    assert_eq!(before_kill["tool_observation"]["result"]["ok"], true);
    let old_receipt = receipt(&before_kill["tool_observation"]["result"]).unwrap();
    assert_eq!(
        host.base.workspace_files(&run, "post-effects.txt"),
        vec![effect.as_bytes().to_vec()]
    );
    assert!(compacted.as_array().unwrap().iter().any(|message| {
        message["content"].to_string().contains("toolResponse")
            && message["content"]
                .to_string()
                .contains("post-compaction-effect-written")
    }));
    assert!(host.record(&run)["results"].get("verify").is_none());
    assert_eq!(provider.requests().len(), 5);
    checkpoint(&host, &provider, &run, "before-host-kill-after-compaction");
    fs::copy(
        provider.root.join("host-http.log"),
        provider.root.join("before-host-kill.log"),
    )
    .unwrap();
    server.kill();
    drop(server);
    reply_gate.open();
    assert_eq!(host.native_conversation(&run, "worker", 1), compacted);

    provider.append(vec![
        stale_completion(&old_receipt).expect_request_contains("do not blindly repeat"),
        command(&format!(
            "set -eu; test \"$(cat effects.txt)\" = once; test \"$(cat post-effects.txt)\" = '{effect}'; cat {SKILL} > plugin-after.txt; if printf corrupt >> {SKILL} 2>/dev/null; then exit 95; fi; cat workspace.txt > evidence.txt; cat plugin-after.txt evidence.txt effects.txt post-effects.txt; printf '\\nplugin-workspace-reread\\n'"
        )).after("after observing business results"),
        complete("verify").after("plugin-workspace-reread"),
        Step::text("Inspected compacted workspace and completed without repeating either effect."),
    ]);
    let restarted = host.serve(&provider);
    assert_eq!(
        provider.requests().len(),
        5,
        "Host restart must not call the model"
    );
    assert_identity(&host, &run, &original);
    resume(&restarted, &run);
    restarted.wait_status(&run, "completed");
    checkpoint(&host, &provider, &run, "completed-after-host-restart");
    provider.assert_consumed();
    let saved = assert_artifacts(&host, &run, &facts);
    assert_identity(&host, &run, &before_kill);
    assert_eq!(
        host.base.file(&saved, "worker", "post-effects.txt"),
        effect.as_bytes()
    );
    assert_eq!(
        host.base.workspace_files(&run, "post-effects.txt"),
        vec![effect.as_bytes().to_vec()]
    );
    let manifest = fixture::read_json(host.base.artifact(&saved, "worker").join("manifest.json"));
    assert_eq!(
        manifest["files"]["post-effects.txt"]["sha256"],
        format!("{:x}", Sha256::digest(effect.as_bytes()))
    );
    assert_eq!(manifest["files"]["post-effects.txt"]["bytes"], effect.len());
    let requests = provider.requests();
    assert_eq!(requests.len(), 9);
    assert_eq!(
        requests
            .iter()
            .filter(|request| is_summary_request(request))
            .count(),
        1
    );
    assert!(requests[5]["messages"].to_string().contains(&facts.summary));
    let (native, fresh_receipt) = assert_resumed_completion(&host, &run, &old_receipt);
    assert_eq!(
        receipt(tool_feedback(&requests[7]).last().unwrap()).unwrap(),
        fresh_receipt
    );
    assert_compaction(&host.native_conversation(&run, "worker", 1), &facts);
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":CASE_SOURCE,"native_entry_compaction":true,"summary_requests":1,
            "plugin_binding":binding,"plugin_readonly_before_and_after":true,
            "before_host_kill":before_kill,"compaction_history":compaction,
            "kill_after_compaction_and_effect_before_model_reply":true,
            "same_invocation":true,"same_native_session":true,"restart_model_requests":0,
            "random_post_compaction_effect":effect,"post_effect_count":1,"seed_effect_count":1,
            "old_receipt_rejected":true,"old_receipt":old_receipt,"fresh_receipt":fresh_receipt,
            "native":native,"artifact_hashes_checked":true,"real_model_requests":0
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_cancelled_summarizer_stays_incomplete_then_resumes_from_plugin_and_workspace() {
    let facts = Facts::new();
    let aborted_summary = random_value("cancelled-summary-must-not-be-persisted");
    let seed_gate = Gate::new();
    let summary_gate = Gate::new();
    let provider = Provider::new(
        "goose-compaction-summary-cancel-resume",
        vec![
            facts.seed(),
            Step::text("The initial interrupted reply cannot finish the Graph.")
                .after("once")
                .gated(&seed_gate),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let binding = install_plugin(&host, &facts);
    let server = host.serve(&provider);
    let (run, original) = stop_after_seed(&host, &server, &provider, &facts, &seed_gate);
    let old_receipt = receipt(&original["tool_observation"]["result"]).unwrap();
    provider.append(vec![
        Step::summary(&aborted_summary)
            .expect_request_contains(&facts.workspace)
            .gated(&summary_gate),
    ]);
    resume(&server, &run);
    summary_gate.wait_entered();
    assert_eq!(provider.requests().len(), 3);
    assert!(is_summary_request(&provider.requests()[2]));
    let pending_history = host.native_conversation(&run, "worker", 1);
    assert!(!pending_history.to_string().contains(&aborted_summary));
    assert!(pending_history.as_array().unwrap().iter().any(|message| {
        message["metadata"]["agentVisible"] == true
            && message["content"].to_string().contains("toolRequest")
            && message["content"].to_string().contains("plugin-before.txt")
    }));
    assert_identity(&host, &run, &original);
    checkpoint(&host, &provider, &run, "while-native-summary-blocked");
    let (status, accepted) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{accepted}");
    server.wait_status(&run, "stopped");
    summary_gate.open();
    let stopped = host.record(&run);
    assert!(stopped["results"].get("worker").is_none());
    assert!(stopped["results"].get("verify").is_none());
    assert!(host.native_fact(&run, "worker", 1)["completion"].is_null());
    let stopped_history = host.native_conversation(&run, "worker", 1);
    assert!(!stopped_history.to_string().contains(&aborted_summary));
    assert!(!stopped_history.to_string().contains(&facts.summary));
    assert_eq!(
        host.base.workspace_files(&run, "effects.txt"),
        vec![b"once".to_vec()]
    );
    assert_eq!(
        host.base.workspace_files(&run, "workspace.txt"),
        vec![facts.workspace.as_bytes().to_vec()]
    );
    assert_eq!(provider.requests().len(), 3);
    checkpoint(&host, &provider, &run, "stopped-during-native-summary");
    fs::copy(
        provider.root.join("host-http.log"),
        provider.root.join("before-summary-cancel-restart.log"),
    )
    .unwrap();
    drop(server);
    let restarted = host.serve(&provider);
    restarted.wait_status(&run, "stopped");
    assert_eq!(host.record(&run), stopped);
    assert_eq!(provider.requests().len(), 3);
    provider.append(vec![
        Step::summary(&facts.summary).expect_request_contains(&facts.workspace),
        stale_completion(&old_receipt).expect_request_contains(&facts.summary),
        facts.reread().after("after observing business results"),
        complete("verify").after("plugin-workspace-reread"),
        Step::text("Inspected facts after cancelling the native summarizer and completed once."),
    ]);
    resume(&restarted, &run);
    restarted.wait_status(&run, "completed");
    checkpoint(
        &host,
        &provider,
        &run,
        "completed-after-summary-cancellation",
    );
    provider.assert_consumed();
    assert_artifacts(&host, &run, &facts);
    assert_identity(&host, &run, &original);
    let requests = provider.requests();
    assert_eq!(requests.len(), 8);
    assert_eq!(
        requests
            .iter()
            .filter(|request| is_summary_request(request))
            .count(),
        2
    );
    assert!(is_summary_request(&requests[3]));
    assert!(tool_feedback(&requests[4]).is_empty());
    let (native, fresh_receipt) = assert_resumed_completion(&host, &run, &old_receipt);
    assert_eq!(
        receipt(tool_feedback(&requests[6]).last().unwrap()).unwrap(),
        fresh_receipt
    );
    let history = host.native_conversation(&run, "worker", 1);
    assert!(!history.to_string().contains(&aborted_summary));
    let compaction = assert_compaction(&history, &facts);
    host.evidence(&provider, &run, json!({
        "case_source":CASE_SOURCE,"native_entry_compaction":true,"summary_requests":2,
        "summaries_applied":1,"cancelled_summary_not_persisted":true,
        "plugin_binding":binding,"plugin_readonly_before_and_after":true,
        "while_summary_blocked_history":pending_history,"stopped_history":stopped_history,
        "stop_settled_with_summary_gate_closed":true,"no_worker_or_downstream_false_completion":true,
        "same_invocation":true,"same_native_session":true,"restart_model_requests":0,
        "effect_count":1,"old_receipt_rejected":true,"old_receipt":old_receipt,
        "fresh_receipt":fresh_receipt,"compaction_history":compaction,"native":native,
        "artifact_hashes_checked":true,"real_model_requests":0
    }));
}

#[test]
#[ignore = "requires pinned Goose binary"]
fn native_goose_context_length_error_compacts_same_prompt_and_rereads_plugin_without_manual_resume()
{
    let facts = Facts::new();
    let provider = Provider::new(
        "goose-compaction-reactive-context-error",
        vec![
            facts.seed().with_usage(11, 7),
            Step::context_length_error()
                .after("once")
                .expect_request_contains(&facts.workspace),
            Step::summary(&facts.summary).expect_request_contains(&facts.workspace),
            facts.reread().expect_request_contains(&facts.summary),
            complete("verify").after("plugin-workspace-reread"),
            Step::text(
                "Goose recovered from context length and verified current Plugin and workspace.",
            ),
        ],
    );
    let host = Host::new(&graph()).default_runtime();
    let binding = install_plugin(&host, &facts);
    let response = host.run(&provider);
    checkpoint(
        &host,
        &provider,
        "fixture",
        "completed-or-failed-reactive-compaction",
    );
    assert_eq!(
        response["status"],
        "completed",
        "{response}; evidence {}",
        provider.root.display()
    );
    provider.assert_consumed();
    assert_artifacts(&host, "fixture", &facts);
    let requests = provider.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(
        requests
            .iter()
            .filter(|request| is_summary_request(request))
            .count(),
        1
    );
    assert!(is_summary_request(&requests[2]));
    assert!(tool_feedback(&requests[3]).is_empty());
    assert!(
        !requests[3]["messages"]
            .to_string()
            .contains(&facts.workspace)
    );
    let old_receipt = receipt(tool_feedback(&requests[1]).last().unwrap()).unwrap();
    let fresh_receipt = receipt(tool_feedback(&requests[4]).last().unwrap()).unwrap();
    assert_ne!(old_receipt, fresh_receipt);
    let (fact, native, _) = host.native_record("fixture", "worker", 1);
    assert_eq!(fact["key"]["invocation"], 1);
    assert!(fact["completion"].is_object());
    assert!(fact["reason"].is_null());
    assert_eq!(native["resume"], false);
    let calls = native["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3, "{native}");
    assert_eq!(calls[0]["tool"], "anchor_run");
    assert_eq!(calls[1]["tool"], "anchor_run");
    assert_eq!(calls[2]["tool"], "final_result");
    assert_eq!(calls[2]["arguments"]["observed_receipt"], fresh_receipt);
    assert_eq!(calls[2]["result"]["ok"], true);
    let history = host.native_conversation("fixture", "worker", 1);
    let compaction = assert_compaction(&history, &facts);
    assert_eq!(receipt(&compaction["old_result"]).unwrap(), old_receipt);
    let transport = fixture::read_json(provider.root.join("provider.json"));
    let errors = transport["exchanges"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|exchange| exchange["response"]["error"]["code"] == "context_length_exceeded")
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1, "{transport}");
    assert_eq!(errors[0]["response_status"], 400);
    host.evidence(
        &provider,
        "fixture",
        json!({
            "case_source":CASE_SOURCE,"native_reactive_compaction":true,"summary_requests":1,
            "context_length_error":errors[0],"manual_stop_or_resume":false,
            "experimental_loop_enabled":false,"same_prompt_and_invocation":true,
            "plugin_binding":binding,"plugin_readonly_before_and_after":true,
            "random_workspace":facts.workspace,"compaction_history":compaction,
            "old_receipt":old_receipt,"fresh_receipt":fresh_receipt,"effect_count":1,
            "artifact_hashes_checked":true,"real_model_requests":0
        }),
    );
}
