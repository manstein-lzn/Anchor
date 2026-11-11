#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use std::{
    os::unix::process::CommandExt,
    process::{Command, ExitCode, Stdio},
};

const SERVICE: &str = "anchor.service";
const WEB_URL: &str = "http://127.0.0.1:8077";

#[derive(Parser)]
#[command(
    version,
    about = "Manage the installed Anchor service and open its local WebUI"
)]
struct Arguments {
    #[command(subcommand)]
    command: Option<Operation>,
}

#[derive(Subcommand)]
enum Operation {
    /// Start Anchor using the system's normal service authorization.
    Start,
    /// Stop Anchor and its managed processes.
    Stop,
    /// Restart the installed Anchor service.
    Restart,
    /// Show the service status and local WebUI address (the default).
    Status,
    /// Show the most recent 100 service log entries.
    Logs {
        /// Continue following new log entries.
        #[arg(short, long)]
        follow: bool,
    },
    /// Print the WebUI address and open a browser when a user desktop is available.
    Open,
    /// Internal entrypoint for the native package's system service.
    #[command(hide = true)]
    Serve,
}

fn main() -> ExitCode {
    let (program, arguments): (&str, &[&str]) =
        match Arguments::parse().command.unwrap_or(Operation::Status) {
            Operation::Start => ("systemctl", &["start", SERVICE]),
            Operation::Stop => ("systemctl", &["stop", SERVICE]),
            Operation::Restart => ("systemctl", &["restart", SERVICE]),
            Operation::Status => {
                println!("{WEB_URL}");
                ("systemctl", &["status", SERVICE, "--no-pager"])
            }
            Operation::Logs { follow: false } => {
                ("journalctl", &["-u", SERVICE, "-n100", "--no-pager"])
            }
            Operation::Logs { follow: true } => (
                "journalctl",
                &["-u", SERVICE, "-n100", "--no-pager", "--follow"],
            ),
            Operation::Open => {
                open();
                return ExitCode::SUCCESS;
            }
            Operation::Serve => return execute(service_entrypoint()),
        };

    let mut command = Command::new(program);
    command.args(arguments);
    execute(command)
}

fn execute(mut command: Command) -> ExitCode {
    // Replacing the CLI preserves the service tool's exit status and signals.
    let error = command.exec();
    eprintln!(
        "anchor: {}: {error}",
        command.get_program().to_string_lossy()
    );
    ExitCode::FAILURE
}

fn service_entrypoint() -> Command {
    // The RPM service enters through /usr/bin's standard SELinux executable label;
    // the admitted platform launcher still owns all runtime and environment setup.
    let mut command =
        Command::new("/var/lib/anchor/platform/release/anchor-runtime/bin/anchor-platform");
    command.arg("start");
    command
}

fn open() {
    println!("{WEB_URL}");
    if rustix::process::getuid().is_root()
        || rustix::process::geteuid().is_root()
        || !["DISPLAY", "WAYLAND_DISPLAY"]
            .iter()
            .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
    {
        return;
    }

    let result = Command::new("xdg-open")
        .arg(WEB_URL)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if !result.is_ok_and(|status| status.success()) {
        eprintln!("anchor: browser unavailable; open {WEB_URL} manually");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_service_entrypoint_only_delegates_to_the_fixed_launcher() {
        let command = super::service_entrypoint();
        assert_eq!(
            command.get_program(),
            "/var/lib/anchor/platform/release/anchor-runtime/bin/anchor-platform"
        );
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["start"]);
        assert_eq!(command.get_envs().count(), 0);
    }
}
