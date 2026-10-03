use anchor_rsi::{
    ecosystem::Ecosystem,
    evidence::{Config, Evidence},
    mcp::RsiService,
};
use axum::Router;
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use std::{
    io::{self, Write},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let evidence = Arc::new(Evidence::collect(Config::from_environment()?)?);
    let ecosystem = Arc::new(Ecosystem::new()?);
    let service = RsiService {
        evidence,
        ecosystem,
    };
    let cancellation = CancellationToken::new();
    let mcp: StreamableHttpService<RsiService, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(service.clone()),
        Default::default(),
        StreamableHttpServerConfig::default()
            .with_sse_keep_alive(None)
            .with_cancellation_token(cancellation.child_token()),
    );
    let router = Router::new().nest_service("/mcp", mcp);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    println!("http://{}/mcp", listener.local_addr()?);
    io::stdout().flush()?;
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancellation.cancel();
        })
        .await?;
    Ok(())
}
