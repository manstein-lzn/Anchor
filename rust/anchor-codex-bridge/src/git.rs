use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command};
use walkdir::WalkDir;

#[derive(Clone, Debug, Serialize)]
pub struct ProjectSummary {
    pub name: String,
    pub path: String,
    pub relative_path: String,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub clean: bool,
    pub managed_worktrees: Vec<WorktreeInfo>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WorktreeInfo {
    pub slug: String,
    pub path: String,
    pub branch: String,
}

#[derive(Clone, Debug)]
pub struct PreparedWorktree {
    pub path: PathBuf,
    pub branch: String,
    pub git_common_dir: PathBuf,
}

pub(crate) fn canonical_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!(
            "directory `{}` must be absolute without `..`",
            path.display()
        ));
    }
    let resolved = path
        .canonicalize()
        .map_err(|e| format!("directory `{}` cannot be resolved: {e}", path.display()))?;
    if !resolved.is_dir() {
        return Err(format!("`{}` is not a directory", path.display()));
    }
    if resolved.to_str().is_none() {
        return Err("directory must be representable as UTF-8 without replacement".into());
    }
    Ok(resolved)
}

/// Inspect each component, including dangling links, before creating managed paths.
/// Metadata pointers may contain `..`; public directory inputs may not.
pub(crate) fn check_path_components(path: &Path, allow_missing: bool) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("path `{}` must be absolute", path.display()));
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "managed path `{}` must not be a symlink",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(e) if allow_missing && e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("cannot inspect `{}`: {e}", current.display())),
        }
    }
    Ok(())
}

pub(crate) fn canonical_managed_directory(path: &Path) -> Result<PathBuf, String> {
    check_path_components(path, false)?;
    canonical_directory(path)
}

fn metadata_line(path: &Path) -> Result<String, String> {
    check_path_components(path, false)?;
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > 8192 {
        return Err(format!("invalid Git metadata file `{}`", path.display()));
    }
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let line = content.trim_end_matches(['\r', '\n']);
    if line.is_empty() || line.chars().any(char::is_control) {
        return Err(format!("invalid Git metadata in `{}`", path.display()));
    }
    Ok(line.to_owned())
}

fn metadata_path(base: &Path, value: &str) -> Result<PathBuf, String> {
    let path = base.join(value);
    check_path_components(&path, false)?;
    path.canonicalize().map_err(|e| e.to_string())
}

fn main_repo_common_dir(repo: &Path) -> Result<PathBuf, String> {
    let common = canonical_managed_directory(&repo.join(".git"))?;
    metadata_line(&common.join("HEAD"))?;
    canonical_managed_directory(&common.join("objects"))?;
    canonical_managed_directory(&common.join("refs"))?;
    // Separate git dirs, bare repositories and linked worktrees are not main repos.
    match fs::symlink_metadata(common.join("commondir")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(common),
        _ => Err("main repository must own its .git common directory".into()),
    }
}

fn linked_worktree_metadata(worktree: &Path) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let worktree = canonical_managed_directory(worktree)?;
    let pointer = metadata_line(&worktree.join(".git"))?;
    let raw = pointer
        .strip_prefix("gitdir: ")
        .ok_or_else(|| "managed worktree has no Git directory pointer".to_owned())?;
    let gitdir = metadata_path(&worktree, raw)?;
    let common = metadata_path(&gitdir, &metadata_line(&gitdir.join("commondir"))?)?;
    if common.file_name().and_then(|n| n.to_str()) != Some(".git")
        || gitdir.parent() != Some(common.join("worktrees").as_path())
    {
        return Err("managed worktree has an unexpected Git common directory".into());
    }
    let repo = common
        .parent()
        .ok_or_else(|| "Git common directory has no parent repository".to_owned())?
        .to_path_buf();
    if main_repo_common_dir(&repo)? != common
        || metadata_path(&gitdir, &metadata_line(&gitdir.join("gitdir"))?)? != worktree.join(".git")
    {
        return Err(
            "managed worktree Git registration does not point back to this checkout".into(),
        );
    }
    Ok((repo, common, gitdir))
}

