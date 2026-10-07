use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};

pub const PAGE_ID: &str = "a2c71876-bdc8-4d75-a4e3-42b83fa93c02";
pub const ATTACHMENT_ID: &str = "ef9cbfbe-c84a-4a87-beb2-04f1988433af";
pub const TOKEN: &str = "fixture-docmost-secret-do-not-print";

#[derive(Clone)]
pub struct RecordedRequest {
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Clone)]
struct FixtureState {
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    status: StatusCode,
    response: Vec<u8>,
    response_headers: HeaderMap,
}

pub struct Fixture {
    pub endpoint: String,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Fixture {
    pub async fn start(status: StatusCode, response: Vec<u8>, response_headers: HeaderMap) -> Self {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = FixtureState {
            requests: requests.clone(),
            status,
            response,
            response_headers,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind isolated HTTP fixture");
        let address = listener.local_addr().expect("fixture address");
        let router = Router::new()
            .route("/{*path}", post(capture))
            .layer(DefaultBodyLimit::max(30 * 1024 * 1024))
            .with_state(state);
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve fixture");
        });
        Self {
            endpoint: format!("http://{address}/api/files/upload"),
            requests,
            task,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn capture(
    State(state): State<FixtureState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    state
        .requests
        .lock()
        .expect("fixture requests lock")
        .push(RecordedRequest {
            headers,
            body: body.to_vec(),
        });
    (state.status, state.response_headers, state.response)
}

pub fn metadata(filename: &str, mime: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "id": ATTACHMENT_ID,
        "fileName": filename,
        "pageId": PAGE_ID,
        "mimeType": mime,
        "ignored": "server field"
    }))
    .expect("fixture response JSON")
}

pub fn expected_body(
    request: &RecordedRequest,
    filename: &str,
    mime: &str,
    bytes: &[u8],
    attachment_id: Option<&str>,
) -> Vec<u8> {
    assert_eq!(request.headers["authorization"], format!("Bearer {TOKEN}"));
    let content_type = request.headers["content-type"]
        .to_str()
        .expect("multipart content type");
    let boundary = content_type
        .strip_prefix("multipart/form-data; boundary=")
        .expect("multipart boundary");
    let mut body = Vec::new();
    if let Some(existing) = attachment_id {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"attachmentId\"\r\n\r\n{existing}\r\n").as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"pageId\"\r\n\r\n{PAGE_ID}\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    body
}
