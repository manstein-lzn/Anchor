use super::*;

pub(super) async fn trigger(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let requested = body
        .get("graph")
        .and_then(Value::as_str)
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "graph is required"))?;
    let input = body.get("input").cloned().unwrap_or(Value::Null);
    if !input.is_null() && !input.is_object() {
        return Err(error(StatusCode::BAD_REQUEST, "input must be an object"));
    }
    let path = graph_path(&state, requested)?;
    let graph_lease = state
        .application
        .graph_admission_lease(&path)
        .map_err(application_error)?;
    let (path, bundle) = load_graph_definition(&state, requested)?;
    let run_id = state
        .application
        .admit(requested.to_owned(), &path, bundle, input, graph_lease)
        .await
        .map_err(application_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run":run_id,"graph":requested})),
    ))
}

pub(super) async fn list_runs(State(state): State<ApiState>) -> Result<Json<Value>, HttpResponse> {
    let active = state.application.active_runs(None).await;
    let mut runs = Vec::new();
    for (id, record) in state.application.records().map_err(application_error)? {
        let metadata = state.application.metadata(&id).map_err(application_error)?;
        let (graph, created, mut trigger) = metadata
            .as_ref()
            .map(|metadata| (
                metadata.graph.clone(),
                metadata.created.clone(),
                json!({"source":metadata.trigger_source,"schedule":metadata.schedule,"scheduled_at":metadata.scheduled_at}),
            ))
            .unwrap_or_else(|| (
                "unknown".into(),
                String::new(),
                json!({"source":"unknown"}),
            ));
        if let Some(source) = metadata
            .as_ref()
            .and_then(|metadata| metadata.graph_call.as_ref())
        {
            trigger["graph"] = json!(source.parent_graph);
            trigger["run"] = json!(source.parent_run);
            trigger["node"] = json!(source.node);
            trigger["invocation"] = json!(source.invocation);
            trigger["mode"] = json!(source.mode);
            trigger["root_run"] = json!(source.root_run);
        }
        let executed = record.executed_nodes();
        runs.push(json!({"run":id,"graph":graph,"status":status(record.status),"running":active.contains(&id),"started":created,"updated":"","executed":executed,"objective":record.snapshot.objective,"trigger":trigger}));
    }
    Ok(Json(json!({"runs":runs})))
}

pub(super) async fn get_run(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    if id.is_empty() || id.contains('/') || id == "." || id == ".." {
        return Err(error(StatusCode::BAD_REQUEST, "invalid run id"));
    }
    let record = FileRunStore::new(state.data_root.join("runs"))
        .load(&id)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such run"))?;
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    let mut nodes = serde_json::Map::new();
    for (name, results) in &record.results {
        if let Some(result) = results.last() {
            let files = artifacts
                .list_files(&result.commit)
                .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
            nodes.insert(
                name.clone(),
                json!({
                    "node_id":name,
                    "pass_number":result.key.invocation,
                    "submission":result.completion.submission,
                    "files":files.into_iter().map(|(path, _)| path).collect::<Vec<_>>(),
                    "submitted":true,
                    "exit_status":Value::Null,
                    "route":result.completion.route,
                    "commit":result.commit.id,
                    "inputs":[]
                }),
            );
        }
    }
    let decided = record
        .decided
        .iter()
        .map(|(key, decision)| (key.clone(), json!([decision.selected, decision.sequence])))
        .collect::<serde_json::Map<_, _>>();
    let cursor = record
        .cursor
        .as_ref()
        .map(|cursor| json!({"node":cursor.node_id,"pass":cursor.key.invocation,"dir":""}));
    let executed = record.executed_nodes();
    let mut traces = serde_json::Map::new();
    let mut trace_keys = record
        .results
        .values()
        .flatten()
        .map(|result| result.key.clone())
        .collect::<Vec<_>>();
    if let Some(cursor) = &record.cursor {
        trace_keys.push(cursor.key.clone());
    }
    if let Some(parallel) = &record.parallel {
        trace_keys.extend(
            parallel
                .branches
                .iter()
                .filter_map(|branch| branch.cursor.as_ref().map(|cursor| cursor.key.clone())),
        );
    }
    trace_keys.sort_by_key(|key| key.durable_key());
    trace_keys.dedup();
    for key in trace_keys {
        let messages = anchor_io_harness_runtime::node_port::trace_messages(
            &state.data_root.join("io-harness/store"),
            &key,
        )
        .map_err(|message| error(StatusCode::INTERNAL_SERVER_ERROR, message))?;
        if !messages.is_empty() {
            let trace_key = serde_json::to_string(&(&key.node_id, key.invocation))
                .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            traces.insert(trace_key, Value::Array(messages));
        }
    }
    let active = state.application.active_runs(None).await;
    let metadata = state
        .application
        .metadata(&id)
        .map_err(application_error)?
        .ok_or_else(|| {
            error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Run has no immutable Graph identity metadata",
            )
        })?;
    let mut calls = Vec::new();
    for call in record.graph_calls.values() {
        if let Some(child_id) = &call.child_run_id {
            let child = FileRunStore::new(state.data_root.join("runs"))
                .load(child_id)
                .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let source = state
                .application
                .metadata(child_id)
                .map_err(application_error)?
                .and_then(|metadata| metadata.graph_call);
            calls.push(json!({
                "run":child_id,
                "child_id":child_id,
                "graph":call.graph,
                "node":call.identity.node_id,
                "op":call.identity.node_id,
                "target":call.graph,
                "invocation":call.identity.invocation,
                "mode":call.mode,
                "status":child.as_ref().map(|record| status(record.status)).unwrap_or("missing"),
                "active":active.contains(child_id),
                "root_run":source.map(|source| source.root_run),
            }));
        }
    }
    for child_metadata in state
        .application
        .child_metadata(&id)
        .map_err(application_error)?
    {
        let Some(source) = child_metadata.graph_call else {
            continue;
        };
        if calls
            .iter()
            .any(|call| call["run"] == child_metadata.run_id)
        {
            continue;
        }
        let child = FileRunStore::new(state.data_root.join("runs"))
            .load(&child_metadata.run_id)
            .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        calls.push(json!({
            "run":child_metadata.run_id,
            "child_id":child_metadata.run_id,
            "graph":child_metadata.graph,
            "node":source.node,
            "op":source.node,
            "target":child_metadata.graph,
            "invocation":source.invocation,
            "mode":source.mode,
            "status":child.as_ref().map(|record| status(record.status)).unwrap_or("missing"),
            "active":active.contains(&child_metadata.run_id),
            "root_run":source.root_run,
        }));
    }
    let mut trigger = json!({"source":metadata.trigger_source});
    if let Some(source) = metadata.graph_call.as_ref() {
        trigger["graph"] = json!(source.parent_graph);
        trigger["run"] = json!(source.parent_run);
        trigger["node"] = json!(source.node);
        trigger["invocation"] = json!(source.invocation);
        trigger["mode"] = json!(source.mode);
        trigger["root_run"] = json!(source.root_run);
    }
    Ok(Json(
        json!({"graph":metadata.graph,"run":id,"state":{"objective":record.snapshot.objective,"started":metadata.created,"updated":"","status":status(record.status),"trigger":trigger,"input":record.input,"cursor":cursor,"passes":record.passes,"decided":decided,"nodes":nodes,"executed":executed,"skipped":[],"error":record.error.unwrap_or_default(),"parallel":record.parallel,"recovery":record.recovery},"calls":calls,"traces":traces,"nodes":record.snapshot.nodes.iter().map(|node|node.id.clone()).collect::<Vec<_>>(),"active":active.contains(&id)}),
    ))
}

