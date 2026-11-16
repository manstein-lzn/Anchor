//! Real Goose, Host, Gateway and pinned CLI; only model/platform transport is fake.
use super::*;

fn feedback(request: &Value) -> Value {
    let messages = goose::tool_feedback(request);
    let content = messages.last().unwrap()["content"].as_str().unwrap();
    let result: Value = serde_json::from_str(content).unwrap();
    assert_eq!(result["ok"], true, "{result}");
    assert_eq!(result["output"][0]["type"], "json", "{result}");
    result["output"][0]["value"].clone()
}

#[test]
#[ignore = "requires pinned Goose, relay, network helper and sibling Gateway/lark-cli 1.0.100; no production endpoints"]
fn authorized_cli_is_discovered_and_runs_official_help_and_skills_in_one_turn() {
    let cli = fixture::host_binary().with_file_name("lark-cli");
    let cli_digest = goose::digest(&cli);
    assert_eq!(cli_digest, anchor_distribution::LARK_CLI_SHA256);
    let provider = Provider::new(
        "goose-feishu-official-cli",
        vec![
            Step::tool("anchor_tools", json!({"name":"feishu_cli"}))
                .expect_request_contains(FIRST_INPUT),
            Step::tool("anchor_tool_detail", json!({"name":"feishu_cli"})).after("feishu_cli"),
            Step::tool(
                "anchor_tools_call",
                json!({"name":"feishu_cli","arguments":{"argv":["--help"]}}),
            )
            .after("argv"),
            Step::tool(
                "anchor_tools_call",
                json!({"name":"feishu_cli","arguments":{"argv":["skills","list"]}}),
            )
            .after("Usage:"),
            Step::tool(
                "final_result",
                json!({"summary":FIRST_REPLY,"route":"reply","files":[]}),
            )
            .after("lark-doc"),
        ],
    );
    let platform = Platform::new();
    let host = Host::new(&json!({
        "entry":"work","ops":{"work":{"run":"true"}},
        "nodes":[{"id":"work","op":"work"}],"edges":[]
    }))
    .default_runtime()
    .with_extra_environment([
        ("ANCHOR_FEISHU_API_BASE_URL", platform.api_base_url.as_str()),
        ("ANCHOR_NODE_TOOL_DISCLOSURE", "1"),
    ]);
    let (gateway, installed) = install_plugin(&host);
    let server = host.serve(&provider);
    let binding = configure_bot_members(
        &server,
        &graph(),
        &json!({"users":[USER],"send_users":[USER],"private_only":true,"cli_enabled":true}),
    );
    assert_eq!(binding["cli_enabled"], true);
    assert!(provider.requests().is_empty());
    platform.send(1, FIRST_MESSAGE, FIRST_INPUT);
    let ack = platform.wait_ack(1);
    let sessions =
        SessionStore::open(host.base.state_root().join("platform/sessions.sqlite")).unwrap();
    wait_until("CLI turn admitted into its Feishu Session", || {
        sessions
            .list("local")
            .unwrap()
            .first()
            .is_some_and(|session| !session.run_ids.is_empty())
    });
    let session = sessions.list("local").unwrap().remove(0);
    let run = &session.run_ids[0];
    let completed = waiting(&host, &sessions, &session.id, run, 1);
    provider.assert_consumed();
    let requests = provider.requests();
    assert_eq!(requests.len(), 5);
    let discovered = feedback(&requests[1]);
    assert_eq!(discovered["tools"].as_array().unwrap().len(), 1);
    assert_eq!(discovered["tools"][0]["name"], "feishu_cli");
    let detail = feedback(&requests[2]);
    assert_eq!(detail["name"], "feishu_cli");
    assert_eq!(detail["complete"], true);
    assert_eq!(detail["parameters"]["required"], json!(["argv"]));
    assert_eq!(detail["parameters"]["additionalProperties"], false);
    for field in ["app_id", "token", "env", "bot", "user"] {
        assert!(detail["parameters"]["properties"].get(field).is_none());
    }
    let help = feedback(&requests[3]);
    let skills = feedback(&requests[4]);
    for result in [&help, &skills] {
        assert_eq!(result["accepted"], true, "{result}");
        assert_eq!(result["exit_code"], 0, "{result}");
        assert_eq!(result["identity"], "bot");
        assert_eq!(result["app_id"], APP_ID);
        assert_eq!(result["files"], json!([]));
        assert!(result.get("replayed").is_none());
    }
    assert!(help["stdout"].as_str().unwrap().contains("Usage:"));
    let catalog: Value = serde_json::from_str(skills["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(catalog["ok"], true, "{catalog}");
    assert!(
        catalog["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["name"] == "lark-doc")
    );
    assert_ne!(help["request_id"], skills["request_id"]);
    // The real Gateway, not a fixed JSON response, confirms each native call.
    let database = host
        .base
        .state_root()
        .join("channels/graphs")
        .join(&format!("{:x}", Sha256::digest(GRAPH.as_bytes()))[..16])
        .join("delivery.sqlite");
    let ledger =
        rusqlite::Connection::open_with_flags(database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let receipts = ledger
        .prepare("SELECT request_id,status FROM deliveries WHERE kind='cli' ORDER BY request_id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(receipts.len(), 2);
    for (id, status) in &receipts {
        assert_eq!(status, "confirmed");
        assert!(
            id == help["request_id"].as_str().unwrap()
                || id == skills["request_id"].as_str().unwrap()
        );
    }
    let turns = sessions.list_turns("local", &session.id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, TurnStatus::Completed);
    let facts = platform.snapshot();
    assert_eq!(facts["replies"].as_array().unwrap().len(), 1);
    assert_eq!(facts["replies"][0]["reply_to"], FIRST_MESSAGE);
    assert_eq!(facts["notifications"], json!([]));
    assert_eq!(facts["uploads"], json!([]));
    for request in &requests {
        goose::tool_definition(request, "anchor_tools").unwrap();
        assert!(goose::tool_definition(request, "feishu_cli").is_err());
        assert!(!request.to_string().contains(APP_SECRET));
        assert!(!request.to_string().contains("fixture-tenant-token"));
    }
    let native = host.native_conversation(run, "assistant", 1);
    assert!(native.to_string().contains("feishu_cli"));
    assert!(native.to_string().contains("lark-doc"));
    host.evidence_report(
        &provider,
        "tests/goose_feishu/cli.rs",
        json!({
            "cli_enabled":true,"discovered_via_real_goose":true,
            "supervised_real_gateway":true,"official_cli_executed":true,
            "gateway_response_stubbed":false,"commands":[["--help"],["skills","list"]],
            "confirmed_cli_receipts":receipts,"completed_turns":1,
            "real_feishu_requests":0,"production_data_used":false,"durable_admission_ack":ack
        }),
        json!({
            "cli_version":anchor_distribution::LARK_CLI_VERSION,"cli_binary_sha256":cli_digest,
            "gateway_binary_sha256":goose::digest(&gateway),"installed_plugin":installed,
            "binding":binding,"platform":facts,"run":completed,"session":session,
            "help_result":help,"skills_result":skills,"native_goose_conversation":native
        }),
    );
}
