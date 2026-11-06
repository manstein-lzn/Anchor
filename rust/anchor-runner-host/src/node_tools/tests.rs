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
async fn direct_rustc_uses_the_frozen_grant_and_rejects_unrelated_host_commands() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let bin = fixture.directory.path().join("shared-rust/bin");
    let home = fixture.directory.path().join("shared-rust/rustup");
    let cargo = fixture.directory.path().join("shared-rust/cargo");
    let other = fixture.directory.path().join("other/bin");
    for directory in [&bin, &home, &cargo, &other] {
        std::fs::create_dir_all(directory).unwrap();
    }
    for name in ["cargo", "rustup", "rustc"] {
        let entry = bin.join(name);
        std::fs::write(
            &entry,
            "#!/bin/sh\nset -eu\nif [ \"$1\" = --version ]; then printf '%s|%s|%s\\n' \"$RUSTUP_TOOLCHAIN\" \"$CARGO_HOME\" \"$CARGO_NET_OFFLINE\"; else exit 1; fi\n",
        )
        .unwrap();
        std::fs::set_permissions(&entry, std::fs::Permissions::from_mode(0o555)).unwrap();
    }
    std::fs::copy("/usr/bin/false", other.join("rustc")).unwrap();
    std::fs::copy("/usr/bin/true", bin.join("unrelated")).unwrap();
    let pixi = fixture.directory.path().join("host-pixi");
    std::fs::copy("/usr/bin/true", &pixi).unwrap();
    std::fs::write(cargo.join("credentials.toml"), "private-host-credential").unwrap();
    let snapshot = crate::runtime_configuration::RuntimeSnapshot {
        python: None,
        python_enabled: false,
        rust: Some(crate::runtime_configuration::RustInstallation {
            proxy: bin.join("rustup"),
            home: home.clone(),
            cargo: cargo.clone(),
            toolchain: Some("stable-x86_64-unknown-linux-gnu".into()),
        }),
        pixi: Some(pixi),
        provisioner: None,
    };
    let environment = crate::tool_environment::ToolEnvironment::default()
        .with_runtime_snapshot(&snapshot)
        .unwrap();
    let tools = fixture.tools(cancellation()).with_environment(environment);
    for command in [
        "rustc".to_owned(),
        bin.join("rustc").to_string_lossy().into_owned(),
    ] {
        let result = run(&tools, &[&command, "--version"]).await;
        assert_eq!(result["exit_code"], 0, "{result}");
        assert_eq!(
            result["stdout"],
            "stable-x86_64-unknown-linux-gnu|/workspace/.anchor-rust/cargo|true\n"
        );
    }
    let definitions = tools.definitions();
    let description = &definitions
        .iter()
        .find(|definition| definition.name == RUN_TOOL_NAME)
        .unwrap()
        .description;
    assert!(
        description.contains("Authorized commands: cargo, cat, pixi, rustc, rustup, sh"),
        "{description}"
    );
    for command in [
        "/usr/bin/true".to_owned(),
        other.join("rustc").to_string_lossy().into_owned(),
    ] {
        let refused = tools
            .call(RUN_TOOL_NAME, json!({"command": [command, "--version"]}))
            .await
            .expect_err("an unrelated host executable must remain unavailable")
            .to_string();
        assert!(refused.contains("not_executed"), "{refused}");
    }
    // The existing Rust bin PATH grant only mounts known proxies. A sibling
    // file on the Host remains unavailable inside the sandbox.
    let unavailable = tools
        .call(
            RUN_TOOL_NAME,
            json!({"command": [bin.join("unrelated").to_string_lossy(), "--version"]}),
        )
        .await;
    let result = unavailable.expect("the sandbox must report the unavailable executable");
    let result = result[0].as_json().unwrap();
    assert!(
        result["exit_code"].as_i64().is_some_and(|code| code != 0),
        "an ungranted sibling executable ran successfully: {result}"
    );
    assert_eq!(result["stdout"], "");
    let refused = fixture
        .tools(cancellation())
        .call(RUN_TOOL_NAME, json!({"command": ["rustc", "--version"]}))
        .await
        .expect_err("an unconfigured Turn must not gain Rust execution")
        .to_string();
    assert!(refused.contains("not_executed"), "{refused}");
    let shared_home = home.to_string_lossy();
    let shared_cargo = cargo.to_string_lossy();
    let shared_rustc = bin.join("rustc").to_string_lossy().into_owned();
    let script = format!(
        "set -eu\ntest ! -e '{shared_cargo}/credentials.toml'\nif printf pollution > '{shared_home}/pollution'; then exit 10; fi\nif printf pollution > '{shared_rustc}'; then exit 11; fi\nif printf pollution > /tools/anchor-pilot-rust/bin/rustc; then exit 12; fi\nprintf readonly"
    );
    let result = run(&tools, &["sh", "-c", &script]).await;
    assert_eq!(result["exit_code"], 0, "{result}");
    assert_eq!(result["stdout"], "readonly");
    assert!(!home.join("pollution").exists());
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
    let route = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    route.connect("192.0.2.1:9").unwrap();
    let host_address = route.local_addr().unwrap().ip();
    let listener = tokio::net::TcpListener::bind((host_address, 0))
        .await
        .unwrap();
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
        "set -e; printf 'network-probe-started\\n'; exec 3<>/dev/tcp/{host_address}/{port}; cat <&3"
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
async fn generated_graph_command_limit_keeps_files_and_plugins_without_exposing_commands() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation()).with_command_authority(false);
    assert_eq!(
        tools
            .definitions()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["echo", READ_TOOL_NAME, EDIT_TOOL_NAME]
    );
    let result = tools
        .call(RUN_TOOL_NAME, json!({"command":["sh","-c","touch bypass"]}))
        .await;
    assert!(result.unwrap_err().to_string().contains("not authorized"));
    assert!(!fixture.workspace.join("bypass").exists());
    tools
        .call(
            EDIT_TOOL_NAME,
            json!({"path":"report.md","content":"report"}),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("report.md")).unwrap(),
        "report"
    );
    assert!(tools.call("echo", json!({})).await.is_ok());
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
async fn requested_timeout_reports_the_actual_limit_and_preserves_node_ceiling() {
    let fixture = Fixture::new();
    let node = fixture.tools(cancellation());
    assert!(
        node.call(
            RUN_TOOL_NAME,
            json!({"command":["sh","-c","true"],"timeout_seconds":31})
        )
        .await
        .is_err()
    );
    let pilot = node.with_timeout_ceiling(Duration::from_secs(600));
    for timeout in [0, 601] {
        assert!(
            pilot
                .call(
                    RUN_TOOL_NAME,
                    json!({"command":["sh","-c","true"],"timeout_seconds":timeout})
                )
                .await
                .is_err()
        );
    }
    let result = pilot
        .call(
            RUN_TOOL_NAME,
            json!({"command":["sh","-c","sleep 5"],"timeout_seconds":1}),
        )
        .await
        .unwrap();
    let result = result[0].as_json().unwrap();
    assert_eq!(result["status"], "timed_out");
    assert_eq!(result["timeout_seconds"], 1);
    assert!(
        result["hint"]
            .as_str()
            .unwrap()
            .contains("1s wall-clock limit")
    );
}

