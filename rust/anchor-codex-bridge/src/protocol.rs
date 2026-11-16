//! Installed Codex protocol discovery, without authorization or execution policy.

use serde_json::{Value, json};
use std::{collections::BTreeMap, fs, path::Path, process::Stdio, time::Duration};
use tokio::process::Command;

const DIRECTIONS: [(&str, &str); 3] = [
    ("request", "ClientRequest.json"),
    ("notification", "ServerNotification.json"),
    ("server_request", "ServerRequest.json"),
];

/// Manager owns the OnceCell; this catalog contains only installed-version schemas.
#[derive(Debug)]
pub struct ProtocolCatalog {
    documents: BTreeMap<&'static str, Value>,
    responses: BTreeMap<String, Value>,
}

impl ProtocolCatalog {
    #[cfg(test)]
    pub(crate) fn writer_fixture() -> Self {
        let properties = json!({
            "threadId": {"type":"string"}, "cwd": {"type":"string"},
            "sandbox": {"type":"string"}, "sandboxPolicy": {"type":"object"},
            "input": {"type":"array"}
        });
        let variants: Vec<_> = [
            "thread/start",
            "thread/resume",
            "turn/start",
            "thread/settings/update",
        ]
        .into_iter()
        .map(|method| {
            json!({"properties":{
                "method":{"enum":[method]},
                "params":{"type":"object","properties":properties}
            }})
        })
        .collect();
        Self {
            documents: BTreeMap::from([("request", json!({"oneOf":variants}))]),
            responses: BTreeMap::new(),
        }
    }

    pub async fn load(codex_bin: &Path) -> Result<Self, String> {
        Self::load_with_timeout(codex_bin, Duration::from_secs(30)).await
    }

