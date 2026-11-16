use super::*;
use serde_json::json;

fn suite() -> ProtectedSuite {
    serde_json::from_slice(include_bytes!(
        "../../../../resources/evaluations/input-discovery/manifest.json"
    ))
    .unwrap()
}

#[test]
fn input_discovery_keeps_protected_cases_and_strict_disjoint_splits() {
    let original = suite();
    original.validate().unwrap();
    assert_eq!(original.id, "input-discovery");
    assert_eq!(original.cases.len(), 12);
    assert_eq!(original.deliverable_path(), "result.json");
    let mut changed = original.clone();
    changed.cases[1].id = changed.cases[0].id.clone();
    assert!(changed.validate().is_err());
    changed = original.clone();
    let development = changed.cases[0].input.clone();
    changed
        .cases
        .iter_mut()
        .find(|case| case.split == Split::Holdout)
        .unwrap()
        .input = development;
    assert!(changed.validate().is_err());
    changed = original.clone();
    changed
        .cases
        .retain(|case| case.split == Split::Development);
    assert!(changed.validate().is_err());
    for file in ["../result.json", "", "view/result.json"] {
        changed = original.clone();
        changed.output_file = Some(file.into());
        assert!(changed.validate().is_err());
    }
    for (field, value) in [
        ("context_branch", json!("paired_continuation")),
        ("oracle", json!("briefing_plan_v1")),
        ("grade", json!({"passed": true})),
    ] {
        let mut value_of_suite = serde_json::to_value(&original).unwrap();
        value_of_suite[field] = value;
        assert!(serde_json::from_value::<ProtectedSuite>(value_of_suite).is_err());
    }
}
