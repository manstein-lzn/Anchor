//! Owner-bound, short-lived review handles for the existing Plugin use cases.
//! Browser connections observe work; dropping a connection never drops publication.
use super::*;
use crate::application::plugins::{PluginInstallPrecondition, PluginUninstallPrecondition};
use crate::dependency_provision::DependencyAccess;
use anchor_library::{Checkout, GitCheckout, GithubSource};
use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_FILES: usize = 2048;
const REVIEW_LIFETIME: Duration = Duration::from_secs(1800);

/// Where install candidates are staged while their review is open. A review
/// lives in this process only, so whatever a previous process left here is
/// unreachable and is swept at start-up; staging in the system temp
/// directory instead leaked one directory per review the process never
/// dropped.
pub(crate) fn staging_root(data_root: &Path) -> PathBuf {
    data_root.join("plugin-staging")
}

/// Remove staging left by an earlier process. The caller holds the
/// deployment writer lease, so no live review can own an entry here.
pub(crate) fn sweep_staging(data_root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(staging_root(data_root)) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| std::fs::remove_dir_all(entry.path()).is_ok())
        .count()
}

pub(super) fn router() -> Router<ApiState> {
    let upload = Router::new()
        .route("/plugins/install-preflight", post(prepare_install))
        .layer(DefaultBodyLimit::max(48 * 1024 * 1024))
        .layer(RequestBodyLimitLayer::new(48 * 1024 * 1024));
    Router::new()
        .route(
            "/plugins/{plugin}/uninstall-preflight",
            post(prepare_uninstall),
        )
        .route("/plugins/preflights", get(list))
        .route("/plugins/preflights/{token}/confirm", post(confirm))
        .route("/plugins/preflights/{token}", get(status).delete(cancel))
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .merge(upload)
}

