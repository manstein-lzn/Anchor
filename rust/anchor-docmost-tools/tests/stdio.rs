mod support;

use anchor_docmost_tools::attachment_tool;
use axum::http::{HeaderMap, StatusCode};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, ClientInfo},
    transport::TokioChildProcess,
};
use serde_json::json;
use support::{ATTACHMENT_ID, Fixture, PAGE_ID, TOKEN, expected_body, metadata};
use tokio::process::Command;

fn sandbox_command(
    root: &std::path::Path,
    cwd: &std::path::Path,
    endpoint: &str,
    key: Option<&str>,
) -> Command {
    let mut command = Command::new("/usr/bin/bwrap");
    command
        .args([
            "--die-with-parent",
            "--new-session",
            "--tmpfs",
            "/",
            "--ro-bind",
            "/usr",
            "/usr",
            "--ro-bind",
            "/lib",
            "/lib",
            "--ro-bind",
            "/lib64",
            "/lib64",
            "--dir",
            "/in/publish",
            "--ro-bind",
        ])
        .arg(root)
        .args(["/in/publish/assets", "--ro-bind"])
        .arg(env!("CARGO_BIN_EXE_anchor-docmost-tools"))
        .arg(env!("CARGO_BIN_EXE_anchor-docmost-tools"))
        .arg("--ro-bind")
        .arg(cwd)
        .arg(cwd)
        .arg("--chdir")
        .arg(cwd)
        .arg("--")
        .arg(env!("CARGO_BIN_EXE_anchor-docmost-tools"))
        .args(["--endpoint", endpoint])
        .env_clear()
        .env("DOCMOST_ENDPOINT", "https://invalid.example/ignored")
        .env("DOCMOST_UPLOAD_ROOT", "/etc");
    if let Some(key) = key {
        command.env("DOCMOST_API_KEY", key);
    }
    command
}

#[tokio::test]
async fn real_stdio_initializes_lists_original_schema_and_uploads_fixed_root() {
    let root = tempfile::tempdir().expect("input root");
    let cwd = tempfile::tempdir().expect("stdio cwd");
    let bytes = b"<svg>\0stdio bytes\xff</svg>";
    std::fs::write(root.path().join("report.svg"), bytes).expect("image");
    std::fs::write(
        cwd.path().join(".env"),
        "DOCMOST_API_KEY=must-not-load\nDOCMOST_UPLOAD_ROOT=/etc\n",
    )
    .expect("ignored dotenv");
    let fixture = Fixture::start(
        StatusCode::OK,
        metadata("report.svg", "image/svg+xml"),
        HeaderMap::new(),
    )
    .await;
    let transport = TokioChildProcess::new(sandbox_command(
        root.path(),
        cwd.path(),
        &fixture.endpoint,
        Some(TOKEN),
    ))
    .expect("stdio child");
    let service = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        ClientInfo::default().serve(transport),
    )
    .await
    .expect("initialize timeout")
    .expect("initialize");
    let info = service.peer_info().expect("server info");
    assert_eq!(info.server_info.name, "docmost-attachments");
    assert_eq!(info.server_info.version, "1.0.0");
    assert!(info.capabilities.tools.is_some());
    let tools = service.list_all_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 1);
    assert_eq!(
        serde_json::to_value(&tools[0]).expect("tool JSON"),
        serde_json::to_value(attachment_tool()).expect("original tool")
    );
    let original = include_str!("../../../plugins/docmost/upload_server.py");
    let expected = json!({
        "name": "upload_page_image",
        "description": "Upload a report image from /in/publish/assets to a Docmost page and return its Markdown URL. Set attachmentId to replace an existing image on that page.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Absolute path under /in/publish/assets"},
                "pageId": {"type": "string", "description": "Target Docmost page UUID"},
                "attachmentId": {"type": "string", "description": "Optional existing attachment UUID to replace"}
            },
            "required": ["path", "pageId"]
        }
    });
    assert!(original.contains(expected["description"].as_str().expect("description")));
    assert_eq!(
        serde_json::to_value(&tools[0]).expect("tool JSON"),
        expected
    );
    let arguments = json!({"path": "/in/publish/assets/report.svg", "pageId": PAGE_ID, "attachmentId": ATTACHMENT_ID});
    let result = service
        .call_tool(
            CallToolRequestParams::new("upload_page_image")
                .with_arguments(arguments.as_object().expect("arguments").clone()),
        )
        .await
        .expect("tools/call");
    assert_eq!(result.is_error, Some(false));
    let text = result.content[0].as_text().expect("text response");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text.text).expect("attachment JSON"),
        json!({
            "attachmentId": ATTACHMENT_ID, "fileName": "report.svg", "url": format!("/api/files/{ATTACHMENT_ID}/report.svg"), "mimeType": "image/svg+xml", "pageId": PAGE_ID
        })
    );
    for arguments in [
        json!({"path": "/etc/passwd", "pageId": PAGE_ID}),
        json!({"path": "/in/publish/assets/report.svg", "pageId": TOKEN}),
        json!({"path": [TOKEN], "pageId": PAGE_ID}),
    ] {
        let result = service
            .call_tool(
                CallToolRequestParams::new("upload_page_image")
                    .with_arguments(arguments.as_object().expect("arguments").clone()),
            )
            .await
            .expect("tool rejection");
        assert_eq!(result.is_error, Some(true));
        assert!(
            !serde_json::to_string(&result)
                .expect("error JSON")
                .contains(TOKEN)
        );
    }
    let unknown = service
        .call_tool(CallToolRequestParams::new("unknown-tool"))
        .await
        .expect_err("unknown tool");
    assert!(unknown.to_string().contains("unknown tool"));
    service.cancel().await.expect("stdio shutdown");
    let requests = fixture.requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].body,
        expected_body(
            &requests[0],
            "report.svg",
            "image/svg+xml",
            bytes,
            Some(ATTACHMENT_ID)
        )
    );
}

