#[allow(dead_code)]
#[path = "support/runtime_fixture.rs"]
mod fixture;
#[allow(dead_code)]
#[path = "support/goose_fixture.rs"]
mod goose;

use anchor_graph_host::{FilePluginCatalog, PluginCatalog};
use axum::Router;
use base64::{Engine, engine::general_purpose::STANDARD};
use goose::{Gate, Host, Provider, Step, command, complete, tool_definition};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use rmcp::{
    RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ContentBlock, ErrorData, ListToolsResult,
        PaginatedRequestParams, ResourceContents, ServerCapabilities, ServerInfo, Tool,
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
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};
use tokio_util::sync::CancellationToken;

const VISION_MODEL: &str = "gpt-4o";
const PLUGIN: &str = "media";
const REMOTE_TOOL: &str = "fixture_media";
const EXPOSED_TOOL: &str = "media-loopback_fixture_media";
const FIRST_TEXT: &str = "media-before-image";
const MIDDLE_TEXT: &str = "media-between-images";
const LAST_TEXT: &str = "media-after-images";
const REJECTED_TEXT: &str = "must-not-leak-partial-media";
const EVIDENCE: &[u8] = b"media-workspace-checked";

#[derive(Clone)]
struct Picture {
    mime_type: &'static str,
    bytes: Vec<u8>,
    data: String,
}

impl Picture {
    fn new(format: ImageFormat, mime_type: &'static str, color: [u8; 3]) -> Self {
        let pixels = if format == ImageFormat::Png {
            let mut state = 0x4d65_6469_u32;
            RgbImage::from_fn(640, 640, |_, _| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                Rgb([(state >> 16) as u8, (state >> 8) as u8, state as u8])
            })
        } else {
            RgbImage::from_pixel(2, 1, Rgb(color))
        };
        let image = DynamicImage::ImageRgb8(pixels);
        let mut encoded = Cursor::new(Vec::new());
        image.write_to(&mut encoded, format).unwrap();
        let bytes = encoded.into_inner();
        let data = STANDARD.encode(&bytes);
        Self {
            mime_type,
            bytes,
            data,
        }
    }

    fn block(&self) -> ContentBlock {
        ContentBlock::image(self.data.clone(), self.mime_type)
    }

    fn sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.bytes))
    }

    fn data_uri(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.data)
    }
}

#[derive(Default)]
struct MediaState {
    tool_lists: AtomicUsize,
    calls: Mutex<Vec<Value>>,
    effects: Mutex<Vec<Value>>,
}

#[derive(Clone)]
struct MediaHandler {
    state: Arc<MediaState>,
    pictures: Arc<Vec<Picture>>,
}

fn structured() -> Value {
    json!({"fixture":"media-structured-content","sequence":1})
}

