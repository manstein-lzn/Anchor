use super::*;
use chrono::{Datelike, Duration, Local, NaiveDateTime, NaiveTime, Timelike};
use rustix::fs::{FlockOperation, flock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration as StdDuration,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(super) type ScheduleStoreHandle = Arc<Mutex<ScheduleStore>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ScheduleItem {
    pub(super) id: String,
    pub(super) graph: String,
    pub(super) rule: Value,
    #[serde(default = "empty_object")]
    pub(super) input: Value,
    pub(super) created_at: String,
    pub(super) next_at: String,
    #[serde(default = "default_enabled")]
    pub(super) enabled: bool,
}

fn empty_object() -> Value {
    json!({})
}
fn default_enabled() -> bool {
    true
}

pub(super) struct ScheduleStore {
    pub(super) path: PathBuf,
    pub(super) items: Vec<ScheduleItem>,
    _owner_lease: fs::File,
}

impl ScheduleStore {
    pub(super) fn open(path: PathBuf) -> Result<ScheduleStoreHandle, String> {
        let requested_parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(requested_parent)
            .map_err(|error| format!("create schedules directory: {error}"))?;
        let parent = fs::canonicalize(requested_parent)
            .map_err(|error| format!("resolve schedules directory: {error}"))?;
        let file_name = path
            .file_name()
            .ok_or_else(|| "schedules path must name a file".to_owned())?;
        let path = parent.join(file_name);
        let lease_path = parent.join(format!("{}.lock", file_name.to_string_lossy()));
        let owner_lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lease_path)
            .map_err(|error| format!("open schedules owner lease: {error}"))?;
        flock(&owner_lease, FlockOperation::NonBlockingLockExclusive)
            .map_err(|error| format!("schedules file already has a Host owner: {error}"))?;
        let create_empty = !path.exists();
        let items = if !create_empty {
            let bytes = fs::read(&path).map_err(|error| format!("read schedules: {error}"))?;
            serde_json::from_slice::<Vec<ScheduleItem>>(&bytes)
                .map_err(|error| format!("invalid schedules JSON: {error}"))?
        } else {
            Vec::new()
        };
        for item in &items {
            if item.id.is_empty() || item.graph.is_empty() || !item.input.is_object() {
                return Err("invalid schedule: id, graph and object input are required".into());
            }
            parse_local_datetime(&item.created_at)
                .map_err(|_| format!("invalid schedule {} created_at", item.id))?;
            parse_local_datetime(&item.next_at)
                .map_err(|_| format!("invalid schedule {} next_at", item.id))?;
            normalize_rule(&item.rule, None)
                .map_err(|message| format!("invalid schedule {} rule: {message}", item.id))?;
        }
        // The file lease supplements the deployment writer lease: different
        // state roots may still be configured to share this exact JSON path.
        let store = Arc::new(Mutex::new(Self {
            path,
            items,
            _owner_lease: owner_lease,
        }));
        if create_empty {
            store
                .lock()
                .map_err(|_| "schedule store unavailable")?
                .persist()?;
        }
        Ok(store)
    }

    pub(super) fn persist(&self) -> Result<(), String> {
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .map_err(|error| format!("create schedules directory: {error}"))?;
        let bytes = serde_json::to_vec_pretty(&self.items)
            .map_err(|error| format!("serialize schedules: {error}"))?;
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let file_name = self.path.file_name().unwrap_or_default().to_string_lossy();
        let temporary = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("create schedules temporary file: {error}"))?;
        let write_result = (|| -> std::io::Result<()> {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, &self.path)?;
            if let Err(error) = fs::File::open(parent).and_then(|directory| directory.sync_all()) {
                // rename has published the new snapshot, so callers must keep
                // their in-memory copy aligned even if directory durability
                // cannot be confirmed on this filesystem.
                eprintln!("schedule snapshot published; directory sync failed: {error}");
            }
            Ok(())
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(format!("persist schedules: {error}"));
        }
        Ok(())
    }
}

