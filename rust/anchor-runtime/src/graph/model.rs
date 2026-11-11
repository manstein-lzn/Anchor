use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphSnapshot {
    pub objective: String,
    #[serde(default)]
    pub input: Value,
    pub entry: String,
    #[serde(default)]
    pub agents: BTreeMap<String, AgentDefinition>,
    #[serde(default)]
    pub ops: BTreeMap<String, Value>,
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    #[serde(default, rename = "_module_rounds")]
    pub module_rounds: BTreeMap<String, u32>,
}

/// A statically validated, non-nested region activated by one fanout node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParallelRegion {
    pub fanout: String,
    pub join: String,
    /// Ordered linear paths, excluding the fanout and join control nodes.
    pub branches: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDefinition {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub max_steps: Option<u64>,
    #[serde(default)]
    pub wall_time_limit_seconds: Option<f64>,
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub writes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphNode {
    pub id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub op: Option<String>,
    #[serde(default, rename = "with")]
    pub input: Option<Value>,
    #[serde(default)]
    pub plugins: Vec<PluginRef>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
}

/// One Plugin a node declares, and whether the Graph can run without it.
///
/// The Graph says what it needs; a deployment or a session may switch an
/// *optional* Plugin off, but never a required one. A bare string in an older
/// Graph means `required: true`, which is what "declared" always meant.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "PluginRefRepr")]
pub struct PluginRef {
    pub id: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    pub graph_tasks: Option<GraphTaskPolicy>,
    pub network: Option<NetworkMode>,
}

/// Node-owned task networking. A requirement never overrides an explicit denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Auto,
    Enabled,
    Denied,
}

/// Host-enforced delegation limits for the official graph-tasks Plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphTaskPolicy {
    pub graphs: Vec<String>,
    pub create_graphs: bool,
    pub plugins: Vec<String>,
    pub network: bool,
    pub commands: bool,
    pub develop_plugins: bool,
}

/// A reference as it is *written*: a bare id, or the full object.
///
/// Snapshots and Run records written before the object form carry `["id"]`; they
/// must keep loading, and a bare id has always meant "required".
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum PluginRefRepr {
    Id(String),
    Full {
        id: String,
        #[serde(default = "default_true")]
        required: bool,
        #[serde(default)]
        default: Option<bool>,
        #[serde(default)]
        graph_tasks: Option<GraphTaskPolicy>,
        #[serde(default)]
        network: Option<NetworkMode>,
    },
}

/// A `PluginRef` whose meaning is exactly the old bare id **writes** the bare id
/// back.
///
/// `GraphSnapshot::digest()` hashes the snapshot's serialization, so a record
/// written before the object form existed can only reproduce its own digest if a
/// declaration that says nothing beyond "required" serializes the way it was
/// written. `required: true` with no `default` is precisely the old form --
/// whether it arrived as a string or as `{"id":…,"required":true}` -- so both
/// collapse to `"id"` and old records keep validating. An optional reference, or
/// one that names a default or delegation policy, still writes the object.
impl Serialize for PluginRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.required
            && self.default.is_none()
            && self.graph_tasks.is_none()
            && self.network.is_none()
        {
            return serializer.serialize_str(&self.id);
        }
        use serde::ser::SerializeStruct;
        let fields = 2
            + usize::from(self.default.is_some())
            + usize::from(self.graph_tasks.is_some())
            + usize::from(self.network.is_some());
        let mut state = serializer.serialize_struct("PluginRef", fields)?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("required", &self.required)?;
        if let Some(default) = self.default {
            state.serialize_field("default", &default)?;
        }
        if let Some(policy) = &self.graph_tasks {
            state.serialize_field("graph_tasks", policy)?;
        }
        if let Some(mode) = &self.network {
            state.serialize_field("network", mode)?;
        }
        state.end()
    }
}

impl TryFrom<PluginRefRepr> for PluginRef {
    type Error = GraphError;

    fn try_from(repr: PluginRefRepr) -> Result<Self, Self::Error> {
        let reference = match repr {
            PluginRefRepr::Id(id) => Self::required(id),
            PluginRefRepr::Full {
                id,
                required,
                default,
                graph_tasks,
                network,
            } => Self {
                id,
                required,
                default,
                graph_tasks,
                network,
            },
        };
        reference.validate_graph_tasks()?;
        Ok(reference)
    }
}

fn default_true() -> bool {
    true
}

impl PluginRef {
    pub fn required(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            required: true,
            default: None,
            graph_tasks: None,
            network: None,
        }
    }

    pub(crate) fn validate_graph_tasks(&self) -> Result<(), GraphError> {
        if self.network.is_some() && self.id != "network" {
            return Err(GraphError::InvalidSnapshot(
                "network policy is only valid on the network Plugin reference".into(),
            ));
        }
        let Some(policy) = &self.graph_tasks else {
            return Ok(());
        };
        if self.id != "graph-tasks" {
            return Err(GraphError::InvalidSnapshot(
                "graph_tasks is only valid on the graph-tasks Plugin reference".into(),
            ));
        }
        for graph in &policy.graphs {
            if !crate::identifier::is_name(graph) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "graph_tasks contains an invalid Graph name: {graph}"
                )));
            }
        }
        for plugin in &policy.plugins {
            if !crate::identifier::is_plugin_id(plugin) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "graph_tasks contains an invalid Plugin id: {plugin}"
                )));
            }
        }
        Ok(())
    }
}

impl GraphSnapshot {
    /// Old snapshots retain their exact bytes and effective ceiling. Remove the
    /// legacy fallback only after all retained pre-policy Runs have retired.
    pub fn node_network_mode(&self, node: &GraphNode) -> NetworkMode {
        if let Some(mode) = node
            .plugins
            .iter()
            .find(|plugin| plugin.id == "network")
            .and_then(|plugin| plugin.network)
        {
            return mode;
        }
        let legacy = node
            .agent
            .as_ref()
            .and_then(|name| self.agents.get(name))
            .is_some_and(|agent| agent.network)
            || node
                .op
                .as_ref()
                .and_then(|name| self.ops.get(name))
                .and_then(|op| op.get("network"))
                .and_then(Value::as_bool)
                == Some(true);
        if legacy {
            NetworkMode::Enabled
        } else {
            NetworkMode::Denied
        }
    }
}

impl From<&str> for PluginRef {
    fn from(id: &str) -> Self {
        Self::required(id)
    }
}

impl From<String> for PluginRef {
    fn from(id: String) -> Self {
        Self::required(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Agent,
    OpRun,
    OpHost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEdge {
    #[serde(rename = "from")]
    pub from_node: String,
    #[serde(rename = "to")]
    pub to_node: String,
}
