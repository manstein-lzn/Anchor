from typing import Protocol
from uuid import UUID

import logging

from anchor.domain.admission import RunDispatch
from anchor.domain.models import Run


logger = logging.getLogger("anchor.dispatch")


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
    """Single-dispatcher pump, at-least-once. Transport failures leave work pending.

    Each message is attempted independently. One that cannot be accepted stays pending and
    is retried on the next pass, but it no longer aborts the batch: previously a single
    message that could not be accepted stalled every dispatch queued behind it, forever,
    and only a log line recorded it — a whole run could wait on one bad row with nothing
    durable saying so.
    """
    delivered = 0
    for message in store.pending_dispatches(limit):
        try:
            await target.accept(message)
        except Exception as exc:  # noqa: BLE001 - reported per message, never fatal
            _record_dispatch_failure(store, message, exc)
            continue
        store.acknowledge_dispatch(message.message_id)
        delivered += 1
    return delivered


def _record_dispatch_failure(store: DispatchStore, message: RunDispatch,
                             exc: Exception) -> None:
    """Leave a durable trace, not only a log line.

    The event is keyed by the stream the dispatch names rather than by a run row, because
    the run usually does not exist yet — that is often what failed. The events table is not
    constrained to existing runs for exactly this reason, and the fixed idempotency key means
    a message that keeps failing records the fact once instead of once per retry.
    """
    logger.error("dispatch %s for run %s failed: %s: %s", message.message_id,
                 message.run_id, type(exc).__name__, exc)
    append = getattr(store, "append_event", None)
    if not callable(append):
        return
    try:
        append(stream_id=message.run_id, event_type="run.dispatch_failed",
               payload={"message_id": str(message.message_id),
                        "task_id": str(message.task_id),
                        "graph_version_id": str(message.graph_version_id),
                        "error_class": type(exc).__name__,
                        "error": str(exc)[:400]},
               idempotency_key=f"dispatch-failed:{message.message_id}")
    except Exception:  # noqa: BLE001 - reporting must not stop the pump
        logger.exception("could not record the dispatch failure for %s", message.message_id)
