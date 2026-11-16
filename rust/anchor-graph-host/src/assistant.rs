//! Persistent-assistant shape and its finite, capability-preserving quality probe.
use anchor_runtime::graph::GraphSnapshot;
use serde_json::{Value, json};

pub const WAIT_INPUT: &str = "session.wait_input";
pub const REPLY: &str = "session.reply";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantPlan {
    pub wait_node: String,
    pub work_node: String,
    pub reply_node: String,
}

impl AssistantPlan {
    /// The plan of a persistent assistant Graph, whose only Agent is the work
    /// node. `None` when the Graph is not an assistant at all.
    pub fn discover(snapshot: &GraphSnapshot) -> Result<Option<Self>, String> {
        let mut agents = snapshot.nodes.iter().filter(|node| node.agent.is_some());
        match (agents.next(), agents.next()) {
            (Some(agent), None) => Self::from_snapshot(snapshot, &agent.id),
            _ => Self::from_snapshot(snapshot, ""),
        }
    }

    pub fn from_snapshot(
        snapshot: &GraphSnapshot,
        work_node: &str,
    ) -> Result<Option<Self>, String> {
        let operations = snapshot
            .nodes
            .iter()
            .filter_map(|node| {
                let operation = node
                    .op
                    .as_ref()
                    .and_then(|name| snapshot.ops.get(name))?
                    .get("host")?
                    .get("operation")?
                    .as_str()?;
                Some((node.id.as_str(), operation))
            })
            .collect::<Vec<_>>();
        if operations.is_empty() {
            return Ok(None);
        }
        let waits = operations
            .iter()
            .filter(|(_, operation)| *operation == WAIT_INPUT)
            .collect::<Vec<_>>();
        let replies = operations
            .iter()
            .filter(|(_, operation)| *operation == REPLY)
            .collect::<Vec<_>>();
        if waits.len() != 1
            || replies.len() != 1
            || operations.len() != 2
            || snapshot.nodes.len() != 3
        {
            return Err(
                "a channel assistant requires one input Op, one Agent and one reply Op".into(),
            );
        }
        let plan = Self {
            wait_node: waits[0].0.to_owned(),
            work_node: work_node.to_owned(),
            reply_node: replies[0].0.to_owned(),
        };
        if snapshot.entry != plan.wait_node
            || !snapshot
                .nodes
                .iter()
                .any(|node| node.id == work_node && node.agent.is_some())
        {
            return Err("assistant entry must wait for input and the configured result node must be an Agent".into());
        }
        let expected = [
            (&plan.wait_node, &plan.work_node),
            (&plan.work_node, &plan.reply_node),
            (&plan.reply_node, &plan.wait_node),
        ];
        if snapshot.edges.len() != 3
            || expected.iter().any(|(from, to)| {
                !snapshot
                    .edges
                    .iter()
                    .any(|edge| &edge.from_node == *from && &edge.to_node == *to)
            })
        {
            return Err("assistant nodes must form the input, Agent, reply loop".into());
        }
        if snapshot.nodes.iter().any(|node| node.max_rounds.is_some())
            || !snapshot.module_rounds.is_empty()
        {
            return Err("a persistent assistant cannot have a finite round ceiling".into());
        }
        Ok(Some(plan))
    }
}

/// Evaluate one work Turn without granting Session input/reply operations.
/// Production still publishes the original full assistant definition.
pub fn assistant_probe_definition(definition: &Value) -> Result<Value, String> {
    let original =
        GraphSnapshot::from_authoring(definition.clone()).map_err(|error| error.to_string())?;
    if !original
        .ops
        .values()
        .any(|op| matches!(op["host"]["operation"].as_str(), Some(WAIT_INPUT | REPLY)))
    {
        return Ok(definition.clone());
    }
    let Some(plan) = AssistantPlan::discover(&original)? else {
        return Ok(definition.clone());
    };
    let work = definition["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|node| node["id"] == plan.work_node))
        .ok_or("assistant probes require a top-level work node")?;
    let mut probe = definition.clone();
    probe["entry"] = json!(plan.work_node);
    probe["nodes"] = json!([work]);
    probe["edges"] = json!([]);
    probe["ops"] = json!({});
    for field in ["graphs", "max_rounds", "_module_rounds", "layout"] {
        probe
            .as_object_mut()
            .expect("compiled Graph object")
            .remove(field);
    }
    let compiled =
        GraphSnapshot::from_authoring(probe.clone()).map_err(|error| error.to_string())?;
    if compiled.agents != original.agents
        || compiled.nodes.len() != 1
        || compiled.nodes[0]
            != *original
                .nodes
                .iter()
                .find(|node| node.id == plan.work_node)
                .expect("validated assistant work")
    {
        return Err("assistant probe changed the work capability or Agent configuration".into());
    }
    Ok(probe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_probe_preserves_the_exact_work_turn_without_session_authority() {
        let original = json!({
            "objective":"Preserve the requested assistant task", "input":{"task":"fixture"},
            "entry":"wait", "agents":{"worker":{"model":"models.worker",
                "instructions":"Use only delivered evidence", "reads":[], "writes":["result.json"]}},
            "ops":{"wait":{"host":{"operation":WAIT_INPUT}},"reply":{"host":{"operation":REPLY}}},
            "nodes":[{"id":"wait","op":"wait"},{"id":"work","agent":"worker",
                "with":"fixture", "plugins":[{"id":"example","revision":"a".repeat(64)}]},
                {"id":"reply","op":"reply"}],
            "edges":[{"from":"wait","to":"work"},{"from":"work","to":"reply"},{"from":"reply","to":"wait"}]
        });
        let probe = assistant_probe_definition(&original).unwrap();
        let compiled = GraphSnapshot::from_authoring(probe.clone()).unwrap();
        assert_eq!(probe["objective"], original["objective"]);
        assert_eq!(probe["input"], original["input"]);
        assert_eq!(probe["agents"], original["agents"]);
        assert_eq!(probe["nodes"], json!([original["nodes"][1]]));
        assert_eq!(compiled.entry, "work");
        assert!(compiled.ops.is_empty() && compiled.edges.is_empty());
        assert_eq!(assistant_probe_definition(&probe).unwrap(), probe);
    }
}
