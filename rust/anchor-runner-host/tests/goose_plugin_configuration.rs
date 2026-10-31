#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use goose::{Gate, Host, HttpHost, Provider, Step, command};
use serde_json::{Value, json};
use std::{fs, time::Duration};

const CONFIG: &str = "/plugins/demo/configuration";
const FIRST: &str = "fixture-private-first";
const SECOND: &str = "fixture-private-second";
const AMBIENT: &str = "fixture-ambient-must-not-be-used";

fn update(server: &HttpHost, revision: u64, field: Value) -> Value {
    let (status, configuration) = server.request("GET", CONFIG, None);
    assert_eq!(status, 200, "{configuration}");
    let (status, saved) = server.request(
        "PUT",
        CONFIG,
        Some(&json!({
            "authorization_digest":configuration["authorization_digest"],
            "revision":revision,"fields":{"api_key":field}
        })),
    );
    assert_eq!(status, 200, "{saved}");
    for value in [FIRST, SECOND, AMBIENT] {
        assert!(
            !saved.to_string().contains(value),
            "secret in public response"
        );
    }
    saved
}

fn start(server: &HttpHost) -> String {
    let (status, accepted) =
        server.request("POST", "/trigger", Some(&json!({"graph":"configured-mcp"})));
    assert_eq!(status, 202, "{accepted}");
    accepted["run"].as_str().unwrap().into()
}

#[test]
#[ignore = "requires pinned Goose binary, built anchor-mcp-host http_fixture and local Bubblewrap"]
fn managed_configuration_rotates_atomically_without_restarting_or_changing_live_invocations() {
    let gate = Gate::new();
    let steps = |gated: bool| {
        let echo = Step::tool("demo-remote_echo", json!({"value":"managed"}));
        vec![
            if gated { echo.gated(&gate) } else { echo },
            command("cat configuration-log; printf 'ready:managed\n' > notes.md")
                .after("ready:managed"),
            Step::tool(
                "final_result",
                json!({"files":["notes.md","configuration-log"],"summary":"configuration checked"}),
            ),
        ]
    };
    let provider = Provider::new(
        "goose-plugin-managed-configuration",
        steps(true).into_iter().chain(steps(false)).collect(),
    );
    let host = Host::new(&json!({"entry":"idle","ops":{"idle":{"run":"true"}},
        "nodes":[{"id":"idle","op":"idle"}],"edges":[]}))
    .default_runtime()
    .with_extra_environment([
        ("ANCHOR_NODE_TOOL_DISCLOSURE", "0"),
        ("FIXTURE_TOKEN", AMBIENT),
    ]);
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
    let source = host.base.root.path().join("checkout/demo");
    fs::create_dir_all(&source).unwrap();
    fs::copy(binary, source.join("mcp-fixture")).unwrap();
    // The dummy credential is checked inside the sandbox and never returned by
    // the MCP. Only a non-secret version label is persisted as evidence.
    let script = "case \"$FIXTURE_TOKEN\" in fixture-private-first) version=first;; fixture-private-second) version=second;; *) exit 28;; esac; printf '%s:%s\\n' \"$1\" \"$version\" >> /workspace/configuration-log; exec /plugins/demo/mcp-fixture --stdio echo --prefix ready:";
    let server_definition = |name: &str| json!({"command":"sh","args":["-c",script,"fixture",name],"env_vars":["FIXTURE_TOKEN"]});
    fs::write(source.join("plugin.json"), json!({"name":"Managed configuration fixture",
        "configuration":{"fields":{"api_key":{"type":"secret"}},"environment":{
            "remote":{"FIXTURE_TOKEN":"api_key"},"attachments":{"FIXTURE_TOKEN":"api_key"}}},
        "mcpServers":{"remote":server_definition("remote"),"attachments":server_definition("attachments")}
    }).to_string()).unwrap();
    anchor_library::Library::new(host.base.root.path())
        .install_directory("demo", &source, false)
        .unwrap();
    let server = host.serve(&provider);
    update(&server, 0, json!({"action":"set","value":FIRST}));
    let (status, graph) = server.request("POST", "/graphs", Some(&json!({"name":"configured-mcp","definition":{
        "entry":"worker","agents":{"worker":{"model":"models.worker","writes":["notes.md","configuration-log"]}},
        "nodes":[{"id":"worker","agent":"worker","plugins":["demo"]}],"edges":[]
    }})));
    assert_eq!(status, 201, "{graph}");
    let first = start(&server);
    gate.wait_entered();
    let rotated = update(&server, 1, json!({"action":"set","value":SECOND}));
    assert_eq!(rotated["revision"], 2);
    gate.open();
    server.wait_status(&first, "completed");
    let second = start(&server);
    server.wait_status(&second, "completed");
    for (run, expected) in [(&first, "first"), (&second, "second")] {
        let record = host.record(run);
        let bytes = host.base.file(&record, "worker", "configuration-log");
        let log = String::from_utf8(bytes).unwrap();
        assert!(log.contains(&format!("remote:{expected}\n")), "{log}");
        assert!(log.contains(&format!("attachments:{expected}\n")), "{log}");
        assert!(
            !log.contains(if expected == "first" {
                "second"
            } else {
                "first"
            }),
            "{log}"
        );
    }
    let (status, tested) = server.request_with_timeout(
        "POST",
        &format!("{CONFIG}/test"),
        Some(&json!({
            "authorization_digest":rotated["authorization_digest"],"revision":2
        })),
        Duration::from_secs(30),
    );
    assert_eq!(status, 200, "{tested}");
    assert_eq!(tested["current"], true, "{tested}");
    assert_eq!(tested["results"].as_array().unwrap().len(), 2);
    for result in tested["results"].as_array().unwrap() {
        assert_eq!(result["status"], "verified", "{tested}");
    }
    let requests_before = provider.requests().len();
    update(&server, 2, json!({"action":"clear"}));
    let missing = start(&server);
    server.wait_status(&missing, "failed");
    assert_eq!(provider.requests().len(), requests_before);
    for request in provider.requests() {
        for value in [FIRST, SECOND, AMBIENT] {
            assert!(!request.to_string().contains(value), "secret reached model");
        }
    }
    let (status, _) = server.request("GET", "/ready", None);
    assert_eq!(status, 200);
    host.evidence(&provider, &second, json!({
        "case_source":"tests/goose_plugin_configuration.rs", "first_run":first,"cleared_run":missing,
        "atomic_consumers":true,"live_invocation_keeps_snapshot":true,"rotation_without_restart":true,
        "clear_does_not_fallback_to_environment":true,"secrets_absent_from_model":true,
        "management_probe_initializes_without_business_calls":true,
        "boundary":"Host, KernelScheduler, NodeExecutionPort, Goose ACP and sandboxed MCP; deterministic model transport"
    }));
}
