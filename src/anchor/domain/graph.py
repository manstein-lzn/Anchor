from __future__ import annotations

import hashlib
import json
from collections.abc import Callable
from graphlib import CycleError, TopologicalSorter
from datetime import datetime
from enum import StrEnum
from uuid import UUID, uuid4

from pydantic import BaseModel, ConfigDict, Field, model_validator

from .conditions import ConditionError, validate_condition
from .models import utc_now


class NodeType(StrEnum):
    AGENT = "agent"
    TOOL = "tool"
    ROUTER = "router"
    PARALLEL = "parallel"
    JOIN = "join"
    VERIFIER = "verifier"
    APPROVAL = "approval"
    WAIT_FOR_EVENT = "wait_for_event"
    HUMAN_TASK = "human_task"
    ARTIFACT = "artifact"
    LOOP = "loop"
    SUBGRAPH = "subgraph"


CONTROL_NODE_TYPES = frozenset({
    NodeType.ROUTER,
    NodeType.PARALLEL,
    NodeType.JOIN,
    NodeType.ARTIFACT,
    NodeType.LOOP,
    NodeType.TOOL,
})

RECOVERABLE_CONTROL_TYPES = frozenset({
    NodeType.ROUTER,
    NodeType.PARALLEL,
    NodeType.JOIN,
    NodeType.ARTIFACT,
    NodeType.LOOP,
})
"""Deterministic control types whose leases may be operator-recovered.

Tool nodes are claimed by the control worker but never recovered through
the lease path: an interrupted tool may have caused an external side
effect, so only the operation reconciliation protocol may resolve it."""


class TriggerType(StrEnum):
    MANUAL = "manual"
    CRON = "cron"
    INTERVAL = "interval"
    WEBHOOK = "webhook"
    INTERNAL_EVENT = "internal_event"


class GraphVersionStatus(StrEnum):
    PUBLISHED = "published"
    ARCHIVED = "archived"


class GraphModel(BaseModel):
    model_config = ConfigDict(extra="forbid")


class GraphNode(GraphModel):
    id: str = Field(min_length=1, max_length=64, pattern=r"^[A-Za-z][A-Za-z0-9_-]*$")
    type: NodeType
    name: str = Field(min_length=1, max_length=200)
    agent_ref: str | None = None
    tool_ref: str | None = None
    verifier_ref: str | None = None
    subgraph_version_id: str | None = None
    input_schema: str | None = None
    output_schema: str | None = None
    retry_policy: str = "default"
    timeout_seconds: int | None = Field(default=None, gt=0)
    approval_required: bool = False
    exit_condition: str | None = Field(default=None, max_length=1000)
    progress_signal: str | None = Field(default=None, max_length=1000)
    metadata: dict[str, str] = Field(default_factory=dict)

    @model_validator(mode="after")
    def validate_reference(self) -> GraphNode:
        required = {
            NodeType.AGENT: ("agent_ref", self.agent_ref),
            NodeType.TOOL: ("tool_ref", self.tool_ref),
            NodeType.VERIFIER: ("verifier_ref", self.verifier_ref),
            NodeType.SUBGRAPH: ("subgraph_version_id", self.subgraph_version_id),
        }.get(self.type)
        if required is not None and not required[1]:
            raise ValueError(f"{self.type.value} node requires {required[0]}")
        if self.type is NodeType.LOOP and not self.exit_condition:
            raise ValueError("loop node requires exit_condition")
        return self


class GraphEdge(GraphModel):
    source: str = Field(min_length=1, max_length=64)
    target: str = Field(min_length=1, max_length=64)
    condition: str | None = Field(default=None, max_length=1000)
    input_mapping: dict[str, str] = Field(default_factory=dict)


