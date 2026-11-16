//! What the deployment authorizes for environment preparation.
//!
//! Every value here comes from the deployment's own environment, never from a
//! model argument, a Graph field or an HTTP request. Resolution and fetching
//! reach the network, so the sources are an allowlist the operator wrote down;
//! the `uv` that performs them is pinned by digest, which also keeps the
//! bootstrap from depending on itself.

use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) use crate::host_fs::sha256_file;
use sha2::{Digest, Sha256};

/// The one deployment-owned Python interpreter used by dependency preparation
/// and, when explicitly granted, Pilot authoring.
pub(crate) const PYTHON_INTERPRETER_ENV: &str = "ANCHOR_DEPENDENCIES_PYTHON";

/// Why preparation could not proceed.
///
/// One variant per *reason*, because the boundaries that surface it map it
/// differently: an unconfigured or invalid ask is the caller's problem, a
/// failed step and a moved plan are the operator's.
#[derive(Debug)]
pub(crate) enum ProvisionError {
    /// This deployment does not authorize environment preparation at all.
    Unconfigured(String),
    /// The ask or the plan itself is not acceptable.
    Invalid(String),
    /// A step (resolve, fetch, build, verify) failed.
    Failed(String),
}

impl std::error::Error for ProvisionError {}

impl std::fmt::Display for ProvisionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconfigured(reason) | Self::Invalid(reason) | Self::Failed(reason) => {
                formatter.write_str(reason)
            }
        }
    }
}

/// One deployment-authorized place packages may come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PackageSource {
    /// A directory of already-built distributions: an offline deployment, or a
    /// test. Used as `--find-links`.
    Directory(PathBuf),
    /// A PEP 503/691 simple index reached over HTTP(S). Used as `--index`.
    Index(String),
}

impl PackageSource {
    /// The text a plan records, so a plan says what it resolved against.
    pub(crate) fn as_str(&self) -> String {
        match self {
            Self::Directory(path) => format!("directory:{}", path.display()),
            Self::Index(url) => format!("index:{url}"),
        }
    }
}

/// How long one build step may take. Installing a real dependency set from a
/// cold cache is not fast, and a premature timeout would leave a half-built
/// staging directory behind rather than a wrong environment.
const STEP_TIMEOUT: Duration = Duration::from_secs(600);
/// How long a single index or artifact request may take.
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
/// Output retained from one build step; the interesting part is the error tail.
const MAX_STEP_OUTPUT: usize = 256 * 1024;
/// Largest single artifact this host will download.
const MAX_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;

/// The deployment's authorized preparation.
#[derive(Debug, Clone)]
pub(crate) struct DependencyProvisionConfig {
    /// The pinned `uv` executable.
    pub(crate) uv: PathBuf,
    /// Its expected SHA-256, checked before every use.
    pub(crate) uv_digest: String,
    /// The base interpreter the environment is built on. Its identity is part
    /// of the plan, because a wide "Python 3.12" is not a build identity.
    pub(crate) python: PathBuf,
    /// Authorized package sources, tried in order.
    pub(crate) sources: Vec<PackageSource>,
    /// Writable staging root. Nothing here is ever published content.
    pub(crate) build_root: PathBuf,
    /// The Bubblewrap executable, as the rest of the host resolves it.
    pub(crate) bwrap: PathBuf,
}

impl DependencyProvisionConfig {
    pub(crate) fn step_timeout(&self) -> Duration {
        STEP_TIMEOUT
    }

    pub(crate) fn http_timeout(&self) -> Duration {
        HTTP_TIMEOUT
    }

    pub(crate) fn max_step_output(&self) -> usize {
        MAX_STEP_OUTPUT
    }

    pub(crate) fn max_artifact_bytes(&self) -> u64 {
        MAX_ARTIFACT_BYTES
    }

