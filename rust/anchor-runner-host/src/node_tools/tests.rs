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
    spill: Option<PathBuf>,
}
impl Fixture {
    fn new() -> Self {
        Self::build(false)
    }
    /// A fixture whose policy authorizes output retention.
    fn retaining() -> Self {
        Self::build(true)
    }
    fn build(retain: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        let source = directory.path().join("input.txt");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::write(&source, "immutable-input").unwrap();
        let spill_root = directory.path().join("spill");
        std::fs::create_dir(&spill_root).unwrap();
        let mut policy = BubblewrapPolicy::new("bwrap", ["sh", "cat"])
            .authorize_workspace_root(&workspace)
            .authorize_readonly_input_root(directory.path())
            .authorize_readonly_destination_root("/in");
        if retain {
            policy = policy
                .authorize_spill_root(&spill_root)
                .authorize_readonly_destination_root("/spill");
        }
        let sandbox = Arc::new(
            BubblewrapSandbox::new(policy)
                .expect("these integration tests require working Bubblewrap isolation"),
        );
        Self {
            directory,
            workspace,
            source,
            sandbox,
            spill: retain.then(|| spill_root.join("run")),
        }
    }
    /// The host directory holding retained output, if the policy authorizes it.
    fn spill_directory(&self) -> Option<PathBuf> {
        self.spill.clone()
    }
    fn tools(&self, cancellation: Cancellation) -> NodeTools {
        let tools = NodeTools::new(
            Arc::new(Inner),
            Arc::clone(&self.sandbox),
            self.workspace.clone(),
            vec![ReadOnlyInput::new(&self.source, "/in/producer/report.txt")],
            cancellation,
        );
        match &self.spill {
            Some(directory) => tools.with_spill(SpillDirectory::new(directory, "/spill")),
            None => tools,
        }
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
    let refused = fixture
        .tools(cancellation())
        .with_network(true)
        .call(RUN_TOOL_NAME, json!({"command":["sh","-c","true"]}))
        .await
        .expect_err("the fixture policy does not authorize network access")
        .to_string();
    assert!(refused.contains("not_executed"), "{refused}");
    assert!(refused.contains("not authorized"), "{refused}");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"network-visible").await.unwrap();
    });
    let shell = std::fs::canonicalize("/usr/bin/bash").unwrap();
    let executable = shell.to_str().unwrap();
    let sandbox = Arc::new(
        BubblewrapSandbox::new(
            BubblewrapPolicy::new(
                "bwrap",
                [shell.file_name().unwrap().to_str().unwrap(), "cat"],
            )
            .authorize_workspace_root(&fixture.workspace)
            .allow_network(),
        )
        .unwrap(),
    );
    let script = format!(
        "set -e; printf 'network-probe-started\\n'; exec 3<>/dev/tcp/127.0.0.1/{port}; cat <&3"
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
    let denied = run(&tools(), &[executable, "-c", &script]).await;
    assert_eq!(denied["status"], "completed", "{denied}");
    assert_eq!(
        denied["stdout"].as_str().unwrap().trim(),
        "network-probe-started",
        "{denied}"
    );
    assert_ne!(denied["exit_code"], 0, "{denied}");
    let allowed = run(&tools().with_network(true), &[executable, "-c", &script]).await;
    assert_eq!(allowed["exit_code"], 0, "{allowed}");
    assert_eq!(
        allowed["stdout"].as_str().unwrap().trim(),
        "network-probe-started\nnetwork-visible"
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
        let refused = tools
            .call(RUN_TOOL_NAME, json!({"command": command}))
            .await
            .expect_err("a command outside the allowlist must be refused")
            .to_string();
        // The refusal is a tool error (so the model sees isError) and it names
        // the commands that would work.
        assert!(refused.contains("not_executed"), "{refused}");
        assert!(refused.contains("not authorized"), "{refused}");
        assert!(
            refused.contains("Authorized commands in this sandbox: cat, sh"),
            "{refused}"
        );
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
        ["echo", RUN_TOOL_NAME, READ_TOOL_NAME, EDIT_TOOL_NAME]
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
        let refused = tools
            .call(RUN_TOOL_NAME, arguments)
            .await
            .expect_err("invalid arguments must be refused without executing")
            .to_string();
        assert!(refused.contains("not_executed"), "{refused}");
        assert!(refused.contains("invalid arguments"), "{refused}");
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
        ["echo", RUN_TOOL_NAME, READ_TOOL_NAME, EDIT_TOOL_NAME]
    );
    let result = tools.call("echo", json!({"owned": true})).await.unwrap();
    assert_eq!(result[0].as_json(), Some(&json!({"owned": true})));
}

