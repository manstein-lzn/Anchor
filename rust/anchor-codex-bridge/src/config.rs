use std::{
    fs,
    net::SocketAddr,
    path::{Component, Path, PathBuf},
};

pub const TOKEN_ENV: &str = "ANCHOR_CODEX_BRIDGE_TOKEN";
const MIN_TOKEN_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Invalid(String),
}

#[derive(Clone)]
pub struct BridgeConfig {
    pub listen: SocketAddr,
    pub bearer_token: String,
    pub allow_roots: Vec<PathBuf>,
    pub worktree_root: PathBuf,
    pub codex_bin: PathBuf,
    pub codex_args: Vec<String>,
}

impl std::fmt::Debug for BridgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeConfig")
            .field("listen", &self.listen)
            .field("bearer_token", &"[REDACTED]")
            .field("allow_roots", &self.allow_roots)
            .field("worktree_root", &self.worktree_root)
            .field("codex_bin", &self.codex_bin)
            .finish()
    }
}

impl BridgeConfig {
    pub fn validate(mut self) -> Result<Self, ConfigError> {
        if self.bearer_token.trim().len() < MIN_TOKEN_LEN
            || self.bearer_token.chars().any(char::is_control)
        {
            return Err(ConfigError::Invalid(format!(
                "{TOKEN_ENV} must contain at least {MIN_TOKEN_LEN} bytes and no control characters"
            )));
        }
        if self.allow_roots.is_empty() {
            return Err(ConfigError::Invalid(
                "at least one --allow-root directory is required".into(),
            ));
        }
        let mut canonical_roots = Vec::with_capacity(self.allow_roots.len());
        for root in &self.allow_roots {
            let resolved = canonicalize_allowed_root(root)?;
            if !canonical_roots.contains(&resolved) {
                canonical_roots.push(resolved);
            }
        }
        self.allow_roots = canonical_roots;

        if !self.worktree_root.is_absolute()
            || self
                .worktree_root
                .components()
                .any(|c| matches!(c, Component::ParentDir))
        {
            return Err(ConfigError::Invalid(
                "--worktree-root must be an absolute path without `..`".into(),
            ));
        }
        // Check existing ancestors before creating anything through a link.
        crate::git::check_path_components(&self.worktree_root, true)
            .map_err(ConfigError::Invalid)?;
        fs::create_dir_all(&self.worktree_root).map_err(|error| {
            ConfigError::Invalid(format!(
                "worktree root `{}` cannot be created: {error}",
                self.worktree_root.display()
            ))
        })?;
        self.worktree_root = crate::git::canonical_managed_directory(&self.worktree_root)
            .map_err(ConfigError::Invalid)?;
        if is_forbidden_broad_root(&self.worktree_root) {
            return Err(ConfigError::Invalid(format!(
                "worktree root `{}` overlaps a reserved system path",
                self.worktree_root.display()
            )));
        }
        Ok(self)
    }

