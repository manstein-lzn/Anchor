#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_library::Library;
use goose::{Host, Provider, Step, command};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

const CASE_SOURCE: &str = "tests/goose_plugin_mcp.rs";

fn host() -> Host {
    Host::new(&json!({
        "entry":"idle", "ops":{"idle":{"run":"true"}},
        "nodes":[{"id":"idle","op":"idle"}], "edges":[]
    }))
    .default_runtime()
    .with_extra_environment([("ANCHOR_NODE_TOOL_DISCLOSURE", "0")])
}

fn install(host: &Host, command: &str, args: Value, binary: Option<&PathBuf>) {
    let root = host.base.root.path();
    let source = root.join("checkout/demo");
    fs::create_dir_all(&source).unwrap();
    if let Some(binary) = binary {
        fs::copy(binary, source.join("mcp-fixture")).unwrap();
    }
    fs::write(
        source.join("plugin.json"),
        json!({
            "name":"Graph MCP regression fixture",
            "mcpServers":{"service":{"command":command,"args":args}}
        })
        .to_string(),
    )
    .unwrap();
    Library::new(root)
        .install_directory("demo", &source, false)
        .unwrap();
}

fn start(server: &goose::HttpHost) -> String {
    let (status, created) = server.request(
        "POST",
        "/graphs",
        Some(&json!({
            "name":"mcp-node",
            "definition":{
                "entry":"worker",
                "agents":{"worker":{"model":"models.worker", "writes":["notes.md"]}},
                "nodes":[{"id":"worker","agent":"worker","plugins":["demo"]}], "edges":[]
            }
        })),
    );
    assert_eq!(status, 201, "{created}");
    let (status, accepted) = server.request("POST", "/trigger", Some(&json!({"graph":"mcp-node"})));
    assert_eq!(status, 202, "{accepted}");
    accepted["run"].as_str().unwrap().to_owned()
}

#[test]
#[ignore = "requires pinned Goose binary and local Bubblewrap"]
fn graph_mcp_startup_errors_settle_without_repolling_completed_resolution() {
    for (case, program, args, expected) in [
        (
            "authorization",
            "anchor-unapproved-mcp-command",
            json!([]),
            "not authorized",
        ),
        (
            "handshake",
            "sh",
            json!(["-c", "exit 23"]),
            "failed to bind",
        ),
    ] {
        let provider = Provider::new(&format!("goose-graph-mcp-{case}-error"), vec![]);
        let host = host();
        install(&host, program, args, None);
        let server = host.serve(&provider);
        let run = start(&server);
        let deadline = Instant::now() + Duration::from_secs(5);
        let detail = loop {
            let (status, detail) = server.request("GET", &format!("/runs/{run}"), None);
            assert_eq!(status, 200, "{detail}");
            if detail["active"] == false || Instant::now() >= deadline {
                break detail;
            }
            thread::sleep(Duration::from_millis(20));
        };
        let log = fs::read_to_string(provider.root.join("host-http.log")).unwrap();
        fs::write(provider.root.join("startup-result.json"), serde_json::to_vec_pretty(&json!({
            "case_source":CASE_SOURCE,"host_binary_sha256":goose::digest(&host.base.root.path().join("anchor-runner-host")),
            "run":detail,"provider_requests":provider.requests(),"host_log_contains_panic":log.contains("panicked"),
            "production_data_used":false,"real_model_requests":0
        })).unwrap()).unwrap();
        assert_eq!(detail["projection"]["outcome"], "failed", "{detail}\n{log}");
        assert_eq!(detail["active"], false, "{detail}");
        assert!(
            detail["projection"]["reason"]
                .as_str()
                .unwrap()
                .contains(expected),
            "{detail}"
        );
        assert!(!log.contains("panicked"), "{log}");
        assert!(provider.requests().is_empty());
        let (status, _) = server.request("GET", "/ready", None);
        assert_eq!(status, 200);
    }
}

#[test]
#[ignore = "requires pinned Goose binary, built anchor-mcp-host http_fixture and local Bubblewrap"]
fn graph_agent_calls_its_sandboxed_stdio_plugin_and_publishes_an_artifact() {
    let executable = std::env::current_exe().unwrap();
    let binary = executable
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("examples/http_fixture");
    assert!(
        binary.is_file(),
        "build anchor-mcp-host --example http_fixture first"
    );
    let provider = Provider::new(
        "goose-graph-mcp-stdio-success",
        vec![
            Step::tool("demo-service_echo", json!({"value":"graph-node"})),
            command("printf 'ready:graph-node\n' > notes.md; cat notes.md")
                .after("ready:graph-node"),
            Step::tool(
                "final_result",
                json!({"files":["notes.md"],"summary":"fixture complete"}),
            )
            .after("ready:graph-node"),
        ],
    );
    let host = host();
    install(
        &host,
        "mcp-fixture",
        json!(["--stdio", "echo", "--prefix", "ready:"]),
        Some(&binary),
    );
    let server = host.serve(&provider);
    let run = start(&server);
    server.wait_status(&run, "completed");
    let record = host.record(&run);
    assert_eq!(
        host.base.file(&record, "worker", "notes.md"),
        b"ready:graph-node\n"
    );
    assert!(
        host.native_conversation(&run, "worker", 1)
            .to_string()
            .contains("demo-service_echo")
    );
    let log = fs::read_to_string(provider.root.join("host-http.log")).unwrap();
    assert!(!log.contains("panicked"), "{log}");
    host.evidence(&provider, &run, json!({
        "case_source":CASE_SOURCE,"sandboxed_stdio_plugin_called":true,"artifact_checked":true,
        "boundary":"real Host, KernelScheduler, NodeExecutionPort, Goose ACP and Plugin MCP; deterministic model transport"
    }));
}