pub(super) async fn list_schedules(
    State(state): State<ApiState>,
) -> Result<Json<Value>, HttpResponse> {
    let schedules = state.schedules.lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "schedule store unavailable",
        )
    })?;
    Ok(Json(json!({"schedules": &schedules.items})))
}

pub(super) async fn create_schedule(
    State(state): State<ApiState>,
    Json(body): Json<Value>,
) -> Result<(StatusCode, Json<Value>), HttpResponse> {
    let Some(object) = body.as_object() else {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "provide graph, rule, and optional object input",
        ));
    };
    if object
        .keys()
        .any(|key| !["graph", "rule", "input"].contains(&key.as_str()))
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "unsupported schedule fields",
        ));
    }
    let graph = object
        .get("graph")
        .and_then(Value::as_str)
        .filter(|graph| !graph.trim().is_empty())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "graph is required"))?;
    let input = object.get("input").cloned().unwrap_or_else(empty_object);
    if !input.is_object() {
        return Err(error(StatusCode::BAD_REQUEST, "input must be an object"));
    }
    let rule = object
        .get("rule")
        .filter(|rule| rule.is_object())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "rule must be an object"))?;

    load_graph_definition(&state, graph)
        .map_err(|_| error(StatusCode::NOT_FOUND, format!("no such graph: {graph}")))?;
    let precise_now = Local::now().naive_local();
    let normalized_rule = normalize_rule(rule, Some(precise_now))
        .map_err(|message| error(StatusCode::BAD_REQUEST, message))?;
    let next_at = next_after(&normalized_rule, precise_now)
        .map_err(|message| error(StatusCode::BAD_REQUEST, message))?;
    let created_at = precise_now
        .with_nanosecond(0)
        .expect("zero nanoseconds is valid");
    let item = ScheduleItem {
        id: unique_schedule_id(),
        graph: graph.to_owned(),
        rule: normalized_rule,
        input,
        created_at: format_local_datetime(created_at),
        next_at: format_local_datetime(next_at),
        enabled: true,
    };
    let mut store = state.schedules.lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "schedule store unavailable",
        )
    })?;
    let previous = store.items.clone();
    store.items.push(item.clone());
    if let Err(message) = store.persist() {
        store.items = previous;
        return Err(error(StatusCode::INTERNAL_SERVER_ERROR, message));
    }
    Ok((StatusCode::CREATED, Json(json!({"schedule": item}))))
}

pub(super) async fn delete_schedule(
    State(state): State<ApiState>,
    AxumPath(identifier): AxumPath<String>,
) -> Result<Json<Value>, HttpResponse> {
    let mut store = state.schedules.lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "schedule store unavailable",
        )
    })?;
    let old_len = store.items.len();
    let previous = store.items.clone();
    store.items.retain(|item| item.id != identifier);
    if store.items.len() == old_len {
        return Err(error(StatusCode::NOT_FOUND, "no such schedule"));
    }
    if let Err(message) = store.persist() {
        store.items = previous;
        return Err(error(StatusCode::INTERNAL_SERVER_ERROR, message));
    }
    Ok(Json(json!({"schedule": identifier, "deleted": true})))
}

pub(super) fn skip_missed_schedules(state: &ApiState, now: NaiveDateTime) -> Result<(), String> {
    let mut store = state
        .schedules
        .lock()
        .map_err(|_| "schedule store unavailable")?;
    let previous = store.items.clone();
    let mut changed = false;
    for item in &mut store.items {
        if !item.enabled
            || parse_local_datetime(&item.next_at).map_err(|message| message.to_string())? > now
        {
            continue;
        }
        if rule_type(&item.rule)? == "once" {
            item.enabled = false;
        } else {
            let next = next_after(&item.rule, now)?;
            item.next_at = format_local_datetime(next);
        }
        changed = true;
    }
    if !changed {
        return Ok(());
    }
    if let Err(message) = store.persist() {
        store.items = previous;
        return Err(message);
    }
    Ok(())
}

pub(super) fn start_schedule_ticker(state: ApiState) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(StdDuration::from_secs(1));
        loop {
            ticker.tick().await;
            if let Err(message) = tick_schedules(&state, Local::now().naive_local()).await {
                eprintln!("schedule tick failed: {message}");
            }
        }
    });
}