/// Shared synchronous identity check for configuration routing and discovery.
/// Git's reciprocal pointers are the authority; the bucket name alone is not.
/// Writers of the shared .git directory can change that authority. These checks
/// are not multi-tenant isolation or an atomic filesystem race boundary.
pub(crate) fn managed_worktree_metadata(
    worktree_root: &Path,
    cwd: &Path,
) -> Result<Option<(PathBuf, String, PathBuf)>, String> {
    let root = canonical_managed_directory(worktree_root)?;
    let resolved = canonical_directory(cwd)?;
    if !resolved.starts_with(&root) && !cwd.starts_with(&root) {
        return Ok(None);
    }
    let cwd = canonical_managed_directory(cwd)?;
    let mut parts = cwd
        .strip_prefix(&root)
        .map_err(|_| "managed worktree escapes its root".to_owned())?
        .components();
    let bucket = parts.next().ok_or("managed worktree bucket is missing")?;
    let slug = parts
        .next()
        .and_then(|part| part.as_os_str().to_str())
        .ok_or("managed worktree slug is missing")?;
    if validate_worktree_slug(slug)? != slug {
        return Err("managed worktree slug is not canonical".into());
    }
    let target = root.join(bucket).join(slug);
    let (repo, common, gitdir) = linked_worktree_metadata(&target)?;
    if repo.starts_with(&root) || target.parent() != Some(repo_bucket_dir(&root, &repo).as_path()) {
        return Err("managed worktree is outside its repository bucket".into());
    }
    if metadata_line(&gitdir.join("HEAD"))? != format!("ref: refs/heads/codex/{slug}") {
        return Err("managed worktree is not on its expected codex branch".into());
    }
    // A nested checkout must not inherit the enclosing worktree's identity.
    for directory in cwd.ancestors().take_while(|path| *path != target) {
        match fs::symlink_metadata(directory.join(".git")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err("nested Git checkout is not the managed worktree".into()),
        }
    }
    Ok(Some((repo, slug.to_owned(), common)))
}

async fn verify_git_layout(cwd: &Path, repo: &Path, common: &Path) -> Result<(), String> {
    let output = git_stdout(
        cwd,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
            "--git-common-dir",
        ],
    )
    .await?;
    let paths: Vec<_> = output.lines().collect();
    if paths.len() != 2
        || canonical_directory(Path::new(paths[0]))? != repo
        || canonical_managed_directory(Path::new(paths[1]))? != common
    {
        return Err("Git checkout/common directory differs from the authorized repository".into());
    }
    Ok(())
}

/// Recover (main repository, slug, common Git directory) for a managed cwd.
/// Only existing directories outside the managed root yield `Ok(None)`; invalid
/// managed paths fail closed. Callers must still authorize the returned repo.
pub async fn managed_worktree_info(
    worktree_root: &Path,
    cwd: &Path,
) -> Result<Option<(PathBuf, String, PathBuf)>, String> {
    let Some((repo, slug, common)) = managed_worktree_metadata(worktree_root, cwd)? else {
        return Ok(None);
    };
    let root = canonical_managed_directory(worktree_root)?;
    verify_git_layout(&repo, &repo, &common).await?;
    verify_git_layout(cwd, &repo_bucket_dir(&root, &repo).join(&slug), &common).await?;
    Ok(Some((repo, slug, common)))
}

pub fn validate_worktree_slug(slug: &str) -> Result<String, String> {
    let trimmed = slug.trim();
    if trimmed.is_empty() || trimmed.len() > 64 {
        return Err("worktree slug must be 1..=64 ASCII characters".into());
    }
    if trimmed.starts_with('.') || trimmed.starts_with('-') || trimmed.contains("..") {
        return Err("worktree slug cannot start with `.` or `-` or contain `..`".into());
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err("worktree slug may contain only ASCII letters, digits, `-` and `_`".into());
    }
    Ok(trimmed.to_owned())
}

