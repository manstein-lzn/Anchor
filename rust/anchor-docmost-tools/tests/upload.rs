mod support;

use std::fs;

use anchor_docmost_tools::{Config, MAX_UPLOAD_BYTES, UploadError, Uploader};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use support::{ATTACHMENT_ID, Fixture, PAGE_ID, TOKEN, expected_body, metadata};

fn uploader(fixture: &Fixture, root: &std::path::Path) -> Uploader {
    Uploader::new(
        Config::new(TOKEN)
            .with_endpoint(&fixture.endpoint)
            .expect("fixture endpoint")
            .with_upload_root(root)
            .expect("fixture root"),
    )
    .expect("fixture uploader")
}

#[tokio::test]
async fn multipart_preserves_original_fields_filename_bytes_and_json() {
    for (filename, mime, bytes) in [
        (
            "报告 +%.SvG",
            "image/svg+xml",
            b"<svg>\0\xff\r\n</svg>".as_slice(),
        ),
        ("report.PNG", "image/png", b"\x89PNG\0\r\n\xff".as_slice()),
        ("report.jpg", "image/jpeg", b"\xff\xd8\0\xff".as_slice()),
        ("report.JPEG", "image/jpeg", b"jpeg\0\xff".as_slice()),
        ("report.webp", "image/webp", b"RIFF\0WEBP\xff".as_slice()),
    ] {
        for existing in [None, Some(ATTACHMENT_ID)] {
            let fixture =
                Fixture::start(StatusCode::OK, metadata(filename, mime), HeaderMap::new()).await;
            let root = tempfile::tempdir().expect("upload root");
            let path = root.path().join(filename);
            fs::write(&path, bytes).expect("write input image");
            let result = uploader(&fixture, root.path())
                .upload(path.to_str().expect("input path"), PAGE_ID, existing)
                .await
                .expect("upload");
            assert_eq!(result.attachment_id, ATTACHMENT_ID);
            assert_eq!(result.file_name, filename);
            assert_eq!(result.mime_type, mime);
            assert_eq!(result.page_id, PAGE_ID);
            let encoded = if filename.starts_with('报') {
                "%E6%8A%A5%E5%91%8A%20%2B%25.SvG"
            } else {
                filename
            };
            assert_eq!(result.url, format!("/api/files/{ATTACHMENT_ID}/{encoded}"));
            assert_eq!(
                serde_json::to_value(&result).expect("result JSON"),
                serde_json::json!({
                    "attachmentId": ATTACHMENT_ID,
                    "fileName": filename,
                    "url": format!("/api/files/{ATTACHMENT_ID}/{encoded}"),
                    "mimeType": mime,
                    "pageId": PAGE_ID
                })
            );
            let requests = fixture.requests.lock().expect("requests lock");
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests[0].body,
                expected_body(&requests[0], filename, mime, bytes, existing)
            );
            assert_eq!(fs::read(path).expect("input unchanged"), bytes);
        }
    }
}

#[tokio::test]
async fn maximum_size_is_inclusive_and_filename_url_is_percent_encoded() {
    let fixture = Fixture::start(
        StatusCode::OK,
        metadata("folder/报告 % +?#.png", "image/png"),
        HeaderMap::new(),
    )
    .await;
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("maximum.png");
    let bytes = vec![0x89; MAX_UPLOAD_BYTES];
    fs::write(&path, &bytes).expect("maximum image");
    let result = uploader(&fixture, root.path())
        .upload(path.to_str().expect("path"), PAGE_ID, None)
        .await
        .expect("exactly 20 MiB accepted");
    assert_eq!(
        result.url,
        format!("/api/files/{ATTACHMENT_ID}/folder/%E6%8A%A5%E5%91%8A%20%25%20%2B%3F%23.png")
    );
    let requests = fixture.requests.lock().expect("requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].body,
        expected_body(&requests[0], "maximum.png", "image/png", &bytes, None)
    );
}

#[tokio::test]
async fn http_redirect_json_and_metadata_failures_are_redacted_and_not_retried() {
    let mut redirect = HeaderMap::new();
    redirect.insert("location", HeaderValue::from_static("/redirect-target"));
    let mut mismatched =
        serde_json::from_slice::<serde_json::Value>(&metadata("image.png", "image/png"))
            .expect("metadata");
    mismatched["pageId"] = TOKEN.into();
    let cases = [
        (
            StatusCode::UNAUTHORIZED,
            TOKEN.as_bytes().to_vec(),
            HeaderMap::new(),
            UploadError::Http(401),
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            TOKEN.as_bytes().to_vec(),
            HeaderMap::new(),
            UploadError::Http(429),
        ),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            TOKEN.as_bytes().to_vec(),
            HeaderMap::new(),
            UploadError::Http(500),
        ),
        (
            StatusCode::TEMPORARY_REDIRECT,
            Vec::new(),
            redirect,
            UploadError::Http(307),
        ),
        (
            StatusCode::OK,
            TOKEN.as_bytes().to_vec(),
            HeaderMap::new(),
            UploadError::Json,
        ),
        (
            StatusCode::OK,
            b"null".to_vec(),
            HeaderMap::new(),
            UploadError::Json,
        ),
        (
            StatusCode::OK,
            b"{}".to_vec(),
            HeaderMap::new(),
            UploadError::Json,
        ),
        (
            StatusCode::OK,
            serde_json::to_vec(&mismatched).expect("mismatch"),
            HeaderMap::new(),
            UploadError::Metadata,
        ),
        (
            StatusCode::OK,
            metadata(TOKEN, "text/html"),
            HeaderMap::new(),
            UploadError::Metadata,
        ),
        (
            StatusCode::OK,
            vec![b' '; 1024 * 1024 + 1],
            HeaderMap::new(),
            UploadError::Json,
        ),
    ];
    for (status, response, headers, expected) in cases {
        let fixture = Fixture::start(status, response, headers).await;
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("image.png");
        fs::write(&path, b"image").expect("image");
        let error = uploader(&fixture, root.path())
            .upload(path.to_str().expect("path"), PAGE_ID, None)
            .await
            .expect_err("failure must surface");
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        assert_eq!(fixture.requests.lock().expect("requests").len(), 1);
    }
}

#[tokio::test]
async fn lost_upload_response_does_not_resend_the_request() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let endpoint = format!("http://{}/upload", listener.local_addr().expect("address"));
    let root = tempfile::tempdir().expect("root");
    let path = root.path().join("image.png");
    fs::write(&path, b"bytes").expect("image");
    let uploader = Uploader::new(
        Config::new(TOKEN)
            .with_endpoint(&endpoint)
            .expect("endpoint")
            .with_upload_root(root.path())
            .expect("root"),
    )
    .expect("uploader");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("first connection");
        let mut received = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = socket.read(&mut buffer).await.expect("read upload");
            assert!(count > 0);
            received.extend_from_slice(&buffer[..count]);
            if let Some(header_end) = received.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&received[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.parse::<usize>().ok())
                    })
                    .expect("content length");
                if received.len() >= header_end + 4 + length {
                    break;
                }
            }
        }
        socket.shutdown().await.expect("close without response");
        drop(socket);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(250), listener.accept())
                .await
                .is_err()
        );
    });
    let error = uploader
        .upload(path.to_str().expect("path"), PAGE_ID, None)
        .await
        .expect_err("unknown upload outcome");
    assert_eq!(error, UploadError::Transport);
    assert!(!error.to_string().contains(TOKEN));
    server.await.expect("response-loss fixture");
}