impl ServerHandler for MediaHandler {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        self.state.tool_lists.fetch_add(1, Ordering::SeqCst);
        Ok(ListToolsResult {
            tools: vec![Tool::new(
                REMOTE_TOOL,
                "Issue fixture media once, or inspect the same local fixture state without issuing again.",
                Arc::new(
                    serde_json::from_value(json!({
                        "type":"object",
                        "properties":{"mode":{"type":"string","enum":[
                            "mixed","inspect","bad_mime","invalid_base64",
                            "invalid_image","unsupported_mime"
                        ]}},
                        "required":["mode"],"additionalProperties":false
                    }))
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
        if request.name != REMOTE_TOOL {
            return Err(ErrorData::invalid_params(
                "unknown media fixture tool",
                None,
            ));
        }
        let arguments = request
            .arguments
            .ok_or_else(|| ErrorData::invalid_params("missing media arguments", None))?;
        let mode = arguments
            .get("mode")
            .and_then(Value::as_str)
            .filter(|_| arguments.len() == 1)
            .ok_or_else(|| ErrorData::invalid_params("expected only mode", None))?;
        let content = match mode {
            "mixed" | "inspect" => {
                let mut effects = self.state.effects.lock().unwrap();
                if mode == "mixed" {
                    let sequence = effects.len() + 1;
                    effects.push(json!({"effect":"issued-fixture-media","sequence":sequence}));
                } else if effects.len() != 1 {
                    return Err(ErrorData::invalid_params(
                        "no single issued media to inspect",
                        None,
                    ));
                }
                vec![
                    ContentBlock::text(FIRST_TEXT),
                    ContentBlock::text(structured().to_string()),
                    self.pictures[0].block(),
                    ContentBlock::text(MIDDLE_TEXT),
                    ContentBlock::resource(
                        ResourceContents::blob(
                            self.pictures[1].data.clone(),
                            "fixture://media/embedded.jpeg",
                        )
                        .with_mime_type(self.pictures[1].mime_type),
                    ),
                    ContentBlock::text("media-before-webp"),
                    self.pictures[2].block(),
                    ContentBlock::text(LAST_TEXT),
                ]
            }
            "bad_mime" | "invalid_base64" | "invalid_image" | "unsupported_mime" => {
                let invalid = match mode {
                    "bad_mime" => ContentBlock::image(self.pictures[0].data.clone(), "image/jpeg"),
                    "invalid_base64" => ContentBlock::resource(
                        ResourceContents::blob("%%%not-base64%%%", "fixture://media/broken.png")
                            .with_mime_type("image/png"),
                    ),
                    "invalid_image" => {
                        ContentBlock::image(STANDARD.encode(b"not an image"), "image/png")
                    }
                    "unsupported_mime" => {
                        ContentBlock::image(self.pictures[0].data.clone(), "image/gif")
                    }
                    _ => unreachable!(),
                };
                vec![
                    ContentBlock::text(REJECTED_TEXT),
                    self.pictures[0].block(),
                    invalid,
                ]
            }
            _ => return Err(ErrorData::invalid_params("unknown media mode", None)),
        };
        self.state
            .calls
            .lock()
            .unwrap()
            .push(json!({"tool":REMOTE_TOOL,"arguments":arguments}));
        let mut result = CallToolResult::success(content);
        result.structured_content = Some(structured());
        Ok(result)
    }
}

struct MediaFixture {
    endpoint: String,
    pictures: Arc<Vec<Picture>>,
    state: Arc<MediaState>,
    stop: CancellationToken,
    task: Option<thread::JoinHandle<()>>,
}

impl MediaFixture {
    fn new() -> Self {
        let pictures = Arc::new(vec![
            Picture::new(ImageFormat::Png, "image/png", [240, 20, 10]),
            Picture::new(ImageFormat::Jpeg, "image/jpeg", [10, 230, 20]),
            Picture::new(ImageFormat::WebP, "image/webp", [20, 10, 220]),
        ]);
        let state = Arc::new(MediaState::default());
        let handler = MediaHandler {
            state: state.clone(),
            pictures: pictures.clone(),
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
                    let service: StreamableHttpService<MediaHandler, LocalSessionManager> =
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
            pictures,
            state,
            stop,
            task: Some(task),
        }
    }

    fn snapshot(&self) -> Value {
        json!({
            "endpoint":self.endpoint,
            "tool_lists":self.state.tool_lists.load(Ordering::SeqCst),
            "calls":self.state.calls.lock().unwrap().clone(),
            "effects":self.state.effects.lock().unwrap().clone()
        })
    }
}

impl Drop for MediaFixture {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            task.join().unwrap();
        }
    }
}

fn graph() -> Value {
    json!({
        "objective":"Check authorized MCP images through the real Goose vision formatter",
        "entry":"worker",
        "agents":{"worker":{
            "model":"models.worker","network":true,
            "instructions":"Write evidence.txt with Anchor tools, observe the media Plugin result, and use its observed receipt to finish via final_result with route verify."
        }},
        "ops":{"verify":{"run":"sh -c 'set -eu; cat /in/worker/evidence.txt > verified.txt'"}},
        "nodes":[{"id":"worker","agent":"worker","plugins":[PLUGIN]},
                 {"id":"verify","op":"verify"}],
        "edges":[{"from":"worker","to":"verify"}]
    })
}

fn install_plugin(host: &Host, media: &MediaFixture) -> Value {
    let bundle = host.base.root.path().join("bundle");
    let plugin = bundle.join("plugins").join(PLUGIN);
    fs::create_dir_all(plugin.join("skills/media")).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        json!({"name":"Media fixture","description":"Loopback-only fixture images.","skills":"./skills"}).to_string(),
    )
    .unwrap();
    fs::write(
        plugin.join(".mcp.json"),
        json!({"mcpServers":{"loopback":{"type":"http","url":media.endpoint}}}).to_string(),
    )
    .unwrap();
    fs::write(
        plugin.join("skills/media/SKILL.md"),
        "---\nname: media-fixture\ndescription: Inspect fixture image results.\n---\nUse media-loopback_fixture_media in mixed mode once. On recovery, inspect rather than issue again.\n",
    )
    .unwrap();
    let binding = FilePluginCatalog::new(&bundle)
        .resolve(&[PLUGIN.into()])
        .unwrap()
        .remove(0);
    let summary = json!({
        "id":binding.id,"digest":binding.digest,
        "resources":binding.resources,"mcp_servers":binding.mcp_servers
    });
    assert_eq!(summary["mcp_servers"], json!(["loopback"]));
    assert_eq!(
        summary["resources"],
        json!([".mcp.json", "plugin.json", "skills/media/SKILL.md"])
    );
    fs::write(
        bundle.join("manifest.json"),
        json!({"format":1,"graph":"graph.json","plugins":[summary]}).to_string(),
    )
    .unwrap();
    summary
}

fn native_pair(history: &Value, name: &str, mode: &str) -> (Value, Value) {
    let blocks = history
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().unwrap())
        .collect::<Vec<_>>();
    let request = blocks
        .iter()
        .find(|block| {
            block["type"] == "toolRequest"
                && block["toolCall"]["value"]["name"] == name
                && block["toolCall"]["value"]["arguments"]["mode"] == mode
        })
        .unwrap_or_else(|| panic!("missing native tool request for {name}: {history}"));
    let response = blocks
        .iter()
        .find(|block| block["type"] == "toolResponse" && block["id"] == request["id"])
        .unwrap_or_else(|| panic!("missing native tool response for {name}: {history}"));
    assert_eq!(request["toolCall"]["status"], "success");
    assert_eq!(response["toolResult"]["status"], "success");
    ((**request).clone(), (**response).clone())
}

