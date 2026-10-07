use super::*;
use serde_json::json;
use std::os::unix::fs::symlink;

const HINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Fixture {
    _root: tempfile::TempDir,
    process: PathBuf,
    facts: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let process = root.path().join("process");
        let facts = root.path().join("goose-acp");
        fs::create_dir_all(&facts).unwrap();
        Self {
            _root: root,
            process,
            facts,
        }
    }

    fn open(
        &self,
        key: &InvocationKey,
        current: Option<&Fact>,
        previous: &[InvocationKey],
    ) -> Result<ConversationScope, GraphError> {
        ConversationScope::open(
            &self.process,
            HINT,
            key,
            &self.facts,
            "binary",
            "model",
            current,
            previous,
        )
    }

    fn save(&self, fact: &Fact) {
        super::super::store_fact(
            &self.facts.join(format!(
                "{}.json",
                super::super::GooseNodePort::stem(&fact.key)
            )),
            fact,
        )
        .unwrap();
    }
}

fn key(run: &str) -> InvocationKey {
    InvocationKey {
        run_id: run.into(),
        graph_digest: "graph".into(),
        node_id: "worker".into(),
        invocation: 1,
    }
}

fn fact(run: &str) -> Fact {
    Fact {
        version: 2,
        key: key(run),
        binary_sha256: "binary".into(),
        model_binding: Some("model".into()),
        session_id: None,
        completion: None,
        reason: None,
        tool_observation: Some(json!({"tool":"external","result":null})),
        conversation_scope: Some(scope_name(HINT).unwrap()),
    }
}

#[test]
fn durable_fact_before_claim_and_session_before_index_are_recoverable() {
    let fixture = Fixture::new();
    let scope = fixture.open(&key("first"), None, &[]).unwrap();
    drop(scope);
    let mut first = fact("first");
    fixture.save(&first);
    let mut scope = fixture.open(&first.key, Some(&first), &[]).unwrap();
    scope.claim(&first).unwrap();
    first.session_id = Some("native-session".into());
    fixture.save(&first);
    drop(scope);
    let mut resumed = fixture.open(&first.key, Some(&first), &[]).unwrap();
    assert_eq!(resumed.session_id().as_deref(), Some("native-session"));
    resumed.claim(&first).unwrap();
    drop(resumed);
    let mut next = fixture
        .open(&key("second"), None, &[first.key.clone()])
        .unwrap();
    assert_eq!(next.session_id().as_deref(), Some("native-session"));
    assert_eq!(next.previous_observation, first.tool_observation);
    let mut second = fact("second");
    second.session_id = next.session_id();
    fixture.save(&second);
    next.claim(&second).unwrap();
}

#[test]
fn unfinished_predecessor_session_saved_before_index_is_not_replaced() {
    let fixture = Fixture::new();
    let mut first = fact("first");
    let mut scope = fixture.open(&first.key, None, &[]).unwrap();
    fixture.save(&first);
    scope.claim(&first).unwrap();
    first.session_id = Some("saved-before-index".into());
    fixture.save(&first);
    drop(scope);
    let next = fixture.open(&key("second"), None, &[first.key]).unwrap();
    assert_eq!(next.session_id().as_deref(), Some("saved-before-index"));
}

#[test]
fn index_claim_without_fact_and_missing_binding_fail_closed() {
    let fixture = Fixture::new();
    let first = fact("first");
    let mut scope = fixture.open(&first.key, None, &[]).unwrap();
    scope.claim(&first).unwrap();
    let root = scope.root.clone();
    drop(scope);
    assert!(fixture.open(&first.key, None, &[]).is_err());
    fixture.save(&first);
    fs::remove_file(root.join("scope.json")).unwrap();
    assert!(fixture.open(&first.key, Some(&first), &[]).is_err());
}

