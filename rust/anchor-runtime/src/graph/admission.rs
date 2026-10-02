use super::*;
use serde_json::Value;
use sha2::Digest;

impl GraphSnapshot {
    pub fn admit(value: Value) -> Result<Self, GraphError> {
        if let Value::Object(fields) = &value {
            let allowed = [
                "objective",
                "input",
                "entry",
                "agents",
                "ops",
                "nodes",
                "edges",
                "_module_rounds",
            ];
            if let Some(unknown) = fields.keys().find(|key| !allowed.contains(&key.as_str())) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "unknown graph snapshot field `{unknown}`"
                )));
            }
        }
        let snapshot: Self = serde_json::from_value(value).map_err(GraphError::SnapshotDecode)?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), GraphError> {
        if self.nodes.is_empty() {
            return Err(GraphError::InvalidSnapshot("graph has no nodes".into()));
        }
        let mut ids = BTreeSet::new();
        for node in &self.nodes {
            validate_node_id(&node.id)?;
            if !ids.insert(node.id.as_str()) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "duplicate node id `{}`",
                    node.id
                )));
            }
            if node.max_rounds == Some(0) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "node `{}` max_rounds must be positive",
                    node.id
                )));
            }
            if node.agent.is_some() == node.op.is_some() {
                return Err(GraphError::InvalidSnapshot(format!(
                    "node `{}` must have exactly one of agent/op",
                    node.id
                )));
            }
            if !node.plugins.is_empty() {
                return Err(GraphError::Unsupported(format!(
                    "node `{}` declares plugins, unsupported in R5",
                    node.id
                )));
            }
            if let Some(op_name) = &node.op {
                let op = self.ops.get(op_name).ok_or_else(|| {
                    GraphError::InvalidSnapshot(format!(
                        "node `{}` references missing op `{op_name}`",
                        node.id
                    ))
                })?;
                if ["run", "call", "fanout", "join"]
                    .iter()
                    .filter(|key| op.get(**key).is_some())
                    .count()
                    != 1
                {
                    return Err(GraphError::InvalidSnapshot(format!(
                        "op `{op_name}` must declare exactly one execution operation"
                    )));
                }
                if op.get("call").is_some() {
                    return Err(GraphError::Unsupported(format!(
                        "op.call `{op_name}` is reserved for R7"
                    )));
                }
                if op.get("run").is_none() && op.get("fanout").is_none() && op.get("join").is_none()
                {
                    return Err(GraphError::Unsupported(format!(
                        "op `{op_name}` has no supported R5 execution capability"
                    )));
                }
            }
            if let Some(agent_name) = &node.agent {
                let agent = self.agents.get(agent_name).ok_or_else(|| {
                    GraphError::InvalidSnapshot(format!(
                        "node `{}` references missing agent `{agent_name}`",
                        node.id
                    ))
                })?;
                validate_wall_time(agent.wall_time_limit_seconds, &node.id)?;
            }
            if let Some(op_name) = &node.op {
                let op = &self.ops[op_name];
                if op.get("network").is_some_and(|value| !value.is_boolean()) {
                    return Err(GraphError::InvalidSnapshot(format!(
                        "op `{op_name}` network must be a boolean"
                    )));
                }
                if op
                    .get("wall_time_limit_seconds")
                    .is_some_and(|value| value.as_f64().is_none())
                {
                    return Err(GraphError::InvalidSnapshot(format!(
                        "op `{op_name}` wall_time_limit_seconds must be a number"
                    )));
                }
                validate_wall_time(
                    op.get("wall_time_limit_seconds").and_then(Value::as_f64),
                    &node.id,
                )?;
            }
        }
        if !ids.contains(self.entry.as_str()) {
            return Err(GraphError::InvalidSnapshot(format!(
                "entry `{}` does not name a node",
                self.entry
            )));
        }
        let mut edge_set = BTreeSet::new();
        for edge in &self.edges {
            if !edge_set.insert((edge.from_node.as_str(), edge.to_node.as_str())) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "duplicate edge {} -> {}",
                    edge.from_node, edge.to_node
                )));
            }
            if !ids.contains(edge.from_node.as_str()) || !ids.contains(edge.to_node.as_str()) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "edge {} -> {} references unknown node",
                    edge.from_node, edge.to_node
                )));
            }
        }
        for (scope, limit) in &self.module_rounds {
            if *limit == 0 || !ids.iter().any(|id| id.starts_with(&format!("{scope}/"))) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "invalid module-round scope or ceiling `{scope}`"
                )));
            }
        }
        self.parallel_regions()?;
        Ok(())
    }

    /// Validate and derive explicit fanout/join regions from the expanded snapshot.
    pub fn parallel_regions(&self) -> Result<BTreeMap<String, ParallelRegion>, GraphError> {
        let nodes = self
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect::<BTreeMap<_, _>>();
        let mut fanouts = BTreeMap::<String, String>::new();
        let mut joins = BTreeSet::<String>::new();
        for node in &self.nodes {
            let Some(op_name) = &node.op else { continue };
            let op = &self.ops[op_name];
            match (op.get("fanout"), op.get("join")) {
                (Some(spec), None) => {
                    let join = spec
                        .as_object()
                        .filter(|object| object.len() == 1)
                        .and_then(|object| object.get("join"))
                        .and_then(Value::as_str)
                        .filter(|join| !join.is_empty())
                        .ok_or_else(|| {
                            GraphError::InvalidSnapshot(format!(
                                "fanout op `{op_name}` must declare exactly one non-empty join id"
                            ))
                        })?;
                    fanouts.insert(node.id.clone(), join.to_owned());
                }
                (None, Some(spec)) => {
                    if !spec.as_object().is_some_and(serde_json::Map::is_empty) {
                        return Err(GraphError::InvalidSnapshot(format!(
                            "join op `{op_name}` must be an empty object"
                        )));
                    }
                    joins.insert(node.id.clone());
                }
                (Some(_), Some(_)) => {
                    return Err(GraphError::InvalidSnapshot(format!(
                        "op `{op_name}` cannot be both fanout and join"
                    )));
                }
                (None, None) => {}
            }
        }

        let mut paired_joins = BTreeSet::new();
        let mut regions = BTreeMap::new();
        let mut owned = BTreeSet::<String>::new();
        for (fanout, join) in &fanouts {
            if !joins.contains(join) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "fanout `{fanout}` references `{join}`, which is not a join node"
                )));
            }
            if !paired_joins.insert(join.clone()) {
                return Err(GraphError::InvalidSnapshot(format!(
                    "join `{join}` is paired with multiple fanouts"
                )));
            }
            let starts = self.outgoing(fanout);
            if starts.len() < 2 {
                return Err(GraphError::InvalidSnapshot(format!(
                    "fanout `{fanout}` needs at least two outgoing branches"
                )));
            }
            if self.outgoing(join).len() > 1 {
                return Err(GraphError::InvalidSnapshot(format!(
                    "join `{join}` cannot choose between multiple outgoing edges"
                )));
            }
            let mut branch_paths = Vec::new();
            let mut tails = BTreeSet::new();
            for start in starts {
                let mut path = Vec::new();
                let mut current = start;
                loop {
                    if current == join {
                        if path.is_empty() {
                            return Err(GraphError::InvalidSnapshot(format!(
                                "fanout `{fanout}` has an empty branch"
                            )));
                        }
                        break;
                    }
                    if !nodes.contains_key(current) || !owned.insert(current.to_owned()) {
                        return Err(GraphError::InvalidSnapshot(format!(
                            "fanout `{fanout}` has overlapping or invalid branch node `{current}`"
                        )));
                    }
                    let branch_node = nodes[current];
                    if branch_node.op.as_ref().is_some_and(|name| {
                        self.ops[name].get("fanout").is_some()
                            || self.ops[name].get("join").is_some()
                    }) {
                        return Err(GraphError::InvalidSnapshot(format!(
                            "fanout `{fanout}` cannot contain nested control node `{current}`"
                        )));
                    }
                    let incoming = self
                        .edges
                        .iter()
                        .filter(|edge| edge.to_node == current)
                        .map(|edge| edge.from_node.as_str())
                        .collect::<Vec<_>>();
                    let expected_source = path.last().map(String::as_str).unwrap_or(fanout);
                    if incoming.len() != 1 || incoming[0] != expected_source {
                        return Err(GraphError::InvalidSnapshot(format!(
                            "fanout `{fanout}` branch node `{current}` has external or overlapping input"
                        )));
                    }
                    let outgoing = self.outgoing(current);
                    if outgoing.len() != 1 {
                        return Err(GraphError::InvalidSnapshot(format!(
                            "fanout `{fanout}` branch node `{current}` must have exactly one outgoing edge"
                        )));
                    }
                    path.push(current.to_owned());
                    current = outgoing[0];
                    if path.len() > self.nodes.len() {
                        return Err(GraphError::InvalidSnapshot(format!(
                            "fanout `{fanout}` branch contains a cycle"
                        )));
                    }
                }
                tails.insert(path.last().expect("non-empty branch").clone());
                branch_paths.push(path);
            }
            let join_incoming = self
                .edges
                .iter()
                .filter(|edge| edge.to_node == *join)
                .map(|edge| edge.from_node.as_str())
                .collect::<BTreeSet<_>>();
            if join_incoming != tails.iter().map(String::as_str).collect() {
                return Err(GraphError::InvalidSnapshot(format!(
                    "join `{join}` must receive exactly the paired branch tails"
                )));
            }
            if owned.contains(&self.entry) || self.entry == *join {
                return Err(GraphError::InvalidSnapshot(format!(
                    "parallel region `{fanout}` cannot be bypassed by the graph entry"
                )));
            }
            regions.insert(
                fanout.clone(),
                ParallelRegion {
                    fanout: fanout.clone(),
                    join: join.clone(),
                    branches: branch_paths,
                },
            );
        }
        if let Some(orphan) = joins.difference(&paired_joins).next() {
            return Err(GraphError::InvalidSnapshot(format!(
                "join `{orphan}` has no paired fanout"
            )));
        }
        Ok(regions)
    }

    fn outgoing<'a>(&'a self, node: &'a str) -> Vec<&'a str> {
        self.edges
            .iter()
            .filter(|edge| edge.from_node == node)
            .map(|edge| edge.to_node.as_str())
            .collect()
    }

    pub fn digest(&self) -> Result<String, GraphError> {
        let bytes = serde_json::to_vec(self).map_err(GraphError::SnapshotDecode)?;
        Ok(format!("{:x}", sha2::Sha256::digest(bytes)))
    }
}
