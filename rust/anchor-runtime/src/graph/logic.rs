use super::*;

pub(crate) fn next_node(record: &GraphRunRecord) -> Result<Option<GraphNode>, GraphError> {
    let back_edges = structural_back_edges(&record.snapshot);
    for node in &record.snapshot.nodes {
        if record.ceased.contains(&node.id)
            || node
                .max_rounds
                .is_some_and(|limit| record.ceased.contains(&format!("{}@{limit}", node.id)))
        {
            continue;
        }
        let scope = scope_of(&node.id);
        if record
            .snapshot
            .module_rounds
            .get(&scope)
            .is_some_and(|limit| record.ceased.contains(&format!("{scope}@{limit}")))
        {
            continue;
        }
        if node.id == record.snapshot.entry
            && record.results.get(&node.id).is_none_or(Vec::is_empty)
        {
            return Ok(Some(node.clone()));
        }
        let incoming = record
            .snapshot
            .edges
            .iter()
            .filter(|e| e.to_node == node.id)
            .collect::<Vec<_>>();
        if incoming.is_empty() {
            continue;
        }
        let all_decided = incoming.iter().all(|e| {
            record
                .decided
                .contains_key(&edge_key(&e.from_node, &e.to_node))
                || (back_edges.contains(&(e.from_node.as_str(), e.to_node.as_str()))
                    && !record.invocations.contains_key(&e.from_node))
        });
        if !all_decided {
            continue;
        }
        let chosen = incoming
            .iter()
            .filter(|e| {
                record
                    .decided
                    .get(&edge_key(&e.from_node, &e.to_node))
                    .is_some_and(|d| d.selected)
            })
            .collect::<Vec<_>>();
        if chosen.is_empty() {
            continue;
        }
        let latest_input = chosen
            .iter()
            .filter_map(|e| {
                record
                    .decided
                    .get(&edge_key(&e.from_node, &e.to_node))
                    .map(|d| d.sequence)
            })
            .max()
            .unwrap_or(0);
        let completed = record
            .results
            .get(&node.id)
            .and_then(|v| v.last())
            .map(|r| r.key.invocation);
        // A selected back-edge can re-enter after its source advances; ordinary selected edges only execute once.
        let has_new = chosen.iter().any(|e| {
            record
                .decided
                .get(&edge_key(&e.from_node, &e.to_node))
                .is_some_and(|d| {
                    d.sequence
                        > record
                            .results
                            .get(&node.id)
                            .and_then(|v| v.last())
                            .map(|r| r.sequence)
                            .unwrap_or(0)
                })
        });
        if completed.is_none() || (latest_input > 0 && has_new) {
            return Ok(Some(node.clone()));
        }
    }
    Ok(record.cursor.as_ref().and_then(|c| {
        record
            .snapshot
            .nodes
            .iter()
            .find(|n| n.id == c.node_id)
            .cloned()
    }))
}

pub(crate) fn expected_input_commits(
    record: &GraphRunRecord,
    node_id: &str,
) -> Result<Vec<CommitRef>, GraphError> {
    let mut expected = Vec::new();
    for edge in record
        .snapshot
        .edges
        .iter()
        .filter(|edge| edge.to_node == node_id)
    {
        let Some(decision) = record
            .decided
            .get(&edge_key(&edge.from_node, &edge.to_node))
        else {
            continue;
        };
        if !decision.selected {
            continue;
        }
        let result = record
            .results
            .get(&edge.from_node)
            .and_then(|results| {
                results.iter().find(|result| {
                    result.key.invocation == decision.source_invocation
                        && result.sequence == decision.result_sequence
                })
            })
            .ok_or_else(|| {
                GraphError::CorruptRun(format!(
                    "selected input edge {} -> {} has no source result",
                    edge.from_node, edge.to_node
                ))
            })?;
        expected.push(result.commit.clone());
    }
    Ok(expected)
}

