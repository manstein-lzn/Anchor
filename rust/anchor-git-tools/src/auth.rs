use base64::{Engine, engine::general_purpose::STANDARD};
use std::{collections::HashMap, io::Write, path::PathBuf};
use tokio::process::Command;
use url::Url;

use crate::Error;

/// Parsed once by the executable entrypoint; no process environment reads below it.
#[derive(Default)]
pub struct Config {
    pub author_name: Option<String>,
    pub author_email: Option<String>,
    pub ssh_private_key: Option<String>,
    pub ssh_known_hosts: Option<String>,
    pub ssh_config: Option<PathBuf>,
    pub https_url: Option<String>,
    pub https_username: Option<String>,
    pub https_token: Option<String>,
}

pub(crate) struct Authentication {
    config: Config,
    executable: PathBuf,
    ssh: Option<(tempfile::TempDir, String)>,
}

impl Authentication {
    pub(crate) fn new(mut config: Config, executable: PathBuf) -> Result<Self, Error> {
        if let Some(key) = &mut config.ssh_private_key
            && !key.trim_start().starts_with("-----BEGIN ")
        {
            *key = String::from_utf8(STANDARD.decode(key.trim()).map_err(
                |_| "ssh_private_key must be a private key or its single-line Base64 encoding",
            )?)
            .map_err(|_| "ssh_private_key must decode to UTF-8 private key content")?;
        }
        let ssh = match (&config.ssh_private_key, &config.ssh_known_hosts) {
            (Some(key), Some(hosts)) => {
                let directory = tempfile::Builder::new().prefix("anchor-git-").tempdir()?;
                let key_path = directory.path().join("identity");
                let hosts_path = directory.path().join("known_hosts");
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&key_path)?;
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                writeln!(file, "{}", key.trim_end())?;
                std::fs::write(&hosts_path, hosts)?;
                let command = shlex::try_join([
                    "ssh",
                    "-F",
                    "/dev/null",
                    "-i",
                    key_path.to_str().ok_or("invalid SSH path")?,
                    "-o",
                    "BatchMode=yes",
                    "-o",
                    "IdentitiesOnly=yes",
                    "-o",
                    "StrictHostKeyChecking=yes",
                    "-o",
                    "GlobalKnownHostsFile=/dev/null",
                    "-o",
                    &format!("UserKnownHostsFile={}", hosts_path.display()),
                ])?;
                Some((directory, command))
            }
            (None, None) => None,
            _ => return Err("SSH requires both ssh_private_key and ssh_known_hosts".into()),
        };
        if let Some(path) = &config.ssh_config {
            if ssh.is_some() {
                return Err("choose either host SSH configuration or Plugin private key".into());
            }
            if !path.is_absolute() || !path.is_file() {
                return Err(
                    "ssh_config must name an operator-granted absolute SSH config file".into(),
                );
            }
        }
        match (
            &config.https_url,
            &config.https_username,
            &config.https_token,
        ) {
            (Some(url), Some(_), Some(_)) => {
                credential_url(url)?;
            }
            (None, None, None) => {}
            _ => return Err("HTTPS requires https_url, https_username and https_token".into()),
        }
        Ok(Self {
            config,
            executable,
            ssh,
        })
    }

    pub(crate) fn apply(&self, command: &mut Command) -> Result<(), Error> {
        if let Some(name) = &self.config.author_name {
            command
                .env("GIT_AUTHOR_NAME", name)
                .env("GIT_COMMITTER_NAME", name);
        }
        if let Some(email) = &self.config.author_email {
            command
                .env("GIT_AUTHOR_EMAIL", email)
                .env("GIT_COMMITTER_EMAIL", email);
        }
        if let Some((_, ssh)) = &self.ssh {
            command
                .env("GIT_SSH_COMMAND", ssh)
                .env("GIT_SSH_VARIANT", "ssh");
        }
        if let Some(path) = &self.config.ssh_config {
            let ssh = shlex::try_join([
                "ssh",
                "-F",
                path.to_str().ok_or("invalid SSH config path")?,
                "-o",
                "BatchMode=yes",
                "-o",
                "StrictHostKeyChecking=yes",
            ])?;
            command
                .env("GIT_SSH_COMMAND", ssh)
                .env("GIT_SSH_VARIANT", "ssh");
        }
        if let (Some(url), Some(username), Some(token)) = (
            &self.config.https_url,
            &self.config.https_username,
            &self.config.https_token,
        ) {
            let helper = format!(
                "!{} --credential",
                shlex::try_quote(self.executable.to_str().ok_or("invalid executable path")?)?
            );
            command
                .env("ANCHOR_GIT_HTTPS_URL", url)
                .env("ANCHOR_GIT_HTTPS_USERNAME", username)
                .env("ANCHOR_GIT_HTTPS_TOKEN", token)
                .args([
                    "-c",
                    "credential.helper=",
                    "-c",
                    &format!("credential.helper={helper}"),
                    "-c",
                    "credential.useHttpPath=true",
                ]);
        }
        Ok(())
    }

    pub(crate) fn redact(&self, text: String) -> String {
        let mut text = text;
        if let Some(key) = &self.config.ssh_private_key {
            text = text.replace(&STANDARD.encode(key), "[REDACTED]");
        }
        for value in [&self.config.https_token, &self.config.ssh_private_key]
            .into_iter()
            .flatten()
        {
            // Git errors/hooks can print individual lines as well as whole values.
            for secret in std::iter::once(value.as_str())
                .chain(value.lines())
                .filter(|s| !s.is_empty())
            {
                text = text.replace(secret, "[REDACTED]");
            }
        }
        text
    }
}

