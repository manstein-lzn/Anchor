use super::*;
use std::process::{Command, Stdio};
use workspace::PreparationLock;

const GIT_CONFIG: &str = "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n\tlogallrefupdates = false\n";
const GIT_IDENTITY: &str = "Anchor Snapshot <snapshot@anchor.invalid> 946684800 +0000";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Projection {
    format: u32,
    artifact: CommitRef,
    manifest_sha256: String,
    head: String,
    git_files: BTreeMap<String, (String, u64)>,
}

struct GitView {
    files: PathBuf,
    projection: Projection,
}

impl HostArtifacts {
    /// A disposable, verified projection for legacy Graphs that read a Git
    /// identity. The immutable files and fs2 manifest remain authoritative.
    pub(super) fn git_projection(
        &self,
        snapshot: &Path,
        manifest: &Manifest,
    ) -> Result<PathBuf, GraphError> {
        let mut chain = vec![(snapshot.to_path_buf(), manifest.clone())];
        loop {
            let current = &chain.last().unwrap().1;
            let inputs = current
                .context
                .as_ref()
                .map(|context| context.input_commits.as_slice())
                .unwrap_or_default();
            let previous = self
                .expanded_snapshots(
                    inputs,
                    Some((&current.key.run_id, &current.key.graph_digest)),
                )?
                .into_iter()
                .find(|(_, previous)| previous.key.node_id == current.key.node_id);
            let Some(previous) = previous else { break };
            if previous.1.key.invocation >= current.key.invocation {
                return Err(corrupt("Git ancestor must be an earlier node invocation"));
            }
            chain.push(previous);
        }
        let mut parent: Option<GitView> = None;
        for (snapshot, manifest) in chain.into_iter().rev() {
            parent = Some(self.prepare_git_view(&snapshot, &manifest, parent.as_ref())?);
        }
        Ok(parent.unwrap().files)
    }