enum Action {
    Install {
        precondition: Box<PluginInstallPrecondition>,
        _staging: tempfile::TempDir,
    },
    Uninstall(PluginUninstallPrecondition),
}
struct Review {
    owner: String,
    data_root: PathBuf,
    expires: Instant,
    action: Option<Action>,
    phase: &'static str,
    projection: Value,
    result: Option<Value>,
    error: Option<String>,
    access: DependencyAccess,
}
impl Review {
    fn active(&self) -> bool {
        matches!(self.phase, "preparing" | "running")
    }
    fn snapshot(&self, token: &str) -> Value {
        let mut value = self.projection.clone();
        value["token"] = json!(token);
        value["state"] = json!(self.phase);
        value["result"] = json!(self.result);
        value["error"] = json!(self.error);
        value["expires_at"] = json!(
            chrono::Utc::now().timestamp()
                + self
                    .expires
                    .saturating_duration_since(Instant::now())
                    .as_secs() as i64
        );
        value
    }
}
static REVIEWS: OnceLock<Mutex<BTreeMap<String, Review>>> = OnceLock::new();
fn reviews() -> &'static Mutex<BTreeMap<String, Review>> {
    REVIEWS.get_or_init(Mutex::default)
}
fn prune(reviews: &mut BTreeMap<String, Review>) {
    reviews.retain(|_, review| review.active() || review.expires > Instant::now());
}
fn app_reason(error: ApplicationError) -> String {
    match error {
        ApplicationError::Missing => "Plugin resource unavailable".into(),
        ApplicationError::Conflict(reason)
        | ApplicationError::Invalid(reason)
        | ApplicationError::Storage(reason) => reason,
    }
}
#[allow(clippy::result_large_err)]
fn reserve(
    state: &ApiState,
    owner: String,
    operation: &str,
    plugin: &str,
    access: DependencyAccess,
) -> Result<(String, Value), HttpResponse> {
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot create Plugin review handle",
            )
        })?;
    use sha2::{Digest, Sha256};
    let token = format!("{:x}", Sha256::digest(bytes));
    let mut reviews = reviews().lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin reviews unavailable",
        )
    })?;
    prune(&mut reviews);
    while reviews.len() >= 16 {
        let oldest = reviews
            .iter()
            .filter(|(_, review)| matches!(review.phase, "succeeded" | "failed"))
            .min_by_key(|(_, review)| review.expires)
            .map(|(token, _)| token.clone());
        let Some(oldest) = oldest else {
            break;
        };
        reviews.remove(&oldest);
    }
    if reviews.len() >= 16
        || reviews
            .values()
            .filter(|review| {
                review.owner == owner
                    && review.data_root == state.data_root
                    && review.phase != "succeeded"
                    && review.phase != "failed"
            })
            .count()
            >= 4
    {
        return Err(error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many pending Plugin reviews; cancel a review and retry",
        ));
    }
    let review = Review {
        owner,
        data_root: state.data_root.clone(),
        expires: Instant::now() + REVIEW_LIFETIME,
        action: None,
        phase: "preparing",
        projection: json!({"operation":operation,"plugin":plugin}),
        result: None,
        error: None,
        access,
    };
    let snapshot = review.snapshot(&token);
    reviews.insert(token.clone(), review);
    Ok((token, snapshot))
}
fn prepared(token: &str, outcome: Result<(Action, Value), String>) {
    if let Ok(mut reviews) = reviews().lock()
        && let Some(review) = reviews.get_mut(token)
    {
        review.expires = Instant::now() + REVIEW_LIFETIME;
        match outcome {
            Ok((action, projection)) => {
                review.action = Some(action);
                review.projection = projection;
                review.phase = "ready";
            }
            Err(reason) => {
                review.phase = "failed";
                review.error = Some(reason);
            }
        }
    }
}
fn completed(token: &str, outcome: Result<Value, String>) {
    if let Ok(mut reviews) = reviews().lock()
        && let Some(review) = reviews.get_mut(token)
    {
        review.expires = Instant::now() + REVIEW_LIFETIME;
        match outcome {
            Ok(result) => {
                review.phase = "succeeded";
                review.result = Some(result);
            }
            Err(reason) => {
                review.phase = "failed";
                review.error = Some(reason);
            }
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadFile {
    path: String,
    content_base64: String,
    #[serde(default)]
    executable: bool,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallInput {
    id: String,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    files: Option<Vec<UploadFile>>,
    /// Network permission comes from the operator, never the candidate.
    #[serde(default)]
    allow_network: bool,
}

fn stage_upload(directory: &Path, files: Vec<UploadFile>) -> Result<(), String> {
    if files.is_empty() || files.len() > MAX_FILES {
        return Err("upload must contain 1–2048 regular files".into());
    }
    let mut names = BTreeSet::new();
    let mut decoded = Vec::new();
    let mut total = 0usize;
    for file in files {
        if file.path.len() > 1024
            || file.path.contains('\\')
            || file.path.contains('\0')
            || file.path.contains(':')
            || file.path.split('/').count() > 32
            || file
                .path
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !names.insert(file.path.clone())
        {
            return Err(
                "upload paths must be unique, relative regular-file paths without traversal".into(),
            );
        }
        let bytes = STANDARD
            .decode(&file.content_base64)
            .map_err(|_| "upload file content is not valid base64")?;
        total = total
            .checked_add(bytes.len())
            .ok_or("upload size overflow")?;
        if total > MAX_BYTES {
            return Err("Plugin upload exceeds 32 MiB".into());
        }
        decoded.push((file.path, bytes, file.executable));
    }
    for path in &names {
        let mut parent = Path::new(path).parent();
        while let Some(value) = parent {
            if names.contains(value.to_str().ok_or("upload path is not UTF-8")?) {
                return Err("upload file conflicts with another file's parent directory".into());
            }
            parent = value.parent();
        }
    }
    // Fresh private staging accepts only regular files. The closed schema has
    // no link/device types and only one explicit executable-mode choice.
    for (relative, bytes, executable) in decoded {
        let path = directory.join(relative);
        std::fs::create_dir_all(path.parent().ok_or("upload parent unavailable")?)
            .map_err(|_| "upload staging failed")?;
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "upload staging failed")?;
        file.write_all(&bytes)
            .map_err(|_| "upload staging failed")?;
        file.set_permissions(std::fs::Permissions::from_mode(if executable {
            0o755
        } else {
            0o644
        }))
        .map_err(|_| "upload staging failed")?;
    }
    Ok(())
}

fn bounded_candidate(directory: &Path, access: &DependencyAccess) -> Result<(), String> {
    let mut pending = vec![(directory.to_path_buf(), 0usize)];
    let (mut files, mut entries, mut bytes) = (0usize, 0usize, 0u64);
    while let Some((path, depth)) = pending.pop() {
        access.check().map_err(|error| error.to_string())?;
        entries += 1;
        if depth > 32 || entries > MAX_FILES * 32 {
            return Err("Plugin candidate has too many entries or excessive nesting".into());
        }
        let metadata =
            std::fs::symlink_metadata(&path).map_err(|_| "Plugin candidate unavailable")?;
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|_| "Plugin candidate unavailable")? {
                pending.push((
                    entry.map_err(|_| "Plugin candidate unavailable")?.path(),
                    depth + 1,
                ));
                if pending.len() > MAX_FILES * 32 {
                    return Err("Plugin candidate has too many entries".into());
                }
            }
        } else if metadata.is_file() {
            files += 1;
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or("Plugin candidate size overflow")?;
            if files > MAX_FILES || bytes > MAX_BYTES as u64 {
                return Err("Plugin candidate exceeds 2048 files or 32 MiB".into());
            }
        } else {
            return Err("Plugin candidates accept only real directories and regular files, without symlinks".into());
        }
    }
    Ok(())
}

async fn prepare_install(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<InstallInput>,
) -> Result<Json<Value>, HttpResponse> {
    if !anchor_runtime::identifier::is_plugin_id(&body.id)
        || body.source.is_some() == body.files.is_some()
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "provide a Plugin id and exactly one GitHub source or file upload",
        ));
    }
    if let Some(source) = &body.source {
        if !body.allow_network {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "GitHub import requires explicit network authorization",
            ));
        }
        GithubSource::parse(source).map_err(|_| {
            error(
                StatusCode::BAD_REQUEST,
                "use an HTTPS github.com tree URL for a Plugin directory",
            )
        })?;
    }
    let access = DependencyAccess {
        network: body.allow_network,
        ..Default::default()
    };
    let (token, snapshot) = reserve(
        &state,
        private_owner(&state, &headers),
        "install",
        &body.id,
        access.clone(),
    )?;
    tokio::spawn(async move {
        let outcome = build_install(&state, body, access).await;
        prepared(&token, outcome);
    });
    Ok(Json(snapshot))
}
async fn build_install(
    state: &ApiState,
    body: InstallInput,
    access: DependencyAccess,
) -> Result<(Action, Value), String> {
    let source = body.source.clone();
    let id = body.id;
    let staging_access = access.clone();
    let staging_root = staging_root(&state.data_root);
    let (staging, candidate) = tokio::task::spawn_blocking(move || {
        staging_access.check().map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&staging_root).map_err(|_| "Plugin staging unavailable")?;
        let staging = tempfile::Builder::new()
            .prefix("upload-")
            .tempdir_in(&staging_root)
            .map_err(|_| "Plugin staging unavailable")?;
        let candidate = if let Some(source) = body.source {
            let source = GithubSource::parse(&source).map_err(|error| error.to_string())?;
            let checkout = staging.path().join("checkout");
            // The fixed Library checkout has bounded step deadlines, but does
            // not support cancellation inside Git. Observe cancellation next.
            GitCheckout
                .checkout(&source, &checkout)
                .map_err(|reason| reason.to_string())?;
            let mut candidate = checkout;
            for part in source.relative_path().components() {
                candidate.push(part);
                if !std::fs::symlink_metadata(&candidate)
                    .map_err(|_| "GitHub Plugin directory unavailable")?
                    .is_dir()
                {
                    return Err("GitHub Plugin paths must contain only real directories".into());
                }
            }
            candidate
        } else {
            let directory = staging.path().join("candidate");
            std::fs::create_dir(&directory).map_err(|_| "Plugin staging unavailable")?;
            stage_upload(&directory, body.files.unwrap_or_default())?;
            directory
        };
        staging_access.check().map_err(|error| error.to_string())?;
        bounded_candidate(&candidate, &staging_access)?;
        Ok::<_, String>((staging, candidate))
    })
    .await
    .map_err(|_| "Plugin staging task failed")??;
    let precondition = state
        .application
        .plugin_install_precondition_with_access(
            state.application.library_root().to_path_buf(),
            candidate,
            id,
            access,
        )
        .await
        .map_err(app_reason)?;
    let review = &precondition.review;
    let projection = json!({"operation":"install","plugin":review.id,"name":review.name,"description":review.description,
        "digest":review.digest,"authorization_digest":precondition.authorization_digest,"network":precondition.network,"mutable_paths":precondition.mutable,
        "existing_digest":precondition.existing_digest,"replacing":precondition.replacing()&&!precondition.already_installed(),
        "already_installed":precondition.already_installed(),"source":source,"resources":review.resources,"resource_bytes":review.resource_bytes,
        "skills":review.skills,"servers":review.servers.iter().map(|server|json!({"name":server.name,"transport":server.transport,"auth":server.auth,
            "command":server.command,"argument_count":server.args.len(),"remote_origin":server.url.as_ref().and_then(|value|url::Url::parse(value).ok()).map(|url|url.origin().ascii_serialization()),
            "environment":server.environment,"optional_environment":server.optional_environment,"channel_identity":server.channel_identity})).collect::<Vec<_>>(),
        "channels":review.channels,"warnings":review.warnings,"inline_values":review.inline_values,"binary_without_source":review.binary_without_source,
        "graphs":precondition.impact.graphs,"coverage":precondition.impact.coverage,
        "dependencies":precondition.dependencies.as_ref().map(|plan|json!({"sources":plan.sources.iter().map(|source| url::Url::parse(source).ok().filter(|url| matches!(url.scheme(), "https" | "http")).map(|url| url.origin().ascii_serialization()).unwrap_or_else(|| "部署授权的本地包目录".into())).collect::<Vec<_>>(),"interpreter":plan.interpreter,"lock_digest":plan.lock_digest,"packages":plan.packages}))});
    Ok((
        Action::Install {
            precondition: Box::new(precondition),
            _staging: staging,
        },
        projection,
    ))
}
async fn prepare_uninstall(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(plugin): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let precondition = state
        .application
        .plugin_uninstall_precondition(state.application.library_root(), &plugin, &store(&state)?)
        .await
        .map_err(application_error)?;
    let projection = json!({"operation":"uninstall","plugin":precondition.id,"name":precondition.name,"digest":precondition.digest,
        "mutable":precondition.mutable.iter().map(|(path,bytes)|json!({"path":path,"bytes":bytes})).collect::<Vec<_>>(),
        "sessions":precondition.sessions,"unfinished_runs":precondition.unfinished_runs,"graphs":precondition.graphs,
        "coverage":precondition.coverage,"refusal":precondition.refusal()});
    let (token, _) = reserve(
        &state,
        private_owner(&state, &headers),
        "uninstall",
        &plugin,
        DependencyAccess {
            network: false,
            ..Default::default()
        },
    )?;
    prepared(&token, Ok((Action::Uninstall(precondition), projection)));
    status(State(state), headers, AxumPath(token)).await
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirm {
    confirm: bool,
    #[serde(default)]
    replace_existing: bool,
    #[serde(default)]
    accept_unreadable: bool,
    #[serde(default)]
    confirm_data_loss: bool,
}
async fn confirm(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(token): AxumPath<String>,
    Json(body): Json<Confirm>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    let (action, snapshot) = {
        let mut reviews = reviews().lock().map_err(|_| {
            error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Plugin reviews unavailable",
            )
        })?;
        prune(&mut reviews);
        let review=reviews.get_mut(&token).filter(|review|review.owner==owner&&review.data_root==state.data_root)
            .ok_or_else(||error(StatusCode::NOT_FOUND,"Plugin review unavailable; refresh the Library and inspect its current version before retrying"))?;
        // Repeated confirmation observes the original execution, even if the
        // original response was lost. It never installs or uninstalls twice.
        if matches!(review.phase, "running" | "succeeded" | "failed") {
            return Ok(Json(review.snapshot(&token)));
        }
        if review.phase != "ready" {
            return Err(error(
                StatusCode::CONFLICT,
                "Plugin preflight is still preparing",
            ));
        }
        if !body.confirm {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "explicit Plugin confirmation is required",
            ));
        }
        match review.action.as_ref() {
            Some(Action::Install { precondition, .. }) => {
                if precondition.replacing()
                    && !precondition.already_installed()
                    && !body.replace_existing
                    || precondition.review.binary_without_source && !body.accept_unreadable
                {
                    return Err(error(
                        StatusCode::BAD_REQUEST,
                        "replacement and unreadable binaries require explicit confirmation",
                    ));
                }
            }
            Some(Action::Uninstall(_)) => {
                if !body.confirm_data_loss {
                    return Err(error(
                        StatusCode::BAD_REQUEST,
                        "uninstall requires confirmation that the Plugin's only data copy will be deleted",
                    ));
                }
            }
            None => {
                return Err(error(
                    StatusCode::CONFLICT,
                    "Plugin review has no prepared action",
                ));
            }
        }
        let action = review
            .action
            .take()
            .ok_or_else(|| error(StatusCode::CONFLICT, "Plugin review unavailable"))?;
        review.phase = "running";
        (action, review.snapshot(&token))
    };
    tokio::spawn(async move {
        let library = state.application.library_root().to_path_buf();
        let outcome = match action {
            Action::Install {
                precondition,
                _staging,
            } => {
                let result=state.application.install_plugin_if_unchanged(library,*precondition).await.map_err(app_reason)
                    .map(|result|json!({"installed":true,"plugin":result.id,"digest":result.digest}));
                drop(_staging);
                result
            }
            Action::Uninstall(precondition) => match store(&state) {
                Ok(sessions) => state
                    .application
                    .uninstall_plugin_if_unchanged_with_access(
                        &library,
                        &precondition,
                        &sessions,
                        &DependencyAccess {
                            network: false,
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(app_reason)
                    .map(|result| json!({"uninstalled":true,"result":result})),
                Err(_) => Err("Session Plugin mounts unavailable".into()),
            },
        };
        completed(&token, outcome);
    });
    Ok(Json(snapshot))
}
async fn list(
    State(state): State<ApiState>,
    headers: HeaderMap,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    let mut reviews = reviews().lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin reviews unavailable",
        )
    })?;
    prune(&mut reviews);
    Ok(Json(
        json!({"reviews":reviews.iter().filter(|(_,review)|review.owner==owner&&review.data_root==state.data_root)
        .map(|(token,review)|review.snapshot(token)).collect::<Vec<_>>(),"restart_note":"Review handles are temporary. After a Host restart, inspect the current Library digest; a missing handle does not prove publication failed."}),
    ))
}
async fn status(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(token): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let owner = private_owner(&state, &headers);
    let mut reviews = reviews().lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin reviews unavailable",
        )
    })?;
    prune(&mut reviews);
    reviews.get(&token).filter(|review|review.owner==owner&&review.data_root==state.data_root).map(|review|Json(review.snapshot(&token)))
        .ok_or_else(||error(StatusCode::NOT_FOUND,"Plugin review unavailable; refresh the Library and inspect its current version before retrying"))
}
async fn cancel(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(token): AxumPath<String>,
) -> Result<StatusCode, HttpResponse> {
    let owner = private_owner(&state, &headers);
    let mut reviews = reviews().lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Plugin reviews unavailable",
        )
    })?;
    let review = reviews
        .get(&token)
        .filter(|review| review.owner == owner && review.data_root == state.data_root)
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "Plugin review unavailable"))?;
    if review.phase == "running" {
        return Err(error(
            StatusCode::CONFLICT,
            "Plugin publication is running; inspect its result instead of cancelling",
        ));
    }
    review
        .access
        .cancellation
        .store(true, std::sync::atomic::Ordering::Release);
    reviews.remove(&token);
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(path: &str) -> UploadFile {
        UploadFile {
            path: path.into(),
            content_base64: STANDARD.encode("test"),
            executable: false,
        }
    }
    #[test]
    fn uploaded_paths_cannot_escape_or_create_file_parent_conflicts() {
        for path in [
            "/escape",
            "../escape",
            "a/../escape",
            "a\\escape",
            "a//escape",
            "a/./escape",
            "C:/escape",
        ] {
            let root = tempfile::tempdir().unwrap();
            assert!(
                stage_upload(root.path(), vec![file(path)]).is_err(),
                "{path}"
            );
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
        let root = tempfile::tempdir().unwrap();
        assert!(stage_upload(root.path(), vec![file("a"), file("a/b")]).is_err());
        assert!(stage_upload(root.path(), vec![file("same"), file("same")]).is_err());
        assert!(stage_upload(root.path(), vec![]).is_err());
        stage_upload(root.path(), vec![file("skills/demo/SKILL.md")]).unwrap();
        assert!(root.path().join("skills/demo/SKILL.md").is_file());
    }

    #[test]
    fn candidate_limits_are_checked_without_reading_large_files_or_following_links() {
        let root = tempfile::tempdir().unwrap();
        let large = root.path().join("large.bin");
        std::fs::File::create(&large)
            .unwrap()
            .set_len(MAX_BYTES as u64 + 1)
            .unwrap();
        assert!(bounded_candidate(root.path(), &DependencyAccess::default()).is_err());
        std::fs::remove_file(large).unwrap();
        std::os::unix::fs::symlink("/etc", root.path().join("linked")).unwrap();
        assert!(bounded_candidate(root.path(), &DependencyAccess::default()).is_err());
    }
}
