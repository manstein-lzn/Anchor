use super::*;

fn fixture() -> (tempfile::TempDir, ModelConfiguration) {
    let root = tempfile::tempdir().unwrap();
    let configuration = ModelConfiguration::new(root.path(), BTreeMap::new()).unwrap();
    (root, configuration)
}
fn save_slot(
    configuration: &ModelConfiguration,
    slot: &str,
    endpoint: &str,
    flash: &str,
    pro: &str,
    key: Value,
) -> Value {
    configuration.save_provider(slot, serde_json::from_value(json!({
        "endpoint": endpoint, "wire_api": "chat", "flash": flash, "pro": pro, "api_key": key,
    })).unwrap()).unwrap()
}
fn legacy(configuration: &ModelConfiguration) {
    configuration.save(serde_json::from_value(json!({
        "endpoint": "https://legacy.example/v1", "wire_api": "chat", "model": "legacy-default",
        "aliases": {"models.research": "legacy-research"}, "api_key": {"action": "set", "value": "legacy-private"},
    })).unwrap()).unwrap();
}
fn file(configuration: &ModelConfiguration) -> Vec<u8> {
    fs::read(configuration.root.join("model-config.json")).unwrap()
}
fn key(registry: &ModelRegistry) -> String {
    registry.connection_test_upstream().api_key
}
fn identity(registry: &ModelRegistry) -> String {
    registry.resolve(None).unwrap().identity
}
fn reference_model(registry: &ModelRegistry, reference: Option<&str>, model: &str) {
    assert_eq!(registry.resolve(None).unwrap().model, model);
    assert_eq!(registry.resolve(reference).unwrap().model, model);
    assert_eq!(
        registry.resolve(None).unwrap().identity,
        registry.resolve(reference).unwrap().identity
    );
}
fn retained_id(status: &Value, slot: &str, endpoint: &str) -> String {
    status["retained_providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|provider| {
            provider["slot"] == slot
                && provider["endpoint"] == endpoint
                && provider["current"] == false
        })
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[test]
fn every_named_reference_selects_only_its_slot_even_for_the_same_upstream_and_models() {
    let (_root, configuration) = fixture();
    let endpoint = "https://provider.example/v1";
    save_slot(
        &configuration,
        "default",
        endpoint,
        "same-fast",
        "same-pro",
        json!({"action":"set","value":"default-account"}),
    );
    save_slot(
        &configuration,
        "backup",
        endpoint,
        "same-fast",
        "same-pro",
        json!({"action":"set","value":"backup-account"}),
    );
    let default = configuration.execution_snapshot(None, None).unwrap();
    let backup = configuration
        .execution_snapshot(None, Some("models.backup.flash"))
        .unwrap();
    assert_ne!(identity(&default), identity(&backup));
    assert!(
        configuration
            .execution_snapshot(Some(&identity(&default)), Some("models.backup.flash"))
            .is_err()
    );
    assert!(
        configuration
            .execution_snapshot(Some(&identity(&backup)), None)
            .is_err()
    );
    for (reference, model, expected_key) in [
        (None, "same-fast", "default-account"),
        (Some("models.default"), "same-fast", "default-account"),
        (Some("models.default.flash"), "same-fast", "default-account"),
        (Some("models.default.pro"), "same-pro", "default-account"),
        (Some("models.backup.flash"), "same-fast", "backup-account"),
        (Some("models.backup.pro"), "same-pro", "backup-account"),
    ] {
        let registry = configuration.execution_snapshot(None, reference).unwrap();
        reference_model(&registry, reference, model);
        assert_eq!(key(&registry), expected_key);
        let restored = configuration
            .execution_snapshot(Some(&identity(&registry)), reference)
            .unwrap();
        assert_eq!(key(&restored), expected_key);
    }
    let backup = configuration
        .execution_snapshot(None, Some("models.backup.flash"))
        .unwrap();
    save_slot(
        &configuration,
        "backup",
        endpoint,
        "same-fast",
        "same-pro",
        json!({"action":"clear"}),
    );
    assert!(
        configuration
            .execution_snapshot(Some(&identity(&backup)), Some("models.backup.flash"))
            .is_err()
    );
    assert_eq!(
        key(&configuration.execution_snapshot(None, None).unwrap()),
        "default-account"
    );
}

