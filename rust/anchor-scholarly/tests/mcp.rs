use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use serde_json::{Value, json};

const BINARY: &str = env!("CARGO_BIN_EXE_anchor-scholarly");

fn plugin_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/academic-research")
}

#[test]
fn academic_plugin_declares_only_the_fixed_rust_stdio_entry() {
    let root = plugin_root();
    let manifest: Value = serde_json::from_slice(&fs::read(root.join("plugin.json")).unwrap())
        .expect("valid plugin manifest");
    let server = &manifest["mcpServers"]["scholarly"];
    assert_eq!(server["command"], "bin/anchor-scholarly-mcp");
    assert_eq!(server["args"], json!([]));
    assert_eq!(server["cwd"], ".");
    assert!(server.get("url").is_none());
    assert!(server.get("env").is_none());

    let command = server["command"].as_str().unwrap();
    let command_path = Path::new(command);
    assert!(!command_path.is_absolute());
    assert!(
        !command_path
            .components()
            .any(|component| { matches!(component, std::path::Component::ParentDir) })
    );
    let wrapper = root.join(command);
    let metadata = fs::metadata(&wrapper).expect("wrapper exists");
    assert!(metadata.is_file());
    assert_ne!(metadata.permissions().mode() & 0o111, 0);
    let script = fs::read_to_string(wrapper).unwrap();
    assert!(script.contains("anchor-scholarly"));
    assert!(!script.contains("python"));
    assert!(!script.contains("venv"));
    assert!(!script.contains("https://"));
}

#[test]
fn fixed_wrapper_help_uses_the_configured_rust_binary() {
    let wrapper = plugin_root().join("bin/anchor-scholarly-mcp");
    let output = Command::new(wrapper)
        .env_clear()
        .env("ANCHOR_SCHOLARLY_BINARY", BINARY)
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("anchor-scholarly mcp"));
    assert!(help.contains("stdio"));
}

#[test]
fn stdio_mcp_negotiates_lists_tools_and_reports_json_errors() {
    let mut child = Command::new(BINARY)
        .arg("mcp")
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());

    write_request(
        &mut input,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}
        }),
    );
    let initialized = read_response(&mut output);
    assert_eq!(initialized["result"]["serverInfo"]["name"], "scholarly");
    assert!(initialized["result"]["capabilities"]["tools"].is_object());

    write_request(
        &mut input,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    write_request(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let listed = read_response(&mut output);
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 6);
    assert!(tools.iter().any(|tool| tool["name"] == "scholarly_search"));
    assert!(tools.iter().all(|tool| tool["inputSchema"].is_object()));

    write_request(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "private", "arguments": {}}}),
    );
    let unknown = read_response(&mut output);
    assert_eq!(unknown["error"]["code"], -32602);
    assert_eq!(unknown["error"]["message"], "unknown tool");

    write_request(
        &mut input,
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "scholarly_search", "arguments": {}}}),
    );
    let invalid = read_response(&mut output);
    assert_eq!(invalid["result"]["isError"], true);
    assert!(
        invalid["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("query must be a string")
    );

    drop(input);
    assert!(child.wait().unwrap().success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.is_empty(), "unexpected stderr: {stderr}");
}

fn write_request(input: &mut impl Write, request: Value) {
    writeln!(input, "{request}").unwrap();
    input.flush().unwrap();
}

fn read_response(output: &mut BufReader<impl Read>) -> Value {
    let mut line = String::new();
    assert!(
        output.read_line(&mut line).unwrap() > 0,
        "MCP stdout closed"
    );
    serde_json::from_str(&line).unwrap()
}
