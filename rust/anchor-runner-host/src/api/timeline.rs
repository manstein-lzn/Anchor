use super::*;
use chrono::{Duration, Local, NaiveDate, NaiveDateTime};

/// Combine durable Rust Runs with the host-owned local-time schedule projection.
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
    let now = Local::now().naive_local();
    let today = now.date().and_hms_opt(0, 0, 0).expect("midnight is valid");
    let to = query
        .get("before")
        .map(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d"))
        .transpose()
        .map_err(|_| error(StatusCode::BAD_REQUEST, "before must be YYYY-MM-DD"))?
        .map(|date| date.and_hms_opt(0, 0, 0).expect("midnight is valid"))
        .unwrap_or_else(|| today + Duration::days(1));
    let from = to - Duration::days(i64::from(days));
    let future_end = today + Duration::days(8);
    let all_runs = projected_runs(&state).await?;
    let scheduled = timeline_projection(&state, now, from, to, future_end, &all_runs)?;
    let mut runs = all_runs
        .into_iter()
        .filter(|run| {
            run.get("started")
                .and_then(Value::as_str)
                .and_then(local_projected_datetime)
                .is_some_and(|started| started >= from && started < to)
        })
        .collect::<Vec<_>>();
    runs.sort_by(|left, right| right["started"].as_str().cmp(&left["started"].as_str()));
    let schedules = timeline_schedules(&state)?;
    Ok(Json(json!({
        "from": format_local(from),
        "to": format_local(to),
        "runs": runs,
        "scheduled": scheduled,
        "schedules": schedules,
        "capabilities": {"scheduling": true}
    })))
}

fn local_projected_datetime(value: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|date| date.with_timezone(&Local).naive_local())
        })
}

fn format_local(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S").to_string()
}
