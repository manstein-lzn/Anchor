use crate::evidence::Entry;
use chrono::Utc;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Dependency {
    pub ecosystem: String,
    pub name: String,
    pub declarations: Vec<String>,
}

pub fn discover(manifests: &[Entry]) -> (Vec<Dependency>, Vec<Value>) {
    let mut dependencies = BTreeMap::<(String, String), Vec<String>>::new();
    let mut errors = Vec::new();
    for entry in manifests {
        let mut add = |ecosystem: &str, name: &str| {
            let name = name.trim();
            if !name.is_empty() {
                let values = dependencies
                    .entry((ecosystem.into(), name.into()))
                    .or_default();
                if !values.contains(&entry.path) {
                    values.push(entry.path.clone());
                }
            }
        };
        if entry.path.ends_with("package.json") {
            match serde_json::from_str::<Value>(&entry.content) {
                Ok(value) => {
                    for kind in [
                        "dependencies",
                        "devDependencies",
                        "peerDependencies",
                        "optionalDependencies",
                    ] {
                        if let Some(values) = value.get(kind).and_then(Value::as_object) {
                            for key in values.keys() {
                                add("npm", key);
                            }
                        }
                    }
                }
                Err(_) => {
                    errors.push(json!({"path":entry.path,"reason":"package manifest parse failed"}))
                }
            }
        } else {
            match entry.content.parse::<toml::Value>() {
                Ok(value) if entry.path.ends_with("Cargo.toml") => {
                    rust_dependencies(&value, &mut add);
                    if let Some(workspace) = value.get("workspace") { rust_dependencies(workspace,&mut add); }
                    if let Some(targets) = value.get("target").and_then(toml::Value::as_table) { for target in targets.values() { rust_dependencies(target,&mut add); } }
                }
                Ok(value) => {
                    let project = value.get("project");
                    if let Some(values) = project.and_then(|p|p.get("dependencies")).and_then(toml::Value::as_array) { python_dependencies(values,&mut add); }
                    if let Some(groups) = project.and_then(|p|p.get("optional-dependencies")).and_then(toml::Value::as_table) { for values in groups.values().filter_map(toml::Value::as_array) { python_dependencies(values,&mut add); } }
                    if let Some(build) = value.get("build-system").and_then(|p|p.get("requires")).and_then(toml::Value::as_array) { python_dependencies(build,&mut add); }
                }
                Err(_) => errors.push(json!({"path":entry.path,"reason":"TOML manifest parse failed; declarations unavailable"})),
            }
        }
    }
    (
        dependencies
            .into_iter()
            .map(|((ecosystem, name), declarations)| Dependency {
                ecosystem,
                name,
                declarations,
            })
            .collect(),
        errors,
    )
}

fn rust_dependencies(value: &toml::Value, add: &mut impl FnMut(&str, &str)) {
    for kind in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = value.get(kind).and_then(toml::Value::as_table) {
            for (name, declaration) in table {
                if declaration.get("path").is_some() && declaration.get("version").is_none() {
                    continue;
                }
                add(
                    "crates",
                    declaration
                        .get("package")
                        .and_then(toml::Value::as_str)
                        .unwrap_or(name),
                );
            }
        }
    }
}
fn python_dependencies(values: &[toml::Value], add: &mut impl FnMut(&str, &str)) {
    for value in values.iter().filter_map(toml::Value::as_str) {
        let name = value
            .split(|c: char| !c.is_ascii_alphanumeric() && !"-_.".contains(c))
            .next()
            .unwrap_or("");
        add("pypi", name);
    }
}

pub struct Ecosystem {
    client: Client,
}

