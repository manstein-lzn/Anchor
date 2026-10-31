use anchor_briefing_tools::BriefingServer;
use rmcp::ServiceExt;
use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() -> Result<(), anchor_briefing_tools::Error> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 3 && args[0] == "package-plugin" {
        anchor_briefing_tools::package_plugin(Path::new(&args[1]), Path::new(&args[2]))?;
        return Ok(());
    }
    if args.len() == 6 && args[0] == "seed-baseline" {
        let server = BriefingServer::new(std::env::current_dir()?, &args[1])?;
        println!(
            "{}",
            server.seed_baseline(Path::new(&args[2]), &args[3], &args[4], &args[5])?
        );
        return Ok(());
    }
    if !args.is_empty() {
        return Err("usage: anchor-briefing-tools [package-plugin <destination> <typst> | seed-baseline <plugin-root> <pdf> <issue-date> <cutoff> <window-start>]".into());
    }
    let workspace = std::env::var_os("ANCHOR_BRIEFING_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    let root = std::env::var_os("ANCHOR_BRIEFING_ROOT")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    BriefingServer::new(workspace, root)?
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}