pub(super) async fn tick_schedules(state: &ApiState, now: NaiveDateTime) -> Result<(), String> {
    let due = {
        let mut store = state
            .schedules
            .lock()
            .map_err(|_| "schedule store unavailable")?;
        let mut due = Vec::new();
        let previous = store.items.clone();
        let mut changed = false;
        for item in &mut store.items {
            if !item.enabled {
                continue;
            }
            let at = parse_local_datetime(&item.next_at)?;
            if at > now {
                continue;
            }
            if rule_type(&item.rule)? == "once" {
                item.enabled = false;
            } else {
                item.next_at = format_local_datetime(next_after(&item.rule, now)?);
            }
            changed = true;
            if now - at <= Duration::seconds(1) {
                due.push((item.graph.clone(), item.input.clone(), item.id.clone(), at));
            }
        }
        if changed && let Err(message) = store.persist() {
            store.items = previous;
            return Err(message);
        }
        due
    };
    for (graph, input, identifier, at) in due {
        let payload = json!({
            "graph": graph,
            "input": input,
            "trigger": {
                "source": "schedule",
                "schedule": identifier,
                "scheduled_at": format_local_datetime(at),
            }
        });
        match super::trigger(State(state.clone()), Json(payload)).await {
            Ok((status, _)) if status.is_success() => {}
            Ok((status, _)) => eprintln!("schedule occurrence skipped with HTTP {status}"),
            Err(response) => eprintln!(
                "schedule occurrence skipped with HTTP {}",
                response.status()
            ),
        }
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
pub(super) fn timeline_projection(
    state: &ApiState,
    now: NaiveDateTime,
    start: NaiveDateTime,
    end: NaiveDateTime,
    future_end: NaiveDateTime,
    runs: &[Value],
) -> Result<Vec<Value>, HttpResponse> {
    let schedules = state.schedules.lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "schedule store unavailable",
        )
    })?;
    let mut projected = Vec::new();
    for item in &schedules.items {
        let created = parse_local_datetime(&item.created_at)
            .map_err(|message| error(StatusCode::INTERNAL_SERVER_ERROR, message))?;
        let mut candidates = occurrences(&item.rule, created, start, end)
            .map_err(|message| error(StatusCode::INTERNAL_SERVER_ERROR, message))?;
        candidates.extend(
            occurrences(
                &item.rule,
                created,
                now.date().and_hms_opt(0, 0, 0).unwrap(),
                future_end,
            )
            .map_err(|message| error(StatusCode::INTERNAL_SERVER_ERROR, message))?,
        );
        candidates.sort_unstable();
        candidates.dedup();
        for at in candidates {
            let scheduled_at = format_local_datetime(at);
            if at > now {
                projected.push(json!({"schedule":item.id,"graph":item.graph,"scheduled_at":scheduled_at,"status":"planned"}));
                continue;
            }
            if let Some(run) = runs.iter().find(|run| {
                run.pointer("/trigger/schedule").and_then(Value::as_str) == Some(&item.id)
                    && run.pointer("/trigger/scheduled_at").and_then(Value::as_str)
                        == Some(&scheduled_at)
            }) {
                projected.push(
                    json!({"schedule":item.id,"graph":item.graph,"scheduled_at":scheduled_at,
                    "run":run.get("run"),"status":run.get("status")}),
                );
                continue;
            }
            let busy = runs.iter().any(|run| {
                if run.get("graph").and_then(Value::as_str) != Some(&item.graph) {
                    return false;
                }
                let Some(began) = run
                    .get("started")
                    .and_then(Value::as_str)
                    .and_then(parse_projected_time)
                else {
                    return false;
                };
                // Admission treats persisted unfinished records as blockers,
                // including paused/orphan Runs after a service restart. Their
                // file mtime is only the last update, not their end time.
                let status = run.get("status").and_then(Value::as_str).unwrap_or("");
                let unfinished = matches!(
                    status,
                    "ready"
                        | "running"
                        | "paused"
                        | "budget_stopped"
                        | "waiting_call"
                        | "waiting_recovery"
                        | "stopped"
                );
                let ended = if unfinished {
                    now
                } else {
                    run.get("updated")
                        .and_then(Value::as_str)
                        .and_then(parse_projected_time)
                        .unwrap_or(began)
                };
                began <= at && at <= ended
            });
            projected.push(
                json!({"schedule":item.id,"graph":item.graph,"scheduled_at":scheduled_at,
                "status":if busy {"missed_busy"} else {"missed_downtime"}}),
            );
        }
    }
    Ok(projected)
}