fn credential_url(value: &str) -> Result<Url, Error> {
    let url = Url::parse(value)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("https_url must be an HTTPS URL without userinfo, query or fragment".into());
    }
    Ok(url)
}

/// Git credential protocol: only `get` for the configured HTTPS destination.
/// No credential store is created in the repository or task workspace.
pub fn credential_response(config: &Config, operation: &str, input: &str) -> Result<String, Error> {
    if operation != "get" {
        return Ok(String::new());
    }
    let (Some(url), Some(username), Some(token)) = (
        &config.https_url,
        &config.https_username,
        &config.https_token,
    ) else {
        return Ok(String::new());
    };
    if username.contains(['\r', '\n']) || token.contains(['\r', '\n']) {
        return Err("HTTPS credential fields must be single-line values".into());
    }
    let allowed = credential_url(url)?;
    let fields: HashMap<_, _> = input
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let Some(host) = fields.get("host") else {
        return Ok(String::new());
    };
    let path = fields
        .get("path")
        .copied()
        .unwrap_or("")
        .trim_start_matches('/');
    let requested = credential_url(&format!("https://{host}/{path}"))?;
    let path = requested.path().trim_start_matches('/');
    let prefix = allowed.path().trim_matches('/');
    let path_allowed = prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|tail| tail.starts_with('/'));
    if fields.get("protocol") != Some(&"https")
        || requested.origin() != allowed.origin()
        || !path_allowed
        || fields
            .get("username")
            .is_some_and(|value| *value != username)
    {
        return Ok(String::new());
    }
    Ok(format!("username={username}\npassword={token}\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn ssh_accepts_settings_field_encoding_and_keeps_private_key_private() {
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nfixture-key-content\n-----END OPENSSH PRIVATE KEY-----\n";
        for configured in [key.to_owned(), STANDARD.encode(key)] {
            let auth = Authentication::new(
                Config {
                    ssh_private_key: Some(configured.clone()),
                    ssh_known_hosts: Some("git.example ssh-ed25519 fixture-host-key\n".into()),
                    ..Config::default()
                },
                PathBuf::from("/unused"),
            )
            .unwrap();
            let directory = &auth.ssh.as_ref().unwrap().0;
            let path = directory.path().join("identity");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), key);
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                auth.redact(format!("{configured} fixture-key-content")),
                "[REDACTED] [REDACTED]"
            );
        }
    }
}