#[tokio::test]
#[ignore = "takes 31s to prove the Pilot command exceeds the original Node limit"]
async fn pilot_command_can_complete_after_the_original_thirty_second_node_limit() {
    let fixture = Fixture::new();
    let tools = fixture
        .tools(cancellation())
        .with_timeout_ceiling(Duration::from_secs(600));
    let result = tools.call(RUN_TOOL_NAME, json!({"command":["sh","-c","sleep 31; printf long-pilot-command-completed"],"timeout_seconds":35})).await.unwrap();
    let result = result[0].as_json().unwrap();
    assert_eq!(result["status"], "completed");
    assert_eq!(result["exit_code"], 0);
    assert_eq!(result["stdout"], "long-pilot-command-completed");
    assert_eq!(result["timeout_seconds"], 35);
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
async fn a_program_installed_under_home_runs_by_name() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture.workspace.join(".local/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let program = bin.join("installed-by-agent");
    // Toolchains locate their files through /proc/self/exe.
    std::fs::write(
        &program,
        "#!/bin/sh\ntest -e /proc/self/exe && printf \"$HOME\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let result = run(
        &fixture.tools(cancellation()),
        &["sh", "-c", "installed-by-agent"],
    )
    .await;
    assert_eq!(result["stdout"], "/workspace", "{result}");
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
    assert!(description.contains("~/.local/bin"), "{description}");
    assert!(description.contains("defaults to 30s"), "{description}");
    assert!(description.contains("up to 30s"), "{description}");
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
async fn read_refuses_a_first_line_over_the_byte_limit() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    std::fs::write(
        fixture.workspace.join("notes.txt"),
        "x".repeat(READ_MAX_BYTES),
    )
    .unwrap();

    let error = call_tool_error(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert!(error.contains("line 1"), "{error}");
    assert!(
        error.contains(&format!("{READ_MAX_BYTES} byte page limit")),
        "{error}"
    );
    assert!(error.contains("counted newline"), "{error}");
    assert!(error.contains("anchor_run"), "{error}");
}

#[tokio::test]
async fn read_advances_before_a_later_line_over_the_byte_limit() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let content = format!("short\n{}\n", "x".repeat(READ_MAX_BYTES));
    std::fs::write(fixture.workspace.join("notes.txt"), &content).unwrap();

    let page = call_tool(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert_eq!(page["lines"], json!([{"line":1,"text":"short"}]));
    assert_eq!(page["returned_lines"], 1);
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_offset"], 1);
    assert_eq!(
        page["sha256"],
        format!("{:x}", Sha256::digest(content.as_bytes()))
    );

    let error = call_tool_error(
        &tools,
        READ_TOOL_NAME,
        json!({"path":"notes.txt","offset":page["next_offset"]}),
    )
    .await;
    assert!(error.contains("line 2"), "{error}");
    assert!(error.contains("anchor_run"), "{error}");
}

#[tokio::test]
async fn read_accepts_an_exact_utf8_byte_limit_and_refuses_one_byte_more() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let line = "界".repeat((READ_MAX_BYTES - 1) / 3);
    assert_eq!(line.len() + 1, READ_MAX_BYTES);
    let content = format!("{line}\ntail\n");
    std::fs::write(fixture.workspace.join("notes.txt"), &content).unwrap();

    let page = call_tool(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert_eq!(page["returned_lines"], 1);
    assert_eq!(page["lines"][0]["line"], 1);
    assert_eq!(page["lines"][0]["text"].as_str(), Some(line.as_str()));
    assert_eq!(page["truncated"], true);
    assert_eq!(page["next_offset"], 1);
    assert_eq!(
        page["sha256"],
        format!("{:x}", Sha256::digest(content.as_bytes()))
    );
    let tail = call_tool(
        &tools,
        READ_TOOL_NAME,
        json!({"path":"notes.txt","offset":page["next_offset"]}),
    )
    .await;
    assert_eq!(tail["lines"], json!([{"line":2,"text":"tail"}]));
    assert_eq!(tail["truncated"], false);
    assert_eq!(tail["next_offset"], Value::Null);
    assert_eq!(tail["sha256"], page["sha256"]);

    std::fs::write(fixture.workspace.join("notes.txt"), format!("{line}x\n")).unwrap();
    let error = call_tool_error(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert!(error.contains("line 1"), "{error}");
    assert!(error.contains("anchor_run"), "{error}");
}

#[tokio::test]
async fn read_counts_newlines_when_paginating_utf8_lines() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let first_line = "界".repeat((READ_MAX_BYTES - 4) / 3);
    assert_eq!(first_line.len() + 1 + "é".len() + 1, READ_MAX_BYTES);
    std::fs::write(
        fixture.workspace.join("notes.txt"),
        format!("{first_line}\né\ntail\n"),
    )
    .unwrap();

    let exact = call_tool(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert_eq!(exact["returned_lines"], 2);
    assert_eq!(
        exact["lines"][0]["text"].as_str(),
        Some(first_line.as_str())
    );
    assert_eq!(exact["lines"][1], json!({"line":2,"text":"é"}));
    assert_eq!(exact["truncated"], true);
    assert_eq!(exact["next_offset"], 2);

    std::fs::write(
        fixture.workspace.join("notes.txt"),
        format!("{first_line}\néx\ntail\n"),
    )
    .unwrap();
    let over = call_tool(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert_eq!(over["returned_lines"], 1);
    assert_eq!(over["lines"][0]["text"].as_str(), Some(first_line.as_str()));
    assert_eq!(over["truncated"], true);
    assert_eq!(over["next_offset"], 1);
    let remainder = call_tool(
        &tools,
        READ_TOOL_NAME,
        json!({"path":"notes.txt","offset":over["next_offset"]}),
    )
    .await;
    assert_eq!(
        remainder["lines"],
        json!([{"line":2,"text":"éx"},{"line":3,"text":"tail"}])
    );
    assert_eq!(remainder["truncated"], false);
    assert_eq!(remainder["next_offset"], Value::Null);
    assert_eq!(remainder["sha256"], over["sha256"]);
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
            error.contains("refused") || error.contains("outside") || error.contains("empty"),
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
async fn file_tools_accept_the_workspace_mount_path_and_name_themselves_in_refusals() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    // `/workspace/<path>` is where the model sees the file in the sandbox.
    let created = call_tool(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"/workspace/notes.txt","content":"one\n"}),
    )
    .await;
    assert_eq!(created["created"], true);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("notes.txt")).unwrap(),
        "one\n"
    );
    let read = call_tool(
        &tools,
        READ_TOOL_NAME,
        json!({"path":"/workspace/notes.txt"}),
    )
    .await;
    let relative = call_tool(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    assert_eq!(read["sha256"], relative["sha256"]);
    assert_eq!(read["lines"][0]["text"], "one");

    // The prefix grants nothing more: the rest is checked as a relative path.
    std::fs::write(fixture.directory.path().join("outside.txt"), "host only").unwrap();
    for path in ["/workspace/../outside.txt", "/workspace//etc/passwd"] {
        let error = call_tool_error(&tools, READ_TOOL_NAME, json!({"path": path})).await;
        assert!(!error.contains("host only"), "{error}");
        assert!(
            error.contains("anchor_read refused: invalid arguments"),
            "{error}"
        );
        assert!(!error.contains("anchor_run"), "{error}");
        let error = call_tool_error(
            &tools,
            EDIT_TOOL_NAME,
            json!({"path": path, "content": "x"}),
        )
        .await;
        assert!(
            error.contains("anchor_edit refused: invalid arguments"),
            "{error}"
        );
        assert!(!error.contains("\"command\""), "{error}");
    }
    // The failure classes recorded as facts stay what they were.
    let error = call_tool_error(&tools, READ_TOOL_NAME, json!({"path":"/etc/passwd"})).await;
    assert_eq!(
        anchor_facts::classify_failure(&error),
        anchor_facts::Failure::NotAuthorized
    );
    let error = call_tool_error(&tools, EDIT_TOOL_NAME, json!({"path":"../x","content":"x"})).await;
    assert_eq!(
        anchor_facts::classify_failure(&error),
        anchor_facts::Failure::InvalidArguments
    );
}

#[tokio::test]
async fn development_projects_are_offered_only_where_they_can_be_used() {
    let fixture = Fixture::new();
    let schema = |tools: &NodeTools| {
        tools
            .definitions()
            .into_iter()
            .find(|tool| tool.name == RUN_TOOL_NAME)
            .unwrap()
            .parameters
    };
    let node = fixture.tools(cancellation());
    assert!(schema(&node)["properties"].get("project").is_none());
    let pilot = fixture.tools(cancellation()).with_development_projects(
        fixture.directory.path().join("library"),
        fixture.directory.path().join("scope"),
    );
    assert_eq!(schema(&pilot)["properties"]["project"]["type"], "string");
}

#[tokio::test]
async fn reads_preserve_relative_and_absolute_symlinks_inside_the_workspace() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    std::fs::create_dir(fixture.workspace.join("actual")).unwrap();
    std::fs::write(
        fixture.workspace.join("actual/notes.txt"),
        "workspace text\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("actual/notes.txt", fixture.workspace.join("relative.txt")).unwrap();
    std::os::unix::fs::symlink(
        fixture.workspace.join("actual/notes.txt"),
        fixture.workspace.join("absolute.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink("actual", fixture.workspace.join("directory")).unwrap();
    let original = call_tool(&tools, READ_TOOL_NAME, json!({"path":"actual/notes.txt"})).await;
    for path in ["relative.txt", "absolute.txt", "directory/notes.txt"] {
        let read = call_tool(&tools, READ_TOOL_NAME, json!({"path":path})).await;
        assert_eq!(read["lines"], original["lines"], "{path}");
        assert_eq!(read["sha256"], original["sha256"], "{path}");
    }
}

#[tokio::test]
async fn edits_refuse_terminal_symlinks_without_reading_their_targets() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let outside = fixture.directory.path().join("outside.txt");
    let outside_bytes = b"isolated outside fixture";
    std::fs::write(&outside, outside_bytes).unwrap();
    std::fs::write(fixture.workspace.join("inside.txt"), "inside fixture").unwrap();
    for (name, target) in [
        ("outside-link.txt", outside.clone()),
        ("inside-link.txt", fixture.workspace.join("inside.txt")),
        (
            "dangling-link.txt",
            fixture.directory.path().join("absent.txt"),
        ),
    ] {
        let link = fixture.workspace.join(name);
        std::os::unix::fs::symlink(&target, &link).unwrap();
        for edit in [
            json!({"path":name,"content":"replacement"}),
            json!({"path":name,"content":"replacement","base_sha256":"0".repeat(64)}),
            json!({"path":name,"old_string":"fixture","new_string":"changed"}),
        ] {
            let error = call_tool_error(&tools, EDIT_TOOL_NAME, edit).await;
            assert!(
                !error.contains(&format!("{:x}", Sha256::digest(outside_bytes))),
                "{error}"
            );
            assert_eq!(std::fs::read_link(&link).unwrap(), target);
        }
    }
    assert_eq!(std::fs::read(&outside).unwrap(), outside_bytes);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("inside.txt")).unwrap(),
        "inside fixture"
    );
    assert!(!fixture.directory.path().join("absent.txt").exists());
}