class GraphDefinition(GraphModel):
    graph_id: str = Field(min_length=1, max_length=128)
    name: str = Field(min_length=1, max_length=200)
    nodes: list[GraphNode] = Field(min_length=1)
    edges: list[GraphEdge] = Field(default_factory=list)
    entry_node_id: str | None = None
    metadata: dict[str, str] = Field(default_factory=dict)

    @model_validator(mode="after")
    def validate_execution_budgets(self):
        # Optional operator policy only. Absence means unbounded; these are not
        # required fields and must never be silently defaulted by the runtime.
        for key, maximum in (("run_timeout_seconds", 86400), ("max_rounds", 100)):
            if key in self.metadata:
                value = int(self.metadata[key])
                if not 1 <= value <= maximum:
                    raise ValueError(f"{key} must be between 1 and {maximum}")
        return self

    def resolved_entry_node_id(self) -> str:
        """Return the explicit entry or the only structural root.

        Callers that execute a graph must first require successful static
        validation; this helper intentionally refuses ambiguous draft graphs.
        """
        if self.entry_node_id is not None:
            if self.entry_node_id not in {node.id for node in self.nodes}:
                raise ValueError("entry node does not exist")
            return self.entry_node_id
        targets = {edge.target for edge in self.edges}
        roots = [node.id for node in self.nodes if node.id not in targets]
        if len(roots) != 1:
            raise ValueError("graph does not have exactly one inferred entry")
        return roots[0]


class GraphValidationIssue(GraphModel):
    code: str
    message: str
    node_id: str | None = None
    edge_index: int | None = None


class GraphValidationResult(GraphModel):
    valid: bool
    issues: list[GraphValidationIssue] = Field(default_factory=list)


class GraphValidator:
    """Deterministic publication checks for the first Graph IR version."""

    def validate(self, graph: GraphDefinition) -> GraphValidationResult:
        issues: list[GraphValidationIssue] = []
        node_ids = [node.id for node in graph.nodes]
        node_set = set(node_ids)
        for node_id in sorted({value for value in node_ids if node_ids.count(value) > 1}):
            issues.append(GraphValidationIssue(code="duplicate_node", message=f"duplicate node id: {node_id}", node_id=node_id))

        adjacency: dict[str, list[str]] = {node_id: [] for node_id in node_set}
        indegree: dict[str, int] = {node_id: 0 for node_id in node_set}
        edge_keys: set[tuple[str, str, str | None]] = set()
        for index, edge in enumerate(graph.edges):
            if edge.source not in node_set:
                issues.append(GraphValidationIssue(code="unknown_source", message=f"unknown source node: {edge.source}", edge_index=index))
                continue
            if edge.target not in node_set:
                issues.append(GraphValidationIssue(code="unknown_target", message=f"unknown target node: {edge.target}", edge_index=index))
                continue
            key = (edge.source, edge.target, edge.condition)
            if key in edge_keys:
                issues.append(GraphValidationIssue(code="duplicate_edge", message=f"duplicate edge: {edge.source} -> {edge.target}", edge_index=index))
            edge_keys.add(key)
            adjacency[edge.source].append(edge.target)
            indegree[edge.target] += 1
            if edge.condition is not None:
                try:
                    validate_condition(edge.condition)
                except ConditionError as exc:
                    issues.append(GraphValidationIssue(
                        code="invalid_edge_condition", message=str(exc), edge_index=index,
                    ))

        roots = sorted(node_id for node_id, degree in indegree.items() if degree == 0)
        if graph.entry_node_id is not None and graph.entry_node_id not in node_set:
            issues.append(GraphValidationIssue(code="unknown_entry", message=f"unknown entry node: {graph.entry_node_id}", node_id=graph.entry_node_id))
            entry = None
        elif graph.entry_node_id is not None:
            entry = graph.entry_node_id
        elif len(roots) != 1:
            issues.append(GraphValidationIssue(code="entry_count", message=f"graph must have exactly one entry node; found {len(roots)}"))
            entry = roots[0] if roots else None
        else:
            entry = roots[0]

        terminals = [node_id for node_id, targets in adjacency.items() if not targets]
        if not terminals:
            issues.append(GraphValidationIssue(code="no_terminal", message="graph must have at least one terminal node"))

        if entry is not None:
            reachable: set[str] = set()
            stack = [entry]
            while stack:
                current = stack.pop()
                if current in reachable:
                    continue
                reachable.add(current)
                stack.extend(adjacency[current])
            for node_id in sorted(node_set - reachable):
                issues.append(GraphValidationIssue(code="unreachable_node", message=f"node is unreachable from entry: {node_id}", node_id=node_id))

        # Every cycle must cross an explicit loop boundary, not merely have a
        # loop node somewhere upstream. Use the standard graph algorithm.
        ordinary = {node.id for node in graph.nodes if node.type is not NodeType.LOOP}
        dependencies = {
            source: [target for target in adjacency[source] if target in ordinary]
            for source in sorted(ordinary)
        }
        try:
            tuple(TopologicalSorter(dependencies).static_order())
        except CycleError:
            issues.append(GraphValidationIssue(code="cycle", message="cycle bypasses an explicit loop node"))

        reverse: dict[str, list[str]] = {node_id: [] for node_id in node_set}
        for source, targets in adjacency.items():
            for target in targets:
                reverse[target].append(source)
        can_finish: set[str] = set()
        pending = list(terminals)
        while pending:
            current = pending.pop()
            if current not in can_finish:
                can_finish.add(current)
                pending.extend(reverse[current])
        for node_id in sorted(node_set - can_finish):
            issues.append(GraphValidationIssue(code="no_exit_path", message="node has no structural path to a terminal", node_id=node_id))

        return GraphValidationResult(valid=not issues, issues=issues)


