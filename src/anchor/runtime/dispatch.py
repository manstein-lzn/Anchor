from typing import Protocol
from uuid import UUID

from anchor.domain.admission import RunDispatch
from anchor.domain.models import Run


class DispatchStore(Protocol):
    def pending_dispatches(self, limit: int = 100) -> list[RunDispatch]: ...

    def acknowledge_dispatch(self, message_id: UUID) -> None: ...

    def accept_dispatch(self, message: RunDispatch) -> Run: ...


class DispatchTarget(Protocol):
    async def accept(self, message: RunDispatch) -> None:
        """Durably accept before returning; deduplicate by message_id/run_id.

        The same message can be delivered again after a crash or lost response.
        Acceptance is not completion, and must not execute an unprotected side effect.
        """
        ...


async def dispatch_pending(store: DispatchStore, target: DispatchTarget, limit: int = 100) -> int:
    """Single-dispatcher pump, at-least-once. Transport failures leave work pending."""
    delivered = 0
    for message in store.pending_dispatches(limit):
        await target.accept(message)
        store.acknowledge_dispatch(message.message_id)
        delivered += 1
    return delivered