/// Persist false decisions through nodes that were structurally skipped because
/// every incoming route was explicitly unselected. A previously-run node is
/// inactive again only when it receives a newer false input than its latest
/// result; this retires stale outgoing selections on later loop rounds.
pub(crate) fn propagate_inactive_edges(record: &mut GraphRunRecord) -> Result<bool, GraphError> {
    if record.cursor.is_some() {
        return Ok(false);
    }

    let mut changed = false;
    loop {
        let mut pass_changed = false;
        // First close provably inactive cycles as units. Internal back-edges
        // cannot become false one node at a time because each waits for the
        // other; SCC condensation gives us the correct finite fixed point.
        let candidates = record
            .snapshot
            .nodes
            .iter()
            .filter(|node| {
                node.id != record.snapshot.entry
                    && !record.invocations.contains_key(&node.id)
                    && record
                        .cursor
                        .as_ref()
                        .is_none_or(|cursor| cursor.node_id != node.id)
            })
            .map(|node| node.id.clone())
            .collect::<BTreeSet<_>>();
        let components = strongly_connected_components(&record.snapshot, &candidates);
        let mut inactive_components = BTreeSet::<usize>::new();
        loop {
            let mut component_changed = false;
            for (component_index, component) in components.iter().enumerate() {
                if inactive_components.contains(&component_index) {
                    continue;
                }
                let external_incoming = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|edge| component.contains(&edge.to_node))
                    .filter(|edge| !component.contains(&edge.from_node))
                    .collect::<Vec<_>>();
                let all_inactive = external_incoming.iter().all(|edge| {
                    let source_component = components
                        .iter()
                        .position(|candidate| candidate.contains(&edge.from_node));
                    if source_component.is_some_and(|index| inactive_components.contains(&index)) {
                        return true;
                    }
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .is_some_and(|decision| !decision.selected)
                });
                if !all_inactive {
                    continue;
                }
                inactive_components.insert(component_index);
                component_changed = true;
                let component_edges = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|edge| component.contains(&edge.from_node))
                    .map(|edge| (edge.from_node.clone(), edge.to_node.clone()))
                    .collect::<Vec<_>>();
                for (from, to) in component_edges {
                    let key = edge_key(&from, &to);
                    if record.decided.get(&key).is_some_and(|decision| {
                        !decision.selected && decision.source_invocation == 0
                    }) {
                        continue;
                    }
                    record.sequence += 1;
                    record.decided.insert(
                        key,
                        EdgeDecision {
                            selected: false,
                            sequence: record.sequence,
                            source_invocation: 0,
                            result_sequence: 0,
                        },
                    );
                    pass_changed = true;
                    changed = true;
                }
            }
            if !component_changed {
                break;
            }
        }
        for node in &record.snapshot.nodes {
            if node.id == record.snapshot.entry
                || record.cursor.as_ref().is_some_and(|c| c.node_id == node.id)
            {
                continue;
            }
            let incoming = record
                .snapshot
                .edges
                .iter()
                .filter(|edge| edge.to_node == node.id)
                .collect::<Vec<_>>();
            if incoming.is_empty()
                || incoming.iter().any(|edge| {
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .is_none_or(|decision| decision.selected)
                })
            {
                continue;
            }

            let latest_result_sequence = record
                .results
                .get(&node.id)
                .and_then(|results| results.last())
                .map(|result| result.sequence);
            let latest_input_sequence = incoming
                .iter()
                .filter_map(|edge| {
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .map(|decision| decision.sequence)
                })
                .max()
                .unwrap_or(0);
            if latest_result_sequence.is_some_and(|result| latest_input_sequence <= result) {
                continue;
            }

            let outgoing = record
                .snapshot
                .edges
                .iter()
                .filter(|edge| edge.from_node == node.id)
                .map(|edge| edge.to_node.clone())
                .collect::<Vec<_>>();
            for target in outgoing {
                let key = edge_key(&node.id, &target);
                if record.decided.get(&key).is_some_and(|decision| {
                    !decision.selected
                        && latest_result_sequence
                            .is_none_or(|result_sequence| decision.sequence > result_sequence)
                }) {
                    continue;
                }
                record.sequence += 1;
                record.decided.insert(
                    key,
                    EdgeDecision {
                        selected: false,
                        sequence: record.sequence,
                        source_invocation: 0,
                        result_sequence: 0,
                    },
                );
                pass_changed = true;
                changed = true;
            }
        }
        if !pass_changed {
            return Ok(changed);
        }
    }
}

