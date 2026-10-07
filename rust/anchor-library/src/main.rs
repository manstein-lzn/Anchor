#![forbid(unsafe_code)]

use std::{path::PathBuf, process::ExitCode};

use anchor_library::{InstallRequest, Library};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "anchor-library", about = "Operator Plugin installation")]
struct Cli {
    #[arg(long)]
    root: PathBuf,
    #[command(subcommand)]
    command: Operation,
}

#[derive(Subcommand)]
enum Operation {
    Install {
        #[arg(
            long,
            conflicts_with = "directory",
            required_unless_present = "directory"
        )]
        source: Option<String>,
        #[arg(long, conflicts_with = "source", requires = "id")]
        directory: Option<PathBuf>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        replace_existing: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let library = Library::new(cli.root);
    let outcome = match cli.command {
        Operation::Install {
            source,
            directory,
            id,
            replace_existing,
        } => match directory {
            Some(directory) => {
                library.install_directory(id.as_deref().unwrap(), directory, replace_existing)
            }
            None => library.install(&InstallRequest {
                source: source.unwrap(),
                id,
                replace_existing,
            }),
        },
    };
    match outcome {
        Ok(outcome) => match serde_json::to_string(&outcome) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(_) => {
                eprintln!("cannot encode Plugin installation result");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