#[allow(clippy::result_large_err)]
pub(super) fn timeline_schedules(state: &ApiState) -> Result<Value, HttpResponse> {
    let store = state.schedules.lock().map_err(|_| {
        error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "schedule store unavailable",
        )
    })?;
    serde_json::to_value(&store.items)
        .map_err(|message| error(StatusCode::INTERNAL_SERVER_ERROR, message.to_string()))
}

fn parse_projected_time(value: &str) -> Option<NaiveDateTime> {
    parse_local_datetime(value).ok().or_else(|| {
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|date| date.with_timezone(&Local).naive_local())
    })
}

fn normalize_rule(value: &Value, now: Option<NaiveDateTime>) -> Result<Value, String> {
    let object = value.as_object().ok_or("rule must be an object")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or("type must be once, interval, daily, weekly, or monthly")?;
    match kind {
        "once" => {
            if object.keys().any(|key| key != "type" && key != "at") {
                return Err("once rule accepts only at".into());
            }
            let at = object
                .get("at")
                .and_then(Value::as_str)
                .and_then(|value| parse_local_datetime(value).ok())
                .ok_or("once.at must be a local datetime without timezone")?;
            if now.is_some_and(|now| at <= now) {
                return Err("once.at must be a future local datetime".into());
            }
            Ok(json!({"type":"once","at":format_local_datetime(at)}))
        }
        "interval" => {
            if object.keys().any(|key| key != "type" && key != "seconds") {
                return Err("interval rule accepts only seconds".into());
            }
            let seconds = object
                .get("seconds")
                .and_then(Value::as_u64)
                .filter(|value| *value > 0)
                .ok_or("interval.seconds must be a positive integer")?;
            Ok(json!({"type":"interval","seconds":seconds}))
        }
        kind @ ("daily" | "weekly" | "monthly") => {
            let allowed: &[&str] = match kind {
                "daily" => &["type", "time"],
                "weekly" => &["type", "time", "weekdays"],
                _ => &["type", "time", "day"],
            };
            if object.keys().any(|key| !allowed.contains(&key.as_str())) {
                return Err(format!("{kind} rule has unsupported fields"));
            }
            let time = object
                .get("time")
                .and_then(Value::as_str)
                .and_then(parse_local_time)
                .ok_or("time must use local HH:MM precision")?;
            let formatted = time.format("%H:%M").to_string();
            match kind {
                "daily" => Ok(json!({"type":kind,"time":formatted})),
                "weekly" => {
                    let weekdays = object
                        .get("weekdays")
                        .and_then(Value::as_array)
                        .filter(|days| !days.is_empty())
                        .ok_or("weekly.weekdays must contain weekdays from 0 (Monday) to 6")?;
                    let mut days = Vec::new();
                    for day in weekdays {
                        let day = day
                            .as_u64()
                            .filter(|day| *day < 7)
                            .ok_or("weekly.weekdays must contain weekdays from 0 (Monday) to 6")?;
                        days.push(day);
                    }
                    days.sort_unstable();
                    days.dedup();
                    Ok(json!({"type":kind,"time":formatted,"weekdays":days}))
                }
                _ => {
                    let day = object
                        .get("day")
                        .and_then(Value::as_u64)
                        .filter(|day| (1..=31).contains(day))
                        .ok_or("monthly.day must be between 1 and 31")?;
                    Ok(json!({"type":kind,"time":formatted,"day":day}))
                }
            }
        }
        _ => Err("type must be once, interval, daily, weekly, or monthly".into()),
    }
}

