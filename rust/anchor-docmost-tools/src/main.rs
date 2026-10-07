use anchor_docmost_tools::{AttachmentServer, Config, Uploader, package_plugin};
use rmcp::ServiceExt;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 2 && arguments[0] == "package-plugin" {
        package_plugin(&arguments[1])?;
        return Ok(());
    }
    let config = match arguments.as_slice() {
        [] => Config::from_env(),
        [flag, endpoint] if flag == "--endpoint" => Config::from_env().with_endpoint(
            endpoint
                .to_str()
                .ok_or("endpoint must be a valid HTTP(S) URL")?,
        )?,
        _ => {
            return Err(
                "usage: anchor-docmost-tools [--endpoint <url>] | package-plugin <destination>"
                    .into(),
            );
        }
    };
    let service = AttachmentServer::new(Uploader::new(config)?)
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|_| "Docmost MCP initialization failed")?;
    service
        .waiting()
        .await
        .map_err(|_| "Docmost MCP service failed")?;
    Ok(())
}