fn content_text(block: &Value) -> &str {
    assert_eq!(block["type"], "text", "expected native MCP Text: {block}");
    block["text"].as_str().unwrap()
}

fn assert_image(block: &Value, picture: &Picture) {
    assert_eq!(
        block["type"], "image",
        "must be native MCP Image, not JSON Text"
    );
    assert_eq!(block["mimeType"], picture.mime_type);
    assert_eq!(block["data"], picture.data);
    assert_eq!(
        STANDARD.decode(block["data"].as_str().unwrap()).unwrap(),
        picture.bytes
    );
}

fn assert_mixed(response: &Value, pictures: &[Picture]) -> String {
    let result = &response["toolResult"]["value"];
    assert_ne!(result["isError"], true, "{response}");
    let content = result["content"].as_array().unwrap();
    assert_eq!(
        content.len(),
        9,
        "receipt followed by the original ordered content"
    );
    let header: Value = serde_json::from_str(content_text(&content[0])).unwrap();
    assert_eq!(header["ok"], true);
    assert_eq!(
        header["images"],
        json!(
            pictures
                .iter()
                .map(|picture| json!({
                    "type":"image","mime_type":picture.mime_type,
                    "bytes":picture.bytes.len(),"data_sha256":picture.sha256()
                }))
                .collect::<Vec<_>>()
        )
    );
    let receipt = header["anchor_receipt"].as_str().unwrap();
    assert_eq!(receipt.len(), 64);
    assert!(
        receipt
            .bytes()
            .all(|character| character.is_ascii_hexdigit())
    );
    for picture in pictures {
        assert!(!header.to_string().contains(&picture.data));
    }
    assert_eq!(content_text(&content[1]), FIRST_TEXT);
    assert_eq!(
        serde_json::from_str::<Value>(content_text(&content[2])).unwrap(),
        structured()
    );
    assert_image(&content[3], &pictures[0]);
    assert_eq!(content_text(&content[4]), MIDDLE_TEXT);
    assert_image(&content[5], &pictures[1]);
    assert_eq!(content_text(&content[6]), "media-before-webp");
    assert_image(&content[7], &pictures[2]);
    assert_eq!(content_text(&content[8]), LAST_TEXT);
    receipt.into()
}