    /// Resolve the effective allowed roots for one call, optionally narrowed by
    /// `X-Codex-Project-Scope`. Scope may never widen access outside `allow_roots`.
    pub fn effective_roots(&self, scope_header: Option<&str>) -> Result<Vec<PathBuf>, String> {
        let Some(scope) = scope_header
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            if self.allow_roots.is_empty()
                || self
                    .allow_roots
                    .iter()
                    .any(|root| !self.is_within_allow_roots(root))
            {
                return Err("configured project roots are unavailable".into());
            }
            return Ok(self.allow_roots.clone());
        };
        let scope_path = Path::new(scope);
        if scope_path
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err("project scope must not contain `..`".into());
        }
        if scope_path.is_absolute() {
            let resolved = scope_path
                .canonicalize()
                .map_err(|e| format!("project scope `{scope}` is unavailable: {e}"))?;
            if !resolved.is_dir() {
                return Err(format!("project scope `{scope}` is not a directory"));
            }
            if self.is_within_allow_roots(scope_path) {
                return Ok(vec![resolved]);
            }
            return Err(format!(
                "project scope `{scope}` is outside the bridge's authorized --allow-root directories"
            ));
        }
        let mut matched = Vec::new();
        for root in &self.allow_roots {
            let candidate = root.join(scope_path);
            if let Ok(resolved) = candidate.canonicalize()
                && resolved.is_dir()
                && resolved.starts_with(root)
                && self.is_within_allow_roots(&candidate)
                && !matched.contains(&resolved)
            {
                matched.push(resolved);
            }
        }
        if matched.is_empty() {
            return Err(format!(
                "project scope `{scope}` did not match an existing directory under authorized --allow-root"
            ));
        }
        Ok(matched)
    }

    /// Resolve a `project` argument (absolute or relative to an effective root)
    /// Managed paths also require a validated main repository in this scope.
    pub fn resolve_project(
        &self,
        project: &str,
        effective_roots: &[PathBuf],
    ) -> Result<PathBuf, String> {
        let trimmed = project.trim();
        if trimmed.is_empty() {
            if effective_roots.len() == 1 {
                return self.resolve_project(".", effective_roots);
            }
            return Err("project path is required when multiple roots are in scope".into());
        }
        let requested = Path::new(trimmed);
        if requested
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err("project path must not contain `..`".into());
        }
        if requested.is_absolute() {
            let resolved = requested
                .canonicalize()
                .map_err(|e| format!("project `{trimmed}` cannot be resolved: {e}"))?;
            if !resolved.is_dir() {
                return Err(format!("project `{trimmed}` is not a directory"));
            }
            if self.is_authorized_cwd(requested, effective_roots) {
                return Ok(resolved);
            }
            return Err(format!(
                "project `{trimmed}` is outside the authorized project scope"
            ));
        }
        let mut matches = Vec::new();
        for root in effective_roots {
            let candidate = root.join(requested);
            if let Ok(resolved) = candidate.canonicalize()
                && resolved.is_dir()
                && self.is_authorized_cwd(&candidate, effective_roots)
                && !matches.contains(&resolved)
            {
                matches.push(resolved);
            }
        }
        match matches.len() {
            0 => Err(format!(
                "project `{trimmed}` was not found under authorized roots"
            )),
            1 => Ok(matches.remove(0)),
            _ => Err(format!(
                "project `{trimmed}` is ambiguous across multiple authorized roots; pass a fuller path"
            )),
        }
    }

    pub fn is_within_allow_roots(&self, path: &Path) -> bool {
        let Ok(resolved) = crate::git::canonical_directory(path) else {
            return false;
        };
        !path.starts_with(&self.worktree_root)
            && !resolved.starts_with(&self.worktree_root)
            && self.allow_roots.iter().any(|root| {
                crate::git::canonical_directory(root).is_ok_and(|current| current == *root)
                    && resolved.starts_with(root)
            })
    }

    /// Routing scope only: this does not restrict a Codex sandbox's read authority.
    pub fn is_authorized_cwd(&self, cwd: &Path, effective_roots: &[PathBuf]) -> bool {
        let Ok(resolved) = crate::git::canonical_directory(cwd) else {
            return false;
        };
        // Managed paths take precedence even when nested inside an allowed root.
        let subject =
            if cwd.starts_with(&self.worktree_root) || resolved.starts_with(&self.worktree_root) {
                match crate::git::managed_worktree_metadata(&self.worktree_root, cwd) {
                    Ok(Some((repo, _, _))) if !repo.starts_with(&self.worktree_root) => repo,
                    _ => return false,
                }
            } else {
                resolved
            };
        effective_roots.iter().any(|root| {
            self.is_within_allow_roots(root)
                && crate::git::canonical_directory(root).is_ok_and(|current| current == *root)
                && subject.starts_with(root)
        })
    }
}

fn canonicalize_allowed_root(path: &Path) -> Result<PathBuf, ConfigError> {
    let resolved = crate::git::canonical_directory(path).map_err(ConfigError::Invalid)?;
    if is_forbidden_broad_root(&resolved) {
        return Err(ConfigError::Invalid(format!(
            "--allow-root `{}` is a reserved system directory; grant a specific user or workspace directory instead",
            resolved.display()
        )));
    }
    Ok(resolved)
}

fn is_forbidden_broad_root(path: &Path) -> bool {
    [
        "/", "/bin", "/boot", "/dev", "/etc", "/lib", "/lib64", "/proc", "/root", "/run", "/sbin",
        "/sys", "/usr", "/var",
    ]
    .iter()
    .any(|reserved| path == Path::new(reserved))
}

