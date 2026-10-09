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
    pub plugins: Vec<String>,
    #[serde(default)]
    pub max_rounds: Option<u32>,
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