fn image_urls(value: &Value) -> Vec<String> {
    let mut urls = Vec::new();
    fn visit(value: &Value, urls: &mut Vec<String>) {
        match value {
            Value::Array(values) => {
                for value in values {
                    visit(value, urls);
                }
            }
            Value::Object(values) => {
                if values.get("type").and_then(Value::as_str) == Some("image_url") {
                    urls.push(value["image_url"]["url"].as_str().unwrap().into());
                }
                for value in values.values() {
                    visit(value, urls);
                }
            }
            _ => {}
        }
    }
    visit(value, &mut urls);
    urls
}

fn image_metadata<'record>(record: &'record Value, hash: &str) -> Option<&'record Value> {
    match record {
        Value::Object(values) => {
            if values.get("data_sha256").and_then(Value::as_str) == Some(hash) {
                Some(record)
            } else {
                values
                    .values()
                    .find_map(|value| image_metadata(value, hash))
            }
        }
        Value::Array(values) => values.iter().find_map(|value| image_metadata(value, hash)),
        _ => None,
    }
}

fn assert_saved_metadata(record: &Value, pictures: &[Picture]) {
    for picture in pictures {
        let hash = picture.sha256();
        let metadata = image_metadata(record, &hash)
            .unwrap_or_else(|| panic!("missing saved image digest {hash}: {record}"));
        assert_eq!(metadata["type"], "image");
        assert_eq!(metadata["mime_type"], picture.mime_type);
        assert_eq!(metadata["bytes"], picture.bytes.len());
        assert!(metadata.get("data").is_none());
        assert!(
            !record.to_string().contains(&picture.data),
            "Anchor must not duplicate image base64"
        );
    }
}

fn assert_artifacts(host: &Host, run: &str) -> Value {
    let saved = host.record(run);
    for (node, name) in [("worker", "evidence.txt"), ("verify", "verified.txt")] {
        assert_eq!(saved["results"][node].as_array().unwrap().len(), 1);
        assert_eq!(saved["results"][node][0]["key"]["invocation"], 1);
        assert_eq!(host.base.file(&saved, node, name), EVIDENCE);
        let manifest = fixture::read_json(host.base.artifact(&saved, node).join("manifest.json"));
        assert_eq!(
            manifest["files"][name]["sha256"],
            format!("{:x}", Sha256::digest(EVIDENCE))
        );
        assert_eq!(manifest["files"][name]["bytes"], EVIDENCE.len());
    }
    assert_eq!(
        host.base.workspace_files(run, "evidence.txt"),
        vec![EVIDENCE.to_vec()]
    );
    assert_eq!(
        host.base.workspace_files(run, "verified.txt"),
        vec![EVIDENCE.to_vec()]
    );
    saved
}