pub(super) async fn delete_run(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
) -> Result<StatusCode, HttpResponse> {
    if id.is_empty()
        || id == "."
        || id == ".."
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(error(StatusCode::NOT_FOUND, "no such run"));
    }
    if state.application.active_runs(None).await.contains(&id) {
        return Err(error(StatusCode::CONFLICT, "that Run is still running"));
    }
    let store = FileRunStore::new(state.data_root.join("runs"));
    let record = store
        .load(&id)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| error(StatusCode::NOT_FOUND, "no such run"))?;
    if !record.graph_calls.is_empty() {
        return Err(error(
            StatusCode::CONFLICT,
            "Run is referenced by or owns a Graph call",
        ));
    }
    for (other_id, other) in state.application.records().map_err(application_error)? {
        if other_id != id
            && other.graph_calls.values().any(|call| {
                call.child_run_id.as_deref() == Some(id.as_str())
                    && call.status != anchor_runtime_rig::graph::GraphCallStatus::Deleted
            })
        {
            return Err(error(
                StatusCode::CONFLICT,
                "Run is referenced by another Graph call",
            ));
        }
    }
    let _lease = store
        .acquire_lease(&id)
        .map_err(|e| error(StatusCode::CONFLICT, e.to_string()))?;
    let artifacts = HostArtifacts::new(
        state.data_root.join("artifacts"),
        state.workspace_root.clone(),
    );
    for result in record.results.values().flatten() {
        // Validate a still-present artifact's identity; a Run that was already
        // partially cleaned up can always be re-deleted.
        let artifact = state.data_root.join("artifacts").join(&result.commit.id);
        if artifact.exists() {
            artifacts
                .list_files(&result.commit)
                .map_err(|e| error(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
        }
        if result.key.run_id != id
            || result.key.graph_digest != record.graph_digest
            || result.commit.node_id != result.key.node_id
            || result.commit.invocation != result.key.invocation
        {
            return Err(error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Run artifact identity mismatch",
            ));
        }
    }
    // Derived data first, Run record last: a crash mid-cleanup stays replayable.
    crate::run_data::delete_run_data(&record, &state.data_root, &state.workspace_root)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    crate::run_data::remove_run_files(&state.data_root, &id)
        .map_err(|e| error(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RecoveryBody {
    node_id: String,
    invocation: u64,
    attempt_id: i64,
    decision: String,
    #[serde(default)]
    observation: Option<String>,
}

pub(super) async fn recover_run(
    State(state): State<ApiState>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<RecoveryBody>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let decision = match body.decision.as_str() {
        "retry" if body.observation.is_none() => anchor_runtime_rig::graph::RecoveryDecision::Retry,
        "completed"
            if body
                .observation
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty()) =>
        {
            let observation = body.observation.expect("validated observation");
            anchor_runtime_rig::graph::RecoveryDecision::Completed { observation }
        }
        "abort" if body.observation.is_none() => anchor_runtime_rig::graph::RecoveryDecision::Abort,
        "retry" | "completed" | "abort" => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "completed requires a non-empty observation; other decisions do not accept one",
            ));
        }
        _ => return Err(error(StatusCode::BAD_REQUEST, "unknown recovery decision")),
    };
    state
        .application
        .recover(
            &id,
            &body.node_id,
            body.invocation,
            body.attempt_id,
            decision,
        )
        .await
        .map_err(application_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run":id,"accepted":true})),
    ))
}

pub(super) async fn control(
    State(state): State<ApiState>,
    AxumPath((id, operation)): AxumPath<(String, String)>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    if !matches!(operation.as_str(), "pause" | "resume" | "stop") {
        return Err(error(StatusCode::BAD_REQUEST, "unknown Run control"));
    }
    state
        .application
        .control(&id, &operation)
        .await
        .map_err(application_error)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({"run":id,"asked":operation})),
    ))
}