class GraphVersion(GraphModel):
    graph_version_id: UUID = Field(default_factory=uuid4)
    graph_id: str
    version: int = Field(gt=0)
    definition: GraphDefinition
    content_hash: str = Field(min_length=64, max_length=64)
    status: GraphVersionStatus = GraphVersionStatus.PUBLISHED
    published_at: datetime = Field(default_factory=utc_now)

    @classmethod
    def publish(cls, definition: GraphDefinition, version: int) -> GraphVersion:
        result = GraphValidator().validate(definition)
        if not result.valid:
            details = "; ".join(issue.message for issue in result.issues)
            raise ValueError(f"graph cannot be published: {details}")
        canonical = json.dumps(definition.model_dump(mode="json"), sort_keys=True, separators=(",", ":"))
        content_hash = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
        return cls(graph_id=definition.graph_id, version=version, definition=definition.model_copy(deep=True), content_hash=content_hash)


class Trigger(GraphModel):
    id: UUID = Field(default_factory=uuid4)
    graph_version_id: UUID
    type: TriggerType
    enabled: bool = True
    cron: str | None = None
    interval_seconds: int | None = Field(default=None, gt=0)
    event_type: str | None = None
    timezone: str = "UTC"
    filter_expression: str | None = Field(default=None, max_length=2000)
    idempotency_field: str | None = None
    webhook_secret_ref: str | None = Field(default=None, max_length=200)

    @model_validator(mode="after")
    def validate_trigger_config(self) -> Trigger:
        if self.type is TriggerType.CRON and not self.cron:
            raise ValueError("cron trigger requires cron")
        if self.type is TriggerType.INTERVAL and self.interval_seconds is None:
            raise ValueError("interval trigger requires interval_seconds")
        if self.type in (TriggerType.WEBHOOK, TriggerType.INTERNAL_EVENT) and not self.event_type:
            raise ValueError(f"{self.type.value} trigger requires event_type")
        if self.webhook_secret_ref and self.type is not TriggerType.WEBHOOK:
            raise ValueError("webhook_secret_ref is only valid for webhook triggers")
        return self



SUBGRAPH_SEPARATOR = "__"
SUBGRAPH_EXPANSION_KEY = "anchor.subgraph_expansion"
SUBGRAPH_AUTHORING_HASH_KEY = "anchor.subgraph_authoring_hash"


def _rewrite_outputs_path(path: str, child_ids: set[str], child_subgraphs: set[str],
                           prefix: str, site: str) -> str:
    """Namespace `outputs.<node>` segments addressing child interior nodes."""
    if not path.startswith("outputs."):
        return path
    head, dot, tail = path[len("outputs."):].partition(".")
    if head in child_subgraphs:
        raise ValueError(
            f"edge mapping on site {site!r} addresses subgraph node {head!r}, "
            "which produces no direct output")
    if head in child_ids:
        return f"outputs.{prefix}{head}" + (f".{tail}" if dot else "")
    return path


