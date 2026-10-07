use std::time::Duration;

use anchor_wecom_gateway::{ConnectionStatus, Gateway, GatewayConfig, GatewayError};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("anchor-wecom-gateway: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), GatewayError> {
    if std::env::args_os().len() != 1 {
        return Err(GatewayError::Invalid(
            "gateway accepts configuration only through its environment",
        ));
    }
    let gateway = Gateway::start(GatewayConfig::from_env()?).await?;
    let mut errors = gateway.subscribe_errors();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|_| GatewayError::Stopped)?;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = terminate.recv() => break,
            Ok(error) = errors.recv() => eprintln!("anchor-wecom-gateway: {error}"),
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                if gateway.status() == ConnectionStatus::Stopped { break; }
            },
        }
    }
    gateway.shutdown().await
}
