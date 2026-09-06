"""Capability-aware worker boundary.

This module deliberately stops before persistence: a caller must provide an
explicit result sink that can atomically checkpoint the model output and advance
the graph. That prevents a model response from being mistaken for a completed
node when the process crashes.
"""

from __future__ import annotations

from dataclasses import dataclass
import asyncio
from typing import Mapping, Protocol
from uuid import UUID, uuid4

from anchor.runtime.capabilities import CapabilityRegistry, CapabilityRegistryError
from anchor.runtime.context import input_hash
from anchor.runtime.model_gateway import ModelGateway, ModelResponse
from anchor.state.protocols import StateStore


class NodeResultSink(Protocol):
    async def persist_model_result(self, *, claim_id: UUID, node_run_id: UUID,
                                   response: ModelResponse, input_hash: str | None = None,
                                   input_snapshot: Mapping[str, object] | None = None) -> None: ...


@dataclass(frozen=True)
class WorkerOutcome:
    claim_id: UUID
    node_run_id: UUID
    response: ModelResponse


class AgentNodeWorker:
    """Execute one claimed Agent node after capability resolution.

    No retry is performed here. A transport exception leaves the lease and
    canonical node state for reconciliation by the supervising runtime.
    """

    def __init__(self, store: StateStore, registry: CapabilityRegistry,
                 gateways: dict[str, ModelGateway], result_sink: NodeResultSink,
                 tool_loop=None) -> None:
        self.store = store
        self.registry = registry
        self.gateways = gateways
        self.result_sink = result_sink
        self.tool_loop = tool_loop

    async def execute_once(self, *, worker_id: str, agent_ref: str, prompt: str,
                           system_prompt: str = "", claim_id: UUID | None = None,
                           expected_node_id: str | None = None,
                           heartbeat_interval: float = 5.0) -> WorkerOutcome | None:
        agent = self.registry.validate_agent(agent_ref)
        gateway = self.gateways.get(agent.model_ref)
        if gateway is None:
            raise RuntimeError(f"no model gateway configured: {agent.model_ref}")
        claim_id = claim_id or uuid4()
        claim = getattr(self.store, "claim_ready_agent_node", self.store.claim_ready_node)
        lease = claim(worker_id, claim_id)
        if lease is None:
            return None
        return await self.execute_claimed_once(worker_id=worker_id, agent_ref=agent_ref,
                                               prompt=prompt, lease=lease,
                                               expected_node_id=expected_node_id,
                                               heartbeat_interval=heartbeat_interval)

    async def execute_claimed_once(self, *, worker_id: str, agent_ref: str, prompt: str,
                                   lease, expected_node_id: str | None = None,
                                   system_prompt: str = "",
                                   input_snapshot: Mapping[str, object] | None = None,
                                   heartbeat_interval: float = 5.0) -> WorkerOutcome:
        """Execute a lease already acquired by the worker loop."""
        agent = self.registry.validate_agent(agent_ref)
        gateway = self.gateways.get(agent.model_ref)
        if gateway is None:
            raise RuntimeError(f"no model gateway configured: {agent.model_ref}")
        # The claim API intentionally selects the oldest ready node. Callers
        # must verify that the resolved Agent capability matches that node
        # before invoking a model; otherwise a misconfigured worker could run
        # the wrong Agent against a valid lease.
        if expected_node_id is not None and lease.node_id != expected_node_id:
            raise CapabilityRegistryError(
                f"claimed node {lease.node_id!r} does not match Agent capability {agent_ref!r}"
            )
        if heartbeat_interval <= 0:
            raise ValueError("heartbeat_interval must be positive")
        async def heartbeat() -> None:
            while True:
                await asyncio.sleep(heartbeat_interval)
                self.store.heartbeat_node_lease(lease.claim_id, worker_id)
        heartbeat_task = asyncio.create_task(heartbeat())
        try:
            if self.tool_loop is not None and agent.tool_refs:
                response = await self.tool_loop.run(
                    gateway, lease=lease, agent=agent, prompt=prompt,
                    system_prompt=system_prompt or agent.instructions)
            else:
                response = await gateway.generate(prompt=prompt, system_prompt=system_prompt or agent.instructions)
            snapshot_hash = input_hash(dict(input_snapshot)) if input_snapshot is not None else None
            result = {"claim_id": lease.claim_id, "node_run_id": lease.node_run_id,
                      "response": response, "input_hash": snapshot_hash}
            if input_snapshot is not None:
                result["input_snapshot"] = input_snapshot
            await self.result_sink.persist_model_result(**result)
        finally:
            heartbeat_task.cancel()
            try:
                await heartbeat_task
            except asyncio.CancelledError:
                pass
        return WorkerOutcome(claim_id=lease.claim_id, node_run_id=lease.node_run_id, response=response)
