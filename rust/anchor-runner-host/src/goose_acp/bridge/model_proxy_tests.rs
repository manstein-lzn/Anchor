use super::*;
use anchor_runtime::{ToolDefinition, ToolError, ToolResultContent};
use std::{future::Future, pin::Pin};

struct NoTools;
impl ToolPort for NoTools {
    fn definitions(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    fn call<'a>(
        &'a self,
        name: &'a str,
        _: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ToolResultContent>, ToolError>> + Send + 'a>> {
        Box::pin(async move { Err(ToolError::Unknown(name.into())) })
    }
}

#[tokio::test]
async fn model_routes_have_no_request_budget_and_still_honor_cancellation() {
    anchor_mcp_host::initialize_crypto_provider();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let provider = Router::new().route(
        "/model",
        post(move || {
            let observed = observed.clone();
            async move {
                observed.fetch_add(1, Ordering::SeqCst);
                Json(json!({"response":"same invocation"}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let provider_task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
    let cancellation = Cancellation::default();
    let bridge = Bridge::start(
        Arc::new(NoTools),
        NodeBridgeContext {
            routes: Vec::new(),
            plugin_instructions: String::new(),
            observation_path: None,
            outputs: DeclaredOutputs {
                workspace: std::env::temp_dir(),
                writes: Vec::new(),
            },
            completion_check: None,
        },
        cancellation.clone(),
        Some(super::super::configuration::ModelUpstream {
            url: reqwest::Url::parse(&format!("http://{address}/model")).unwrap(),
            api_key: "fixture-only".into(),
        }),
        false,
        "fixture-token".into(),
    )
    .await
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let routes = [
        "/v1/chat/completions",
        "/anchor/v1/chat/completions",
        "/v1/responses",
    ];
    // Cross the retired per-arm ceiling on each transport without resetting the invocation.
    let per_route = 2049;
    for route in routes {
        for _ in 0..per_route {
            let response = client
                .post(format!("{}{route}", bridge.url))
                .bearer_auth(&bridge.token)
                .json(&json!({"model":"fixture","messages":[]}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.json::<Value>().await.unwrap(),
                json!({"response":"same invocation"})
            );
        }
    }
    let total = routes.len() * per_route;
    assert_eq!(calls.load(Ordering::SeqCst), total);
    assert_eq!(
        bridge.state.provider_calls.load(Ordering::SeqCst),
        total as u64
    );

    cancellation.store(true, Ordering::Release);
    for route in routes {
        let response = client
            .post(format!("{}{route}", bridge.url))
            .bearer_auth(&bridge.token)
            .json(&json!({"model":"fixture","messages":[]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    assert_eq!(calls.load(Ordering::SeqCst), total);
    assert_eq!(
        bridge.state.provider_calls.load(Ordering::SeqCst),
        total as u64
    );
    bridge.close().await.unwrap();
    provider_task.abort();
}