#[test]
#[ignore = "requires pinned Goose v1.53.0 binary and sqlite3"]
fn native_goose_mixed_media_preserves_blocks_vision_wire_receipt_and_artifacts() {
    let media = MediaFixture::new();
    assert!(media.pictures[0].bytes.len() > 1024 * 1024);
    assert!(media.pictures[0].data.len() > 1024 * 1024);
    assert!(media.pictures[0].data.len() < 2 * 1024 * 1024);
    let provider = Provider::with_model(
        "goose-media-mixed-vision",
        vec![
            command("printf media-workspace-checked > evidence.txt; cat evidence.txt"),
            Step::tool(EXPOSED_TOOL, json!({"mode":"mixed"})).after("media-workspace-checked"),
            Step::tool(
                "final_result",
                json!({
                    "summary":"guessed receipt must not complete","route":"verify",
                    "observed_receipt":"guessed-image-receipt"
                }),
            )
            .after(LAST_TEXT),
            complete("verify").after("after observing business results"),
            Step::text("observed native images and completed the checked branch"),
        ],
        VISION_MODEL,
    );
    let host = Host::new(&graph()).default_runtime();
    let binding = install_plugin(&host, &media);
    let response = host.run(&provider);
    assert_eq!(response["status"], "completed", "{response}");
    provider.assert_consumed();
    let requests = provider.requests();
    assert_eq!(requests.len(), 5);
    assert!(
        requests
            .iter()
            .all(|request| request["model"] == VISION_MODEL)
    );
    assert!(image_urls(&requests[1]["messages"]).is_empty());
    assert_eq!(
        image_urls(&requests[2]["messages"]),
        media
            .pictures
            .iter()
            .map(Picture::data_uri)
            .collect::<Vec<_>>()
    );
    assert!(
        requests[2]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "user" && !image_urls(&message["content"]).is_empty()
            })
    );
    let tool_name = tool_definition(&requests[1], EXPOSED_TOOL).unwrap()["function"]["name"]
        .as_str()
        .unwrap();
    let history = host.native_conversation("fixture", "worker", 1);
    let (native_request, native_response) = native_pair(&history, tool_name, "mixed");
    assert_eq!(
        native_request["toolCall"]["value"]["arguments"],
        json!({"mode":"mixed"})
    );
    let receipt = assert_mixed(&native_response, &media.pictures);
    assert!(serde_json::to_vec(&native_response).unwrap().len() > 1024 * 1024);
    let (fact, native, _) = host.native_record("fixture", "worker", 1);
    assert_saved_metadata(&fact, &media.pictures);
    assert_saved_metadata(&native["tool_calls"], &media.pictures);
    assert_eq!(native["tool_calls"][2]["result"]["ok"], false);
    assert_eq!(native["tool_calls"][3]["result"]["ok"], true);
    assert_eq!(
        native["tool_calls"][3]["arguments"]["observed_receipt"],
        receipt
    );
    assert_eq!(
        fact["completion"]["output"],
        json!({"summary":"fixture complete","route":"verify"})
    );
    let saved = assert_artifacts(&host, "fixture");
    let snapshot = media.snapshot();
    assert!(snapshot["tool_lists"].as_u64().unwrap() > 0);
    assert_eq!(snapshot["calls"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["effects"].as_array().unwrap().len(), 1);
    assert_eq!(host.run(&provider)["status"], "completed");
    assert_eq!(host.record("fixture"), saved);
    assert_eq!(provider.requests(), requests);
    assert_eq!(media.snapshot(), snapshot);
    host.evidence(
        &provider,
        "fixture",
        json!({
            "case_source":"tests/goose_media.rs","vision_model":VISION_MODEL,
            "plugin_binding":binding,"mcp":snapshot,"native_media_response":native_response,
            "native_mcp_order":true,"png_jpeg_webp":true,"embedded_blob_image":true,
            "real_model_wire_image_urls":true,"receipt_bound_to_media":true,
            "image_metadata_hashes":true,"workspace_and_artifacts":true,
            "png_dimensions":[640,640],"image_result_above_old_acp_frame_limit":true,
            "completed_restart_without_replay":true,"real_model_requests":0
        }),
    );
}