    fn prepare_git_view(
        &self,
        snapshot: &Path,
        manifest: &Manifest,
        parent: Option<&GitView>,
    ) -> Result<GitView, GraphError> {
        let commit = if manifest.format == 1 {
            legacy_commit_for(&manifest.key)
        } else {
            commit_for(&manifest.key)
        };
        let manifest_sha256 = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(manifest).map_err(|error| corrupt(error.to_string()))?
            )
        );
        if manifest
            .files
            .keys()
            .any(|path| path.split('/').next() == Some(".git"))
            || manifest
                .directories
                .iter()
                .any(|path| path.split('/').next() == Some(".git"))
        {
            return Err(corrupt("artifact .git conflicts with host Git metadata"));
        }
        let published = snapshot.join("git-view");
        let _lock = PreparationLock::acquire(&snapshot.join(".git-view.lock"))?;
        if fs::symlink_metadata(&published).is_ok() {
            return verify_view(&published, manifest, &commit, &manifest_sha256, parent);
        }
        let temporary = self.root.join(format!(
            ".{}.{}.{}.git-view.tmp",
            commit.id,
            std::process::id(),
            TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&temporary)?;
        let result = (|| {
            let files_dir = temporary.join("files");
            let (files, directories) = scan_tree(&snapshot.join("files"), Some(&files_dir))?;
            if files != manifest.files || directories != manifest.directories {
                return Err(corrupt("artifact changed while building its Git view"));
            }
            let git_dir = files_dir.join(".git");
            let template = temporary.join("template");
            fs::create_dir(&template)?;
            let mut init = git_command(None);
            init.arg("init")
                .args(["--bare", "--quiet", "--object-format=sha1"])
                .arg(format!("--template={}", template.display()))
                .arg(&git_dir);
            git_output(init, None)?;
            fs::remove_dir(&template)?;
            fs::write(git_dir.join("config"), GIT_CONFIG)?;
            if let Some(parent) = parent {
                fs::remove_dir_all(git_dir.join("objects"))?;
                scan_tree(
                    &parent.files.join(".git/objects"),
                    Some(&git_dir.join("objects")),
                )?;
            }
            let mut tree = Tree::default();
            for path in manifest.files.keys() {
                let mut hash = git_command(Some(&git_dir));
                hash.args(["hash-object", "-w", "--no-filters", "--"])
                    .arg(files_dir.join(path));
                tree.insert(path, oid(git_output(hash, None)?)?)?;
            }
            let tree = tree.write(&git_dir)?;
            let message = commit_message(&commit, &manifest_sha256);
            let mut create = git_command(Some(&git_dir));
            create.args(["commit-tree", &tree, "--no-gpg-sign"]);
            if let Some(parent) = parent {
                create.args(["-p", &parent.projection.head]);
            }
            let head = oid(git_output(create, Some(message.as_bytes()))?)?;
            let mut update = git_command(Some(&git_dir));
            update.args(["update-ref", "--no-deref", "HEAD", &head]);
            git_output(update, None)?;
            let projection = Projection {
                format: 1,
                artifact: commit.clone(),
                manifest_sha256: manifest_sha256.clone(),
                head,
                git_files: scan_hash_tree(&git_dir)?,
            };
            write_json_atomic(&temporary.join("projection.json"), &projection)?;
            sync_tree_files(&temporary)?;
            let mut verified =
                verify_view(&temporary, manifest, &commit, &manifest_sha256, parent)?;
            fs::rename(&temporary, &published)?;
            fs::File::open(snapshot)?.sync_all()?;
            fs::File::open(&self.root)?.sync_all()?;
            verified.files = published.join("files");
            Ok(verified)
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }
}

fn commit_message(commit: &CommitRef, manifest_sha256: &str) -> String {
    format!(
        "Anchor Artifact {}\nArtifact-Node: {}\nArtifact-Invocation: {}\nManifest-SHA256: {manifest_sha256}\n",
        commit.id, commit.node_id, commit.invocation
    )
}

fn verify_view(
    view: &Path,
    manifest: &Manifest,
    commit: &CommitRef,
    manifest_sha256: &str,
    parent: Option<&GitView>,
) -> Result<GitView, GraphError> {
    require_directory(view)?;
    let mut entries = fs::read_dir(view)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort();
    if entries
        != [
            std::ffi::OsString::from("files"),
            std::ffi::OsString::from("projection.json"),
        ]
    {
        return Err(corrupt("Git view contains undeclared resources"));
    }
    let projection_path = view.join("projection.json");
    require_file(&projection_path)?;
    let projection: Projection = serde_json::from_slice(&fs::read(projection_path)?)
        .map_err(|error| corrupt(format!("Git projection is unreadable: {error}")))?;
    if projection.format != 1
        || projection.artifact != *commit
        || projection.manifest_sha256 != manifest_sha256
        || !valid_oid(&projection.head)
    {
        return Err(corrupt("Git projection belongs to another artifact"));
    }
    let files_dir = view.join("files");
    let (files, directories) = scan_workspace(&files_dir, None)?;
    if files != manifest.files || directories != manifest.directories {
        return Err(corrupt("Git view files do not match the artifact manifest"));
    }
    let git_dir = files_dir.join(".git");
    let (git_files, git_directories) = scan_tree(&git_dir, None)?;
    if git_files
        .keys()
        .any(|path| path != "HEAD" && path != "config" && !object_path(path))
        || git_directories.iter().any(|path| !git_directory(path))
        || fs::read(git_dir.join("config"))? != GIT_CONFIG.as_bytes()
        || fs::read(git_dir.join("HEAD"))? != format!("{}\n", projection.head).as_bytes()
        || scan_hash_tree(&git_dir)? != projection.git_files
    {
        return Err(corrupt("Git projection metadata is unsafe or changed"));
    }
    let mut listing = git_command(Some(&git_dir));
    listing.args(["ls-tree", "-rz", "HEAD"]);
    let mut observed = BTreeMap::new();
    for entry in git_output(listing, None)?
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let entry =
            std::str::from_utf8(entry).map_err(|_| corrupt("Git tree paths must be UTF-8"))?;
        let (metadata, path) = entry
            .split_once('\t')
            .ok_or_else(|| corrupt("malformed Git tree"))?;
        let blob = metadata
            .strip_prefix("100644 blob ")
            .filter(|blob| valid_oid(blob))
            .ok_or_else(|| corrupt("unexpected Git tree object"))?;
        validate_file_relative(path)?;
        if observed
            .insert(path.to_owned(), hash_blob(&git_dir, blob)?)
            .is_some()
        {
            return Err(corrupt("duplicate Git tree path"));
        }
    }
    if observed != manifest.files {
        return Err(corrupt(
            "Git commit files do not match the artifact manifest",
        ));
    }
    let mut tree = git_command(Some(&git_dir));
    tree.args(["rev-parse", "HEAD^{tree}"]);
    let tree = oid(git_output(tree, None)?)?;
    let mut expected = format!("tree {tree}\n");
    if let Some(parent) = parent {
        expected.push_str(&format!("parent {}\n", parent.projection.head));
    }
    expected.push_str(&format!(
        "author {GIT_IDENTITY}\ncommitter {GIT_IDENTITY}\n\n{}",
        commit_message(commit, manifest_sha256)
    ));
    let mut show = git_command(Some(&git_dir));
    show.args(["cat-file", "commit", "HEAD"]);
    if git_output(show, None)? != expected.as_bytes() {
        return Err(corrupt(
            "Git commit does not bind its artifact manifest and ancestry",
        ));
    }
    let mut check = git_command(Some(&git_dir));
    check.args([
        "-c",
        "fsck.hasDotgit=ignore",
        "fsck",
        "--full",
        "--strict",
        "--no-reflogs",
    ]);
    if !git_output(check, None)?.is_empty() {
        return Err(corrupt("Git projection contains unreachable objects"));
    }
    Ok(GitView {
        files: files_dir,
        projection,
    })
}

