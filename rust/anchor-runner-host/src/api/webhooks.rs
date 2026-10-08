use super::*;

async fn busy_run(state: &ApiState, graph: &str) -> Result<Option<String>, HttpResponse> {
    if let Some(run) = state
        .application
        .active_runs(Some(graph))
        .await
        .into_iter()
        .next()
    {
        return Ok(Some(run));
    }
    let mut candidates = state
        .application
        .records()
        .map_err(application_error)?
        .into_iter()
        .filter(|(_, record)| {
            matches!(
                record.status,
                RunStatus::Ready
                    | RunStatus::Running
                    | RunStatus::Paused
                    | RunStatus::BudgetStopped
                    | RunStatus::WaitingCall
                    | RunStatus::WaitingRecovery
                    | RunStatus::Stopped
            )
        })
        .filter_map(|(run, _)| match state.application.metadata(&run) {
            Ok(Some(metadata)) if metadata.graph == graph => Some(Ok(run)),
            Ok(_) => None,
            Err(failure) => Some(Err(application_error(failure))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    candidates.sort();
    Ok(candidates.into_iter().next())
}

pub(super) async fn graph_webhook(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(graph): AxumPath<String>,
    body: Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let Json(body) = body.map_err(|_| error(StatusCode::BAD_REQUEST, "invalid webhook request"))?;
    let Some(fields) = body.as_object() else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "body must contain only an object input",
        ));
    };
    if fields.keys().any(|field| field != "input") {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "body must contain only an object input",
        ));
    }
    let input = match fields.get("input") {
        None => json!({}),
        Some(Value::Object(input)) => Value::Object(input.clone()),
        _ => {
            return Err(error(
                StatusCode::BAD_REQUEST,
                "body must contain only an object input",
            ));
        }
    };
    load_graph_definition(&state, &graph)?;
    if let Some(run) = busy_run(&state, &graph).await? {
        return Ok((
            StatusCode::CONFLICT,
            Json(json!({"error":"this graph is already running","running":run})),
        ));
    }
    let request = json!({"graph":graph,"input":input});
    match trigger_as_owner(
        state.clone(),
        request,
        super::oauth::binding_owner(&private_owner(&state, &headers)),
    )
    .await
    {
        Ok(accepted) => Ok(accepted),
        Err(failure) if failure.status() == StatusCode::CONFLICT => {
            if let Some(run) = busy_run(&state, &graph).await? {
                Ok((
                    StatusCode::CONFLICT,
                    Json(json!({"error":"this graph is already running","running":run})),
                ))
            } else {
                Err(failure)
            }
        }
        Err(failure) => Err(failure),
    }
}
