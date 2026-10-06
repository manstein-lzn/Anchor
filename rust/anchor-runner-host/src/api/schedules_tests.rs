use super::*;
use chrono::NaiveDateTime;

fn dt(value: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S").unwrap()
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