pub(crate) fn strongly_connected_components(
    snapshot: &GraphSnapshot,
    candidates: &BTreeSet<String>,
) -> Vec<BTreeSet<String>> {
    fn visit(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !visited.insert(node.to_owned()) {
            return;
        }
        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                visit(neighbor, adjacency, visited, order);
            }
        }
        order.push(node.to_owned());
    }
    fn collect(
        node: &str,
        adjacency: &BTreeMap<String, Vec<String>>,
        component: &mut BTreeSet<String>,
    ) {
        if !component.insert(node.to_owned()) {
            return;
        }
        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                collect(neighbor, adjacency, component);
            }
        }
    }

    let mut forward = candidates
        .iter()
        .map(|node| (node.clone(), Vec::new()))
        .collect::<BTreeMap<_, _>>();
    let mut reverse = forward.clone();
    for edge in &snapshot.edges {
        if candidates.contains(&edge.from_node) && candidates.contains(&edge.to_node) {
            forward
                .get_mut(&edge.from_node)
                .expect("candidate adjacency")
                .push(edge.to_node.clone());
            reverse
                .get_mut(&edge.to_node)
                .expect("candidate reverse adjacency")
                .push(edge.from_node.clone());
        }
    }
    let mut order = Vec::new();
    let mut visited = BTreeSet::new();
    for node in candidates {
        visit(node, &forward, &mut visited, &mut order);
    }
    visited.clear();
    let mut components = Vec::new();
    while let Some(node) = order.pop() {
        if visited.contains(&node) {
            continue;
        }
        let mut component = BTreeSet::new();
        collect(&node, &reverse, &mut component);
        visited.extend(component.iter().cloned());
        components.push(component);
    }
    components
}

pub(crate) fn refuse_module_activation(
    record: &mut GraphRunRecord,
    scope: &str,
    entry: &str,
) -> Result<(), GraphError> {
    let prefix = format!("{scope}/");
    // A denied activation invalidates the incoming trigger for this round.
    // Keep prior commits/results as history, while making the latest edge facts
    // show that this attempt did not enter or traverse the module.
    let prior_result_sequence = record
        .results
        .get(entry)
        .and_then(|results| results.last())
        .map(|result| result.sequence)
        .unwrap_or(0);
    let trigger = record
        .snapshot
        .edges
        .iter()
        .filter(|edge| edge.to_node == entry && !edge.from_node.starts_with(&prefix))
        .filter_map(|edge| {
            record
                .decided
                .get(&edge_key(&edge.from_node, entry))
                .filter(|decision| decision.selected && decision.sequence > prior_result_sequence)
                .map(|decision| (edge.from_node.clone(), decision.sequence))
        })
        .max_by_key(|(_, sequence)| *sequence)
        .map(|(source, _)| source);
    if let Some(source) = trigger {
        record.sequence += 1;
        record.decided.insert(
            edge_key(&source, entry),
            EdgeDecision {
                selected: false,
                sequence: record.sequence,
                source_invocation: 0,
                result_sequence: 0,
            },
        );
    }

    let members = record
        .snapshot
        .nodes
        .iter()
        .filter(|node| node.id.starts_with(&prefix))
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let outgoing = record
        .snapshot
        .edges
        .iter()
        .filter(|edge| members.contains(&edge.from_node))
        .map(|edge| (edge.from_node.clone(), edge.to_node.clone()))
        .collect::<Vec<_>>();
    for (from, to) in outgoing {
        record.sequence += 1;
        record.decided.insert(
            edge_key(&from, &to),
            EdgeDecision {
                selected: false,
                sequence: record.sequence,
                source_invocation: 0,
                result_sequence: 0,
            },
        );
    }
    Ok(())
}

