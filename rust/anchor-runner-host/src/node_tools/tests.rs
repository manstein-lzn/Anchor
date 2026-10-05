use super::*;
use anchor_sandbox_bwrap::BubblewrapPolicy;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

struct Inner;
impl ToolPort for Inner {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![ToolDefinition::new(
            ToolName::new("echo").unwrap(),
            "fixture",
            json!({}),
        )]
    }
    fn call<'a>(
        &'a self,
        name: &'a str,
        arguments: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move {
            if name == "echo" {
                Ok(vec![ToolResultContent::json(arguments)])
            } else {
                Err(ToolError::Unknown(name.into()))
            }
        })
    }
}

struct Fixture {
    directory: tempfile::TempDir,
    workspace: PathBuf,
    source: PathBuf,
    sandbox: Arc<BubblewrapSandbox>,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let source = directory.path().join("input.txt");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&source, "immutable-input").unwrap();
        let sandbox = Arc::new(
            BubblewrapSandbox::new(
                BubblewrapPolicy::new("bwrap", ["sh", "cat"])
                    .authorize_workspace_root(&workspace)
                    .authorize_readonly_input_root(directory.path())
                    .authorize_readonly_destination_root("/in"),
            )
            .expect("these integration tests require working Bubblewrap isolation"),
        );
        Self {
            directory,
            workspace,
            source,
            sandbox,
        }
    }
    fn tools(&self, cancellation: Cancellation) -> NodeTools {
        NodeTools::new(
            Arc::new(Inner),
            Arc::clone(&self.sandbox),
            self.workspace.clone(),
            vec![ReadOnlyInput::new(&self.source, "/in/producer/report.txt")],
            cancellation,
        )
    }
}
fn cancellation() -> Cancellation {
    Arc::new(AtomicBool::new(false))
}
async fn run(tools: &NodeTools, command: &[&str]) -> Value {
    tools
        .call(RUN_TOOL_NAME, json!({"command": command}))
        .await
        .unwrap()[0]
        .as_json()
        .unwrap()
        .clone()
}

#[tokio::test]
async fn graph_network_intent_is_applied_and_host_authority_still_limits_it() {
    use tokio::io::AsyncWriteExt;
    let fixture = Fixture::new();
    let refused = run(
        &fixture.tools(cancellation()).with_network(true),
        &["sh", "-c", "true"],
    )
    .await;
    assert_eq!(refused["status"], "not_executed");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"network-visible").await.unwrap();
    });
    let sandbox = Arc::new(
        BubblewrapSandbox::new(
            BubblewrapPolicy::new("bwrap", ["python3"])
                .authorize_workspace_root(&fixture.workspace)
                .allow_network(),
        )
        .unwrap(),
    );
    let script = format!(
        "import socket; s=socket.create_connection(('127.0.0.1',{port}), timeout=1); print(s.recv(64).decode())"
    );
    let tools = || {
        NodeTools::new(
            Arc::new(Inner),
            sandbox.clone(),
            fixture.workspace.clone(),
            vec![],
            cancellation(),
        )
    };
    let denied = run(&tools(), &["python3", "-c", &script]).await;
    assert_ne!(denied["exit_code"], 0, "{denied}");
    let allowed = run(&tools().with_network(true), &["python3", "-c", &script]).await;
    assert_eq!(allowed["exit_code"], 0, "{allowed}");
    assert_eq!(
        allowed["stdout"].as_str().unwrap().trim(),
        "network-visible"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn real_bwrap_reads_input_writes_workspace_and_rejects_input_mutation() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let result = run(&tools, &["cat", "/in/producer/report.txt"]).await;
    assert_eq!(result["status"], "completed");
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "immutable-input");
    let result = run(
        &tools,
        &[
            "sh",
            "-c",
            "printf artifact > report.txt; printf output; printf diagnostic >&2",
        ],
    )
    .await;
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "output");
    assert_eq!(result["stderr"], "diagnostic");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("report.txt")).unwrap(),
        "artifact"
    );
    let result = run(
        &tools,
        &["sh", "-c", "printf changed > /in/producer/report.txt"],
    )
    .await;
    assert_ne!(result["exit_code"], 0);
    assert_eq!(
        std::fs::read_to_string(&fixture.source).unwrap(),
        "immutable-input"
    );
}