#[tokio::test]
async fn unauthorized_command_returns_feedback_and_next_authorized_cat_succeeds() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let refused = tools
        .call(
            RUN_TOOL_NAME,
            json!({"command": ["od", "/in/producer/report.txt"]}),
        )
        .await
        .expect_err("od is not in the fixture allowlist")
        .to_string();
    assert!(refused.contains("not authorized"), "{refused}");
    assert!(refused.contains("cat, sh"), "{refused}");
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

#[tokio::test]
async fn run_description_states_limits_authorized_commands_and_retention() {
    let fixture = Fixture::retaining();
    let tools = fixture.tools(cancellation());
    let definitions = tools.definitions();
    let definition = definitions
        .iter()
        .find(|tool| tool.name == RUN_TOOL_NAME)
        .expect("anchor_run is defined");
    let description = &definition.description;
    assert!(
        description.contains("Authorized commands: cat, sh"),
        "{description}"
    );
    assert!(description.contains("killed after 30s"), "{description}");
    assert!(description.contains("64 KiB preview"), "{description}");
    assert!(description.contains("/spill"), "{description}");
    assert!(
        description.contains("non-zero exit code is a normal result"),
        "{description}"
    );
    // Without an authorized retention root the description must not promise it.
    let plain = Fixture::new().tools(cancellation());
    let definitions = plain.definitions();
    let definition = definitions
        .iter()
        .find(|tool| tool.name == RUN_TOOL_NAME)
        .unwrap();
    assert!(
        definition
            .description
            .contains("longer output is discarded"),
        "{}",
        definition.description
    );
}

#[tokio::test]
async fn long_output_is_retained_read_only_and_readable_when_authorized() {
    let fixture = Fixture::retaining();
    let tools = fixture.tools(cancellation());
    let result = run(
        &tools,
        &[
            "sh",
            "-c",
            "i=0; while [ $i -lt 4000 ]; do echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; i=$((i+1)); done",
        ],
    )
    .await;
    assert_eq!(result["status"], "completed", "{result}");
    assert_eq!(result["incomplete"], false, "{result}");
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("[truncated:66464-bytes;"),
        "{result}"
    );
    assert!(
        result["hint"].as_str().unwrap().contains("full_output"),
        "{result}"
    );
    let retained = result["full_output"]
        .as_array()
        .expect("retained paths")
        .clone();
    assert_eq!(retained.len(), 1, "{result}");
    let visible = retained[0].as_str().unwrap();
    assert!(visible.starts_with("/spill/"), "{visible}");

    // The stream is covered by the preview plus the retained remainder, so
    // nothing was lost: 132000 bytes written, 65536 shown, 66464 retained.
    let remainder = 4000 * 33 - 65536;
    let directory = fixture.spill_directory().unwrap();
    let files = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(
        std::fs::read(&files[0]).unwrap().len(),
        remainder,
        "the retained remainder must complete the preview"
    );

    // ...and the sandbox can read it with an authorized command.
    let probe = format!("wc -c < {visible}");
    let read = run(&tools, &["sh", "-c", probe.as_str()]).await;
    assert_eq!(
        read["stdout"].as_str().unwrap().trim(),
        remainder.to_string(),
        "{read}"
    );
}