fn repo_bucket_dir(worktree_root: &Path, repo: &Path) -> PathBuf {
    let name = repo
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("repo");
    let safe_name: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let digest = format!("{:x}", Sha256::digest(repo.to_string_lossy().as_bytes()));
    worktree_root.join(format!("{safe_name}-{}", &digest[..12]))
}

pub async fn discover_projects(roots: &[PathBuf], worktree_root: &Path) -> Vec<ProjectSummary> {
    let mut repos = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for root in roots {
        let root_clone = root.clone();
        let wt_root = worktree_root.to_path_buf();
        let found = tokio::task::spawn_blocking(move || scan_git_repos(&root_clone, &wt_root))
            .await
            .unwrap_or_default();
        for repo in found {
            if seen.insert(repo.clone()) {
                repos.push((root.clone(), repo));
            }
        }
    }
    let mut out = Vec::with_capacity(repos.len());
    for (base, repo) in repos {
        if validate_main_repo(worktree_root, &repo).await.is_err() {
            continue;
        }
        let branch = git_stdout(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
            .await
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let head = git_stdout(&repo, &["rev-parse", "--short=12", "HEAD"])
            .await
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        let clean = git_stdout(&repo, &["status", "--porcelain"])
            .await
            .map(|s| s.trim().is_empty())
            .unwrap_or(false);
        let managed_worktrees = list_managed_worktrees(worktree_root, &repo);
        let relative = repo
            .strip_prefix(&base)
            .ok()
            .and_then(|p| p.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(".")
            .to_owned();
        let name = repo
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(".")
            .to_owned();
        out.push(ProjectSummary {
            name,
            path: repo.to_string_lossy().into_owned(),
            relative_path: relative,
            branch,
            head,
            clean,
            managed_worktrees,
        });
    }
    out
}

fn scan_git_repos(root: &Path, worktree_root: &Path) -> Vec<PathBuf> {
    let Ok(root) = canonical_directory(root) else {
        return Vec::new();
    };
    let Ok(worktree_root) = canonical_managed_directory(worktree_root) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut it = WalkDir::new(&root)
        .max_depth(3)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    while let Some(entry) = it.next() {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if path.starts_with(&worktree_root) {
            it.skip_current_dir();
            continue;
        }
        if entry.depth() > 0
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
            && (name.starts_with('.')
                || matches!(
                    name,
                    "node_modules" | "target" | "vendor" | "__pycache__" | "dist" | "build"
                ))
        {
            it.skip_current_dir();
            continue;
        }
        if entry.file_type().is_dir() && path.join(".git").exists() {
            if let Ok(canonical) = path.canonicalize() {
                found.push(canonical);
            }
            it.skip_current_dir();
        }
    }
    found.sort();
    found
}

pub fn list_managed_worktrees(worktree_root: &Path, repo: &Path) -> Vec<WorktreeInfo> {
    let Ok(repo) = canonical_directory(repo) else {
        return Vec::new();
    };
    let Ok(root) = canonical_managed_directory(worktree_root) else {
        return Vec::new();
    };
    let bucket = repo_bucket_dir(&root, &repo);
    if canonical_managed_directory(&bucket).is_err() {
        return Vec::new();
    }
    let Ok(entries) = fs::read_dir(&bucket) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(Some((main, slug, _))) = managed_worktree_metadata(&root, &path) else {
            continue;
        };
        if main != repo {
            continue;
        }
        items.push(WorktreeInfo {
            slug: slug.clone(),
            path: path.to_string_lossy().into_owned(),
            branch: format!("codex/{slug}"),
        });
    }
    items.sort_by(|a, b| a.slug.cmp(&b.slug));
    items
}

pub async fn ensure_worktree(
    worktree_root: &Path,
    repo: &Path,
    slug: &str,
    base_ref: Option<&str>,
) -> Result<PreparedWorktree, String> {
    let slug = validate_worktree_slug(slug)?;
    let (root, repo, common) = validate_main_repo(worktree_root, repo).await?;
    let start_point = validated_base_commit(&repo, base_ref).await?;
    let bucket = repo_bucket_dir(&root, &repo);
    check_path_components(&bucket, true)?;
    match fs::create_dir(&bucket) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("cannot create worktree bucket: {e}")),
    }
    canonical_managed_directory(&bucket)?;
    let target = bucket.join(&slug);
    let branch = format!("codex/{slug}");
    let exists = match fs::symlink_metadata(&target) {
        Ok(_) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(format!("cannot inspect worktree target: {e}")),
    };
    if !exists {
        // Git accepts an existing empty target. Coordinate on that inode, not
        // the shared .git directory, so independent worktrees stay parallel.
        fs::create_dir(&target).map_err(|e| format!("cannot reserve worktree target: {e}"))?;
        let _lease = anchor_file_lease::ProjectLease::acquire(&target).map_err(|e| {
            // Roll back only our new, still-empty reservation.
            let _ = fs::remove_dir(&target);
            format!("project writer lease unavailable: {e}")
        })?;
        let branch_ref = format!("refs/heads/{branch}");
        let branch_exists = git_stdout(
            &repo,
            &["for-each-ref", "--format=%(refname)", "--", &branch_ref],
        )
        .await?
        .lines()
        .any(|line| line == branch_ref);
        if branch_exists {
            git_stdout(
                &repo,
                &["worktree", "add", "--", &target.to_string_lossy(), &branch],
            )
            .await?;
        } else {
            git_stdout(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &branch,
                    "--",
                    &target.to_string_lossy(),
                    &start_point,
                ],
            )
            .await?;
        }
    }
    validate_expected_worktree(&root, &target, &repo, &slug, &common).await?;
    Ok(PreparedWorktree {
        path: target,
        branch,
        git_common_dir: common,
    })
}