#[tokio::test]
async fn file_tools_refuse_parent_symlinks_outside_the_workspace() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let outside = fixture.directory.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("notes.txt"), "isolated outside fixture").unwrap();
    std::os::unix::fs::symlink(&outside, fixture.workspace.join("escape")).unwrap();
    let read = call_tool_error(&tools, READ_TOOL_NAME, json!({"path":"escape/notes.txt"})).await;
    assert!(read.contains("outside /workspace"), "{read}");
    for path in ["escape/notes.txt", "escape/new.txt"] {
        let edit = call_tool_error(
            &tools,
            EDIT_TOOL_NAME,
            json!({"path":path,"content":"replacement"}),
        )
        .await;
        assert!(edit.contains("outside /workspace"), "{edit}");
    }
    assert_eq!(
        std::fs::read_to_string(outside.join("notes.txt")).unwrap(),
        "isolated outside fixture"
    );
    assert!(!outside.join("new.txt").exists());
}

#[tokio::test]
async fn file_tools_refuse_directories_fifos_and_sockets_without_replacing_them() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    std::fs::create_dir(fixture.workspace.join("directory")).unwrap();
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        fixture.workspace.join("fifo"),
        rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
    )
    .unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(fixture.workspace.join("socket")).unwrap();
    for path in ["directory", "fifo", "socket"] {
        let before = std::fs::symlink_metadata(fixture.workspace.join(path))
            .unwrap()
            .file_type();
        call_tool_error(&tools, READ_TOOL_NAME, json!({"path":path})).await;
        call_tool_error(
            &tools,
            EDIT_TOOL_NAME,
            json!({"path":path,"content":"replacement"}),
        )
        .await;
        let after = std::fs::symlink_metadata(fixture.workspace.join(path))
            .unwrap()
            .file_type();
        assert_eq!(before, after, "{path} must not be replaced");
    }
}

