"""Deterministic, compact input snapshots for NodeRun execution."""

from __future__ import annotations

from typing import Any, Mapping, Sequence

from anchor.domain.context import canonical_json, input_hash
from anchor.domain.graph import GraphEdge

# Re-exported for the runtime package surface; callers import them from here.
__all__ = ["build_input_snapshot", "canonical_json", "input_hash"]


def build_input_snapshot(*, run_inputs: Mapping[str, Any], edges: Sequence[GraphEdge] | None = None,
                         predecessor_outputs: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Build a JSON-safe snapshot using only declared edge mappings.

    Callers pass only the incoming edges selected by durable routing decisions.
    Their mappings are merged in deterministic (source, target) order. Without
    edges or mappings, only trigger inputs are passed. Mapping values use dotted
    paths into the combined source object and fail closed when absent; two edges
    writing different values to the same target key also fail closed.
    """
    outputs = dict(predecessor_outputs or {})
    source = {"inputs": dict(run_inputs), "outputs": outputs}
    mappings: list[tuple[str, str, str]] = []
    for edge in sorted(edges or [], key=lambda item: (item.source, item.target)):
        for target, path in edge.input_mapping.items():
            mappings.append((target, path, edge.source))
    if not mappings:
        return {"inputs": dict(run_inputs)}
    result: dict[str, Any] = {}
    for target, path, source_id in mappings:
        current: Any = source
        for part in path.split("."):
            if not isinstance(current, Mapping) or part not in current:
                raise KeyError(f"input mapping path is missing: {path}")
            current = current[part]
        if target in result and result[target] != current:
            raise ValueError(f"conflicting input mapping for {target!r} from edge {source_id!r}")
        result[target] = current
    return result