fn rejected_media(scenario: &str, mode: &str) {
    let media = MediaFixture::new();
    let provider = Provider::with_model(
        scenario,
        vec![
            Step::tool(EXPOSED_TOOL, json!({"mode":mode})),
            Step::text("media rejected; no successful completion is claimed")
                .after("anchor_receipt"),
        ],
        VISION_MODEL,
    );
    let host = Host::new(&graph()).native();
    let binding = install_plugin(&host, &media);
    let response = host.run(&provider);
    assert_eq!(response["status"], "stopped", "{response}");
    provider.assert_consumed();
    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request["model"], VISION_MODEL);
        assert!(image_urls(&request["messages"]).is_empty());
        assert!(!request["messages"].to_string().contains(REJECTED_TEXT));
        for picture in media.pictures.iter() {
            assert!(!request["messages"].to_string().contains(&picture.data));
        }
    }
    let tool_name = tool_definition(&requests[0], EXPOSED_TOOL).unwrap()["function"]["name"]
        .as_str()
        .unwrap();
    let history = host.native_conversation("fixture", "worker", 1);
    let (_, native_response) = native_pair(&history, tool_name, mode);
    let result = &native_response["toolResult"]["value"];
    assert_eq!(result["isError"], true, "{native_response}");
    assert!(
        result["content"]
            .as_array()
            .unwrap()
            .iter()
            .all(|block| block["type"] == "text")
    );
    assert!(!result.to_string().contains(REJECTED_TEXT));
    assert!(!result.to_string().contains("media-structured-content"));
    let feedback: Value = serde_json::from_str(content_text(&result["content"][0])).unwrap();
    assert_eq!(feedback["ok"], false);
    assert!(
        feedback["error"]
            .as_str()
            .is_some_and(|error| !error.is_empty())
    );
    assert_eq!(feedback["anchor_receipt"].as_str().unwrap().len(), 64);
    let (fact, native, _) = host.native_record("fixture", "worker", 1);
    assert!(fact["completion"].is_null());
    assert_eq!(native["tool_calls"][0]["result"]["ok"], false);
    let saved = host.record("fixture");
    assert!(saved["results"].get("worker").is_none());
    assert!(saved["results"].get("verify").is_none());
    assert!(
        host.base
            .workspace_files("fixture", "verified.txt")
            .is_empty()
    );
    let snapshot = media.snapshot();
    assert_eq!(snapshot["calls"].as_array().unwrap().len(), 1);
    assert!(snapshot["effects"].as_array().unwrap().is_empty());
    host.evidence(
        &provider,
        "fixture",
        json!({
            "case_source":"tests/goose_media.rs","mode":mode,"vision_model":VISION_MODEL,
            "plugin_binding":binding,"mcp":snapshot,"native_media_error":native_response,
            "native_is_error":true,"no_partial_text_json_or_image":true,
            "no_completion_or_artifact":true,"real_model_requests":0
        }),
    );
}

#[test]
#[ignore = "requires pinned Goose v1.53.0 binary and sqlite3"]
fn native_goose_media_mime_bytes_mismatch_is_fail_closed() {
    rejected_media("goose-media-bad-mime", "bad_mime");
}

#[test]
#[ignore = "requires pinned Goose v1.53.0 binary and sqlite3"]
fn native_goose_media_invalid_embedded_base64_is_fail_closed() {
    rejected_media("goose-media-invalid-blob-base64", "invalid_base64");
}

#[test]
#[ignore = "requires pinned Goose v1.53.0 binary and sqlite3"]
fn native_goose_media_invalid_image_bytes_is_fail_closed() {
    rejected_media("goose-media-invalid-image", "invalid_image");
}

#[test]
#[ignore = "requires pinned Goose v1.53.0 binary and sqlite3"]
fn native_goose_media_unsupported_image_mime_is_fail_closed() {
    rejected_media("goose-media-unsupported-mime", "unsupported_mime");
}

