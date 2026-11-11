//! The four duties: adjudicate, record, settle, measure.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::model::*;
use crate::store::{Store, StoreError};

/// The constitution's own validator: only people change validators.
pub const CONSTITUTION: &str = "validator/constitution";
/// People complete pending verdicts.
pub const HUMAN_DECISION: &str = "validator/human-decision";
/// The lifecycle of an external effect.
pub const EFFECT: &str = "validator/effect";

/// Host authority, frozen per turn or Run. Nothing in a submission can create
/// authority; the kernel only asks. Judge by `principal.id` and `kind` only:
/// `config` is recorded identity, never a grant.
pub trait Authority: Send + Sync {
    fn permits(&self, principal: &Principal, action: Action, target: &str) -> bool;
}

pub trait Probe: Send + Sync {
    fn observe(&self, point: &Point);
}

/// The kernel's only source of time. It stamps time; it never judges by it.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("validator `{0}` is not installed")]
    MissingValidator(String),
}

#[derive(Default, Clone)]
pub struct Validators {
    map: BTreeMap<ResourceId, Arc<dyn Validator>>,
}

impl Validators {
    pub fn get(&self, id: &str) -> Option<&Arc<dyn Validator>> {
        self.map.get(id)
    }
}

pub struct Kernel<S: Store> {
    store: S,
    authority: Box<dyn Authority>,
    validators: Validators,
    probe: Option<Box<dyn Probe>>,
    clock: Box<dyn Clock>,
    /// One writer per state root. Adjudication is short; executors run outside.
    writer: Mutex<()>,
}

impl<S: Store> Kernel<S> {
    pub fn new(store: S, authority: Box<dyn Authority>) -> Result<Self, KernelError> {
        Self::with_clock(store, authority, Box::new(SystemClock))
    }

    /// The clock is fixed at construction, so every creation and stamp,
    /// including the constitutional validators', reads the same time source.
    pub fn with_clock(
        store: S,
        authority: Box<dyn Authority>,
        clock: Box<dyn Clock>,
    ) -> Result<Self, KernelError> {
        let mut kernel = Self {
            store,
            authority,
            validators: Validators::default(),
            probe: None,
            clock,
            writer: Mutex::new(()),
        };
        kernel.install(CONSTITUTION, Arc::new(HumanOnly))?;
        kernel.install(HUMAN_DECISION, Arc::new(HumanDecision))?;
        kernel.install(EFFECT, Arc::new(EffectLifecycle))?;
        Ok(kernel)
    }

    pub fn with_probe(mut self, probe: Box<dyn Probe>) -> Self {
        self.probe = Some(probe);
        self
    }

    /// Register validator code. Its resource is judged by the constitution, so
    /// only people can change it.
    pub fn install(&mut self, id: &str, validator: Arc<dyn Validator>) -> Result<(), KernelError> {
        self.validators.map.insert(id.into(), validator);
        if self.store.load(id)?.is_none() {
            self.store.create(&self.stamp_creation(
                Resource::new(id, CONSTITUTION, Value::Null),
                Some(self.clock.now_ms()),
            ))?;
        }
        Ok(())
    }

    pub fn create(&self, resource: Resource) -> Result<(), KernelError> {
        if self.validators.get(&resource.validator).is_none() {
            return Err(KernelError::MissingValidator(resource.validator));
        }
        let resource = self.stamp_creation(resource, Some(self.clock.now_ms()));
        Ok(self.store.create(&resource)?)
    }

    /// The creation time is always the kernel's; only the configuration digest
    /// comes from the creator.
    fn stamp_creation(&self, mut resource: Resource, at: Option<u64>) -> Resource {
        let config = resource.created.take().and_then(|created| created.config);
        resource.created = Some(Creation { at, config });
        resource
    }

    pub fn load(&self, id: &str) -> Result<Option<Resource>, KernelError> {
        Ok(self.store.load(id)?)
    }

