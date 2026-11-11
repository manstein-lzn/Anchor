//! Current Library facts shared by HTTP, Session and Pilot management reads.
//! A read never prepares dependencies, connects servers or grants execution.

use super::ApplicationError;
use anchor_graph_host::{PluginDefinition, PluginMetadata, SystemPlugin};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) const INVALID_DEFINITION: &str = "Plugin is unavailable or malformed";
pub(crate) const UNREADY_DEPENDENCIES: &str = "Plugin dependency view is missing or unusable";

#[derive(Debug, Clone)]
pub(crate) struct InstalledPlugin {
    pub(crate) id: String,
    pub(crate) metadata: Option<PluginMetadata>,
    pub(crate) dependencies_ready: Option<bool>,
}

impl InstalledPlugin {
    pub(crate) fn unavailable(id: String) -> Self {
        Self {
            id,
            metadata: None,
            dependencies_ready: None,
        }
    }

    pub(crate) fn available(&self) -> bool {
        self.metadata.is_some() && self.dependencies_ready == Some(true)
    }

    /// No private parser input, credential values or Host paths cross this boundary.
    pub(crate) fn project_health(&self, record: &mut Value) {
        // Reserved ownership survives a damaged installation, but never makes it executable.
        let system = self
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.system)
            .or_else(|| SystemPlugin::from_id(&self.id));
        record["kind"] = json!(if system.is_some() {
            "system"
        } else {
            "extension"
        });
        record["canUninstall"] = json!(system.is_none() && self.metadata.is_some());
        record["canReplace"] = json!(system.is_none() && self.metadata.is_some());
        record["updateSource"] = json!(if system.is_some() {
            "anchor-release"
        } else {
            "extension"
        });
        record["definitionValid"] = json!(self.metadata.is_some());
        record["dependenciesReady"] = json!(self.dependencies_ready);
        record["contextAuthorized"] = Value::Null;
        record["connectionVerified"] = Value::Null;
        record["available"] = json!(self.available());
        record["error"] = if self.metadata.is_none() {
            json!(INVALID_DEFINITION)
        } else if self.dependencies_ready != Some(true) {
            json!(UNREADY_DEPENDENCIES)
        } else {
            Value::Null
        };
    }
}

/// Holds a read lease through the adapter's projection and optional detail read.
pub(crate) struct InstalledPluginCatalog {
    root: PathBuf,
    pub(crate) entries: Vec<InstalledPlugin>,
    read: crate::plugin_library::ReadCatalog,
}

impl InstalledPluginCatalog {
    fn open(root: &Path) -> Result<Self, ApplicationError> {
        let read = crate::plugin_library::ReadCatalog::open(root).map_err(|_| {
            ApplicationError::Conflict("Plugin Library is changing; retry this read".into())
        })?;
        Ok(Self {
            root: root.into(),
            entries: Vec::new(),
            read,
        })
    }

    fn inspect(&self, id: String) -> InstalledPlugin {
        let metadata = self.read.catalog.metadata(&id).ok();
        let dependencies_ready = metadata.as_ref().map(|metadata| {
            crate::plugin_view::prepare_dependency_view(&self.root, &id, &metadata.dependencies)
                .is_ok()
        });
        InstalledPlugin {
            id,
            metadata,
            dependencies_ready,
        }
    }

    /// Selected detail reads do not scan unrelated installed assets.
    pub(crate) fn read_selected(root: &Path, id: &str) -> Result<Self, ApplicationError> {
        let mut catalog = Self::open(root)?;
        if catalog.read.exists() {
            catalog.entries.push(catalog.inspect(id.into()));
        }
        Ok(catalog)
    }

    pub(crate) fn read(root: &Path) -> Result<Self, ApplicationError> {
        let mut catalog = Self::open(root)?;
        if !catalog.read.exists() {
            return Ok(catalog);
        }
        let directories = match std::fs::read_dir(root.join("plugins")) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(catalog),
            Err(_) => {
                return Err(ApplicationError::Storage(
                    "cannot read Plugin catalog".into(),
                ));
            }
        };
        for directory in directories {
            let directory = directory
                .map_err(|_| ApplicationError::Storage("cannot read Plugin catalog".into()))?;
            let id = directory.file_name().to_string_lossy().into_owned();
            if id.starts_with('.') {
                continue;
            }
            catalog.entries.push(catalog.inspect(id));
        }
        catalog
            .entries
            .sort_by(|left, right| left.id.cmp(&right.id));
        Ok(catalog)
    }

    pub(crate) fn definition(&self, id: &str) -> Result<PluginDefinition, ApplicationError> {
        self.read
            .catalog
            .definition(id)
            .map_err(|_| ApplicationError::Invalid(INVALID_DEFINITION.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_distinguishes_definition_and_dependencies_without_claiming_authorization() {
        let root = tempfile::tempdir().unwrap();
        for (id, manifest) in [
            ("healthy", r#"{"name":"Healthy"}"#),
            (
                "needsview",
                r#"{"name":"Needs view","dependencies":{"python":["idna==3.7"]}}"#,
            ),
            ("broken", "{broken"),
        ] {
            let directory = root.path().join("plugins").join(id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("plugin.json"), manifest).unwrap();
        }
        let catalog = InstalledPluginCatalog::read(root.path()).unwrap();
        assert_eq!(
            catalog
                .entries
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["broken", "healthy", "needsview"]
        );
        for entry in &catalog.entries {
            let mut row = json!({});
            entry.project_health(&mut row);
            assert_eq!(row["definitionValid"], entry.id != "broken");
            assert_eq!(
                row["dependenciesReady"],
                match entry.id.as_str() {
                    "broken" => Value::Null,
                    "healthy" => json!(true),
                    _ => json!(false),
                }
            );
            assert_eq!(row["available"], entry.id == "healthy");
            assert!(row["contextAuthorized"].is_null());
            assert!(row["connectionVerified"].is_null());
            assert!(!row.to_string().contains(root.path().to_str().unwrap()));
        }
        assert!(!root.path().join("dependencies").exists());
    }

    #[test]
    fn catalog_projects_platform_lifecycle_even_when_reserved_resources_are_damaged() {
        let root = tempfile::tempdir().unwrap();
        crate::test_system_plugin(root.path(), SystemPlugin::GraphTasks);
        std::fs::create_dir_all(root.path().join("plugins/extension")).unwrap();
        std::fs::write(
            root.path().join("plugins/extension/plugin.json"),
            r#"{"name":"Extension"}"#,
        )
        .unwrap();
        let catalog = InstalledPluginCatalog::read(root.path()).unwrap();
        for entry in &catalog.entries {
            let mut row = json!({});
            entry.project_health(&mut row);
            let system = entry.id == "graph-tasks";
            assert_eq!(row["kind"], if system { "system" } else { "extension" });
            assert_eq!(row["canUninstall"], !system);
            assert_eq!(row["canReplace"], !system);
            assert_eq!(
                row["updateSource"],
                if system {
                    "anchor-release"
                } else {
                    "extension"
                }
            );
        }
        drop(catalog);
        std::fs::write(
            root.path().join("plugins/graph-tasks/plugin.json"),
            r#"{"name":"collision"}"#,
        )
        .unwrap();
        let catalog = InstalledPluginCatalog::read_selected(root.path(), "graph-tasks").unwrap();
        let mut row = json!({});
        catalog.entries[0].project_health(&mut row);
        assert_eq!(row["kind"], "system");
        assert_eq!(row["available"], false);
        assert_eq!(row["canUninstall"], false);
        assert_eq!(row["canReplace"], false);
        assert!(catalog.definition("graph-tasks").is_err());
    }
}