    /// The `uv` binary, after checking it is still the one the deployment pinned.
    pub(crate) fn verified_uv(&self) -> Result<PathBuf, ProvisionError> {
        let observed = sha256_file(&self.uv)
            .map_err(|error| ProvisionError::Failed(format!("uv cannot be read: {error}")))?;
        if observed != self.uv_digest {
            return Err(ProvisionError::Failed(format!(
                "the configured uv binary is not the pinned one (expected {}, found {observed})",
                self.uv_digest
            )));
        }
        Ok(self.uv.clone())
    }
}

/// Read the deployment's authorized preparation, if it configured any.
///
/// All-or-nothing on purpose: a half-configured deployment would otherwise fail
/// later with a worse message, or worse, silently resolve against a default
/// index the operator never authorized. Setting none of the variables means
/// "this deployment does not prepare environments", which is not an error.
///
/// `library_root` is where published environments live, when this deployment has
/// one. Staging and published content must stay disjoint, so the deployment is
/// refused at startup rather than mid-build.
pub(crate) fn from_env(
    state_root: &Path,
    bwrap: PathBuf,
    library_root: Option<&Path>,
) -> Result<Option<DependencyProvisionConfig>, String> {
    const PREPARATION_VARIABLES: &[&str] = &[
        "ANCHOR_DEPENDENCIES_SOURCES",
        "ANCHOR_DEPENDENCIES_UV",
        "ANCHOR_DEPENDENCIES_UV_SHA256",
        "ANCHOR_DEPENDENCIES_BUILD_ROOT",
    ];
    const VARIABLES: &[&str] = &[
        "ANCHOR_DEPENDENCIES_SOURCES",
        "ANCHOR_DEPENDENCIES_UV",
        "ANCHOR_DEPENDENCIES_UV_SHA256",
        PYTHON_INTERPRETER_ENV,
        "ANCHOR_DEPENDENCIES_BUILD_ROOT",
    ];
    let mut values = std::collections::BTreeMap::new();
    for name in VARIABLES {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
            values.insert(*name, value);
        }
    }
    // The interpreter is a shared runtime setting. It does not by itself grant
    // package resolution, downloads, or builds; those require a preparation
    // setting from the separate deployment capability.
    if !PREPARATION_VARIABLES
        .iter()
        .any(|name| values.contains_key(*name))
    {
        return Ok(None);
    }
    let required = |name: &str| -> Result<String, String> {
        values
            .get(name)
            .cloned()
            .ok_or_else(|| format!("{name} is required once dependency preparation is configured"))
    };

    let sources = parse_sources(&required("ANCHOR_DEPENDENCIES_SOURCES")?)?;
    let uv = PathBuf::from(required("ANCHOR_DEPENDENCIES_UV")?);
    if !uv.is_absolute() {
        return Err("ANCHOR_DEPENDENCIES_UV must be an absolute path".into());
    }
    let uv_digest = required("ANCHOR_DEPENDENCIES_UV_SHA256")?;
    if uv_digest.len() != 64 || !uv_digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("ANCHOR_DEPENDENCIES_UV_SHA256 must be a 64-character hex digest".into());
    }
    let python = PathBuf::from(required(PYTHON_INTERPRETER_ENV)?);
    if !python.is_absolute() {
        return Err(format!("{PYTHON_INTERPRETER_ENV} must be an absolute path"));
    }
    let build_root = values
        .get("ANCHOR_DEPENDENCIES_BUILD_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_root.join("environment-build"));
    if !build_root.is_absolute() {
        return Err("ANCHOR_DEPENDENCIES_BUILD_ROOT must be an absolute path".into());
    }
    // The same overlap rule publication enforces on a candidate directory: a
    // build root inside the Library, or a Library inside the build root, means
    // staging and published content share a tree. A build that is interrupted
    // there would leave half-installed files where published environments are
    // read from, and publishing would copy the Library into itself.
    if let Some(library_root) = library_root {
        let build = std::fs::canonicalize(&build_root).unwrap_or_else(|_| build_root.clone());
        let library =
            std::fs::canonicalize(library_root).unwrap_or_else(|_| library_root.to_path_buf());
        if build.starts_with(&library) || library.starts_with(&build) {
            return Err(format!(
                "ANCHOR_DEPENDENCIES_BUILD_ROOT ({}) and the environment Library root ({}) must not overlap: staging inside published content (or the reverse) would make an interrupted build and a published environment the same files",
                build_root.display(),
                library_root.display()
            ));
        }
        // What actually needs one filesystem is the *publish* rename: staging is
        // moved into the shared store and a view is moved to its final path, and
        // `rename` cannot cross devices. Views and the store themselves may live
        // on different devices — views are symlinks — so this is the only device
        // rule, and it is refused now rather than mid-install.
        if let (Some(build_device), Some(library_device)) =
            (device_of(&build_root), device_of(library_root))
            && build_device != library_device
        {
            return Err(format!(
                "ANCHOR_DEPENDENCIES_BUILD_ROOT ({}) and the environment Library root ({}) are on different filesystems, so a built dependency set could not be moved into the Library atomically",
                build_root.display(),
                library_root.display()
            ));
        }
    }
    Ok(Some(DependencyProvisionConfig {
        uv,
        uv_digest,
        python,
        sources,
        build_root,
        bwrap,
    }))
}