    pub fn history(&self, id: &str) -> Result<Vec<Record>, KernelError> {
        Ok(self.store.history(id)?)
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    /// What this resource would currently consider. Empty unless executing:
    /// a waiting or settled task offers nothing, so a scheduler never has to
    /// check for stop, pause or completion itself.
    pub fn open(&self, id: &str) -> Result<Vec<Slot>, KernelError> {
        let Some(resource) = self.store.load(id)? else {
            return Ok(Vec::new());
        };
        if resource.phase().is_some_and(|p| *p != Phase::Executing) {
            return Ok(Vec::new());
        }
        let validator = self
            .validators
            .get(&resource.validator)
            .ok_or_else(|| KernelError::MissingValidator(resource.validator.clone()))?;
        Ok(validator.open(&resource))
    }

    /// The single entry point for change. `cost` is a host observation: what
    /// the proposer spent since its previous submission to this target.
    /// Every recorded verdict is charged. Retrying the same work after a
    /// stale-version verdict reports zero new cost.
    pub fn submit(
        &self,
        principal: &Principal,
        cost: u64,
        submission: Submission,
    ) -> Result<Verdict, KernelError> {
        let _writer = self.writer.lock().expect("kernel writer");
        let stamp = Stamp {
            principal: principal.clone(),
            cost,
            at: Some(self.clock.now_ms()),
        };
        let Some(resource) = self.store.load(&submission.target)? else {
            let verdict = Verdict::refused(Reason::UnknownResource);
            self.measure(&stamp, &submission, "", verdict);
            return Ok(verdict);
        };
        let (verdict, validator, after, effects) =
            self.adjudicate(&stamp, &submission, resource)?;
        // Record: the target's history gets every verdict, refusals included.
        self.store.commit(
            &submission.target,
            &Record {
                stamp: stamp.clone(),
                submission: submission.clone(),
                verdict,
                after,
            },
        )?;
        for mut created in effects {
            // Resources the kernel creates for a submission share its stamp.
            created.created = principal.config.clone().map(|config| Creation {
                at: None,
                config: Some(config),
            });
            self.store.create(&self.stamp_creation(created, stamp.at))?;
        }
        self.measure(&stamp, &submission, &validator, verdict);
        Ok(verdict)
    }

    #[allow(clippy::type_complexity)]
    fn adjudicate(
        &self,
        stamp: &Stamp,
        submission: &Submission,
        mut resource: Resource,
    ) -> Result<(Verdict, ResourceId, Option<Resource>, Vec<Resource>), KernelError> {
        let principal = &stamp.principal;
        let validator_id = resource.validator.clone();
        let waiting_work =
            matches!(resource.phase(), Some(Phase::Waiting { .. })) && !is_lifecycle(submission);
        // Costs belong to history, not the accepted-state version. The store
        // derives spent from all previous records; include this record before
        // any verdict, including authority, waiting and stale refusals.
        if charge(&mut resource, stamp.cost) {
            return Ok((
                Verdict::refused(if waiting_work {
                    Reason::TaskWaiting
                } else {
                    Reason::BudgetExhausted
                }),
                validator_id,
                Some(resource),
                Vec::new(),
            ));
        }
        let refuse = |reason| {
            Ok((
                Verdict::refused(reason),
                validator_id.clone(),
                None,
                Vec::new(),
            ))
        };

        // No principal can bypass the waiting gate, including one without
        // authority. Budget exhaustion above still settles the task, while
        // its late work is refused as TaskWaiting and never applied.
        if waiting_work {
            return refuse(Reason::TaskWaiting);
        }
        // Authority only descends; the submission cannot widen it.
        if !self
            .authority
            .permits(principal, submission.action, &submission.target)
        {
            return refuse(Reason::Unauthorized);
        }
        // Law 3: settled outcomes never change. Waiting tasks accept only
        // lifecycle requests, never late work, regardless of its principal.
        if let Some(task) = &resource.task {
            match &task.phase {
                Phase::Settled { .. } => return refuse(Reason::TaskSettled),
                Phase::Waiting { .. } if principal.kind == PrincipalKind::Agent => {
                    return refuse(Reason::TaskWaiting);
                }
                _ => {}
            }
        }
        if submission.seen != resource.version {
            return Ok((
                Verdict::returned(Reason::StaleVersion),
                validator_id,
                None,
                Vec::new(),
            ));
        }
        // Law 3 again: the lifecycle of every task is the same three requests,
        // whatever its validator. No Graph implements stop, resume or abandon.
        if submission.action == Action::Request
            && resource.task.is_some()
            && let Some(control) = submission.content.get("control").and_then(Value::as_str)
        {
            return Ok(
                match task_control(
                    &mut resource,
                    principal,
                    control,
                    &submission.content,
                    &submission.evidence,
                ) {
                    Ok(()) => {
                        resource.version += 1;
                        (
                            Verdict {
                                effect: Effect::Accepted,
                                verification: Verification::NotApplicable,
                                reason: Reason::Ok,
                            },
                            validator_id,
                            Some(resource),
                            Vec::new(),
                        )
                    }
                    Err(verdict) => (verdict, validator_id, None, Vec::new()),
                },
            );
        }
        // Law 2: what was not observed is unknown, and blocks the next effect.
        if submission.action == Action::Execute && self.has_unknown_effect(&submission.target)? {
            return refuse(Reason::EffectUnknown);
        }
        let validator = self
            .validators
            .get(&validator_id)
            .ok_or_else(|| KernelError::MissingValidator(validator_id.clone()))?;
        let judgement = validator.judge(
            &JudgeCtx {
                resource: &resource,
                principal,
            },
            submission,
        );
        let verdict = judgement.verdict;
        match verdict.effect {
            Effect::Accepted => {
                if validator_id == HUMAN_DECISION {
                    return self.resolve_pending(stamp, resource, judgement);
                }
                let mut created = Vec::new();
                if submission.action == Action::Execute {
                    created.push(Resource::new(
                        format!("effect/{}/{}", submission.target, self.place(submission)?),
                        EFFECT,
                        json!({"endpoint": submission.target, "status": "authorized"}),
                    ));
                }
                resource.version += 1;
                if let Some(state) = judgement.state {
                    resource.state = state;
                }
                if let Some(task) = &mut resource.task {
                    if let Some(phase) = judgement.phase {
                        task.phase = phase;
                    } else if submission.action == Action::Settle {
                        task.phase = Phase::Settled {
                            result: Outcome::Done,
                        };
                    }
                }
                Ok((verdict, validator_id, Some(resource), created))
            }
            Effect::Pending => {
                let pending = Resource::new(
                    format!("pending/{}/{}", submission.target, self.place(submission)?),
                    HUMAN_DECISION,
                    json!({
                        "target": submission.target,
                        "seen": resource.version,
                        "state": judgement.state,
                        "phase": judgement.phase,
                    }),
                );
                Ok((verdict, validator_id, None, vec![pending]))
            }
            Effect::Returned | Effect::Refused => Ok((verdict, validator_id, None, Vec::new())),
        }
    }

    /// A person decided. The approval binds to the exact version the proposal
    /// saw; a target that changed meanwhile refuses it.
    #[allow(clippy::type_complexity)]
    fn resolve_pending(
        &self,
        stamp: &Stamp,
        mut pending: Resource,
        judgement: Judgement,
    ) -> Result<(Verdict, ResourceId, Option<Resource>, Vec<Resource>), KernelError> {
        let decision = judgement.state.unwrap_or(Value::Null);
        let approved = decision.get("decision").and_then(Value::as_str) == Some("approve");
        let proposal = pending.state.clone();
        pending.version += 1;
        pending.state = json!({"proposal": proposal, "decision": decision});
        if approved {
            let target_id = proposal["target"].as_str().unwrap_or_default().to_owned();
            let Some(mut target) = self.store.load(&target_id)? else {
                return Ok((
                    Verdict::refused(Reason::UnknownResource),
                    HUMAN_DECISION.into(),
                    Some(pending),
                    Vec::new(),
                ));
            };
            if target.is_settled() {
                return Ok((
                    Verdict::refused(Reason::TaskSettled),
                    HUMAN_DECISION.into(),
                    Some(pending),
                    Vec::new(),
                ));
            }
            if Some(target.version) != proposal["seen"].as_u64() {
                return Ok((
                    Verdict::refused(Reason::PendingTargetChanged),
                    HUMAN_DECISION.into(),
                    Some(pending),
                    Vec::new(),
                ));
            }
            // The observation written to the target's history is charged too.
            let exhausted = charge(&mut target, stamp.cost);
            let target_verdict = if exhausted {
                Verdict::refused(Reason::BudgetExhausted)
            } else {
                target.version += 1;
                if !proposal["state"].is_null() {
                    target.state = proposal["state"].clone();
                }
                if let (Some(task), Ok(phase)) = (
                    &mut target.task,
                    serde_json::from_value::<Phase>(proposal["phase"].clone()),
                ) {
                    task.phase = phase;
                }
                judgement.verdict
            };
            self.store.commit(
                &target_id,
                &Record {
                    stamp: stamp.clone(),
                    submission: Submission::new(&target_id, target.version - 1, Action::Write)
                        .content(json!({"approved": pending.id})),
                    verdict: target_verdict,
                    after: Some(target),
                },
            )?;
            if exhausted {
                return Ok((
                    target_verdict,
                    HUMAN_DECISION.into(),
                    Some(pending),
                    Vec::new(),
                ));
            }
        }
        Ok((
            judgement.verdict,
            HUMAN_DECISION.into(),
            Some(pending),
            Vec::new(),
        ))
    }

    fn has_unknown_effect(&self, endpoint: &str) -> Result<bool, KernelError> {
        for id in self.store.list(&format!("effect/{endpoint}/"))? {
            if let Some(effect) = self.store.load(&id)?
                && effect.state["status"] == "unknown"
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Where this submission's record will stand in its target's history.
    /// Histories only grow, so the place names what the kernel creates for
    /// the record without any counter shared between resources.
    fn place(&self, submission: &Submission) -> Result<String, KernelError> {
        Ok(format!(
            "{:012}",
            self.store.history_len(&submission.target)? + 1
        ))
    }

    fn measure(&self, stamp: &Stamp, submission: &Submission, validator: &str, verdict: Verdict) {
        if let Some(probe) = &self.probe {
            probe.observe(&Point {
                component: submission
                    .target
                    .split('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned(),
                validator: validator.to_owned(),
                principal: stamp.principal.kind,
                action: submission.action,
                effect: verdict.effect,
                verification: verdict.verification,
                reason: verdict.reason,
                cost: stamp.cost,
                at: stamp.at,
                config: stamp.principal.config.clone(),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Task lifecycle: the whole of it
// ---------------------------------------------------------------------------

/// Cost is a projection of history. Only crossing the budget settles a live
/// task and advances its accepted-state version; charging alone never does.
fn charge(resource: &mut Resource, cost: u64) -> bool {
    let Some(task) = &mut resource.task else {
        return false;
    };
    task.budget.spent = task.budget.spent.saturating_add(cost);
    if matches!(task.phase, Phase::Settled { .. }) || task.budget.spent <= task.budget.limit {
        return false;
    }
    task.phase = Phase::Settled {
        result: Outcome::Failed {
            reason: Reason::BudgetExhausted,
            detail: format!("spent {} of {}", task.budget.spent, task.budget.limit),
        },
    };
    resource.version += 1;
    true
}

fn is_lifecycle(submission: &Submission) -> bool {
    submission.action == Action::Request
        && matches!(
            submission.content.get("control").and_then(Value::as_str),
            Some("stop" | "resume" | "abandon")
        )
}

/// - `stop`: a person or an evidenced host observation parks the task for a person.
/// - `resume`: whoever the task waits on releases it — a person for a person,
///   host code for a child task or an observed effect.
/// - `abandon`: a person settles it as aborted.
///
/// Settled tasks never reach here (refused earlier), so "a finished or
/// abandoned Run cannot be resumed" needs no rule of its own.
fn task_control(
    resource: &mut Resource,
    principal: &Principal,
    control: &str,
    content: &Value,
    evidence: &[Evidence],
) -> Result<(), Verdict> {
    let task = resource.task.as_mut().expect("task resource");
    let detail = content
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or(control)
        .to_owned();
    match control {
        "stop" => {
            match principal.kind {
                PrincipalKind::Human => {}
                PrincipalKind::Code => {
                    if !evidence
                        .iter()
                        .any(|e| matches!(e, Evidence::Observation { .. }))
                    {
                        return Err(Verdict::refused(Reason::EvidenceMissing));
                    }
                }
                PrincipalKind::Agent => return Err(Verdict::refused(Reason::HumanOnly)),
            }
            task.phase = Phase::Waiting {
                waiter: Waiter::Human { reason: detail },
            };
        }
        "resume" => {
            let releaser = match &task.phase {
                Phase::Waiting {
                    waiter: Waiter::Human { .. },
                } => PrincipalKind::Human,
                Phase::Waiting { .. } => PrincipalKind::Code,
                _ => return Err(Verdict::refused(Reason::NotWaiting)),
            };
            if principal.kind != releaser {
                return Err(Verdict::refused(if releaser == PrincipalKind::Human {
                    Reason::HumanOnly
                } else {
                    Reason::Unauthorized
                }));
            }
            task.phase = Phase::Executing;
        }
        "abandon" => {
            if principal.kind != PrincipalKind::Human {
                return Err(Verdict::refused(Reason::HumanOnly));
            }
            task.phase = Phase::Settled {
                result: Outcome::Aborted { detail },
            };
        }
        _ => return Err(Verdict::returned(Reason::ContentInvalid)),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Constitutional validators
// ---------------------------------------------------------------------------

struct HumanOnly;

impl Validator for HumanOnly {
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement {
        if ctx.principal.kind != PrincipalKind::Human {
            return Judgement::refused(Reason::HumanOnly);
        }
        Judgement::accept(submission.content.clone(), Verification::Reviewed)
    }
}

struct HumanDecision;

impl Validator for HumanDecision {
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement {
        if ctx.principal.kind != PrincipalKind::Human {
            return Judgement::refused(Reason::HumanOnly);
        }
        if ctx.resource.state.get("decision").is_some() {
            return Judgement::refused(Reason::TaskSettled);
        }
        match submission.content.get("decision").and_then(Value::as_str) {
            Some("approve" | "reject") => {
                Judgement::accept(submission.content.clone(), Verification::Reviewed)
            }
            _ => Judgement::returned(Reason::ContentInvalid),
        }
    }
}

/// authorized → started → succeeded | failed | unknown; unknown → succeeded |
/// failed only with an observation as evidence. Never back to authorized.
struct EffectLifecycle;

impl Validator for EffectLifecycle {
    fn judge(&self, ctx: &JudgeCtx, submission: &Submission) -> Judgement {
        if ctx.principal.kind == PrincipalKind::Agent {
            return Judgement::refused(Reason::Unauthorized);
        }
        let from = ctx.resource.state["status"].as_str().unwrap_or_default();
        let Some(to) = submission.content.get("status").and_then(Value::as_str) else {
            return Judgement::returned(Reason::ContentInvalid);
        };
        let observed = submission
            .evidence
            .iter()
            .any(|e| matches!(e, Evidence::Observation { .. }));
        let allowed = match (from, to) {
            ("authorized", "started") => true,
            ("started", "succeeded" | "failed" | "unknown") => true,
            ("unknown", "succeeded" | "failed") => observed,
            _ => false,
        };
        if !allowed {
            return Judgement::returned(if from == "unknown" {
                Reason::EvidenceMissing
            } else {
                Reason::ContentInvalid
            });
        }
        let mut state = ctx.resource.state.clone();
        state["status"] = json!(to);
        Judgement::accept(
            state,
            if observed {
                Verification::Evidenced
            } else {
                Verification::Asserted
            },
        )
    }
}