pub(crate) fn structural_back_edges(snapshot: &GraphSnapshot) -> BTreeSet<(&str, &str)> {
    let mut color: BTreeMap<&str, u8> = BTreeMap::new();
    let mut found = BTreeSet::new();
    let mut stack = vec![(snapshot.entry.as_str(), 0usize)];
    color.insert(snapshot.entry.as_str(), 1);
    while let Some((node, index)) = stack.pop() {
        let outgoing = snapshot
            .edges
            .iter()
            .filter(|e| e.from_node == node)
            .collect::<Vec<_>>();
        if index >= outgoing.len() {
            color.insert(node, 2);
            continue;
        }
        stack.push((node, index + 1));
        let target = outgoing[index].to_node.as_str();
        match color.get(target).copied() {
            Some(1) => {
                found.insert((node, target));
            }
            None => {
                color.insert(target, 1);
                stack.push((target, 0));
            }
            _ => {}
        }
    }
    found
}

pub(crate) fn select_route(
    c: &NodeCompletion,
    routes: &[String],
) -> Result<Option<String>, GraphError> {
    if let Some(route) = &c.route
        && !routes.iter().any(|allowed| allowed == route)
    {
        return Err(GraphError::InvalidRoute(format!(
            "route `{route}` is not an exit"
        )));
    }
    match routes.len() {
        0 => Ok(None),
        1 => Ok(Some(routes[0].clone())),
        _ => {
            let route = c
                .route
                .as_ref()
                .ok_or_else(|| GraphError::InvalidRoute("multiple exits require route".into()))?;
            Ok(Some(route.clone()))
        }
    }
}
pub(crate) fn interruption_completion(reason: &str, route: Option<String>) -> NodeCompletion {
    NodeCompletion {
        submission: reason.to_owned(),
        route,
        model_requests: 0,
        output: serde_json::json!({"interrupted": true, "reason": reason}),
    }
}

pub(crate) fn edge_key(a: &str, b: &str) -> String {
    format!("{a}|{b}")
}
pub(crate) fn merge_values(default: &Value, override_value: &Value, node_value: &Value) -> Value {
    fn merge(base: &Value, over: &Value) -> Value {
        match (base, over) {
            (Value::Object(a), Value::Object(b)) => {
                let mut out = a.clone();
                for (k, v) in b {
                    out.insert(
                        k.clone(),
                        if out.get(k).is_some_and(Value::is_object) && v.is_object() {
                            merge(&out[k], v)
                        } else {
                            v.clone()
                        },
                    );
                }
                Value::Object(out)
            }
            (_, Value::Null) => base.clone(),
            (_, v) => v.clone(),
        }
    }
    merge(&merge(default, override_value), node_value)
}
pub(crate) fn node_task(
    objective: &str,
    instructions: &str,
    local_instruction: &str,
    input: &Value,
) -> String {
    let input_guidance = "\n\nUpstream artifacts, when present, are mounted read-only under `/in/<node-id>`. Use `find /in` to inspect the files available to this node; write deliverables in `/workspace`.";
    format!(
        "Objective:\n{objective}\n\nInstructions:\n{instructions}{}{}\n\nInput:\n{}",
        if local_instruction.is_empty() {
            String::new()
        } else {
            format!("\n\nNode instructions:\n{local_instruction}")
        },
        input_guidance,
        input
    )
}

