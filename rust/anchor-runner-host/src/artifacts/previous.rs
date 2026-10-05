//! Frozen conversation input, including unfinished work; never a completion.
use super::*;

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PreviousInput {
    key: InvocationKey,
    source: InvocationKey,
    committed: Option<CommitRef>,
    files: BTreeMap<String, FileHash>,
    directories: Vec<String>,
}

impl HostArtifacts {
    pub(crate) fn previous_input(
        &self,
        key: &InvocationKey,
        source: &InvocationKey,
        committed: Option<&CommitRef>,
    ) -> Result<ReadOnlyInput, GraphError> {
        validate_key(key)?;
        validate_key(source)?;
        if key.run_id == source.run_id || key.node_id != source.node_id {
            return Err(corrupt("previous input must be another Run of this node"));
        }
        // The workspace root already owns all per-Run disposable inputs. This
        // sibling stays outside the Agent's writable invocation directory.
        let parent = self
            .workspace_root
            .join(&key.run_id)
            .join("previous-inputs");
        checked_path(&parent)?;
        fs::create_dir_all(&parent)?;
        let _lock =
            workspace::PreparationLock::acquire(&parent.join(format!(".{}.lock", key_hash(key))))?;
        let bundle = parent.join(key_hash(key));
        let manifest = bundle.join("input.json");
        let files = bundle.join("files");
        if bundle.exists() {
            require_file(&manifest)?;
            let saved: PreviousInput = serde_json::from_slice(&fs::read(&manifest)?)
                .map_err(|error| corrupt(error.to_string()))?;
            let (actual_files, actual_directories) = scan_tree(&files, None)?;
            if saved.key != *key
                || saved.source != *source
                || saved.committed.as_ref() != committed
                || saved.files != actual_files
                || saved.directories != actual_directories
            {
                return Err(corrupt("frozen previous input identity or content changed"));
            }
            return Ok(ReadOnlyInput {
                source: files,
                destination: "/previous".into(),
            });
        }
        let source_path = if let Some(commit) = committed {
            let (path, saved) = self.read_snapshot(commit)?;
            if saved.key != *source {
                return Err(corrupt("previous commit belongs to another invocation"));
            }
            path.join("files")
        } else {
            self.workspace_path(source)?
        };
        let temporary = parent.join(format!(".{}.tmp", key_hash(key)));
        // The preparation lock owns this invocation. A process may have died
        // before publishing this private stage; it is never an accepted input.
        if temporary.exists() {
            require_directory(&temporary)?;
            fs::remove_dir_all(&temporary)?;
        }
        fs::create_dir(&temporary)?;
        let result = (|| {
            let (copied, directories) =
                scan_workspace(&source_path, Some(&temporary.join("files")))?;
            let (after, after_directories) = scan_workspace(&source_path, None)?;
            if copied != after || directories != after_directories {
                return Err(corrupt("previous workspace changed during handoff"));
            }
            let frozen = PreviousInput {
                key: key.clone(),
                source: source.clone(),
                committed: committed.cloned(),
                files: copied,
                directories,
            };
            fs::write(
                temporary.join("input.json"),
                serde_json::to_vec(&frozen).map_err(|error| corrupt(error.to_string()))?,
            )?;
            fs::File::open(temporary.join("input.json"))?.sync_all()?;
            fs::File::open(&temporary)?.sync_all()?;
            fs::rename(&temporary, &bundle)?;
            for ancestor in parent.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                fs::File::open(ancestor)?.sync_all()?;
            }
            Ok(ReadOnlyInput {
                source: files,
                destination: "/previous".into(),
            })
        })();
        if temporary.exists() {
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(run: &str) -> InvocationKey {
        InvocationKey {
            run_id: run.into(),
            graph_digest: "graph-digest".into(),
            node_id: "module/assistant".into(),
            invocation: 1,
        }
    }

    #[test]
    fn unfinished_files_are_frozen_without_publishing_a_completion() {
        let root = tempfile::tempdir().unwrap();
        let artifacts = HostArtifacts::new(root.path().join("artifacts"), root.path().join("work"));
        let old = key("prior");
        let current = key("current");
        let source = artifacts.workspace_path(&old).unwrap();
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("draft.md"), "unfinished work").unwrap();
        let abandoned = root
            .path()
            .join("work/current/previous-inputs")
            .join(format!(".{}.tmp", key_hash(&current)));
        fs::create_dir_all(abandoned.join("files")).unwrap();
        fs::write(abandoned.join("files/draft.md"), "half-copied").unwrap();
        let mount = artifacts.previous_input(&current, &old, None).unwrap();
        assert_eq!(mount.destination, Path::new("/previous"));
        assert_eq!(
            fs::read_to_string(mount.source.join("draft.md")).unwrap(),
            "unfinished work"
        );
        assert!(!root.path().join("artifacts").exists());
        fs::write(source.join("draft.md"), "later local edit").unwrap();
        let again = artifacts.previous_input(&current, &old, None).unwrap();
        assert_eq!(
            fs::read_to_string(again.source.join("draft.md")).unwrap(),
            "unfinished work"
        );
        fs::write(again.source.join("draft.md"), "corrupted frozen input").unwrap();
        assert!(artifacts.previous_input(&current, &old, None).is_err());
    }

    #[test]
    fn previous_inputs_reject_symlinks_and_wrong_node_or_run() {
        let root = tempfile::tempdir().unwrap();
        let artifacts = HostArtifacts::new(root.path().join("artifacts"), root.path().join("work"));
        let old = key("prior");
        let current = key("current");
        let source = artifacts.workspace_path(&old).unwrap();
        fs::create_dir_all(&source).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", source.join("escape")).unwrap();
        assert!(artifacts.previous_input(&current, &old, None).is_err());
        assert!(artifacts.previous_input(&old, &old, None).is_err());
        assert!(
            artifacts
                .previous_input(
                    &current,
                    &InvocationKey {
                        node_id: "other".into(),
                        ..old
                    },
                    None
                )
                .is_err()
        );
    }
}