#[test]
fn legacy_and_named_default_bindings_with_identical_fields_never_share_accounts() {
    let (_root, configuration) = fixture();
    legacy(&configuration);
    let old = configuration.execution_snapshot(None, None).unwrap();
    save_slot(
        &configuration,
        "default",
        "https://legacy.example/v1",
        "legacy-default",
        "legacy-default",
        json!({"action":"set","value":"named-account"}),
    );
    let named = configuration.execution_snapshot(None, None).unwrap();
    assert_ne!(identity(&old), identity(&named));
    let pro = configuration
        .execution_snapshot(None, Some("models.default.pro"))
        .unwrap();
    assert_eq!(identity(&named), identity(&pro));
    for reference in [None, Some("models.default"), Some("models.default.flash")] {
        let resumed = configuration
            .execution_snapshot(Some(&identity(&old)), reference)
            .unwrap();
        assert_eq!(key(&resumed), "legacy-private");
        assert_eq!(identity(&resumed), identity(&old));
    }
    assert_eq!(
        key(&configuration
            .execution_snapshot(Some(&identity(&named)), None)
            .unwrap()),
        "named-account"
    );
    save_slot(
        &configuration,
        "default",
        "https://legacy.example/v1",
        "legacy-default",
        "legacy-default",
        json!({"action":"clear"}),
    );
    assert!(
        configuration
            .execution_snapshot(Some(&identity(&named)), None)
            .is_err()
    );
    assert_eq!(
        key(&configuration
            .execution_snapshot(Some(&identity(&old)), None)
            .unwrap()),
        "legacy-private"
    );
}

#[test]
fn legacy_reserved_alias_bindings_restore_exact_models_while_new_calls_select_named_slots() {
    let (_root, configuration) = fixture();
    let aliases = [
        ("models.default.flash", "old-default-fast"),
        ("models.default.pro", "old-default-pro"),
        ("models.backup.flash", "old-backup-fast"),
        ("models.backup.pro", "old-backup-pro"),
    ];
    configuration.save(serde_json::from_value(json!({
        "endpoint": "https://shared.example/v1", "wire_api": "chat", "model": "legacy-primary",
        "aliases": aliases.into_iter().collect::<BTreeMap<_,_>>(),
        "api_key": {"action":"set","value":"legacy-account"},
    })).unwrap()).unwrap();
    let legacy = configuration.snapshot(None, None).unwrap();
    let bindings = aliases.map(|(reference, _)| legacy.resolve(Some(reference)).unwrap().identity);
    // Deliberately give each named model the same public upstream/model as its
    // old alias: only credential scope should distinguish the retained identity.
    save_slot(
        &configuration,
        "default",
        "https://shared.example/v1",
        "old-default-fast",
        "old-default-pro",
        json!({"action":"set","value":"default-account"}),
    );
    save_slot(
        &configuration,
        "backup",
        "https://shared.example/v1",
        "old-backup-fast",
        "old-backup-pro",
        json!({"action":"set","value":"backup-account"}),
    );
    let mut named = Vec::new();
    for ((reference, model), old_binding) in aliases.into_iter().zip(&bindings) {
        let resumed = configuration
            .execution_snapshot(Some(old_binding), Some(reference))
            .unwrap();
        assert_eq!(resumed.resolve(Some(reference)).unwrap().model, model);
        assert_eq!(
            resumed.resolve(Some(reference)).unwrap().identity,
            *old_binding
        );
        assert_eq!(key(&resumed), "legacy-account");
        let selected = configuration
            .execution_snapshot(None, Some(reference))
            .unwrap();
        reference_model(&selected, Some(reference), model);
        assert_ne!(identity(&selected), *old_binding);
        assert_eq!(
            key(&selected),
            if reference.starts_with("models.default.") {
                "default-account"
            } else {
                "backup-account"
            }
        );
        named.push((reference, identity(&selected)));
    }
    assert_eq!(
        configuration
            .execution_snapshot(None, None)
            .unwrap()
            .resolve(None)
            .unwrap()
            .model,
        "old-default-fast"
    );
    configuration
        .clear_provider_credential("slot:default")
        .unwrap();
    configuration
        .clear_provider_credential("slot:backup")
        .unwrap();
    for (reference, named_binding) in named {
        assert!(
            configuration
                .execution_snapshot(None, Some(reference))
                .is_err()
        );
        assert!(
            configuration
                .execution_snapshot(Some(&named_binding), Some(reference))
                .is_err()
        );
    }
    for ((reference, model), old_binding) in aliases.into_iter().zip(&bindings) {
        let resumed = configuration
            .execution_snapshot(Some(old_binding), Some(reference))
            .unwrap();
        assert_eq!(resumed.resolve(Some(reference)).unwrap().model, model);
        assert_eq!(key(&resumed), "legacy-account");
    }
}

