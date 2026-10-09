use super::*;
use anchor_platform_session::{SessionStore, TurnWindow};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, Timelike};

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
    let mut runs = timeline_runs(&state, all_runs, from, to)?;
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

/// The Runs one history page shows, each with the work it actually did.
///
/// A resident assistant instance keeps one Run while its Session lives, so its
/// own `started` is the wrong anchor for the board: placing it there paints
/// every idle wait up to now as execution and hides the Run from pages that no
/// longer contain its start. Such a Run is placed by its Turn windows instead,
/// and reports `activity` even when it has not executed a Turn yet, which is how
/// the board tells "an instance is alive and waiting" from "here is its work".
#[allow(clippy::result_large_err)]
fn timeline_runs(
    state: &ApiState,
    all_runs: Vec<Value>,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> Result<Vec<Value>, HttpResponse> {
    let mut sessions: Option<SessionStore> = None;
    let mut sessions_unavailable = false;
    let mut runs = Vec::new();
    for mut run in all_runs {
        let id = run["run"].as_str().unwrap_or_default().to_owned();
        let started = run
            .get("started")
            .and_then(Value::as_str)
            .and_then(local_projected_datetime);
        let mut activity = Vec::new();
        let resident = resident_instance(state, &run, &id)?;
        if resident {
            if sessions.is_none() && !sessions_unavailable {
                match super::sessions::store(state) {
                    Ok(store) => sessions = Some(store),
                    Err(_) => {
                        eprintln!(
                            "anchor-runner-host: timeline cannot read the Session store; \
                             resident Runs are drawn without Turn windows"
                        );
                        sessions_unavailable = true;
                    }
                }
            }
            if let Some(store) = &sessions {
                match store.turn_windows_for_run(&id) {
                    Ok(windows) => {
                        let active = run["running"].as_bool().unwrap_or(false);
                        activity = activity_windows(windows, from, to, active);
                    }
                    Err(error) => eprintln!(
                        "anchor-runner-host: timeline has no Turn windows for Run {id}: {error}"
                    ),
                }
            }
        }
        if !started.is_some_and(|started| started >= from && started < to) && activity.is_empty() {
            continue;
        }
        if resident {
            run["activity"] = Value::Array(activity);
        }
        runs.push(run);
    }
    Ok(runs)
}

/// Whether this Run is a resident assistant instance.
///
/// `trigger.session` narrows the metadata read to Runs bound to a conversation
/// (channel Messages); pilot Runs are one Run per Turn and carry no session in
/// this projection. Session-call Runs also carry a session, so the assistant
/// binding is what actually decides.
#[allow(clippy::result_large_err)]
fn resident_instance(state: &ApiState, run: &Value, id: &str) -> Result<bool, HttpResponse> {
    if !run["trigger"]["session"].is_string() {
        return Ok(false);
    }
    Ok(state
        .application
        .metadata(id)
        .map_err(application_error)?
        .is_some_and(|metadata| metadata.assistant.is_some()))
}

/// The windows of a resident instance that touch the page, merged and converted
/// to the local clock the rest of the board uses.
///
/// Turns are serial, but a superseded Turn can be created before its predecessor
/// is finished, so adjacent work is merged into one stretch of execution. The
/// merge happens at the board's own resolution (whole seconds, which is also
/// what `format_local` emits): two Turns it cannot tell apart are one bar, not
/// one bar drawn twice. A window is only open when its Turn is still running
/// *and* the host still owns the Run: a crash that leaves a Turn running must
/// not be drawn as work continuing to now.
pub(super) fn activity_windows(
    windows: Vec<TurnWindow>,
    from: NaiveDateTime,
    to: NaiveDateTime,
    active: bool,
) -> Vec<Value> {
    let mut ordered = windows
        .into_iter()
        .map(|window| {
            let start = whole_second(window.created_at.with_timezone(&Local).naive_local());
            let end =
                whole_second(window.updated_at.with_timezone(&Local).naive_local()).max(start);
            (start, end, window.running && active)
        })
        .collect::<Vec<_>>();
    ordered.sort_by_key(|window| window.0);
    let mut merged: Vec<(NaiveDateTime, NaiveDateTime, bool)> = Vec::new();
    for (start, end, running) in ordered {
        match merged.last_mut() {
            Some(last) if start <= last.1 => {
                last.1 = last.1.max(end);
                last.2 = running;
            }
            _ => merged.push((start, end, running)),
        }
    }
    merged
        .into_iter()
        .filter(|(start, end, _)| *end >= from && *start < to)
        .map(|(start, end, running)| {
            json!({
                "start": format_local(start),
                "end": format_local(end),
                "running": running,
            })
        })
        .collect()
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

/// The board draws whole seconds, so sub-second differences are not visible work.
fn whole_second(value: NaiveDateTime) -> NaiveDateTime {
    value.with_nanosecond(0).unwrap_or(value)
}
