#![forbid(unsafe_code)]

use anchor_distribution::{
    PackageRequest, PlatformPackageRequest, ToolBinary, build_package, build_platform_package,
};
use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Build a deterministic source-free Goose Runtime archive"
)]
struct Arguments {
    #[arg(long)]
    host: PathBuf,
    #[arg(long)]
    goose: PathBuf,
    #[arg(
        long,
        required_unless_present = "platform",
        conflicts_with = "platform"
    )]
    bundle: Option<PathBuf>,
    /// Build the complete platform with an empty business catalog.
    #[arg(long)]
    platform: bool,
    #[arg(long)]
    web: Option<PathBuf>,
    #[arg(long = "tool", value_name = "NAME=ELF", value_parser = parse_tool, conflicts_with = "platform")]
    tools: Vec<ToolBinary>,
    #[arg(long, value_name = "ELF", conflicts_with = "platform")]
    scholarly: Option<PathBuf>,
    #[arg(long, value_name = "ELF", conflicts_with = "platform")]
    docmost_tools: Option<PathBuf>,
    #[arg(long, value_name = "ELF")]
    wecom_tools: Option<PathBuf>,
    #[arg(long, value_name = "ELF")]
    wecom_gateway: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    feishu_gateway: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    net_relay: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    sandbox_network: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    platform_launcher: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    network_tools: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    git_tools: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    uv: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    rustup_init: Option<PathBuf>,
    #[arg(long, value_name = "ELF", requires = "platform")]
    pixi: Option<PathBuf>,
    #[arg(long)]
    output: PathBuf,
}

fn parse_tool(value: &str) -> Result<ToolBinary, String> {
    let (name, path) = value
        .split_once('=')
        .filter(|(name, path)| !name.is_empty() && !path.is_empty())
        .ok_or_else(|| "tool must have the form NAME=ELF".to_owned())?;
    Ok(ToolBinary {
        name: name.into(),
        path: path.into(),
    })
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    match package(arguments) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("anchor-distribution: {error}");
            ExitCode::FAILURE
        }
    }
}

fn package(arguments: Arguments) -> anchor_distribution::Result<String> {
    if arguments.platform {
        let required = |value: Option<PathBuf>, flag: &str| {
            value.ok_or_else(|| {
                anchor_distribution::PackageError::Invalid(format!("--platform requires {flag}"))
            })
        };
        let request = PlatformPackageRequest {
            host: arguments.host,
            goose: arguments.goose,
            net_relay: required(arguments.net_relay, "--net-relay")?,
            sandbox_network: required(arguments.sandbox_network, "--sandbox-network")?,
            platform_launcher: required(arguments.platform_launcher, "--platform-launcher")?,
            web: required(arguments.web, "--web")?,
            network_tools: required(arguments.network_tools, "--network-tools")?,
            git_tools: required(arguments.git_tools, "--git-tools")?,
            wecom_tools: required(arguments.wecom_tools, "--wecom-tools")?,
            feishu_gateway: required(arguments.feishu_gateway, "--feishu-gateway")?,
            wecom_gateway: required(arguments.wecom_gateway, "--wecom-gateway")?,
            uv: required(arguments.uv, "--uv")?,
            rustup_init: required(arguments.rustup_init, "--rustup-init")?,
            pixi: required(arguments.pixi, "--pixi")?,
            output: arguments.output,
        };
        return Ok(serde_json::to_string(&build_platform_package(&request)?)?);
    }
    let request = PackageRequest {
        host: arguments.host,
        goose: arguments.goose,
        bundle: arguments
            .bundle
            .expect("clap requires --bundle for a Graph package"),
        web: arguments.web,
        tools: arguments.tools,
        scholarly: arguments.scholarly,
        docmost_tools: arguments.docmost_tools,
        wecom_tools: arguments.wecom_tools,
        wecom_gateway: arguments.wecom_gateway,
        output: arguments.output,
    };
    Ok(serde_json::to_string(&build_package(&request)?)?)
}
