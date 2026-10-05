//! Small durable locator for io-harness-native conversations.
//!
//! Session rows, turn trees and resumable runs remain owned by io-harness. This
//! module stores only the conversation key -> Session id pointer and the exact
//! native run/turn associated with one Anchor invocation.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::Ordering;

use anchor_runtime_rig::graph::{InvocationKey, NodeExecutionRequest};
use io_harness::{Session, Store};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Locator {
    pub scope: String,
    pub invocation: String,
    pub session_id: i64,
    pub run_id: i64,
    pub turn_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Admission {
    pub invocation: String,
    pub session_id: i64,
    pub parent_turn_id: Option<i64>,
    pub prompt: String,
    pub runs_before: Vec<i64>,
}

pub(crate) struct ConversationLease {
    _file: File,
}

impl Drop for ConversationLease {
    fn drop(&mut self) {
        let _ = self._file.unlock();
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ConversationPaths {
    pub scope: String,
    scope_dir: PathBuf,
    store_path: PathBuf,
    root_path: PathBuf,
    invocation_path: PathBuf,
    admission_path: PathBuf,
    locator_path: PathBuf,
}

impl ConversationPaths {
    pub fn new(io_root: &Path, hint: &str, key: &InvocationKey) -> Self {
        let scope = format!("nc1-{:x}", Sha256::digest(hint.as_bytes()));
        Self::for_invocation(io_root, &scope, &key.durable_key())
    }

    pub fn for_invocation(io_root: &Path, scope: &str, durable_key: &str) -> Self {
        let invocation = format!("np1-{:x}", Sha256::digest(durable_key.as_bytes()));
        let scope_dir = io_root.join("conversations").join(scope);
        Self {
            scope: scope.to_owned(),
            store_path: scope_dir.join("framework.sqlite3"),
            root_path: io_root.join("conversation-roots").join(scope),
            invocation_path: io_root.join(format!("{invocation}.conversation.json")),
            admission_path: scope_dir
                .join("invocations")
                .join(format!("{invocation}.admission.json")),
            locator_path: scope_dir
                .join("invocations")
                .join(format!("{invocation}.json")),
            scope_dir,
        }
    }

    pub fn store_path(&self) -> &Path {
        &self.store_path
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn read_pending(&self) -> Result<Option<Admission>, String> {
        read_json(&self.scope_dir.join("pending.json"))?
            .map(parse_admission)
            .transpose()
    }

    pub fn publish_admission(&self, admission: &Admission) -> Result<(), String> {
        self.write_admission(admission)?;
        if let Some(locator) = self.read_locator()? {
            self.write_locator(&locator)?;
        } else {
            write_json_atomic(
                &self.invocation_path,
                &json!({
                    "version": 1,
                    "scope": self.scope,
                    "invocation": admission.invocation,
                    "session_id": admission.session_id,
                }),
            )?;
        }
        write_json_atomic(
            &self.scope_dir.join("pending.json"),
            &admission_value(admission),
        )
    }

    pub fn acquire(&self, io_root: &Path) -> Result<ConversationLease, String> {
        fs::create_dir_all(io_root.join("conversation-locks"))
            .map_err(|error| error.to_string())?;
        fs::create_dir_all(self.scope_dir.parent().expect("scope directory has parent"))
            .map_err(|error| error.to_string())?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(
                io_root
                    .join("conversation-locks")
                    .join(format!("{}.lock", self.scope)),
            )
            .map_err(|error| error.to_string())?;
        lock.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => {
                format!("native conversation {} is already active", self.scope)
            }
            std::fs::TryLockError::Error(error) => error.to_string(),
        })?;
        Ok(ConversationLease { _file: lock })
    }

    pub fn open_or_create(&self) -> Result<(Store, Session), String> {
        if !self.scope_dir.exists() {
            self.create_staged()?;
        }
        let session_id = read_session_id(&self.scope_dir.join("session.json"))?;
        let store = Store::open(&self.store_path).map_err(|error| error.to_string())?;
        let session = Session::reopen(&store, session_id).map_err(|error| error.to_string())?;
        if session.root() != self.root_path() {
            return Err("native conversation root does not match its key-derived root".into());
        }
        Ok((store, session))
    }

    pub fn read_locator(&self) -> Result<Option<Locator>, String> {
        read_json(&self.locator_path)?
            .map(|value| parse_locator(value, &self.scope))
            .transpose()
    }

    pub fn write_locator(&self, locator: &Locator) -> Result<(), String> {
        write_json_atomic(
            &self.locator_path,
            &json!({
                "version": 1,
                "scope": locator.scope,
                "invocation": locator.invocation,
                "session_id": locator.session_id,
                "run_id": locator.run_id,
                "turn_id": locator.turn_id,
            }),
        )?;
        // The public sidecar lets trace_messages retain its stable signature.
        write_json_atomic(
            &self.invocation_path,
            &json!({
                "version": 1,
                "scope": locator.scope,
                "invocation": locator.invocation,
                "session_id": locator.session_id,
                "run_id": locator.run_id,
                "turn_id": locator.turn_id,
            }),
        )
    }

    pub fn read_admission(&self) -> Result<Option<Admission>, String> {
        read_json(&self.admission_path)?
            .map(parse_admission)
            .transpose()
    }

    pub fn write_admission(&self, admission: &Admission) -> Result<(), String> {
        write_json_atomic(&self.admission_path, &admission_value(admission))
    }

    pub fn invocation_from_sidecar(path: &Path) -> Result<Option<Locator>, String> {
        let Some(value) = read_json(path)? else {
            return Ok(None);
        };
        if value.get("run_id").is_none() {
            return Ok(None);
        }
        parse_locator(value, "").map(Some)
    }

    pub fn scope_from_sidecar(path: &Path) -> Result<Option<String>, String> {
        read_json(path)?
            .map(|value| {
                let scope = string(&value, "scope")?;
                validate_scope(&scope)?;
                Ok(scope)
            })
            .transpose()
    }

    pub fn scope_store_path(io_root: &Path, scope: &str) -> PathBuf {
        io_root
            .join("conversations")
            .join(scope)
            .join("framework.sqlite3")
    }

    fn create_staged(&self) -> Result<(), String> {
        fs::create_dir_all(
            self.root_path
                .parent()
                .expect("conversation root has parent"),
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir_all(self.root_path()).map_err(|error| error.to_string())?;
        let stage = self
            .scope_dir
            .parent()
            .expect("scope directory has parent")
            .join(format!(".init-{}-{}", self.scope, std::process::id()));
        match fs::create_dir(&stage) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                fs::remove_dir_all(&stage).map_err(|error| error.to_string())?;
                fs::create_dir(&stage).map_err(|error| error.to_string())?;
            }
            Err(error) => return Err(error.to_string()),
        }
        let result = (|| {
            let store =
                Store::open(stage.join("framework.sqlite3")).map_err(|error| error.to_string())?;
            let session =
                Session::open(&store, &self.root_path).map_err(|error| error.to_string())?;
            write_json_atomic(
                &stage.join("session.json"),
                &json!({"version": 1, "session_id": session.id()}),
            )?;
            drop(store);
            sync_tree(&stage)?;
            fs::rename(&stage, &self.scope_dir).map_err(|error| error.to_string())?;
            sync_dir(self.scope_dir.parent().expect("scope directory has parent"))
        })();
        if result.is_err() {
            let _ = fs::remove_dir_all(&stage);
        }
        result
    }
}

pub(crate) fn remove_scope(io_root: &Path, hint: &str) -> Result<(), String> {
    let scope = format!("nc1-{:x}", Sha256::digest(hint.as_bytes()));
    let paths = ConversationPaths::for_invocation(io_root, &scope, "");
    let _lease = paths.acquire(io_root)?;
    let mut pointers = Vec::new();
    for entry in fs::read_dir(io_root).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("np1-") || !name.ends_with(".conversation.json") {
            continue;
        }
        if ConversationPaths::scope_from_sidecar(&entry.path())?.as_deref() == Some(scope.as_str())
        {
            pointers.push(entry.path());
        }
    }
    for path in [&paths.scope_dir, &paths.root_path] {
        match fs::remove_dir_all(path) {
            Ok(()) => sync_dir(path.parent().expect("conversation directory has parent"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    for pointer in pointers {
        fs::remove_file(&pointer).map_err(|error| error.to_string())?;
        let stem = pointer
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".conversation.json"))
            .ok_or_else(|| "conversation pointer has invalid filename".to_owned())?;
        match fs::remove_file(io_root.join(format!("{stem}.run"))) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        match fs::remove_dir_all(io_root.join(format!("{stem}.recordings"))) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        match fs::remove_file(crate::recording::call_ids_path(
            &io_root.join(format!("{stem}.recordings")),
        )) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    sync_dir(io_root)
}

pub(crate) struct PreparedConversation {
    pub(crate) paths: ConversationPaths,
    pub(crate) store: Store,
    pub(crate) session: io_harness::Session,
    pub(crate) admission: Admission,
    pub(crate) previous_effects: String,
    pub(crate) resume_after_interleaving: bool,
}

pub(crate) fn find_invocation_paths(
    io_root: &Path,
    key: &InvocationKey,
) -> Result<Option<ConversationPaths>, String> {
    let entries = match fs::read_dir(io_root.join("conversations")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut found = None;
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let scope = entry.file_name().to_string_lossy().into_owned();
        if validate_scope(&scope).is_err() {
            continue;
        }
        let paths = ConversationPaths::for_invocation(io_root, &scope, &key.durable_key());
        if let Some(admission) = paths.read_admission()? {
            if admission.invocation != key.durable_key() {
                return Err("native conversation admission invocation mismatch".into());
            }
            if found.is_some() {
                return Err("invocation is admitted to multiple native conversation scopes".into());
            }
            found = Some(paths);
        }
    }
    Ok(found)
}

pub(crate) fn prepare(
    io_root: &Path,
    paths: &ConversationPaths,
    request: &NodeExecutionRequest,
    resume_after_interleaving: bool,
) -> Result<PreparedConversation, String> {
    let (store, mut session) = paths.open_or_create()?;
    let existing = paths.read_admission()?;
    if let Some(admission) = &existing
        && (admission.invocation != request.key.durable_key()
            || admission.session_id != session.id()
            || admission.prompt != request.task)
    {
        return Err("native conversation admission mismatch".into());
    }
    // Repair this invocation first: a prior publication may have stopped
    // before replacing pending, while the native head already points here.
    let locator = existing
        .as_ref()
        .map(|admission| resolve_locator(paths, &store, admission))
        .transpose()?
        .flatten();
    if let Some(locator) = &locator
        && session.head() != Some(locator.turn_id)
        && !(resume_after_interleaving && is_descendant(&store, &session, locator)?)
        && session.head()
            != existing
                .as_ref()
                .expect("locator has admission")
                .parent_turn_id
    {
        return Err(
            "cannot resume an earlier native conversation turn after a later turn was admitted"
                .into(),
        );
    }
    if locator
        .as_ref()
        .is_none_or(|locator| session.head() != Some(locator.turn_id))
        && let Some(previous) = paths.read_pending()?
    {
        let previous_paths =
            ConversationPaths::for_invocation(io_root, &paths.scope, &previous.invocation);
        if let Some(previous_locator) = resolve_locator(&previous_paths, &store, &previous)? {
            reconcile_session(&store, &mut session, &previous_locator)?;
        }
    }
    let admission = match existing {
        Some(admission) => {
            if locator.is_none() && session.head() != admission.parent_turn_id {
                return Err(
                    "cannot resume an earlier native conversation admission after its head changed"
                        .into(),
                );
            }
            admission
        }
        None => Admission {
            invocation: request.key.durable_key(),
            session_id: session.id(),
            parent_turn_id: session.head(),
            prompt: request.task.clone(),
            runs_before: store.runs().map_err(|error| error.to_string())?,
        },
    };
    let interleaved = match &locator {
        Some(locator) => {
            resume_after_interleaving
                && session.head() != Some(locator.turn_id)
                && is_descendant(&store, &session, locator)?
        }
        None => false,
    };
    if let Some(locator) = &locator {
        reconcile_with_policy(&store, &mut session, locator, resume_after_interleaving)?;
        if interleaved {
            append_interleaved_turns(&store, &session, locator)?;
        }
    }
    // Re-entering a frozen admission completes all its publication writes,
    // including the key-only pointer used by trace/recovery and pending fence.
    if !interleaved {
        paths.publish_admission(&admission)?;
    }
    let mut previous_effects = String::new();
    for turn in session.history(&store).map_err(|error| error.to_string())? {
        if locator
            .as_ref()
            .is_some_and(|locator| locator.turn_id == turn.id)
        {
            continue;
        }
        previous_effects.push_str(&unfinished_effects(
            &store,
            &Locator {
                scope: paths.scope.clone(),
                invocation: String::new(),
                session_id: turn.session_id,
                run_id: turn.run_id,
                turn_id: turn.id,
            },
        )?);
    }
    Ok(PreparedConversation {
        paths: paths.clone(),
        store,
        session,
        admission,
        previous_effects,
        resume_after_interleaving,
    })
}

pub(crate) fn resolve_locator(
    paths: &ConversationPaths,
    store: &Store,
    admission: &Admission,
) -> Result<Option<Locator>, String> {
    if let Some(locator) = paths.read_locator()? {
        validate_locator(store, &locator, admission)?;
        return Ok(Some(locator));
    }
    // This admission is fenced by the conversation file lock. A missing
    // Started sidecar can bind only the sole new row whose public identities
    // match; selecting the newest row of a shared database is never safe.
    let candidates = store
        .runs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|id| !admission.runs_before.contains(id))
        .collect::<Vec<_>>();
    let run_id = match candidates.as_slice() {
        [] => return Ok(None),
        [run_id] => *run_id,
        _ => return Err("multiple native runs appeared after conversation admission; exact binding is uncertain".into()),
    };
    if store
        .run_goal(run_id)
        .map_err(|error| error.to_string())?
        .as_deref()
        != Some(admission.prompt.as_str())
        || store
            .run_file(run_id)
            .map_err(|error| error.to_string())?
            .as_deref()
            != Some(paths.root_path().to_string_lossy().as_ref())
    {
        return Err("native run does not match the frozen conversation admission".into());
    }
    let turn_id = match store
        .turn_for_run(run_id)
        .map_err(|error| error.to_string())?
    {
        Some(turn_id) => turn_id,
        None => {
            // run_with_extras creates the run before record_turn. No provider
            // or tool can run in that gap; reconstruct only its missing turn.
            if !store
                .step_turns(run_id)
                .map_err(|error| error.to_string())?
                .is_empty()
                || !store
                    .provider_calls(run_id)
                    .map_err(|error| error.to_string())?
                    .is_empty()
                || !store
                    .open_attempts(run_id)
                    .map_err(|error| error.to_string())?
                    .is_empty()
            {
                return Err(
                    "native run has effects but no Session turn; exact binding is uncertain".into(),
                );
            }
            store
                .record_turn(
                    admission.session_id,
                    admission.parent_turn_id,
                    run_id,
                    &admission.prompt,
                )
                .map_err(|error| error.to_string())?
        }
    };
    let locator = Locator {
        scope: paths.scope.clone(),
        invocation: admission.invocation.clone(),
        session_id: admission.session_id,
        run_id,
        turn_id,
    };
    validate_locator(store, &locator, admission)?;
    paths.write_locator(&locator)?;
    Ok(Some(locator))
}

fn validate_locator(store: &Store, locator: &Locator, admission: &Admission) -> Result<(), String> {
    let turn = store
        .session_turn(locator.turn_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "native conversation locator has no recorded turn".to_owned())?;
    if locator.invocation != admission.invocation
        || locator.session_id != admission.session_id
        || turn.run_id != locator.run_id
        || turn.session_id != locator.session_id
        || turn.parent_turn_id != admission.parent_turn_id
        || turn.prompt != admission.prompt
    {
        return Err("native conversation locator does not match its frozen admission".into());
    }
    Ok(())
}

pub(crate) fn reconcile_session(
    store: &Store,
    session: &mut io_harness::Session,
    locator: &Locator,
) -> Result<(), String> {
    reconcile_with_policy(store, session, locator, false)
}

pub(crate) fn reconcile_with_policy(
    store: &Store,
    session: &mut io_harness::Session,
    locator: &Locator,
    preserve_descendant_head: bool,
) -> Result<(), String> {
    let turn = store
        .session_turn(locator.turn_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "native conversation turn is missing".to_owned())?;
    if turn.session_id != session.id() || turn.run_id != locator.run_id {
        return Err("native conversation turn identity mismatch".into());
    }
    // Only a real native terminal outcome closes a turn. A provider error or
    // process crash retains the unfinished row and its unknown tool effects.
    if let Some(summary) = store
        .run_summary(locator.run_id)
        .map_err(|error| error.to_string())?
        && turn.outcome.as_deref() != Some(summary.outcome.as_str())
    {
        let reply = store
            .step_turns(locator.run_id)
            .map_err(|error| error.to_string())?
            .into_iter()
            .rev()
            .find(|step| step.calls.is_empty() && step.text.is_some())
            .and_then(|step| step.text);
        store
            .finish_turn(locator.turn_id, reply.as_deref(), &summary.outcome)
            .map_err(|error| error.to_string())?;
    }
    if session.head() == Some(locator.turn_id) {
        return Ok(());
    }
    if preserve_descendant_head && is_descendant(store, session, locator)? {
        return Ok(());
    }
    if session.head() != turn.parent_turn_id {
        return Err(
            "native conversation head advanced to another turn; refusing to replace it".into(),
        );
    }
    session
        .branch_from(store, locator.turn_id)
        .map_err(|error| error.to_string())
}

fn is_descendant(store: &Store, session: &Session, locator: &Locator) -> Result<bool, String> {
    if session.id() != locator.session_id {
        return Ok(false);
    }
    let history = session.history(store).map_err(|error| error.to_string())?;
    Ok(history
        .iter()
        .any(|turn| turn.id == locator.turn_id && turn.run_id == locator.run_id))
}

fn append_interleaved_turns(
    store: &Store,
    session: &Session,
    locator: &Locator,
) -> Result<(), String> {
    use io_harness::context::{ObsKind, Observation, Origin, SEED_AGENT, SEED_OPERATOR};
    let history = session.history(store).map_err(|error| error.to_string())?;
    let start = history
        .iter()
        .position(|turn| turn.id == locator.turn_id)
        .ok_or("background turn is not an ancestor of the current Session head")?;
    let observed = store
        .observations(locator.run_id)
        .map_err(|error| error.to_string())?;
    let step = observed.iter().map(|item| item.step).max().unwrap_or(0);
    let mut additions = Vec::new();
    for turn in &history[start + 1..] {
        for (role, content, origin) in [
            (SEED_OPERATOR, Some(turn.prompt.as_str()), Origin::Operator),
            (SEED_AGENT, turn.reply.as_deref(), Origin::Agent),
        ] {
            let Some(content) = content else {
                continue;
            };
            let marker = format!(
                "[Session turn {} {} while this background task yielded]\n",
                turn.id, role
            );
            if observed
                .iter()
                .any(|item| item.target.as_deref() == Some(role) && item.text.starts_with(&marker))
            {
                continue;
            }
            additions.push(Observation::new(
                step,
                ObsKind::Message,
                Some(role.into()),
                format!("{marker}{content}"),
                origin,
            ));
        }
    }
    store
        .record_observations(locator.run_id, &additions)
        .map_err(|error| error.to_string())
}

pub(crate) fn unfinished_effects(store: &Store, locator: &Locator) -> Result<String, String> {
    let turn = store
        .session_turn(locator.turn_id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "previous native conversation turn is missing".to_owned())?;
    let attempts = store
        .open_attempts(locator.run_id)
        .map_err(|error| error.to_string())?;
    if matches!(turn.outcome.as_deref(), Some("success" | "finished")) && attempts.is_empty() {
        return Ok(String::new());
    }
    let mut facts = format!(
        "\n\nThe preceding conversation turn did not complete successfully (native outcome: {}). Its prompt remains in the native conversation. Inspect the available previous workspace and anchor_conversation_history before continuing unfinished work. Recorded tool results are historical evidence, not current external state. Do not assume an unrecorded tool completed or repeat its operation automatically.",
        turn.outcome.as_deref().unwrap_or("unfinished"),
    );
    for attempt in attempts {
        facts.push_str(&format!(
            "\nUnrecorded tool result: tool={}, step={}, attempt={}, started_at={}. External outcome is unknown; verify the actual state before deciding whether to act.",
            attempt.tool, attempt.step, attempt.id, attempt.started_at,
        ));
    }
    Ok(facts)
}

pub(crate) struct ConversationObserver {
    paths: ConversationPaths,
    admission: Admission,
    cancellation: anchor_runtime_rig::Cancellation,
    error: Mutex<Option<String>>,
}

impl ConversationObserver {
    pub(crate) fn new(
        paths: ConversationPaths,
        admission: Admission,
        cancellation: anchor_runtime_rig::Cancellation,
    ) -> Self {
        Self {
            paths,
            admission,
            cancellation,
            error: Mutex::new(None),
        }
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        match self
            .error
            .lock()
            .map_err(|_| "conversation observer lock was poisoned".to_owned())?
            .clone()
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl io_harness::Observer for ConversationObserver {
    fn event(&self, event: &io_harness::RunEvent) -> io_harness::Flow {
        if matches!(event.kind, io_harness::EventKind::Started { .. }) && event.depth == 0 {
            let result = (|| {
                let store =
                    Store::open(self.paths.store_path()).map_err(|error| error.to_string())?;
                let turn_id = store
                    .turn_for_run(event.run_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| "Started native conversation run has no turn".to_owned())?;
                let locator = Locator {
                    scope: self.paths.scope.clone(),
                    invocation: self.admission.invocation.clone(),
                    session_id: self.admission.session_id,
                    run_id: event.run_id,
                    turn_id,
                };
                validate_locator(&store, &locator, &self.admission)?;
                self.paths.write_locator(&locator)
            })();
            if let Err(error) = result {
                if let Ok(mut slot) = self.error.lock() {
                    *slot = Some(error);
                }
                return io_harness::Flow::Cancel;
            }
        }
        if self.cancellation.load(Ordering::Relaxed) || self.check().is_err() {
            io_harness::Flow::Cancel
        } else {
            io_harness::Flow::Continue
        }
    }
}

fn admission_value(admission: &Admission) -> Value {
    json!({
        "version": 1,
        "invocation": admission.invocation,
        "session_id": admission.session_id,
        "parent_turn_id": admission.parent_turn_id,
        "prompt": admission.prompt,
        "runs_before": admission.runs_before,
    })
}

fn parse_locator(value: Value, expected_scope: &str) -> Result<Locator, String> {
    let scope = string(&value, "scope")?;
    validate_scope(&scope)?;
    if !expected_scope.is_empty() && scope != expected_scope {
        return Err("native conversation locator scope mismatch".into());
    }
    Ok(Locator {
        scope,
        invocation: string(&value, "invocation")?,
        session_id: integer(&value, "session_id")?,
        run_id: integer(&value, "run_id")?,
        turn_id: integer(&value, "turn_id")?,
    })
}

fn validate_scope(scope: &str) -> Result<(), String> {
    if !scope.starts_with("nc1-")
        || scope.len() != 68
        || !scope[4..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("native conversation scope is not a key digest".into());
    }
    Ok(())
}

fn parse_admission(value: Value) -> Result<Admission, String> {
    let runs_before = value
        .get("runs_before")
        .and_then(Value::as_array)
        .ok_or_else(|| "native conversation admission has no runs_before".to_owned())?
        .iter()
        .map(|id| {
            id.as_i64()
                .ok_or_else(|| "invalid prior native run id".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Admission {
        invocation: string(&value, "invocation")?,
        session_id: integer(&value, "session_id")?,
        parent_turn_id: value.get("parent_turn_id").and_then(Value::as_i64),
        prompt: string(&value, "prompt")?,
        runs_before,
    })
}

fn string(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("native conversation locator has invalid {field}"))
}

fn integer(value: &Value, field: &str) -> Result<i64, String> {
    value
        .get(field)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("native conversation locator has invalid {field}"))
}

fn read_session_id(path: &Path) -> Result<i64, String> {
    let value =
        read_json(path)?.ok_or_else(|| "native conversation has no Session pointer".to_owned())?;
    integer(&value, "session_id")
}

fn read_json(path: &Path) -> Result<Option<Value>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("invalid {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn write_json_atomic(path: &Path, value: &Value) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "durable file has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    use std::io::Write;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temporary, path).map_err(|error| error.to_string())?;
    sync_dir(parent)
}

fn sync_dir(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}

fn sync_tree(path: &Path) -> Result<(), String> {
    for entry in fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
        {
            File::open(entry.path())
                .and_then(|file| file.sync_all())
                .map_err(|error| error.to_string())?;
        }
    }
    sync_dir(path)
}
