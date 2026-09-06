"""Deterministic executor for Graph control nodes, including tool calls.

Tool nodes execute through the ledger-backed ToolGateway under an explicit
owner agent (`owner_agent` node metadata) with arguments taken from the
resolved input snapshot. Side-effect tools fail closed; approval-gated
tools are a later milestone, never silent retries.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass
from uuid import UUID, uuid4, uuid5

from anchor.domain.graph import CONTROL_NODE_TYPES, NodeType
from anchor.domain.operations import OperationStatus
from anchor.runtime.artifacts import ArtifactStore
from anchor.runtime.resolution import resolve_node_context
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.tool_gateway import ToolDenied


logger = logging.getLogger("anchor.control_worker")


@dataclass(frozen=True)
class ControlOutcome:
    claim_id: UUID
    node_run_id: UUID
    node_id: str
    output_ref: str


class ControlNodeWorker:
    """Execute Router/Parallel/Join/Artifact nodes without invoking a model."""

    def __init__(self, store, artifacts: ArtifactStore, sink: ArtifactCheckpointSink,
                 tools=None, registry=None) -> None:
        self.store = store
        self.artifacts = artifacts
        self.sink = sink
        self.tools = tools
        self.registry = registry

    async def execute_once(
        self,
        *,
        worker_id: str,
        claim_id: UUID | None = None,
    ) -> ControlOutcome | None:
        claim_id = claim_id or uuid4()
        lease = self.store.claim_ready_control_node(worker_id, claim_id)
        if lease is None:
            return None
        return await self.execute_claimed_once(worker_id=worker_id, lease=lease)

    async def execute_claimed_once(self, *, worker_id: str, lease) -> ControlOutcome:
        resolved = resolve_node_context(
            self.store, lease.run_id, lease.node_id, self.artifacts,
        )
        if resolved.node.type not in CONTROL_NODE_TYPES:
            raise ValueError(f"node {lease.node_id} is not an executable control node")
        if resolved.node.type is NodeType.TOOL:
            return await self.execute_tool_once(worker_id=worker_id, lease=lease,
                                                resolved=resolved)
        # A control node's output is its fully resolved, selected input. This
        # is a deterministic checkpoint, not a fabricated model response.
        output_ref = await self.sink.persist_control_result(
            claim_id=lease.claim_id,
            node_run_id=lease.node_run_id,
            output=resolved.snapshot,
            input_snapshot=resolved.snapshot,
        )
        return ControlOutcome(
            claim_id=lease.claim_id,
            node_run_id=lease.node_run_id,
            node_id=lease.node_id,
            output_ref=output_ref,
        )

    async def execute_tool_once(self, *, worker_id: str, lease, resolved) -> ControlOutcome:
        """Execute one tool node through the ledger-backed gateway."""
        from anchor.domain.conditions import build_condition_context

        node_id = lease.node_id
        if self.tools is None:
            raise RuntimeError(f"tool node {node_id!r} needs a configured tool gateway")
        owner = (resolved.node.metadata or {}).get("owner_agent")
        if not owner:
            self.store.fail_node_and_propagate(
                lease.claim_id, worker_id, error_code="tool_no_owner", phase="tool")
            raise ValueError(f"tool node {node_id!r} declares no owner_agent")
        snapshot = resolved.snapshot if isinstance(resolved.snapshot, dict) else {}
        operation_id = uuid5(lease.claim_id, (resolved.node.tool_ref or "tool") + ":1")
        try:
            result = self.tools.execute(
                lease, agent_ref=owner, tool_ref=resolved.node.tool_ref or "",
                arguments=snapshot, operation_id=operation_id)
        except ToolDenied as exc:
            self.store.fail_node_and_propagate(
                lease.claim_id, worker_id, error_code="tool_denied", phase="tool")
            logger.warning("tool node %s denied [%s]: %s", node_id, exc.code, exc)
            raise ValueError(f"tool node {node_id!r} denied [{exc.code}]") from exc
        if result.status is not OperationStatus.SUCCEEDED or not result.result_ref:
            self.store.fail_node_and_propagate(
                lease.claim_id, worker_id,
                error_code=result.error_code or "tool_failed", phase="tool")
            raise ValueError(
                f"tool node {node_id!r} failed [{result.error_code or 'tool_failed'}]")
        text = self.artifacts.get_text(result.result_ref)
        self.store.complete_node_and_propagate(
            lease.claim_id, worker_id, output_ref=result.result_ref,
            input_snapshot=snapshot,
            condition_context=build_condition_context(text, snapshot),
        )
        return ControlOutcome(
            claim_id=lease.claim_id,
            node_run_id=lease.node_run_id,
            node_id=lease.node_id,
            output_ref=result.result_ref,
        )
