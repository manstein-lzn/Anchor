#![forbid(unsafe_code)]

use anchor_distribution::{PackageRequest, ToolBinary, build_package};
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
    #[arg(long)]
    bundle: PathBuf,
    #[arg(long)]
    web: Option<PathBuf>,
    #[arg(long = "tool", value_name = "NAME=ELF", value_parser = parse_tool)]
    tools: Vec<ToolBinary>,
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
    let request = PackageRequest {
        host: arguments.host,
        goose: arguments.goose,
        bundle: arguments.bundle,
        web: arguments.web,
        tools: arguments.tools,
        output: arguments.output,
    };
    match build_package(&request).and_then(|report| Ok(serde_json::to_string(&report)?)) {
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