#[test]
#[ignore = "requires pinned Goose v1.53.0 binary and sqlite3"]
fn native_goose_media_restart_checks_same_session_without_reissuing_effect() {
    let media = MediaFixture::new();
    let gate = Gate::new();
    let provider = Provider::with_model(
        "goose-media-restart-inspect",
        vec![
            command("printf media-workspace-checked > evidence.txt; cat evidence.txt"),
            Step::tool(EXPOSED_TOOL, json!({"mode":"mixed"})).after("media-workspace-checked"),
            complete("verify").after(LAST_TEXT).gated(&gate),
            complete("verify"),
            Step::tool(EXPOSED_TOOL, json!({"mode":"inspect"}))
                .after("after observing business results"),
            complete("verify").after(LAST_TEXT),
            Step::text("inspected the issued images without issuing a second effect"),
        ],
        VISION_MODEL,
    );
    let host = Host::new(&graph()).native();
    let binding = install_plugin(&host, &media);
    let mut server = host.serve(&provider);
    let run = server.trigger();
    gate.wait_entered();
    let before_fact = host.native_fact(&run, "worker", 1);
    let session = before_fact["session_id"].clone();
    assert!(session.as_str().is_some_and(|value| !value.is_empty()));
    assert!(before_fact["completion"].is_null());
    assert_eq!(before_fact["tool_observation"]["tool"], EXPOSED_TOOL);
    assert_eq!(
        before_fact["tool_observation"]["arguments"],
        json!({"mode":"mixed"})
    );
    assert_saved_metadata(&before_fact, &media.pictures);
    let before_history = host.native_conversation(&run, "worker", 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 3);
    let name = tool_definition(&requests[1], EXPOSED_TOOL).unwrap()["function"]["name"]
        .as_str()
        .unwrap();
    let (_, before_response) = native_pair(&before_history, name, "mixed");
    let before_receipt = assert_mixed(&before_response, &media.pictures);
    server.kill();
    drop(server);
    gate.open();
    let restarted = host.serve(&provider);
    let (status, accepted) = restarted.request("POST", &format!("/runs/{run}/resume"), None);
    assert_eq!(status, 202, "{accepted}");
    restarted.wait_status(&run, "completed");
    provider.assert_consumed();
    let (fact, native, _) = host.native_record(&run, "worker", 1);
    assert_eq!(fact["session_id"], session);
    assert_eq!(native["resume"], true);
    assert_saved_metadata(&fact, &media.pictures);
    assert_saved_metadata(&native["tool_calls"], &media.pictures);
    assert_eq!(
        fact["tool_observation"]["arguments"],
        json!({"mode":"inspect"})
    );
    let snapshot = media.snapshot();
    assert_eq!(snapshot["calls"].as_array().unwrap().len(), 2);
    assert_eq!(snapshot["calls"][0]["arguments"]["mode"], "mixed");
    assert_eq!(snapshot["calls"][1]["arguments"]["mode"], "inspect");
    assert_eq!(snapshot["effects"].as_array().unwrap().len(), 1);
    let requests = provider.requests();
    assert_eq!(requests.len(), 7);
    assert!(
        requests[3]["messages"]
            .to_string()
            .contains("do not blindly repeat")
    );
    let resumed_images = image_urls(&requests[5]["messages"]);
    for picture in media.pictures.iter() {
        assert!(resumed_images.contains(&picture.data_uri()));
    }
    let calls = native["tool_calls"].as_array().unwrap();
    assert_eq!(calls[0]["tool"], "final_result");
    assert_eq!(calls[0]["result"]["ok"], false);
    assert_eq!(calls[1]["tool"], EXPOSED_TOOL);
    assert_eq!(calls[1]["arguments"], json!({"mode":"inspect"}));
    assert_eq!(calls[2]["tool"], "final_result");
    assert_eq!(calls[2]["result"]["ok"], true);
    assert_ne!(calls[2]["arguments"]["observed_receipt"], before_receipt);
    assert_artifacts(&host, &run);
    let history = host.native_conversation(&run, "worker", 1);
    let (_, inspected_response) = native_pair(&history, name, "inspect");
    let inspected_receipt = assert_mixed(&inspected_response, &media.pictures);
    assert_eq!(calls[2]["arguments"]["observed_receipt"], inspected_receipt);
    host.evidence(
        &provider,
        &run,
        json!({
            "case_source":"tests/goose_media.rs","vision_model":VISION_MODEL,
            "plugin_binding":binding,"mcp":snapshot,"native_session":session,
            "before_restart_fact":before_fact,"before_restart_media":before_response,
            "native_history":history,"same_session_and_invocation":true,
            "stale_receipt_rejected_before_inspection":true,
            "agent_inspects_before_completion":true,"issued_effect_count":1,
            "workspace_and_artifacts":true,"real_model_requests":0
        }),
    );
}
