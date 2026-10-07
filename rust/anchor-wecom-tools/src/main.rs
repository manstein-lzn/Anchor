use anchor_wecom_tools::{WecomService, package_plugin};
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("anchor-wecom-tools: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    match arguments.next() {
        None => {
            let service = WecomService::from_env()?
                .serve(rmcp::transport::stdio())
                .await
                .map_err(|_| "MCP initialization failed")?;
            service.waiting().await.map_err(|_| "MCP service failed")?;
        }
        Some(command) if command == "package-plugin" => {
            let destination = arguments
                .next()
                .ok_or("usage: anchor-wecom-tools package-plugin <destination>")?;
            if arguments.next().is_some() {
                return Err("usage: anchor-wecom-tools package-plugin <destination>".into());
            }
            package_plugin(destination)?;
        }
        Some(_) => return Err("usage: anchor-wecom-tools [package-plugin <destination>]".into()),
    }
    Ok(())
}
