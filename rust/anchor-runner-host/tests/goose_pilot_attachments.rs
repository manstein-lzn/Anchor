#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;

#[allow(dead_code)]
mod goose {
    include!("support/goose_fixture.rs");

    impl Provider {
        pub fn assert_no_failures(&self) {
            let state = self.state.lock().unwrap();
            assert!(
                state.failures.is_empty(),
                "{:?}; evidence {}",
                state.failures,
                self.root.display()
            );
        }
    }

    // Keep the shared fixture unchanged. Attachment endpoints return original
    // bytes, whereas its normal request helper deliberately decodes JSON.
    impl HttpHost {
        pub fn attachment_bytes(&self, path: &str) -> (u16, reqwest::header::HeaderMap, Vec<u8>) {
            assert!(path.starts_with("/sessions/"));
            self.runtime.block_on(async {
                let response = self
                    .client
                    .get(format!("{}{path}", self.url))
                    .send()
                    .await
                    .unwrap();
                let status = response.status().as_u16();
                let headers = response.headers().clone();
                (status, headers, response.bytes().await.unwrap().to_vec())
            })
        }
    }
}

use axum::Router;
use base64::{Engine, engine::general_purpose::STANDARD};
use goose::{Host, Provider, Step};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::local::LocalSessionManager,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Cursor,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};
use tokio_util::sync::CancellationToken;

const SESSION: &str = "pilot-attachments";
const VISION_MODEL: &str = "gpt-4o";
const INPUT_TEXT: &[u8] = b"attachment-input-fixture\n";
const OUTPUT_TEXT: &[u8] = b"published-report-fixture\n";
const CURRENT_TEXT: &[u8] = b"current-turn-only-fixture\n";
const PLUGIN: &str = "pilot-images";
const REMOTE_TOOL: &str = "fixture_image";
const EXPOSED_TOOL: &str = "pilot-images-loopback_fixture_image";

fn graph() -> Value {
    json!({"entry":"seed","ops":{"seed":{"run":"true"}},
        "nodes":[{"id":"seed","op":"seed"}],"edges":[]})
}

fn png(color: [u8; 3]) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(RgbImage::from_pixel(2, 1, Rgb(color)));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, ImageFormat::Png).unwrap();
    encoded.into_inner()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn upload(name: &str, bytes: &[u8], media_type: &str) -> Value {
    json!({"name":name,"data_base64":STANDARD.encode(bytes),"media_type":media_type})
}

fn write_png(path: &str, bytes: &[u8]) -> String {
    let octal = bytes
        .iter()
        .map(|byte| format!("\\{byte:03o}"))
        .collect::<String>();
    format!("printf '{octal}' > {path}")
}

fn create(server: &goose::HttpHost) {
    let (status, saved) = server.request("POST", "/sessions", Some(&json!({"id":SESSION})));
    assert_eq!(status, 201, "{saved}");
}

fn submit(server: &goose::HttpHost, request: &Value) -> Value {
    let (status, saved) =
        server.request("POST", &format!("/sessions/{SESSION}/turns"), Some(request));
    assert_eq!(status, 202, "{saved}");
    saved["turn"].clone()
}

fn wait(server: &goose::HttpHost, id: &str) -> Value {
    let mut turn = Value::Null;
    goose::wait_until("Goose Pilot attachment Turn", || {
        let (status, saved) =
            server.request("GET", &format!("/sessions/{SESSION}/turns/{id}"), None);
        assert_eq!(status, 200, "{saved}");
        turn = saved["turn"].clone();
        assert!(
            turn["status"] == "running" || turn["status"] == "completed",
            "{turn}"
        );
        turn["status"] == "completed"
    });
    turn
}

fn messages(server: &goose::HttpHost) -> Value {
    let (status, saved) = server.request("GET", &format!("/sessions/{SESSION}/messages"), None);
    assert_eq!(status, 200, "{saved}");
    saved["messages"].clone()
}

fn message<'a>(messages: &'a Value, turn: &str, role: &str) -> &'a Value {
    let matches = messages
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["turn"] == turn && message["role"] == role)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{messages}");
    matches[0]
}

fn attachment<'a>(attachments: &'a Value, name: &str) -> &'a Value {
    let matches = attachments
        .as_array()
        .unwrap()
        .iter()
        .filter(|attachment| attachment["name"] == name)
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{attachments}");
    matches[0]
}

fn delivered_attachments(messages: &Value, turn: &str) -> Value {
    json!(
        messages
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["turn"] == turn && message["role"] == "assistant")
            .filter_map(|message| message.get("attachments").and_then(Value::as_array))
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
    )
}

