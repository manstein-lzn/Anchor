use super::{Evidence, MAX_FILE, safe_path};
use anchor_graph_host::{FilePluginCatalog, PluginDefinition, SystemPlugin, all_system_plugins};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, io, path::Path};

// Catalog hashing reads executable bytes; bound that read separately from the
// 8 MiB text-body limit. Native bytes never enter a frozen evidence file.
const MAX_NATIVE_HASH_BYTES: u64 = 256 * 1024 * 1024;

impl Evidence {
    pub(super) fn system_capabilities(&mut self, library: &Path) -> Result<(), String> {
        let shared = [
            ("contract", "docs/plugins.md"),
            ("contract", "docs/architecture.md"),
            ("identity", "rust/anchor-graph-host/src/system_plugins.rs"),
            ("lifecycle", "rust/anchor-library/src/lib.rs"),
            ("regression", "rust/anchor-library/tests/system_plugins.rs"),
        ]
        .map(|(role, path)| self.system_source_anchor(role, path));
        let mut plugins = Vec::new();
        for plugin in all_system_plugins() {
            let installed = match installed_definition(library, *plugin) {
                Ok(definition) => json!({
                    "status":"verified",
                    "identity_basis":"shared catalog public bytes and complete resource inventory",
                    "code_digest":definition.digest,
                    "authorization_digest":definition.authorization_digest,
                    "resources":definition.resources,
                    "native_provenance":"unknown",
                }),
                Err((status, reason)) => {
                    self.issue(&format!("plugins/plugins/{}", plugin.id()), reason);
                    json!({
                        "status":status, "reason":reason,
                        "code_digest":null, "authorization_digest":null,
                        "native_provenance":"unknown",
                    })
                }
            };
            let public_resources = plugin
                .public_files()
                .iter()
                .map(|(relative, bytes)| {
                    let compiled_sha256 = format!("{:x}", Sha256::digest(bytes));
                    let source_path = format!("plugins/{}/{relative}", plugin.id());
                    let source_matches_compiled = self
                        .entries
                        .get(&format!("code/{source_path}"))
                        .map(|entry| entry.original_sha256 == compiled_sha256);
                    json!({
                        "path":relative,
                        "compiled_sha256":compiled_sha256,
                        "source_matches_compiled":source_matches_compiled,
                        "source":self.system_source_anchor("public_resource", &source_path),
                    })
                })
                .collect::<Vec<_>>();
            let anchors = source_anchors(*plugin)
                .iter()
                .map(|(role, path)| self.system_source_anchor(role, path))
                .collect::<Vec<_>>();
            plugins.push(json!({
                "id":plugin.id(), "kind":"system",
                "declared_library_source":library.join("plugins").join(plugin.id()),
                "installed":installed,
                "public_resources":public_resources,
                "native_entrypoints":plugin.executables(),
                "source_anchors":anchors,
            }));
        }
        let projection = json!({
            "format":1, "captured_at":self.captured_at,
            "collector_anchor_version":env!("CARGO_PKG_VERSION"),
            "declared_library_source":library,
            "shared_source_anchors":shared, "plugins":plugins,
            "limitations":[
                "Only the configured data root's current Library is inspected; custom Library roots and the running Host revision are not inferred.",
                "Verified means the current shared catalog accepted the public bytes and resource inventory. It does not attest native executable provenance or release compatibility.",
                "Code and authorization digests describe the current read, not an immutable Run/Turn authorization snapshot or restored historical implementation bytes.",
                "Source anchors identify already frozen files within the source grant; collection does not prove the tests passed or that this source built the deployed executable.",
                "Native entrypoints are hashed only through the shared catalog, never copied; entrypoints above 256 MiB are unavailable.",
            ],
        });
        // Every field is allowlisted metadata. In particular, authority digests
        // are public identities, not credential values for generic redaction.
        let content =
            serde_json::to_string_pretty(&projection).map_err(|error| error.to_string())?;
        self.add(
            "plugins/system-capabilities.json",
            "plugins",
            "shared system inventory and configured current Library catalog",
            &content,
            &content,
        )
    }

