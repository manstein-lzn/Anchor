"""Long-lived worker loop with explicit liveness and failure semantics."""
from __future__ import annotations
import asyncio
import logging
from collections.abc import Awaitable, Callable
from dataclasses import dataclass, field
from uuid import UUID, uuid4
from anchor.runtime.node_prompt import PromptSegments
from anchor.runtime.worker import AgentNodeWorker

logger = logging.getLogger("anchor.worker")


@dataclass(frozen=True)
class ResolvedPrompt:
    """Everything the loop hands a worker about one node attempt.

    A dataclass rather than a widening tuple: the resolver now returns five things, and
    ``(agent_ref, prompt, node_id, snapshot)`` read by position was already at the point where
    a sixth field would have been a silent bug everywhere.
    """

    agent_ref: str
    prompt: str
    expected_node_id: str
    input_snapshot: dict = field(default_factory=dict)
    #: The parts the prompt was built from, for the spend report. `None` when a caller
    #: assembled its own prompt, which the report says out loud rather than assuming zero.
    segments: PromptSegments | None = None


PromptFactory = Callable[[UUID, str], Awaitable[ResolvedPrompt]]

async def run_worker_loop(worker: AgentNodeWorker, *, worker_id: str,
                          resolve_prompt: PromptFactory, interval: float = 1.0,
                          stop: asyncio.Event | None = None) -> None:
    if not worker_id or interval <= 0:
        raise ValueError("worker_id and positive interval are required")
    stop = stop or asyncio.Event()
    instance_id = uuid4()
    while not stop.is_set():
        claim_id = uuid4()
        try:
            worker.store.record_runtime_heartbeat("agent_worker", instance_id)
            claim = getattr(worker.store, "claim_ready_agent_node", worker.store.claim_ready_node)
            lease = claim(worker_id, claim_id)
            if lease is None:
                try: await asyncio.wait_for(stop.wait(), timeout=interval)
                except asyncio.TimeoutError: pass
                continue
            resolved_prompt = await resolve_prompt(lease.run_id, lease.node_id)
            worker.store.heartbeat_node_lease(claim_id, worker_id)
            await worker.execute_claimed_once(
                worker_id=worker_id, agent_ref=resolved_prompt.agent_ref,
                prompt=resolved_prompt.prompt, lease=lease,
                expected_node_id=resolved_prompt.expected_node_id,
                input_snapshot=resolved_prompt.input_snapshot,
                prompt_segments=resolved_prompt.segments)
        except asyncio.CancelledError:
            # A provider/runtime cancellation belongs to this iteration. Do
            # not take down the long-lived worker; supervision can reconcile
            # the released or stale lease and the next iteration can claim
            # other ready nodes. Only an explicit loop stop cancels the
            # service itself.
            if stop.is_set():
                raise
            logger.exception("worker iteration cancelled; continuing loop")
            await asyncio.sleep(0)
        except Exception:
            logger.exception("worker iteration failed; lease requires supervision")
            await asyncio.sleep(0)
