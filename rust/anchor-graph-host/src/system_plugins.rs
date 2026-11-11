//! Platform-owned capability identities and their public release resources.
//! Installed identity is verified by the catalog, never asserted by a manifest.

use crate::resources::is_executable;
use crate::{invalid, resources::inside_file};
use anchor_runtime::graph::GraphError;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPlugin {
    Network,
    GraphTasks,
    Feishu,
    Wecom,
}

pub const fn all_system_plugins() -> &'static [SystemPlugin] {
    &[
        SystemPlugin::Network,
        SystemPlugin::GraphTasks,
        SystemPlugin::Feishu,
        SystemPlugin::Wecom,
    ]
}

impl SystemPlugin {
    /// A reserved name is only a candidate identity; the catalog verifies its resources.
    pub fn from_id(id: &str) -> Option<Self> {
        all_system_plugins()
            .iter()
            .copied()
            .find(|plugin| plugin.id() == id)
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Network => "network",
            Self::GraphTasks => "graph-tasks",
            Self::Feishu => "feishu",
            Self::Wecom => "wecom",
        }
    }

    /// Public resource paths are relative to the Plugin directory.
    pub const fn public_files(self) -> &'static [(&'static str, &'static [u8])] {
        match self {
            Self::Network => &[
                (
                    "plugin.json",
                    include_bytes!("../../../plugins/network/plugin.json"),
                ),
                (
                    "skills/network/SKILL.md",
                    include_bytes!("../../../plugins/network/skills/network/SKILL.md"),
                ),
            ],
            Self::GraphTasks => &[
                (
                    "plugin.json",
                    include_bytes!("../../../plugins/graph-tasks/plugin.json"),
                ),
                (
                    "skills/graph-tasks/SKILL.md",
                    include_bytes!("../../../plugins/graph-tasks/skills/graph-tasks/SKILL.md"),
                ),
            ],
            Self::Feishu => &[
                (
                    "plugin.json",
                    include_bytes!("../../../plugins/feishu/plugin.json"),
                ),
                (
                    "channel.json",
                    include_bytes!("../../../plugins/feishu/channel.json"),
                ),
                (
                    "skills/feishu/SKILL.md",
                    include_bytes!("../../../plugins/feishu/skills/feishu/SKILL.md"),
                ),
            ],
            Self::Wecom => &[
                (
                    "plugin.json",
                    include_bytes!("../../../plugins/wecom/plugin.json"),
                ),
                (
                    "channel.json",
                    include_bytes!("../../../plugins/wecom/channel.json"),
                ),
                (
                    "skills/wecom/SKILL.md",
                    include_bytes!("../../../plugins/wecom/skills/wecom/SKILL.md"),
                ),
            ],
        }
    }

    /// Executable bytes are bound by the release's admitted full Plugin digest.
    pub const fn executables(self) -> &'static [&'static str] {
        match self {
            Self::Network => &["bin/anchor-network-tools"],
            Self::GraphTasks => &[],
            Self::Feishu => &["bin/anchor-feishu-gateway"],
            Self::Wecom => &["bin/anchor-wecom-tools", "bin/anchor-wecom-gateway"],
        }
    }

    pub(crate) fn verify(
        self,
        directory: &Path,
        resources: &[String],
        manifest_bytes: &[u8],
    ) -> Result<(), GraphError> {
        let mut expected = self
            .public_files()
            .iter()
            .map(|(path, _)| *path)
            .chain(self.executables().iter().copied())
            .collect::<Vec<_>>();
        expected.sort_unstable();
        if resources.iter().map(String::as_str).ne(expected) {
            return Err(invalid(format!(
                "System Plugin {}: resource inventory differs from this Anchor release",
                self.id()
            )));
        }
        for (path, bytes) in self.public_files() {
            let matches = if *path == "plugin.json" {
                manifest_bytes == *bytes
            } else {
                std::fs::read(inside_file(directory, path).map_err(invalid)?)? == *bytes
            };
            if !matches {
                return Err(invalid(format!(
                    "System Plugin {}: public resource differs from this Anchor release: {path}",
                    self.id()
                )));
            }
        }
        for path in self.executables() {
            if !is_executable(&inside_file(directory, path).map_err(invalid)?) {
                return Err(invalid(format!(
                    "System Plugin {}: executable is unavailable: {path}",
                    self.id()
                )));
            }
        }
        Ok(())
    }
}
