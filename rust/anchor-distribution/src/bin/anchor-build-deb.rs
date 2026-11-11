#![forbid(unsafe_code)]

use anchor_distribution::{NativeDebRequest, build_native_deb};
use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    version,
    about = "Build a fresh-install amd64 DEB from an admitted platform archive"
)]
struct Arguments {
    #[arg(long)]
    archive: PathBuf,
    #[arg(long)]
    sha256: String,
    /// Reviewed native Anchor service-management ELF.
    #[arg(long)]
    cli: PathBuf,
    #[arg(long, default_value = "1")]
    revision: String,
    #[arg(long, default_value = "dpkg-deb")]
    dpkg_deb: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let result = build_native_deb(&NativeDebRequest {
        archive: arguments.archive,
        sha256: arguments.sha256,
        cli: arguments.cli,
        revision: arguments.revision,
        dpkg_deb: arguments.dpkg_deb,
        output: arguments.output,
    });
    match result.and_then(|report| Ok(serde_json::to_string(&report)?)) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("anchor-build-deb: {error}");
            ExitCode::FAILURE
        }
    }
}
