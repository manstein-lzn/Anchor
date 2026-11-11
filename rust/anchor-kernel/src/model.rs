//! The primitives. Everything in Anchor is one of these, or made of them.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type ResourceId = String;

// ---------------------------------------------------------------------------
// Principals: who proposes
// ---------------------------------------------------------------------------

/// What kind of principal proposes. The kernel uses it for exactly two laws:
/// while a task waits, only people and host code may act on it; validators can
/// only be changed by people.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrincipalKind {
    Agent,
    Human,
    /// Deterministic host code: observers, schedulers, adapters.
    Code,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub kind: PrincipalKind,
    /// Digest of the configuration the host froze for this principal: Graph,
    /// node, Plugins, model and prompts. Part of who proposes, recorded on
    /// every stamp; the kernel never judges by it. Authorities and validators
    /// must decide by `id` and `kind` only, never by comparing or keying on a
    /// whole `Principal`, whose equality includes this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
}

impl Principal {
    pub fn new(id: impl Into<String>, kind: PrincipalKind) -> Self {
        Self {
            id: id.into(),
            kind,
            config: None,
        }
    }

    pub fn with_config(mut self, digest: impl Into<String>) -> Self {
        self.config = Some(digest.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Submission: the only way to change anything
// ---------------------------------------------------------------------------

/// The four things a proposer can do to the world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Change a resource.
    Write,
    /// Ask a higher level for authority or information.
    Request,
    /// Produce an effect on the outside world.
    Execute,
    /// End a task.
    Settle,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    /// A host observation, identified by its owner's stable id.
    Observation { id: String },
    /// A resource at an exact version.
    Resource { id: ResourceId, version: u64 },
}

/// Everything a proposer may fill in. There is no field for identity, cost,
/// authority, approval or time: those are host facts, and a field that does
/// not exist cannot be forged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    pub target: ResourceId,
    /// The version the proposer saw (optimistic concurrency).
    pub seen: u64,
    pub action: Action,
    /// The open slot this submission fills, when the validator offers slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
    /// Free content. The kernel never interprets it; validators may.
    #[serde(default)]
    pub content: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

impl Submission {
    pub fn new(target: impl Into<ResourceId>, seen: u64, action: Action) -> Self {
        Self {
            target: target.into(),
            seen,
            action,
            slot: None,
            content: Value::Null,
            evidence: Vec::new(),
        }
    }

    pub fn slot(mut self, slot: impl Into<String>) -> Self {
        self.slot = Some(slot.into());
        self
    }

    pub fn content(mut self, content: Value) -> Self {
        self.content = content;
        self
    }

    pub fn evidence(mut self, evidence: Evidence) -> Self {
        self.evidence.push(evidence);
        self
    }
}

/// Host-observed facts about one submission. Only the kernel constructs one.
/// A record needs no number: its history is append-only, so its place in
/// that history orders it, and time orders records across resources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stamp {
    pub(crate) principal: Principal,
    pub(crate) cost: u64,
    /// Milliseconds since the Unix epoch, from the kernel's clock. Recorded,
    /// never judged by. Absent on records made before it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) at: Option<u64>,
}

impl Stamp {
    pub fn principal(&self) -> &Principal {
        &self.principal
    }
    pub fn cost(&self) -> u64 {
        self.cost
    }
    pub fn at(&self) -> Option<u64> {
        self.at
    }
    pub fn config(&self) -> Option<&str> {
        self.principal.config.as_deref()
    }
}

// ---------------------------------------------------------------------------
// Verdict: two independent dimensions and a reason code
// ---------------------------------------------------------------------------

/// May the change take effect?
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Accepted,
    /// Fixable: back to the proposer.
    Returned,
    /// Not fixable: authority, a law, or a settled fact.
    Refused,
    /// Waiting for a person.
    Pending,
}

/// What verification the content received. Independent of [`Effect`]:
/// accepted never means correct.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    Mechanical,
    Evidenced,
    Asserted,
    Reviewed,
    NotApplicable,
}

/// A closed set, chosen by code. `Unclassified` growing means the set is
/// missing something.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Ok,
    UnknownResource,
    UnknownValidator,
    Unauthorized,
    StaleVersion,
    TaskSettled,
    TaskWaiting,
    NotWaiting,
    BudgetExhausted,
    EffectUnknown,
    HumanOnly,
    AwaitingHuman,
    PendingTargetChanged,
    SlotClosed,
    ContentInvalid,
    RouteInvalid,
    EvidenceMissing,
    Unclassified,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub effect: Effect,
    pub verification: Verification,
    pub reason: Reason,
}

impl Verdict {
    pub fn refused(reason: Reason) -> Self {
        Self {
            effect: Effect::Refused,
            verification: Verification::NotApplicable,
            reason,
        }
    }
    pub fn returned(reason: Reason) -> Self {
        Self {
            effect: Effect::Returned,
            verification: Verification::NotApplicable,
            reason,
        }
    }
    pub fn is_accepted(&self) -> bool {
        self.effect == Effect::Accepted
    }
}

// ---------------------------------------------------------------------------
// Resource and task
// ---------------------------------------------------------------------------

