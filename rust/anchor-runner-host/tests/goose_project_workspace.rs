#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_platform_session::SessionStore;
use goose::{Host, HttpHost, Provider, Step, command, wait_until};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::MetadataExt, time::Duration};

const GRAPH: &str = "fixture-manager";
const USER: &str = "project-workspace-user";
const PROTECTED: &str = "host-only-protected-project-bytes";

fn graph() -> Value {
    json!({
        "objective":"Exercise direct project tools without a Codex Plugin or MCP",
        "entry":"wait_input",
        "agents":{"manager":{
            "model":"models.worker",
            "instructions":"Use authorized tools and inspect their results before replying. Keep the Manager workspace separate from the direct project."
        }},
        "ops":{
            "wait_input":{"host":{"operation":"session.wait_input"}},
            "reply":{"host":{"operation":"session.reply"}}
        },
        "nodes":[
            {"id":"wait_input","op":"wait_input"},
            {"id":"manager","agent":"manager","plugins":["wecom",{"id":"network","network":"denied"}]},
            {"id":"reply","op":"reply"}
        ],
        "edges":[
            {"from":"wait_input","to":"manager"},
            {"from":"manager","to":"reply"},
            {"from":"reply","to":"wait_input"}
        ]
    })
}

fn finish(summary: &str) -> Step {
    Step::tool(
        "final_result",
        json!({"summary":summary,"route":"reply","files":[]}),
    )
}

fn submit(server: &HttpHost, identity: &str, expected: &str) -> Value {
    let (status, response) = server.request_with_timeout(
        "POST",
        &format!("/channels/{GRAPH}/events"),
        Some(&json!({"event":{
            "source":"wecom","event_id":identity,"sender_id":USER,
            "conversation_id":"project-workspace-conversation","text":identity,
            "reply_target":"project-workspace-conversation","message_type":"text",
            "metadata":{"chat_type":"single","request_id":identity},"attachments":[]
        }})),
        Duration::from_secs(60),
    );
    assert_eq!(status, 200, "{response}");
    assert_eq!(response["text"], expected, "{response}");
    response
}

fn waiting(host: &Host, run: &str, turns: usize) -> Value {
    let mut saved = Value::Null;
    wait_until("Manager waiting for the next channel input", || {
        saved = host.record(run);
        saved["results"]["reply"]
            .as_array()
            .is_some_and(|results| results.len() == turns)
            && saved["frontier"][0]["node_id"] == "wait_input"
            && saved["frontier"][0]["key"]["invocation"] == turns + 1
    });
    assert_eq!(saved["results"]["manager"].as_array().unwrap().len(), turns);
    saved
}