async fn validate_main_repo(
    worktree_root: &Path,
    repo: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    let root = canonical_managed_directory(worktree_root)?;
    let repo = canonical_directory(repo)?;
    if repo.starts_with(&root) {
        return Err("managed worktree cannot be used as a main repository".into());
    }
    let common = main_repo_common_dir(&repo)?;
    verify_git_layout(&repo, &repo, &common).await?;
    Ok((root, repo, common))
}

async fn validated_base_commit(repo: &Path, base_ref: Option<&str>) -> Result<String, String> {
    let base_ref = base_ref.unwrap_or("HEAD");
    if base_ref.is_empty()
        || base_ref.starts_with('-')
        || base_ref
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("base_ref must be a nonempty Git revision, not an option".into());
    }
    let commit = git_stdout(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{base_ref}^{{commit}}"),
        ],
    )
    .await?;
    let commit = commit.trim();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("base_ref did not resolve to a commit object".into());
    }
    Ok(commit.to_owned())
}

async fn validate_expected_worktree(
    root: &Path,
    target: &Path,
    repo: &Path,
    slug: &str,
    common: &Path,
) -> Result<(), String> {
    match managed_worktree_info(root, target).await? {
        Some((main, actual_slug, actual_common))
            if main == repo && actual_slug == slug && actual_common == common =>
        {
            Ok(())
        }
        _ => Err("managed worktree does not belong to the authorized repository".into()),
    }
}

pub async fn remove_managed_worktree(
    worktree_root: &Path,
    repo: &Path,
    slug: &str,
    force: bool,
) -> Result<PathBuf, String> {
    let slug = validate_worktree_slug(slug)?;
    let (root, repo, common) = validate_main_repo(worktree_root, repo).await?;
    let target = repo_bucket_dir(&root, &repo).join(&slug);
    validate_expected_worktree(&root, &target, &repo, &slug, &common).await?;
    let _lease = anchor_file_lease::ProjectLease::acquire(&target)
        .map_err(|e| format!("project writer lease unavailable: {e}"))?;
    if !force {
        let dirty = !git_stdout(&target, &["status", "--porcelain"])
            .await?
            .trim()
            .is_empty();
        if dirty {
            return Err(format!(
                "managed worktree `{slug}` has uncommitted changes; commit/merge first or pass force=true"
            ));
        }
    }
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let target_str = target.to_string_lossy().into_owned();
    args.push("--");
    args.push(&target_str);
    git_stdout(&repo, &args).await?;
    Ok(target)
}