#[test]
fn latest_native_fact_may_be_an_authorized_background_continuation_not_chain_head() {
    let fixture = Fixture::new();
    let mut background = fact("background");
    background.session_id = Some("native-session".into());
    let mut scope = fixture.open(&background.key, None, &[]).unwrap();
    fixture.save(&background);
    scope.claim(&background).unwrap();
    drop(scope);
    let mut foreground = fact("foreground");
    foreground.session_id = background.session_id.clone();
    let mut scope = fixture
        .open(&foreground.key, None, std::slice::from_ref(&background.key))
        .unwrap();
    fixture.save(&foreground);
    scope.claim(&foreground).unwrap();
    drop(scope);
    assert!(
        fixture
            .open(&background.key, Some(&background), &[])
            .is_err()
    );
    let mut scope = fixture
        .open(
            &background.key,
            Some(&background),
            std::slice::from_ref(&foreground.key),
        )
        .unwrap();
    assert_eq!(scope.session_id().as_deref(), Some("native-session"));
    scope.claim(&background).unwrap();
    drop(scope);
    let scope = fixture
        .open(&key("next"), None, &[foreground.key, background.key])
        .unwrap();
    assert_eq!(scope.session_id().as_deref(), Some("native-session"));
}

#[test]
fn identity_model_session_and_untrusted_predecessor_changes_are_refused() {
    let fixture = Fixture::new();
    let mut first = fact("first");
    let mut scope = fixture.open(&first.key, None, &[]).unwrap();
    first.session_id = Some("native".into());
    fixture.save(&first);
    scope.claim(&first).unwrap();
    drop(scope);
    assert!(fixture.open(&key("second"), None, &[]).is_err());
    for field in ["model", "binary", "session", "scope", "key"] {
        let mut changed = fact("first");
        changed.session_id = first.session_id.clone();
        match field {
            "model" => changed.model_binding = Some("different".into()),
            "binary" => changed.binary_sha256 = "different".into(),
            "session" => changed.session_id = Some("different".into()),
            "scope" => changed.conversation_scope = Some(scope_name(OTHER).unwrap()),
            "key" => changed.key.node_id = "other".into(),
            _ => unreachable!(),
        }
        super::super::store_fact(
            &fixture.facts.join(format!(
                "{}.json",
                super::super::GooseNodePort::stem(&first.key)
            )),
            &changed,
        )
        .unwrap();
        assert!(
            fixture
                .open(&key("second"), None, &[first.key.clone()])
                .is_err(),
            "{field}"
        );
        fixture.save(&first);
    }
}

#[test]
fn lease_serializes_execution_and_cleanup_and_keeps_other_scope() {
    let fixture = Fixture::new();
    let scope = fixture.open(&key("first"), None, &[]).unwrap();
    assert!(fixture.open(&key("first"), None, &[]).is_err());
    assert!(remove(&fixture.process, HINT).is_err());
    let other = ConversationScope::open(
        &fixture.process,
        OTHER,
        &key("other"),
        &fixture.facts,
        "binary",
        "model",
        None,
        &[],
    )
    .unwrap();
    let first_root = scope.root.clone();
    let other_root = other.root.clone();
    drop(scope);
    remove(&fixture.process, HINT).unwrap();
    assert!(!first_root.exists());
    assert!(other_root.exists());
    assert!(
        fixture
            .process
            .join("conversation-locks")
            .join(format!("{}.lock", scope_name(HINT).unwrap()))
            .is_file()
    );
}

#[test]
fn symlink_binding_temporary_lease_and_ancestor_are_refused() {
    for target in ["binding", "temporary", "lease", "ancestor"] {
        let fixture = Fixture::new();
        let scope = fixture.open(&key("first"), None, &[]).unwrap();
        let root = scope.root.clone();
        drop(scope);
        let protected = fixture._root.path().join("protected");
        fs::write(&protected, "untouched").unwrap();
        let path = match target {
            "binding" => root.join("scope.json"),
            "temporary" => root.join("scope.json.tmp"),
            "lease" => fixture
                .process
                .join("conversation-locks")
                .join(format!("{}.lock", scope_name(HINT).unwrap())),
            "ancestor" => fixture.process.join("conversations"),
            _ => unreachable!(),
        };
        if path.is_dir() {
            fs::rename(&path, path.with_extension("retained")).unwrap();
        } else if path.exists() {
            fs::remove_file(&path).unwrap();
        }
        symlink(&protected, &path).unwrap();
        assert!(fixture.open(&key("first"), None, &[]).is_err(), "{target}");
        assert_eq!(fs::read(&protected).unwrap(), b"untouched");
    }
}
