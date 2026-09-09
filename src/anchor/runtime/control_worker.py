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
from anchor.runtime.behaviors import BehaviorRegistry
from anchor.runtime.resolution import resolve_node_context
from anchor.runtime.sinks import ArtifactCheckpointSink
from anchor.runtime.tool_gateway import ToolDenied


class ToolOutcomeUnknown(RuntimeError):
    """A side-effect tool may have executed remotely; reconciliation is required.

    The node deliberately stays running with its lease so the operator can
    reconcile the operation ledger and then apply the resolved outcome.
    """


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
                 tools=None, registry=None, behaviors=None, committer=None) -> None:
        self.store = store
        self.artifacts = artifacts
        self.sink = sink
        self.tools = tools
        self.registry = registry
        self.behaviors = behaviors or BehaviorRegistry()
        self.committer = committer

    def _workspace_output(self, lease, resolved):
        """A control node that declares a workspace publishes its revision."""
        if self.committer is None:
            return None, {}, None
        workspace_id = (resolved.node.metadata or {}).get("workspace_id")
        if not workspace_id:
            return None, {}, None
        node_run = self.store.get_node_run(lease.node_run_id)
        attempt = node_run.attempt if node_run is not None else 0
        prepared = self.committer.prepare(run_id=lease.run_id, node_run_id=lease.node_run_id,
                                          attempt=attempt, workspace_id=workspace_id)
        prepared = self.committer.record_verification(prepared, {"kind": "control", "valid": True})
        return prepared.content_ref, {"workspace_id": workspace_id,
                                      "workspace_revision": prepared.revision,
                                      "manifest_digest": prepared.manifest_digest}, prepared

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
        try:
            budget = resolved.graph.definition.metadata.get("run_timeout_seconds")
            if budget is not None:
                from anchor.domain.models import utc_now
                run = self.store.get_run(lease.run_id)
                if (utc_now() - run.created_at).total_seconds() >= float(budget):
                    raise TimeoutError("Run time budget exhausted")
            return await self.execute_control_once(worker_id=worker_id, lease=lease, resolved=resolved)
        except (ValueError, KeyError, TypeError, TimeoutError) as exc:
            self.store.fail_node_and_propagate(lease.claim_id, worker_id,
                error_code="execution_budget_exceeded" if isinstance(exc, TimeoutError) else "control_input_invalid",
                phase="control", input_snapshot=resolved.snapshot)
            raise

    async def execute_control_once(self, *, worker_id: str, lease, resolved) -> ControlOutcome:
        behavior_ref = (resolved.node.metadata or {}).get("behavior_ref")
        if behavior_ref:
            from anchor.domain.conditions import build_condition_context

            output = self.behaviors.get(behavior_ref).execute_control(
                resolved.snapshot, store=self.store, artifacts=self.artifacts,
                run_id=lease.run_id, node_id=lease.node_id)
            output_ref, event_payload, prepared = self._workspace_output(lease, resolved)
            if isinstance(output, str):
                ref = self.artifacts.put_text(output, media_type="text/markdown")
                self.artifacts.export_markdown(lease.run_id, lease.node_id, output)
                payload = {"response_ref": ref, **event_payload}
                self.store.complete_node_and_propagate(
                    lease.claim_id, worker_id, output_ref=output_ref or ref,
                    input_snapshot=resolved.snapshot,
                    condition_context=build_condition_context(output, resolved.snapshot),
                    event_payload=payload)
                final = output_ref or ref
            else:
                final = await self.sink.persist_control_result(
                    claim_id=lease.claim_id, node_run_id=lease.node_run_id,
                    output=output, input_snapshot=resolved.snapshot,
                    output_ref=output_ref, event_payload=event_payload)
            if prepared is not None:
                self.committer.store.clear_prepared_revision(prepared.node_run_id, prepared.attempt)
            return ControlOutcome(lease.claim_id, lease.node_run_id, lease.node_id, final)
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

    def _approved_for_side_effect(self, *, resolved, run_id) -> bool:
        """A side-effect tool node needs a completed approval/human predecessor."""
        from anchor.domain.graph import NodeType
        sources = {edge.source for edge in resolved.graph.definition.edges
                   if edge.target == resolved.node.id}
        types = {node.id: node.type for node in resolved.graph.definition.nodes}
        runs = self.store.list_node_runs(run_id)
        for source in sources:
            if types.get(source) not in (NodeType.APPROVAL, NodeType.HUMAN_TASK):
                continue
            history = [item for item in runs if item.node_id == source]
            if history and max(history, key=lambda item: item.attempt).status.value == "completed":
                return True
        return False

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
        tool = self.registry.tool(resolved.node.tool_ref or "") if self.registry is not None else None
        approved = bool(tool is not None and tool.side_effect
                        and self._approved_for_side_effect(resolved=resolved, run_id=lease.run_id))
        snapshot = resolved.snapshot if isinstance(resolved.snapshot, dict) else {}
        operation_id = uuid5(lease.claim_id, (resolved.node.tool_ref or "tool") + ":1")
        try:
            result = self.tools.execute(
                lease, agent_ref=owner, tool_ref=resolved.node.tool_ref or "",
                arguments=snapshot, operation_id=operation_id, approved=approved)
        except ToolDenied as exc:
            self.store.fail_node_and_propagate(
                lease.claim_id, worker_id, error_code="tool_denied", phase="tool")
            logger.warning("tool node %s denied [%s]: %s", node_id, exc.code, exc)
            raise ValueError(f"tool node {node_id!r} denied [{exc.code}]") from exc
        if result.status is OperationStatus.OUTCOME_UNKNOWN:
            # Never retry or fail automatically: the remote may have acted. Keep
            # the lease and let the operator reconcile the ledger, then apply.
            logger.warning("tool node %s outcome unknown [%s]; reconciliation required",
                           node_id, result.error_code)
            raise ToolOutcomeUnknown(
                f"tool node {node_id!r} outcome unknown [{result.error_code}]")
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