#[test]
fn new_invocations_hot_read_settings_while_existing_snapshots_remain_frozen() {
    let (root, configuration) = fixture();
    let endpoint = "https://provider.example/v1";
    save_slot(
        &configuration,
        "default",
        endpoint,
        "fast-v1",
        "pro",
        json!({"action":"set","value":"key-v1"}),
    );
    let frozen = configuration.execution_snapshot(None, None).unwrap();
    let writer = ModelConfiguration::new(root.path(), BTreeMap::new()).unwrap();
    save_slot(
        &writer,
        "default",
        endpoint,
        "fast-v2",
        "pro",
        json!({"action":"set","value":"key-v2"}),
    );
    let latest = configuration.execution_snapshot(None, None).unwrap();
    reference_model(&latest, Some("models.default.flash"), "fast-v2");
    assert_eq!(key(&latest), "key-v2");
    reference_model(&frozen, None, "fast-v1");
    assert_eq!(key(&frozen), "key-v1");
    let restored = configuration
        .execution_snapshot(Some(&identity(&frozen)), None)
        .unwrap();
    reference_model(&restored, None, "fast-v1");
    assert_eq!(key(&restored), "key-v2");
    let bytes = file(&configuration);
    assert_eq!(String::from_utf8_lossy(&bytes).matches("key-v2").count(), 1);
    assert!(!String::from_utf8_lossy(&bytes).contains("key-v1"));
    assert_eq!(
        configuration.status().unwrap()["execution"]["fields"]["model"],
        "fast-v2"
    );
    assert_eq!(file(&configuration), bytes);
}

#[test]
fn changed_service_retains_the_original_binding_and_only_its_original_credential() {
    let (_root, configuration) = fixture();
    let old_endpoint = "https://old.example/v1";
    save_slot(
        &configuration,
        "default",
        old_endpoint,
        "first",
        "pro",
        json!({"action":"set","value":"old-account"}),
    );
    let old = configuration.execution_snapshot(None, None).unwrap();
    let status = save_slot(
        &configuration,
        "default",
        "https://new.example/v1",
        "second",
        "new-pro",
        json!({"action":"set","value":"new-account"}),
    );
    let retained = configuration
        .execution_snapshot(Some(&identity(&old)), None)
        .unwrap();
    assert_eq!(
        retained.connection_test_upstream().url.as_str(),
        "https://old.example/v1/chat/completions"
    );
    assert_eq!(key(&retained), "old-account");
    reference_model(&retained, None, "first");
    let id = retained_id(&status, "default", old_endpoint);
    let cleared = configuration.clear_provider_credential(&id).unwrap();
    assert!(
        configuration
            .execution_snapshot(Some(&identity(&old)), None)
            .is_err()
    );
    assert!(!cleared.to_string().contains("old-account"));
    assert!(!String::from_utf8_lossy(&file(&configuration)).contains("old-account"));
    configuration
        .set_provider_credential(&id, "restored-account".into())
        .unwrap();
    assert_eq!(
        key(&configuration
            .execution_snapshot(Some(&identity(&old)), None)
            .unwrap()),
        "restored-account"
    );
    assert_eq!(
        key(&configuration.execution_snapshot(None, None).unwrap()),
        "new-account"
    );
    assert_eq!(key(&old), "old-account");
    let mut changed: ProviderUpdate = serde_json::from_value(json!({"endpoint": "https://new.example/v1", "wire_api": "responses", "flash":"second", "pro":"new-pro", "api_key":{"action":"keep"}})).unwrap();
    let new = configuration.execution_snapshot(None, None).unwrap();
    configuration.save_provider("default", changed).unwrap();
    assert!(configuration.execution_snapshot(None, None).is_err());
    let restored = configuration
        .execution_snapshot(Some(&identity(&new)), None)
        .unwrap();
    assert_eq!(
        restored.connection_test_upstream().url.as_str(),
        "https://new.example/v1/chat/completions"
    );
    assert_eq!(key(&restored), "new-account");
    changed = serde_json::from_value(json!({"endpoint": "https://new.example/v1", "wire_api": "responses", "flash":"second", "pro":"new-pro", "api_key":{"action":"set","value":"responses-account"}})).unwrap();
    configuration.save_provider("default", changed).unwrap();
    assert_eq!(
        key(&configuration
            .execution_snapshot(Some(&identity(&new)), None)
            .unwrap()),
        "new-account"
    );
}