fn attachment_events(server: &goose::HttpHost, turn: &str) -> (String, Vec<Value>) {
    let stream = server.events(&format!("/sessions/{SESSION}/turns/{turn}/events"), 0);
    let attachments = stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str::<Value>(data).unwrap())
        .filter(|event| event["type"] == "attachment")
        .map(|event| event["attachment"].clone())
        .collect::<Vec<_>>();
    (stream, attachments)
}

fn assert_download(
    server: &goose::HttpHost,
    turn: &str,
    attachment: &Value,
    expected: &[u8],
    media_type: Option<&str>,
) {
    assert_eq!(attachment["size"], expected.len() as u64);
    assert_eq!(attachment["sha256"], sha256(expected));
    assert_eq!(attachment["media_type"], json!(media_type));
    let id = attachment["id"].as_str().unwrap();
    assert!(!id.is_empty());
    let url = format!("/sessions/{SESSION}/turns/{turn}/attachments/{id}");
    assert_eq!(attachment["url"], url);
    assert_eq!(attachment["download_url"], format!("{url}?download=1"));
    let inline = media_type.is_some_and(|media_type| media_type.starts_with("image/"));
    for (path, disposition) in [
        (url.as_str(), if inline { "inline" } else { "attachment" }),
        (attachment["download_url"].as_str().unwrap(), "attachment"),
    ] {
        let (status, headers, bytes) = server.attachment_bytes(path);
        assert_eq!(status, 200, "{attachment}");
        assert_eq!(bytes, expected, "{attachment}");
        assert_eq!(
            headers["content-type"],
            if disposition == "inline" {
                media_type.unwrap()
            } else {
                "application/octet-stream"
            }
        );
        assert!(
            headers["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with(disposition),
            "{headers:?}"
        );
    }
}

fn image_urls(value: &Value) -> Vec<String> {
    match value {
        Value::Array(values) => values.iter().flat_map(image_urls).collect(),
        Value::Object(values)
            if values.get("type").and_then(Value::as_str) == Some("image_url") =>
        {
            vec![value["image_url"]["url"].as_str().unwrap().into()]
        }
        Value::Object(values) => values.values().flat_map(image_urls).collect(),
        _ => Vec::new(),
    }
}

fn evidence(host: &Host, provider: &Provider, checks: Value) {
    host.evidence_report(
        provider,
        "tests/goose_pilot_attachments.rs",
        checks,
        json!({"test_binary_sha256":goose::digest(&std::env::current_exe().unwrap())}),
    );
}

#[test]
#[ignore = "requires pinned Goose binary and Bubblewrap"]
fn goose_pilot_attachment_only_turn_preserves_inputs_outputs_restart_and_idempotency() {
    let input_image = png([230, 40, 10]);
    let output_image = png([10, 60, 220]);
    let modified_image = png([50, 210, 70]);
    let create_outputs = format!(
        "set -eu; printf 'read:'; cat /in/pilot/notes.txt; test -s /in/pilot/input.png; \
         cat /in/pilot/input.png > /dev/null; \
         if (printf replacement > /in/pilot/notes.txt) 2>/dev/null; then \
         printf unexpected-writable-input; exit 1; fi; \
         test \"$(cat /in/pilot/notes.txt)\" = attachment-input-fixture; \
         printf 'input-read-only\\n'; printf 'published-report-fixture\\n' > report.txt; \
         {}; printf outputs-created",
        write_png("preview.png", &output_image)
    );
    let rewrite_outputs = format!(
        "set -eu; printf workspace-changed-after-publish > report.txt; {}; printf workspace-rewritten",
        write_png("preview.png", &modified_image)
    );
    let provider = Provider::with_model(
        "goose-pilot-attachments-restart",
        vec![
            Step::tool("anchor_read", json!({"path":"/in/pilot/notes.txt"})),
            goose::command(&create_outputs).after("attachment-input-fixture"),
            Step::tool(
                "publish_file",
                json!({"path":"report.txt","name":"delivered-report.txt"}),
            )
            .after("outputs-created"),
            Step::tool(
                "publish_file",
                json!({"path":"preview.png","name":"delivered-preview.png"}),
            )
            .after("delivered-report.txt"),
            goose::command(&rewrite_outputs).after("delivered-preview.png"),
            Step::text("Delivered the immutable report and preview.").after("workspace-rewritten"),
            goose::command(
                "set -eu; test ! -e /in/pilot/notes.txt; test ! -e /in/pilot/input.png; \
                 cat /in/pilot/current.txt; test \"$(cat report.txt)\" = workspace-changed-after-publish; \
                 printf current-input-only",
            ),
            Step::text("Checked this Turn's input and the existing workspace.")
                .after("current-input-only"),
            Step::tool("anchor_read", json!({"path":"/in/pilot/current.txt"}))
                .expect_request_contains("Checked this Turn's input"),
            Step::text("Read the retained current Turn attachment while resuming.")
                .after("current-turn-only-fixture"),
        ],
        VISION_MODEL,
    );
    let host = Host::new(&graph()).default_runtime();
    let mut server = host.serve(&provider);
    create(&server);
    let first_request = json!({"request_id":"files-only","attachments":[
        upload("notes.txt", INPUT_TEXT, "text/plain"),
        upload("input.png", &input_image, "image/png")
    ]});
    let accepted = submit(&server, &first_request);
    let first_id = accepted["id"].as_str().unwrap();
    let first = wait(&server, first_id);
    provider.assert_no_failures();
    assert!(first["prompt"].is_null(), "{first}");
    assert_eq!(first["attachments"].as_array().unwrap().len(), 2);
    let requests = provider.requests();
    assert_eq!(requests.len(), 6);
    assert_eq!(
        image_urls(&requests[0]["messages"]),
        vec![format!(
            "data:image/png;base64,{}",
            STANDARD.encode(&input_image)
        )]
    );
    assert!(goose::tool_definition(&requests[0], "publish_file").is_ok());
    let first_messages = messages(&server);
    let user = message(&first_messages, first_id, "user");
    assert_eq!(user["text"], "");
    assert_eq!(user["attachments"], first["attachments"]);
    let delivered = delivered_attachments(&first_messages, first_id);
    assert_eq!(delivered.as_array().unwrap().len(), 2);
    let report = attachment(&delivered, "delivered-report.txt");
    let preview = attachment(&delivered, "delivered-preview.png");
    assert_download(&server, first_id, report, OUTPUT_TEXT, None);
    assert_download(&server, first_id, preview, &output_image, Some("image/png"));
    assert_download(
        &server,
        first_id,
        attachment(&first["attachments"], "notes.txt"),
        INPUT_TEXT,
        Some("text/plain"),
    );
    assert_download(
        &server,
        first_id,
        attachment(&first["attachments"], "input.png"),
        &input_image,
        Some("image/png"),
    );
    let (stream, emitted) = attachment_events(&server, first_id);
    assert_eq!(json!(emitted), delivered);
    assert_eq!(submit(&server, &first_request), first);
    assert_eq!(provider.requests().len(), requests.len());
    server.kill();
    drop(server);
    let mut server = host.serve(&provider);
    assert_eq!(wait(&server, first_id), first);
    assert_eq!(messages(&server), first_messages);
    assert_eq!(attachment_events(&server, first_id).0, stream);
    assert_download(&server, first_id, report, OUTPUT_TEXT, None);
    assert_download(&server, first_id, preview, &output_image, Some("image/png"));
    assert_eq!(submit(&server, &first_request), first);
    assert_eq!(provider.requests().len(), requests.len());
    let second = submit(
        &server,
        &json!({"request_id":"current-only","message":"Read only this Turn's uploaded file and check the workspace.",
            "attachments":[upload("current.txt", CURRENT_TEXT, "text/plain")]}),
    );
    let second_id = second["id"].as_str().unwrap();
    let second = wait(&server, second_id);
    provider.assert_no_failures();
    assert_eq!(first["goose"], second["goose"]);
    assert_eq!(second["attachments"].as_array().unwrap().len(), 1);
    let final_messages = messages(&server);
    assert_eq!(
        &final_messages.as_array().unwrap()[..first_messages.as_array().unwrap().len()],
        first_messages.as_array().unwrap().as_slice()
    );
    assert_eq!(
        final_messages
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "user")
            .count(),
        2
    );
    assert!(
        final_messages
            .to_string()
            .contains("Checked this Turn's input")
    );
    assert_eq!(delivered_attachments(&final_messages, first_id), delivered);
    assert_eq!(delivered_attachments(&final_messages, second_id), json!([]));
    assert!(attachment_events(&server, second_id).1.is_empty());
    assert_download(&server, first_id, report, OUTPUT_TEXT, None);
    assert_download(&server, first_id, preview, &output_image, Some("image/png"));
    assert_download(
        &server,
        second_id,
        attachment(&second["attachments"], "current.txt"),
        CURRENT_TEXT,
        Some("text/plain"),
    );
    let resume_request = json!({"request_id":"resume-input","resume":true});
    let resumed = submit(&server, &resume_request);
    let resumed_id = resumed["id"].as_str().unwrap();
    let resumed = wait(&server, resumed_id);
    provider.assert_no_failures();
    assert!(resumed["prompt"].is_null(), "{resumed}");
    assert_eq!(resumed["attachments"], second["attachments"]);
    assert_eq!(resumed["goose"], second["goose"]);
    let resumed_messages = messages(&server);
    assert_eq!(
        &resumed_messages.as_array().unwrap()[..final_messages.as_array().unwrap().len()],
        final_messages.as_array().unwrap().as_slice()
    );
    assert_eq!(
        resumed_messages
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["role"] == "user")
            .count(),
        2
    );
    assert!(
        resumed_messages
            .to_string()
            .contains("Read the retained current Turn attachment")
    );
    let scope = first["goose"]["scope"].as_str().unwrap();
    let attachments_root = host
        .base
        .root
        .path()
        .join("state/platform/pilot")
        .join(scope)
        .join("attachments");
    let origin_files = attachments_root.join(second_id).join("input/files");
    assert_eq!(
        fs::read(origin_files.join("current.txt")).unwrap(),
        CURRENT_TEXT
    );
    assert_eq!(fs::read_dir(&origin_files).unwrap().count(), 1);
    assert_eq!(
        fs::read_dir(attachments_root.join(resumed_id).join("input/files"))
            .unwrap()
            .count(),
        0
    );
    let inventory = goose::file_inventory(&attachments_root);
    assert_eq!(
        inventory
            .iter()
            .filter(|file| file["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("input/files/current.txt")))
            .count(),
        1
    );
    let calls = provider.requests().len();
    assert_eq!(calls, 10);
    assert_eq!(submit(&server, &resume_request), resumed);
    assert_eq!(provider.requests().len(), calls);
    server.kill();
    drop(server);
    let server = host.serve(&provider);
    assert_eq!(wait(&server, resumed_id), resumed);
    assert_eq!(messages(&server), resumed_messages);
    assert_eq!(submit(&server, &resume_request), resumed);
    assert_eq!(provider.requests().len(), calls);
    assert_download(
        &server,
        second_id,
        attachment(&resumed["attachments"], "current.txt"),
        CURRENT_TEXT,
        Some("text/plain"),
    );
    let (_, runs) = server.request("GET", "/runs", None);
    assert!(runs["runs"].as_array().unwrap().is_empty(), "{runs}");
    evidence(
        &host,
        &provider,
        json!({"attachment_only_turn":first,"followup_turn":second,"resumed_turn":resumed,"messages":resumed_messages,
            "native_input_image_wire":true,"anchor_read_input":true,"anchor_run_read_only_input":true,
            "current_turn_mount_only":true,"published_attachments":delivered,
            "immutable_bytes_after_workspace_rewrite":true,"inline_and_download_bytes":true,
            "restart_download_messages_and_sse":true,"duplicate_request_idempotent":true,
            "resume_reuses_original_references_and_bytes":true,"resume_does_not_duplicate_user_attachment_message":true,
            "resume_request_idempotent_after_restart":true,"attachment_storage":inventory,
            "same_native_session":true,"no_unrequested_graph_run":true}),
    );
}

#[derive(Clone)]
struct ImageHandler {
    bytes: Arc<Vec<u8>>,
    calls: Arc<AtomicUsize>,
}

impl ServerHandler for ImageHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: vec![Tool::new(
                REMOTE_TOOL,
                "Return the locally generated fixture PNG image.",
                Arc::new(
                    serde_json::from_value(
                        json!({"type":"object","properties":{},"additionalProperties":false}),
                    )
                    .unwrap(),
                ),
            )],
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        if request.name != REMOTE_TOOL
            || request
                .arguments
                .as_ref()
                .is_some_and(|arguments| !arguments.is_empty())
        {
            return Err(ErrorData::invalid_params(
                "expected fixture_image with no arguments",
                None,
            ));
        }
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(CallToolResult::success(vec![
            ContentBlock::text("plugin-image-issued"),
            ContentBlock::image(STANDARD.encode(self.bytes.as_slice()), "image/png"),
        ]))
    }
}

