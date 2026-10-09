use std::collections::BTreeMap;
use std::net::IpAddr;

use anchor_runtime::SandboxEnvironment;
use reqwest::Url;
use sha2::{Digest, Sha256};

pub(super) fn binary() -> Result<(std::path::PathBuf, String), String> {
    if std::env::var("ANCHOR_GOOSE_ALLOW_SHARED_NETWORK").as_deref() != Ok("1") {
        return Err("Goose requires explicit shared control network authorization".into());
    }
    let binary = std::env::var_os("ANCHOR_GOOSE_BINARY")
        .map(std::path::PathBuf::from)
        .ok_or("ANCHOR_GOOSE_BINARY is required")?
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let digest = super::file_sha256(&binary)?;
    if std::env::var("ANCHOR_GOOSE_BINARY_SHA256").ok().as_deref() != Some(&digest) {
        return Err("ANCHOR_GOOSE_BINARY_SHA256 must match the configured binary".into());
    }
    Ok((binary, digest))
}

pub(super) fn command(
    sandbox: &anchor_sandbox_bwrap::BubblewrapSandbox,
    directory: &std::path::Path,
    binary: &std::path::Path,
    environment: Vec<SandboxEnvironment>,
    cancellation: anchor_runtime::Cancellation,
) -> Result<tokio::process::Command, String> {
    let grant = anchor_runtime::ReadOnlyInput {
        source: binary.to_path_buf(),
        destination: "/tools/goose".into(),
    };
    let sandbox = sandbox
        .with_readonly_grants(std::slice::from_ref(&grant))
        .map_err(|error| error.to_string())?;
    // The packaged Goose is the Anchor-built lean ACP server (`goose-acp`), which speaks
    // ACP on stdio directly and takes `--with-builtin` as its only argument. The upstream
    // `goose acp` subcommand form belongs to the full CLI and is intentionally not used.
    let mut process = anchor_runtime::SandboxRequest::new(directory, ["/tools/goose"]);
    process.readonly_inputs.push(grant);
    process.network = anchor_runtime::NetworkPolicy::Enabled;
    process.cancellation = cancellation;
    process.environment = [
        ("GOOSE_MODE", "auto"),
        ("GOOSE_PATH_ROOT", "/workspace"),
        ("GOOSE_TELEMETRY_OFF", "1"),
        ("NO_COLOR", "1"),
        ("RUST_LOG", "warn"),
    ]
    .into_iter()
    .map(|(name, value)| SandboxEnvironment::new(name, value))
    .collect();
    process.environment.extend(environment);
    sandbox
        .isolated_command(process)
        .map_err(|error| error.to_string())
}

pub(super) struct ModelRegistry {
    fixture: bool,
    endpoint: Url,
    wire: &'static str,
    model: String,
    api_key: String,
    aliases: BTreeMap<String, String>,
}

pub(super) struct ModelBinding {
    pub(super) model: String,
    pub(super) identity: String,
}