#[test]
fn saved_incomplete_default_or_requested_tier_never_falls_back_to_legacy_or_backup() {
    let (_root, configuration) = fixture();
    legacy(&configuration);
    save_slot(
        &configuration,
        "backup",
        "https://backup.example/v1",
        "backup-fast",
        "backup-pro",
        json!({"action":"set","value":"backup-key"}),
    );
    save_slot(
        &configuration,
        "default",
        "",
        "",
        "",
        json!({"action":"keep"}),
    );
    for reference in [
        None,
        Some("models.default"),
        Some("models.default.flash"),
        Some("models.default.pro"),
    ] {
        assert!(configuration.execution_snapshot(None, reference).is_err());
    }
    let status = configuration.status().unwrap();
    assert_eq!(status["configured"], true);
    assert_eq!(status["execution"]["source"], "host");
    assert_eq!(status["execution"]["configured"], false);
    assert_eq!(status["execution"]["api_key_present"], false);
    save_slot(
        &configuration,
        "default",
        "https://default.example/v1",
        "only-flash",
        "",
        json!({"action":"set","value":"default-key"}),
    );
    assert!(
        configuration
            .execution_snapshot(None, Some("models.default.pro"))
            .is_err()
    );
    reference_model(
        &configuration.execution_snapshot(None, None).unwrap(),
        None,
        "only-flash",
    );
    save_slot(
        &configuration,
        "default",
        "https://default.example/v1",
        "only-flash",
        "",
        json!({"action":"clear"}),
    );
    assert!(configuration.execution_snapshot(None, None).is_err());
    assert_eq!(
        configuration
            .execution_snapshot(None, Some("models.research"))
            .unwrap()
            .resolve(Some("models.research"))
            .unwrap()
            .model,
        "legacy-research"
    );
}

#[test]
fn returning_to_an_old_service_requires_a_key_for_new_calls_and_clear_closes_history() {
    let (_root, configuration) = fixture();
    let original = "https://original.example/v1";
    save_slot(
        &configuration,
        "default",
        original,
        "old",
        "pro",
        json!({"action":"set","value":"original-key"}),
    );
    let old = configuration.execution_snapshot(None, None).unwrap();
    save_slot(
        &configuration,
        "default",
        "https://other.example/v1",
        "other",
        "pro",
        json!({"action":"set","value":"other-key"}),
    );
    save_slot(
        &configuration,
        "default",
        original,
        "old",
        "pro",
        json!({"action":"keep"}),
    );
    assert!(configuration.execution_snapshot(None, None).is_err());
    assert_eq!(
        key(&configuration
            .execution_snapshot(Some(&identity(&old)), None)
            .unwrap()),
        "original-key"
    );
    save_slot(
        &configuration,
        "default",
        original,
        "old",
        "pro",
        json!({"action":"clear"}),
    );
    assert!(
        configuration
            .execution_snapshot(Some(&identity(&old)), None)
            .is_err()
    );
    assert!(!String::from_utf8_lossy(&file(&configuration)).contains("original-key"));
}

