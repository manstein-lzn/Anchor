//! Original Graph shell commands and the deterministic routing helper.

use anchor_runtime_rig::{ReadOnlyInput, SandboxEnvironment, SandboxRequest};
use std::path::Path;

const ROUTE_HELPER: &str = "/tools/anchor-runtime/anchor-route";

pub(crate) fn shell_command(command: &str, allowed: &[String]) -> Result<Vec<String>, String> {
    if command.trim().is_empty() || command.contains('\0') {
        return Err("Op.run must be a non-empty shell command without NUL bytes".into());
    }
    if !allowed.iter().any(|name| name == "sh") {
        return Err("Op.run requires `sh` in ANCHOR_RUNNER_ALLOWED_COMMANDS".into());
    }
    Ok(vec!["sh".into(), "-c".into(), command.into()])
}

pub(crate) fn route_helper_mount() -> Result<ReadOnlyInput, String> {
    Ok(ReadOnlyInput::new(
        std::env::current_exe().map_err(|error| format!("route helper unavailable: {error}"))?,
        ROUTE_HELPER,
    ))
}

pub(crate) fn add_route_helper(request: &mut SandboxRequest, mount: ReadOnlyInput) {
    request.readonly_inputs.push(mount);
    let directories = std::iter::once("/tools/anchor-runtime".to_owned())
        .chain(
            request
                .tool_dirs
                .iter()
                .map(|path| path.to_string_lossy().into_owned()),
        )
        .chain(["/usr/bin".into(), "/bin".into()])
        .collect::<Vec<_>>()
        .join(":");
    request
        .environment
        .push(SandboxEnvironment::new("PATH", directories));
}

/// A read-only mount of this binary named anchor-route needs no installed
/// Anchor source, console script, or Python interpreter.
pub(crate) fn route_cli_args() -> Option<Vec<String>> {
    let mut args = std::env::args();
    if Path::new(&args.next()?).file_name()? == "anchor-route" {
        return Some(args.collect());
    }
    if args.next().as_deref() == Some("anchor-route") {
        return Some(args.collect());
    }
    None
}

pub(crate) fn run_route_cli(args: &[String]) -> i32 {
    let (target, reason) = match parse_route_args(args) {
        Ok(Some(arguments)) => arguments,
        Ok(None) => {
            println!("usage: anchor-route --to TARGET [--reason REASON]");
            return 0;
        }
        Err(error) => {
            eprintln!("anchor-route: {error}");
            return 2;
        }
    };
    let routes = std::env::var("ANCHOR_ROUTES").unwrap_or_default();
    let allowed = routes
        .split(',')
        .filter(|name| !name.is_empty())
        .collect::<Vec<_>>();
    if allowed.is_empty() {
        eprintln!("anchor-route: this node has no route to choose");
        return 1;
    }
    if !allowed.contains(&target.as_str()) {
        eprintln!(
            "anchor-route: {target:?} is not a way out of this node. Choose one of: {}",
            allowed.join(", ")
        );
        return 1;
    }
    println!("ANCHOR_ROUTE: {target}");
    if !reason.is_empty() {
        println!("{reason}");
    }
    0
}

fn parse_route_args(args: &[String]) -> Result<Option<(String, String)>, String> {
    let mut target = None;
    let mut reason = String::new();
    let mut args = args.iter();
    while let Some(argument) = args.next() {
        let value = match argument.as_str() {
            "--help" | "-h" => return Ok(None),
            "--to" | "--reason" => args
                .next()
                .ok_or_else(|| format!("{argument} requires a value"))?
                .clone(),
            _ if argument.starts_with("--to=") => argument[5..].to_owned(),
            _ if argument.starts_with("--reason=") => argument[9..].to_owned(),
            _ => return Err(format!("unrecognized argument {argument:?}")),
        };
        if argument == "--to" || argument.starts_with("--to=") {
            target = Some(value);
        } else {
            reason = value;
        }
    }
    Ok(Some((target.ok_or("--to is required")?, reason)))
}