#[tokio::test]
async fn edits_do_not_follow_or_overwrite_conflicting_legacy_temporary_files() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let outside = fixture.directory.path().join("outside.txt");
    std::fs::write(&outside, "isolated outside fixture").unwrap();
    std::fs::write(fixture.workspace.join("notes.txt"), "old\n").unwrap();
    let symlink = fixture
        .workspace
        .join(format!(".notes.txt.anchor-edit-{}", std::process::id()));
    std::os::unix::fs::symlink(&outside, &symlink).unwrap();
    let conflict = fixture
        .workspace
        .join(format!(".new.txt.anchor-edit-{}", std::process::id()));
    std::fs::write(&conflict, "unrelated temporary file").unwrap();

    let before = call_tool(&tools, READ_TOOL_NAME, json!({"path":"notes.txt"})).await;
    let replaced = call_tool(
        &tools,
        EDIT_TOOL_NAME,
        json!({
            "path":"notes.txt","content":"new\n","base_sha256":before["sha256"]
        }),
    )
    .await;
    assert_eq!(replaced["bytes_before"], 4);
    assert_eq!(replaced["bytes_after"], 4);
    assert_eq!(replaced["sha256_before"], before["sha256"]);
    assert_eq!(
        replaced["sha256_after"],
        format!("{:x}", Sha256::digest(b"new\n"))
    );
    let created = call_tool(
        &tools,
        EDIT_TOOL_NAME,
        json!({"path":"new.txt","content":"created\n"}),
    )
    .await;
    assert_eq!(created["created"], true);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.join("notes.txt")).unwrap(),
        "new\n"
    );
    assert_eq!(
        std::fs::read_to_string(&outside).unwrap(),
        "isolated outside fixture"
    );
    assert_eq!(std::fs::read_link(&symlink).unwrap(), outside);
    assert_eq!(
        std::fs::read_to_string(&conflict).unwrap(),
        "unrelated temporary file"
    );
    assert!(
        std::fs::read_dir(&fixture.workspace).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".anchor-edit-")
        }),
        "successful edits must remove their private temporary entries"
    );
}