impl Ecosystem {
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(Duration::from_secs(12))
    }
    pub fn with_timeout(timeout: Duration) -> Result<Self, String> {
        Ok(Self {
            client: Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .user_agent("Anchor-RSI/0.1 (public read-only research)")
                .build()
                .map_err(|e| e.to_string())?,
        })
    }
    pub async fn research(&self, targets: &[Dependency]) -> Value {
        let mut results = Vec::new();
        // Requests are independent; one registry failure never becomes a claim
        // that every dependency was checked successfully.
        for target in targets {
            let endpoint = endpoint(target);
            let result = match endpoint {
                Ok(endpoint) => {
                    let mut result = self.fetch(&endpoint).await;
                    if result["status"] == "ok" {
                        let payload = result["metadata"].take();
                        result["metadata"] = metadata(&target.ecosystem, &payload);
                        if let Some(repo) = repository(&payload) {
                            let url =
                                format!("https://api.github.com/repos/{repo}/releases?per_page=3");
                            let mut release = self.fetch(&url).await;
                            if release["status"] == "ok" {
                                release["metadata"] = json!(release["metadata"].as_array().map(|list|list.iter().take(3).map(|item|json!({"tag":item["tag_name"],"published_at":item["published_at"],"url":item["html_url"],"name":item["name"]})).collect::<Vec<_>>()));
                            }
                            result["github_releases"] = release;
                        }
                    }
                    result
                }
                Err(reason) => json!({"status":"error","error":reason}),
            };
            results.push(json!({"dependency":target,"evidence":result}));
        }
        json!({"retrieved_at":Utc::now(),"results":results,"limitations":["Public registry metadata is a change signal, not a recommendation or an installed-version assertion.","Requests are HTTPS allowlisted, redirects disabled, without deployment credentials or proxy environment.","Missing repositories and GitHub rate limits are not evidence of no recent changes."]})
    }

    pub async fn fetch(&self, endpoint: &str) -> Value {
        let started = Utc::now();
        let result = async {
            let url = Url::parse(endpoint).map_err(|_| "invalid public endpoint".to_owned())?;
            if url.scheme() != "https"
                || !matches!(
                    url.host_str(),
                    Some("crates.io" | "pypi.org" | "registry.npmjs.org" | "api.github.com")
                )
                || !url.username().is_empty()
                || url.password().is_some()
                || url.port().is_some_and(|port| port != 443)
                || url.fragment().is_some()
            {
                return Err("endpoint is outside public HTTPS allowlist".to_owned());
            }
            let mut response = self
                .client
                .get(url)
                .header("Accept", "application/json")
                .send()
                .await
                .map_err(|error| {
                    if error.is_timeout() {
                        "request timeout".to_owned()
                    } else {
                        "public request failed".to_owned()
                    }
                })?;
            let status = response.status();
            if !status.is_success() {
                return Err(format!("HTTP {}", status.as_u16()));
            }
            let mut body = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "response read failed".to_owned())?
            {
                if body.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                    return Err("response exceeds 4 MiB; incomplete evidence".into());
                }
                body.extend_from_slice(&chunk);
            }
            serde_json::from_slice::<Value>(&body).map_err(|_| "response is not valid JSON".into())
        }
        .await;
        match result {
            Ok(metadata) => {
                json!({"status":"ok","url":endpoint,"retrieved_at":started,"metadata":metadata})
            }
            Err(error) => {
                json!({"status":"error","url":safe_locator(endpoint),"retrieved_at":started,"error":error})
            }
        }
    }
}

fn safe_locator(endpoint: &str) -> String {
    if let Ok(mut url) = Url::parse(endpoint) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        return url.into();
    }
    "[invalid endpoint]".into()
}
fn endpoint(target: &Dependency) -> Result<String, String> {
    if target.name.is_empty()
        || !target
            .name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.@/".contains(&c))
        || target.name.contains("..")
    {
        return Err("invalid dependency name".into());
    }
    let mut url = Url::parse(match target.ecosystem.as_str() {
        "crates" => "https://crates.io/api/v1/crates/",
        "pypi" => "https://pypi.org/pypi/",
        "npm" => "https://registry.npmjs.org/",
        _ => return Err("unknown ecosystem".into()),
    })
    .unwrap();
    let mut path = url
        .path_segments_mut()
        .map_err(|_| "invalid registry URL")?;
    path.pop_if_empty().push(&target.name);
    if target.ecosystem == "pypi" {
        path.push("json");
    }
    drop(path);
    Ok(url.into())
}
fn repository(value: &Value) -> Option<String> {
    let url = value
        .pointer("/crate/repository")
        .and_then(Value::as_str)
        .or_else(|| value.pointer("/repository/url").and_then(Value::as_str))
        .or_else(|| {
            value
                .pointer("/info/project_urls/Source")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .pointer("/info/project_urls/Repository")
                .and_then(Value::as_str)
        })?;
    let url = Url::parse(url.strip_prefix("git+").unwrap_or(url)).ok()?;
    if url.host_str() != Some("github.com") {
        return None;
    }
    let repo = url.path().trim_matches('/').trim_end_matches(".git");
    if repo.split('/').count() != 2
        || !repo
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_./".contains(&c))
    {
        return None;
    }
    Some(repo.into())
}
fn metadata(ecosystem: &str, value: &Value) -> Value {
    match ecosystem {
        "crates" => {
            json!({"version":value["crate"]["newest_version"],"updated_at":value["crate"]["updated_at"],"repository":value["crate"]["repository"],"description":value["crate"]["description"],"recent_versions":value["versions"].as_array().map(|versions|versions.iter().take(3).map(|v|json!({"version":v["num"],"created_at":v["created_at"],"yanked":v["yanked"]})).collect::<Vec<_>>())})
        }
        "pypi" => {
            json!({"version":value["info"]["version"],"summary":value["info"]["summary"],"project_urls":value["info"]["project_urls"],"latest_files":value["urls"].as_array().map(|files|files.iter().take(3).map(|f|json!({"filename":f["filename"],"uploaded_at":f["upload_time_iso_8601"]})).collect::<Vec<_>>())})
        }
        "npm" => {
            json!({"dist_tags":value["dist-tags"],"modified":value["time"]["modified"],"repository":value["repository"],"description":value["description"]})
        }
        _ => Value::Null,
    }
}
