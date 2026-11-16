//! One application-bound CLI adapter, not an API-by-API tool catalog.
use std::{
    collections::BTreeSet, io::ErrorKind, path::Path, process::Stdio, sync::Arc, time::Duration,
};

use anchor_channel_transport::{digest, ledger::Ledger};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::watch,
};

use crate::{GatewayConfig, GatewayError, api::Api, protocol::valid_id, worker::while_running};

const FILE_LIMIT: usize = 20 * 1024 * 1024;
const TEXT_LIMIT: usize = 48 * 1024;
const TIME_LIMIT: Duration = Duration::from_secs(120);

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Invocation {
    pub argv: Vec<String>,
    #[serde(default)]
    pub stdin: String,
    #[serde(default)]
    pub files: Vec<InputFile>,
    #[serde(default)]
    pub outputs: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InputFile {
    pub name: String,
    pub data_base64: String,
}

fn filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !matches!(name, "." | "..")
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn validate(invocation: &Invocation) -> Result<(), GatewayError> {
    let invalid = GatewayError::Invalid(
        "invalid CLI invocation; use argv, inline JSON or staged workspace files",
    );
    if invocation.argv.is_empty()
        || invocation.argv.len() > 128
        || invocation.argv.iter().any(|s| s.contains('\0'))
        || invocation.argv.iter().map(String::len).sum::<usize>() > 128 * 1024
        || invocation.stdin.len() > 128 * 1024
        || invocation.files.len() > 8
        || invocation.outputs.len() > 8
    {
        return Err(invalid);
    }
    // Management commands can change accounts, reveal credentials, install code
    // or start a competing event listener. Business domains stay discoverable.
    if matches!(
        invocation.argv[0].as_str(),
        "auth"
            | "authen"
            | "config"
            | "profile"
            | "update"
            | "doctor"
            | "event"
            | "whoami"
            | "completion"
            | "login"
            | "logout"
            | "__complete"
            | "__completeNoDesc"
    ) || (invocation.argv[0].starts_with('-')
        && (invocation.argv.len() != 1
            || !matches!(
                invocation.argv[0].as_str(),
                "--help" | "-h" | "--version" | "-v"
            )))
        || invocation.argv.iter().any(|arg| {
            matches!(
                arg.split('=').next().unwrap_or_default(),
                "--as" | "--profile" | "--debug" | "--verbose" | "--trace"
            )
        })
    {
        return Err(GatewayError::Invalid(
            "CLI identity, credentials, configuration and event lifecycle are managed by Anchor",
        ));
    }
    let api_help =
        invocation.argv.len() == 2 && matches!(invocation.argv[1].as_str(), "--help" | "-h");
    if invocation.argv[0] == "api" && !api_help {
        // Require method/path before flags: otherwise a flag value can occupy
        // argv[2] while Cobra dispatches a different, unchecked endpoint.
        let method = invocation
            .argv
            .get(1)
            .map(String::as_str)
            .unwrap_or_default();
        let path = invocation
            .argv
            .get(2)
            .map(String::as_str)
            .unwrap_or_default();
        let domain = path
            .strip_prefix("/open-apis/")
            .and_then(|path| path.split('/').next());
        if !matches!(
            method.to_ascii_uppercase().as_str(),
            "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
        ) || matches!(domain, None | Some("" | "auth" | "authen"))
            || !path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-~".contains(&b))
            || path.split('/').any(|part| matches!(part, "." | ".."))
            || path.contains("//")
            || invocation.argv.iter().any(|arg| {
                arg.starts_with("-H")
                    || matches!(
                        arg.split('=').next().unwrap_or_default(),
                        "--header" | "--headers"
                    )
            })
        {
            return Err(GatewayError::Invalid(
                "raw API requires an /open-apis/ business path; token endpoints are not exposed",
            ));
        }
    }
    let mut names = BTreeSet::new();
    for file in &invocation.files {
        if !filename(&file.name) || !names.insert(&file.name) {
            return Err(invalid);
        }
    }
    names.clear();
    for name in &invocation.outputs {
        if !filename(name) || !names.insert(name) {
            return Err(invalid);
        }
    }
    Ok(())
}

fn command(
    binary: &Path,
    network_helper: &Path,
    workspace: &Path,
    app_id: &str,
    token: &str,
) -> Command {
    let mut cmd = Command::new(network_helper);
    cmd.env_clear()
        .env("HOME", "/home")
        .env("TMPDIR", "/tmp")
        .env("LARKSUITE_CLI_APP_ID", app_id)
        .env("LARKSUITE_CLI_TENANT_ACCESS_TOKEN", token)
        .env("LARKSUITE_CLI_BRAND", "feishu")
        .env("LARKSUITE_CLI_DEFAULT_AS", "bot")
        .env("LARKSUITE_CLI_STRICT_MODE", "bot")
        .env("LARKSUITE_CLI_NO_UPDATE_NOTIFIER", "1")
        .env("LARKSUITE_CLI_NO_SKILLS_NOTIFIER", "1")
        .args([
            "--bwrap",
            "/usr/bin/bwrap",
            "--slirp",
            "/usr/bin/slirp4netns",
            "--",
        ])
        .args(["--unshare-all", "--die-with-parent", "--new-session"])
        .args(["--ro-bind"])
        .arg(binary)
        .arg("/bin/lark-cli")
        .args(["--bind"])
        .arg(workspace)
        .arg("/workspace")
        .args([
            "--tmpfs",
            "/tmp",
            "--dir",
            "/home",
            "--dev",
            "/dev",
            "--chdir",
            "/workspace",
        ]);
    // No host root, personal HOME, shell or /proc: @file cannot read credentials
    // or another process's environment. The shared network helper supplies
    // isolated egress and DNS without forwarding host loopback.
    // RHEL's certs directory and bundle are symlinks into /etc/pki. Mount
    // only the resolved public bundle, not that tree (which has private keys).
    if let Some(bundle) = [
        "/etc/ssl/certs/ca-certificates.crt",
        "/etc/pki/tls/certs/ca-bundle.crt",
    ]
    .iter()
    .find_map(|path| {
        std::fs::canonicalize(path)
            .ok()
            .filter(|path| path.is_file())
    }) {
        let target = "/etc/ssl/cert.pem";
        cmd.args(["--ro-bind"])
            .arg(bundle)
            .arg(target)
            .env("SSL_CERT_FILE", target);
    }
    cmd.args(["--", "/bin/lark-cli"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

async fn bounded(
    mut stream: impl AsyncRead + Unpin,
    limit: usize,
) -> Result<Vec<u8>, GatewayError> {
    let mut bytes = Vec::new();
    (&mut stream)
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| GatewayError::Unconfirmed)?;
    if bytes.len() > limit {
        return Err(GatewayError::Invalid(
            "CLI output exceeded its limit; outcome may be unknown, inspect state before retrying and narrow output with --jq",
        ));
    }
    Ok(bytes)
}

async fn run(
    mut cmd: Command,
    input: &str,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(Vec<u8>, Vec<u8>, std::process::ExitStatus), GatewayError> {
    let mut child = cmd.spawn().map_err(|_| {
        GatewayError::Invalid(
            "CLI sandbox failed to start; inspect the retained invocation before retrying",
        )
    })?;
    let operation = async {
        let mut stdin = child.stdin.take().ok_or(GatewayError::Unconfirmed)?;
        let stdout = child.stdout.take().ok_or(GatewayError::Unconfirmed)?;
        let stderr = child.stderr.take().ok_or(GatewayError::Unconfirmed)?;
        let input = async {
            match stdin.write_all(input.as_bytes()).await {
                Ok(()) => {}
                // Help, validation errors and commands without stdin can exit
                // early. Their actual status/output, not EPIPE, is the result.
                Err(error) if error.kind() == ErrorKind::BrokenPipe => {}
                Err(_) => return Err(GatewayError::Unconfirmed),
            }
            drop(stdin);
            Ok(())
        };
        let wait = async { child.wait().await.map_err(|_| GatewayError::Unconfirmed) };
        let (_, stdout, stderr, status) = tokio::try_join!(
            input,
            bounded(stdout, TEXT_LIMIT),
            bounded(stderr, 8192),
            wait
        )?;
        if status.code().is_none() {
            return Err(GatewayError::Unconfirmed);
        }
        Ok((stdout, stderr, status))
    };
    let result = while_running(shutdown, async {
        tokio::time::timeout(TIME_LIMIT, operation)
            .await
            .map_err(|_| GatewayError::Unconfirmed)?
    })
    .await;
    if result.is_err() {
        // Drop alone only schedules a kill/reap. Join it before removing the
        // workspace or letting Gateway release its exclusive state lease.
        let _ = child.kill().await;
    }
    result
}

pub(crate) async fn execute(
    config: &GatewayConfig,
    api: &Api,
    ledger: &Arc<Ledger>,
    request_id: &str,
    mut invocation: Invocation,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Value, GatewayError> {
    if !config.cli_enabled {
        return Err(GatewayError::Invalid(
            "Feishu application tools are not authorized for this bot; enable cli_enabled in its channel settings",
        ));
    }
    if !valid_id(request_id) {
        return Err(GatewayError::Invalid("invalid CLI request identity"));
    }
    validate(&invocation)?;
    if invocation.argv == ["capabilities"] {
        invocation.argv = vec![
            "api".into(),
            "GET".into(),
            format!(
                "/open-apis/application/v6/applications/{}/app_versions",
                config.app_id
            ),
            "--params".into(),
            r#"{"lang":"zh_cn","page_size":2}"#.into(),
        ];
    }
    let content_digest =
        digest(&serde_json::to_vec(&invocation).map_err(|_| GatewayError::Ledger)?);
    let previous_id = request_id.to_owned();
    let previous_digest = content_digest.clone();
    let replay = || {
        json!({"accepted":true,"request_id":request_id,"replayed":true,
        "outcome":"previous_process_completed","guidance":"The original output belongs to the tool history. Do not execute the write again; inspect the resource if its business outcome is unclear."})
    };
    if ledger
        .blocking(move |ledger| ledger.previous("cli", &previous_id, &previous_digest))
        .await?
    {
        return Ok(replay());
    }
    let binary = config
        .cli_binary
        .as_ref()
        .filter(|path| path.is_absolute() && path.is_file())
        .ok_or(GatewayError::Invalid(
            "pinned lark-cli binary is not installed beside the Feishu gateway",
        ))?;
    let network_helper = config
        .cli_network_helper
        .as_ref()
        .filter(|path| path.is_absolute() && path.is_file())
        .ok_or(GatewayError::Invalid(
            "CLI isolated network helper is not installed; host networking is not permitted",
        ))?;
    let workspace = tempfile::Builder::new()
        .prefix("cli-")
        .tempdir_in(&config.state_dir)
        .map_err(|_| GatewayError::PrivateState)?;
    let mut total = 0;
    for file in &invocation.files {
        if file.data_base64.len() > FILE_LIMIT.div_ceil(3) * 4 {
            return Err(GatewayError::Invalid("CLI input files exceed 20 MiB"));
        }
        let bytes = STANDARD
            .decode(&file.data_base64)
            .map_err(|_| GatewayError::Invalid("invalid CLI input file"))?;
        total += bytes.len();
        if total > FILE_LIMIT {
            return Err(GatewayError::Invalid("CLI input files exceed 20 MiB"));
        }
        std::fs::write(workspace.path().join(&file.name), bytes)
            .map_err(|_| GatewayError::PrivateState)?;
    }
    let token = while_running(shutdown, api.token()).await?;
    let mut cmd = command(
        binary,
        network_helper,
        workspace.path(),
        &config.app_id,
        &token,
    );
    cmd.args(&invocation.argv);
    let wire_id = format!("feishu-cli-{}", digest(request_id.as_bytes()));
    let claim_id = request_id.to_owned();
    let claim_wire = wire_id.clone();
    if !ledger
        .blocking(move |ledger| ledger.claim("cli", &claim_id, &content_digest, &claim_wire))
        .await?
    {
        return Ok(replay());
    }
    // A shutdown that arrived during the durable claim must not start a CLI.
    if *shutdown.borrow() {
        return Err(GatewayError::Stopped);
    }
    let (stdout, stderr, status) = run(cmd, &invocation.stdin, shutdown).await?;
    let redact = |bytes: &[u8]| String::from_utf8_lossy(bytes).replace(&token, "[REDACTED]");
    let mut outputs = Vec::new();
    total = 0;
    for name in &invocation.outputs {
        let path = workspace.path().join(name);
        let bytes = match anchor_file_lease::read_regular(&path, FILE_LIMIT.saturating_sub(total)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => continue,
            Err(_) => {
                return Err(GatewayError::Invalid(
                    "CLI output must be a regular file; total output is limited to 20 MiB; inspect state before retrying",
                ));
            }
        };
        // Never alter binary output silently, or allow an exported diagnostic
        // file to bypass the redaction applied to stdout/stderr.
        if bytes
            .windows(token.len())
            .any(|part| part == token.as_bytes())
        {
            return Err(GatewayError::Invalid(
                "CLI output contained a credential; inspect state before retrying",
            ));
        }
        total += bytes.len();
        outputs.push(json!({"name":name,"data_base64":STANDARD.encode(bytes)}));
    }
    // This receipt confirms process completion, not business success. In
    // particular a nonzero CLI exit or partial shortcut result is not success.
    ledger
        .blocking(move |ledger| ledger.confirm(&wire_id))
        .await?;
    Ok(
        json!({"accepted":true,"request_id":request_id,"identity":"bot","app_id":config.app_id,
        "exit_code":status.code(),"stdout":redact(&stdout),"stderr":redact(&stderr),"outputs":outputs,
        "guidance":"Check the CLI result, not accepted, for business success. Permissions, resource access and execution are separate. Never replay an uncertain write automatically."}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_discovery_and_staged_input_remain_generic() {
        let invocation = |args: &[&str]| Invocation {
            argv: args.iter().map(|s| (*s).into()).collect(),
            stdin: String::new(),
            files: vec![],
            outputs: vec![],
        };
        for args in [
            vec!["--help"],
            vec!["skills", "read", "lark-doc"],
            vec!["schema", "im.chats.get"],
            vec!["api", "--help"],
            vec!["api", "GET", "/open-apis/im/v1/chats"],
            vec!["docs", "+create", "--data", "@input.json"],
        ] {
            validate(&invocation(&args)).unwrap();
        }
    }

    #[test]
    fn staged_names_cannot_reach_host_or_sandbox_credentials() {
        for name in ["../input", "/proc/self/environ", "a/b", "..", "", "a\\b"] {
            assert!(!filename(name));
        }
        assert!(filename("document.md"));
        let cmd = command(
            Path::new("/trusted/lark-cli"),
            Path::new("/trusted/anchor-sandbox-network"),
            Path::new("/private/work"),
            "app",
            "token",
        );
        let std = cmd.as_std();
        let args = std
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(
            !args
                .iter()
                .any(|s| s.contains("token") || *s == "/proc" || *s == "/")
        );
        assert!(!args.iter().any(|s| *s == "--share-net"));
        let env = std
            .get_envs()
            .map(|(key, _)| key.to_string_lossy())
            .collect::<Vec<_>>();
        assert!(
            !env.iter()
                .any(|s| s.contains("SECRET") || s.contains("USER_ACCESS_TOKEN"))
        );
    }
}
