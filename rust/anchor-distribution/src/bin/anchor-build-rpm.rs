#![forbid(unsafe_code)]

use anchor_distribution::{NativeRpmRequest, build_native_rpm};
use clap::Parser;
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    version,
    about = "Build a fresh-install x86_64 RPM from an admitted platform archive"
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
    release: String,
    #[arg(long, default_value = "rpmbuild")]
    rpmbuild: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let result = build_native_rpm(&NativeRpmRequest {
        archive: arguments.archive,
        sha256: arguments.sha256,
        cli: arguments.cli,
        release: arguments.release,
        rpmbuild: arguments.rpmbuild,
        output: arguments.output,
    });
    match result.and_then(|report| Ok(serde_json::to_string(&report)?)) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("anchor-build-rpm: {error}");
            ExitCode::FAILURE
        }
    }
}
