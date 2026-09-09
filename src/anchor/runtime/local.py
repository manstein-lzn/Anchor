from __future__ import annotations

from typing import Protocol
from uuid import UUID

from anchor.domain.models import Run, RunStatus
from anchor.runtime.protocols import GeneralHarness, HarnessResult
from anchor.state.protocols import StateStore


class LocalHarness(Protocol):
    async def prepare(self, run_id: UUID) -> None: ...

    async def step(self, run_id: UUID) -> HarnessResult: ...

    async def verify(self, run_id: UUID, result: HarnessResult) -> bool: ...

    async def commit(self, run_id: UUID, result: HarnessResult) -> None: ...


class InProcessWorkflowService:
    """Small deterministic adapter used as the first contract-test runtime."""

    def __init__(self, store: StateStore, harness: GeneralHarness) -> None:
        self.store = store
        self.harness = harness

    async def start(self, task_id: UUID, graph_version_id: UUID) -> UUID:
        graph_version = self.store.get_graph_version(graph_version_id)
        if graph_version is None:
            raise KeyError(graph_version_id)
        run = self.store.create_run(Run(task_id=task_id, graph_version_id=graph_version_id))
        run = self.store.transition_run(
            run_id=run.id,
            expected_revision=run.revision,
            status=RunStatus.RUNNING,
            phase="execute",
            payload={"task_id": str(task_id)},
            idempotency_key=f"run:{run.id}:started",
        )
        await self.harness.prepare(run.id)
        result = await self.harness.step(run.id)
        if result.requires_approval:
            self.store.transition_run(
                run_id=run.id,
                expected_revision=run.revision,
                status=RunStatus.WAITING_APPROVAL,
                phase=result.phase,
                payload={"reason": "approval_required"},
                idempotency_key=f"run:{run.id}:approval",
            )
            return run.id

        if not await self.harness.verify(run.id, result):
            self.store.transition_run(
                run_id=run.id,
                expected_revision=run.revision,
                status=RunStatus.FAILED,
                phase="verify",
                payload={"reason": "verification_failed"},
                idempotency_key=f"run:{run.id}:failed",
            )
            return run.id

        await self.harness.commit(run.id, result)
        latest = self.store.get_run(run.id)
        assert latest is not None
        self.store.transition_run(
            run_id=run.id,
            expected_revision=latest.revision,
            status=RunStatus.COMPLETED,
            phase="complete",
            payload={"output": result.output},
            idempotency_key=f"run:{run.id}:completed",
        )
        return run.id


class DeterministicHarness:
    """A fake harness for proving runtime semantics without an LLM."""

    async def prepare(self, run_id: UUID) -> None:
        return None

    async def step(self, run_id: UUID) -> HarnessResult:
        return HarnessResult(phase="execute", output={"run_id": str(run_id)}, complete=True)

    async def verify(self, run_id: UUID, result: HarnessResult) -> bool:
        return result.complete and bool(result.output)

    async def commit(self, run_id: UUID, result: HarnessResult) -> None:
        return None