#[tokio::test]
async fn without_an_authorized_spill_root_long_output_is_discarded() {
    let fixture = Fixture::new();
    let tools = fixture
        .tools(cancellation())
        .with_spill(SpillDirectory::new(
            fixture.directory.path().join("unauthorized"),
            "/spill",
        ));
    let result = run(
        &tools,
        &[
            "sh",
            "-c",
            "i=0; while [ $i -lt 4000 ]; do echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; i=$((i+1)); done",
        ],
    )
    .await;
    assert_eq!(result["incomplete"], true, "{result}");
    assert!(result.get("full_output").is_none(), "{result}");
    assert!(
        result["hint"].as_str().unwrap().contains("not retained"),
        "{result}"
    );
}

#[tokio::test]
async fn timed_out_commands_report_a_narrowing_hint() {
    let fixture = Fixture::retaining();
    let mut tools = fixture.tools(cancellation());
    // The command wall clock is a constant in production; the test shortens it.
    tools.timeout = Duration::from_secs(1);
    let result = run(&tools, &["sh", "-c", "sleep 5"]).await;
    assert_eq!(result["status"], "timed_out", "{result}");
    let hint = result["hint"].as_str().expect("a timeout explains itself");
    assert!(hint.contains("wall-clock limit"), "{hint}");
    assert!(hint.contains("narrow"), "{hint}");
}

#[test]
fn prune_spill_keeps_only_the_newest_retained_streams() {
    let directory = tempfile::tempdir().unwrap();
    for index in 0..5 {
        std::fs::write(directory.path().join(format!("stdout-{index}.spill")), "x").unwrap();
    }
    std::fs::write(directory.path().join("notes.txt"), "keep me").unwrap();
    prune_spill(directory.path(), 2).unwrap();
    let mut retained = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    retained.sort();
    assert_eq!(retained, ["notes.txt", "stdout-3.spill", "stdout-4.spill"]);
}

async fn call_tool(tools: &NodeTools, name: &str, arguments: Value) -> Value {
    tools
        .call(name, arguments)
        .await
        .unwrap_or_else(|error| panic!("{name} failed: {error}"))[0]
        .as_json()
        .unwrap()
        .clone()
}

async fn call_tool_error(tools: &NodeTools, name: &str, arguments: Value) -> String {
    tools
        .call(name, arguments)
        .await
        .expect_err("the tool must refuse")
        .to_string()
}

#[tokio::test]
async fn read_returns_numbered_pages_and_a_content_hash() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    std::fs::write(
        fixture.workspace.join("notes.txt"),
        "one\ntwo\nthree\nfour\n",
    )
    .unwrap();
    let first = call_tool(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert_eq!(first["total_lines"], 4, "{first}");
    assert_eq!(first["lines"][0], json!({"line":1,"text":"one"}), "{first}");
    assert_eq!(first["truncated"], false, "{first}");
    assert_eq!(first["next_offset"], Value::Null, "{first}");
    let sha = first["sha256"].as_str().unwrap().to_owned();
    assert_eq!(sha.len(), 64);

    let page = call_tool(
        &tools,
        READ_TOOL_NAME,
        json!({"path":"notes.txt","offset":2,"limit":1}),
    )
    .await;
    assert_eq!(page["lines"][0], json!({"line":3,"text":"three"}), "{page}");
    assert_eq!(page["truncated"], true, "{page}");
    assert_eq!(page["next_offset"], 3, "{page}");
    assert_eq!(page["sha256"], sha, "the hash covers the whole file");
}

#[tokio::test]
async fn file_tools_stay_inside_the_workspace() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    std::fs::write(fixture.directory.path().join("outside.txt"), "host only").unwrap();
    for path in ["/etc/passwd", "../outside.txt", "sub/../../outside.txt", ""] {
        let error = call_tool_error(&tools, READ_TOOL_NAME, json!({"path": path})).await;
        assert!(!error.contains("host only"), "{error}");
        assert!(
            error.contains("not_executed") || error.contains("outside") || error.contains("empty"),
            "{path}: {error}"
        );
    }
    // A symlink that leaves the workspace is refused even though it is relative.
    std::os::unix::fs::symlink(
        fixture.directory.path().join("outside.txt"),
        fixture.workspace.join("link.txt"),
    )
    .unwrap();
    let error = call_tool_error(&tools, READ_TOOL_NAME, json!({"path":"link.txt"})).await;
    assert!(error.contains("outside /workspace"), "{error}");
}

