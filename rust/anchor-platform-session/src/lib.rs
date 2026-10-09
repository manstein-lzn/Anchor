use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

mod associations;
mod channel;
mod database;
mod question_schema;
mod questions;
mod store;
mod turns;

pub use question_schema::validate_question_schema;
pub use store::SessionStore;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct CreateSession {
    pub id: Option<String>,
    pub title: String,
    pub graph: String,
    pub reply_node: String,
    pub channel: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelIdentity {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    pub conversation_id: String,
    pub sender_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AttachmentManifest {
    #[serde(default = "attachment_manifest_format")]
    pub format: u32,
    #[serde(default)]
    pub files: Vec<AttachmentManifestEntry>,
}

impl Default for AttachmentManifest {
    fn default() -> Self {
        Self {
            format: 1,
            files: Vec::new(),
        }
    }
}

fn attachment_manifest_format() -> u32 {
    1
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AttachmentManifestEntry {
    pub name: String,
    pub path: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// The turn and Run an admitted channel message belongs to. `run_id` is absent
/// while admission has not created the Run yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelInboundRun {
    pub session_id: String,
    pub turn_id: String,
    pub run_id: Option<String>,
}

/// One admitted channel message that still has no confirmed reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelPendingMessage {
    pub text: String,
    pub attachments: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelInboundRequest {
    pub inbound_id: String,
    pub identity: ChannelIdentity,
    pub graph: String,
    pub reply_node: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub attachments: AttachmentManifest,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub replace_running: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelInboundRelation {
    pub inbound_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub run_id: Option<String>,
    pub superseded_by_turn_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelAdmission {
    pub session: Session,
    pub turn: Turn,
    pub relation: ChannelInboundRelation,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelInboundAdmission {
    pub request: ChannelInboundRequest,
    pub relation: ChannelInboundRelation,
    pub turn: Turn,
    pub previous_run: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelAssistant {
    pub session_id: String,
    pub run_id: String,
    pub wait_node: String,
    pub work_node: String,
    pub reply_node: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelDeliveryRequest {
    pub key: String,
    pub kind: String,
    pub content_sha256: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelDeliveryStatus {
    Pending,
    Sending,
    Confirmed,
    Failed,
    Unknown,
    Suppressed,
}

impl ChannelDeliveryStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Confirmed => "confirmed",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Suppressed => "suppressed",
        }
    }

    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Confirmed | Self::Failed | Self::Suppressed)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ChannelDelivery {
    pub key: String,
    pub session_id: String,
    pub turn_id: String,
    pub kind: String,
    pub content_sha256: String,
    pub status: ChannelDeliveryStatus,
    pub error: Option<String>,
    pub superseded_by_turn_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Active,
    WaitingUser,
    Interrupted,
    Archived,
}

impl SessionStatus {
    pub(crate) fn event_kind(self) -> &'static str {
        match self {
            Self::Active => "session.active",
            Self::WaitingUser => "session.waiting_user",
            Self::Interrupted => "session.interrupted",
            Self::Archived => "session.archived",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Session {
    pub id: String,
    pub conversation_id: String,
    pub title: String,
    pub status: SessionStatus,
    pub waiting_reason: String,
    pub run_ids: Vec<String>,
    pub graph: String,
    pub reply_node: String,
    pub channel: BTreeMap<String, String>,
    pub approval: Option<Value>,
    pub approvals: Vec<Value>,
    pub questions: Vec<Value>,
    pub operation: Option<Value>,
    pub operations: BTreeMap<String, Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionEvent {
    pub seq: u64,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub data: BTreeMap<String, Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Running,
    Completed,
    Failed,
    Stopped,
    Interrupted,
}

impl TurnStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Interrupted => "interrupted",
        }
    }

    pub(crate) fn event_kind(self) -> &'static str {
        match self {
            Self::Running => "turn.started",
            Self::Completed => "turn.completed",
            Self::Failed => "turn.failed",
            Self::Stopped => "turn.stopped",
            Self::Interrupted => "turn.interrupted",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeExecution {
    pub scope: String,
    pub session: i64,
    pub run: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct GooseExecution {
    pub scope: String,
    pub session: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Turn {
    pub id: String,
    pub session: String,
    pub request_id: String,
    pub prompt: Option<String>,
    pub status: TurnStatus,
    pub error: Option<String>,
    #[serde(default)]
    pub native: Option<NativeExecution>,
    #[serde(default)]
    pub goose: Option<GooseExecution>,
    #[serde(default)]
    pub runs: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// When one Turn of a Session was executing, for a Run that outlives a single
/// Turn. It carries no prompt, delivery or execution detail: the only fact it
/// exposes is the interval work was actually attributed to a Run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnWindow {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The Turn had not reached a terminal status when this was read, so its
    /// window has no recorded end yet.
    pub running: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TurnEvent {
    pub seq: u64,
    pub data: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub session: String,
    pub turn: String,
    pub message: String,
    pub requested_schema: Value,
    pub status: QuestionStatus,
    pub answer: Option<QuestionAnswer>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionStatus {
    Pending,
    Answered,
    Interrupted,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuestionAnswer {
    pub action: QuestionAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionAction {
    Accept,
    Decline,
    Cancel,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SessionError {
    #[error("invalid session request: {0}")]
    Invalid(String),
    #[error("session not found")]
    Missing,
    #[error("session conflict: {0}")]
    Conflict(String),
    #[error("session storage error: {0}")]
    Storage(String),
}

impl From<rusqlite::Error> for SessionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.to_string())
    }
}

impl From<serde_json::Error> for SessionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Storage(error.to_string())
    }
}