pub async fn git_workspace_snapshot(cwd: &Path) -> Option<serde_json::Value> {
    canonical_directory(cwd).ok()?;
    let branch = git_stdout(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .ok()?
        .trim()
        .to_owned();
    let head = git_stdout(cwd, &["rev-parse", "--short=12", "HEAD"])
        .await
        .ok()
        .map(|s| s.trim().to_owned());
    let status = git_stdout(cwd, &["status", "--short"]).await.ok()?;
    let diff_stat = git_stdout(cwd, &["diff", "--stat"]).await.ok()?;
    Some(serde_json::json!({
        "branch": branch,
        "head": head,
        "clean": status.trim().is_empty(),
        "status_short": truncate_chars(status.trim(), 4000),
        "diff_stat": truncate_chars(diff_stat.trim(), 4000),
    }))
}

pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    let count = input.chars().count();
    if count <= max_chars {
        return input.to_owned();
    }
    let kept: String = input.chars().take(max_chars).collect();
    format!("{kept}\n...[truncated {} chars]", count - max_chars)
}

async fn git_stdout(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("--no-pager")
        .arg("--no-optional-locks")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "diff.autoRefreshIndex=false",
        ])
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0");
    // Inherited repository selectors must not override the authorized -C.
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_NAMESPACE",
    ] {
        command.env_remove(key);
    }
    let output = bounded_output(command, Duration::from_secs(15))
        .await
        .map_err(|e| format!("git {args:?} in {}: {e}", cwd.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "git {:?} exited with {:?}: {}",
            args,
            output.status.code(),
            stderr.trim()
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("Git output is not UTF-8: {e}"))
}

