use std::io::{self, Write};

use anchor_scholarly::{Scholarly, cli::Cli, mcp::serve_stdio};
use clap::Parser;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let arguments = Cli::parse();
    if arguments.is_mcp() {
        return match serve_stdio(&Scholarly::new()).await {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("anchor-scholarly: MCP stdio failed: {error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    match arguments.execute(&Scholarly::new()).await {
        Ok(result) => {
            let mut output = io::stdout().lock();
            if serde_json::to_writer(&mut output, &result).is_err() || writeln!(output).is_err() {
                eprintln!("anchor-scholarly: OSError: could not write JSON output");
                return std::process::ExitCode::FAILURE;
            }
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("anchor-scholarly: {}", error.summary(1000));
            std::process::ExitCode::FAILURE
        }
    }
}
