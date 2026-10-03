use super::*;

/// Project the durable Rust Run records into the small timeline shape consumed by the
/// existing React workbench. Scheduling mutation/execution is intentionally a separate
/// host capability; an unavailable scheduler must not hide real Graph/Run data.
pub(super) async fn timeline(
    State(state): State<ApiState>,
    Query(query): Query<HashMap<String, String>>,
) -> Result<Json<Value>, HttpResponse> {
    let days = query
        .get("days")
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "days must be an integer"))?
        .unwrap_or(30)
        .clamp(1, 366);
    let runs = projected_runs(&state).await?;
    let now = chrono::Utc::now();
    let to = query
        .get("before")
        .map(|value| chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "before must be YYYY-MM-DD"))?
        .map(|date| {
            date.and_hms_opt(0, 0, 0)
                .expect("midnight is valid")
                .and_utc()
        })
        .unwrap_or_else(|| now + chrono::Duration::days(1));
    let from = to - chrono::Duration::days(i64::from(days));
    let runs = runs
        .into_iter()
        .filter(|run| {
            run.get("started")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<chrono::DateTime<chrono::FixedOffset>>().ok())
                .is_some_and(|started| started >= from && started < to)
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "from": from.to_rfc3339(),
        "to": to.to_rfc3339(),
        "runs": runs,
        "scheduled": [],
        "schedules": [],
        "capabilities": {"scheduling": false}
    })))
}

#[allow(clippy::result_large_err)]
async fn projected_runs(state: &ApiState) -> Result<Vec<Value>, HttpResponse> {
    let active = state.application.active_runs(None).await;
    let mut runs = state
        .application
        .records()
        .map_err(application_error)?
        .into_iter()
        .map(|(id, record)| {
            let metadata = state.application.metadata(&id).map_err(application_error)?;
            let graph = metadata
                .as_ref()
                .map(|metadata| metadata.graph.as_str())
                .unwrap_or("unknown");
            let started = metadata
                .as_ref()
                .map(|metadata| metadata.created.as_str())
                .unwrap_or("");
            let source = metadata
                .as_ref()
                .map(|metadata| metadata.trigger_source.as_str())
                .unwrap_or("unknown");
            let executed = record
                .results
                .values()
                .flat_map(|items| {
                    items
                        .iter()
                        .map(|item| (item.sequence, item.node_id.clone()))
                })
                .collect::<std::collections::BTreeMap<_, _>>()
                .into_values()
                .collect::<Vec<_>>();
            Ok(json!({
                "run": id,
                "graph": graph,
                "status": super::status(record.status),
                "running": active.contains(&id),
                "started": started,
                "updated": "",
                "executed": executed,
                "objective": record.snapshot.objective,
                "trigger": {"source": source}
            }))
        })
        .collect::<Result<Vec<_>, HttpResponse>>()?;
    runs.sort_by(|left, right| right["started"].as_str().cmp(&left["started"].as_str()));
    Ok(runs)
}
