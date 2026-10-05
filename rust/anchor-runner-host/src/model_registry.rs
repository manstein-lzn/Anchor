//! The Python-compatible ANCHOR_MODEL_ALIASES contract on one operator endpoint.

use std::collections::{BTreeMap, BTreeSet};

use anchor_io_harness_runtime::node_port::RigModelRegistry;
use anchor_runtime_rig::RigCompletionPort;

/// Resolve environment-only credentials into live Rig transports. No credential
/// or raw endpoint is passed to the Graph, checkpoint or model binding fact.
pub(crate) fn from_env() -> Result<Option<RigModelRegistry>, String> {
    from_values(|name| std::env::var(name).ok())
}

fn from_values(
    mut value: impl FnMut(&str) -> Option<String>,
) -> Result<Option<RigModelRegistry>, String> {
    let Some(api_key) = value("ANCHOR_MODEL_API_KEY").filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let Some(base_url) = value("ANCHOR_MODEL_URL").filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let uri = base_url
        .parse::<axum::http::Uri>()
        .map_err(|_| "ANCHOR_MODEL_URL must be an absolute HTTP(S) endpoint".to_owned())?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || uri.authority().is_none() {
        return Err("ANCHOR_MODEL_URL must be an absolute HTTP(S) endpoint".into());
    }
    let model = value("ANCHOR_MODEL_NAME").unwrap_or_else(|| "default".into());
    if model.trim().is_empty() {
        return Err("ANCHOR_MODEL_NAME must be a nonempty model name".into());
    }
    let wire = value("ANCHOR_MODEL_WIRE_API").unwrap_or_else(|| "responses".into());
    if !matches!(wire.as_str(), "chat" | "responses") {
        return Err("ANCHOR_MODEL_WIRE_API must be chat or responses".into());
    }
    let aliases = value("ANCHOR_MODEL_ALIASES").unwrap_or_default();
    let aliases: serde_json::Value =
        serde_json::from_str(if aliases.is_empty() { "{}" } else { &aliases })
            .map_err(|_| "ANCHOR_MODEL_ALIASES must be a JSON object".to_owned())?;
    let aliases = aliases
        .as_object()
        .ok_or_else(|| "ANCHOR_MODEL_ALIASES must be a JSON object".to_owned())?;
    let mut names = BTreeMap::new();
    for (reference, name) in aliases {
        let Some(name) = name.as_str().filter(|name| !name.trim().is_empty()) else {
            return Err(
                "model aliases require a non-default models.* name and a model name".into(),
            );
        };
        if !reference.starts_with("models.") || reference == "models.default" {
            return Err(
                "model aliases require a non-default models.* name and a model name".into(),
            );
        }
        names.insert(reference, name.trim());
    }

    let image_models = value("ANCHOR_MODEL_IMAGE_MODELS").unwrap_or_else(|| "[]".into());
    let image_models: Vec<String> = serde_json::from_str(&image_models)
        .map_err(|_| "ANCHOR_MODEL_IMAGE_MODELS must be a JSON array of model names".to_owned())?;
    if image_models.iter().any(|name| name.trim().is_empty()) {
        return Err("ANCHOR_MODEL_IMAGE_MODELS requires nonempty model names".into());
    }
    let image_models = image_models.into_iter().collect::<BTreeSet<_>>();

    let binding_identity = |name: &str| {
        serde_json::to_string(&(&base_url, &wire, name))
            .expect("endpoint, wire and model names are JSON strings")
    };
    let transport = |name: &str| {
        RigCompletionPort::openai_compatible(api_key.clone(), base_url.clone(), name, &wire)
            .map(|provider| provider.dyn_model())
            .map_err(|_| "could not construct the configured Rig model transport".to_owned())
    };
    let mut registry = RigModelRegistry::new_with_image_capability(
        transport(&model)?,
        &binding_identity(&model),
        image_models.contains(&model),
    );
    for (reference, name) in names {
        registry = registry.with_alias_and_image_capability(
            reference,
            transport(name)?,
            &binding_identity(name),
            image_models.contains(name),
        )?;
    }
    Ok(Some(registry))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_constructor_is_available_to_host_entry_point() {
        let _constructor: fn() -> Result<Option<RigModelRegistry>, String> = from_env;
    }

    fn configured(aliases: &str) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("ANCHOR_MODEL_URL".into(), "http://127.0.0.1:1/v1".into()),
            ("ANCHOR_MODEL_API_KEY".into(), "test-secret".into()),
            ("ANCHOR_MODEL_NAME".into(), "default-wire-name".into()),
            ("ANCHOR_MODEL_ALIASES".into(), aliases.into()),
        ])
    }

    fn registry(values: &BTreeMap<String, String>) -> RigModelRegistry {
        from_values(|name| values.get(name).cloned())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn aliases_use_trimmed_wire_names_and_unknown_references_fall_back() {
        let models = registry(&configured(
            r#"{"models.research":" research-wire ","models.review":"review-wire"}"#,
        ));
        assert_eq!(
            models.model(Some("models.research")).id(),
            Some("research-wire")
        );
        assert_eq!(
            models.model(Some("models.review")).id(),
            Some("review-wire")
        );
        for reference in [
            None,
            Some("models.default"),
            Some("models.unknown"),
            Some("other"),
        ] {
            assert_eq!(models.model(reference).id(), Some("default-wire-name"));
        }
    }

    #[test]
    fn unset_optional_configuration_uses_python_defaults() {
        let mut values = configured("");
        values.remove("ANCHOR_MODEL_NAME");
        let models = registry(&values);
        assert_eq!(models.model(None).id(), Some("default"));
        assert!(!models.accepts_images(None));
    }

    #[test]
    fn image_capability_uses_exact_wire_model_name_for_aliases_and_fallback() {
        let mut values =
            configured(r#"{"models.research":" vision-wire ","models.review":"text-wire"}"#);
        values.insert(
            "ANCHOR_MODEL_IMAGE_MODELS".into(),
            r#"["vision-wire","default-wire-name"]"#.into(),
        );
        let models = registry(&values);
        assert!(models.accepts_images(Some("models.research")));
        assert!(!models.accepts_images(Some("models.review")));
        for reference in [None, Some("models.default"), Some("models.unknown")] {
            assert!(models.accepts_images(reference));
        }
        values.insert(
            "ANCHOR_MODEL_IMAGE_MODELS".into(),
            r#"["models.research"]"#.into(),
        );
        assert!(!registry(&values).accepts_images(Some("models.research")));
    }

    #[test]
    fn invalid_image_model_configuration_is_rejected_without_echoing_values() {
        for config in [
            "",
            "not-secret-json",
            "{}",
            r#"[7]"#,
            r#"[null]"#,
            r#"[""]"#,
            r#"["  "]"#,
        ] {
            let mut values = configured("{}");
            values.insert("ANCHOR_MODEL_IMAGE_MODELS".into(), config.into());
            let error = from_values(|name| values.get(name).cloned()).err().unwrap();
            assert!(error.contains("ANCHOR_MODEL_IMAGE_MODELS"));
            assert!(!error.contains("not-secret-json"));
        }
    }

    #[test]
    fn missing_credentials_keep_agent_transport_unconfigured() {
        assert!(from_values(|_| None).unwrap().is_none());
        for missing in ["ANCHOR_MODEL_API_KEY", "ANCHOR_MODEL_URL"] {
            let mut values = configured("{}");
            values.remove(missing);
            assert!(
                from_values(|name| values.get(name).cloned())
                    .unwrap()
                    .is_none()
            );
        }
    }

    #[test]
    fn invalid_aliases_and_wire_are_rejected_without_echoing_config_values() {
        for aliases in [
            "[\"secret-value\"]",
            "invalid-secret-value",
            r#"{"models.default":"secret-value"}"#,
            r#"{"other":"secret-value"}"#,
            r#"{"models.empty":"  "}"#,
            r#"{"models.nonstring":7}"#,
        ] {
            let values = configured(aliases);
            let error = from_values(|name| values.get(name).cloned()).err().unwrap();
            assert!(!error.contains("secret-value"));
        }
        let mut values = configured("{}");
        values.insert("ANCHOR_MODEL_WIRE_API".into(), "secret-value".into());
        assert_eq!(
            from_values(|name| values.get(name).cloned()).err().unwrap(),
            "ANCHOR_MODEL_WIRE_API must be chat or responses",
        );
        values.insert(
            "ANCHOR_MODEL_URL".into(),
            "not-an-endpoint-secret-value".into(),
        );
        assert_eq!(
            from_values(|name| values.get(name).cloned()).err().unwrap(),
            "ANCHOR_MODEL_URL must be an absolute HTTP(S) endpoint",
        );
    }

    #[tokio::test]
    async fn configured_alias_wire_models_reach_chat_and_default_responses_endpoint() {
        use std::sync::{Arc, Mutex};

        use axum::{Json, Router, extract::State, routing::post};
        use serde_json::{Value, json};

        type Requests = Arc<Mutex<Vec<Value>>>;
        async fn chat(State(seen): State<Requests>, Json(request): Json<Value>) -> Json<Value> {
            seen.lock()
                .unwrap()
                .push(json!({"wire":"chat", "model":request["model"]}));
            Json(json!({
                "id":"chat-fixture", "object":"chat.completion", "created":0,
                "model":request["model"],
                "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2},
            }))
        }
        async fn responses(
            State(seen): State<Requests>,
            Json(request): Json<Value>,
        ) -> Json<Value> {
            seen.lock()
                .unwrap()
                .push(json!({"wire":"responses", "model":request["model"]}));
            Json(json!({
                "id":"responses-fixture", "object":"response", "created_at":0, "status":"completed",
                "model":request["model"], "error":null, "incomplete_details":null,
                "instructions":null, "max_output_tokens":null, "tools":[],
                "output":[{"id":"message-fixture","type":"message","role":"assistant","status":"completed",
                           "content":[{"type":"output_text","text":"ok","annotations":[]}]}],
                "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},
            }))
        }
        let seen = Requests::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/v1/responses", post(responses))
            .with_state(seen.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        for wire in [None, Some("chat")] {
            let mut values =
                configured(r#"{"models.research":"research-wire","models.review":"review-wire"}"#);
            values.insert("ANCHOR_MODEL_URL".into(), base_url.clone());
            if let Some(wire) = wire {
                values.insert("ANCHOR_MODEL_WIRE_API".into(), wire.into());
            }
            let models = registry(&values);
            for reference in ["models.research", "models.review", "models.unknown"] {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    models
                        .model(Some(reference))
                        .call(rig_core::completion::CompletionRequest::new("probe")),
                )
                .await
                .unwrap()
                .unwrap();
            }
        }
        server.abort();
        let _ = server.await;
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                json!({"wire":"responses","model":"research-wire"}),
                json!({"wire":"responses","model":"review-wire"}),
                json!({"wire":"responses","model":"default-wire-name"}),
                json!({"wire":"chat","model":"research-wire"}),
                json!({"wire":"chat","model":"review-wire"}),
                json!({"wire":"chat","model":"default-wire-name"}),
            ]
        );
    }
}
