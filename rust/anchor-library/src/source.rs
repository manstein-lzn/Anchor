use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use std::os::unix::process::CommandExt;

use crate::InstallError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubSource {
    owner: String,
    repository: String,
    revision: String,
    relative: PathBuf,
}

impl GithubSource {
    pub fn parse(source: &str) -> Result<Self, InstallError> {
        let parsed = url::Url::parse(source).map_err(|_| InstallError::InvalidSource)?;
        let path = source
            .strip_prefix("https://github.com/")
            .ok_or(InstallError::InvalidSource)?;
        if parsed.scheme() != "https"
            || parsed.host_str() != Some("github.com")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.port().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(InstallError::InvalidSource);
        }
        let parts: Vec<_> = path.split('/').filter(|part| !part.is_empty()).collect();
        if parts.len() < 5 || parts[2] != "tree" || parts.iter().any(|part| !valid_segment(part)) {
            return Err(InstallError::InvalidSource);
        }
        Ok(Self {
            owner: parts[0].to_owned(),
            repository: parts[1].to_owned(),
            revision: parts[3].to_owned(),
            relative: parts[4..].iter().collect(),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn relative_path(&self) -> &Path {
        &self.relative
    }

    pub(crate) fn default_id(&self) -> &str {
        self.relative.file_name().unwrap().to_str().unwrap()
    }

    fn repository_url(&self) -> String {
        format!("https://github.com/{}/{}.git", self.owner, self.repository)
    }
}

fn valid_segment(part: &str) -> bool {
    part.as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && part
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

pub trait Checkout: Send + Sync {
    fn checkout(&self, source: &GithubSource, destination: &Path) -> Result<(), InstallError>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GitCheckout;

impl Checkout for GitCheckout {
    fn checkout(&self, source: &GithubSource, destination: &Path) -> Result<(), InstallError> {
        let mut clone = git_command();
        clone
            .arg("clone")
            .args(["--depth=1", "--filter=blob:none", "--sparse", "--branch"])
            .arg(source.revision())
            .arg("--")
            .arg(source.repository_url())
            .arg(destination);
        run(&mut clone, Duration::from_secs(180))?;
        let mut sparse = git_command();
        sparse
            .arg("-C")
            .arg(destination)
            .args(["sparse-checkout", "set", "--"])
            .arg(source.relative_path());
        run(&mut sparse, Duration::from_secs(60))
    }
}

fn git_command() -> Command {
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "")
        .args([
            "-c",
            "credential.helper=",
            "-c",
            "core.askPass=",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "http.followRedirects=false",
            "-c",
            "http.sslVerify=true",
            "-c",
            "http.proxy=",
        ]);
    command
}

fn run(command: &mut Command, timeout: Duration) -> Result<(), InstallError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().map_err(|_| InstallError::CheckoutFailed)?;
    let deadline = Instant::now() + timeout;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err(InstallError::CheckoutFailed)
                };
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => break InstallError::CheckoutTimedOut,
            Err(_) => break InstallError::CheckoutFailed,
        }
    };
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
    Err(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_python_compatible_tree_paths_and_preserves_revision() {
        let source = GithubSource::parse(
            "https://github.com//owner/repo/tree/release-1.2/plugins/my-plugin//",
        )
        .unwrap();
        assert_eq!(source.owner(), "owner");
        assert_eq!(source.repository(), "repo");
        assert_eq!(source.revision(), "release-1.2");
        assert_eq!(source.relative_path(), Path::new("plugins/my-plugin"));
        assert_eq!(source.default_id(), "my-plugin");
        assert_eq!(source.repository_url(), "https://github.com/owner/repo.git");
    }

    #[test]
    fn rejects_hosts_credentials_queries_normalization_and_option_injection() {
        for source in [
            "http://github.com/owner/repo/tree/main/plugin",
            "https://evil.example/owner/repo/tree/main/plugin",
            "https://github.com.evil.example/owner/repo/tree/main/plugin",
            "https://user:sentinel-secret@github.com/owner/repo/tree/main/plugin",
            "https://github.com:443/owner/repo/tree/main/plugin",
            "https://github.com/owner/repo/tree/main/plugin?token=sentinel-secret",
            "https://github.com/owner/repo/tree/main/plugin#sentinel-secret",
            "https://github.com/owner/repo/tree/main",
            "https://github.com/owner/repo/blob/main/plugin",
            "https://github.com/owner/repo/tree/--config/plugin",
            "https://github.com/owner/repo/tree/main/../plugin",
            "https://github.com/owner/repo/tree/main/./plugin",
            "https://github.com/owner/repo/tree/main/%2e%2e/plugin",
            "https://github.com/owner/repo/tree/main/plugin%2fescape",
            "https://github.com/owner/repo/tree/main/plugin;command",
            "https://github.com/owner/repo/tree/main/plugin\\escape",
            "https://github.com/owner/repo/tree/main/plugin\n",
            "https://github.com/owner/repo/tree/main/plugin name",
            "https://github.com/owner/repo/tree/main/--plugin",
            "https://github.com/owner/repo/tree/main/plugin?",
            "https://github.com/owner/repo/tree/main/plugin#",
            "https://github.com/owner/repo/tree/main/plugin\0",
            "https://GITHUB.com/owner/repo/tree/main/plugin",
            "/operator/plugin",
            "file:///operator/plugin",
            "git@github.com:owner/repo.git",
            "",
        ] {
            let error = GithubSource::parse(source).unwrap_err();
            assert!(matches!(error, InstallError::InvalidSource), "{source:?}");
            assert!(!error.to_string().contains("sentinel-secret"));
        }
    }

    #[test]
    fn git_ignores_host_configuration_credentials_and_redirects() {
        let command = git_command();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(args.contains(&"credential.helper="));
        assert!(args.contains(&"protocol.allow=never"));
        assert!(args.contains(&"protocol.https.allow=always"));
        assert!(args.contains(&"http.followRedirects=false"));
        assert!(args.contains(&"http.sslVerify=true"));
        assert!(args.contains(&"core.hooksPath=/dev/null"));
        let environment: Vec<_> = command
            .get_envs()
            .map(|(key, value)| (key.to_str().unwrap(), value.unwrap().to_str().unwrap()))
            .collect();
        assert!(environment.contains(&("GIT_TERMINAL_PROMPT", "0")));
        assert!(environment.contains(&("GIT_CONFIG_GLOBAL", "/dev/null")));
        assert!(!environment.iter().any(|(key, _)| key.contains("TOKEN")));
    }

    #[test]
    fn subprocess_failure_is_sanitized_and_timeout_reaps_child() {
        let error = run(&mut Command::new("/bin/false"), Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.to_string(), "Git checkout failed");
        let started = Instant::now();
        let error = run(
            Command::new("/bin/sleep").arg("30"),
            Duration::from_millis(40),
        )
        .unwrap_err();
        assert!(matches!(error, InstallError::CheckoutTimedOut));
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