#[test]
#[ignore = "requires pinned real Goose binary and local Bubblewrap"]
fn resident_project_tools_reach_real_directory_without_rebinding_source_workspace() {
    // The project is neither inside the fixture Host nor its managed workspace.
    let project_root = tempfile::tempdir().unwrap();
    let project = project_root.path().canonicalize().unwrap();
    fs::create_dir(project.join(".local")).unwrap();
    fs::write(project.join(".local/protected.txt"), PROTECTED).unwrap();
    fs::write(project.join("notes.txt"), "project-original\n").unwrap();

    let provider = Provider::new(
        "goose-project-workspace",
        vec![
            command("set -eu; test ! -e notes.txt; printf 'manager-original\n' > notes.txt; printf manager-only > manager-only.txt; printf manager-workspace-seeded")
                .expect_request_contains("project-workspace-seed"),
            finish("manager-seeded").after("manager-workspace-seeded"),
            Step::tool("project_read", json!({"path":"notes.txt"}))
                .expect_request_contains("project-workspace-exercise"),
            Step::tool(
                "project_edit",
                json!({"path":"notes.txt","old_string":"project-original","new_string":"project-edited"}),
            ).after("project-original"),
            Step::tool("project_read", json!({"path":"notes.txt"})).after("sha256_after"),
            Step::tool("project_read", json!({"path":".local/protected.txt"})).after("project-edited"),
            Step::tool(
                "project_edit",
                json!({"path":".local/protected.txt","old_string":"protected","new_string":"blocked-file-write"}),
            ).after("excluded"),
            Step::tool(
                "project_run",
                json!({"command":["sh","-c","set -eu; test \"$(pwd)\" = /workspace; test \"$(cat /workspace/notes.txt)\" = project-edited; test ! -e /workspace/manager-only.txt; test ! -e /workspace/.local/protected.txt; test -z \"$(ls -A /workspace/.local)\"; if cat /workspace/.local/protected.txt 2>/dev/null; then exit 41; fi; if (printf blocked-shell-write > /workspace/.local/blocked.txt) 2>/dev/null; then exit 42; fi; printf project-command > /workspace/command.txt; printf project-run-verified"]}),
            ).after("excluded"),
            Step::tool("anchor_read", json!({"path":"notes.txt"})).after("project-run-verified"),
            command("set -eu; test \"$(pwd)\" = /workspace; test \"$(cat notes.txt)\" = manager-original; test \"$(cat manager-only.txt)\" = manager-only; test ! -e command.txt; printf manager-workspace-unchanged")
                .after("manager-original"),
            finish("project-workspace-verified").after("manager-workspace-unchanged"),
        ],
    );
    let host = Host::new(&graph())
        .default_runtime()
        .with_bot(GRAPH, &[USER])
        .with_bot_credential("WECOM_BOT_ID", "fixture-project-bot")
        .with_bot_credential("WECOM_BOT_SECRET", "fixture-only-project-credential")
        .with_extra_environment([
            ("ANCHOR_RUNNER_GRAPH_NAME", GRAPH.to_owned()),
            ("ANCHOR_WECOM_WS_URL", "ws://127.0.0.1:1".to_owned()),
            // Exercise direct tool execution; discovery has its own fixture suite.
            ("ANCHOR_NODE_TOOL_DISCLOSURE", "0".to_owned()),
            (
                "ANCHOR_PROJECT_WORKSPACES",
                json!({GRAPH:{"owner":"local","path":project,"exclude":[".local"]}}).to_string(),
            ),
        ]);
    fixture::install_system_plugin(
        host.base.root.path(),
        anchor_graph_host::SystemPlugin::Wecom,
    );
    fs::write(
        host.base.root.path().join("bundle/manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[{"id":"wecom"},{"id":"network"}]})
            .to_string(),
    )
    .unwrap();
    assert!(!project.starts_with(host.base.root.path()));
    assert!(!project.starts_with(host.base.state_root()));
    let server = host.serve(&provider);

    let first = submit(&server, "project-workspace-seed", "manager-seeded");
    let run = first["run"].as_str().unwrap();
    let session = first["session"].as_str().unwrap();
    waiting(&host, run, 1);
    let workspace = host.base.workspace(run, "manager");
    let original_directory = fs::metadata(&workspace).unwrap();
    assert_ne!(workspace.canonicalize().unwrap(), project);
    assert_eq!(
        fs::read(workspace.join("notes.txt")).unwrap(),
        b"manager-original\n"
    );
    assert_eq!(
        fs::read(project.join("notes.txt")).unwrap(),
        b"project-original\n"
    );
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    let before = sessions
        .get_channel_assistant("local", session)
        .unwrap()
        .unwrap();
    assert_eq!(before.run_id, run);
    assert_eq!(before.work_node, "manager");
    assert_eq!(sessions.get("local", session).unwrap().graph, GRAPH);
    let first_native = host.native_fact(run, "manager", 1);

    let second = submit(
        &server,
        "project-workspace-exercise",
        "project-workspace-verified",
    );
    assert_eq!(second["session"], first["session"]);
    assert_eq!(second["run"], first["run"]);
    let saved = waiting(&host, run, 2);
    provider.assert_consumed();
    assert_eq!(
        fs::read(project.join("notes.txt")).unwrap(),
        b"project-edited\n"
    );
    assert_eq!(
        fs::read(project.join("command.txt")).unwrap(),
        b"project-command"
    );
    assert_eq!(
        fs::read_to_string(project.join(".local/protected.txt")).unwrap(),
        PROTECTED
    );
    assert!(!project.join(".local/blocked.txt").exists());
    assert!(!project.join("manager-only.txt").exists());
    assert_eq!(host.base.workspace(run, "manager"), workspace);
    let current_directory = fs::metadata(&workspace).unwrap();
    assert_eq!(current_directory.dev(), original_directory.dev());
    assert_eq!(current_directory.ino(), original_directory.ino());
    assert_eq!(
        fs::read(workspace.join("notes.txt")).unwrap(),
        b"manager-original\n"
    );
    assert_eq!(
        fs::read(workspace.join("manager-only.txt")).unwrap(),
        b"manager-only"
    );
    assert!(!workspace.join("command.txt").exists());
    let after = sessions
        .get_channel_assistant("local", session)
        .unwrap()
        .unwrap();
    assert_eq!(after.run_id, before.run_id);
    assert_eq!(after.work_node, before.work_node);
    assert_eq!(sessions.get("local", session).unwrap().graph, GRAPH);
    let second_native = host.native_fact(run, "manager", 2);
    assert_eq!(second_native["session_id"], first_native["session_id"]);
    assert_eq!(
        second_native["conversation_scope"],
        first_native["conversation_scope"]
    );
    assert!(
        second_native["session_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
    );

    let requests = provider.requests();
    assert_eq!(requests.len(), 11);
    for (index, stdout) in [
        (1, "manager-workspace-seeded"),
        (8, "project-run-verified"),
        (10, "manager-workspace-unchanged"),
    ] {
        let feedback = goose::tool_feedback(&requests[index]);
        let output: Value =
            serde_json::from_str(feedback.last().unwrap()["content"].as_str().unwrap()).unwrap();
        assert_eq!(output["ok"], true, "{output}");
        let result = &output["output"][0]["value"];
        assert_eq!(result["status"], "completed", "{output}");
        assert_eq!(result["exit_code"], 0, "{output}");
        assert_eq!(result["stdout"], stdout, "{output}");
        assert_eq!(result["incomplete"], false, "{output}");
    }
    for request in &requests {
        assert!(
            !request.to_string().contains(PROTECTED),
            "protected project bytes reached the model"
        );
        for (project_tool, anchor_tool) in [
            ("project_read", "anchor_read"),
            ("project_edit", "anchor_edit"),
            ("project_run", "anchor_run"),
        ] {
            assert_eq!(
                goose::tool_definition(request, project_tool).unwrap()["function"]["parameters"],
                goose::tool_definition(request, anchor_tool).unwrap()["function"]["parameters"],
            );
        }
        assert!(
            request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .all(|tool| { !tool["function"]["name"].as_str().unwrap().contains("codex") })
        );
    }
    let (status, stopped) = server.request("POST", &format!("/runs/{run}/stop"), None);
    assert_eq!(status, 202, "{stopped}");
    server.wait_status(run, "stopped");
    host.evidence_report(
        &provider,
        "tests/goose_project_workspace.rs",
        json!({
            "project_read_edit_run_reached_external_directory":true,
            "excluded_directory_hidden_from_file_and_command_tools":true,
            "source_channel_session_and_workspace_preserved":true,
            "codex_plugin_or_mcp_used":false
        }),
        json!({
            "run":saved,"responses":[first,second],"project":project,"source_workspace":workspace,
            "native_facts":[first_native,second_native],
            "native_goose_conversation":host.native_conversation(run, "manager", 2),
            "project_files":goose::file_inventory(&project),
            "source_workspace_files":goose::file_inventory(&workspace)
        }),
    );
}