#[tokio::test]
async fn stdio_without_key_does_not_load_dotenv_or_send_http() {
    let root = tempfile::tempdir().expect("root");
    let cwd = tempfile::tempdir().expect("cwd");
    std::fs::write(root.path().join("image.png"), b"image").expect("image");
    std::fs::write(
        cwd.path().join(".env"),
        format!("DOCMOST_API_KEY={TOKEN}\n"),
    )
    .expect("dotenv bait");
    let fixture = Fixture::start(
        StatusCode::OK,
        metadata("image.png", "image/png"),
        HeaderMap::new(),
    )
    .await;
    let transport = TokioChildProcess::new(sandbox_command(
        root.path(),
        cwd.path(),
        &fixture.endpoint,
        None,
    ))
    .expect("stdio child");
    let service = ClientInfo::default()
        .serve(transport)
        .await
        .expect("initialize without key");
    let arguments = json!({"path": "/in/publish/assets/image.png", "pageId": PAGE_ID});
    let result = service
        .call_tool(
            CallToolRequestParams::new("upload_page_image")
                .with_arguments(arguments.as_object().expect("arguments").clone()),
        )
        .await
        .expect("call");
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.content[0].as_text().expect("error text").text,
        "DOCMOST_API_KEY is not configured"
    );
    service.cancel().await.expect("shutdown");
    assert!(fixture.requests.lock().expect("requests").is_empty());
}

#[test]
fn cli_rejects_endpoint_credentials_invalid_url_and_host_root_override() {
    for arguments in [
        vec![
            "--endpoint",
            "https://operator:fixture-secret@localhost/upload",
        ],
        vec!["--endpoint", "file:///etc/passwd"],
        vec!["--endpoint", "/relative"],
        vec!["--root", "/etc"],
        vec!["--endpoint"],
    ] {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_anchor-docmost-tools"))
            .args(arguments)
            .env_clear()
            .env("DOCMOST_API_KEY", TOKEN)
            .output()
            .expect("CLI rejection");
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(!error.contains(TOKEN));
        assert!(!error.contains("fixture-secret"));
    }
}

#[tokio::test]
async fn no_arguments_defaults_to_stdio_and_ignores_root_and_endpoint_environment() {
    let cwd = tempfile::tempdir().expect("default stdio cwd");
    let path = cwd.path().join("outside.png");
    std::fs::write(&path, b"outside fixed root").expect("fixture image");
    let fixture = Fixture::start(
        StatusCode::OK,
        metadata("outside.png", "image/png"),
        HeaderMap::new(),
    )
    .await;
    let mut command = Command::new(env!("CARGO_BIN_EXE_anchor-docmost-tools"));
    command
        .current_dir(cwd.path())
        .env_clear()
        .env("DOCMOST_API_KEY", TOKEN)
        .env("DOCMOST_UPLOAD_ROOT", cwd.path())
        .env("DOCMOST_ENDPOINT", &fixture.endpoint);
    let transport = TokioChildProcess::new(command).expect("default stdio child");
    let service = ClientInfo::default()
        .serve(transport)
        .await
        .expect("default initialize");
    assert_eq!(
        service
            .list_all_tools()
            .await
            .expect("default tools/list")
            .len(),
        1
    );
    let arguments = json!({"path": path, "pageId": PAGE_ID});
    let result = service
        .call_tool(
            CallToolRequestParams::new("upload_page_image")
                .with_arguments(arguments.as_object().expect("arguments").clone()),
        )
        .await
        .expect("default tools/call");
    assert_eq!(result.is_error, Some(true));
    assert!(
        result.content[0]
            .as_text()
            .expect("error text")
            .text
            .contains("/in/publish/assets")
    );
    service.cancel().await.expect("default shutdown");
    assert!(fixture.requests.lock().expect("requests").is_empty());
}