impl ModelRegistry {
    pub(super) fn from_env(fixture: bool) -> Result<Self, String> {
        let mut invalid_unicode = None;
        let registry = Self::from_values(fixture, |name| match std::env::var(name) {
            Ok(value) => Some(value),
            Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                invalid_unicode = Some(name.to_owned());
                None
            }
        });
        if let Some(name) = invalid_unicode {
            return Err(format!("{name} must be valid Unicode"));
        }
        registry
    }

    fn from_values(
        fixture: bool,
        mut value: impl FnMut(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let (endpoint, wire, model, api_key, aliases) = if fixture {
            let host =
                value("ANCHOR_GOOSE_OPENAI_HOST").ok_or("ANCHOR_GOOSE_OPENAI_HOST is required")?;
            let mut endpoint = configured_url(&host, true)?;
            endpoint.set_path("/v1/chat/completions");
            let model = model_name(
                value("ANCHOR_GOOSE_MODEL").unwrap_or_else(|| "fixture-goose".into()),
                "ANCHOR_GOOSE_MODEL",
            )?;
            (endpoint, "chat", model, String::new(), BTreeMap::new())
        } else {
            let host = value("ANCHOR_MODEL_URL").ok_or("ANCHOR_MODEL_URL is required")?;
            let mut endpoint = configured_url(&host, false)?;
            let api_key =
                value("ANCHOR_MODEL_API_KEY").ok_or("ANCHOR_MODEL_API_KEY is required")?;
            validate_text(&api_key, "ANCHOR_MODEL_API_KEY")?;
            let model = model_name(
                value("ANCHOR_MODEL_NAME").unwrap_or_else(|| "default".into()),
                "ANCHOR_MODEL_NAME",
            )?;
            let wire = value("ANCHOR_MODEL_WIRE_API").unwrap_or_else(|| "responses".into());
            let (wire, suffix) = match wire.as_str() {
                "chat" => ("chat", "chat/completions"),
                "responses" => ("responses", "responses"),
                _ => return Err("ANCHOR_MODEL_WIRE_API must be chat or responses".into()),
            };
            let path = format!("{}/{suffix}", endpoint.path().trim_end_matches('/'));
            endpoint.set_path(&path);
            let aliases = model_aliases(value("ANCHOR_MODEL_ALIASES").as_deref())?;
            (endpoint, wire, model, api_key, aliases)
        };
        Ok(Self {
            fixture,
            endpoint,
            wire,
            model,
            api_key,
            aliases,
        })
    }

    pub(super) fn resolve(&self, reference: Option<&str>) -> Result<ModelBinding, String> {
        let model = match reference {
            None | Some("models.default") => &self.model,
            Some(reference) if reference == self.model => &self.model,
            Some(reference) => self
                .aliases
                .get(reference)
                .ok_or("model reference is not explicitly configured for Goose")?,
        };
        let identity = serde_json::to_vec(&(self.endpoint.as_str(), self.wire, model))
            .expect("model binding strings are serializable");
        Ok(ModelBinding {
            model: model.clone(),
            identity: format!("{:x}", Sha256::digest(identity)),
        })
    }

    pub(super) fn fixture_upstream(&self) -> Option<Url> {
        self.fixture.then(|| self.endpoint.clone())
    }

    pub(super) fn environment(
        &self,
        binding: &ModelBinding,
        bridge_url: &str,
        bridge_token: &str,
    ) -> Vec<SandboxEnvironment> {
        let (host, base_path, api_key) = if self.fixture {
            (
                bridge_url.to_owned(),
                "v1/chat/completions".to_owned(),
                bridge_token,
            )
        } else {
            (
                self.endpoint.origin().ascii_serialization(),
                format!(".{}", self.endpoint.path()),
                self.api_key.as_str(),
            )
        };
        vec![
            SandboxEnvironment::new("GOOSE_PROVIDER", "openai"),
            SandboxEnvironment::new("GOOSE_MODEL", binding.model.clone()),
            SandboxEnvironment::new("OPENAI_HOST", host),
            SandboxEnvironment::new("OPENAI_BASE_PATH", base_path),
            SandboxEnvironment::new("OPENAI_API_KEY", api_key),
        ]
    }
}

fn validate_text(value: &str, name: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        return Err(format!(
            "{name} must be nonempty and contain no control characters"
        ));
    }
    Ok(())
}

fn model_name(value: String, name: &str) -> Result<String, String> {
    validate_text(&value, name)?;
    Ok(value.trim().to_owned())
}