fn next_after(rule: &Value, after: NaiveDateTime) -> Result<NaiveDateTime, String> {
    let rule = normalize_rule(rule, None)?;
    let kind = rule_type(&rule)?;
    if kind == "once" {
        return parse_local_datetime(rule["at"].as_str().ok_or("once.at is missing")?);
    }
    if kind == "interval" {
        let seconds = rule["seconds"]
            .as_i64()
            .ok_or("interval.seconds is invalid")?;
        return after
            .checked_add_signed(Duration::seconds(seconds))
            .ok_or("interval next_at is out of range".into());
    }
    let target = parse_local_time(rule["time"].as_str().ok_or("rule time is missing")?)
        .ok_or("rule time is invalid")?;
    for offset in 0..367 {
        let date = after
            .date()
            .checked_add_signed(Duration::days(offset))
            .ok_or("date out of range")?;
        if kind == "weekly" {
            let weekday = date.weekday().num_days_from_monday();
            if !rule["weekdays"].as_array().is_some_and(|days| {
                days.iter()
                    .any(|day| day.as_u64() == Some(u64::from(weekday)))
            }) {
                continue;
            }
        }
        if kind == "monthly" && date.day() != rule["day"].as_u64().unwrap_or(0) as u32 {
            continue;
        }
        let candidate = date.and_time(target);
        if candidate > after {
            return Ok(candidate);
        }
    }
    Err("no occurrence in the next year".into())
}

fn occurrences(
    rule: &Value,
    created: NaiveDateTime,
    start: NaiveDateTime,
    end: NaiveDateTime,
) -> Result<Vec<NaiveDateTime>, String> {
    let rule = normalize_rule(rule, None)?;
    let kind = rule_type(&rule)?;
    if kind == "once" {
        let at = parse_local_datetime(rule["at"].as_str().ok_or("once.at is missing")?)?;
        return Ok(if start <= at && at < end {
            vec![at]
        } else {
            vec![]
        });
    }
    let mut result = Vec::new();
    if kind == "interval" {
        let seconds = rule["seconds"]
            .as_i64()
            .ok_or("interval.seconds is invalid")?;
        let step = Duration::seconds(seconds);
        let first = created
            .checked_add_signed(step)
            .ok_or("interval out of range")?;
        let mut cursor = first;
        if cursor < start {
            let elapsed = start.signed_duration_since(cursor).num_seconds();
            let count = (elapsed / seconds).max(0);
            let advance = seconds.checked_mul(count).ok_or("interval out of range")?;
            cursor = cursor
                .checked_add_signed(Duration::seconds(advance))
                .ok_or("interval out of range")?;
            while cursor < start {
                cursor = cursor
                    .checked_add_signed(step)
                    .ok_or("interval out of range")?;
            }
        }
        while cursor < end {
            if cursor >= created {
                result.push(cursor);
            }
            cursor = cursor
                .checked_add_signed(step)
                .ok_or("interval out of range")?;
        }
        return Ok(result);
    }
    let mut current = next_after(&rule, start - Duration::seconds(1))?;
    while current < end {
        if current >= created {
            result.push(current);
        }
        current = next_after(&rule, current)?;
    }
    Ok(result)
}

fn rule_type(rule: &Value) -> Result<&str, String> {
    rule.get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "rule type is missing".into())
}

fn parse_local_datetime(value: &str) -> Result<NaiveDateTime, String> {
    [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M",
    ]
    .iter()
    .find_map(|format| NaiveDateTime::parse_from_str(value, format).ok())
    .ok_or_else(|| "datetime must be a local ISO datetime without timezone".into())
}

fn parse_local_time(value: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(value, "%H:%M").ok()
}

fn format_local_datetime(value: NaiveDateTime) -> String {
    value.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn unique_schedule_id() -> String {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let seed = format!("anchor-schedule:{}:{nanos}:{sequence}", std::process::id());
    format!("{:x}", Sha256::digest(seed.as_bytes()))
}

#[cfg(test)]
#[path = "schedules_tests.rs"]
mod tests;
