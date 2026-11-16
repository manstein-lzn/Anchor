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
                crate::host_fs::invocation_stem(&fact.key)
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
        execution_closure_digest: None,
        frozen_model_digest: None,
        input_guidance: None,
        context_checkpoint_digest: None,
        context_request_digest: None,
        session_id: None,
        completion: None,
        reason: None,
        tool_observation: Some(json!({"tool":"external","result":null})),
        conversation_scope: Some(scope_name(HINT).unwrap()),
        provider_failure: None,
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
fn next_invocation_can_change_model_but_recovery_keeps_its_selected_model() {
    let fixture = Fixture::new();
    let mut first = fact("first");
    first.session_id = Some("native-session".into());
    let mut scope = fixture.open(&first.key, None, &[]).unwrap();
    fixture.save(&first);
    scope.claim(&first).unwrap();
    drop(scope);
    let mut second = fact("second");
    second.model_binding = Some("new-model".into());
    second.session_id = first.session_id.clone();
    let open = |current: Option<&Fact>, model: &str| {
        ConversationScope::open(
            &fixture.process,
            HINT,
            &second.key,
            &fixture.facts,
            "binary",
            model,
            current,
            std::slice::from_ref(&first.key),
        )
    };
    let scope = open(None, "new-model").unwrap();
    assert_eq!(scope.session_id(), first.session_id);
    assert_eq!(scope.previous_observation, first.tool_observation);
    drop(scope);
    // A crash after the new invocation fact, before the scope index update,
    // preserves the new selection and the existing native history.
    fixture.save(&second);
    let mut recovered = open(Some(&second), "new-model").unwrap();
    recovered.claim(&second).unwrap();
    drop(recovered);
    assert!(open(Some(&second), "third-model").is_err());
    let recovered = open(Some(&second), "new-model").unwrap();
    assert_eq!(recovered.session_id(), first.session_id);
}

#[test]
fn reviewed_provider_patches_load_old_history_only_for_a_new_invocation() {
    for (old, new) in [
        (revision::BEFORE_REASONING_FIX, revision::WITH_REASONING_FIX),
        (revision::BEFORE_REASONING_FIX, revision::WITH_MULTICALL_FIX),
        (revision::WITH_REASONING_FIX, revision::WITH_MULTICALL_FIX),
        (
            revision::BEFORE_REASONING_FIX,
            revision::WITH_COMPLETION_FIX,
        ),
        (revision::WITH_REASONING_FIX, revision::WITH_COMPLETION_FIX),
        (revision::WITH_MULTICALL_FIX, revision::WITH_COMPLETION_FIX),
        (
            revision::BEFORE_REASONING_FIX,
            revision::WITH_PROVIDER_ERROR_FIX,
        ),
        (
            revision::WITH_REASONING_FIX,
            revision::WITH_PROVIDER_ERROR_FIX,
        ),
        (
            revision::WITH_MULTICALL_FIX,
            revision::WITH_PROVIDER_ERROR_FIX,
        ),
        (
            revision::WITH_COMPLETION_FIX,
            revision::WITH_PROVIDER_ERROR_FIX,
        ),
    ] {
        let fixture = Fixture::new();
        let mut first = fact("first");
        first.binary_sha256 = old.into();
        first.session_id = Some("retained-native-session".into());
        let open =
            |key: &InvocationKey, current: Option<&Fact>, previous: &[InvocationKey], binary| {
                ConversationScope::open(
                    &fixture.process,
                    HINT,
                    key,
                    &fixture.facts,
                    binary,
                    "model",
                    current,
                    previous,
                )
            };
        let mut scope = open(&first.key, None, &[], old).unwrap();
        fixture.save(&first);
        scope.claim(&first).unwrap();
        drop(scope);
        // A failed/unfinished invocation retains its exact executable identity.
        assert!(open(&first.key, Some(&first), &[], new).is_err());
        assert!(open(&key("second"), None, &[first.key.clone()], "unreviewed").is_err());
        let scope = open(&key("second"), None, &[first.key.clone()], new).unwrap();
        assert_eq!(scope.session_id(), first.session_id);
        assert_eq!(scope.previous_observation, first.tool_observation);
        drop(scope);
        let mut second = fact("second");
        second.binary_sha256 = new.into();
        second.session_id = first.session_id.clone();
        fixture.save(&second);
        // Crash after the new fact but before updating the scope index is recoverable.
        let mut scope = open(&second.key, Some(&second), &[first.key.clone()], new).unwrap();
        scope.claim(&second).unwrap();
        drop(scope);
        let scope = open(&second.key, Some(&second), &[first.key.clone()], new).unwrap();
        assert_eq!(scope.binding.binary_sha256, new);
        drop(scope);
        assert!(open(&key("third"), None, &[second.key.clone()], old).is_err());
        let scope = open(&key("third"), None, &[second.key], new).unwrap();
        assert_eq!(scope.session_id(), first.session_id);
    }
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
                crate::host_fs::invocation_stem(&first.key)
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
