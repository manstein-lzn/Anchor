use std::{
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{ChannelEvent, GatewayError, protocol::event_digest};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Confirmed,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryFact {
    pub kind: String,
    pub request_id: String,
    pub content_digest: String,
    pub wire_request_id: String,
    pub status: DeliveryStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SettlementStatus {
    Confirmed,
    Unknown,
    Suppressed,
}

impl SettlementStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unknown => "unknown",
            Self::Suppressed => "suppressed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryReceipt {
    pub key: String,
    pub content_sha256: String,
}

pub(crate) struct PendingSettlement {
    pub event: ChannelEvent,
    pub receipt: DeliveryReceipt,
    pub status: SettlementStatus,
}

pub(crate) struct Ledger {
    connection: Mutex<Connection>,
}

pub(crate) struct ReadyReply {
    pub event_id: String,
    pub callback_id: String,
    pub sender_id: String,
    pub conversation_id: String,
    pub text: String,
    pub items: Option<String>,
}

impl Ledger {
    pub fn open(path: &Path, profile: &str) -> Result<Self, GatewayError> {
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA synchronous=FULL; PRAGMA journal_mode=DELETE;")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 0 {
            let tables: i64 = connection.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                [],
                |row| row.get(0),
            )?;
            if tables != 0 {
                return Err(GatewayError::Ledger);
            }
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "CREATE TABLE inbound (event_id TEXT PRIMARY KEY, digest TEXT NOT NULL,
                  callback_id TEXT NOT NULL UNIQUE, sender_id TEXT NOT NULL, conversation_id TEXT NOT NULL,
                  state TEXT NOT NULL CHECK(state IN ('processing','ready','completed','unknown','rejected')), reply TEXT,
                  event_json TEXT, receipt_key TEXT, receipt_digest TEXT, reply_items TEXT,
                  created_at INTEGER NOT NULL DEFAULT 0,
                  superseded INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE deliveries (kind TEXT NOT NULL, request_id TEXT NOT NULL,
                  digest TEXT NOT NULL, wire_id TEXT NOT NULL UNIQUE,
                  status TEXT NOT NULL CHECK(status IN ('confirmed','unknown')),
                  PRIMARY KEY(kind, request_id));
                 CREATE TABLE gateway_profile (id INTEGER PRIMARY KEY CHECK(id=1), digest TEXT NOT NULL);
                 CREATE UNIQUE INDEX inbound_receipt_key_unique ON inbound(receipt_key);
                 CREATE TABLE settlement_outbox (receipt_key TEXT PRIMARY KEY, content_sha256 TEXT NOT NULL,
                  status TEXT NOT NULL CHECK(status IN ('confirmed','unknown','suppressed')),
                  event_id TEXT NOT NULL, event_json TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                  retry_at INTEGER NOT NULL DEFAULT 0, delivered INTEGER NOT NULL DEFAULT 0
                  CHECK(delivered IN (0,1)));
                 PRAGMA user_version=5;",
            )?;
            transaction.commit()?;
        } else if version == 1 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "ALTER TABLE inbound ADD COLUMN event_json TEXT;
                 ALTER TABLE inbound ADD COLUMN receipt_key TEXT;
                 ALTER TABLE inbound ADD COLUMN receipt_digest TEXT;
                 CREATE UNIQUE INDEX inbound_receipt_key_unique ON inbound(receipt_key);
                 CREATE TABLE settlement_outbox (receipt_key TEXT PRIMARY KEY, content_sha256 TEXT NOT NULL,
                  status TEXT NOT NULL CHECK(status IN ('confirmed','unknown','suppressed')),
                  event_id TEXT NOT NULL, event_json TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                  retry_at INTEGER NOT NULL DEFAULT 0, delivered INTEGER NOT NULL DEFAULT 0
                  CHECK(delivered IN (0,1)));
                 PRAGMA user_version=2;",
            )?;
            transaction.commit()?;
        } else if !(2..=5).contains(&version) {
            return Err(GatewayError::Ledger);
        }
        // Re-read the version: an older database may still need later steps.
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 2 {
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "ALTER TABLE inbound ADD COLUMN reply_items TEXT; PRAGMA user_version=3;",
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 3 {
            // v4 adds an admission timestamp and a terminal `rejected` state.
            // SQLite cannot widen a CHECK constraint in place, so the inbound
            // table is rebuilt in one transaction. Rows that predate this
            // column are stamped 0, i.e. "already expired": an undelivered
            // event from before the recovery window existed must be retired,
            // not replayed against a reply context that is long gone. New
            // admissions always carry their real timestamp.
            let transaction =
                connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute_batch(
                "DROP INDEX IF EXISTS inbound_receipt_key_unique;
                 ALTER TABLE inbound RENAME TO inbound_legacy_v3;
                 CREATE TABLE inbound (event_id TEXT PRIMARY KEY, digest TEXT NOT NULL,
                  callback_id TEXT NOT NULL UNIQUE, sender_id TEXT NOT NULL, conversation_id TEXT NOT NULL,
                  state TEXT NOT NULL CHECK(state IN ('processing','ready','completed','unknown','rejected')), reply TEXT,
                  event_json TEXT, receipt_key TEXT, receipt_digest TEXT, reply_items TEXT,
                  created_at INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO inbound(event_id,digest,callback_id,sender_id,conversation_id,state,reply,
                  event_json,receipt_key,receipt_digest,reply_items,created_at)
                  SELECT event_id,digest,callback_id,sender_id,conversation_id,state,reply,
                   event_json,receipt_key,receipt_digest,reply_items,0 FROM inbound_legacy_v3;
                 DROP TABLE inbound_legacy_v3;
                 CREATE UNIQUE INDEX inbound_receipt_key_unique ON inbound(receipt_key);
                 PRAGMA user_version=4;",
            )?;
            transaction.commit()?;
        }
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version == 4 {
            // v5 records the Host's decision that a message was cancelled by a
            // newer one. Arrival order alone cannot decide which turn is live:
            // a message whose media had to be downloaded first reaches the Host
            // later and takes the conversation over.
            connection.execute_batch(
                "ALTER TABLE inbound ADD COLUMN superseded INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version=5;",
            )?;
        } else if version != 5 {
            return Err(GatewayError::Ledger);
        }
        let saved: Option<String> = connection
            .query_row("SELECT digest FROM gateway_profile WHERE id=1", [], |row| {
                row.get(0)
            })
            .optional()?;
        match saved {
            Some(saved) if saved != profile => {
                return Err(GatewayError::Conflict(
                    "gateway state is bound to a different bot or transport endpoint",
                ));
            }
            None => {
                connection.execute(
                    "INSERT INTO gateway_profile(id,digest) VALUES(1,?1)",
                    [profile],
                )?;
            }
            _ => {}
        }
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "UPDATE inbound SET state='unknown' WHERE state='processing'",
            [],
        )?;
        transaction.execute_batch(
            "UPDATE inbound SET state='completed' WHERE state='ready' AND EXISTS(
             SELECT 1 FROM deliveries WHERE kind='reply' AND request_id=inbound.event_id AND status='confirmed');
             UPDATE inbound SET state='unknown' WHERE state='ready' AND EXISTS(
             SELECT 1 FROM deliveries WHERE kind='reply' AND request_id=inbound.event_id AND status='unknown');",
        )?;
        let unknown_replies = {
            let mut statement = transaction.prepare(
                "SELECT inbound.event_id FROM inbound JOIN deliveries
                 ON deliveries.kind='reply' AND deliveries.request_id=inbound.event_id
                 WHERE deliveries.status='unknown' AND inbound.receipt_key IS NOT NULL",
            )?;
            statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        for event_id in unknown_replies {
            enqueue_settlement(&transaction, &event_id, SettlementStatus::Unknown)?;
        }
        transaction.commit()?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn admit(&self, event: &ChannelEvent) -> Result<bool, GatewayError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let callback = event.metadata["request_id"]
            .as_str()
            .ok_or(GatewayError::Ledger)?;
        let previous: Option<(String, String)> = transaction
            .query_row(
                "SELECT digest,callback_id FROM inbound WHERE event_id=?1",
                [&event.event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((previous, previous_callback)) = previous {
            if previous != event_digest(event) {
                return Err(GatewayError::Conflict(
                    "callback event_id was already used for different content or identity",
                ));
            }
            if previous_callback != callback {
                return Err(GatewayError::Conflict(
                    "callback event_id is bound to a different req_id",
                ));
            }
            return Ok(false);
        }
        let collision: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbound WHERE callback_id=?1)",
            [callback],
            |row| row.get(0),
        )?;
        if collision {
            return Err(GatewayError::Conflict(
                "callback req_id was already bound to a different event",
            ));
        }
        transaction.execute(
            "INSERT INTO inbound(event_id,digest,callback_id,sender_id,conversation_id,state,event_json,created_at)
             VALUES(?1,?2,?3,?4,?5,'processing',?6,?7)",
            params![
                event.event_id,
                event_digest(event),
                callback,
                event.sender_id,
                event.conversation_id,
                serde_json::to_string(event).map_err(|_| GatewayError::Ledger)?,
                unix_millis()?
            ],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn ready(
        &self,
        event_id: &str,
        text: &str,
        items: Option<&str>,
        receipt: Option<&DeliveryReceipt>,
    ) -> Result<(), GatewayError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = receipt {
            bind_receipt(&transaction, event_id, receipt)?;
        }
        let changed = transaction.execute(
            "UPDATE inbound SET state='ready',reply=?2,reply_items=?3 WHERE event_id=?1
             AND state IN ('processing','unknown')",
            params![event_id, text, items],
        )?;
        if changed != 1 {
            return Err(GatewayError::Ledger);
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn suppress(
        &self,
        event_id: &str,
        receipt: Option<&DeliveryReceipt>,
    ) -> Result<(), GatewayError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = receipt {
            bind_receipt(&transaction, event_id, receipt)?;
        }
        let delivery: Option<String> = transaction
            .query_row(
                "SELECT status FROM deliveries WHERE kind='reply' AND request_id=?1",
                [event_id],
                |row| row.get(0),
            )
            .optional()?;
        if delivery.is_some() {
            return Err(GatewayError::Ledger);
        }
        enqueue_settlement(&transaction, event_id, SettlementStatus::Suppressed)?;
        let changed = transaction.execute(
            "UPDATE inbound SET state='completed' WHERE event_id=?1
             AND state IN ('processing','ready','unknown')",
            [event_id],
        )?;
        if changed != 1 {
            return Err(GatewayError::Ledger);
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn ready_replies(&self, limit: usize) -> Result<Vec<ReadyReply>, GatewayError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT event_id,callback_id,sender_id,conversation_id,reply,reply_items FROM inbound
             WHERE state='ready' AND NOT EXISTS(SELECT 1 FROM deliveries WHERE kind='reply'
             AND request_id=inbound.event_id) ORDER BY rowid LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok(ReadyReply {
                event_id: row.get(0)?,
                callback_id: row.get(1)?,
                sender_id: row.get(2)?,
                conversation_id: row.get(3)?,
                text: row.get(4)?,
                items: row.get(5)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Reply images worth attempting again: the text reply is absent or
    /// confirmed, never uncertain. The per-image delivery claim still decides
    /// whether one specific platform send may happen.
    pub fn pending_media(&self, limit: usize) -> Result<Vec<ReadyReply>, GatewayError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT event_id,callback_id,sender_id,conversation_id,reply,reply_items FROM inbound
             WHERE reply_items IS NOT NULL AND state IN ('ready','completed')
             AND NOT EXISTS(SELECT 1 FROM deliveries WHERE kind='reply' AND request_id=inbound.event_id
              AND status='unknown') ORDER BY rowid LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok(ReadyReply {
                event_id: row.get(0)?,
                callback_id: row.get(1)?,
                sender_id: row.get(2)?,
                conversation_id: row.get(3)?,
                text: row.get(4)?,
                items: row.get(5)?,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn finish_event(&self, event_id: &str, confirmed: bool) -> Result<(), GatewayError> {
        self.lock()?.execute(
            "UPDATE inbound SET state=?2 WHERE event_id=?1",
            params![event_id, if confirmed { "completed" } else { "unknown" }],
        )?;
        Ok(())
    }

    /// Retire every undelivered inbound that is older than the recovery
    /// window. A restart must rescue a message the platform handed over just
    /// before the process died; it must not replay an event whose reply context
    /// expired hours ago and whose Run already settled.
    pub fn retire_stale_inbounds(
        &self,
        now: i64,
        window_millis: i64,
    ) -> Result<usize, GatewayError> {
        let cutoff = now.saturating_sub(window_millis.max(0));
        let changed = self.lock()?.execute(
            "UPDATE inbound SET state='rejected' WHERE state='unknown' AND created_at < ?1
             AND NOT EXISTS(
                 SELECT 1 FROM deliveries
                 WHERE deliveries.kind='reply' AND deliveries.request_id=inbound.event_id
             )",
            [cutoff],
        )?;
        Ok(changed)
    }

    /// Mark one inbound as permanently refused by the Host.
    pub fn reject(&self, event_id: &str) -> Result<(), GatewayError> {
        let changed = self.lock()?.execute(
            "UPDATE inbound SET state='rejected' WHERE event_id=?1
             AND state IN ('processing','ready','unknown')",
            [event_id],
        )?;
        if changed != 1 {
            return Err(GatewayError::Ledger);
        }
        Ok(())
    }

    /// Record the Host's decision that this message was cancelled by a newer
    /// one. Superseded messages stop shadowing the turn that is really running,
    /// so that turn keeps its progress bubble and its reply.
    pub fn supersede(&self, event_id: &str) -> Result<(), GatewayError> {
        self.lock()?.execute(
            "UPDATE inbound SET superseded=1 WHERE event_id=?1",
            [event_id],
        )?;
        Ok(())
    }

    /// Whether the Host is still processing this inbound, which is what makes
    /// a "processing" bubble worth showing.
    pub fn is_processing(&self, event_id: &str) -> Result<bool, GatewayError> {
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbound WHERE event_id=?1 AND state='processing')",
            [event_id],
            |row| row.get::<_, bool>(0),
        )?)
    }

    /// Whether an inbound is still inside the recovery window, which decides
    /// whether a failure is worth telling the user about.
    pub fn is_recent(
        &self,
        event_id: &str,
        now: i64,
        window_millis: i64,
    ) -> Result<bool, GatewayError> {
        let cutoff = now.saturating_sub(window_millis.max(0));
        Ok(self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbound WHERE event_id=?1 AND created_at >= ?2)",
            params![event_id, cutoff],
            |row| row.get::<_, bool>(0),
        )?)
    }

    pub fn retryable_events(
        &self,
        now: i64,
        window_millis: i64,
    ) -> Result<Vec<ChannelEvent>, GatewayError> {
        let cutoff = now.saturating_sub(window_millis.max(0));
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT inbound.event_id,inbound.digest,inbound.event_json
             FROM inbound
             WHERE inbound.state='unknown' AND inbound.event_json IS NOT NULL
             AND inbound.created_at >= ?1
             AND NOT EXISTS(
                 SELECT 1 FROM deliveries
                 WHERE deliveries.kind='reply' AND deliveries.request_id=inbound.event_id
             )
             ORDER BY inbound.rowid",
        )?;
        let rows = statement.query_map([cutoff], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (event_id, saved_digest, event_json) = row?;
            let event: ChannelEvent =
                serde_json::from_str(&event_json).map_err(|_| GatewayError::Ledger)?;
            if event.event_id != event_id || event_digest(&event) != saved_digest {
                return Err(GatewayError::Ledger);
            }
            events.push(event);
        }
        Ok(events)
    }

    pub fn finish_reply(
        &self,
        event_id: &str,
        wire_id: &str,
        status: SettlementStatus,
    ) -> Result<(), GatewayError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(String, String)> = transaction
            .query_row(
                "SELECT wire_id,status FROM deliveries WHERE kind='reply' AND request_id=?1",
                [event_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((saved_wire_id, previous_status)) = previous else {
            return Err(GatewayError::Ledger);
        };
        if saved_wire_id != wire_id {
            return Err(GatewayError::Ledger);
        }
        if status == SettlementStatus::Confirmed {
            if previous_status != "confirmed" {
                transaction.execute(
                    "UPDATE deliveries SET status='confirmed' WHERE kind='reply' AND request_id=?1",
                    [event_id],
                )?;
            }
        } else if previous_status == "confirmed" {
            return Err(GatewayError::Ledger);
        }
        transaction.execute(
            "UPDATE inbound SET state=?2 WHERE event_id=?1",
            params![
                event_id,
                if status == SettlementStatus::Confirmed {
                    "completed"
                } else {
                    "unknown"
                }
            ],
        )?;
        enqueue_settlement(&transaction, event_id, status)?;
        transaction.commit()?;
        Ok(())
    }

    pub fn pending_settlements(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<PendingSettlement>, GatewayError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT event_json,receipt_key,content_sha256,status FROM settlement_outbox
             WHERE delivered=0 AND retry_at<=?1 ORDER BY rowid LIMIT ?2",
        )?;
        let rows = statement.query_map(params![now, limit as i64], |row| {
            let event_json: String = row.get(0)?;
            let status: String = row.get(3)?;
            let event =
                serde_json::from_str(&event_json).map_err(|_| rusqlite::Error::InvalidQuery)?;
            let status = match status.as_str() {
                "confirmed" => SettlementStatus::Confirmed,
                "unknown" => SettlementStatus::Unknown,
                "suppressed" => SettlementStatus::Suppressed,
                _ => return Err(rusqlite::Error::InvalidQuery),
            };
            Ok(PendingSettlement {
                event,
                receipt: DeliveryReceipt {
                    key: row.get(1)?,
                    content_sha256: row.get(2)?,
                },
                status,
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Stop retrying a settlement the Host permanently refused. The reply was
    /// delivered; only the receipt can never be acknowledged, so the outbox
    /// entry must not be replayed on every restart.
    pub fn abandon_settlement(&self, key: &str) -> Result<(), GatewayError> {
        self.lock()?.execute(
            "UPDATE settlement_outbox SET delivered=1 WHERE receipt_key=?1 AND delivered=0",
            [key],
        )?;
        Ok(())
    }

    pub fn finish_settlement(&self, key: &str, succeeded: bool) -> Result<(), GatewayError> {
        let connection = self.lock()?;
        let changed = if succeeded {
            connection.execute(
                "UPDATE settlement_outbox SET delivered=1 WHERE receipt_key=?1",
                [key],
            )?
        } else {
            let now = unix_millis()?;
            connection.execute(
                "UPDATE settlement_outbox SET attempts=attempts+1,
                 retry_at=?2 + min(300000, 1000 * (1 << min(attempts, 8)))
                WHERE receipt_key=?1 AND delivered=0",
                params![key, now],
            )?
        };
        if changed != 1 {
            return Err(GatewayError::Ledger);
        }
        Ok(())
    }

    pub fn is_latest(&self, event: &ChannelEvent) -> Result<bool, GatewayError> {
        self.is_latest_identity(&event.event_id, &event.sender_id, &event.conversation_id)
    }

    pub fn is_latest_identity(
        &self,
        event_id: &str,
        sender_id: &str,
        conversation_id: &str,
    ) -> Result<bool, GatewayError> {
        Ok(!self.lock()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbound WHERE rowid > (SELECT rowid FROM inbound WHERE event_id=?1)
             AND sender_id=?2 AND conversation_id=?3 AND superseded=0)",
            params![event_id, sender_id, conversation_id], |row| row.get::<_, bool>(0),
        )?)
    }

    pub fn claim(
        &self,
        kind: &str,
        request_id: &str,
        digest: &str,
        wire_id: &str,
    ) -> Result<bool, GatewayError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<(String, String)> = transaction
            .query_row(
                "SELECT digest,status FROM deliveries WHERE kind=?1 AND request_id=?2",
                params![kind, request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((previous, status)) = previous {
            if previous != digest {
                return Err(GatewayError::Conflict(
                    "send request_id was already used for different content or identity",
                ));
            }
            return if status == "confirmed" {
                Ok(false)
            } else {
                Err(GatewayError::PreviousUnconfirmed)
            };
        }
        transaction.execute(
            "INSERT INTO deliveries(kind,request_id,digest,wire_id,status) VALUES(?1,?2,?3,?4,'unknown')",
            params![kind, request_id, digest, wire_id],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn previous(
        &self,
        kind: &str,
        request_id: &str,
        digest: &str,
    ) -> Result<bool, GatewayError> {
        let previous: Option<(String, String)> = self
            .lock()?
            .query_row(
                "SELECT digest,status FROM deliveries WHERE kind=?1 AND request_id=?2",
                params![kind, request_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match previous {
            Some((previous, _)) if previous != digest => Err(GatewayError::Conflict(
                "send request_id was already used for different content or identity",
            )),
            Some((_, status)) if status == "confirmed" => Ok(true),
            Some(_) => Err(GatewayError::PreviousUnconfirmed),
            None => Ok(false),
        }
    }

    pub fn confirm(&self, wire_id: &str) -> Result<(), GatewayError> {
        let changed = self.lock()?.execute(
            "UPDATE deliveries SET status='confirmed' WHERE wire_id=?1",
            [wire_id],
        )?;
        if changed != 1 {
            return Err(GatewayError::Ledger);
        }
        Ok(())
    }

    pub fn facts(&self) -> Result<Vec<DeliveryFact>, GatewayError> {
        let connection = self.lock()?;
        let mut statement = connection.prepare(
            "SELECT kind,request_id,digest,wire_id,status FROM deliveries ORDER BY rowid",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(DeliveryFact {
                kind: row.get(0)?,
                request_id: row.get(1)?,
                content_digest: row.get(2)?,
                wire_request_id: row.get(3)?,
                status: if row.get::<_, String>(4)? == "confirmed" {
                    DeliveryStatus::Confirmed
                } else {
                    DeliveryStatus::Unknown
                },
            })
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, GatewayError> {
        self.connection.lock().map_err(|_| GatewayError::Ledger)
    }
}

fn bind_receipt(
    transaction: &rusqlite::Transaction<'_>,
    event_id: &str,
    receipt: &DeliveryReceipt,
) -> Result<(), GatewayError> {
    let saved: Option<(Option<String>, Option<String>)> = transaction
        .query_row(
            "SELECT receipt_key,receipt_digest FROM inbound WHERE event_id=?1",
            [event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((saved_key, saved_digest)) = saved else {
        return Err(GatewayError::Ledger);
    };
    match (saved_key, saved_digest) {
        (Some(key), Some(digest)) if key == receipt.key && digest == receipt.content_sha256 => {
            return Ok(());
        }
        (Some(_), _) | (_, Some(_)) => {
            return Err(GatewayError::Conflict(
                "channel receipt is already bound to different content or identity",
            ));
        }
        (None, None) => {}
    }
    let collision: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM inbound WHERE receipt_key=?1)",
        [&receipt.key],
        |row| row.get(0),
    )?;
    if collision {
        return Err(GatewayError::Conflict(
            "channel receipt key was already bound to a different event",
        ));
    }
    transaction.execute(
        "UPDATE inbound SET receipt_key=?2,receipt_digest=?3 WHERE event_id=?1",
        params![event_id, receipt.key, receipt.content_sha256],
    )?;
    Ok(())
}

fn enqueue_settlement(
    transaction: &rusqlite::Transaction<'_>,
    event_id: &str,
    status: SettlementStatus,
) -> Result<(), GatewayError> {
    let saved: Option<(Option<String>, Option<String>, Option<String>)> = transaction
        .query_row(
            "SELECT event_json,receipt_key,receipt_digest FROM inbound WHERE event_id=?1",
            [event_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((Some(event_json), Some(key), Some(content_sha256))) = saved else {
        return Ok(());
    };
    let previous: Option<(String, String, String, String)> = transaction
        .query_row(
            "SELECT content_sha256,status,event_id,event_json FROM settlement_outbox
             WHERE receipt_key=?1",
            [&key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    if let Some((previous_digest, previous_status, previous_event, previous_json)) = previous {
        if previous_digest != content_sha256
            || previous_status != status.as_str()
            || previous_event != event_id
            || previous_json != event_json
        {
            return Err(GatewayError::Conflict(
                "channel receipt key was already settled for different content or outcome",
            ));
        }
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO settlement_outbox(receipt_key,content_sha256,status,event_id,event_json)
         VALUES(?1,?2,?3,?4,?5)",
        params![key, content_sha256, status.as_str(), event_id, event_json],
    )?;
    Ok(())
}

fn unix_millis() -> Result<i64, GatewayError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GatewayError::Ledger)?;
    i64::try_from(duration.as_millis()).map_err(|_| GatewayError::Ledger)
}

pub(crate) fn current_time_millis() -> Result<i64, GatewayError> {
    unix_millis()
}
