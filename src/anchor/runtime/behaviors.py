"""Domain-specific node behavior hooks.

The kernel executes a node generically: claim, call, checkpoint, propagate.
A *behavior* adds domain policy (preflight checks, output schema, rendering)
without the kernel importing any domain module. Behaviors are registered by
reference at the composition root, exactly like model gateways and tools.
"""

from __future__ import annotations

from typing import Protocol


class NodeBehavior(Protocol):
    """Hooks the generic worker/control worker may invoke for one capability."""

    def preflight(self, snapshot: dict, *, store, artifacts, run_id) -> dict | None:
        """Return a synthetic response to skip the model, or None to continue."""
        ...

    def validate_output(self, text: str) -> None:
        """Raise ValueError when a model response violates the domain schema."""
        ...

    def execute_control(self, snapshot: dict, *, store, artifacts, run_id, node_id) -> dict | str:
        """Deterministic control-node result: a dict checkpoint or Markdown text."""
        ...


class NullBehavior:
    """Default for capabilities that declare no behavior."""

    def preflight(self, snapshot: dict, *, store, artifacts, run_id) -> dict | None:
        return None

    def validate_output(self, text: str) -> None:
        """Generic JSON contract: a JSON output must be one JSON object."""
        import json
        try:
            value = json.loads(text)
        except json.JSONDecodeError as exc:
            raise ValueError(str(exc)) from exc
        if not isinstance(value, dict):
            raise ValueError("Output must be a JSON object")

    def execute_control(self, snapshot: dict, *, store, artifacts, run_id, node_id) -> dict | str:
        raise ValueError(f"behavior does not support control execution for node {node_id!r}")


class BehaviorRegistry:
    """Reference -> behavior map populated by the process composition root."""

    def __init__(self, behaviors: dict[str, NodeBehavior] | None = None) -> None:
        self._behaviors: dict[str, NodeBehavior] = dict(behaviors or {})

    def register(self, ref: str, behavior: NodeBehavior) -> None:
        if not ref:
            raise ValueError("behavior reference is required")
        if ref in self._behaviors:
            raise ValueError(f"duplicate behavior reference: {ref}")
        self._behaviors[ref] = behavior

    def get(self, ref: str | None) -> NodeBehavior:
        if not ref:
            return NullBehavior()
        try:
            return self._behaviors[ref]
        except KeyError:
            raise LookupError(f"unknown node behavior: {ref}") from None

    def refs(self) -> tuple[str, ...]:
        return tuple(sorted(self._behaviors))
