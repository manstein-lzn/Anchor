use crate::{BridgeConfig, manager::Manager, mcp::CodexService};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::{SessionManager, local::LocalSessionManager},
};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub struct RunningBridge {
    pub address: SocketAddr,
    manager: Arc<Manager>,
    sessions: Arc<LocalSessionManager>,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}
impl RunningBridge {
    pub fn endpoint(&self) -> String {
        format!("http://{}/mcp", self.address)
    }

    pub async fn shutdown(mut self) -> Result<(), String> {
        self.cancellation.cancel();
        let ids = self
            .sessions
            .sessions
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            let _ = tokio::time::timeout(Duration::from_secs(2), self.sessions.close_session(&id))
                .await;
        }
        self.manager.shutdown().await;
        match tokio::time::timeout(Duration::from_secs(5), &mut self.task).await {
            Ok(result) => result
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string()),
            Err(_) => {
                self.task.abort();
                let _ = (&mut self.task).await;
                Err("HTTP shutdown timed out".into())
            }
        }
    }
}

impl Drop for RunningBridge {
    fn drop(&mut self) {
        self.cancellation.cancel();
        self.task.abort();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let manager = self.manager.clone();
            runtime.spawn(async move {
                manager.shutdown().await;
            });
        }
    }
}

async fn authorize(State(manager): State<Arc<Manager>>, request: Request, next: Next) -> Response {
    // A local bearer is a single-user trust boundary. Browser-origin requests are not admitted.
    if request.headers().contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let provided = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    let authenticated = provided.is_some_and(|provided| {
        let actual = Sha256::digest(provided.as_bytes());
        let expected = Sha256::digest(manager.config.bearer_token.as_bytes());
        actual
            .iter()
            .zip(expected.iter())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    });
    if !authenticated {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(request).await
}

pub async fn start_server(config: BridgeConfig) -> Result<RunningBridge, String> {
    let config = config.validate().map_err(|e| e.to_string())?;
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let manager = Manager::new(config);
    let handler = CodexService(manager.clone());
    let cancellation = CancellationToken::new();
    let sessions = anchor_mcp_host::local_session_manager();
    let service: StreamableHttpService<CodexService, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(handler.clone()),
            sessions.clone(),
            StreamableHttpServerConfig::default()
                .with_sse_keep_alive(None)
                .with_cancellation_token(cancellation.child_token()),
        );
    let router = Router::new()
        .nest_service("/mcp", service)
        .route(
            "/healthz",
            get(|| async {
                axum::Json(serde_json::json!({"status":"ok","codex":"lazy_connection"}))
            }),
        )
        .layer(middleware::from_fn_with_state(manager.clone(), authorize));
    let stopped = cancellation.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(stopped.cancelled_owned())
            .await
    });
    Ok(RunningBridge {
        address,
        manager,
        sessions,
        cancellation,
        task,
    })
}