#[tokio::test]
async fn real_bwrap_hides_host_files_and_enforces_command_allowlist() {
    let fixture = Fixture::new();
    let secret = fixture.directory.path().join("outside.txt");
    std::fs::write(&secret, "host-only-marker").unwrap();
    let tools = fixture.tools(cancellation());
    let result = run(&tools, &["cat", secret.to_str().unwrap()]).await;
    assert_ne!(result["exit_code"], 0);
    assert!(
        !result["stdout"]
            .as_str()
            .unwrap()
            .contains("host-only-marker")
    );
    for command in [
        vec!["printf", "forbidden"],
        vec!["/usr/bin/printf", "forbidden"],
    ] {
        let result = run(&tools, &command).await;
        assert_eq!(result["status"], "not_executed");
        assert!(!result["error"].as_str().unwrap().is_empty());
    }
    let result = run(&tools, &["/bin/cat", "/in/producer/report.txt"]).await;
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "immutable-input");
}

#[tokio::test]
async fn strict_arguments_and_other_tool_forwarding() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    assert_eq!(
        tools
            .definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["echo", RUN_TOOL_NAME]
    );
    assert!(!tools.is_read_only(RUN_TOOL_NAME));
    let result = tools
        .call("echo", json!({"arbitrary":"inner"}))
        .await
        .unwrap();
    assert_eq!(result[0].as_json(), Some(&json!({"arbitrary":"inner"})));
    assert!(
        matches!(tools.call("unknown", Value::Null).await, Err(ToolError::Unknown(name)) if name == "unknown")
    );
    for arguments in [
        json!({"command":"cat /in/producer/report.txt"}),
        json!({"command":[]}),
        json!({"command":["cat", 1]}),
        json!({"command":["cat"],"workspace":"/tmp"}),
        json!({}),
    ] {
        let result = tools.call(RUN_TOOL_NAME, arguments).await.unwrap();
        let result = result[0].as_json().unwrap();
        assert_eq!(result["status"], "not_executed");
        assert!(!result["error"].as_str().unwrap().is_empty());
    }
}

#[tokio::test]
async fn owned_node_tools_are_static_arc_ports_with_owned_invocation_context() {
    let fixture = Fixture::new();
    let tools: Arc<dyn ToolPort> = Arc::new(NodeTools::new(
        Arc::new(Inner),
        Arc::clone(&fixture.sandbox),
        fixture.workspace.clone(),
        vec![ReadOnlyInput::new(
            &fixture.source,
            "/in/producer/report.txt",
        )],
        cancellation(),
    ));

    assert_eq!(
        tools
            .definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["echo", RUN_TOOL_NAME]
    );
    let result = tools.call("echo", json!({"owned": true})).await.unwrap();
    assert_eq!(result[0].as_json(), Some(&json!({"owned": true})));
}

#[tokio::test]
async fn unauthorized_command_returns_feedback_and_next_authorized_cat_succeeds() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let result = run(&tools, &["od", "/in/producer/report.txt"]).await;
    assert_eq!(result["status"], "not_executed");
    assert!(result["error"].as_str().unwrap().contains("not authorized"));
    let result = run(&tools, &["cat", "/in/producer/report.txt"]).await;
    assert_eq!(result["status"], "completed");
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "immutable-input");
}

#[tokio::test]
async fn cancellation_remains_an_error_before_argument_validation() {
    let fixture = Fixture::new();
    let cancellation = cancellation();
    cancellation.store(true, Ordering::Relaxed);
    let tools = fixture.tools(cancellation);
    assert!(matches!(
        tools.call(RUN_TOOL_NAME, json!({})).await,
        Err(ToolError::Failed(reason)) if reason.contains("cancelled")
    ));
}

#[tokio::test]
async fn real_bwrap_cancellation_is_an_error_after_command_starts() {
    let fixture = Fixture::new();
    let cancellation = cancellation();
    let tools = fixture.tools(cancellation.clone());
    let command = tools.call(
        RUN_TOOL_NAME,
        json!({"command":["sh","-c","printf started > started; while :; do :; done"]}),
    );
    let stop = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.workspace.join("started").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("sandbox command must start before cancellation");
        cancellation.store(true, Ordering::Relaxed);
    };
    let (result, ()) = tokio::join!(command, stop);
    assert!(matches!(result, Err(ToolError::Failed(reason)) if reason.contains("cancelled")));
}