struct ImageFixture {
    endpoint: String,
    bytes: Arc<Vec<u8>>,
    calls: Arc<AtomicUsize>,
    stop: CancellationToken,
    task: Option<thread::JoinHandle<()>>,
}

impl ImageFixture {
    fn new() -> Self {
        let bytes = Arc::new(png([210, 30, 190]));
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = ImageHandler {
            bytes: bytes.clone(),
            calls: calls.clone(),
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let task = thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let service: StreamableHttpService<ImageHandler, LocalSessionManager> =
                        StreamableHttpService::new(
                            move || Ok(handler.clone()),
                            Default::default(),
                            StreamableHttpServerConfig::default()
                                .with_sse_keep_alive(None)
                                .with_cancellation_token(stopped.child_token()),
                        );
                    let app = Router::new().nest_service("/mcp", service);
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    tokio::select! {
                        _ = async { axum::serve(listener, app).await.unwrap(); } => {},
                        _ = stopped.cancelled() => {},
                    }
                });
        });
        Self {
            endpoint: format!("http://{address}/mcp"),
            bytes,
            calls,
            stop,
            task: Some(task),
        }
    }
}

impl Drop for ImageFixture {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

#[test]
#[ignore = "requires pinned Goose binary and Bubblewrap"]
fn goose_pilot_plugin_image_result_is_published_as_turn_attachment() {
    let image = ImageFixture::new();
    let provider = Provider::with_model(
        "goose-pilot-plugin-image-attachment",
        vec![
            Step::tool("anchor_tool_detail", json!({"name":EXPOSED_TOOL})),
            Step::tool(
                "anchor_tools_call",
                json!({"name":EXPOSED_TOOL,"arguments":{}}),
            )
            .after("locally generated fixture PNG"),
            Step::text("The Plugin image is available as an attachment.")
                .after("plugin-image-issued"),
        ],
        VISION_MODEL,
    );
    let host = Host::new(&graph()).default_runtime();
    fixture::install_system_plugin(
        host.base.root.path(),
        anchor_graph_host::SystemPlugin::Network,
    );
    let directory = host.base.root.path().join("checkout").join(PLUGIN);
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("plugin.json"),
        json!({"name":"Pilot image fixture","description":"Locally generated attachment image.","network":true}).to_string(),
    ).unwrap();
    fs::write(
        directory.join(".mcp.json"),
        json!({"mcpServers":{"loopback":{"type":"http","url":image.endpoint}}}).to_string(),
    )
    .unwrap();
    anchor_library::Library::new(host.base.root.path())
        .install_directory(PLUGIN, &directory, false)
        .unwrap();
    let mut server = host.serve(&provider);
    create(&server);
    let (status, network_enabled) = server.request(
        "POST",
        &format!("/sessions/{SESSION}/plugins/network"),
        Some(&json!({"on":true})),
    );
    assert_eq!(status, 200, "{network_enabled}");
    let (status, mounted) = server.request(
        "POST",
        &format!("/sessions/{SESSION}/plugins/{PLUGIN}"),
        Some(&json!({"on":true})),
    );
    assert_eq!(status, 200, "{mounted}");
    let request = json!({"request_id":"plugin-image","message":"Call the authorized fixture image tool once and deliver its image."});
    let accepted = submit(&server, &request);
    let turn_id = accepted["id"].as_str().unwrap();
    let turn = wait(&server, turn_id);
    provider.assert_no_failures();
    assert_eq!(image.calls.load(Ordering::SeqCst), 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        image_urls(&requests[2]["messages"]),
        vec![format!(
            "data:image/png;base64,{}",
            STANDARD.encode(image.bytes.as_slice())
        )]
    );
    assert!(
        turn.get("attachments")
            .is_none_or(|attachments| attachments.as_array().is_some_and(Vec::is_empty))
    );
    let saved_messages = messages(&server);
    let delivered = delivered_attachments(&saved_messages, turn_id);
    assert_eq!(delivered.as_array().unwrap().len(), 1);
    let attachment = &delivered[0];
    assert_download(
        &server,
        turn_id,
        attachment,
        &image.bytes,
        Some("image/png"),
    );
    let (stream, emitted) = attachment_events(&server, turn_id);
    assert_eq!(json!(emitted), delivered);
    assert_eq!(submit(&server, &request), turn);
    assert_eq!(image.calls.load(Ordering::SeqCst), 1);
    assert_eq!(provider.requests().len(), requests.len());
    server.kill();
    drop(server);
    let server = host.serve(&provider);
    assert_eq!(wait(&server, turn_id), turn);
    assert_eq!(messages(&server), saved_messages);
    assert_eq!(attachment_events(&server, turn_id).0, stream);
    assert_download(
        &server,
        turn_id,
        attachment,
        &image.bytes,
        Some("image/png"),
    );
    assert_eq!(image.calls.load(Ordering::SeqCst), 1);
    evidence(
        &host,
        &provider,
        json!({"turn":turn,"messages":saved_messages,"published_attachments":delivered,
            "authorized_plugin_image_called_once":true,"native_plugin_image_wire":true,
            "inline_and_download_bytes":true,"restart_download_messages_and_sse":true,
            "duplicate_request_idempotent":true}),
    );
}