#[test]
fn historical_services_from_both_slots_remain_separate_and_manageable() {
    let (_root, configuration) = fixture();
    let original = "https://shared.example/v1";
    let mut bindings = BTreeMap::new();
    for slot in ["default", "backup"] {
        save_slot(
            &configuration,
            slot,
            original,
            "same",
            "same",
            json!({"action":"set","value":format!("{slot}-original")}),
        );
        let reference = format!("models.{slot}.flash");
        let registry = configuration
            .execution_snapshot(None, Some(&reference))
            .unwrap();
        bindings.insert(slot, identity(&registry));
        save_slot(
            &configuration,
            slot,
            "https://new.example/v1",
            "new",
            "new",
            json!({"action":"set","value":format!("{slot}-new")}),
        );
    }
    let status = configuration.status().unwrap();
    let default_id = retained_id(&status, "default", original);
    let backup_id = retained_id(&status, "backup", original);
    assert_ne!(default_id, backup_id);
    configuration
        .clear_provider_credential(&default_id)
        .unwrap();
    assert!(
        configuration
            .execution_snapshot(Some(&bindings["default"]), None)
            .is_err()
    );
    let backup = configuration
        .execution_snapshot(Some(&bindings["backup"]), Some("models.backup.flash"))
        .unwrap();
    assert_eq!(key(&backup), "backup-original");
    configuration
        .set_provider_credential(&default_id, "default-restored".into())
        .unwrap();
    assert_eq!(
        key(&configuration
            .execution_snapshot(Some(&bindings["default"]), None)
            .unwrap()),
        "default-restored"
    );
    assert_eq!(
        key(&configuration
            .execution_snapshot(None, Some("models.backup.flash"))
            .unwrap()),
        "backup-new"
    );
    assert!(
        !configuration
            .status()
            .unwrap()
            .to_string()
            .contains("default-restored")
    );
}

#[test]
fn partial_slot_credentials_remain_visible_after_the_service_is_replaced() {
    let (_root, configuration) = fixture();
    let original = "https://partial.example/v1";
    save_slot(
        &configuration,
        "backup",
        original,
        "",
        "",
        json!({"action":"set","value":"partial-key"}),
    );
    let status = save_slot(
        &configuration,
        "backup",
        "https://new.example/v1",
        "new",
        "new",
        json!({"action":"set","value":"new-key"}),
    );
    let id = retained_id(&status, "backup", original);
    configuration.clear_provider_credential(&id).unwrap();
    assert!(!String::from_utf8_lossy(&file(&configuration)).contains("partial-key"));
}

