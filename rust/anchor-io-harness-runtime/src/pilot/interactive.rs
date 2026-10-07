use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PilotQuestionRef {
    pub scope: PathBuf,
    pub session_id: i64,
    pub turn_id: i64,
    pub run_id: i64,
    pub question_id: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct PilotPendingQuestion {
    pub identity: PilotQuestionRef,
    pub question: String,
    pub context: Option<String>,
    pub choices: Vec<io_harness::Choice>,
}

#[derive(Debug, Clone)]
pub struct PilotAnswer {
    pub question: PilotQuestionRef,
    pub answer: String,
}

#[derive(Debug, Clone)]
pub enum InteractivePilotOutcome {
    Finished(PilotOutcome),
    AwaitingAnswer(PilotPendingQuestion),
}

/// Opt in to the framework's singular question tool. Host tools are unchanged.
/// Step and token budgets are io-harness budgets, not a provider-request quota.
pub async fn run_interactive_pilot(
    request: PilotRequest,
    provider: &RigProviderAdapter,
    port: Arc<dyn ToolPort>,
    observer: Arc<dyn PilotObserver>,
) -> Result<InteractivePilotOutcome, String> {
    let max_steps = validate_request(&request)?;
    validate_port(&port)?;
    let paths = PilotPaths::new(&request.root)?;
    paths.ensure_root()?;
    let _scope_lease = paths.acquire()?;
    let (store, mut session) = paths.open_or_create()?;
    attach_unfinished_turn(&store, &mut session)?;
    if pending(&request.root, &store, &session)?.is_some() {
        return Err("Pilot has a pending native question; answer the existing turn".into());
    }
    let projection = interactive_projection(observer, session.id());
    let guarded: Arc<dyn ToolPort> = Arc::new(GuardedPort {
        port,
        projection: Arc::clone(&projection),
    });
    let (policy, contract) = pilot_contract(&request, &store, &session, &guarded, max_steps, true)?;
    let provider = interactive_provider(provider, &guarded);
    let broadcast = Broadcast::new(
        Store::open(&paths.store).map_err(native_error)?,
        projection.as_ref(),
    );
    let result = tokio::time::timeout(
        request.wall_time,
        session.turn_bounded_observed(
            &contract,
            &provider,
            &store,
            &policy,
            &ApproveAll,
            &broadcast,
        ),
    )
    .await;
    attach_unfinished_turn(&store, &mut session)?;
    if let Some(error) = projection.error() {
        return Ok(InteractivePilotOutcome::Finished(pilot_failure(error)));
    }
    let outcome = match result {
        Err(_) => pilot_timeout(),
        Ok(Err(error)) => pilot_failure(native_error(error)),
        Ok(Ok(turn)) => {
            if let RunOutcome::AwaitingAnswer { question_id, .. } = turn.outcome {
                return waiting_outcome(&request.root, &store, &session, question_id, &projection);
            }
            pilot_outcome(turn.outcome, turn.reply)
        }
    };
    finished_outcome(outcome, &projection)
}

/// Read the native head's pending question without creating a scope or a wait row.
pub fn pilot_pending_question(root: &Path) -> Result<Option<PilotPendingQuestion>, String> {
    let paths = PilotPaths::new(root)?;
    if !paths.root_exists()? {
        return Ok(None);
    }
    let Some((store, session)) = paths.open_existing()? else {
        return Ok(None);
    };
    pending(root, &store, &session)
}

/// Resume the original native turn; `request.prompt` must match its saved prompt.
/// An already accepted answer is rejected, even if its text is identical. After
/// interruption, callers must inspect native recovery rather than resubmit it.
/// The native Session head must still reference this turn. `finish_turn` updates
/// only its reply/outcome; it neither creates a turn nor advances the head.
pub async fn resume_pilot_with_answer(
    request: PilotRequest,
    answer: PilotAnswer,
    provider: &RigProviderAdapter,
    port: Arc<dyn ToolPort>,
    observer: Arc<dyn PilotObserver>,
) -> Result<InteractivePilotOutcome, String> {
    let max_steps = validate_request(&request)?;
    validate_port(&port)?;
    if answer.answer.trim().is_empty() || answer.question.scope != request.root {
        return Err("Pilot answer requires nonempty text in the original scope".into());
    }
    let paths = PilotPaths::new(&request.root)?;
    if !paths.root_exists()? {
        return Err("Pilot answer scope does not exist".into());
    }
    let _scope_lease = paths.acquire()?;
    let (store, session) = paths
        .open_existing()?
        .ok_or_else(|| "Pilot answer has no native Session".to_owned())?;
    let identity = &answer.question;
    let turn = store
        .session_turn(identity.turn_id)
        .map_err(native_error)?
        .ok_or_else(|| "Pilot answer has no native turn".to_owned())?;
    let question = store
        .question(identity.question_id)
        .map_err(native_error)?
        .ok_or_else(|| "Pilot answer has no native question".to_owned())?;
    if identity.session_id != session.id()
        || turn.session_id != session.id()
        || turn.run_id != identity.run_id
        || question.run_id != identity.run_id
        || session.head() != Some(turn.id)
        || turn.prompt != request.prompt
    {
        return Err(
            "Pilot answer scope/Session/turn/run/question does not match the native head".into(),
        );
    }
    if question.resolved || question.answer.is_some() {
        return Err(
            "Pilot native question was already answered; refusing to drive it again".into(),
        );
    }
    let waiting = pending(&request.root, &store, &session)?
        .ok_or_else(|| "Pilot native run is not awaiting an answer".to_owned())?;
    if waiting.identity != *identity {
        return Err("Pilot answer does not match the pending native question".into());
    }
    store
        .check_resumable(identity.run_id)
        .map_err(native_error)?;
    let projection = interactive_projection(observer, session.id());
    let guarded: Arc<dyn ToolPort> = Arc::new(GuardedPort {
        port,
        projection: Arc::clone(&projection),
    });
    let (policy, contract) = pilot_contract(&request, &store, &session, &guarded, max_steps, true)?;
    let provider = interactive_provider(provider, &guarded);
    let _native_lease = store
        .acquire_lease(identity.run_id, contract.lease_ttl.as_secs() as i64)
        .map_err(native_error)?;
    projection
        .observer
        .native_run(session.id(), identity.run_id)?;
    projection.emit(json!({"type":"resume-start", "runId":identity.run_id}))?;
    if projection.cancelled() {
        return Err("Pilot answer cancelled before native answer acceptance".into());
    }
    let broadcast = Broadcast::new(
        Store::open(&paths.store).map_err(native_error)?,
        projection.as_ref(),
    );
    let result = tokio::time::timeout(
        request.wall_time,
        io_harness::resume_with_answer_observed(
            &contract,
            &provider,
            &store,
            identity.run_id,
            identity.question_id,
            &answer.answer,
            &policy,
            &ApproveAll,
            &broadcast,
        ),
    )
    .await;
    let _history_lease = store
        .acquire_lease(identity.run_id, contract.lease_ttl.as_secs() as i64)
        .map_err(native_error)?;
    let reply = native_reply(&store, identity.run_id)?;
    let status = store
        .outcome(identity.run_id)
        .map_err(native_error)?
        .ok_or_else(|| "Pilot native run disappeared after answer acceptance".to_owned())?;
    store
        .finish_turn(identity.turn_id, reply.as_deref(), &status)
        .map_err(native_error)?;
    if let Some(error) = projection.error() {
        return Ok(InteractivePilotOutcome::Finished(pilot_failure(error)));
    }
    let outcome = match result {
        Err(_) => pilot_timeout(),
        Ok(Err(error)) => pilot_failure(native_error(error)),
        Ok(Ok(run)) => {
            if let RunOutcome::AwaitingAnswer { question_id, .. } = run.outcome {
                return waiting_outcome(&request.root, &store, &session, question_id, &projection);
            }
            pilot_outcome(run.outcome, reply)
        }
    };
    finished_outcome(outcome, &projection)
}

fn validate_request(request: &PilotRequest) -> Result<u32, String> {
    let max_steps = u32::try_from(request.max_steps)
        .ok()
        .filter(|steps| *steps > 0)
        .ok_or_else(|| "Pilot max_steps must be in 1..=u32::MAX".to_owned())?;
    if request.prompt.trim().is_empty() || request.max_tokens == 0 || request.wall_time.is_zero() {
        return Err("Pilot requires a prompt and nonzero token/time budgets".into());
    }
    Ok(max_steps)
}

fn validate_port(port: &Arc<dyn ToolPort>) -> Result<(), String> {
    if port.definitions().iter().any(|tool| {
        matches!(
            tool.name.as_str(),
            "session_ask" | io_harness::ASK_QUESTION_TOOL
        )
    }) {
        return Err("Pilot interactive question names are reserved for native io-harness".into());
    }
    Ok(())
}

fn interactive_provider(
    provider: &RigProviderAdapter,
    port: &Arc<dyn ToolPort>,
) -> RigProviderAdapter {
    provider
        .clone()
        .with_pilot_question_alias()
        .with_tool_catalog(
            port.definitions()
                .into_iter()
                .map(|tool| tool.name)
                .chain(std::iter::once("session_ask".to_owned())),
        )
}

fn interactive_projection(observer: Arc<dyn PilotObserver>, session_id: i64) -> Arc<Projection> {
    let mut projection = Projection::new(observer, session_id);
    projection.interactive = true;
    Arc::new(projection)
}

pub(super) fn external_name(name: &str, interactive: bool) -> &str {
    if interactive && name == io_harness::ASK_QUESTION_TOOL {
        "session_ask"
    } else {
        name
    }
}

fn pending(
    root: &Path,
    store: &Store,
    session: &Session,
) -> Result<Option<PilotPendingQuestion>, String> {
    let Some(turn_id) = session.head() else {
        return Ok(None);
    };
    let turn = store
        .session_turn(turn_id)
        .map_err(native_error)?
        .ok_or_else(|| "Pilot native Session head has no turn".to_owned())?;
    if store.outcome(turn.run_id).map_err(native_error)?.as_deref() != Some("awaiting_answer") {
        return Ok(None);
    }
    let questions = store.questions(turn.run_id).map_err(native_error)?;
    let mut unresolved = questions.into_iter().filter(|question| !question.resolved);
    let question = unresolved.next().ok_or_else(|| {
        "Pilot native run has no unresolved question; inspect recovery".to_owned()
    })?;
    if unresolved.next().is_some() || !question.questions.is_empty() {
        return Err("Pilot supports one singular pending native question".into());
    }
    Ok(Some(PilotPendingQuestion {
        identity: PilotQuestionRef {
            scope: root.into(),
            session_id: session.id(),
            turn_id,
            run_id: turn.run_id,
            question_id: question.id,
        },
        question: question.question,
        context: question.context,
        choices: question.choices,
    }))
}

fn waiting_outcome(
    root: &Path,
    store: &Store,
    session: &Session,
    question_id: i64,
    projection: &Projection,
) -> Result<InteractivePilotOutcome, String> {
    let question = pending(root, store, session)?
        .filter(|question| question.identity.question_id == question_id)
        .ok_or_else(|| "Pilot wait outcome does not match its native question".to_owned())?;
    projection.emit(json!({"type":"session-awaiting-answer", "question":question}))?;
    Ok(InteractivePilotOutcome::AwaitingAnswer(question))
}

fn native_reply(store: &Store, run_id: i64) -> Result<Option<String>, String> {
    Ok(store
        .step_turns(run_id)
        .map_err(native_error)?
        .into_iter()
        .rev()
        .find(|step| {
            step.calls.is_empty()
                && step
                    .text
                    .as_deref()
                    .is_some_and(|text| !text.trim().is_empty())
        })
        .and_then(|step| step.text)
        .map(|text| text.trim().to_owned()))
}

fn finished_outcome(
    outcome: PilotOutcome,
    projection: &Projection,
) -> Result<InteractivePilotOutcome, String> {
    if let Some(error) = &outcome.error {
        projection.emit(json!({"type":"error", "errorText":error}))?;
    }
    Ok(InteractivePilotOutcome::Finished(outcome))
}