pub(crate) fn execution_request(
    record: &GraphRunRecord,
    cursor: &RunCursor,
    cancellation: crate::Cancellation,
) -> Result<NodeExecutionRequest, GraphError> {
    let node = record
        .snapshot
        .nodes
        .iter()
        .find(|node| node.id == cursor.node_id)
        .expect("validated cursor node");
    let agent = node
        .agent
        .as_ref()
        .and_then(|name| record.snapshot.agents.get(name));
    let operation = node
        .op
        .as_ref()
        .and_then(|name| record.snapshot.ops.get(name))
        .and_then(|value| {
            value
                .get("run")
                .or_else(|| value.get("host"))
                .cloned()
                .or_else(|| {
                    value
                        .get("join")
                        .map(|join| serde_json::json!({"join":join}))
                })
        });
    let routes = record
        .snapshot
        .edges
        .iter()
        .filter(|edge| edge.from_node == node.id)
        .map(|edge| edge.to_node.clone())
        .collect::<Vec<_>>();
    let (kind, model, instructions, max_provider_requests, wall_time_limit_seconds, network) =
        if let Some(agent) = agent {
            (
                NodeKind::Agent,
                Some(agent.model.clone()),
                agent.instructions.clone(),
                agent.max_steps,
                Some(agent.wall_time_limit_seconds.unwrap_or(3600.0)),
                agent.network,
            )
        } else {
            let op = node
                .op
                .as_ref()
                .and_then(|name| record.snapshot.ops.get(name))
                .expect("validated op");
            let host = op.get("host").is_some();
            let wall_time_limit_seconds = op
                .get("wall_time_limit_seconds")
                .and_then(Value::as_f64)
                .or_else(|| (!host).then_some(3600.0));
            (
                if host {
                    NodeKind::OpHost
                } else {
                    NodeKind::OpRun
                },
                None,
                String::new(),
                None,
                wall_time_limit_seconds,
                op.get("network").and_then(Value::as_bool).unwrap_or(false),
            )
        };
    let local_instruction = node
        .input
        .as_ref()
        .and_then(Value::as_str)
        .unwrap_or_default();
    Ok(NodeExecutionRequest {
        key: cursor.key.clone(),
        model,
        task: node_task(
            &record.snapshot.objective,
            &instructions,
            local_instruction,
            &cursor.prepared_input,
        ),
        instructions,
        routes,
        input: cursor.prepared_input.clone(),
        input_commits: cursor.input_commits.clone(),
        plugins: node
            .plugins
            .iter()
            .map(|id| {
                record.plugin_bindings.get(id).cloned().ok_or_else(|| {
                    GraphError::CorruptRun(format!("Run has no frozen Plugin binding for `{id}`"))
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        max_provider_requests,
        wall_time_limit_seconds,
        network,
        kind,
        operation,
        cancellation,
    })
}
pub(crate) fn scope_of(node_id: &str) -> String {
    node_id
        .rsplit_once('/')
        .map(|(scope, _)| scope.to_owned())
        .unwrap_or_default()
}
pub(crate) fn validate_component(s: &str) -> Result<(), GraphError> {
    if s.is_empty()
        || s == "."
        || s == ".."
        || s.chars()
            .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        Err(GraphError::InvalidRunId(s.to_owned()))
    } else {
        Ok(())
    }
}
pub(crate) fn validate_node_id(s: &str) -> Result<(), GraphError> {
    if s.is_empty()
        || s.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part == ".graph-calls"
                || part
                    .chars()
                    .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        })
    {
        Err(GraphError::InvalidSnapshot(format!("unsafe node id `{s}`")))
    } else {
        Ok(())
    }
}
pub(crate) fn validate_wall_time(value: Option<f64>, node_id: &str) -> Result<(), GraphError> {
    if value.is_some_and(|seconds| !seconds.is_finite() || seconds <= 0.0) {
        return Err(GraphError::InvalidSnapshot(format!(
            "node `{node_id}` wall_time_limit_seconds must be positive and finite"
        )));
    }
    Ok(())
}