fn configured_url(value: &str, fixture: bool) -> Result<Url, String> {
    let error = if fixture {
        "ANCHOR_GOOSE_OPENAI_HOST must be an HTTP loopback IP origin without credentials, path, query or fragment"
    } else {
        "ANCHOR_MODEL_URL must be an HTTPS endpoint or HTTP loopback IP endpoint without credentials, query or fragment"
    };
    let (_, remainder) = value.split_once("://").ok_or(error)?;
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control() || character == '\\')
        || authority.is_empty()
        || authority.contains('@')
        || (fixture && !matches!(&remainder[authority.len()..], "" | "/"))
    {
        return Err(error.into());
    }
    let url = Url::parse(value).map_err(|_| error.to_owned())?;
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
    });
    let permitted_scheme = if fixture {
        url.scheme() == "http" && loopback
    } else {
        url.scheme() == "https" || (url.scheme() == "http" && loopback)
    };
    if !permitted_scheme
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(error.into());
    }
    Ok(url)
}

fn model_aliases(value: Option<&str>) -> Result<BTreeMap<String, String>, String> {
    let value = value.filter(|value| !value.is_empty()).unwrap_or("{}");
    let aliases: serde_json::Value = serde_json::from_str(value)
        .map_err(|_| "ANCHOR_MODEL_ALIASES must be a JSON object".to_owned())?;
    let aliases = aliases
        .as_object()
        .ok_or("ANCHOR_MODEL_ALIASES must be a JSON object")?;
    let mut models = BTreeMap::new();
    for (reference, model) in aliases {
        if !reference.starts_with("models.")
            || reference.len() == "models.".len()
            || reference == "models.default"
            || reference.chars().any(char::is_whitespace)
            || reference.chars().any(char::is_control)
        {
            return Err("model aliases require a non-default models.* reference".into());
        }
        let model = model
            .as_str()
            .ok_or("model aliases require a nonempty model name without control characters")?;
        let model = model_name(model.to_owned(), "model alias name")?;
        models.insert(reference.clone(), model);
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_values() -> BTreeMap<String, String> {
        BTreeMap::from([(
            "ANCHOR_GOOSE_OPENAI_HOST".into(),
            "http://127.0.0.1:43210".into(),
        )])
    }

    fn native_values() -> BTreeMap<String, String> {
        BTreeMap::from([
            (
                "ANCHOR_MODEL_URL".into(),
                "https://provider.example/gateway/v1".into(),
            ),
            ("ANCHOR_MODEL_API_KEY".into(), "operator-secret".into()),
            ("ANCHOR_MODEL_NAME".into(), "primary-model".into()),
        ])
    }

    fn registry(fixture: bool, values: &BTreeMap<String, String>) -> ModelRegistry {
        ModelRegistry::from_values(fixture, |name| values.get(name).cloned()).unwrap()
    }

    fn environment(models: &ModelRegistry, reference: Option<&str>) -> BTreeMap<String, String> {
        let binding = models.resolve(reference).unwrap();
        let entries = models.environment(&binding, "http://127.0.0.1:54321", "bridge-secret");
        assert_eq!(entries.len(), 5);
        assert!(!format!("{entries:?}").contains("secret"));
        entries
            .into_iter()
            .map(|entry| (entry.key.clone(), entry.value().to_owned()))
            .collect()
    }

    fn error(fixture: bool, values: &BTreeMap<String, String>) -> String {
        ModelRegistry::from_values(fixture, |name| values.get(name).cloned())
            .err()
            .expect("configuration must be rejected")
    }

    #[test]
    fn environment_constructor_has_the_host_api() {
        let _constructor: fn(bool) -> Result<ModelRegistry, String> = ModelRegistry::from_env;
    }

    #[test]
    fn fixture_defaults_and_environment_use_only_the_bridge() {
        let mut values = fixture_values();
        values.extend(native_values());
        values.insert("ANCHOR_MODEL_ALIASES".into(), "invalid JSON".into());
        let models = registry(true, &values);
        assert_eq!(
            models.fixture_upstream().unwrap().as_str(),
            "http://127.0.0.1:43210/v1/chat/completions"
        );
        assert_eq!(
            environment(&models, None),
            BTreeMap::from([
                ("GOOSE_PROVIDER".into(), "openai".into()),
                ("GOOSE_MODEL".into(), "fixture-goose".into()),
                ("OPENAI_HOST".into(), "http://127.0.0.1:54321".into()),
                ("OPENAI_BASE_PATH".into(), "v1/chat/completions".into()),
                ("OPENAI_API_KEY".into(), "bridge-secret".into()),
            ])
        );
    }

    #[test]
    fn fixture_accepts_only_loopback_ip_origins() {
        let mut values = fixture_values();
        for (host, expected) in [
            ("http://127.0.0.1", "http://127.0.0.1/v1/chat/completions"),
            (
                "http://127.5.6.7:8080/",
                "http://127.5.6.7:8080/v1/chat/completions",
            ),
            ("http://[::1]:8080", "http://[::1]:8080/v1/chat/completions"),
        ] {
            values.insert("ANCHOR_GOOSE_OPENAI_HOST".into(), host.into());
            assert_eq!(
                registry(true, &values).fixture_upstream().unwrap().as_str(),
                expected
            );
        }
        for host in [
            "https://127.0.0.1",
            "http://localhost",
            "http://provider.example",
            "http://192.0.2.1",
            "http://[::2]",
            "http://127.0.0.1/v1",
            "http://127.0.0.1/a/..",
            "http://127.0.0.1//",
            "http://secret@127.0.0.1",
            "http://user:secret@127.0.0.1",
            "http://@127.0.0.1",
            "http://127.0.0.1?secret",
            "http://127.0.0.1?",
            "http://127.0.0.1#secret",
            "http://127.0.0.1#",
            "http:127.0.0.1",
            "http:///127.0.0.1",
            "http://127.0.0.1/\\..",
            " http://127.0.0.1",
            "http://127.0.0.1\n",
            "",
            "secret",
        ] {
            values.insert("ANCHOR_GOOSE_OPENAI_HOST".into(), host.into());
            let failure = error(true, &values);
            assert!(failure.contains("ANCHOR_GOOSE_OPENAI_HOST"));
            assert!(!failure.contains("secret"));
        }
        values.remove("ANCHOR_GOOSE_OPENAI_HOST");
        assert_eq!(error(true, &values), "ANCHOR_GOOSE_OPENAI_HOST is required");
    }

    #[test]
    fn fixture_resolves_only_default_or_the_configured_model() {
        let mut values = fixture_values();
        values.insert("ANCHOR_GOOSE_MODEL".into(), "configured-model".into());
        values.insert(
            "ANCHOR_MODEL_ALIASES".into(),
            r#"{"models.alias":"configured-model"}"#.into(),
        );
        let models = registry(true, &values);
        for reference in [None, Some("models.default"), Some("configured-model")] {
            assert_eq!(models.resolve(reference).unwrap().model, "configured-model");
        }
        for reference in [
            "models.alias",
            "models.unknown",
            "fixture-goose",
            "default",
            "",
        ] {
            assert!(models.resolve(Some(reference)).is_err());
        }
    }

    #[test]
    fn fixture_rejects_empty_or_control_character_model_names() {
        let mut values = fixture_values();
        for model in [
            "",
            "  ",
            "secret\n",
            "secret\r",
            "secret\0",
            "secret\t",
            "secret\u{7f}",
            "secret\u{85}",
        ] {
            values.insert("ANCHOR_GOOSE_MODEL".into(), model.into());
            let failure = error(true, &values);
            assert!(failure.contains("ANCHOR_GOOSE_MODEL"));
            assert!(!failure.contains("secret"));
        }
    }

    #[test]
    fn native_chat_and_responses_keep_the_configured_prefix() {
        let mut values = native_values();
        for (url, host, prefix) in [
            ("https://provider.example", "https://provider.example", ""),
            ("https://provider.example/", "https://provider.example", ""),
            (
                "https://provider.example/v1",
                "https://provider.example",
                "v1/",
            ),
            (
                "https://provider.example/gateway/openai/v1/",
                "https://provider.example",
                "gateway/openai/v1/",
            ),
            (
                "https://provider.example:8443/custom/api",
                "https://provider.example:8443",
                "custom/api/",
            ),
            (
                "https://provider.example/team%2Fname/v1",
                "https://provider.example",
                "team%2Fname/v1/",
            ),
            (
                "http://127.0.0.1:8080/custom/v1/",
                "http://127.0.0.1:8080",
                "custom/v1/",
            ),
            (
                "http://[::1]:8080/custom/v1",
                "http://[::1]:8080",
                "custom/v1/",
            ),
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), url.into());
            for (wire, suffix) in [("chat", "chat/completions"), ("responses", "responses")] {
                values.insert("ANCHOR_MODEL_WIRE_API".into(), wire.into());
                let models = registry(false, &values);
                assert!(models.fixture_upstream().is_none());
                let entries = environment(&models, None);
                assert_eq!(entries["OPENAI_HOST"], host);
                assert_eq!(entries["OPENAI_BASE_PATH"], format!("./{prefix}{suffix}"));
                assert_eq!(
                    Url::parse(&entries["OPENAI_HOST"])
                        .unwrap()
                        .join(&entries["OPENAI_BASE_PATH"])
                        .unwrap(),
                    models.endpoint
                );
            }
        }
    }

    #[test]
    fn native_environment_calls_the_endpoint_without_bridge_credentials() {
        let mut values = native_values();
        values.extend(fixture_values());
        let models = registry(false, &values);
        assert_eq!(
            environment(&models, None),
            BTreeMap::from([
                ("GOOSE_PROVIDER".into(), "openai".into()),
                ("GOOSE_MODEL".into(), "primary-model".into()),
                ("OPENAI_HOST".into(), "https://provider.example".into()),
                ("OPENAI_BASE_PATH".into(), "./gateway/v1/responses".into()),
                ("OPENAI_API_KEY".into(), "operator-secret".into()),
            ])
        );
    }

    #[test]
    fn native_base_path_cannot_change_origin_through_url_join() {
        let mut values = native_values();
        for prefix in [
            "/https://other.example/v1",
            "/https://user:secret@other.example/v1",
            "//other.example/v1",
            "///other.example/v1",
            "/tenant:name/v1",
        ] {
            values.insert(
                "ANCHOR_MODEL_URL".into(),
                format!("https://provider.example{prefix}"),
            );
            for wire in ["chat", "responses"] {
                values.insert("ANCHOR_MODEL_WIRE_API".into(), wire.into());
                let models = registry(false, &values);
                let entries = environment(&models, None);
                let joined = Url::parse(&entries["OPENAI_HOST"])
                    .unwrap()
                    .join(&entries["OPENAI_BASE_PATH"])
                    .unwrap();
                assert_eq!(joined, models.endpoint);
                assert_eq!(
                    joined.origin().ascii_serialization(),
                    "https://provider.example"
                );
                assert!(joined.username().is_empty());
                assert!(joined.password().is_none());
            }
        }
    }

    #[test]
    fn native_allows_https_or_explicit_http_loopback_only() {
        let mut values = native_values();
        for url in [
            "https://provider.example/v1",
            "https://192.0.2.1/v1",
            "https://localhost/v1",
            "http://127.0.0.1/v1",
            "http://127.255.255.254/v1",
            "http://[::1]/v1",
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), url.into());
            assert!(ModelRegistry::from_values(false, |name| values.get(name).cloned()).is_ok());
        }
        for url in [
            "http://provider.example/v1",
            "http://192.0.2.1/v1",
            "http://localhost/v1",
            "http://[::2]/v1",
            "ftp://provider.example/v1",
            "https://user:secret@provider.example/v1",
            "https://secret@provider.example/v1",
            "https://@provider.example/v1",
            "https://provider.example/v1?token=secret",
            "https://provider.example/v1?",
            "https://provider.example/v1#secret",
            "https://provider.example/v1#",
            "https:provider.example/v1",
            "https:///provider.example/v1",
            "https://",
            "https://provider.example:secret/v1",
            "https://provider.example/\nsecret",
            "https://provider.example/\\secret",
            "",
            "secret",
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), url.into());
            let failure = error(false, &values);
            assert!(failure.contains("ANCHOR_MODEL_URL"));
            assert!(!failure.contains("secret"));
        }
    }

    #[test]
    fn native_requires_endpoint_and_key_and_preserves_existing_name_default() {
        for missing in ["ANCHOR_MODEL_URL", "ANCHOR_MODEL_API_KEY"] {
            let mut values = native_values();
            values.remove(missing);
            assert_eq!(error(false, &values), format!("{missing} is required"));
        }
        let mut values = native_values();
        values.remove("ANCHOR_MODEL_NAME");
        let models = registry(false, &values);
        assert_eq!(models.resolve(None).unwrap().model, "default");
    }

    #[test]
    fn native_rejects_empty_control_character_or_unsupported_values_without_echoing() {
        for name in [
            "ANCHOR_MODEL_API_KEY",
            "ANCHOR_MODEL_NAME",
            "ANCHOR_MODEL_WIRE_API",
        ] {
            for invalid in [
                "",
                "  ",
                "secret\n",
                "secret\r",
                "secret\0",
                "secret\t",
                "secret\u{7f}",
                "secret\u{85}",
            ] {
                let mut values = native_values();
                values.insert(name.into(), invalid.into());
                let failure = error(false, &values);
                assert!(failure.contains(name));
                assert!(!failure.contains("secret"));
            }
        }
        let mut values = native_values();
        values.insert("ANCHOR_MODEL_WIRE_API".into(), "secret-wire".into());
        assert_eq!(
            error(false, &values),
            "ANCHOR_MODEL_WIRE_API must be chat or responses"
        );
    }

    #[test]
    fn aliases_are_explicit_and_unknown_references_never_fall_back() {
        let mut values = native_values();
        values.insert("ANCHOR_MODEL_ALIASES".into(), r#"{"models.research":" research-model ","models.review":"review-model","models.same":"primary-model"}"#.into());
        let models = registry(false, &values);
        for reference in [None, Some("models.default"), Some("primary-model")] {
            assert_eq!(models.resolve(reference).unwrap().model, "primary-model");
        }
        let research = models.resolve(Some("models.research")).unwrap();
        assert_eq!(research.model, "research-model");
        assert_eq!(
            environment(&models, Some("models.research"))["GOOSE_MODEL"],
            "research-model"
        );
        assert_eq!(
            models.resolve(Some("models.review")).unwrap().model,
            "review-model"
        );
        assert_eq!(
            models.resolve(Some("models.same")).unwrap().identity,
            models.resolve(None).unwrap().identity
        );
        for reference in [
            "models.unknown",
            "research-model",
            "review-model",
            "default",
            "",
            "secret-reference",
        ] {
            let failure = models.resolve(Some(reference)).err().unwrap();
            assert!(!failure.contains("secret"));
        }
    }

    #[test]
    fn aliases_reject_invalid_json_references_and_model_names_without_echoing() {
        let mut values = native_values();
        for aliases in [
            "secret-json",
            "[]",
            "null",
            r#""secret""#,
            r#"{"secret":"model"}"#,
            r#"{"models.":"model"}"#,
            r#"{"models.default":"secret"}"#,
            r#"{"models. secret":"model"}"#,
            r#"{"models.secret\n":"model"}"#,
            r#"{"models.secret":null}"#,
            r#"{"models.secret":3}"#,
            r#"{"models.secret":""}"#,
            r#"{"models.secret":"   "}"#,
            r#"{"models.secret":"secret\n"}"#,
            r#"{"models.secret":"secret\t"}"#,
            r#"{"models.secret":"secret\u0000"}"#,
            r#"{"models.secret":"secret\u007f"}"#,
            r#"{"models.secret":"secret\u0085"}"#,
        ] {
            values.insert("ANCHOR_MODEL_ALIASES".into(), aliases.into());
            assert!(!error(false, &values).contains("secret"));
        }
        for aliases in ["", "{}"] {
            values.insert("ANCHOR_MODEL_ALIASES".into(), aliases.into());
            assert!(
                registry(false, &values)
                    .resolve(Some("models.unknown"))
                    .is_err()
            );
        }
    }

    #[test]
    fn identity_is_a_normalized_endpoint_wire_and_model_hash_without_the_key() {
        let mut values = native_values();
        let original = registry(false, &values).resolve(None).unwrap();
        assert_eq!(
            original.identity,
            "e2c3f48fcf29379264d15917b7ae9fc1f0d2a157d324107522925ba1aa3f4972"
        );
        assert_eq!(original.identity.len(), 64);
        assert!(
            original
                .identity
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert!(!original.identity.contains("operator-secret"));
        for endpoint in [
            "https://PROVIDER.example:443/gateway/v1/",
            "https://provider.example/gateway/ignored/../v1",
        ] {
            values.insert("ANCHOR_MODEL_URL".into(), endpoint.into());
            assert_eq!(
                registry(false, &values).resolve(None).unwrap().identity,
                original.identity
            );
        }
        values.insert("ANCHOR_MODEL_API_KEY".into(), "rotated-secret".into());
        let rotated = registry(false, &values);
        assert_eq!(rotated.resolve(None).unwrap().identity, original.identity);
        assert_eq!(
            environment(&rotated, None)["OPENAI_API_KEY"],
            "rotated-secret"
        );
        for (name, replacement) in [
            ("ANCHOR_MODEL_URL", "https://other.example/gateway/v1"),
            ("ANCHOR_MODEL_URL", "https://provider.example/other/v1"),
            ("ANCHOR_MODEL_WIRE_API", "chat"),
            ("ANCHOR_MODEL_NAME", "other-model"),
        ] {
            let mut changed = native_values();
            changed.insert(name.into(), replacement.into());
            assert_ne!(
                registry(false, &changed).resolve(None).unwrap().identity,
                original.identity
            );
        }
    }

    #[test]
    fn fixture_identity_tracks_upstream_and_model_but_not_the_bridge() {
        let values = fixture_values();
        let models = registry(true, &values);
        let binding = models.resolve(None).unwrap();
        models.environment(&binding, "http://127.0.0.1:54321", "first-secret");
        models.environment(&binding, "http://127.0.0.1:54322", "rotated-secret");
        assert_eq!(models.resolve(None).unwrap().identity, binding.identity);
        assert_eq!(
            models.resolve(Some("models.default")).unwrap().identity,
            binding.identity
        );
        for (name, replacement) in [
            ("ANCHOR_GOOSE_OPENAI_HOST", "http://127.0.0.1:43211"),
            ("ANCHOR_GOOSE_MODEL", "other-model"),
        ] {
            let mut changed = values.clone();
            changed.insert(name.into(), replacement.into());
            assert_ne!(
                registry(true, &changed).resolve(None).unwrap().identity,
                binding.identity
            );
        }
    }

    #[test]
    fn configuration_reads_only_its_explicit_mode_specific_variables() {
        for (fixture, expected) in [
            (true, vec!["ANCHOR_GOOSE_OPENAI_HOST", "ANCHOR_GOOSE_MODEL"]),
            (
                false,
                vec![
                    "ANCHOR_MODEL_URL",
                    "ANCHOR_MODEL_API_KEY",
                    "ANCHOR_MODEL_NAME",
                    "ANCHOR_MODEL_WIRE_API",
                    "ANCHOR_MODEL_ALIASES",
                ],
            ),
        ] {
            let values = if fixture {
                fixture_values()
            } else {
                native_values()
            };
            let mut read = Vec::new();
            ModelRegistry::from_values(fixture, |name| {
                read.push(name.to_owned());
                values.get(name).cloned()
            })
            .unwrap();
            assert_eq!(read, expected);
        }
    }
}