    async fn load_with_timeout(codex_bin: &Path, timeout: Duration) -> Result<Self, String> {
        let directory = tempfile::Builder::new()
            .prefix("anchor-codex-protocol-")
            .tempdir()
            .map_err(|error| format!("create Codex schema directory: {error}"))?;
        let mut child = schema_command(codex_bin, directory.path())
            .spawn()
            .map_err(|error| format!("start Codex schema generator: {error}"))?;
        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(status) => {
                status.map_err(|error| format!("wait for Codex schema generator: {error}"))?
            }
            Err(_) => {
                let _ = tokio::time::timeout(Duration::from_secs(2), child.kill()).await;
                return Err("Codex schema generation timed out".into());
            }
        };
        if !status.success() {
            return Err(format!("Codex schema generator exited with {status}"));
        }
        // Own the TempDir in the blocking task so cancellation cannot remove it mid-read.
        tokio::task::spawn_blocking(move || Self::from_directory(directory.path()))
            .await
            .map_err(|error| format!("load Codex protocol schemas: {error}"))?
    }

    fn from_directory(directory: &Path) -> Result<Self, String> {
        let mut documents = BTreeMap::new();
        for (direction, filename) in DIRECTIONS {
            let document = read_schema(&directory.join(filename))?;
            let variants = document["oneOf"]
                .as_array()
                .ok_or_else(|| format!("{filename} has no native method variants"))?;
            let mut names = std::collections::BTreeSet::new();
            for variant in variants {
                let name = method_name(variant)?;
                if !names.insert(name) {
                    return Err(format!("duplicate {direction} method {name}"));
                }
            }
            documents.insert(direction, document);
        }
        let mut responses = BTreeMap::new();
        for folder in [
            directory.to_path_buf(),
            directory.join("v1"),
            directory.join("v2"),
        ] {
            let entries = match fs::read_dir(&folder) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(format!("read Codex schema directory: {error}")),
            };
            for entry in entries {
                let path = entry
                    .map_err(|error| format!("read Codex schema entry: {error}"))?
                    .path();
                let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
                    continue;
                };
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                    && name.ends_with("Response")
                {
                    let response = read_schema(&path)?;
                    if let Some(previous) = responses.insert(name.to_owned(), response.clone())
                        && previous != response
                    {
                        return Err(format!("ambiguous native response schema {name}"));
                    }
                }
            }
        }
        Ok(Self {
            documents,
            responses,
        })
    }

    /// Sorted native method summaries. Unknown directions return an empty list.
    /// `notification` means server-to-client notifications, not client notifications.
    pub fn methods(&self, direction: &str) -> Vec<Value> {
        let Some(document) = self.documents.get(direction) else {
            return Vec::new();
        };
        let mut methods: Vec<_> = document["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .map(|variant| summary(direction, variant, document))
            .collect();
        methods.sort_by(|a, b| a["method"].as_str().cmp(&b["method"].as_str()));
        methods
    }

    /// Native schemas are self-contained; only transitively referenced definitions survive.
    /// Response lookup follows exported Params/Response names, not an invented wire contract.
    pub fn describe(&self, direction: &str, method: &str) -> Result<Value, String> {
        let document = self
            .documents
            .get(direction)
            .ok_or_else(|| format!("unknown protocol direction {direction}"))?;
        let variant = document["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|variant| method_name(variant).ok() == Some(method))
            .ok_or_else(|| format!("unknown native {direction} method {method}"))?;
        let mut description = summary(direction, variant, document);
        let params = &variant["properties"]["params"];
        description["params_schema"] = if params.is_null() {
            Value::Null
        } else {
            let schema = if let Some(reference) = params.get("$ref").and_then(Value::as_str) {
                definition_name(reference)?;
                document
                    .pointer(&reference[1..])
                    .ok_or_else(|| format!("missing native schema reference {reference}"))?
            } else {
                params
            };
            reachable_schema(schema, document)?
        };
        if direction != "notification" {
            let mut references = Vec::new();
            collect_references(params, &mut references);
            for reference in references {
                let name = definition_name(&reference)?;
                if let Some(stem) = name.strip_suffix("Params")
                    && let Some(response) = self.responses.get(&format!("{stem}Response"))
                {
                    description["response_schema"] = reachable_schema(response, response)?;
                    break;
                }
            }
        }
        Ok(description)
    }

    pub fn contains(&self, method: &str) -> bool {
        self.documents["request"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|variant| method_name(variant).ok() == Some(method))
    }
}

fn schema_command(codex_bin: &Path, directory: &Path) -> Command {
    let mut command = Command::new(codex_bin);
    command
        .args([
            "app-server",
            "generate-json-schema",
            "--experimental",
            "--out",
        ])
        .arg(directory)
        .env_remove(crate::config::TOKEN_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

fn read_schema(path: &Path) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn method_name(variant: &Value) -> Result<&str, String> {
    let names = variant
        .pointer("/properties/method/enum")
        .and_then(Value::as_array);
    names
        .filter(|names| names.len() == 1)
        .and_then(|names| names[0].as_str())
        .ok_or_else(|| "native schema variant must name exactly one method".to_owned())
}

fn summary(direction: &str, variant: &Value, document: &Value) -> Value {
    let mut value = json!({"direction": direction, "method": method_name(variant).unwrap()});
    if let Some(title) = variant.get("title") {
        value["title"] = title.clone();
    }
    let params = &variant["properties"]["params"];
    let resolved = params
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|pointer| document.pointer(pointer));
    if let Some(description) = variant
        .get("description")
        .or_else(|| params.get("description"))
        .or_else(|| resolved.and_then(|schema| schema.get("description")))
    {
        value["description"] = description.clone();
    }
    value
}

fn collect_references(value: &Value, references: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            if let Some(reference) = fields.get("$ref").and_then(Value::as_str) {
                references.push(reference.to_owned());
            }
            for (name, value) in fields {
                if name != "definitions" {
                    collect_references(value, references);
                }
            }
        }
        Value::Array(values) => values
            .iter()
            .for_each(|value| collect_references(value, references)),
        _ => {}
    }
}

