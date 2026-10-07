mod support;

use anchor_wecom_tools::{Config, WecomService, tools};
use axum::http::StatusCode;
use rmcp::model::CallToolResult;
use serde_json::{Map, Value, json};
use std::{sync::atomic::Ordering, time::Duration};
use support::{Fixture, Reply};

fn service(fixture: &Fixture) -> WecomService {
    WecomService::new(Config::new(" corp &中文 ", " +42 ", " secret &中文 ", &fixture.url).unwrap())
        .unwrap()
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

fn content(result: &CallToolResult) -> Value {
    let serialized = serde_json::to_value(result).unwrap();
    assert_eq!(serialized["content"].as_array().unwrap().len(), 1);
    assert_eq!(serialized["content"][0]["type"], "text");
    serde_json::from_str(serialized["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn error(result: &CallToolResult, expected: &str) {
    assert_eq!(result.is_error, Some(true));
    assert!(
        content(result)["error"]
            .as_str()
            .unwrap()
            .contains(expected),
        "{result:?}"
    );
}

#[test]
fn schemas_names_and_descriptions_are_unchanged() {
    assert_eq!(
        serde_json::to_value(tools()).unwrap(),
        json!([
            {"name":"wecom_send_text", "description":"Send a text message through a WeCom application.",
             "inputSchema":{"type":"object", "properties":{
                "content":{"type":"string"}, "touser":{"type":"string"},
                "toparty":{"type":"string"}, "totag":{"type":"string"}},"required":["content"]}},
            {"name":"wecom_send_markdown", "description":"Send a Markdown message through a WeCom application.",
             "inputSchema":{"type":"object", "properties":{
                "content":{"type":"string"}, "touser":{"type":"string"},
                "toparty":{"type":"string"}, "totag":{"type":"string"}},"required":["content"]}},
            {"name":"wecom_get_user", "description":"Get a WeCom member by userid.",
             "inputSchema":{"type":"object", "properties":{"userid":{"type":"string"}},"required":["userid"]}}
        ])
    );
}

#[test]
fn invalid_url_configuration_never_echoes_the_value() {
    for url in [
        "",
        "bad-secret-url",
        "file:///tmp/secret",
        "http://user:secret@localhost",
        "http://localhost/?secret=bad",
        "http://localhost/#secret",
    ] {
        let error = Config::new("corp", "42", "secret", url)
            .err()
            .unwrap()
            .to_string();
        assert_eq!(error, "WECOM_API_BASE_URL is invalid");
    }
    assert!(Config::new("", "", "", "https://qyapi.weixin.qq.com/").is_ok());
}

#[tokio::test]
async fn request_payloads_targets_and_query_encoding_match_python() {
    let fixture = Fixture::start().await;
    let service = service(&fixture);
    for (name, msgtype) in [
        ("wecom_send_text", "text"),
        ("wecom_send_markdown", "markdown"),
    ] {
        let result = service.call(name, args(json!({
            "content":" 中文内容\n ", "touser":"成员|other", "toparty":"2|3", "totag":"4", "extra":"ignored"
        }))).await.unwrap();
        assert_eq!(result.is_error, Some(false));
        assert_eq!(content(&result)["msgid"], "fixture-message");
        let requests = fixture.state.requests.lock().unwrap();
        let (kind, query, body) = requests.last().unwrap();
        assert_eq!(kind, "send");
        assert_eq!(query["access_token"], "fixture-token-1");
        let body = body.as_ref().unwrap();
        assert_eq!(body["agentid"], 42);
        assert_eq!(body["msgtype"], msgtype);
        assert_eq!(body[msgtype]["content"], " 中文内容\n ");
        assert_eq!(body["touser"], "成员|other");
        assert_eq!(body["toparty"], "2|3");
        assert_eq!(body["totag"], "4");
        assert!(body.get("extra").is_none());
    }
    service
        .call(
            "wecom_send_text",
            args(json!({"content":"ok", "touser":null, "toparty":2, "totag":""})),
        )
        .await
        .unwrap();
    {
        let requests = fixture.state.requests.lock().unwrap();
        let body = requests.last().unwrap().2.as_ref().unwrap();
        assert_eq!(body["toparty"], "2");
        assert!(body.get("touser").is_none());
        assert!(body.get("totag").is_none());
        assert_eq!(requests[0].1["corpid"], "corp &中文");
        assert_eq!(requests[0].1["corpsecret"], "secret &中文");
    }
    let result = service
        .call("wecom_get_user", args(json!({"userid":" 成员 &中文 "})))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(false));
    let requests = fixture.state.requests.lock().unwrap();
    assert_eq!(requests.last().unwrap().1["userid"], " 成员 &中文 ");
    assert_eq!(fixture.state.token_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn invalid_arguments_and_configuration_do_not_reach_send() {
    let fixture = Fixture::start().await;
    let service = service(&fixture);
    for value in [
        Value::Null,
        json!(0),
        json!(false),
        json!(""),
        json!(" \n\t "),
    ] {
        error(
            &service
                .call(
                    "wecom_send_text",
                    args(json!({"content":value,"touser":"user"})),
                )
                .await
                .unwrap(),
            "content is required",
        );
        error(
            &service
                .call("wecom_get_user", args(json!({"userid":value})))
                .await
                .unwrap(),
            "userid is required",
        );
    }
    error(
        &service
            .call(
                "wecom_send_text",
                args(json!({"content":"ok","touser":"","toparty":null})),
            )
            .await
            .unwrap(),
        "one of touser",
    );
    for (corp, agent, secret, expected) in [
        ("corp", "", "secret", "WECOM_AGENT_ID is not configured"),
        (
            "corp",
            "42.0",
            "secret",
            "WECOM_AGENT_ID must be an integer",
        ),
        (
            "corp",
            "secret-value",
            "secret",
            "WECOM_AGENT_ID must be an integer",
        ),
        ("", "42", "secret", "WECOM_CORP_ID is not configured"),
        ("corp", "42", "  ", "WECOM_SECRET is not configured"),
    ] {
        let service =
            WecomService::new(Config::new(corp, agent, secret, &fixture.url).unwrap()).unwrap();
        error(
            &service
                .call(
                    "wecom_send_text",
                    args(json!({"content":"ok","touser":"user"})),
                )
                .await
                .unwrap(),
            expected,
        );
    }
    assert_eq!(fixture.state.token_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.sends.load(Ordering::SeqCst), 0);
    let unknown = service
        .call("secret-unknown-tool", Map::new())
        .await
        .unwrap_err();
    assert_eq!(unknown.code.0, -32602);
    assert_eq!(unknown.message, "unknown tool");
}

#[tokio::test]
async fn user_query_does_not_require_application_agent_id() {
    let fixture = Fixture::start().await;
    let service =
        WecomService::new(Config::new("corp", "", "secret", &fixture.url).unwrap()).unwrap();
    let result = service
        .call("wecom_get_user", args(json!({"userid":"user"})))
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(false));
    assert_eq!(fixture.state.users.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn cached_token_refreshes_once_across_concurrent_calls_and_expiry_margin() {
    let fixture = Fixture::start().await;
    let service = service(&fixture);
    for expected_tokens in [1, 2, 3] {
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..16 {
            let service = service.clone();
            tasks.spawn(async move {
                service
                    .call(
                        "wecom_get_user",
                        args(json!({"userid":format!("user-{index}")})),
                    )
                    .await
                    .unwrap()
            });
        }
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap().is_error, Some(false));
        }
        assert_eq!(
            fixture.state.token_calls.load(Ordering::SeqCst),
            expected_tokens
        );
        if expected_tokens < 3 {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(if expected_tokens == 1 {
                7141
            } else {
                7201
            }))
            .await;
            tokio::time::resume();
        }
    }
    assert_eq!(fixture.state.users.load(Ordering::SeqCst), 48);
    let requests = fixture.state.requests.lock().unwrap();
    assert_eq!(
        requests.last().unwrap().1["access_token"],
        "fixture-token-3"
    );
}

#[tokio::test]
async fn token_failures_are_sanitized_and_never_cached() {
    let fixture = Fixture::start().await;
    let service = service(&fixture);
    for (reply, expected) in [
        (
            Reply {
                status: StatusCode::BAD_GATEWAY,
                body: "secret &中文 access_token=fixture-token-1".into(),
            },
            "HTTP failure 502",
        ),
        (
            Reply::json(
                json!({"errcode":40001,"errmsg":"secret &中文 https://example.invalid/?access_token=fixture-token-1"}),
            ),
            "API error 40001",
        ),
        (
            Reply::json(json!({"errcode":0})),
            "did not return access_token",
        ),
        (
            Reply::json(json!({"access_token":""})),
            "did not return access_token",
        ),
        (Reply::json(json!(["secret &中文"])), "invalid response"),
        (
            Reply::json(json!({"access_token":"fixture-token-1", "expires_in":null})),
            "invalid response",
        ),
        (
            Reply::json(json!({"access_token":"fixture-token-1", "expires_in":"NaN"})),
            "invalid response",
        ),
        (
            Reply {
                status: StatusCode::OK,
                body: "not-json-secret".into(),
            },
            "invalid response",
        ),
    ] {
        *fixture.state.token_reply.lock().unwrap() = Some(reply);
        let result = service
            .call(
                "wecom_send_text",
                args(json!({"content":"ok","touser":"user"})),
            )
            .await
            .unwrap();
        error(&result, expected);
        let text = content(&result).to_string();
        for private in [
            "secret &中文",
            "access_token=",
            "fixture-token-1",
            "example.invalid",
            "not-json-secret",
        ] {
            assert!(!text.contains(private), "{text}");
        }
    }
    assert_eq!(fixture.state.token_calls.load(Ordering::SeqCst), 8);
    assert_eq!(fixture.state.sends.load(Ordering::SeqCst), 0);
    *fixture.state.token_reply.lock().unwrap() = None;
    assert_eq!(
        service
            .call("wecom_get_user", args(json!({"userid":"user"})))
            .await
            .unwrap()
            .is_error,
        Some(false)
    );
}

#[tokio::test]
async fn send_failure_records_unknown_delivery_or_api_rejection_without_retry() {
    let fixture = Fixture::start().await;
    let service = service(&fixture);
    for (index, reply) in [
        Reply {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "secret &中文 fixture-token-1".into(),
        },
        Reply {
            status: StatusCode::TEMPORARY_REDIRECT,
            body: "redirect".into(),
        },
        Reply {
            status: StatusCode::OK,
            body: "secret-invalid-json".into(),
        },
        Reply::json(json!(["secret-array"])),
        Reply::json(json!({"errcode":"secret-errcode"})),
        Reply::json(json!({"errcode":40014,"errmsg":"secret &中文 ?access_token=fixture-token-1"})),
    ]
    .into_iter()
    .enumerate()
    {
        *fixture.state.send_reply.lock().unwrap() = reply;
        let result = service
            .call(
                "wecom_send_text",
                args(json!({"content":"ok","touser":"user"})),
            )
            .await
            .unwrap();
        if index == 5 {
            error(&result, "API error 40014");
            assert!(content(&result).get("delivery").is_none());
        } else {
            error(&result, "delivery is unknown");
            assert_eq!(content(&result)["delivery"], "unknown");
        }
        let text = content(&result).to_string();
        for private in [
            "secret &中文",
            "fixture-token-1",
            "access_token=",
            "secret-invalid-json",
            "secret-errcode",
        ] {
            assert!(!text.contains(private));
        }
        assert_eq!(fixture.state.sends.load(Ordering::SeqCst), index + 1);
        assert_eq!(fixture.state.token_calls.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn user_http_and_api_errors_are_tool_errors_without_private_response_text() {
    let fixture = Fixture::start().await;
    let service = service(&fixture);
    for (reply, expected) in [
        (
            Reply {
                status: StatusCode::FORBIDDEN,
                body: "fixture-token-1 secret".into(),
            },
            "HTTP failure 403",
        ),
        (
            Reply::json(json!({"errcode":60111,"errmsg":"fixture-token-1 secret"})),
            "API error 60111",
        ),
    ] {
        *fixture.state.user_reply.lock().unwrap() = reply;
        let result = service
            .call("wecom_get_user", args(json!({"userid":"user"})))
            .await
            .unwrap();
        error(&result, expected);
        assert!(!content(&result).to_string().contains("fixture-token-1"));
    }
    assert_eq!(fixture.state.users.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.state.sends.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn accepted_post_with_lost_response_is_unknown_and_is_never_repeated() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    async fn request(
        listener: &tokio::net::TcpListener,
    ) -> (tokio::net::TcpStream, String, Vec<u8>) {
        let (socket, _) = listener.accept().await.unwrap();
        let mut reader = BufReader::new(socket);
        let mut first_line = String::new();
        reader.read_line(&mut first_line).await.unwrap();
        let mut size = 0;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).await.unwrap();
            if header == "\r\n" {
                break;
            }
            if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                size = value.trim().parse().unwrap();
            }
        }
        let mut body = vec![0; size];
        reader.read_exact(&mut body).await.unwrap();
        (reader.into_inner(), first_line, body)
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, first_line, body) = request(&listener).await;
        assert!(first_line.starts_with("GET /cgi-bin/gettoken?"));
        assert!(body.is_empty());
        let token =
            json!({"errcode":0,"access_token":"private-lost-response-token","expires_in":7200})
                .to_string();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{token}",
                    token.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.shutdown().await.unwrap();
        let (mut socket, first_line, body) = request(&listener).await;
        assert!(first_line.starts_with("POST /cgi-bin/message/send?"));
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["text"]["content"], "single fixture effect");
        socket.shutdown().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
    });
    let service =
        WecomService::new(Config::new("corp", "42", "private-lost-response-secret", &url).unwrap())
            .unwrap();
    let result = service
        .call(
            "wecom_send_text",
            args(json!({"content":"single fixture effect","touser":"user"})),
        )
        .await
        .unwrap();
    error(&result, "delivery is unknown");
    assert_eq!(content(&result)["delivery"], "unknown");
    assert!(
        !content(&result)
            .to_string()
            .contains("private-lost-response")
    );
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn unavailable_endpoint_reports_a_sanitized_transport_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let service =
        WecomService::new(Config::new("private-corp", "42", "private-secret", &url).unwrap())
            .unwrap();
    let result = service
        .call("wecom_get_user", args(json!({"userid":"user"})))
        .await
        .unwrap();
    error(&result, "API request failed");
    for private in [
        url.as_str(),
        "private-corp",
        "private-secret",
        "corpsecret=",
    ] {
        assert!(!content(&result).to_string().contains(private));
    }
}
