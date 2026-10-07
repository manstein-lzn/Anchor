use super::super::transport::{NotificationBuffer, NotificationRetention};
use super::project_update;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

struct Projection {
    session: Option<String>,
    notifications: NotificationBuffer,
    prompt_count: usize,
}

type SharedProjection = Mutex<Projection>;
type Registry = Mutex<HashMap<PathBuf, Weak<SharedProjection>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) struct LiveTrace {
    fact: PathBuf,
    projection: Arc<SharedProjection>,
}

impl LiveTrace {
    pub(crate) fn open(fact: &Path) -> Result<Self, String> {
        let fact = fact.canonicalize().map_err(|error| error.to_string())?;
        let projection = Arc::new(Mutex::new(Projection {
            session: None,
            notifications: NotificationBuffer::new(NotificationRetention::Tail),
            prompt_count: 0,
        }));
        let mut registry = registry().lock().map_err(|_| "live trace unavailable")?;
        if registry.get(&fact).and_then(Weak::upgrade).is_some() {
            return Err("Goose invocation already has an active trace".into());
        }
        registry.insert(fact.clone(), Arc::downgrade(&projection));
        Ok(Self { fact, projection })
    }

    pub(crate) fn restore(&self, session: &str, history: &[Value]) -> Result<(), String> {
        {
            let mut projection = self
                .projection
                .lock()
                .map_err(|_| "live trace unavailable")?;
            if projection.session.is_some() {
                return Err("Goose live trace Session was already bound".into());
            }
            projection.session = Some(session.to_owned());
        }
        for event in history {
            self.observe(event)?;
        }
        self.projection
            .lock()
            .map_err(|_| "live trace unavailable")?
            .prompt_count = 0;
        Ok(())
    }

    pub(crate) fn observe(&self, event: &Value) -> Result<(), String> {
        if event["method"] != "session/update" {
            return Ok(());
        }
        let mut projection = self
            .projection
            .lock()
            .map_err(|_| "live trace unavailable")?;
        if projection.session.is_none()
            || event["params"]["sessionId"].as_str() != projection.session.as_deref()
        {
            return Err("Goose live trace update belongs to another Session".into());
        }
        let bytes = serde_json::to_vec(event)
            .map_err(|_| "Goose live trace serialization failed")?
            .len();
        projection.notifications.push(event.clone(), bytes)?;
        projection.prompt_count = projection.prompt_count.saturating_add(1);
        Ok(())
    }

    pub(crate) fn prompt_notifications(&self) -> Result<Vec<Value>, String> {
        let projection = self
            .projection
            .lock()
            .map_err(|_| "live trace unavailable")?;
        Ok(projection
            .notifications
            .last_values(projection.prompt_count))
    }
}

impl Drop for LiveTrace {
    fn drop(&mut self) {
        if let Ok(mut registry) = registry().lock() {
            registry.remove(&self.fact);
        }
    }
}

pub(super) fn snapshot(fact: &Path, session: Option<&str>) -> Result<Option<Vec<Value>>, String> {
    let fact = fact.canonicalize().map_err(|error| error.to_string())?;
    let projection = registry()
        .lock()
        .map_err(|_| "live trace unavailable")?
        .get(&fact)
        .and_then(Weak::upgrade);
    let Some(projection) = projection else {
        return Ok(None);
    };
    let (events, expected_session) = {
        let projection = projection.lock().map_err(|_| "live trace unavailable")?;
        if projection.session.is_none() {
            return Ok(None);
        }
        if projection.session.as_deref() != session {
            return Err("Goose live trace Session binding changed".into());
        }
        (
            projection.notifications.values(),
            projection.session.clone(),
        )
    };
    let mut messages = Vec::new();
    for event in events {
        project_update(&mut messages, &event, expected_session.as_deref())?;
    }
    Ok(Some(messages))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn notification(session: &str, text: &str) -> Value {
        json!({"method":"session/update","params":{"sessionId":session,
            "update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}}})
    }

    #[test]
    fn active_projection_is_scoped_bound_and_removed_without_persisting_a_log() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first.json");
        let second = root.path().join("second.json");
        std::fs::write(&first, "{}").unwrap();
        std::fs::write(&second, "{}").unwrap();
        let live = LiveTrace::open(&first).unwrap();
        assert!(snapshot(&first, None).unwrap().is_none());
        live.restore("native", &[notification("native", "first ")])
            .unwrap();
        live.observe(&notification("native", "second")).unwrap();
        assert_eq!(
            live.prompt_notifications().unwrap(),
            vec![notification("native", "second")]
        );
        assert_eq!(
            snapshot(&first, Some("native")).unwrap().unwrap()[0]["text"],
            "first second"
        );
        assert!(snapshot(&second, Some("native")).unwrap().is_none());
        assert!(snapshot(&first, Some("foreign")).is_err());
        assert!(live.observe(&notification("foreign", "ignored")).is_err());
        assert!(LiveTrace::open(&first).is_err());
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "{}");
        drop(live);
        assert!(snapshot(&first, Some("native")).unwrap().is_none());
        assert!(LiveTrace::open(&first).is_ok());
    }

    #[test]
    fn long_live_projection_is_bounded_and_marks_lost_display_history() {
        let root = tempfile::tempdir().unwrap();
        let fact = root.path().join("fact.json");
        std::fs::write(&fact, "{}").unwrap();
        let live = LiveTrace::open(&fact).unwrap();
        live.restore("native", &[]).unwrap();
        for index in 0..4100 {
            let mut event = notification("native", "x");
            event["params"]["update"]["sessionUpdate"] = json!("tool_call");
            event["params"]["update"]["toolCallId"] = json!(format!("call-{index}"));
            live.observe(&event).unwrap();
        }
        let messages = snapshot(&fact, Some("native")).unwrap().unwrap();
        assert!(messages.len() <= 4096);
        assert_eq!(messages[0]["truncated"], true);
        assert_eq!(messages.last().unwrap()["tool_call_id"], "call-4099");
    }
}
