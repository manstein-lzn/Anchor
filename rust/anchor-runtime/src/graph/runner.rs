use super::*;
use sha2::Digest;

pub struct GraphRunner<'a, S, A, N, C> {
    pub(super) store: &'a S,
    pub(super) artifacts: &'a A,
    pub(super) nodes: &'a N,
    pub(super) control: &'a C,
}

impl<'a, S: RunStore, A: ArtifactPort, N: NodeExecutionPort, C: RunControl>
    GraphRunner<'a, S, A, N, C>
{
    pub fn new(store: &'a S, artifacts: &'a A, nodes: &'a N, control: &'a C) -> Self {
        Self {
            store,
            artifacts,
            nodes,
            control,
        }
    }
    pub async fn run(&self, mut record: GraphRunRecord) -> Result<GraphRunRecord, GraphError> {
        record.migrate_format()?;
        record.validate()?;
        self.admit_capabilities(&record.snapshot)?;
        let _lease = self.store.acquire_lease(&record.run_id)?;
        if let Some(mut saved) = self.store.load(&record.run_id)? {
            saved.migrate_format()?;
            if saved != record {
                return Err(GraphError::RunConflict);
            }
        }
        // Upgrade an in-flight record written by the earlier R5 counter timing:
        // it persisted a cursor before dispatch, but counted only completed nodes.
        // The cursor itself proves this invocation started, so promote that fact
        // once before writing the resumed record.
        if let Some(cursor) = &record.cursor
            && record.invocations.get(&cursor.node_id).copied() != Some(cursor.key.invocation)
        {
            record
                .invocations
                .insert(cursor.node_id.clone(), cursor.key.invocation);
            *record.passes.entry(cursor.node_id.clone()).or_default() += 1;
        }
        if matches!(
            record.status,
            RunStatus::Failed | RunStatus::Completed | RunStatus::Aborted
        ) {
            self.store.save(&record)?;
            return Ok(record);
        }
        if record.status == RunStatus::WaitingRecovery {
            // Recovery is an explicit operator action. Re-entering GraphRunner
            // directly must not turn a waiting record into ordinary resume.
            self.store.save(&record)?;
            return Ok(record);
        }
        if let Err(reason) = self.bind_plugin_manifests(&mut record) {
            return self.fail(record, reason);
        }
        record.status = RunStatus::Running;
        self.store.save(&record)?;
        loop {
            if self.control.stop_requested() {
                record.status = RunStatus::Stopped;
                self.store.save(&record)?;
                return Ok(record);
            }
            if self.control.pause_requested() && record.cursor.is_none() {
                record.status = RunStatus::Paused;
                self.store.save(&record)?;
                return Ok(record);
            }
            if propagate_inactive_edges(&mut record)? {
                self.store.save(&record)?;
            }
            if record.parallel.as_ref().is_some_and(|activation| {
                activation
                    .branches
                    .iter()
                    .any(|branch| branch.status != ParallelBranchStatus::Completed)
            }) {
                if !self.run_parallel_wave(&mut record).await? {
                    return Ok(record);
                }
                continue;
            }
            let Some(node) = next_node(&record)? else {
                record.status = if record.ceased.is_empty() {
                    RunStatus::Completed
                } else {
                    RunStatus::Stopped
                };
                self.store.save(&record)?;
                return Ok(record);
            };
            if record.cursor.is_none() {
                let scope = scope_of(&node.id);
                let prior_sequence = record
                    .results
                    .get(&node.id)
                    .and_then(|results| results.last())
                    .map(|r| r.sequence)
                    .unwrap_or(0);
                let enters_scope = !scope.is_empty()
                    && record
                        .snapshot
                        .edges
                        .iter()
                        .filter(|e| {
                            e.to_node == node.id && !e.from_node.starts_with(&format!("{scope}/"))
                        })
                        .any(|e| {
                            record
                                .decided
                                .get(&edge_key(&e.from_node, &e.to_node))
                                .is_some_and(|d| d.selected && d.sequence > prior_sequence)
                        });
                if enters_scope
                    && let Some(limit) = record.snapshot.module_rounds.get(&scope).copied()
                {
                    let next = record.module_activations.get(&scope).copied().unwrap_or(0) + 1;
                    if next > u64::from(limit) {
                        record.ceased.insert(format!("{scope}@{limit}"));
                        refuse_module_activation(&mut record, &scope, &node.id)?;
                        continue;
                    }
                    record.module_activations.insert(scope.clone(), next);
                    for member in &record.snapshot.nodes {
                        if member.id.starts_with(&format!("{scope}/")) {
                            record.passes.remove(&member.id);
                        }
                    }
                    for nested in record
                        .module_activations
                        .keys()
                        .filter(|nested| nested.starts_with(&format!("{scope}/")))
                        .cloned()
                        .collect::<Vec<_>>()
                    {
                        record.module_activations.remove(&nested);
                    }
                }
                if let Some(limit) = node.max_rounds
                    && record.passes.get(&node.id).copied().unwrap_or(0) >= u64::from(limit)
                {
                    record.ceased.insert(format!("{}@{limit}", node.id));
                    self.refuse_edges(&mut record, &node.id)?;
                    continue;
                }
                let invocation = record.invocations.get(&node.id).copied().unwrap_or(0) + 1;
                let key = InvocationKey {
                    run_id: record.run_id.clone(),
                    graph_digest: record.graph_digest.clone(),
                    node_id: node.id.clone(),
                    invocation,
                };
                let mut input_commits = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|e| e.to_node == node.id)
                    .filter_map(|e| {
                        record
                            .decided
                            .get(&edge_key(&e.from_node, &e.to_node))
                            .filter(|d| d.selected)
                            .and_then(|decision| {
                                record
                                    .results
                                    .get(&e.from_node)
                                    .and_then(|rs| {
                                        let result_sequence = if decision.result_sequence == 0 {
                                            decision.sequence
                                        } else {
                                            decision.result_sequence
                                        };
                                        rs.iter().find(|r| r.sequence == result_sequence)
                                    })
                                    .map(|r| r.commit.clone())
                            })
                    })
                    .collect::<Vec<_>>();
                let join_activation = record
                    .parallel
                    .as_ref()
                    .filter(|activation| activation.join_node == node.id);
                if let Some(activation) = join_activation {
                    input_commits = activation
                        .branches
                        .iter()
                        .flat_map(|branch| branch.completed.iter().cloned())
                        .collect();
                }
                let mut resolved = Vec::new();
                for commit in &input_commits {
                    resolved.push(self.artifacts.resolve(commit).await?);
                }
                let mut prepared_input = serde_json::json!({
                    "input":record.input,
                    "committed_inputs":resolved,
                });
                if let Some(activation) = join_activation {
                    let branch_inputs = activation
                        .branches
                        .iter()
                        .map(|branch| {
                            let nodes = branch
                                .completed
                                .iter()
                                .map(|commit| {
                                    let output = record
                                        .results
                                        .get(&commit.node_id)
                                        .and_then(|results| {
                                            results.iter().find(|result| result.commit == *commit)
                                        })
                                        .map(|result| result.completion.output.clone())
                                        .unwrap_or(Value::Null);
                                    serde_json::json!({"node":commit.node_id,
                                        "commit":commit,
                                        "output":output})
                                })
                                .collect::<Vec<_>>();
                            serde_json::json!({"branch_id":branch.branch_id,
                                "entry":branch.entry,"nodes":nodes})
                        })
                        .collect::<Vec<_>>();
                    prepared_input["parallel"] = serde_json::json!({
                        "activation_id":activation.activation_id,
                        "fanout":activation.fanout_node,
                        "join":activation.join_node,
                        "branches":branch_inputs,
                    });
                }
                record.cursor = Some(RunCursor {
                    node_id: node.id.clone(),
                    key,
                    input_commits,
                    prepared_input,
                });
                // Python's Graph runner records a pass/run when the node starts,
                // including attempts that stop at the provider budget boundary.
                // Persist these counters with the cursor so resume observes the
                // same started invocation without counting it a second time.
                let cursor = record.cursor.as_ref().expect("cursor established");
                record
                    .invocations
                    .insert(cursor.node_id.clone(), cursor.key.invocation);
                *record.passes.entry(cursor.node_id.clone()).or_default() += 1;
                record.status = RunStatus::Running;
                self.store.save(&record)?; // durable before dispatch
            }
            if self.control.stop_requested() {
                record.status = RunStatus::Stopped;
                self.store.save(&record)?;
                return Ok(record);
            }
            let cursor = record.cursor.clone().expect("cursor established");
            let is_fanout = record
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == cursor.node_id)
                .and_then(|node| node.op.as_ref())
                .and_then(|op| record.snapshot.ops.get(op))
                .is_some_and(|operation| operation.get("fanout").is_some());
            let call_spec = record
                .snapshot
                .nodes
                .iter()
                .find(|node| node.id == cursor.node_id)
                .and_then(|node| node.op.as_ref())
                .and_then(|name| record.snapshot.ops.get(name))
                .and_then(|operation| operation.get("call"))
                .cloned();
            let is_call = call_spec.is_some();
            let is_join_control = record
                .parallel
                .as_ref()
                .is_some_and(|activation| activation.join_node == cursor.node_id);
            let fact = if is_fanout || is_join_control || is_call {
                CompletionFact::NotStarted
            } else {
                self.nodes.completion_fact(&cursor.key).await?
            };
            let completion = match fact {
                CompletionFact::Completed(c) => Some(c),
                CompletionFact::Failed(reason) => {
                    return self
                        .fail_known_node(record, format!("{} failed: {reason}", cursor.node_id));
                }
                CompletionFact::Uncertain(reason) => {
                    return self.fail(
                        record,
                        format!("uncertain node result for {}: {reason}", cursor.node_id),
                    );
                }
                CompletionFact::NotStarted | CompletionFact::Resumable => None,
            };
            let completion = if let Some(c) = completion {
                c
            } else if let Some(spec) = call_spec {
                let target_graph = spec
                    .get("graph")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        GraphError::InvalidSnapshot("op.call.graph must be a string".into())
                    })?
                    .to_owned();
                let mode = spec.get("mode").and_then(Value::as_str).unwrap_or("wait");
                let spec_bytes = serde_json::to_vec(&spec).map_err(GraphError::SnapshotDecode)?;
                let identity = CallIdentity {
                    parent_run_id: record.run_id.clone(),
                    parent_graph_digest: record.graph_digest.clone(),
                    node_id: cursor.node_id.clone(),
                    invocation: cursor.key.invocation,
                    call_spec_digest: format!("{:x}", sha2::Sha256::digest(spec_bytes)),
                };
                let identity_key = identity.durable_key();
                // Older Run records may contain a `Deleted` marker written by the
                // previous cascade-delete behavior. Preserve their resume behavior;
                // new deletions reject callers and never create this marker.
                if let Some(existing) = record
                    .graph_calls
                    .get(&identity_key)
                    .filter(|call| call.status == GraphCallStatus::Deleted)
                {
                    let output = existing.output.clone().unwrap_or_else(|| {
                        serde_json::json!({
                            "graph": target_graph,
                            "run_id": existing.child_run_id,
                            "mode": existing.mode,
                            "status": "deleted",
                        })
                    });
                    NodeCompletion {
                        submission: output.to_string(),
                        route: None,
                        model_requests: 0,
                        output,
                    }
                } else {
                    let port = self.nodes.graph_call_port().ok_or_else(|| {
                        GraphError::Unsupported("op.call requires a GraphCallPort".into())
                    })?;
                    let call_outcome = port
                        .call(
                            &identity,
                            &spec,
                            &cursor.prepared_input,
                            self.control.cancellation(),
                        )
                        .await?;
                    if mode == "wait"
                        && self.control.stop_requested()
                        && let GraphCallOutcome::Waiting { child_run_id } = &call_outcome
                    {
                        record.graph_calls.insert(
                            identity_key,
                            GraphCallRecord {
                                identity,
                                graph: target_graph,
                                child_run_id: Some(child_run_id.clone()),
                                mode: "wait".into(),
                                status: GraphCallStatus::Waiting,
                                output: None,
                                error: None,
                            },
                        );
                        record.status = RunStatus::Stopped;
                        self.store.save(&record)?;
                        return Ok(record);
                    }
                    match call_outcome {
                        GraphCallOutcome::Waiting { child_run_id } if mode == "wait" => {
                            record.graph_calls.insert(
                                identity_key,
                                GraphCallRecord {
                                    identity,
                                    graph: target_graph,
                                    child_run_id: Some(child_run_id),
                                    mode: "wait".into(),
                                    status: GraphCallStatus::Waiting,
                                    output: None,
                                    error: None,
                                },
                            );
                            record.status = RunStatus::WaitingCall;
                            self.store.save(&record)?;
                            return Ok(record);
                        }
                        GraphCallOutcome::Detached { child_run_id } if mode == "detach" => {
                            let output = serde_json::json!({"graph":target_graph,"run_id":child_run_id,
                            "mode":"detach","status":"accepted"});
                            record.graph_calls.insert(
                                identity_key,
                                GraphCallRecord {
                                    identity,
                                    graph: target_graph,
                                    child_run_id: Some(child_run_id),
                                    mode: "detach".into(),
                                    status: GraphCallStatus::Detached,
                                    output: Some(output.clone()),
                                    error: None,
                                },
                            );
                            NodeCompletion {
                                submission: output.to_string(),
                                route: None,
                                model_requests: 0,
                                output,
                            }
                        }
                        GraphCallOutcome::Completed {
                            child_run_id,
                            output,
                        } if mode == "wait" => {
                            record.graph_calls.insert(
                                identity_key,
                                GraphCallRecord {
                                    identity,
                                    graph: target_graph,
                                    child_run_id: Some(child_run_id),
                                    mode: "wait".into(),
                                    status: GraphCallStatus::Completed,
                                    output: Some(output.clone()),
                                    error: None,
                                },
                            );
                            NodeCompletion {
                                submission: output.to_string(),
                                route: None,
                                model_requests: 0,
                                output,
                            }
                        }
                        GraphCallOutcome::Completed { child_run_id, .. } if mode == "detach" => {
                            let output = serde_json::json!({"graph":target_graph,"run_id":child_run_id,
                            "mode":"detach","status":"accepted"});
                            record.graph_calls.insert(
                                identity_key,
                                GraphCallRecord {
                                    identity,
                                    graph: target_graph,
                                    child_run_id: Some(child_run_id),
                                    mode: "detach".into(),
                                    status: GraphCallStatus::Detached,
                                    output: Some(output.clone()),
                                    error: None,
                                },
                            );
                            NodeCompletion {
                                submission: output.to_string(),
                                route: None,
                                model_requests: 0,
                                output,
                            }
                        }
                        GraphCallOutcome::Failed {
                            child_run_id,
                            reason,
                        } => {
                            record.graph_calls.insert(
                                identity_key,
                                GraphCallRecord {
                                    identity,
                                    graph: target_graph,
                                    child_run_id,
                                    mode: mode.into(),
                                    status: GraphCallStatus::Failed,
                                    output: None,
                                    error: Some(reason.clone()),
                                },
                            );
                            return self
                                .fail_known_node(record, format!("Graph call failed: {reason}"));
                        }
                        GraphCallOutcome::Uncertain {
                            child_run_id,
                            reason,
                        } => {
                            record.graph_calls.insert(
                                identity_key,
                                GraphCallRecord {
                                    identity,
                                    graph: target_graph,
                                    child_run_id,
                                    mode: mode.into(),
                                    status: GraphCallStatus::Uncertain,
                                    output: None,
                                    error: Some(reason.clone()),
                                },
                            );
                            return self.fail(
                                record,
                                format!("Graph call outcome uncertain; refusing replay: {reason}"),
                            );
                        }
                        outcome => {
                            return Err(GraphError::Unsupported(format!(
                                "GraphCallPort returned incompatible outcome: {outcome:?}"
                            )));
                        }
                    }
                }
            } else if is_fanout {
                let region = record
                    .snapshot
                    .parallel_regions()?
                    .get(&cursor.node_id)
                    .cloned()
                    .expect("admitted fanout region");
                let activation_id = format!(
                    "{}:{}:{}",
                    record.run_id, cursor.node_id, cursor.key.invocation
                );
                NodeCompletion {
                    submission: format!("fanout activation {activation_id}"),
                    route: None,
                    model_requests: 0,
                    output: serde_json::json!({
                        "activation_id":activation_id,
                        "fanout":region.fanout,
                        "join":region.join,
                        "branches":region.branches,
                    }),
                }
            } else if is_join_control {
                let activation = record.parallel.as_ref().expect("paired join activation");
                let branches = activation
                    .branches
                    .iter()
                    .map(|branch| {
                        let nodes = branch
                            .completed
                            .iter()
                            .map(|commit| {
                                let result = record
                                    .results
                                    .get(&commit.node_id)
                                    .and_then(|results| {
                                        results.iter().find(|result| result.commit == *commit)
                                    })
                                    .expect("activation validation binds branch result");
                                serde_json::json!({"node":commit.node_id,"commit":commit,
                                "files":[],"summary":result.completion.submission,
                                "output":result.completion.output})
                            })
                            .collect::<Vec<_>>();
                        serde_json::json!({"branch_id":branch.branch_id,"entry":branch.entry,
                            "output":branch.nodes.last(),"status":"completed","nodes":nodes})
                    })
                    .collect::<Vec<_>>();
                let manifest = serde_json::json!({"activation_id":activation.activation_id,
                    "fanout":activation.fanout_node,"join":activation.join_node,"branches":branches});
                NodeCompletion {
                    submission: serde_json::to_string(&manifest).unwrap_or_default(),
                    route: None,
                    model_requests: 0,
                    output: manifest,
                }
            } else {
                if self.control.pause_requested() {
                    record.status = RunStatus::Paused;
                    self.store.save(&record)?;
                    return Ok(record);
                }
                let def = record
                    .snapshot
                    .nodes
                    .iter()
                    .find(|n| n.id == cursor.node_id)
                    .expect("validated node");
                let agent = def
                    .agent
                    .as_ref()
                    .and_then(|name| record.snapshot.agents.get(name));
                let operation = def
                    .op
                    .as_ref()
                    .and_then(|name| record.snapshot.ops.get(name))
                    .and_then(|value| {
                        value.get("run").cloned().or_else(|| {
                            value
                                .get("join")
                                .map(|join| serde_json::json!({"join":join}))
                        })
                    });
                let routes = record
                    .snapshot
                    .edges
                    .iter()
                    .filter(|e| e.from_node == def.id)
                    .map(|e| e.to_node.clone())
                    .collect::<Vec<_>>();
                if self.control.stop_requested() {
                    record.status = RunStatus::Stopped;
                    self.store.save(&record)?;
                    return Ok(record);
                }
                let (
                    kind,
                    model,
                    instructions,
                    max_provider_requests,
                    wall_time_limit_seconds,
                    network,
                ) = if let Some(agent) = agent {
                    (
                        NodeKind::Agent,
                        Some(agent.model.clone()),
                        agent.instructions.clone(),
                        agent.max_steps,
                        Some(agent.wall_time_limit_seconds.unwrap_or(3600.0)),
                        agent.network,
                    )
                } else {
                    let op = def
                        .op
                        .as_ref()
                        .and_then(|name| record.snapshot.ops.get(name))
                        .expect("validated op");
                    (
                        NodeKind::OpRun,
                        None,
                        String::new(),
                        None,
                        Some(
                            op.get("wall_time_limit_seconds")
                                .and_then(Value::as_f64)
                                .unwrap_or(3600.0),
                        ),
                        op.get("network").and_then(Value::as_bool).unwrap_or(false),
                    )
                };
                let local_instruction = def
                    .input
                    .as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let request = NodeExecutionRequest {
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
                    plugins: def
                        .plugins
                        .iter()
                        .map(|id| {
                            record.plugin_bindings.get(id).cloned().ok_or_else(|| {
                                GraphError::CorruptRun(format!(
                                    "Run has no frozen Plugin binding for `{id}`"
                                ))
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?,
                    max_provider_requests,
                    wall_time_limit_seconds,
                    network,
                    kind,
                    operation,
                    cancellation: self.control.cancellation(),
                };
                match self.nodes.execute(request).await? {
                    NodeExecutionOutcome::Completed(c) => {
                        record.recovery.retain(|pending| pending.key != cursor.key);
                        c
                    }
                    NodeExecutionOutcome::WaitingRecovery { attempts } => {
                        record.recovery.retain(|pending| pending.key != cursor.key);
                        record.recovery.extend(attempts.into_iter().map(|attempt| {
                            PendingRecovery {
                                key: cursor.key.clone(),
                                attempt,
                            }
                        }));
                        record.status = RunStatus::WaitingRecovery;
                        self.store.save(&record)?;
                        return Ok(record);
                    }
                    NodeExecutionOutcome::BudgetExhausted { .. } => {
                        record.recovery.retain(|pending| pending.key != cursor.key);
                        record.status = RunStatus::BudgetStopped;
                        self.store.save(&record)?;
                        return Ok(record);
                    }
                    NodeExecutionOutcome::Cancelled => {
                        record.recovery.retain(|pending| pending.key != cursor.key);
                        record.status = RunStatus::Stopped;
                        self.store.save(&record)?;
                        return Ok(record);
                    }
                    NodeExecutionOutcome::Aborted => {
                        record.recovery.retain(|pending| pending.key != cursor.key);
                        record.status = RunStatus::Aborted;
                        record.error = Some(format!("{} aborted by operator", cursor.node_id));
                        self.store.save(&record)?;
                        return Ok(record);
                    }
                    NodeExecutionOutcome::Failed { reason } => {
                        return self.fail_known_node(
                            record,
                            format!("{} failed: {reason}", cursor.node_id),
                        );
                    }
                }
            };
            let routes = record
                .snapshot
                .edges
                .iter()
                .filter(|e| e.from_node == cursor.node_id)
                .map(|e| e.to_node.clone())
                .collect::<Vec<_>>();
            let chosen = if is_fanout {
                None
            } else {
                match select_route(&completion, &routes) {
                    Ok(route) => route,
                    Err(error) => {
                        return self
                            .fail_known_node(record, format!("{}: {error}", cursor.node_id));
                    }
                }
            };
            let context = ArtifactFreezeContext {
                kind: if is_fanout {
                    ArtifactKind::Fanout
                } else if is_join_control {
                    ArtifactKind::Join
                } else if is_call {
                    ArtifactKind::GraphCall
                } else {
                    ArtifactKind::Node
                },
                input_commits: cursor.input_commits.clone(),
            };
            let commit = self
                .artifacts
                .freeze_with_context(&cursor.key, &completion, &context)
                .await?;
            record.sequence += 1;
            let result = RunResult {
                node_id: cursor.node_id.clone(),
                key: cursor.key.clone(),
                completion,
                commit,
                sequence: record.sequence,
            };
            // completion fact is durable in the NodeExecutionPort, then artifact freeze,
            // then this atomic Run record establishes result/history/edge decisions.
            let result_sequence = result.sequence;
            record
                .results
                .entry(cursor.node_id.clone())
                .or_default()
                .push(result);
            // Edge decisions happen after the result is committed. Giving them
            // their own sequence lets a selected self-loop be recognized as a
            // fresh re-entry after the node's latest result.
            record.sequence += 1;
            for target in routes {
                record.decided.insert(
                    edge_key(&cursor.node_id, &target),
                    EdgeDecision {
                        selected: is_fanout || Some(&target) == chosen.as_ref(),
                        sequence: record.sequence,
                        source_invocation: cursor.key.invocation,
                        result_sequence,
                    },
                );
            }
            if is_fanout {
                let region = record
                    .snapshot
                    .parallel_regions()?
                    .get(&cursor.node_id)
                    .cloned()
                    .expect("admitted fanout region");
                let activation_id = format!(
                    "{}:{}:{}",
                    record.run_id, cursor.node_id, cursor.key.invocation
                );
                record.parallel = Some(ParallelActivation {
                    activation_id: activation_id.clone(),
                    fanout_node: cursor.node_id.clone(),
                    join_node: region.join,
                    fanout_invocation: cursor.key.invocation,
                    branches: region
                        .branches
                        .into_iter()
                        .enumerate()
                        .map(|(index, nodes)| ParallelBranchRecord {
                            branch_id: format!("{activation_id}:branch:{index}"),
                            entry: nodes[0].clone(),
                            nodes,
                            next_index: 0,
                            status: ParallelBranchStatus::Ready,
                            cursor: None,
                            completed: Vec::new(),
                            error: None,
                        })
                        .collect(),
                });
            } else if is_join_control {
                record.parallel = None;
            }
            record.cursor = None;
            record.status = RunStatus::Running;
            self.store.save(&record)?;
        }
    }
    pub(super) fn refuse_edges(
        &self,
        record: &mut GraphRunRecord,
        node_id: &str,
    ) -> Result<(), GraphError> {
        record.sequence += 1;
        let invocation = record.invocations.get(node_id).copied().unwrap_or(0);
        for edge in record
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.from_node == node_id)
        {
            record.decided.insert(
                edge_key(node_id, &edge.to_node),
                EdgeDecision {
                    selected: false,
                    sequence: record.sequence,
                    source_invocation: invocation,
                    result_sequence: 0,
                },
            );
        }
        record.status = RunStatus::Running;
        self.store.save(record)
    }
    fn admit_capabilities(&self, snapshot: &GraphSnapshot) -> Result<(), GraphError> {
        let caps = self.nodes.capabilities();
        for node in &snapshot.nodes {
            if node.agent.is_some() && !caps.agent {
                return Err(GraphError::Unsupported(format!(
                    "NodeExecutionPort does not support Agent node `{}`",
                    node.id
                )));
            }
            let op = node.op.as_ref().and_then(|name| snapshot.ops.get(name));
            let is_fanout = op.is_some_and(|op| op.get("fanout").is_some());
            let is_join = op.is_some_and(|op| op.get("join").is_some());
            let is_call = op.is_some_and(|op| op.get("call").is_some());
            if is_call && self.nodes.graph_call_port().is_none() {
                return Err(GraphError::Unsupported(format!(
                    "op.call node `{}` requires a GraphCallPort",
                    node.id
                )));
            }
            if node.op.is_some() && !is_fanout && !is_join && !is_call && !caps.op_run {
                return Err(GraphError::Unsupported(format!(
                    "NodeExecutionPort does not support Op.run node `{}`",
                    node.id
                )));
            }
            if let Some(agent) = node.agent.as_ref().and_then(|n| snapshot.agents.get(n))
                && agent.max_steps.is_some()
                && !caps.exact_provider_request_budget
            {
                return Err(GraphError::Unsupported(format!(
                    "Agent node `{}` has max_steps but NodeExecutionPort cannot enforce cumulative provider-request budget",
                    node.id
                )));
            }
        }
        Ok(())
    }

    fn bind_plugin_manifests(&self, record: &mut GraphRunRecord) -> Result<(), String> {
        let ids = record
            .snapshot
            .nodes
            .iter()
            .flat_map(|node| node.plugins.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if ids.is_empty() {
            if !record.plugin_bindings.is_empty() {
                return Err("Run contains Plugin bindings not referenced by its Graph".into());
            }
            if !record.plugin_bindings_initialized {
                record.plugin_bindings_initialized = true;
            }
            return Ok(());
        }
        let resolved = self
            .nodes
            .resolve_plugins(&ids)
            .map_err(|error| format!("Plugin manifest resolution failed: {error}"))?;
        let valid = resolved.len() == ids.len()
            && resolved
                .iter()
                .zip(&ids)
                .all(|(binding, id)| binding.id == *id && !binding.digest.is_empty());
        if !valid {
            return Err(
                "Plugin resolver returned incomplete, reordered, or unhashed bindings".into(),
            );
        }
        let bindings = resolved
            .into_iter()
            .map(|binding| (binding.id.clone(), binding))
            .collect::<BTreeMap<_, _>>();
        if !record.plugin_bindings_initialized {
            if record.status != RunStatus::Ready
                || !record.plugin_bindings.is_empty()
                || record.cursor.is_some()
                || record.sequence > 0
                || !record.results.is_empty()
            {
                return Err(
                    "cannot establish Plugin resource identity for an in-progress legacy Run"
                        .into(),
                );
            }
            record.plugin_bindings = bindings;
            record.plugin_bindings_initialized = true;
            self.store
                .save(record)
                .map_err(|error| format!("failed to persist Plugin manifest bindings: {error}"))?;
        } else if record.plugin_bindings != bindings {
            return Err("Plugin manifest changed since this Graph Run was admitted".into());
        }
        Ok(())
    }
    fn fail(
        &self,
        mut record: GraphRunRecord,
        reason: String,
    ) -> Result<GraphRunRecord, GraphError> {
        record.status = RunStatus::Failed;
        record.error = Some(reason);
        self.store.save(&record)?;
        Ok(record)
    }

    fn fail_known_node(
        &self,
        mut record: GraphRunRecord,
        reason: String,
    ) -> Result<GraphRunRecord, GraphError> {
        // A known failed outcome cannot be resumed as the same invocation and
        // must never settle outgoing edges. Its started counters remain factual.
        record.cursor = None;
        self.fail(record, reason)
    }
}