#[test]
fn editing_uses_the_open_parent_when_its_workspace_name_is_replaced() {
    let fixture = Fixture::new();
    let parent = fixture.workspace.join("parent");
    let moved = fixture.workspace.join("moved");
    let outside = fixture.directory.path().join("outside");
    std::fs::create_dir(&parent).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(parent.join("notes.txt"), "workspace fixture").unwrap();
    std::fs::write(outside.join("notes.txt"), "isolated outside fixture").unwrap();
    let files = workspace_files::WorkspaceFiles::open(&fixture.workspace).unwrap();
    let target = files.edit_target(Path::new("parent/notes.txt")).unwrap();
    std::fs::rename(&parent, &moved).unwrap();
    std::os::unix::fs::symlink(&outside, &parent).unwrap();
    assert_eq!(
        target.read_existing().unwrap().unwrap(),
        b"workspace fixture"
    );
    target.replace(b"changed workspace fixture").unwrap();
    assert_eq!(
        std::fs::read(moved.join("notes.txt")).unwrap(),
        b"changed workspace fixture"
    );
    assert_eq!(
        std::fs::read(outside.join("notes.txt")).unwrap(),
        b"isolated outside fixture"
    );
    assert!(std::fs::read_dir(&moved).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".anchor-edit-")
    }));
}

