use super::*;
use chrono::NaiveDateTime;

fn dt(value: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").unwrap()
}

#[tokio::test]
async fn retired_system_schedule_never_falls_through_to_ordinary_admission() {
    for prefix in ["dreaming-install-", "dreaming-rebind-"] {
        let (_root, state) = super::super::tests::fixture();
        let at = "2026-11-15T03:00:00";
        {
            let mut store = state.schedules.lock().unwrap();
            store.items.push(ScheduleItem {
                id: "historical-system".into(),
                graph: "fixture".into(),
                rule: json!({"type":"once","at":at}),
                input: json!({}),
                created_at: at.into(),
                next_at: at.into(),
                enabled: true,
                delegation: None,
                remaining: None,
                group: None,
            });
            store.creation_requests.push(ScheduleCreationRequest {
                owner: "local".into(),
                key: format!("{prefix}historical"),
                content: json!({"graph":"fixture","input":{},"enabled":false}),
                schedule: "historical-system".into(),
                group: None,
            });
            store.persist().unwrap();
        }
        let path = state.schedules.lock().unwrap().path.clone();
        let before = fs::read(&path).unwrap();
        tick_schedules(&state, dt(at)).await.unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!state.data_root.exists());
    }
}

#[test]
fn local_recurrence_rules_validate_and_skip_nonexistent_month_dates() {
    let once = json!({"type":"once","at":"2026-10-06T09:30:00"});
    assert_eq!(
        normalize_rule(&once, Some(dt("2026-10-06T09:00:00"))).unwrap(),
        once
    );
    assert!(normalize_rule(&once, Some(dt("2026-10-06T09:30:00"))).is_err());
    assert!(
        normalize_rule(
            &json!({"type":"once","at":"2026-10-06T09:30:00+08:00"}),
            None
        )
        .is_err()
    );
    assert!(normalize_rule(&json!({"type":"interval","seconds":true}), None).is_err());
    assert!(normalize_rule(&json!({"type":"interval","seconds":i64::MAX}), None).is_err());
    assert!(normalize_rule(&json!({"type":"weekly","time":"09:00","weekdays":[]}), None).is_err());
    assert!(normalize_rule(&json!({"type":"monthly","time":"09:00","day":32}), None).is_err());

    let daily = json!({"type":"daily","time":"09:30"});
    assert_eq!(
        next_after(&daily, dt("2026-10-06T09:30:00")).unwrap(),
        dt("2026-10-07T09:30:00")
    );
    let weekly = json!({"type":"weekly","time":"09:30","weekdays":[2,0,2]});
    assert_eq!(
        normalize_rule(&weekly, None).unwrap(),
        json!({"type":"weekly","time":"09:30","weekdays":[0,2]})
    );
    assert_eq!(
        next_after(&weekly, dt("2026-10-06T10:00:00")).unwrap(),
        dt("2026-10-07T09:30:00")
    );
    let monthly = json!({"type":"monthly","time":"09:30","day":31});
    assert_eq!(
        next_after(&monthly, dt("2026-02-01T00:00:00")).unwrap(),
        dt("2026-03-31T09:30:00")
    );
}

#[test]
fn finite_calendar_projection_counts_occurrences_before_the_requested_window() {
    let rule = json!({"type":"weekly","time":"09:00","weekdays":[0,2,4]});
    assert_eq!(
        occurrences(
            &rule,
            dt("2026-10-05T09:00:00"),
            dt("2026-10-07T00:00:00"),
            dt("2026-10-14T00:00:00"),
            Some(3)
        )
        .unwrap(),
        [dt("2026-10-07T09:00:00"), dt("2026-10-09T09:00:00")]
    );
}
