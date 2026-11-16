use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    snapshot: PathBuf,
    commit: CommitRef,
    filename: &'static str,
    bytes: Vec<u8>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

impl Fixture {
    fn new(kind: ArtifactKind) -> Self {
        let root = std::env::temp_dir().join(format!(
            "anchor-control-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        fs::File::create(root.join(".store.lock")).unwrap();
        let key = InvocationKey {
            run_id: "run-control".into(),
            graph_digest: "graph-control".into(),
            node_id: "control".into(),
            invocation: 1,
        };
        let commit = commit_for(&key);
        let snapshot = root.join(&commit.id);
        fs::create_dir_all(snapshot.join("view")).unwrap();
        // These are already-published preserve_order bytes. A reader built
        // without that feature must verify, not regenerate, their identity.
        let bytes =
            br#"{"z":3,"branches":[{"nodes":[{"z":2,"node":"source","a":1}]}],"a":1}"#.to_vec();
        let mut output: Value = serde_json::from_slice(&bytes).unwrap();
        if kind == ArtifactKind::Join {
            output["branches"][0]["nodes"][0]["files"] = serde_json::json!([]);
        }
        let context = ArtifactFreezeContext {
            kind,
            input_commits: vec![],
        };
        let filename = control_filename(kind).unwrap();
        let manifest = Manifest {
            format: 3,
            key,
            completion: NodeCompletion {
                submission: String::new(),
                route: None,
                model_requests: 0,
                output,
            },
            files: BTreeMap::from([(
                filename.into(),
                FileHash {
                    sha256: format!("{:x}", Sha256::digest(&bytes)),
                    bytes: bytes.len() as u64,
                    executable: false,
                },
            )]),
            directories: vec![],
            context_sha256: Some(context_hash(&context).unwrap()),
            context: Some(context),
        };
        fs::write(snapshot.join("view").join(filename), &bytes).unwrap();
        let fixture = Self {
            root,
            snapshot,
            commit,
            filename,
            bytes,
        };
        fixture.manifest(&manifest);
        fixture
    }

    fn manifest(&self, manifest: &Manifest) {
        fs::write(
            self.snapshot.join("manifest.json"),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            self.snapshot
                .join("view")
                .join(super::super::ARTIFACT_DESCRIPTOR_FILE),
            serde_json::to_vec(&manifest.descriptor(&self.snapshot).unwrap()).unwrap(),
        )
        .unwrap();
    }

    fn readable(&self) -> bool {
        ArtifactStoreReader::open(&self.root)
            .unwrap()
            .read_view(&self.commit)
            .is_ok()
    }

    fn traversable(&self) -> bool {
        expanded_snapshots(&self.root, std::slice::from_ref(&self.commit), None).is_ok()
    }
}

#[test]
fn published_control_bytes_keep_their_identity_across_json_ordering_and_reject_tampering() {
    for kind in [
        ArtifactKind::Fanout,
        ArtifactKind::Join,
        ArtifactKind::Interruption,
    ] {
        let fixture = Fixture::new(kind);
        let original_manifest = fs::read(fixture.snapshot.join("manifest.json")).unwrap();
        assert!(fixture.readable());
        assert!(fixture.traversable());
        assert_eq!(
            fs::read(fixture.snapshot.join("manifest.json")).unwrap(),
            original_manifest
        );
        assert_eq!(
            fs::read(fixture.snapshot.join("view").join(fixture.filename)).unwrap(),
            fixture.bytes
        );

        let changed = String::from_utf8(fixture.bytes.clone())
            .unwrap()
            .replace("\"z\":3", "\"z\":4");
        fs::write(
            fixture.snapshot.join("view").join(fixture.filename),
            changed,
        )
        .unwrap();
        assert!(!fixture.readable());
        assert!(!fixture.traversable());

        fs::write(
            fixture.snapshot.join("view").join(fixture.filename),
            &fixture.bytes,
        )
        .unwrap();
        let (_, mut manifest) = read_snapshot(&fixture.root, &fixture.commit).unwrap();
        manifest.completion.output["z"] = serde_json::json!(9);
        fixture.manifest(&manifest);
        assert!(!fixture.readable());
        assert!(!fixture.traversable());
    }
}

#[test]
fn collected_control_metadata_stays_readable_without_claiming_content_is_available() {
    let fixture = Fixture::new(ArtifactKind::Fanout);
    fs::write(fixture.snapshot.join("expired.json"), "{}").unwrap();
    fs::remove_dir_all(fixture.snapshot.join("view")).unwrap();
    let (_, manifest) = read_snapshot(&fixture.root, &fixture.commit).unwrap();
    assert_eq!(manifest.key.node_id, "control");
    assert!(!fixture.readable());
    assert!(!fixture.traversable());
}
