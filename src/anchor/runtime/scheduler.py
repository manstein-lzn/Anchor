"""Deterministic interval-trigger scheduler boundary."""
from __future__ import annotations
import asyncio
from datetime import datetime, timezone
from typing import Protocol
from anchor.domain.graph import TriggerType
from anchor.domain.admission import RunRequest
from anchor.state.errors import AdmissionConflict
try:
    from croniter import croniter
except ImportError:  # optional until scheduler is installed
    croniter = None

class TriggerStore(Protocol):
    def list_active_triggers(self) -> list: ...
    def admit_run(self, request: RunRequest): ...

def occurrence_key(trigger_id, slot: int) -> str:
    return f"schedule:{trigger_id}:{slot}"

def due_slot(now: datetime, interval_seconds: int) -> int:
    if interval_seconds <= 0: raise ValueError("interval_seconds must be positive")
    return int(now.astimezone(timezone.utc).timestamp()) // interval_seconds

def cron_slot(now: datetime, expression: str, timezone_name: str = "UTC") -> int:
    from zoneinfo import ZoneInfo
    local = now.astimezone(ZoneInfo(timezone_name)).replace(second=0, microsecond=0)
    if croniter is None:
        # Keep the core scheduler usable without the optional croniter package.
        # Support the common fixed-minute form ("*/N * * * *") deterministically;
        # richer expressions require installing the scheduler extra.
        fields = expression.split()
        if len(fields) == 5 and fields[0].startswith("*/") and all(f in ("*", "?") for f in fields[1:]):
            try:
                step = int(fields[0][2:])
            except ValueError:
                step = 0
            if step > 0:
                minute = (local.minute // step) * step
                return int(local.replace(minute=minute).timestamp())
        raise RuntimeError("install Anchor's scheduler extra to use cron triggers")
    return int(croniter(expression, local).get_prev(datetime).timestamp())

async def run_interval_scheduler(store: TriggerStore, *, objective_factory, interval: float = 1.0,
                                 stop: asyncio.Event | None = None) -> None:
    if interval <= 0: raise ValueError("interval must be positive")
    stop = stop or asyncio.Event(); seen: set[tuple[str, int]] = set()
    while not stop.is_set():
        now = datetime.now(timezone.utc)
        for trigger in store.list_active_triggers():
            if not trigger.enabled or trigger.type not in (TriggerType.INTERVAL, TriggerType.CRON): continue
            slot = due_slot(now, trigger.interval_seconds) if trigger.type is TriggerType.INTERVAL else cron_slot(now, trigger.cron, trigger.timezone)
            identity = (str(trigger.id), slot)
            if identity in seen: continue
            seen.add(identity)
            objective, inputs = objective_factory(trigger, now)
            try:
                store.admit_run(RunRequest(trigger_id=trigger.id, idempotency_key=occurrence_key(trigger.id, slot), objective=objective, inputs=inputs))
            except AdmissionConflict:
                # Admission is idempotent across scheduler processes. A
                # duplicate occurrence is expected; storage/network failures
                # remain visible to the outer supervisor.
                pass
        try: await asyncio.wait_for(stop.wait(), timeout=interval)
        except asyncio.TimeoutError: pass
