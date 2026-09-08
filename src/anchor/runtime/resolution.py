"""Rebuild one node's canonical execution input from durable projections."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any
from uuid import UUID

from anchor.domain.graph import GraphNode, GraphVersion
from anchor.domain.models import Task
from anchor.runtime.artifacts import ArtifactStore
from anchor.runtime.context import build_input_snapshot


PREDECESSOR_TEXT_LIMIT = 4000


@dataclass(frozen=True)
class ResolvedNodeContext:
    task: Task
    graph: GraphVersion
    node: GraphNode
    snapshot: dict[str, Any]


def resolve_node_context(
    store,
    run_id: UUID,
    node_id: str,
    artifacts: ArtifactStore | None = None,
) -> ResolvedNodeContext:
    run = store.get_run(run_id)
    if run is None:
        raise KeyError(run_id)
    task = store.get_task(run.task_id)
    graph = store.get_graph_version(run.graph_version_id)
    if task is None or graph is None:
        raise KeyError(run_id)
    node = next((item for item in graph.definition.nodes if item.id == node_id), None)
    if node is None:
        raise KeyError(node_id)
    dispatch = next(
        (item for item in store.accepted_dispatches(1000) if item.run_id == run_id),
        None,
    )
    run_inputs = dispatch.inputs if dispatch is not None else {}
    node_runs = store.list_node_runs(run_id)
    current_node_run = None
    for item in node_runs:
        if item.node_id != node_id:
            continue
        if current_node_run is None or item.attempt > current_node_run.attempt:
            current_node_run = item
    outputs: dict[str, object] = {}
    for item in node_runs:
        if not item.output_ref:
            continue
        text = item.output_ref
        if artifacts is not None and text.startswith("artifact://"):
            text = artifacts.get_text(text)
            if node.metadata.get("context_mode") != "full" and len(text) > PREDECESSOR_TEXT_LIMIT:
                text = text[:PREDECESSOR_TEXT_LIMIT] + (
                    f"\n[truncated:{len(text) - PREDECESSOR_TEXT_LIMIT}-chars]"
                )
            try:
                outputs[item.node_id] = json.loads(text)
            except json.JSONDecodeError:
                outputs[item.node_id] = text
        else:
            outputs[item.node_id] = text

    persisted = None
    get_snapshot = getattr(store, "get_context_snapshot", None)
    if current_node_run is not None and get_snapshot is not None:
        persisted = get_snapshot(current_node_run.id)
    if persisted is not None:
        snapshot = persisted.snapshot
    else:
        list_decisions = getattr(store, "list_edge_decisions", None)
        decisions = list_decisions(run_id) if list_decisions is not None else []
        incoming_decisions = [item for item in decisions if item.target_node_id == node_id]
        if incoming_decisions:
            # Latest evaluation per edge, mirroring plan_propagation.
            newest: dict[int, object] = {}
            for item in incoming_decisions:
                prior = newest.get(item.edge_index)
                if (prior is None or (item.source_attempt, item.decided_at)
                        >= (prior.source_attempt, prior.decided_at)):
                    newest[item.edge_index] = item
            selected = {index: item for index, item in newest.items() if item.selected}
            # Loop iterations can map the same target key from successive
            # edges; the newest decision wins deterministically per key. The
            # pure builder still fails closed on direct conflicts.
            winners: dict[str, tuple] = {}
            for index in sorted(selected):
                edge = graph.definition.edges[index]
                marker = (selected[index].decided_at, selected[index].source_attempt, index)
                for key in edge.input_mapping:
                    if key not in winners or marker > winners[key][0]:
                        winners[key] = (marker, index)
            edges = []
            for index in sorted(selected):
                edge = graph.definition.edges[index]
                kept = {key: value for key, value in edge.input_mapping.items()
                        if winners.get(key, (None, -1))[1] == index}
                if kept or not edge.input_mapping:
                    edges.append(edge.model_copy(update={"input_mapping": kept}))
        else:
            # Compatibility for Runs made ready before edge-decision storage.
            edges = [
                edge for edge in graph.definition.edges
                if edge.target == node_id and edge.condition is None
            ]
        snapshot = build_input_snapshot(
            run_inputs=run_inputs,
            edges=edges,
            predecessor_outputs=outputs,
        )
    return ResolvedNodeContext(task=task, graph=graph, node=node, snapshot=snapshot)