/// The device a path lives on, resolved through its deepest existing ancestor.
///
/// A build root or a Library root may not exist yet when the deployment is
/// checked, but the directory a missing leaf would be created in already names
/// the filesystem it would land on.
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    let mut candidate = path.to_path_buf();
    loop {
        if let Ok(metadata) = std::fs::metadata(&candidate) {
            return Some(metadata.dev());
        }
        if !candidate.pop() {
            return None;
        }
    }
}

fn parse_sources(value: &str) -> Result<Vec<PackageSource>, String> {
    let mut sources = Vec::new();
    for entry in value.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            return Err("ANCHOR_DEPENDENCIES_SOURCES has an empty entry".into());
        }
        if entry.starts_with("https://") || entry.starts_with("http://") {
            // The same literal URL policy every outbound host request already
            // passes: https on a public address, http only for a loopback index,
            // no credentials, no fragment, a real port. This is a *tightening* —
            // an arbitrary host over plain http used to be accepted here — and it
            // is literal only: a host name is not resolved, so startup never
            // reaches the network.
            let url = url::Url::parse(entry).map_err(|error| {
                format!("package source `{entry}` is not a usable URL: {error}")
            })?;
            crate::oauth_http::validate_oauth_url(&url).map_err(|_| {
                format!(
                    "package source `{entry}` must be an https index on a public address (http is allowed only for a loopback index), with no credentials, no fragment and a valid port"
                )
            })?;
            sources.push(PackageSource::Index(entry.trim_end_matches('/').to_owned()));
        } else {
            let path = PathBuf::from(entry);
            if !path.is_absolute() {
                return Err(format!(
                    "package source `{entry}` must be an absolute directory or an http(s) index"
                ));
            }
            sources.push(PackageSource::Directory(path));
        }
    }
    if sources.is_empty() {
        return Err("ANCHOR_DEPENDENCIES_SOURCES must name at least one source".into());
    }
    Ok(sources)
}

