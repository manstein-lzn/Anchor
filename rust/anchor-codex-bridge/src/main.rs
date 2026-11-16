use anchor_codex_bridge::{config::parse_cli_args, start_server};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let Some(config) = parse_cli_args(&args)? else {
        println!(
            "anchor-codex-bridge serve --allow-root <ABS_PATH> [--allow-root <ABS_PATH> ...]\n  [--listen 127.0.0.1:8091] [--worktree-root <ABS_PATH>] [--codex-bin <PATH>]\nRequires ANCHOR_CODEX_BRIDGE_TOKEN (at least 32 bytes). Uses the local Codex configuration."
        );
        return Ok(());
    };
    let server = start_server(config).await?;
    eprintln!("Anchor Codex bridge listening at {}", server.endpoint());
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! { result = tokio::signal::ctrl_c() => result?, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    server.shutdown().await?;
    Ok(())
}