#[tokio::test]
async fn read_reports_binary_files_without_lines() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    std::fs::write(fixture.workspace.join("blob.bin"), [0u8, 1, 2, 3]).unwrap();
    let result = call_tool(&tools, READ_TOOL_NAME, json!({"path":"blob.bin"})).await;
    assert_eq!(result["binary"], true, "{result}");
    assert!(result.get("lines").is_none(), "{result}");
}

#[tokio::test]
async fn whole_file_edits_require_the_base_hash_and_can_create_files() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    // A missing parent directory is an explicit refusal, not an implicit mkdir.
    let missing = call_tool_error(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"new/report.txt","content":"first\n"}),
    )
    .await;
    assert!(missing.contains("directory does not exist"), "{missing}");
    std::fs::create_dir(fixture.workspace.join("new")).unwrap();
    let created = call_tool(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"new/report.txt","content":"first\n"}),
    )
    .await;
    assert_eq!(created["created"], true, "{created}");
    let sha = created["sha256_after"].as_str().unwrap().to_owned();

    // Replacing the whole file without the current hash is refused.
    let error = call_tool_error(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"new/report.txt","content":"blind\n"}),
    )
    .await;
    assert!(
        error.contains("base_sha256") && error.contains("required"),
        "{error}"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("new/report.txt")).unwrap(),
        "first\n"
    );

    // A stale hash is refused too, and the refusal names the current hash.
    let stale = "0".repeat(64);
    let error = call_tool_error(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"new/report.txt","content":"stale\n","base_sha256":stale}),
    )
    .await;
    assert!(error.contains(&sha), "{error}");

    let replaced = call_tool(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"new/report.txt","content":"second\n","base_sha256":sha}),
    )
    .await;
    assert_eq!(replaced["created"], false, "{replaced}");
    assert_eq!(replaced["bytes_after"], 7, "{replaced}");
}

#[tokio::test]
async fn counted_edits_replace_exactly_the_expected_matches() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    std::fs::write(
        fixture.workspace.join("code.rs"),
        "let a = 1;\nlet b = 2;\n",
    )
    .unwrap();
    let ambiguous = call_tool_error(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"code.rs","old_string":"let ","new_string":"let mut "}),
    )
    .await;
    assert!(ambiguous.contains("occurs 2 time(s)"), "{ambiguous}");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("code.rs")).unwrap(),
        "let a = 1;\nlet b = 2;\n",
        "a refused edit must not change the file"
    );

    let applied = call_tool(
        &tools,
        EDIT_TOOL_NAME,
        json!({
            "path":"code.rs","old_string":"let ","new_string":"let mut ","expected_matches":2
        }),
    )
    .await;
    assert_eq!(applied["replaced"], 2, "{applied}");
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("code.rs")).unwrap(),
        "let mut a = 1;\nlet mut b = 2;\n"
    );

    // A targeted edit can also carry the base hash, and a wrong hash is refused.
    let current = call_tool(&tools, READ_TOOL_NAME, json!({"path":"code.rs"})).await;
    let sha = current["sha256"].as_str().unwrap().to_owned();
    let error = call_tool_error(
        &tools,
        EDIT_TOOL_NAME,
        json!({
            "path":"code.rs","old_string":"let mut a","new_string":"let a",
            "base_sha256":"1".repeat(64)
        }),
    )
    .await;
    assert!(error.contains(&sha), "{error}");
    let reverted = call_tool(
        &tools,
        EDIT_TOOL_NAME,
        json!({
            "path":"code.rs","old_string":"let mut a","new_string":"let a","base_sha256":sha
        }),
    )
    .await;
    assert_eq!(reverted["replaced"], 1, "{reverted}");
}

#[tokio::test]
async fn file_tools_declare_read_only_semantics() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    assert!(tools.is_read_only(READ_TOOL_NAME));
    assert!(!tools.is_read_only(EDIT_TOOL_NAME));
    assert!(!tools.is_read_only(RUN_TOOL_NAME));
    // The inner fixture port is not read-only, and the wrapper does not change that.
    assert!(!tools.is_read_only("echo"));
}