fn object_path(path: &str) -> bool {
    path.strip_prefix("objects/").is_some_and(|path| {
        path.split_once('/').is_some_and(|(directory, name)| {
            directory.len() == 2 && name.len() == 38 && valid_oid(&format!("{directory}{name}"))
        })
    })
}

fn git_directory(path: &str) -> bool {
    matches!(
        path,
        "objects" | "objects/info" | "objects/pack" | "refs" | "refs/heads" | "refs/tags"
    ) || path.strip_prefix("objects/").is_some_and(|path| {
        path.len() == 2
            && path
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[derive(Default)]
struct Tree {
    files: BTreeMap<String, String>,
    directories: BTreeMap<String, Tree>,
}

impl Tree {
    fn insert(&mut self, path: &str, blob: String) -> Result<(), GraphError> {
        if let Some((directory, rest)) = path.split_once('/') {
            if self.files.contains_key(directory) {
                return Err(corrupt("conflicting Git paths"));
            }
            self.directories
                .entry(directory.to_owned())
                .or_default()
                .insert(rest, blob)
        } else if self.directories.contains_key(path)
            || self.files.insert(path.to_owned(), blob).is_some()
        {
            Err(corrupt("conflicting Git paths"))
        } else {
            Ok(())
        }
    }

    fn write(&self, git_dir: &Path) -> Result<String, GraphError> {
        let mut entries = Vec::new();
        for (name, blob) in &self.files {
            entries.extend_from_slice(format!("100644 blob {blob}\t{name}\0").as_bytes());
        }
        for (name, tree) in &self.directories {
            entries.extend_from_slice(
                format!("040000 tree {}\t{name}\0", tree.write(git_dir)?).as_bytes(),
            );
        }
        let mut write = git_command(Some(git_dir));
        write.args(["mktree", "-z"]);
        oid(git_output(write, Some(&entries))?)
    }
}

fn git_command(git_dir: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    command.env_clear();
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_AUTHOR_NAME", "Anchor Snapshot")
        .env("GIT_AUTHOR_EMAIL", "snapshot@anchor.invalid")
        .env("GIT_AUTHOR_DATE", "@946684800 +0000")
        .env("GIT_COMMITTER_NAME", "Anchor Snapshot")
        .env("GIT_COMMITTER_EMAIL", "snapshot@anchor.invalid")
        .env("GIT_COMMITTER_DATE", "@946684800 +0000")
        .env("LC_ALL", "C")
        .args([
            "--no-optional-locks",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "commit.gpgSign=false",
        ]);
    if let Some(git_dir) = git_dir {
        command.arg("--git-dir").arg(git_dir);
    }
    command
}

fn git_output(mut command: Command, input: Option<&[u8]>) -> Result<Vec<u8>, GraphError> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        GraphError::Unsupported(format!("Git snapshot projection requires git: {error}"))
    })?;
    let written = input
        .map(|input| child.stdin.take().unwrap().write_all(input))
        .transpose();
    let output = child.wait_with_output()?;
    written?;
    if !output.status.success() {
        return Err(corrupt(format!(
            "Git snapshot projection failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn oid(bytes: Vec<u8>) -> Result<String, GraphError> {
    let value =
        String::from_utf8(bytes).map_err(|_| corrupt("Git object identity must be ASCII"))?;
    let value = value.trim();
    if !valid_oid(value) {
        return Err(corrupt("invalid Git object identity"));
    }
    Ok(value.to_owned())
}

fn valid_oid(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn hash_blob(git_dir: &Path, blob: &str) -> Result<FileHash, GraphError> {
    let mut command = git_command(Some(git_dir));
    command
        .args(["cat-file", "blob", blob])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let mut source = child.stdout.take().unwrap();
    let mut buffer = vec![0_u8; FILE_BUFFER_BYTES];
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    loop {
        let count = match source.read(&mut buffer) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes = bytes
            .checked_add(count as u64)
            .ok_or_else(|| corrupt("Git blob size overflow"))?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(corrupt("Git blob is unreadable"));
    }
    Ok(FileHash {
        sha256: format!("{:x}", hash.finalize()),
        bytes,
    })
}

fn sync_tree_files(root: &Path) -> Result<(), GraphError> {
    checked_path(root)?;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree_files(&entry.path())?;
        } else {
            require_file(&entry.path())?;
            fs::File::open(entry.path())?.sync_all()?;
        }
    }
    fs::File::open(root)?.sync_all()?;
    Ok(())
}
