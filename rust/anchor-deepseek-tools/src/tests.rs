use super::*;
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};

fn fixture(workspace: &Path, endpoint: String) -> DeepSeekServer {
    let mut server = DeepSeekServer::new(Config {
        api_key: "fixture-key".into(),
        base_url: "https://api.deepseek.com/v1/".into(),
        workspace: workspace.into(),
    })
    .unwrap();
    server.endpoint = Url::parse(&endpoint).unwrap();
    server
}

use std::path::Path;

#[tokio::test]
async fn preserves_full_search_and_fetch_evidence_while_bounding_tool_previews() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let full_text = "证据原文。".repeat(20_000);
    let served_text = full_text.clone();
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/v1/tools/search",
                    post(|headers: HeaderMap, Json(body): Json<Value>| async move {
                        assert_eq!(headers["authorization"], "Bearer fixture-key");
                        assert_eq!(body["query"], "AI news after:2026-10-30");
                        assert_eq!(body["count"], 10);
                        assert_eq!(body["need_content"], false);
                        Json(json!({"results":[{
                            "title":"Original announcement", "url":"https://example.com/source",
                            "snippet":"Discovery only", "date":"2026-10-31T00:00:00Z"
                        }]}))
                    }),
                )
                .route(
                    "/v1/tools/fetch",
                    post(
                        move |headers: HeaderMap, Json(body): Json<Value>| async move {
                            assert_eq!(headers["authorization"], "Bearer fixture-key");
                            assert_eq!(body["url"], "https://example.com/source");
                            Json(json!({"title":"Original announcement", "content":served_text}))
                        },
                    ),
                ),
        )
        .await
        .unwrap();
    });
    let workspace = tempfile::tempdir().unwrap();
    let server = fixture(workspace.path(), endpoint);
    let search = server
        .search(
            serde_json::from_value(json!({
                "query":"AI news after:2026-10-30", "save_as":"evidence/search.json"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(search["ok"], true);
    assert_eq!(search["http_status"], 200);
    assert_eq!(
        search["response"]["results"][0]["url"],
        "https://example.com/source"
    );
    assert_eq!(search["response_truncated"], false);
    let fetched = server
        .fetch(FetchRequest {
            url: "https://example.com/source".into(),
            save_as: "evidence/source.json".into(),
        })
        .await
        .unwrap();
    assert_eq!(fetched["ok"], true);
    assert_eq!(fetched["response_truncated"], true);
    assert!(fetched.to_string().len() < MAX_PREVIEW + 1024);
    let saved: Value = serde_json::from_slice(
        &std::fs::read(workspace.path().join("evidence/source.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["response"]["content"], full_text);
    assert_eq!(saved["request"]["url"], "https://example.com/source");
    assert!(chrono::DateTime::parse_from_rfc3339(saved["fetched_at"].as_str().unwrap()).is_ok());
    assert!(!saved.to_string().contains("fixture-key"));
    task.abort();
}

#[tokio::test]
async fn saves_http_failures_without_redirecting_and_confines_new_evidence_files() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/tools/fetch",
                post(|Json(body): Json<Value>| async move {
                    if body["url"] == "https://example.com/redirect" {
                        (
                            StatusCode::TEMPORARY_REDIRECT,
                            [("location", "http://127.0.0.1:1/credential-leak")],
                            "redirect fixture-key",
                        )
                            .into_response()
                    } else {
                        (
                            StatusCode::FORBIDDEN,
                            Json(json!({"error":"upstream forbidden"})),
                        )
                            .into_response()
                    }
                }),
            ),
        )
        .await
        .unwrap();
    });
    let workspace = tempfile::tempdir().unwrap();
    let server = fixture(workspace.path(), endpoint);
    let request = |url: &str, path: &str| FetchRequest {
        url: url.into(),
        save_as: path.into(),
    };
    let denied = server
        .fetch(request(
            "https://example.com/forbidden",
            "evidence/denied.json",
        ))
        .await
        .unwrap();
    assert_eq!(denied["ok"], false);
    assert_eq!(denied["http_status"], 403);
    let redirect = server
        .fetch(request(
            "https://example.com/redirect",
            "evidence/redirect.json",
        ))
        .await
        .unwrap();
    assert_eq!(redirect["ok"], false);
    assert_eq!(redirect["http_status"], 307);
    assert!(!redirect.to_string().contains("fixture-key"));
    assert!(
        server
            .fetch(request(
                "https://example.com/source",
                "evidence/denied.json"
            ))
            .await
            .is_err()
    );
    assert!(
        server
            .fetch(request("https://example.com/source", "../escape.json"))
            .await
            .is_err()
    );
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), workspace.path().join("link")).unwrap();
    assert!(
        server
            .fetch(request("https://example.com/source", "link/escape.json"))
            .await
            .is_err()
    );
    assert!(!outside.path().join("escape.json").exists());
    assert!(endpoint_url("http://api.deepseek.com").is_err());
    assert!(endpoint_url("https://user:password@api.deepseek.com").is_err());
    assert!(endpoint_url("https://api.deepseek.com?credential=value").is_err());
    assert_eq!(
        endpoint_url("https://api.deepseek.com/v1/")
            .unwrap()
            .as_str(),
        "https://api.deepseek.com/"
    );
    task.abort();
}