fn definition_name(reference: &str) -> Result<String, String> {
    reference
        .strip_prefix("#/definitions/")
        .map(|name| {
            name.split('/')
                .next()
                .unwrap()
                .replace("~1", "/")
                .replace("~0", "~")
        })
        .ok_or_else(|| format!("unsupported native schema reference {reference}"))
}

fn reachable_schema(schema: &Value, document: &Value) -> Result<Value, String> {
    let mut schema = schema.clone();
    let fields = schema
        .as_object_mut()
        .ok_or("native schema must be an object")?;
    fields.remove("definitions");
    if let Some(dialect) = document.get("$schema") {
        fields.insert("$schema".into(), dialect.clone());
    }
    let mut pending = Vec::new();
    collect_references(&schema, &mut pending);
    let mut definitions = serde_json::Map::new();
    while let Some(reference) = pending.pop() {
        let name = definition_name(&reference)?;
        if document.pointer(&reference[1..]).is_none() {
            return Err(format!("missing native schema reference {reference}"));
        }
        if definitions.contains_key(&name) {
            continue;
        }
        let definition = &document["definitions"][&name];
        collect_references(definition, &mut pending);
        definitions.insert(name, definition.clone());
    }
    if !definitions.is_empty() {
        schema["definitions"] = Value::Object(definitions);
    }
    Ok(schema)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(directory: &Path) {
        let definitions = json!({
            "ExampleParams": {"type":"object","description":"Native parameter documentation.","required":["node"],"properties":{"node":{"$ref":"#/definitions/Node"}}},
            "ApprovalParams": {"type":"object","properties":{"command":{"type":"string"}}},
            "Node": {"description":"Recursive node.","properties":{"next":{"$ref":"#/definitions/Node"},"leaf":{"$ref":"#/definitions/Leaf~1Kind~0"}}},
            "Leaf/Kind~": {"type":"string","description":"Native leaf documentation."},
            "Unused": {"$ref":"#/definitions/Missing"}
        });
        for (_, filename) in DIRECTIONS {
            let method = match filename {
                "ClientRequest.json" => "example/read",
                "ServerNotification.json" => "example/changed",
                _ => "example/approve",
            };
            let params = if filename == "ServerRequest.json" {
                "#/definitions/ApprovalParams"
            } else {
                "#/definitions/ExampleParams"
            };
            let document = json!({"$schema":"http://json-schema.org/draft-07/schema#",
            "definitions": definitions, "oneOf":[
                    {"description":"Original method description.","properties":{"method":{"enum":[method]},"params":{"$ref":params}}},
                {"properties":{"method":{"enum":["empty"]},"params":{"type":"null"}}}
            ]});
            fs::write(
                directory.join(filename),
                serde_json::to_vec(&document).unwrap(),
            )
            .unwrap();
        }
        fs::create_dir(directory.join("v2")).unwrap();
        fs::write(directory.join("ApprovalResponse.json"), serde_json::to_vec(&json!({
            "type":"object","required":["decision"],"properties":{"decision":{"enum":["allow","deny"]}}
        })).unwrap()).unwrap();
        fs::write(directory.join("v2/ExampleResponse.json"), serde_json::to_vec(&json!({
            "description":"Native response.","type":"object",
            "properties":{"result":{"$ref":"#/definitions/Leaf~1Kind~0"}},"definitions": definitions
        })).unwrap()).unwrap();
    }

    #[test]
    fn catalog_preserves_directions_descriptions_and_reachable_schemas() {
        let directory = tempfile::tempdir().unwrap();
        fixture(directory.path());
        let catalog = ProtocolCatalog::from_directory(directory.path()).unwrap();
        assert!(catalog.contains("example/read"));
        assert!(!catalog.contains("example/changed"));
        assert!(!catalog.contains("example/approve"));
        assert!(!catalog.contains("absent"));
        let methods = catalog.methods("request");
        assert_eq!(
            methods
                .iter()
                .map(|v| v["method"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["empty", "example/read"]
        );
        assert_eq!(methods[1]["description"], "Original method description.");
        let detail = catalog.describe("request", "example/read").unwrap();
        let params = &detail["params_schema"];
        assert_eq!(params["properties"]["node"]["$ref"], "#/definitions/Node");
        assert_eq!(params["definitions"].as_object().unwrap().len(), 2);
        assert_eq!(params["description"], "Native parameter documentation.");
        assert_eq!(
            params["definitions"]["Leaf/Kind~"]["description"],
            "Native leaf documentation."
        );
        assert_eq!(detail["response_schema"]["description"], "Native response.");
        assert_eq!(
            detail["response_schema"]["definitions"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert!(
            catalog
                .describe("server_request", "example/approve")
                .unwrap()
                .get("response_schema")
                .is_some()
        );
        assert!(
            catalog
                .describe("notification", "example/changed")
                .unwrap()
                .get("response_schema")
                .is_none()
        );
        assert_eq!(
            catalog.describe("request", "empty").unwrap()["params_schema"]["type"],
            "null"
        );
        assert!(
            catalog
                .describe("request", "empty")
                .unwrap()
                .get("response_schema")
                .is_none()
        );
        assert!(catalog.methods("invalid").is_empty());
        assert!(catalog.describe("invalid", "empty").is_err());
        assert!(catalog.describe("request", "example/approve").is_err());
        let validator = jsonschema::validator_for(params).unwrap();
        assert!(validator.is_valid(&json!({"node": {"next": {"leaf": "ok"}}})));
        assert!(!validator.is_valid(&json!({})));
        assert!(!validator.is_valid(&json!({"node": {"leaf": 1}})));
        // Native schemas allow additional keys; admission owns the stricter top-level guard.
        assert!(validator.is_valid(&json!({"node": {}, "extra": true})));
        let approval = catalog
            .describe("server_request", "example/approve")
            .unwrap();
        let validator = jsonschema::validator_for(&approval["response_schema"]).unwrap();
        assert!(validator.is_valid(&json!({"decision": "allow"})));
        assert!(!validator.is_valid(&json!({"decision": "other"})));
    }

    #[test]
    fn invalid_native_references_fail_without_fetching_external_schemas() {
        for reference in ["#/definitions/Missing", "https://example.invalid/schema"] {
            assert!(reachable_schema(&json!({"$ref":reference}), &json!({})).is_err());
        }
        let command = schema_command(Path::new("configured-codex"), Path::new("output"));
        assert!(
            command
                .as_std()
                .get_envs()
                .any(|(key, value)| key == crate::config::TOKEN_ENV && value.is_none())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn generator_uses_configured_binary_and_cleans_up_success_failure_and_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let fixtures = directory.path().join("fixture");
        fs::create_dir(&fixtures).unwrap();
        fixture(&fixtures);
        let binary = directory.path().join("codex");
        let marker = directory.path().join("output-path");
        for (body, timeout, expected_error) in [
            (
                format!("cp -R '{}'/. \"$5\"", fixtures.display()),
                Duration::from_secs(5),
                None,
            ),
            ("exit 7".into(), Duration::from_secs(5), Some("exited")),
            (
                "exec sleep 60".into(),
                Duration::from_millis(100),
                Some("timed out"),
            ),
        ] {
            fs::write(&binary, format!("#!/bin/sh\nset -eu\n[ \"$1 $2 $3 $4\" = 'app-server generate-json-schema --experimental --out' ]\n[ -z \"${{ANCHOR_CODEX_BRIDGE_TOKEN+x}}\" ]\nprintf '%s' \"$5\" > '{}'\n{body}\n", marker.display())).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
            let result = if expected_error.is_none() {
                ProtocolCatalog::load(&binary).await
            } else {
                ProtocolCatalog::load_with_timeout(&binary, timeout).await
            };
            match expected_error {
                Some(expected) => assert!(result.unwrap_err().contains(expected)),
                None => assert!(result.unwrap().contains("example/read")),
            }
            let output = fs::read_to_string(&marker).unwrap();
            assert!(
                !Path::new(&output).exists(),
                "schema output was not cleaned up"
            );
        }
    }
}