#[test]
fn legacy_aliases_and_pre_slot_default_bindings_stay_compatible_without_read_time_writes() {
    for format in [1, 2] {
        let (root, configuration) = fixture();
        legacy(&configuration);
        let mut managed: Value = serde_json::from_slice(&file(&configuration)).unwrap();
        managed["format"] = json!(format);
        if format == 1 {
            managed.as_object_mut().unwrap().remove("providers");
        } else {
            managed["providers"]
                .as_object_mut()
                .unwrap()
                .remove("history");
        }
        fs::write(
            configuration.root.join("model-config.json"),
            serde_json::to_vec(&managed).unwrap(),
        )
        .unwrap();
        let before = file(&configuration);
        let frozen = configuration.execution_snapshot(None, None).unwrap();
        for reference in [None, Some("models.default"), Some("models.default.flash")] {
            reference_model(
                &configuration.execution_snapshot(None, reference).unwrap(),
                reference,
                "legacy-default",
            );
        }
        assert!(
            configuration
                .execution_snapshot(None, Some("models.default.pro"))
                .is_err()
        );
        assert!(
            configuration
                .execution_snapshot(None, Some("models.backup.flash"))
                .is_err()
        );
        assert_eq!(
            configuration.status().unwrap()["execution"]["source"],
            "legacy"
        );
        assert_eq!(file(&configuration), before);
        save_slot(
            &configuration,
            "default",
            "https://new.example/v1",
            "new-fast",
            "new-pro",
            json!({"action":"set","value":"new-key"}),
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&file(&configuration)).unwrap()["format"],
            3
        );
        let restored = ModelConfiguration::new(root.path(), BTreeMap::new()).unwrap();
        assert_eq!(
            key(&restored
                .execution_snapshot(Some(&identity(&frozen)), None)
                .unwrap()),
            "legacy-private"
        );
        assert_eq!(
            restored
                .execution_snapshot(None, Some("models.research"))
                .unwrap()
                .resolve(Some("models.research"))
                .unwrap()
                .model,
            "legacy-research"
        );
        assert_eq!(
            restored
                .snapshot(None, None)
                .unwrap()
                .resolve(None)
                .unwrap()
                .model,
            "legacy-default"
        );
        let status = restored.status().unwrap();
        assert_eq!(status["fields"]["model"], "legacy-default");
        assert_eq!(status["execution"]["fields"]["model"], "new-fast");
        assert_eq!(
            fs::metadata(restored.root.join("model-config.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn rejected_edits_and_reads_preserve_the_published_configuration_and_do_not_expose_secrets() {
    let (_root, configuration) = fixture();
    save_slot(
        &configuration,
        "default",
        "https://old.example/v1",
        "fast",
        "pro",
        json!({"action":"set","value":"private-stored-key"}),
    );
    let frozen = configuration.execution_snapshot(None, None).unwrap();
    let before = file(&configuration);
    let invalid: ProviderUpdate = serde_json::from_value(json!({"endpoint":"https://new.example/v1","wire_api":"chat","flash":"fast","pro":"pro","api_key":{"action":"set","value":"private invalid\n"}})).unwrap();
    let error = configuration.save_provider("default", invalid).unwrap_err();
    assert!(!error.contains("private invalid"));
    assert_eq!(file(&configuration), before);
    configuration.status().unwrap();
    configuration
        .execution_snapshot(Some(&identity(&frozen)), None)
        .unwrap();
    assert_eq!(file(&configuration), before);
    assert!(
        !configuration
            .status()
            .unwrap()
            .to_string()
            .contains("private-stored-key")
    );
}

#[test]
fn reference_validation_and_runtime_share_resolution_and_name_unavailable_references() {
    let (_root, configuration) = fixture();
    for reference in [
        None,
        Some("models.default"),
        Some("models.default.flash"),
        Some("models.default.pro"),
        Some("models.backup.flash"),
        Some("models.backup.pro"),
    ] {
        configuration.validate_reference(reference).unwrap();
    }
    save_slot(
        &configuration,
        "default",
        "https://provider.example/v1",
        "fast",
        "pro",
        json!({"action":"set","value":"test-key"}),
    );
    for reference in ["claude-sonnet-4-5", "default", "models.unknown"] {
        let error = configuration
            .validate_reference(Some(reference))
            .unwrap_err();
        assert!(
            error.contains(reference) && error.contains(MODEL_REFERENCES),
            "{error}"
        );
        let runtime = configuration
            .execution_snapshot(None, Some(reference))
            .err()
            .unwrap();
        assert!(runtime.contains(&error), "{runtime}");
        assert!(!runtime.contains("no model service configured"));
    }
    legacy(&configuration);
    for reference in ["models.research", "legacy-default"] {
        configuration.validate_reference(Some(reference)).unwrap();
        configuration
            .execution_snapshot(None, Some(reference))
            .unwrap();
    }
    // Removing a configured alias invalidates new invocations, not retained ones.
    let mut managed = configuration.load().unwrap();
    managed.current.aliases.clear();
    configuration.publish(&managed).unwrap();
    let error = configuration
        .execution_snapshot(None, Some("models.research"))
        .err()
        .unwrap();
    assert!(error.contains("models.research") && error.contains("accepted references"));
    save_slot(
        &configuration,
        "default",
        "https://provider.example/v1",
        "fast",
        "",
        json!({"action":"keep"}),
    );
    let error = configuration
        .execution_snapshot(None, Some("models.default.pro"))
        .err()
        .unwrap();
    assert!(error.contains("models.default.pro") && error.contains("no model configured"));
}