/// Why a task is not executing. Every "parked" state of a Run is one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", rename_all = "snake_case")]
pub enum Waiter {
    /// A person must act: paused, stopped, a confirmation.
    Human { reason: String },
    /// A child task must settle.
    Child { task: ResourceId },
    /// An unknown external effect must be observed.
    Effect { effect: ResourceId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum Outcome {
    Done,
    Failed { reason: Reason, detail: String },
    Escalated { detail: String },
    Unknown { detail: String },
    Aborted { detail: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum Phase {
    Executing,
    Waiting { waiter: Waiter },
    Settled { result: Outcome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub limit: u64,
    /// Derived from every historical stamp, independently of resource.version.
    pub spent: u64,
}

/// A resource has one owner (its store), one validator, and a version that
/// moves only through accepted submissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Resource {
    pub id: ResourceId,
    pub version: u64,
    pub validator: ResourceId,
    /// Present only on task resources.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<Task>,
    #[serde(default)]
    pub state: Value,
    /// When and with which configuration the resource was created. The
    /// kernel stamps it on creation and carries it through every change;
    /// no rule reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<Creation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Creation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub phase: Phase,
    pub budget: Budget,
}

impl Resource {
    pub fn new(id: impl Into<ResourceId>, validator: impl Into<ResourceId>, state: Value) -> Self {
        Self {
            id: id.into(),
            version: 0,
            validator: validator.into(),
            task: None,
            state,
            created: None,
        }
    }

    /// The configuration digest to record with the creation. The time is
    /// always the kernel's own.
    pub fn configured(mut self, digest: impl Into<String>) -> Self {
        self.created = Some(Creation {
            at: None,
            config: Some(digest.into()),
        });
        self
    }

    pub fn task(mut self, budget: u64) -> Self {
        self.task = Some(Task {
            phase: Phase::Executing,
            budget: Budget {
                limit: budget,
                spent: 0,
            },
        });
        self
    }

    pub fn phase(&self) -> Option<&Phase> {
        self.task.as_ref().map(|task| &task.phase)
    }

    pub fn is_settled(&self) -> bool {
        matches!(self.phase(), Some(Phase::Settled { .. }))
    }
}

/// One appended fact: a submission, who made it, and how it was judged.
/// Accepted records carry the resulting resource, which makes recovery after a
/// crash between "history appended" and "state saved" a read, not a replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub stamp: Stamp,
    pub submission: Submission,
    pub verdict: Verdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<Resource>,
}

/// A history fact without its recovery snapshot. Observers use the same
/// stamps, submissions and verdicts without retaining every resource version.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordEntry {
    pub stamp: Stamp,
    pub submission: Submission,
    pub verdict: Verdict,
}

impl From<Record> for RecordEntry {
    fn from(record: Record) -> Self {
        Self {
            stamp: record.stamp,
            submission: record.submission,
            verdict: record.verdict,
        }
    }
}

// ---------------------------------------------------------------------------
// Validators
// ---------------------------------------------------------------------------

/// An opening a validator offers: a submission it would currently consider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slot {
    pub id: String,
    pub action: Action,
}

pub struct JudgeCtx<'a> {
    pub resource: &'a Resource,
    pub principal: &'a Principal,
}

impl JudgeCtx<'_> {
    /// Weakest honest classification from the evidence alone. Validators that
    /// check content mechanically report [`Verification::Mechanical`] themselves.
    pub fn verification_of(&self, submission: &Submission) -> Verification {
        if submission.evidence.is_empty() {
            Verification::Asserted
        } else {
            Verification::Evidenced
        }
    }
}

/// What a validator decides. On acceptance it may set the next state and, for
/// tasks, the next phase; the kernel enforces that a settled phase never moves.
#[derive(Debug, Clone, PartialEq)]
pub struct Judgement {
    pub verdict: Verdict,
    pub state: Option<Value>,
    pub phase: Option<Phase>,
}

impl Judgement {
    pub fn accept(state: Value, verification: Verification) -> Self {
        Self {
            verdict: Verdict {
                effect: Effect::Accepted,
                verification,
                reason: Reason::Ok,
            },
            state: Some(state),
            phase: None,
        }
    }
    pub fn with_phase(mut self, phase: Phase) -> Self {
        self.phase = Some(phase);
        self
    }
    pub fn returned(reason: Reason) -> Self {
        Self {
            verdict: Verdict::returned(reason),
            state: None,
            phase: None,
        }
    }
    pub fn refused(reason: Reason) -> Self {
        Self {
            verdict: Verdict::refused(reason),
            state: None,
            phase: None,
        }
    }
    /// A person must decide. `state` is what applies after approval.
    pub fn pending(state: Value) -> Self {
        Self {
            verdict: Verdict {
                effect: Effect::Pending,
                verification: Verification::NotApplicable,
                reason: Reason::AwaitingHuman,
            },
            state: Some(state),
            phase: None,
        }
    }
}

pub trait Validator: Send + Sync {
    /// The submissions this resource would currently consider. A scheduler
    /// offers these to executors; the kernel never calls an executor.
    fn open(&self, _resource: &Resource) -> Vec<Slot> {
        Vec::new()
    }
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement;
}

// ---------------------------------------------------------------------------
// Measurement
// ---------------------------------------------------------------------------

/// One point per adjudication, produced at constant cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Point {
    /// The resource kind: the target id up to its first `/`.
    pub component: String,
    pub validator: ResourceId,
    pub principal: PrincipalKind,
    pub action: Action,
    pub effect: Effect,
    pub verification: Verification,
    pub reason: Reason,
    pub cost: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<String>,
}
