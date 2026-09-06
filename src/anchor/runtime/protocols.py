from __future__ import annotations

from typing import Protocol
from uuid import UUID

from pydantic import BaseModel, ConfigDict


class HarnessResult(BaseModel):
    model_config = ConfigDict(extra="forbid")

    phase: str
    output: dict
    complete: bool = False
    requires_approval: bool = False


class GeneralHarness(Protocol):
    async def prepare(self, run_id: UUID) -> None: ...

    async def step(self, run_id: UUID) -> HarnessResult: ...

    async def verify(self, run_id: UUID, result: HarnessResult) -> bool: ...

    async def commit(self, run_id: UUID, result: HarnessResult) -> None: ...


class WorkflowService(Protocol):
    async def start(self, task_id: UUID, graph_version_id: UUID) -> UUID: ...

    async def suspend(self, run_id: UUID, reason: str) -> None: ...

    async def resume(self, run_id: UUID) -> None: ...

    async def cancel(self, run_id: UUID) -> None: ...
