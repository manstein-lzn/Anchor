"""Deterministic branch decisions and downstream transition planning."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Iterable, Mapping
from uuid import UUID

from anchor.domain.conditions import EVALUATOR_NAME, EVALUATOR_VERSION, ConditionError, evaluate_condition
from anchor.domain.context import input_hash
from anchor.domain.graph import GraphVersion
from anchor.domain.models import EdgeDecision, EdgeDecisionReason, NodeRun, NodeRunStatus


ROUTING_EVALUATOR = "anchor-routing"
ROUTING_EVALUATOR_VERSION = "1"


class PropagationError(ValueError):
    pass


class RoutingDecisionError(PropagationError):
    """A completed source result cannot produce valid outgoing decisions."""


@dataclass(frozen=True)
class PropagationPlan:
    ready_node_ids: tuple[str, ...]
    skipped_node_ids: tuple[str, ...]
    inferred_decisions: tuple[EdgeDecision, ...]


def decide_outgoing_edges(
    version: GraphVersion,
    *,
    run_id: UUID,
    source_node_id: str,
    evaluation_context: Mapping[str, Any] | None,
    evidence_ref: str,
    source_attempt: int = 0,
) -> tuple[EdgeDecision, ...]:
    """Resolve every outgoing edge when its source completes.

    Conditional edges require the canonical evaluator context. Unconditional
    edges are also persisted so joins can distinguish selected, rejected and
    unresolved predecessors without inference from mutable runtime state.
    """
    known_nodes = {node.id for node in version.definition.nodes}
    if source_node_id not in known_nodes:
        raise PropagationError("source node does not belong to graph version")
    outgoing = [
        (index, edge)
        for index, edge in enumerate(version.definition.edges)
        if edge.source == source_node_id
    ]
    if any(edge.condition is not None for _, edge in outgoing) and evaluation_context is None:
        raise RoutingDecisionError("conditional outgoing edges require an evaluation context")
    context_hash = input_hash(evaluation_context) if evaluation_context is not None else None
    decisions: list[EdgeDecision] = []
    for index, edge in outgoing:
        if edge.condition is None:
            selected = True
            reason = EdgeDecisionReason.UNCONDITIONAL
            evaluator = ROUTING_EVALUATOR
            evaluator_version = ROUTING_EVALUATOR_VERSION
            decision_context_hash = None
        else:
            try:
                selected = evaluate_condition(edge.condition, evaluation_context or {})
            except ConditionError as exc:
                raise RoutingDecisionError(str(exc)) from exc
            reason = (
                EdgeDecisionReason.CONDITION_TRUE
                if selected
                else EdgeDecisionReason.CONDITION_FALSE
            )
            evaluator = EVALUATOR_NAME
            evaluator_version = EVALUATOR_VERSION
            decision_context_hash = context_hash
        decisions.append(EdgeDecision(
            run_id=run_id,
            edge_index=index,
            source_attempt=source_attempt,
            source_node_id=edge.source,
            target_node_id=edge.target,
            selected=selected,
            reason=reason,
            condition=edge.condition,
            evaluator=evaluator,
            evaluator_version=evaluator_version,
            evaluation_context_hash=decision_context_hash,
            evidence_ref=evidence_ref,
        ))
    return tuple(decisions)


def _validate_decision(version: GraphVersion, run_id: UUID, decision: EdgeDecision) -> None:
    if decision.run_id != run_id:
        raise PropagationError("edge decision belongs to another run")
    if decision.edge_index >= len(version.definition.edges):
        raise PropagationError("edge decision index does not belong to graph version")
    edge = version.definition.edges[decision.edge_index]
    if (decision.source_node_id, decision.target_node_id, decision.condition) != (
        edge.source, edge.target, edge.condition,
    ):
        raise PropagationError("edge decision does not match pinned graph version")


def cycle_back_edges(version: GraphVersion) -> set[int]:
    """Edge indexes that close a structural cycle (loop back-edges).

    A pending node ignores undecided back-edges from sources that never
    completed: on first arrival the cycle has not started yet. Once the
    source completes, its decisions exist and gate normally. Acyclic graphs
    have no back-edges, so their join semantics are unchanged."""
    try:
        entry = version.definition.resolved_entry_node_id()
    except ValueError:
        return set()
    adjacency: dict[str, list[tuple[int, str]]] = {}
    for index, edge in enumerate(version.definition.edges):
        adjacency.setdefault(edge.source, []).append((index, edge.target))
    back: set[int] = set()
    color: dict[str, int] = {entry: 1}
    stack = [(entry, iter(adjacency.get(entry, [])))]
    while stack:
        node, edges_iter = stack[-1]
        advanced = False
        for index, target in edges_iter:
            if color.get(target) == 1:
                back.add(index)
            elif color.get(target) is None:
                color[target] = 1
                stack.append((target, iter(adjacency.get(target, []))))
                advanced = True
                break
        if not advanced:
            color[node] = 2
            stack.pop()
    return back


def plan_propagation(
    version: GraphVersion,
    node_runs: Iterable[NodeRun],
    edge_decisions: Iterable[EdgeDecision],
) -> PropagationPlan:
    """Plan ready/skipped nodes after applying durable edge decisions.

    A node is ready only when every incoming edge is resolved and at least one
    is selected. A node with no selected incoming edge is skipped; its outgoing
    edges become not-selected, which can cascade through an unchosen branch
    until a selected join becomes ready.
    """
    runs: dict[str, NodeRun] = {}
    for node in node_runs:
        prior = runs.get(node.node_id)
        if prior is None or node.attempt > prior.attempt:
            runs[node.node_id] = node
    known = {node.id for node in version.definition.nodes}
    if set(runs) != known:
        raise PropagationError("node runs do not match graph version")
    run_ids = {node.run_id for node in runs.values()}
    if len(run_ids) != 1:
        raise PropagationError("node runs do not belong to one run")
    run_id = next(iter(run_ids))
    decisions: dict[tuple[int, int], EdgeDecision] = {}
    for decision in edge_decisions:
        _validate_decision(version, run_id, decision)
        key = (decision.edge_index, decision.source_attempt)
        prior = decisions.get(key)
        if prior is not None and prior != decision:
            raise PropagationError("conflicting edge decisions")
        decisions[key] = decision
    # Readiness uses the latest evaluation per edge; earlier iterations
    # remain as immutable evidence but no longer gate their targets.
    latest: dict[int, EdgeDecision] = {}
    for (index, _), decision in sorted(decisions.items()):
        prior = latest.get(index)
        if prior is None or decision.source_attempt >= prior.source_attempt:
            latest[index] = decision
    decisions: dict[int, EdgeDecision] = latest

    status = {node_id: node.status for node_id, node in runs.items()}
    incoming: dict[str, list[int]] = {node_id: [] for node_id in known}
    outgoing: dict[str, list[int]] = {node_id: [] for node_id in known}
    for index, edge in enumerate(version.definition.edges):
        incoming[edge.target].append(index)
        outgoing[edge.source].append(index)

    back_edges = cycle_back_edges(version)
    completed_once = {node.node_id for node in node_runs
                      if node.status is NodeRunStatus.COMPLETED}
    inferred: list[EdgeDecision] = []
    ready: list[str] = []
    skipped: list[str] = []
    changed = True
    while changed:
        changed = False
        # A skipped node cannot activate any outgoing edge. Persist this fact
        # before considering downstream nodes in the same deterministic pass.
        for node in version.definition.nodes:
            if status[node.id] is not NodeRunStatus.SKIPPED:
                continue
            for edge_index in outgoing[node.id]:
                if edge_index in decisions:
                    continue
                edge = version.definition.edges[edge_index]
                decision = EdgeDecision(
                    run_id=run_id,
                    edge_index=edge_index,
                    source_attempt=runs[edge.source].attempt,
                    source_node_id=edge.source,
                    target_node_id=edge.target,
                    selected=False,
                    reason=EdgeDecisionReason.UPSTREAM_SKIPPED,
                    condition=edge.condition,
                    evaluator=ROUTING_EVALUATOR,
                    evaluator_version=ROUTING_EVALUATOR_VERSION,
                )
                decisions[edge_index] = decision
                inferred.append(decision)
                changed = True

        for node in version.definition.nodes:
            if status[node.id] not in (NodeRunStatus.PENDING, NodeRunStatus.SKIPPED):
                continue
            edge_indexes = incoming[node.id]
            if not edge_indexes:
                continue
            blocking = [
                index for index in edge_indexes if index not in decisions
                and (index not in back_edges or version.definition.edges[index].source
                     in completed_once)
            ]
            if blocking:
                continue
            selected = [decisions[index] for index in edge_indexes
                        if index in decisions and decisions[index].selected]
            if not selected:
                if status[node.id] is not NodeRunStatus.PENDING:
                    continue
                if any(index not in decisions for index in edge_indexes):
                    continue
                status[node.id] = NodeRunStatus.SKIPPED
                skipped.append(node.id)
                changed = True
                continue
            if not all(status[item.source_node_id] is NodeRunStatus.COMPLETED for item in selected):
                raise PropagationError("selected edge source is not completed")
            status[node.id] = NodeRunStatus.READY
            ready.append(node.id)
            changed = True

    return PropagationPlan(tuple(ready), tuple(skipped), tuple(inferred))


def plan_ready_nodes(
    version: GraphVersion,
    node_runs: Iterable[NodeRun],
    edge_decisions: Iterable[EdgeDecision] = (),
) -> tuple[str, ...]:
    """Backward-compatible ready-node view over the richer propagation plan."""
    nodes = tuple(node_runs)
    supplied = tuple(edge_decisions)
    if not supplied and nodes:
        run_id = nodes[0].run_id
        inferred: list[EdgeDecision] = []
        for node in nodes:
            if node.status is not NodeRunStatus.COMPLETED:
                continue
            for index, edge in enumerate(version.definition.edges):
                if edge.source != node.node_id or edge.condition is not None:
                    continue
                inferred.append(EdgeDecision(
                    run_id=run_id,
                    edge_index=index,
                    source_node_id=edge.source,
                    target_node_id=edge.target,
                    selected=True,
                    reason=EdgeDecisionReason.UNCONDITIONAL,
                    condition=None,
                    evaluator=ROUTING_EVALUATOR,
                    evaluator_version=ROUTING_EVALUATOR_VERSION,
                    evidence_ref=node.output_ref or "state://completed",
                ))
        supplied = tuple(inferred)
    return plan_propagation(version, nodes, supplied).ready_node_ids