def expand_subgraphs(definition: GraphDefinition,
                     resolve: Callable[[str], GraphDefinition],
                     ) -> tuple[GraphDefinition, list[dict[str, str]]]:
    """Materialize SUBGRAPH nodes into a flat executable definition.

    Each `subgraph` node pins one immutable Graph Version by ID. The pinned
    child is expanded inline with `{site_id}__` namespaced node IDs, so the
    stored version executes with the existing machinery unchanged: claims,
    propagation, resolution, verification and artifacts all see ordinary
    nodes. Per-site provenance plus the authoring definition hash are
    recorded in metadata for traceability.

    `resolve` maps a version ID string to its stored definition and raises
    `KeyError` for unknown versions. Reference cycles raise `ValueError`;
    with content-pinned versions they cannot arise by construction, so the
    check is purely defensive.
    """
    authoring_hash = hashlib.sha256(json.dumps(
        definition.model_dump(mode="json"), sort_keys=True,
        separators=(",", ":")).encode("utf-8")).hexdigest()

    def expand(nodes: list[GraphNode], edges: list[GraphEdge], entry: str | None,
               prefix: str, stack: tuple[str, ...],
               ) -> tuple[list[GraphNode], list[GraphEdge], list[dict[str, str]],
                          str | None, list[str]]:
        out_nodes: list[GraphNode] = []
        out_edges: list[GraphEdge] = []
        provenance: list[dict[str, str]] = []
        frags: dict[str, tuple[str, list[str], set[str], set[str], str]] = {}
        for node in nodes:
            if node.type is not NodeType.SUBGRAPH:
                out_nodes.append(node.model_copy(update={"id": f"{prefix}{node.id}"}))
                continue
            ref = node.subgraph_version_id or ""
            try:
                child_id = str(UUID(ref))
            except ValueError:
                raise ValueError(
                    f"invalid subgraph_version_id {ref!r} on node {node.id!r}") from None
            if child_id in stack:
                raise ValueError(
                    f"subgraph cycle detected at version {child_id} "
                    f"(via node {node.id!r})")
            try:
                child = resolve(child_id)
            except KeyError:
                raise ValueError(
                    f"unknown subgraph version {child_id} on node {node.id!r}") from None
            site_prefix = f"{prefix}{node.id}{SUBGRAPH_SEPARATOR}"
            child_nodes, child_edges, child_prov, child_entry, _ = expand(
                child.nodes, child.edges, child.entry_node_id, site_prefix,
                stack + (child_id,))
            out_nodes.extend(child_nodes)
            out_edges.extend(child_edges)
            provenance.extend(child_prov)
            provenance.append({"site": f"{prefix}{node.id}", "child_version": child_id,
                               "prefix": site_prefix})
            child_ids = {item.id for item in child.nodes}
            child_subgraphs = {item.id for item in child.nodes
                               if item.type is NodeType.SUBGRAPH}
            child_terminals = sorted(item.id for item in child_nodes
                                     if item.id not in {edge.source for edge in child_edges})
            if child_entry is None:
                raise ValueError(f"subgraph {child_id} has no resolvable entry")
            frags[node.id] = (child_entry, child_terminals, child_ids,
                              child_subgraphs, site_prefix)
        for edge in edges:
            source_frag = frags.get(edge.source)
            target_frag = frags.get(edge.target)
            if source_frag is not None:
                _, _, child_ids, child_subgraphs, site_prefix = source_frag
                mapping = {key: _rewrite_outputs_path(value, child_ids, child_subgraphs,
                                                      site_prefix, edge.source)
                           for key, value in edge.input_mapping.items()}
                sources = source_frag[1]
            else:
                mapping = dict(edge.input_mapping)
                sources = [f"{prefix}{edge.source}"]
            target = target_frag[0] if target_frag is not None else f"{prefix}{edge.target}"
            for source in sources:
                out_edges.append(edge.model_copy(update={"source": source, "target": target,
                                                         "input_mapping": mapping}))
        if entry is not None:
            if entry in frags:
                resolved_entry: str | None = frags[entry][0]
            else:
                resolved_entry = f"{prefix}{entry}"
                if resolved_entry not in {node.id for node in out_nodes}:
                    raise ValueError(f"entry node does not exist: {entry}")
        else:
            roots = sorted(node.id for node in out_nodes
                           if node.id not in {edge.target for edge in out_edges})
            resolved_entry = roots[0] if len(roots) == 1 else None
        terminals = sorted(node.id for node in out_nodes
                           if node.id not in {edge.source for edge in out_edges})
        return out_nodes, out_edges, provenance, resolved_entry, terminals

    flat_nodes, flat_edges, provenance, entry, _ = expand(
        definition.nodes, definition.edges, definition.entry_node_id, "", ())
    metadata = dict(definition.metadata)
    metadata[SUBGRAPH_EXPANSION_KEY] = json.dumps(provenance, sort_keys=True,
                                                  separators=(",", ":"))
    metadata[SUBGRAPH_AUTHORING_HASH_KEY] = authoring_hash
    return (GraphDefinition(graph_id=definition.graph_id, name=definition.name,
                            nodes=flat_nodes, edges=flat_edges,
                            entry_node_id=entry, metadata=metadata),
            provenance)