async fn bounded_output(mut command: Command, timeout: Duration) -> Result<Output, String> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| format!("subprocess failed to launch: {e}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let result = tokio::time::timeout(timeout, async {
        let (status, _, _) = tokio::try_join!(
            child.wait(),
            stdout.read_to_end(&mut out),
            stderr.read_to_end(&mut err)
        )?;
        Ok::<_, std::io::Error>(Output {
            status,
            stdout: out,
            stderr: err,
        })
    })
    .await;
    match result {
        Ok(result) => result.map_err(|e| format!("subprocess failed: {e}")),
        Err(_) => {
            // Do not hand the checkout to another writer while Git is exiting.
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err("subprocess timed out".into())
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) async fn init_repo(path: &Path) -> PathBuf {
        fs::create_dir_all(path).unwrap();
        git_stdout(path, &["init", "--initial-branch=main"])
            .await
            .unwrap();
        git_stdout(
            path,
            &[
                "-c",
                "user.name=Bridge Test",
                "-c",
                "user.email=bridge@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        )
        .await
        .unwrap();
        path.canonicalize().unwrap()
    }

    fn root(temp: &tempfile::TempDir) -> PathBuf {
        let path = temp.path().join("managed");
        fs::create_dir(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn project_inspection_does_not_refresh_index_under_host_lease() {
        let temp = tempfile::tempdir().unwrap();
        let repo = init_repo(&temp.path().join("project")).await;
        let file = repo.join("tracked");
        fs::write(&file, "unchanged content\n").unwrap();
        git_stdout(&repo, &["add", "tracked"]).await.unwrap();
        let index = repo.join(".git/index");
        let before = fs::read(&index).unwrap();
        fs::File::open(&file)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(123456)),
            )
            .unwrap();
        let _host = anchor_file_lease::ProjectLease::acquire(&repo).unwrap();
        assert!(git_workspace_snapshot(&repo).await.is_some());
        assert_eq!(discover_projects(&[repo], &root(&temp)).await.len(), 1);
        assert_eq!(fs::read(index).unwrap(), before);
    }

    #[test]
    fn invalid_slugs_never_become_paths_or_options() {
        for slug in [
            "",
            ".",
            "..",
            "../escape",
            "/absolute",
            "-flag",
            "a/b",
            "a\\b",
            "a..b",
            "a b",
        ] {
            assert!(validate_worktree_slug(slug).is_err(), "accepted {slug:?}");
        }
        assert!(validate_worktree_slug(&"a".repeat(65)).is_err());
        assert_eq!(
            validate_worktree_slug(" task-1_ok "),
            Ok("task-1_ok".into())
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_validation_never_normalizes_missing_or_lossy_paths() {
        use std::os::unix::ffi::OsStringExt;
        let temp = tempfile::tempdir().unwrap();
        assert!(canonical_directory(&temp.path().join("missing")).is_err());
        assert!(canonical_directory(Path::new(".")).is_err());
        assert!(canonical_directory(&temp.path().join("..")).is_err());
        let non_utf8 = temp.path().join(std::ffi::OsString::from_vec(vec![0xff]));
        fs::create_dir(&non_utf8).unwrap();
        assert!(canonical_directory(&non_utf8).is_err());
    }

    #[tokio::test]
    async fn worktree_roundtrip_recovers_identity_and_preserves_dirty_data() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let repo = init_repo(&temp.path().join("repo")).await;
        assert!(managed_worktree_info(&root, &repo).await.unwrap().is_none());
        assert!(
            managed_worktree_info(&root, &root.join("missing"))
                .await
                .is_err()
        );
        let target = repo_bucket_dir(&root, &repo).join("task");
        let blocker = anchor_file_lease::ProjectLease::acquire(&root).unwrap();
        let error = ensure_worktree(&root, &repo, "task", Some("main"))
            .await
            .unwrap_err();
        assert!(
            error.contains("project writer lease unavailable"),
            "{error}"
        );
        assert!(
            !target.exists(),
            "busy creation must not leave a reserved target"
        );
        drop(blocker);
        let wt = ensure_worktree(&root, &repo, "task", Some("main"))
            .await
            .unwrap();
        let (_, _, gitdir) = linked_worktree_metadata(&wt.path).unwrap();
        fs::write(
            wt.path.join(".git"),
            format!(
                "gitdir: ../../../repo/.git/worktrees/{}\n",
                gitdir.file_name().unwrap().to_str().unwrap()
            ),
        )
        .unwrap();
        assert_eq!(wt.git_common_dir, repo.join(".git"));
        assert_eq!(wt.path, target);
        let nested = wt.path.join("src");
        fs::create_dir(&nested).unwrap();
        let identity = (repo.clone(), "task".to_owned(), repo.join(".git"));
        assert_eq!(
            managed_worktree_info(&root, &nested).await.unwrap(),
            Some(identity)
        );
        assert_eq!(
            ensure_worktree(&root, &repo, "task", None)
                .await
                .unwrap()
                .path,
            wt.path
        );
        let listed = list_managed_worktrees(&root, &repo);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].slug, "task");
        fs::write(nested.join("pending"), "retain this").unwrap();
        assert!(
            remove_managed_worktree(&root, &repo, "task", false)
                .await
                .is_err()
        );
        assert!(nested.join("pending").is_file());
        remove_managed_worktree(&root, &repo, "task", true)
            .await
            .unwrap();
        assert!(!wt.path.exists());
        // The surviving branch must be safely reused after its worktree is removed.
        ensure_worktree(&root, &repo, "task", None).await.unwrap();
        remove_managed_worktree(&root, &repo, "task", false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn base_revisions_are_validated_even_when_reusing_existing_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let repo = init_repo(&temp.path().join("repo")).await;
        let wt = ensure_worktree(&root, &repo, "task", None).await.unwrap();
        for base in [
            "--detach",
            "-bmalicious",
            "--help",
            "",
            " main",
            "HEAD\n",
            "does-not-exist",
        ] {
            for slug in ["new", "task"] {
                assert!(
                    ensure_worktree(&root, &repo, slug, Some(base))
                        .await
                        .is_err()
                );
            }
        }
        assert!(!wt.path.parent().unwrap().join("new").exists());
        assert_eq!(
            git_stdout(&wt.path, &["branch", "--show-current"])
                .await
                .unwrap()
                .trim(),
            "codex/task"
        );
        assert!(
            ensure_worktree(&root, &repo, "commit", Some("HEAD^{commit}"))
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn unrelated_checkout_or_wrong_bucket_is_never_reused_or_force_removed() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let a = init_repo(&temp.path().join("a")).await;
        let b = init_repo(&temp.path().join("b")).await;
        let bucket = repo_bucket_dir(&root, &a);
        fs::create_dir(&bucket).unwrap();
        let unrelated = init_repo(&bucket.join("unrelated")).await;
        let wrong = bucket.join("wrong");
        git_stdout(
            &b,
            &[
                "worktree",
                "add",
                "-b",
                "codex/wrong",
                "--",
                wrong.to_str().unwrap(),
            ],
        )
        .await
        .unwrap();
        for slug in ["unrelated", "wrong"] {
            assert!(ensure_worktree(&root, &a, slug, None).await.is_err());
            assert!(
                remove_managed_worktree(&root, &a, slug, true)
                    .await
                    .is_err()
            );
            assert!(
                managed_worktree_info(&root, &bucket.join(slug))
                    .await
                    .is_err()
            );
            assert!(bucket.join(slug).is_dir());
        }
        assert!(unrelated.join(".git").is_dir());
        assert!(list_managed_worktrees(&root, &a).is_empty());
        assert!(scan_git_repos(&wrong, &root).is_empty());
    }

    #[tokio::test]
    async fn copied_metadata_wrong_branch_and_nested_repos_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let repo = init_repo(&temp.path().join("repo")).await;
        let wt = ensure_worktree(&root, &repo, "task", None).await.unwrap();
        let fake = wt.path.parent().unwrap().join("fake");
        fs::create_dir(&fake).unwrap();
        fs::copy(wt.path.join(".git"), fake.join(".git")).unwrap();
        assert!(managed_worktree_info(&root, &fake).await.is_err());
        assert!(
            remove_managed_worktree(&root, &repo, "fake", true)
                .await
                .is_err()
        );
        let nested = init_repo(&wt.path.join("nested")).await;
        assert!(managed_worktree_info(&root, &nested).await.is_err());
        git_stdout(&wt.path, &["checkout", "--detach"])
            .await
            .unwrap();
        assert!(ensure_worktree(&root, &repo, "task", None).await.is_err());
        assert!(
            remove_managed_worktree(&root, &repo, "task", true)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn mismatched_common_directory_or_git_worktree_override_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let repo = init_repo(&temp.path().join("repo")).await;
        let other = init_repo(&temp.path().join("other")).await;
        let wt = ensure_worktree(&root, &repo, "task", None).await.unwrap();
        let (_, _, gitdir) = linked_worktree_metadata(&wt.path).unwrap();
        let commondir = gitdir.join("commondir");
        let original = fs::read(&commondir).unwrap();
        fs::write(&commondir, other.join(".git").to_str().unwrap()).unwrap();
        assert!(managed_worktree_info(&root, &wt.path).await.is_err());
        assert!(
            remove_managed_worktree(&root, &repo, "task", true)
                .await
                .is_err()
        );
        fs::write(&commondir, original).unwrap();
        git_stdout(&repo, &["config", "core.worktree", other.to_str().unwrap()])
            .await
            .unwrap();
        assert!(ensure_worktree(&root, &repo, "new", None).await.is_err());
        assert!(
            remove_managed_worktree(&root, &repo, "task", true)
                .await
                .is_err()
        );
        assert!(wt.path.is_dir());
    }

    #[tokio::test]
    async fn inherited_git_selectors_cannot_override_authorized_repository() {
        const CHILD_ROOT: &str = "ANCHOR_BRIDGE_GIT_TEST_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            let root = PathBuf::from(root);
            let repo = root.join("repo");
            let wt = ensure_worktree(&root.join("managed"), &repo, "task", None)
                .await
                .unwrap();
            assert_eq!(wt.git_common_dir, repo.join(".git"));
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        root(&temp);
        init_repo(&temp.path().join("repo")).await;
        let other = init_repo(&temp.path().join("other")).await;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "git::tests::inherited_git_selectors_cannot_override_authorized_repository",
                "--nocapture",
            ])
            .env(CHILD_ROOT, temp.path())
            .env("GIT_DIR", other.join(".git"))
            .env("GIT_COMMON_DIR", other.join(".git"))
            .env("GIT_WORK_TREE", &other)
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.worktree")
            .env("GIT_CONFIG_VALUE_0", &other);
        let output = bounded_output(command, Duration::from_secs(15))
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            git_stdout(&other, &["for-each-ref", "refs/heads/codex"])
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_targets_buckets_and_git_directories_are_rejected() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let repo = init_repo(&temp.path().join("repo")).await;
        let wt = ensure_worktree(&root, &repo, "task", None).await.unwrap();
        let bucket = wt.path.parent().unwrap();
        for (slug, destination) in [
            ("alias", wt.path.clone()),
            ("dangling", temp.path().join("missing")),
        ] {
            symlink(destination, bucket.join(slug)).unwrap();
            assert!(ensure_worktree(&root, &repo, slug, None).await.is_err());
            assert!(
                remove_managed_worktree(&root, &repo, slug, true)
                    .await
                    .is_err()
            );
        }
        let alias = temp.path().join("alias");
        symlink(&wt.path, &alias).unwrap();
        assert!(managed_worktree_info(&root, &alias).await.is_err());
        assert_eq!(list_managed_worktrees(&root, &repo).len(), 1);
        let other = init_repo(&temp.path().join("other")).await;
        symlink(bucket, repo_bucket_dir(&root, &other)).unwrap();
        assert!(ensure_worktree(&root, &other, "new", None).await.is_err());
        assert!(
            remove_managed_worktree(&root, &other, "task", true)
                .await
                .is_err()
        );
        let original = repo.join(".git");
        let relocated = repo.join("relocated-git");
        fs::rename(&original, &relocated).unwrap();
        symlink(&relocated, &original).unwrap();
        assert!(ensure_worktree(&root, &repo, "new", None).await.is_err());
        assert!(
            remove_managed_worktree(&root, &repo, "task", true)
                .await
                .is_err()
        );
        assert!(wt.path.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn scanner_continues_after_unreadable_directory() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = root(&temp);
        let denied = temp.path().join("a-denied");
        let repo = temp.path().join("z-readable");
        fs::create_dir(&denied).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o0)).unwrap();
        let permission_error = fs::read_dir(&denied).is_err();
        let found = scan_git_repos(temp.path(), &root);
        fs::set_permissions(&denied, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(found, vec![repo]);
        if !permission_error {
            eprintln!(
                "permission-error branch not exercised: process bypasses filesystem permissions"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn timed_out_and_cancelled_subprocesses_are_killed() {
        let temp = tempfile::tempdir().unwrap();
        for cancel in [false, true] {
            let pidfile = temp
                .path()
                .join(if cancel { "cancel.pid" } else { "timeout.pid" });
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    "printf '%s' $$ > \"$1\"; exec sleep 30",
                    "bridge-test",
                ])
                .arg(&pidfile);
            let duration = if cancel {
                Duration::from_secs(60)
            } else {
                Duration::from_millis(500)
            };
            let task = tokio::spawn(bounded_output(command, duration));
            let pid = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if let Ok(pid) = fs::read_to_string(&pidfile)
                        && pid.parse::<u32>().is_ok()
                    {
                        break pid;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if cancel {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                assert!(task.await.unwrap().unwrap_err().contains("timed out"));
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while Path::new("/proc").join(&pid).exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("timed-out/cancelled child must be reaped");
        }
    }
}