#[tokio::test]
async fn reads_cannot_escape_during_concurrent_directory_symlink_replacement() {
    let fixture = Fixture::new();
    let tools = fixture.tools(cancellation());
    let inside = fixture.workspace.join("inside");
    let parked = fixture.workspace.join("parked");
    let outside = fixture.directory.path().join("outside");
    std::fs::create_dir(&inside).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(inside.join("notes.txt"), "workspace fixture\n").unwrap();
    std::fs::write(outside.join("notes.txt"), "isolated outside fixture\n").unwrap();
    let start = Arc::new(std::sync::Barrier::new(2));
    let other_start = Arc::clone(&start);
    let replacement = std::thread::spawn(move || {
        other_start.wait();
        for _ in 0..256 {
            std::fs::rename(&inside, &parked).unwrap();
            std::os::unix::fs::symlink(&outside, &inside).unwrap();
            std::thread::yield_now();
            std::fs::remove_file(&inside).unwrap();
            std::fs::rename(&parked, &inside).unwrap();
        }
    });
    start.wait();
    let expected = format!("{:x}", Sha256::digest(b"workspace fixture\n"));
    for _ in 0..512 {
        if let Ok(result) = tools
            .call(READ_TOOL_NAME, json!({"path":"inside/notes.txt"}))
            .await
        {
            assert_eq!(result[0].as_json().unwrap()["sha256"], expected);
        }
    }
    replacement.join().unwrap();
    let read = call_tool(&tools, READ_TOOL_NAME, json!({"path":"inside/notes.txt"})).await;
    assert_eq!(read["sha256"], expected);
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
