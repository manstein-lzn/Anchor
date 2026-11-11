//! How complete an impact scan of the Graph catalog was.
//!
//! The scan compiles the Graphs a run can come from and reports what it could not
//! read, so an "nobody uses this" answer is only given when it really saw
//! everything.

use super::super::{ApplicationError, RunApplication};
use anchor_graph_host::LoadedGraphBundle;
use serde::{Deserialize, Serialize};

/// How complete one derived impact report is.
///
/// Shared by every impact scan, not just the environment one: an "nobody uses
/// this" answer may only be given when the scan really saw everything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ImpactCoverage {
    Complete,
    /// The scan stopped early or could not read some definitions.
    Partial {
        /// How many Graphs this report considered before its bound.
        considered: usize,
        /// How many Graphs a run could come from.
        total: usize,
        missing: Vec<ImpactGap>,
    },
    /// The report could not be produced at all.
    Unknown {
        reason: String,
    },
}

/// One Graph an impact report could not read, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImpactGap {
    pub(crate) graph: String,
    pub(crate) reason: String,
}

/// The scan bound for a derived impact report.
const ENVIRONMENT_REFERENCE_LIMIT: usize = 200;

/// What an impact scan could see.
pub(crate) enum ImpactScan {
    Read {
        bundles: Vec<(String, LoadedGraphBundle)>,
        considered: usize,
        total: usize,
        missing: Vec<ImpactGap>,
    },
    /// The report could not be produced at all.
    Unreadable(String),
}

/// How complete a scan was, in the shape every report and every stored record
/// already uses.
pub(crate) fn coverage_of(
    considered: usize,
    total: usize,
    missing: Vec<ImpactGap>,
) -> ImpactCoverage {
    if missing.is_empty() && total <= ENVIRONMENT_REFERENCE_LIMIT {
        ImpactCoverage::Complete
    } else {
        ImpactCoverage::Partial {
            considered,
            total,
            missing,
        }
    }
}

impl RunApplication {
    pub(crate) fn impact_scan(&self) -> ImpactScan {
        let mut candidates: Vec<String> = Vec::new();
        if self.catalog_root.is_dir() {
            let entries = match std::fs::read_dir(&self.catalog_root) {
                Ok(entries) => entries,
                Err(error) => {
                    // An unreadable catalog is not "nobody uses this".
                    return ImpactScan::Unreadable(format!(
                        "{} could not be read: {error}",
                        self.catalog_root.display()
                    ));
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join("graph.json").is_file() {
                    candidates.push(entry.file_name().to_string_lossy().into_owned());
                }
            }
        } else if self.configured_graph.is_none() {
            return ImpactScan::Unreadable(format!(
                "{} is not a readable Graph catalog",
                self.catalog_root.display()
            ));
        }
        if let Some((configured, _)) = &self.configured_graph {
            candidates.push(configured.clone());
        }
        candidates.sort();
        // The configured bundle may also be a child of the catalog; scanning it
        // twice would double-count its declarations. A name whose definition is
        // gone is not a Graph a run can come from — it has no nodes, so it
        // neither declares an environment nor calls anything — so only a Graph
        // that is still *present* and unreadable is a gap.
        let mut seen_paths = std::collections::BTreeSet::new();
        candidates.retain(|graph| match self.graph_path_checked(graph) {
            Ok(path) => path.exists() && seen_paths.insert(path.canonicalize().unwrap_or(path)),
            Err(_) => true,
        });
        let total = candidates.len();
        let considered = total.min(ENVIRONMENT_REFERENCE_LIMIT);
        candidates.truncate(ENVIRONMENT_REFERENCE_LIMIT);

        let mut bundles = Vec::new();
        let mut missing = Vec::new();
        for graph in candidates {
            match self.load_graph(&graph) {
                Ok((_, bundle)) => bundles.push((graph, bundle)),
                Err(error) => {
                    // A Graph this host cannot load is one it cannot report on,
                    // and the report says which one and why.
                    missing.push(ImpactGap {
                        graph,
                        reason: error.to_string(),
                    });
                }
            }
        }
        ImpactScan::Read {
            bundles,
            considered,
            total,
            missing,
        }
    }

    /// The current bindings of the Plugins one bundle references.
    ///
    /// A bundle carries ids only; the code and the views live in the Library, so
    /// this is where every consumer resolves what a Run will actually use.
    pub(crate) fn bundle_plugin_bindings(
        &self,
        bundle: &anchor_graph_host::LoadedGraphBundle,
    ) -> Result<
        std::collections::BTreeMap<String, anchor_runtime::graph::PluginBinding>,
        ApplicationError,
    > {
        use anchor_graph_host::PluginCatalog;
        Ok(crate::plugin_library::Catalog::new(self.library_root())
            .resolve_snapshot(&bundle.snapshot)?
            .into_iter()
            .map(|binding| (binding.id.clone(), binding))
            .collect())
    }
}