pub fn parse_cli_args(args: &[String]) -> Result<Option<BridgeConfig>, ConfigError> {
    if args.is_empty() || args.iter().any(|arg| arg == "-h" || arg == "--help") {
        return Ok(None);
    }
    if args[0] != "serve" {
        return Err(ConfigError::Invalid(format!(
            "unknown subcommand `{}`; expected `serve`",
            args[0]
        )));
    }
    let mut listen: SocketAddr = "127.0.0.1:8091".parse().expect("valid default socket");
    let mut allow_roots = Vec::new();
    let mut worktree_root: Option<PathBuf> = None;
    let mut codex_bin = PathBuf::from("codex");
    let mut index = 1;
    while index < args.len() {
        let flag = &args[index];
        let value = args
            .get(index + 1)
            .ok_or_else(|| ConfigError::Invalid(format!("missing value for `{flag}`")))?;
        match flag.as_str() {
            "--listen" => {
                listen = value.parse().map_err(|error| {
                    ConfigError::Invalid(format!("invalid --listen `{value}`: {error}"))
                })?;
            }
            "--allow-root" => {
                allow_roots.push(PathBuf::from(value));
            }
            "--worktree-root" => {
                worktree_root = Some(PathBuf::from(value));
            }
            "--codex-bin" => {
                codex_bin = PathBuf::from(value);
            }
            other => {
                return Err(ConfigError::Invalid(format!("unknown flag `{other}`")));
            }
        }
        index += 2;
    }
    let bearer_token = std::env::var(TOKEN_ENV).map_err(|_| {
        ConfigError::Invalid(format!(
            "environment variable {TOKEN_ENV} is required (at least {MIN_TOKEN_LEN} bytes)"
        ))
    })?;
    let default_wt = allow_roots
        .first()
        .map(|first| first.join(".anchor-codex-worktrees"))
        .unwrap_or_else(|| PathBuf::from("/tmp/anchor-codex-worktrees"));
    BridgeConfig {
        listen,
        bearer_token,
        allow_roots,
        worktree_root: worktree_root.unwrap_or(default_wt),
        codex_bin,
        codex_args: vec!["app-server".into(), "--stdio".into()],
    }
    .validate()
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(root: &Path) -> BridgeConfig {
        BridgeConfig {
            listen: "127.0.0.1:8091".parse().unwrap(),
            bearer_token: "t".repeat(32),
            allow_roots: vec![root.to_owned()],
            worktree_root: root.join("worktrees"),
            codex_bin: "codex".into(),
            codex_args: vec![],
        }
    }

    #[test]
    fn token_minimum_is_bytes_and_debug_redacts_it() {
        let temp = tempfile::tempdir().unwrap();
        for token in [
            "t".repeat(31),
            format!("{}\n", "t".repeat(32)),
            " ".repeat(32),
        ] {
            let mut candidate = config(temp.path());
            candidate.bearer_token = token;
            assert!(candidate.validate().is_err());
        }
        let valid = config(temp.path()).validate().unwrap();
        assert!(!format!("{valid:?}").contains(&valid.bearer_token));
        let mut multibyte = config(temp.path());
        multibyte.bearer_token = "\u{e9}".repeat(16);
        assert!(multibyte.validate().is_ok());
    }

    #[tokio::test]
    async fn cli_defaults_to_loopback_and_requires_32_token_bytes() {
        const CHILD_ROOT: &str = "ANCHOR_BRIDGE_CONFIG_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let result = parse_cli_args(&[
                "serve".into(),
                "--allow-root".into(),
                root.to_str().unwrap().into(),
            ]);
            if std::env::var(TOKEN_ENV).unwrap().len() < 32 {
                assert!(result.is_err());
            } else {
                let config = result.unwrap().unwrap();
                assert_eq!(config.listen, "127.0.0.1:8091".parse().unwrap());
                assert_eq!(config.worktree_root, root.join(".anchor-codex-worktrees"));
            }
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        for length in [31, 32] {
            let output = tokio::time::timeout(
                std::time::Duration::from_secs(15),
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "config::tests::cli_defaults_to_loopback_and_requires_32_token_bytes",
                        "--nocapture",
                    ])
                    .env(CHILD_ROOT, temp.path())
                    .env(TOKEN_ENV, "t".repeat(length))
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn unknown_paths_and_untrusted_scopes_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let config = config(temp.path()).validate().unwrap();
        let scope = config.effective_roots(None).unwrap();
        let unknown = temp.path().join("missing");
        let file = temp.path().join("file");
        fs::write(&file, "not a directory").unwrap();
        for path in [&unknown, &file, &config.worktree_root, outside.path()] {
            assert!(!config.is_authorized_cwd(path, &scope));
            assert!(
                config
                    .resolve_project(path.to_str().unwrap(), &scope)
                    .is_err()
            );
        }
        assert!(!config.is_authorized_cwd(temp.path(), &[]));
        assert!(!config.is_authorized_cwd(outside.path(), &[outside.path().to_owned()]));
        assert!(!config.is_authorized_cwd(temp.path(), &[PathBuf::from("/")]));
        assert!(
            config
                .resolve_project("", std::slice::from_ref(&unknown))
                .is_err()
        );
        assert!(
            config
                .resolve_project(".", std::slice::from_ref(&unknown))
                .is_err()
        );
        assert!(config.effective_roots(Some("../escape")).is_err());
        assert!(config.effective_roots(Some("worktrees")).is_err());
        assert!(config.effective_roots(Some("missing")).is_err());
        assert!(config.is_authorized_cwd(temp.path(), &scope));
        assert_eq!(config.resolve_project("", &scope).unwrap(), temp.path());
    }

    #[test]
    fn scoped_roots_cannot_be_missing_or_ambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        fs::create_dir_all(a.join("project")).unwrap();
        fs::create_dir_all(b.join("project")).unwrap();
        let mut config = config(temp.path());
        config.allow_roots = vec![a.clone(), b.clone()];
        let config = config.validate().unwrap();
        let roots = config.effective_roots(None).unwrap();
        assert!(config.resolve_project("project", &roots).is_err());
        assert!(config.resolve_project("", &roots).is_err());
        fs::remove_dir_all(a).unwrap();
        assert!(config.effective_roots(None).is_err());
    }

    #[tokio::test]
    async fn nested_managed_root_never_bypasses_parent_repo_scope() {
        let temp = tempfile::tempdir().unwrap();
        let a = crate::git::tests::init_repo(&temp.path().join("a")).await;
        let b = crate::git::tests::init_repo(&temp.path().join("b")).await;
        let mut config = config(temp.path());
        config.worktree_root = a.join("managed");
        let config = config.validate().unwrap();
        let a_scope = config.effective_roots(Some("a")).unwrap();
        let b_scope = config.effective_roots(Some("b")).unwrap();
        let wt = crate::git::ensure_worktree(&config.worktree_root, &b, "task", None)
            .await
            .unwrap();
        let child = wt.path.join("src");
        fs::create_dir(&child).unwrap();
        for path in [&wt.path, &child] {
            assert!(!config.is_authorized_cwd(path, &a_scope));
            assert!(
                config
                    .resolve_project(path.to_str().unwrap(), &a_scope)
                    .is_err()
            );
            assert!(config.is_authorized_cwd(path, &b_scope));
            assert_eq!(
                config
                    .resolve_project(path.to_str().unwrap(), &b_scope)
                    .unwrap(),
                *path
            );
        }
        assert!(config.is_authorized_cwd(&wt.path, &config.allow_roots));
        let unknown = config.worktree_root.join("unknown");
        fs::create_dir(&unknown).unwrap();
        assert!(!config.is_authorized_cwd(&unknown, &config.allow_roots));
        fs::write(wt.path.join(".git"), "gitdir: /unavailable\n").unwrap();
        assert!(!config.is_authorized_cwd(&wt.path, &b_scope));
        assert!(!config.is_authorized_cwd(&wt.path, &config.allow_roots));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_scope_or_form_managed_roots() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let mut config = config(temp.path());
        symlink(outside.path(), &config.worktree_root).unwrap();
        assert!(config.clone().validate().is_err());
        config.worktree_root = temp.path().join("safe");
        let config = config.validate().unwrap();
        let link = temp.path().join("escape");
        symlink(outside.path(), &link).unwrap();
        assert!(!config.is_authorized_cwd(&link, &config.allow_roots));
        assert!(config.effective_roots(Some("escape")).is_err());
        let alias = config.worktree_root.join("alias");
        symlink(temp.path(), &alias).unwrap();
        assert!(
            config
                .effective_roots(Some(alias.to_str().unwrap()))
                .is_err()
        );
        assert!(!config.is_authorized_cwd(&alias, &config.allow_roots));
    }
}
