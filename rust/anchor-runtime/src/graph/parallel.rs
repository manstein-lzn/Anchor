use super::*;
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use std::{future::Future, panic::AssertUnwindSafe, pin::Pin};

pub(super) enum ParallelPreparation {
    Cursor(RunCursor),
    Ceased(String),
}

fn parallel_scope_started(record: &GraphRunRecord, current_branch: usize, scope: &str) -> bool {
    let Some(activation) = record.parallel.as_ref() else {
        return false;
    };
    let prefix = format!("{scope}/");
    activation
        .branches
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != current_branch)
        .any(|(_, branch)| {
            (branch.next_index > 0
                && branch
                    .nodes
                    .iter()
                    .take(branch.next_index.min(branch.nodes.len()))
                    .any(|node| node.starts_with(&prefix)))
                || branch
                    .cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.node_id.starts_with(&prefix))
        })
}

impl<'a, S: RunStore, A: ArtifactPort, N: NodeExecutionPort, C: RunControl>
    GraphRunner<'a, S, A, N, C>
{
    pub(super) async fn run_parallel_wave(
        &self,
        record: &mut GraphRunRecord,
    ) -> Result<bool, GraphError> {
        let activation = record.parallel.as_ref().expect("active parallel region");
        let failed = activation
            .branches
            .iter()
            .any(|branch| branch.status == ParallelBranchStatus::Failed);
        if failed {
            record.status = RunStatus::Failed;
            record.error = activation
                .branches
                .iter()
                .find_map(|branch| branch.error.clone());
            self.store.save(record)?;
            return Ok(false);
        }

        // Build and durably publish every branch cursor before dispatching any of
        // this wave's work. Only this coordinator mutates or saves the Run.
        for branch_index in 0..record.parallel.as_ref().unwrap().branches.len() {
            let branch = &record.parallel.as_ref().unwrap().branches[branch_index];
            if branch.status == ParallelBranchStatus::Completed || branch.cursor.is_some() {
                continue;
            }
            if branch.status != ParallelBranchStatus::Ready {
                continue;
            }
            if self.control.stop_requested() || self.control.pause_requested() {
                record.status = if self.control.stop_requested() {
                    RunStatus::Stopped
                } else {
                    RunStatus::Paused
                };
                self.store.save(record)?;
                return Ok(false);
            }
            let prepared = self.prepare_parallel_branch(record, branch_index).await?;
            let ParallelPreparation::Cursor(cursor) = prepared else {
                let reason = match prepared {
                    ParallelPreparation::Ceased(reason) => reason,
                    ParallelPreparation::Cursor(_) => unreachable!(),
                };
                let branch = &mut record.parallel.as_mut().unwrap().branches[branch_index];
                branch.status = ParallelBranchStatus::Failed;
                branch.error = Some(reason.clone());
                record.status = RunStatus::Failed;
                record.error = Some(reason);
                self.store.save(record)?;
                return Ok(false);
            };
            let node_id = cursor.node_id.clone();
            let branch = &mut record.parallel.as_mut().unwrap().branches[branch_index];
            branch.cursor = Some(cursor);
            branch.status = ParallelBranchStatus::Running;
            record.invocations.insert(
                node_id.clone(),
                branch.cursor.as_ref().unwrap().key.invocation,
            );
            *record.passes.entry(node_id).or_default() += 1;
            record.status = RunStatus::Running;
            self.store.save(record)?;
        }

        type CompletionFuture<'a> = Pin<
            Box<
                dyn Future<
                        Output = (
                            usize,
                            Result<
                                Result<NodeExecutionOutcome, GraphError>,
                                Box<dyn std::any::Any + Send>,
                            >,
                        ),
                    > + Send
                    + 'a,
            >,
        >;
        let mut pending: FuturesUnordered<CompletionFuture<'_>> = FuturesUnordered::new();
        let mut dispatch = Vec::new();
        let mut fact_error = None;
        for branch_index in 0..record.parallel.as_ref().unwrap().branches.len() {
            let Some(cursor) = record.parallel.as_ref().unwrap().branches[branch_index]
                .cursor
                .clone()
            else {
                continue;
            };
            match self.nodes.completion_fact(&cursor.key).await? {
                CompletionFact::Completed(completion) => {
                    self.settle_parallel_completion(record, branch_index, cursor, completion)
                        .await?;
                }
                CompletionFact::Failed(reason) => {
                    let branch = &mut record.parallel.as_mut().unwrap().branches[branch_index];
                    branch.status = ParallelBranchStatus::Failed;
                    branch.cursor = None;
                    branch.error = Some(reason.clone());
                    record.status = RunStatus::Failed;
                    record.error = Some(format!("{} failed: {reason}", cursor.node_id));
                    self.store.save(record)?;
                    fact_error.get_or_insert(reason);
                }
                CompletionFact::Uncertain(reason) => {
                    record.status = RunStatus::Failed;
                    record.error = Some(format!(
                        "uncertain node result for {}: {reason}",
                        cursor.node_id
                    ));
                    self.store.save(record)?;
                    fact_error.get_or_insert(reason);
                }
                CompletionFact::NotStarted => {
                    dispatch.push((branch_index, cursor));
                }
            }
        }

        // A durable failure or uncertainty in any active branch fences all new
        // work for this activation. Existing facts above are reconciled first.
        if let Some(error) = fact_error {
            record.status = RunStatus::Failed;
            record.error.get_or_insert(error);
            self.store.save(record)?;
            return Ok(false);
        }
        for (branch_index, cursor) in dispatch {
            let request = execution_request(record, &cursor, self.control.cancellation());
            let nodes = self.nodes;
            pending.push(Box::pin(async move {
                let outcome = AssertUnwindSafe(async move { nodes.execute(request).await })
                    .catch_unwind()
                    .await;
                (branch_index, outcome)
            }));
        }

        let mut terminal_error = None;
        let mut stopped = false;
        while let Some((branch_index, outcome)) = pending.next().await {
            let cursor = record.parallel.as_ref().unwrap().branches[branch_index]
                .cursor
                .clone()
                .expect("dispatched branch cursor");
            let outcome = match outcome {
                Ok(Ok(outcome)) => outcome,
                Ok(Err(error)) => {
                    terminal_error.get_or_insert(error.to_string());
                    continue;
                }
                Err(_) => {
                    terminal_error.get_or_insert_with(|| {
                        format!("node executor panicked at {}", cursor.node_id)
                    });
                    continue;
                }
            };
            match outcome {
                NodeExecutionOutcome::Completed(completion) => {
                    self.settle_parallel_completion(record, branch_index, cursor, completion)
                        .await?;
                }
                NodeExecutionOutcome::BudgetExhausted { .. } => {
                    record.status = RunStatus::BudgetStopped;
                    self.store.save(record)?;
                    stopped = true;
                }
                NodeExecutionOutcome::Cancelled => {
                    record.status = RunStatus::Stopped;
                    self.store.save(record)?;
                    stopped = true;
                }
                NodeExecutionOutcome::Failed { reason } => {
                    let branch = &mut record.parallel.as_mut().unwrap().branches[branch_index];
                    branch.status = ParallelBranchStatus::Failed;
                    branch.cursor = None;
                    branch.error = Some(reason.clone());
                    record.status = RunStatus::Failed;
                    record.error = Some(format!("{} failed: {reason}", cursor.node_id));
                    self.store.save(record)?;
                    terminal_error.get_or_insert(reason);
                }
            }
        }
        if let Some(error) = terminal_error {
            if record.status != RunStatus::Failed {
                record.status = RunStatus::Failed;
                record.error = Some(error);
            }
            self.store.save(record)?;
            return Ok(false);
        }
        if stopped {
            self.store.save(record)?;
            return Ok(false);
        }
        Ok(true)
    }

    async fn prepare_parallel_branch(
        &self,
        record: &mut GraphRunRecord,
        branch_index: usize,
    ) -> Result<ParallelPreparation, GraphError> {
        let activation = record.parallel.as_ref().expect("active parallel region");
        let branch = &activation.branches[branch_index];
        let node_id = branch.nodes[branch.next_index].clone();
        let node = record
            .snapshot
            .nodes
            .iter()
            .find(|candidate| candidate.id == node_id)
            .cloned()
            .expect("validated parallel branch node");

        let scope = scope_of(&node.id);
        let prior_sequence = record
            .results
            .get(&node.id)
            .and_then(|results| results.last())
            .map(|result| result.sequence)
            .unwrap_or(0);
        let enters_scope = !scope.is_empty()
            && record
                .snapshot
                .edges
                .iter()
                .filter(|edge| {
                    edge.to_node == node.id && !edge.from_node.starts_with(&format!("{scope}/"))
                })
                .any(|edge| {
                    record
                        .decided
                        .get(&edge_key(&edge.from_node, &edge.to_node))
                        .is_some_and(|decision| {
                            decision.selected && decision.sequence > prior_sequence
                        })
                })
            && !parallel_scope_started(record, branch_index, &scope);

        if enters_scope && let Some(limit) = record.snapshot.module_rounds.get(&scope).copied() {
            let next = record.module_activations.get(&scope).copied().unwrap_or(0) + 1;
            if next > u64::from(limit) {
                record.ceased.insert(format!("{scope}@{limit}"));
                refuse_module_activation(record, &scope, &node.id)?;
                return Ok(ParallelPreparation::Ceased(format!(
                    "parallel branch `{}` exceeded module activation ceiling `{scope}@{limit}`",
                    node.id
                )));
            }
            record.module_activations.insert(scope.clone(), next);
            let prefix = format!("{scope}/");
            for member in &record.snapshot.nodes {
                if member.id.starts_with(&prefix) {
                    record.passes.remove(&member.id);
                }
            }
            for nested in record
                .module_activations
                .keys()
                .filter(|nested| nested.starts_with(&prefix))
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
            self.refuse_edges(record, &node.id)?;
            return Ok(ParallelPreparation::Ceased(format!(
                "parallel branch `{}` exceeded node round ceiling `{}`",
                node.id, limit
            )));
        }

        let input_commits = expected_input_commits(record, &node.id)?;
        let mut resolved = Vec::with_capacity(input_commits.len());
        for commit in &input_commits {
            resolved.push(self.artifacts.resolve(commit).await?);
        }
        let prepared_input = serde_json::json!({
            "input":record.input,
            "committed_inputs":resolved,
            "parallel_activation":activation.activation_id,
            "branch_id":branch.branch_id,
        });
        let invocation = record.invocations.get(&node.id).copied().unwrap_or(0) + 1;
        let key = InvocationKey {
            run_id: record.run_id.clone(),
            graph_digest: record.graph_digest.clone(),
            node_id: node.id.clone(),
            invocation,
        };
        Ok(ParallelPreparation::Cursor(RunCursor {
            node_id: node.id,
            key,
            input_commits,
            prepared_input,
        }))
    }

    async fn settle_parallel_completion(
        &self,
        record: &mut GraphRunRecord,
        branch_index: usize,
        cursor: RunCursor,
        completion: NodeCompletion,
    ) -> Result<(), GraphError> {
        let routes = record
            .snapshot
            .edges
            .iter()
            .filter(|edge| edge.from_node == cursor.node_id)
            .map(|edge| edge.to_node.clone())
            .collect::<Vec<_>>();
        if routes.len() != 1 {
            return Err(GraphError::InvalidSnapshot(format!(
                "parallel branch node `{}` must have one outgoing route",
                cursor.node_id
            )));
        }
        select_route(&completion, &routes)?;
        let commit = self.artifacts.freeze(&cursor.key, &completion).await?;
        record.sequence += 1;
        let result_sequence = record.sequence;
        record
            .results
            .entry(cursor.node_id.clone())
            .or_default()
            .push(RunResult {
                node_id: cursor.node_id.clone(),
                key: cursor.key.clone(),
                completion,
                commit: commit.clone(),
                sequence: result_sequence,
            });
        record.sequence += 1;
        record.decided.insert(
            edge_key(&cursor.node_id, &routes[0]),
            EdgeDecision {
                selected: true,
                sequence: record.sequence,
                source_invocation: cursor.key.invocation,
                result_sequence,
            },
        );
        let branch = &mut record.parallel.as_mut().unwrap().branches[branch_index];
        branch.completed.push(commit);
        branch.next_index += 1;
        branch.cursor = None;
        branch.status = if branch.next_index == branch.nodes.len() {
            ParallelBranchStatus::Completed
        } else {
            ParallelBranchStatus::Ready
        };
        self.store.save(record)?;
        Ok(())
    }
}
