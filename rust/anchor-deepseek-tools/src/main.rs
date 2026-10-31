use anchor_deepseek_tools::{Config, DeepSeekServer, Error};
use rmcp::ServiceExt;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Error> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    if arguments.len() == 2 && arguments[0] == "package-plugin" {
        anchor_deepseek_tools::package_plugin(&arguments[1])?;
        return Ok(());
    }
    if !arguments.is_empty() {
        return Err("usage: anchor-deepseek-tools [package-plugin <destination>]".into());
    }
    let config = Config {
        api_key: std::env::var("ANCHOR_DEEPSEEK_API_KEY")
            .map_err(|_| "ANCHOR_DEEPSEEK_API_KEY is required")?,
        base_url: std::env::var("ANCHOR_DEEPSEEK_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com".into()),
        workspace: std::env::var_os("ANCHOR_DEEPSEEK_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/workspace")),
    };
    DeepSeekServer::new(config)?
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
