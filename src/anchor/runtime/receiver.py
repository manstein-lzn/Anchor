"""Durable outbox receiver boundary.

The receiver acknowledges a dispatch only after its admission into canonical state.
It deliberately does not call an LLM or execute a tool: execution adapters consume
the running Run from this boundary in a later step.
"""

from __future__ import annotations

import asyncio
import logging
from contextlib import suppress
from uuid import uuid4

from anchor.domain.admission import RunDispatch
from .dispatch import dispatch_pending

logger = logging.getLogger("anchor.receiver")


class DurableExecutionReceiver:
    """Idempotent receiver backed by the same canonical state store."""

    def __init__(self, store) -> None:
        self.store = store

    async def accept(self, message: RunDispatch) -> None:
        self.store.accept_dispatch(message)


async def pump(store, receiver: DurableExecutionReceiver, *, interval: float = 1.0, batch_size: int = 20,
               stop: asyncio.Event | None = None) -> None:
    """Continuously drain pending dispatches with at-least-once delivery."""
    stop = stop or asyncio.Event()
    instance_id = uuid4()
    while not stop.is_set():
        store.record_runtime_heartbeat("execution_receiver", instance_id)
        try:
            delivered = await dispatch_pending(store, receiver, limit=batch_size)
        except Exception:
            logger.exception("dispatch batch failed; message remains pending")
            delivered = 0
        if delivered:
            await asyncio.sleep(0)
            continue
        try:
            await asyncio.wait_for(stop.wait(), timeout=interval)
        except asyncio.TimeoutError:
            pass


def main() -> None:
    from anchor.state.relational import RelationalStateStore

    from anchor.runtime.settings import AnchorSettings

    settings = AnchorSettings()
    logging.basicConfig(level=settings.log_level)
    if not settings.database_url:
        raise SystemExit("set ANCHOR_DATABASE_URL explicitly")
    store = RelationalStateStore(settings.database_url)
    receiver = DurableExecutionReceiver(store)
    try:
        asyncio.run(pump(store, receiver, interval=settings.dispatch_interval))
    except KeyboardInterrupt:
        logger.info("receiver stopped")
    finally:
        with suppress(Exception):
            store.close()


if __name__ == "__main__":
    main()