/// SHA-256 of a byte string.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn a_half_configured_deployment_is_an_error_rather_than_a_default() {
        let error = parse_sources(" ").unwrap_err();
        assert!(error.contains("empty entry"), "{error}");
        assert!(parse_sources("/tmp/a,,/tmp/b").is_err());
        let error = parse_sources("relative/dir").unwrap_err();
        assert!(error.contains("absolute directory"), "{error}");
        assert_eq!(
            parse_sources("/opt/wheels, https://pypi.org/simple/").unwrap(),
            vec![
                PackageSource::Directory(PathBuf::from("/opt/wheels")),
                PackageSource::Index("https://pypi.org/simple".to_owned()),
            ]
        );
    }

    #[test]
    fn the_recorded_source_text_distinguishes_a_directory_from_an_index() {
        assert_eq!(
            PackageSource::Directory(PathBuf::from("/opt/wheels")).as_str(),
            "directory:/opt/wheels"
        );
        assert_eq!(
            PackageSource::Index("https://pypi.org/simple".to_owned()).as_str(),
            "index:https://pypi.org/simple"
        );
    }

    use crate::test_support::Environment;

    #[test]
    fn configuring_the_shared_python_interpreter_does_not_enable_preparation() {
        let _guard = crate::PROCESS_ENV_LOCK.blocking_lock();
        let _environment = Environment::cleared(&[
            "ANCHOR_DEPENDENCIES_SOURCES",
            "ANCHOR_DEPENDENCIES_UV",
            "ANCHOR_DEPENDENCIES_UV_SHA256",
            PYTHON_INTERPRETER_ENV,
            "ANCHOR_DEPENDENCIES_BUILD_ROOT",
        ]);
        unsafe { std::env::set_var(PYTHON_INTERPRETER_ENV, "/opt/anchor/python/bin/python3") };

        assert!(
            from_env(
                Path::new("/var/lib/anchor/state"),
                PathBuf::from("bwrap"),
                None,
            )
            .unwrap()
            .is_none()
        );
    }

    /// A fully authorized deployment whose staging root is `build_root`, read
    /// against a Library at `library`.
    fn configured(
        build_root: &Path,
        library: &Path,
    ) -> Result<Option<DependencyProvisionConfig>, String> {
        let sources = build_root.join("wheels");
        std::fs::create_dir_all(&sources).unwrap();
        unsafe {
            std::env::set_var("ANCHOR_DEPENDENCIES_SOURCES", &sources);
            std::env::set_var("ANCHOR_DEPENDENCIES_UV", "/opt/anchor/uv");
            std::env::set_var("ANCHOR_DEPENDENCIES_UV_SHA256", "a".repeat(64));
            std::env::set_var("ANCHOR_DEPENDENCIES_PYTHON", "/usr/bin/python3");
            std::env::set_var("ANCHOR_DEPENDENCIES_BUILD_ROOT", build_root);
        }
        from_env(
            &build_root.join("state"),
            PathBuf::from("bwrap"),
            Some(library),
        )
    }

    /// Staging and published content sharing a tree is refused when the host
    /// starts, whatever the direction of the overlap.
    #[test]
    fn a_build_root_that_overlaps_the_library_is_refused_at_startup() {
        let _guard = crate::PROCESS_ENV_LOCK.blocking_lock();
        let _environment = Environment::cleared(&[
            "ANCHOR_DEPENDENCIES_SOURCES",
            "ANCHOR_DEPENDENCIES_UV",
            "ANCHOR_DEPENDENCIES_UV_SHA256",
            "ANCHOR_DEPENDENCIES_PYTHON",
            "ANCHOR_DEPENDENCIES_BUILD_ROOT",
        ]);
        let root = tempfile::tempdir().unwrap();

        // Identical roots.
        let shared = root.path().join("shared");
        std::fs::create_dir_all(&shared).unwrap();
        let error = configured(&shared, &shared).unwrap_err();
        assert!(error.contains("must not overlap"), "{error}");

        // The build root inside the Library.
        let library = root.path().join("library");
        let inside = library.join("build");
        std::fs::create_dir_all(&inside).unwrap();
        let error = configured(&inside, &library).unwrap_err();
        assert!(error.contains("must not overlap"), "{error}");

        // The Library inside the build root.
        let build = root.path().join("build");
        let nested_library = build.join("library");
        std::fs::create_dir_all(&nested_library).unwrap();
        let error = configured(&build, &nested_library).unwrap_err();
        assert!(error.contains("must not overlap"), "{error}");

        // Disjoint roots are a working deployment.
        let disjoint = root.path().join("library-disjoint");
        std::fs::create_dir_all(&disjoint).unwrap();
        let config = configured(&build, &disjoint)
            .unwrap()
            .expect("a fully configured deployment");
        assert_eq!(config.build_root, build);
    }

    /// What must share a filesystem is the publish rename: staging is moved into
    /// the shared store and a view is moved to its final path. Views and the
    /// store themselves are linked, so they may live on different devices.
    #[test]
    fn a_build_root_on_another_filesystem_is_refused_at_startup() {
        if device_of(Path::new("/dev/shm")) == device_of(Path::new("/tmp"))
            || !Path::new("/dev/shm").is_dir()
        {
            eprintln!("SKIPPED (no verification): this host has one filesystem");
            return;
        }
        let _guard = crate::PROCESS_ENV_LOCK.blocking_lock();
        let _environment = Environment::cleared(&[
            "ANCHOR_DEPENDENCIES_SOURCES",
            "ANCHOR_DEPENDENCIES_UV",
            "ANCHOR_DEPENDENCIES_UV_SHA256",
            "ANCHOR_DEPENDENCIES_PYTHON",
            "ANCHOR_DEPENDENCIES_BUILD_ROOT",
        ]);
        let root = tempfile::tempdir().unwrap();
        let library = root.path().join("library");
        std::fs::create_dir_all(&library).unwrap();

        // Same filesystem: accepted.
        let same = configured(&root.path().join("build"), &library);
        assert!(same.unwrap().is_some());

        // A build root on another filesystem is refused, whatever the overlap.
        unsafe { std::env::set_var("ANCHOR_DEPENDENCIES_BUILD_ROOT", "/dev/shm") };
        let error = from_env(
            &root.path().join("state"),
            PathBuf::from("bwrap"),
            Some(&library),
        )
        .unwrap_err();
        assert!(error.contains("different filesystems"), "{error}");
    }

    /// Every source is checked when the host starts, literally and without a
    /// request: a non-public address and plain http on a public host are refused,
    /// while a loopback http index and a public https index are accepted.
    #[test]
    fn an_index_source_the_host_will_not_reach_is_refused_at_startup() {
        let _guard = crate::PROCESS_ENV_LOCK.blocking_lock();
        let _environment = Environment::cleared(&[
            "ANCHOR_DEPENDENCIES_SOURCES",
            "ANCHOR_DEPENDENCIES_UV",
            "ANCHOR_DEPENDENCIES_UV_SHA256",
            "ANCHOR_DEPENDENCIES_PYTHON",
            "ANCHOR_DEPENDENCIES_BUILD_ROOT",
        ]);
        let root = tempfile::tempdir().unwrap();
        unsafe {
            std::env::set_var("ANCHOR_DEPENDENCIES_UV", "/opt/anchor/uv");
            std::env::set_var("ANCHOR_DEPENDENCIES_UV_SHA256", "a".repeat(64));
            std::env::set_var("ANCHOR_DEPENDENCIES_PYTHON", "/usr/bin/python3");
        }
        let state = root.path().join("state");
        for sources in [
            // A literal private address, even over https.
            "https://10.0.0.1/simple",
            // Plain http is only for loopback: this used to be accepted.
            "http://mirror.example/simple",
            "https://user:secret@mirror.example/simple",
            "https://mirror.example/simple#fragment",
        ] {
            unsafe { std::env::set_var("ANCHOR_DEPENDENCIES_SOURCES", sources) };
            let error = from_env(&state, PathBuf::from("bwrap"), None).unwrap_err();
            assert!(
                error.contains(sources)
                    && error.contains("must be an https index on a public address"),
                "{sources}: {error}"
            );
        }

        // A loopback http index and a public https index are both allowed, in the
        // order written, and nothing above resolved a host name or opened a
        // socket.
        unsafe {
            std::env::set_var(
                "ANCHOR_DEPENDENCIES_SOURCES",
                "http://127.0.0.1:8080/simple,https://pypi.tuna.tsinghua.edu.cn/simple",
            );
        }
        let config = from_env(&state, PathBuf::from("bwrap"), None)
            .unwrap()
            .expect("a fully configured deployment");
        assert_eq!(
            config.sources,
            vec![
                PackageSource::Index("http://127.0.0.1:8080/simple".to_owned()),
                PackageSource::Index("https://pypi.tuna.tsinghua.edu.cn/simple".to_owned()),
            ]
        );
    }
}
