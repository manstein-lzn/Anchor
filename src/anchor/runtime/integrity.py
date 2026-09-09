"""Mechanical anti-drift gate: deterministic integrity checks over canonical state.

This is layer 1 of the anti-drift design (see PRODUCT_VISION.md, Runtime
kernel): reference integrity, not semantic judgment. Every check is a pure,
read-only assertion over persisted facts — snapshot hashes, generation order,
pinned-version identity, edge-decision structure, and predecessor evidence
existence. No model call, no guessed numeric budget.

Fail-closed contract: `check_run` returns issues; it never mutates state. The
Agent prompt resolver refuses to build a prompt when issues exist, so no model
call consumes a corrupt context. The lease is left for supervision, exactly
like a model transport failure.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from uuid import UUID

from anchor.domain.context import input_hash
from anchor.domain.graph import GraphVersion


@dataclass(frozen=True)
class IntegrityIssue:
    code: str
    message: str
    node_id: str | None = None
    generation: int | None = None


class IntegrityError(RuntimeError):
    """Raised when canonical state fails the mechanical integrity gate."""

    def __init__(self, issues: tuple[IntegrityIssue, ...]) -> None:
        self.issues = issues
        super().__init__("; ".join(
            f"{issue.code}" + (f"({issue.node_id})" if issue.node_id else "")
            for issue in issues
        ))


def _artifact_digest(ref: str | None) -> str | None:
    if not ref or not ref.startswith("artifact://sha256/"):
        return None
    return ref.split("/", 3)[-1]


def check_run(store, run_id: UUID,
              read_artifact: Callable[[str], str] | None = None,
              ) -> tuple[IntegrityIssue, ...]:
    """Check the mechanical integrity of one run. Read-only; returns issues."""
    issues: list[IntegrityIssue] = []
    run = store.get_run(run_id)
    if run is None:
        return (IntegrityIssue(code="unknown_run", message="run does not exist"),)
    task = store.get_task(run.task_id)
    if task is None:
        issues.append(IntegrityIssue(code="missing_task", message="run task is missing"))
    version = store.get_graph_version(run.graph_version_id)
    if version is None:
        issues.append(IntegrityIssue(code="missing_graph_version",
                                     message="pinned graph version is missing"))
        return tuple(issues)
    try:
        republished = GraphVersion.publish(version.definition, version.version)
    except ValueError as exc:
        issues.append(IntegrityIssue(code="invalid_pin", message=f"pinned definition is invalid: {exc}"))
        return tuple(issues)
    if republished.content_hash != version.content_hash:
        issues.append(IntegrityIssue(code="pin_hash_mismatch",
                                     message="pinned definition does not match its content hash"))

    snapshots = store.list_context_snapshots(run_id)
    generations = sorted(item.generation for item in snapshots)
    if generations != list(range(1, len(snapshots) + 1)):
        issues.append(IntegrityIssue(code="generation_gap",
                                     message=f"context generations are not dense from 1: {generations}"))
    for item in snapshots:
        if item.input_hash != input_hash(item.snapshot):
            issues.append(IntegrityIssue(code="snapshot_hash_mismatch",
                                         message="snapshot content does not match its hash",
                                         generation=item.generation))
    if snapshots and run.context_generation != generations[-1]:
        issues.append(IntegrityIssue(code="generation_stale",
                                     message="run context generation does not match latest snapshot"))
    by_node_run = {(item.node_run_id, item.generation): item for item in snapshots}

    nodes = store.list_node_runs(run_id)
    completed_digests: set[str] = set()
    for node in nodes:
        digest = _artifact_digest(node.output_ref)
        if node.status.value == "completed" and digest:
            completed_digests.add(digest)
        if node.status.value != "completed" or node.context_generation <= 0:
            continue
        key = (node.id, node.context_generation)
        snapshot = by_node_run.get(key)
        if snapshot is None:
            issues.append(IntegrityIssue(code="missing_snapshot",
                                         message="completed node has no context snapshot",
                                         node_id=node.node_id, generation=node.context_generation))
            continue
        if node.input_hash and node.input_hash != snapshot.input_hash:
            issues.append(IntegrityIssue(code="node_hash_mismatch",
                                         message="node input hash does not match its snapshot",
                                         node_id=node.node_id, generation=node.context_generation))

    edges = version.definition.edges
    for decision in store.list_edge_decisions(run_id):
        if decision.run_id != run_id:
            issues.append(IntegrityIssue(code="decision_run_mismatch",
                                         message="edge decision belongs to another run"))
            continue
        if decision.edge_index >= len(edges):
            issues.append(IntegrityIssue(code="decision_index_out_of_range",
                                         message="edge decision index is outside the pinned graph"))
            continue
        edge = edges[decision.edge_index]
        if (decision.source_node_id, decision.target_node_id, decision.condition) != (
                edge.source, edge.target, edge.condition):
            issues.append(IntegrityIssue(code="decision_pin_mismatch",
                                         message="edge decision does not match the pinned graph",
                                         node_id=decision.target_node_id))

    if read_artifact is not None:
        for node in nodes:
            if node.status.value != "completed" or not node.output_ref:
                continue
            if not node.output_ref.startswith("artifact://"):
                continue
            try:
                read_artifact(node.output_ref)
            except (OSError, ValueError) as exc:
                issues.append(IntegrityIssue(code="evidence_missing",
                                             message=f"predecessor evidence unreadable: {exc}",
                                             node_id=node.node_id))

    snapshot_hashes = {item.input_hash for item in snapshots}
    for record in store.list_verifications(run_id):
        if record.verified_context_hash not in snapshot_hashes:
            issues.append(IntegrityIssue(code="verification_context_orphan",
                                         message="verification binds an unknown context hash",
                                         node_id=record.node_id))
        unknown = [digest for digest in record.verified_artifact_hashes
                   if digest not in completed_digests]
        if unknown:
            issues.append(IntegrityIssue(code="verification_artifact_orphan",
                                         message="verification binds unknown artifact hashes",
                                         node_id=record.node_id))
    return tuple(issues)


def require_clean(store, run_id: UUID,
                  read_artifact: Callable[[str], str] | None = None) -> None:
    """Raise IntegrityError unless the run passes every mechanical check."""
    issues = check_run(store, run_id, read_artifact=read_artifact)
    if issues:
        raise IntegrityError(issues)
