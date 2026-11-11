use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

use chrono::{SecondsFormat, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{CreateSession, Session, SessionError, SessionEvent, SessionStatus, database};

type Activity = (&'static str, BTreeMap<String, Value>);

#[derive(Clone)]
pub struct SessionStore {
    connection: Arc<Mutex<Connection>>,
}

impl SessionStore {
    pub fn open(database_path: impl AsRef<Path>) -> Result<Self, SessionError> {
        Ok(Self {
            connection: Arc::new(Mutex::new(database::open(database_path.as_ref())?)),
        })
    }

    pub fn create(&self, owner: &str, request: CreateSession) -> Result<Session, SessionError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = create_session(&transaction, owner, request)?;
        transaction.commit()?;
        Ok(session)
    }

    pub fn resolve_creation_request(
        &self,
        owner: &str,
        request: CreateSession,
        operation: (&str, &str),
        content: &Value,
        existing: Option<&str>,
    ) -> Result<Session, SessionError> {
        validate_identity(owner, "owner")?;
        let (namespace, key) = operation;
        validate_identity(namespace, "creation namespace")?;
        crate::turns::validate_input(key, None)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let retained: Option<(String, String)> = transaction
            .query_row(
                "SELECT sessions.id, session_events.data FROM session_events
             JOIN sessions ON sessions.id = session_events.session_id
             WHERE sessions.owner = ?1 AND session_events.kind = 'session.creation_request'
             AND json_extract(session_events.data, '$.namespace') = ?2
             AND json_extract(session_events.data, '$.key') = ?3 LIMIT 1",
                params![owner, namespace, key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((id, raw)) = retained {
            let data: Value = serde_json::from_str(&raw)?;
            if &data["content"] != content {
                return Err(SessionError::Conflict(
                    "operation key was already used for different content".into(),
                ));
            }
            let session = read_session(&transaction, owner, &id)?;
            transaction.commit()?;
            return Ok(session);
        }
        let session = match existing {
            Some(id) => read_session(&transaction, owner, id)?,
            None => create_session(&transaction, owner, request)?,
        };
        if !session.graph.is_empty() || !session.channel.is_empty() {
            return Err(SessionError::Invalid(
                "creation requests only support Pilot Sessions".into(),
            ));
        }
        append_event(
            &transaction,
            &session,
            "session.creation_request",
            BTreeMap::from([
                ("namespace".into(), json!(namespace)),
                ("key".into(), json!(key)),
                ("content".into(), content.clone()),
            ]),
        )?;
        transaction.commit()?;
        Ok(session)
    }

    pub fn get(&self, owner: &str, id: &str) -> Result<Session, SessionError> {
        validate_lookup(owner, id)?;
        let connection = self.lock()?;
        read_session(&connection, owner, id)
    }
}

fn create_session(
    connection: &Connection,
    owner: &str,
    request: CreateSession,
) -> Result<Session, SessionError> {
    validate_identity(owner, "owner")?;
    let id = request.id.unwrap_or_else(|| Uuid::new_v4().to_string());
    validate_identity(&id, "session id")?;
    let title = if request.title.trim().is_empty() {
        String::new()
    } else {
        validate_title(&request.title)?
    };
    for plugin in request.plugin_mounts.keys() {
        validate_identity(plugin, "plugin")?;
    }
    let exists: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
        [&id],
        |row| row.get(0),
    )?;
    if exists {
        return Err(SessionError::Conflict("session id already exists".into()));
    }
    let now = Utc::now();
    let session = Session {
        conversation_id: id.clone(),
        id,
        title,
        status: SessionStatus::Active,
        waiting_reason: String::new(),
        run_ids: Vec::new(),
        graph: request.graph,
        reply_node: request.reply_node,
        channel: request.channel,
        approval: None,
        approvals: Vec::new(),
        questions: Vec::new(),
        operation: None,
        operations: BTreeMap::new(),
        plugin_mounts: request.plugin_mounts,
        network_allowed: true,
        created_at: now,
        updated_at: now,
    };
    connection.execute(
        "INSERT INTO sessions(id, owner, data, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            session.id,
            owner,
            serde_json::to_string(&session)?,
            timestamp(&session)
        ],
    )?;
    append_event(connection, &session, "session.created", BTreeMap::new())?;
    Ok(session)
}

impl SessionStore {
    pub fn list(&self, owner: &str) -> Result<Vec<Session>, SessionError> {
        validate_identity(owner, "owner")?;
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT data FROM sessions WHERE owner = ?1 ORDER BY updated_at DESC, id ASC",
        )?;
        let rows = statement.query_map([owner], |row| row.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn rename(&self, owner: &str, id: &str, title: &str) -> Result<Session, SessionError> {
        self.mutate(owner, id, |_, session| {
            session.title = validate_title(title)?;
            Ok(Some((
                "session.renamed",
                BTreeMap::from([("title".into(), json!(session.title))]),
            )))
        })
    }

    pub fn set_status(
        &self,
        owner: &str,
        id: &str,
        status: SessionStatus,
        reason: &str,
    ) -> Result<Session, SessionError> {
        self.mutate(owner, id, |connection, session| {
            crate::turns::ensure_no_running(connection, id)?;
            if session.status == SessionStatus::Archived && status != SessionStatus::Archived {
                return Err(SessionError::Conflict(
                    "archived sessions cannot be resumed".into(),
                ));
            }
            session.status = status;
            session.waiting_reason = reason.into();
            let data = if reason.is_empty() {
                BTreeMap::new()
            } else {
                BTreeMap::from([("reason".into(), json!(reason))])
            };
            Ok(Some((status.event_kind(), data)))
        })
    }

    /// Set one Plugin's mount switch for this Session, and record who did it.
    ///
    /// `required` is decided by the caller from the Graph the Session runs: a
    /// required Plugin is never switched off. The event is the audit fact the
    /// authorization rests on, so it is written in the same transaction.
    pub fn set_plugin_mount(
        &self,
        owner: &str,
        id: &str,
        plugin: &str,
        on: bool,
        actor: &str,
        reason: &str,
    ) -> Result<Session, SessionError> {
        validate_identity(plugin, "plugin")?;
        self.mutate(owner, id, |_, session| {
            if session.plugin_mounts.get(plugin) == Some(&on) {
                return Ok(None);
            }
            session.plugin_mounts.insert(plugin.to_owned(), on);
            Ok(Some((
                "session.plugin_mount.changed",
                BTreeMap::from([
                    ("plugin".into(), json!(plugin)),
                    ("on".into(), json!(on)),
                    ("actor".into(), json!(actor)),
                    ("at".into(), json!(Utc::now().to_rfc3339())),
                    ("reason".into(), json!(reason)),
                ]),
            )))
        })
    }

    /// The Plugin mount switches of one Session, read by its id alone.
    ///
    /// A node call learns the Session from its Run's immutable metadata, not from
    /// the caller's owner, and `sessions.id` is the primary key: the host does not
    /// need the owner to answer what that Session mounted. A Session that is gone
    /// reads as "no override" rather than an error, so an older Run stays runnable
    /// and mounts only what its Graph declares.
    pub fn plugin_mounts(&self, id: &str) -> Result<BTreeMap<String, bool>, SessionError> {
        self.plugin_policy(id).map(|(mounts, _)| mounts)
    }

    /// Read mounts and the network ceiling from the same persisted Session row.
    pub fn plugin_policy(&self, id: &str) -> Result<(BTreeMap<String, bool>, bool), SessionError> {
        validate_identity(id, "session id")?;
        let connection = self.lock()?;
        let data: Option<String> = connection
            .query_row("SELECT data FROM sessions WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?;
        let Some(data) = data else {
            return Ok((BTreeMap::new(), true));
        };
        let session: Session = serde_json::from_str(&data)?;
        Ok((session.plugin_mounts, session.network_allowed))
    }

    pub fn set_network_allowed(
        &self,
        owner: &str,
        id: &str,
        allowed: bool,
        actor: &str,
    ) -> Result<Session, SessionError> {
        self.mutate(owner, id, |_, session| {
            if session.network_allowed == allowed {
                return Ok(None);
            }
            session.network_allowed = allowed;
            Ok(Some((
                "session.network.changed",
                BTreeMap::from([
                    ("allowed".into(), json!(allowed)),
                    ("actor".into(), json!(actor)),
                ]),
            )))
        })
    }

    /// The network Plugin's complete policy is one transaction. Readers never
    /// observe an intermediate permission/mount pair during a mode change.
    pub fn set_network_policy(
        &self,
        owner: &str,
        id: &str,
        enabled: bool,
        allowed: bool,
        actor: &str,
    ) -> Result<Session, SessionError> {
        self.mutate(owner, id, |_, session| {
            if session.network_allowed == allowed
                && session.plugin_mounts.get("network") == Some(&enabled)
            {
                return Ok(None);
            }
            session.network_allowed = allowed;
            session.plugin_mounts.insert("network".into(), enabled);
            Ok(Some((
                "session.network.changed",
                BTreeMap::from([
                    ("allowed".into(), json!(allowed)),
                    ("enabled".into(), json!(enabled)),
                    ("actor".into(), json!(actor)),
                ]),
            )))
        })
    }

    /// Every Session that mounts this Plugin, across every owner.
    ///
    /// Uninstalling a Plugin has to know who still holds it, and a node call only
    /// ever needs one Session: this is the one read that must not be owner-scoped.
    /// A switch that is explicitly off is not a holder.
    pub fn sessions_mounting(&self, plugin: &str) -> Result<Vec<String>, SessionError> {
        validate_identity(plugin, "plugin")?;
        let connection = self.lock()?;
        let mut statement = connection.prepare("SELECT id, data FROM sessions")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut sessions = Vec::new();
        for row in rows {
            let (id, data) = row?;
            let session: Session = serde_json::from_str(&data)?;
            if session.plugin_mounts.get(plugin).copied().unwrap_or(false) {
                sessions.push(id);
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    /// Append one audit event to a Session the caller already authorized.
    pub fn append_event(
        &self,
        owner: &str,
        id: &str,
        kind: &'static str,
        data: BTreeMap<String, Value>,
    ) -> Result<(), SessionError> {
        self.mutate(owner, id, |_, _| Ok(Some((kind, data))))?;
        Ok(())
    }

    pub fn events(
        &self,
        owner: &str,
        id: &str,
        after: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        validate_lookup(owner, id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction()?;
        read_session(&transaction, owner, id)?;
        let events = {
            let mut statement = transaction.prepare(
                "SELECT seq, at, kind, data FROM session_events
                 WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC",
            )?;
            let rows = statement.query_map(
                params![id, i64::try_from(after).unwrap_or(i64::MAX)],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )?;
            rows.map(|row| {
                let (seq, at, kind, data) = row?;
                Ok(SessionEvent {
                    seq: u64::try_from(seq)
                        .map_err(|error| SessionError::Storage(error.to_string()))?,
                    at: at.parse().map_err(|error: chrono::ParseError| {
                        SessionError::Storage(error.to_string())
                    })?,
                    kind,
                    data: serde_json::from_str(&data)?,
                })
            })
            .collect::<Result<Vec<_>, SessionError>>()?
        };
        transaction.commit()?;
        Ok(events)
    }

    pub fn attach_run(&self, owner: &str, id: &str, run: &str) -> Result<Session, SessionError> {
        self.mutate(owner, id, |connection, session| {
            validate_identity(run, "run id")?;
            crate::channel::ensure_assistant_run_scope(connection, &session.id, run)?;
            if session.run_ids.iter().any(|existing| existing == run) {
                return Ok(None);
            }
            session.run_ids.push(run.into());
            Ok(Some((
                "run.attached",
                BTreeMap::from([("run".into(), json!(run))]),
            )))
        })
    }

    pub fn delete(&self, owner: &str, id: &str) -> Result<(), SessionError> {
        self.delete_with_cleanup(owner, id, || Ok(()))
    }

    /// The Host may remove owned files only after the same transactional
    /// channel/Turn/Run checks that protect Session facts. A cleanup failure
    /// retains the row so the operator can inspect and retry.
    pub fn delete_with_cleanup(
        &self,
        owner: &str,
        id: &str,
        cleanup: impl FnOnce() -> Result<(), SessionError>,
    ) -> Result<(), SessionError> {
        validate_lookup(owner, id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let session = read_session(&transaction, owner, id)?;
        let channel: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM channel_sessions WHERE owner = ?1 AND session_id = ?2)",
            params![owner, id],
            |row| row.get(0),
        )?;
        if channel {
            return Err(SessionError::Conflict(
                "channel Sessions must be deleted through delete_channel_session".into(),
            ));
        }
        crate::turns::ensure_no_running(&transaction, id)?;
        if !session.run_ids.is_empty() {
            return Err(SessionError::Conflict(
                "sessions with retained runs cannot be deleted".into(),
            ));
        }
        cleanup()?;
        transaction.execute(
            "DELETE FROM sessions WHERE owner = ?1 AND id = ?2",
            params![owner, id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Connection>, SessionError> {
        self.connection
            .lock()
            .map_err(|_| SessionError::Storage("Session database lock poisoned".into()))
    }

    fn mutate(
        &self,
        owner: &str,
        id: &str,
        change: impl FnOnce(&Connection, &mut Session) -> Result<Option<Activity>, SessionError>,
    ) -> Result<Session, SessionError> {
        validate_lookup(owner, id)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut session = read_session(&transaction, owner, id)?;
        if let Some((kind, data)) = change(&transaction, &mut session)? {
            session.updated_at = Utc::now();
            write_session(&transaction, owner, &session)?;
            append_event(&transaction, &session, kind, data)?;
        }
        transaction.commit()?;
        Ok(session)
    }
}

pub(crate) fn read_session(
    connection: &Connection,
    owner: &str,
    id: &str,
) -> Result<Session, SessionError> {
    let data: String = connection
        .query_row(
            "SELECT data FROM sessions WHERE owner = ?1 AND id = ?2",
            params![owner, id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(SessionError::Missing)?;
    Ok(serde_json::from_str(&data)?)
}

pub(crate) fn write_session(
    connection: &Connection,
    owner: &str,
    session: &Session,
) -> Result<(), SessionError> {
    connection.execute(
        "UPDATE sessions SET data = ?3, updated_at = ?4 WHERE owner = ?1 AND id = ?2",
        params![
            owner,
            session.id,
            serde_json::to_string(session)?,
            timestamp(session)
        ],
    )?;
    Ok(())
}

pub(crate) fn append_event(
    connection: &Connection,
    session: &Session,
    kind: &str,
    data: BTreeMap<String, Value>,
) -> Result<(), SessionError> {
    let previous: i64 = connection.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM session_events WHERE session_id = ?1",
        [&session.id],
        |row| row.get(0),
    )?;
    let seq = previous
        .checked_add(1)
        .ok_or_else(|| SessionError::Storage("Session event sequence exhausted".into()))?;
    connection.execute(
        "INSERT INTO session_events(session_id, seq, at, kind, data) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            session.id,
            seq,
            timestamp(session),
            kind,
            serde_json::to_string(&data)?
        ],
    )?;
    Ok(())
}

fn timestamp(session: &Session) -> String {
    session
        .updated_at
        .to_rfc3339_opts(SecondsFormat::Nanos, true)
}

pub(crate) fn validate_lookup(owner: &str, id: &str) -> Result<(), SessionError> {
    validate_identity(owner, "owner")?;
    validate_identity(id, "session id")
}

pub(crate) fn validate_identity(value: &str, label: &str) -> Result<(), SessionError> {
    if value.is_empty()
        || value.len() > 256
        || matches!(value, "." | "..")
        || !value
            .chars()
            .all(|character| character.is_alphanumeric() || "_-.:@".contains(character))
    {
        return Err(SessionError::Invalid(format!("invalid {label}")));
    }
    Ok(())
}

fn validate_title(title: &str) -> Result<String, SessionError> {
    let trimmed = title.trim();
    if !(1..=120).contains(&trimmed.chars().count()) {
        return Err(SessionError::Invalid(
            "title must contain 1 to 120 Unicode characters".into(),
        ));
    }
    Ok(trimmed.into())
}