    fn system_source_anchor(&mut self, role: &str, path: &str) -> Value {
        let evidence_path = format!("code/{path}");
        match self.entries.get(&evidence_path) {
            Some(entry) => json!({
                "role":role, "source_relative":path, "status":"collected",
                "evidence_path":entry.path, "sha256":entry.sha256,
                "original_sha256":entry.original_sha256,
            }),
            None => {
                self.issue(
                    &evidence_path,
                    "system Plugin source anchor not collected within the granted source",
                );
                json!({"role":role,"source_relative":path,"status":"not_collected","evidence_path":null})
            }
        }
    }
}

fn installed_definition(
    library: &Path,
    plugin: SystemPlugin,
) -> Result<PluginDefinition, (&'static str, &'static str)> {
    let directory = library.join("plugins").join(plugin.id());
    safe_path(&directory).map_err(|_| ("invalid", "system Plugin has an unsafe resource path"))?;
    match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err((
                "missing",
                "system Plugin is absent from the configured Library",
            ));
        }
        Err(_) => return Err(("invalid", "system Plugin directory is unreadable")),
        Ok(_) => {}
    }
    for (relative, limit) in plugin
        .public_files()
        .iter()
        .map(|(path, _)| (*path, MAX_FILE))
        .chain(
            plugin
                .executables()
                .iter()
                .map(|path| (*path, MAX_NATIVE_HASH_BYTES)),
        )
    {
        let path = directory.join(relative);
        safe_path(&path).map_err(|_| ("invalid", "system Plugin has an unsafe resource path"))?;
        let metadata = fs::symlink_metadata(path).map_err(|_| {
            (
                "invalid",
                "system Plugin required resource is missing or unreadable",
            )
        })?;
        if !metadata.is_file() {
            return Err(("invalid", "system Plugin resource is not an ordinary file"));
        }
        if metadata.len() > limit {
            return Err((
                "unavailable",
                "system Plugin identity exceeds collection read bounds",
            ));
        }
    }
    FilePluginCatalog::new(library)
        .definition(plugin.id())
        .map_err(|_| {
            (
                "invalid",
                "shared catalog could not verify the current system Plugin identity",
            )
        })
}

fn source_anchors(plugin: SystemPlugin) -> &'static [(&'static str, &'static str)] {
    match plugin {
        SystemPlugin::Network => &[
            (
                "implementation",
                "rust/anchor-runner-host/src/plugin_network.rs",
            ),
            ("implementation", "rust/anchor-network-tools/src/lib.rs"),
            (
                "regression",
                "rust/anchor-runner-host/src/node_host/resolver_tests/turn_plugins.rs",
            ),
        ],
        SystemPlugin::GraphTasks => &[
            (
                "implementation",
                "rust/anchor-runner-host/src/graph_task_tools.rs",
            ),
            (
                "implementation",
                "rust/anchor-runner-host/src/application/graph_tasks.rs",
            ),
            (
                "regression",
                "rust/anchor-runner-host/src/application/graph_tasks/tests.rs",
            ),
        ],
        SystemPlugin::Feishu => &[
            ("contract", "docs/feishu-assistant.md"),
            (
                "implementation",
                "rust/anchor-runner-host/src/channel_tools/feishu.rs",
            ),
            (
                "implementation",
                "rust/anchor-feishu-gateway/src/gateway.rs",
            ),
            (
                "regression",
                "rust/anchor-runner-host/tests/goose_feishu.rs",
            ),
        ],
        SystemPlugin::Wecom => &[
            ("contract", "docs/wecom-assistant.md"),
            (
                "implementation",
                "rust/anchor-runner-host/src/channel_tools.rs",
            ),
            ("implementation", "rust/anchor-wecom-gateway/src/gateway.rs"),
            ("implementation", "rust/anchor-wecom-tools/src/lib.rs"),
            (
                "regression",
                "rust/anchor-runner-host/tests/goose_channel.rs",
            ),
        ],
    }
}
