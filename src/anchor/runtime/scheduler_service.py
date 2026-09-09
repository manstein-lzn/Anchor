"""Long-lived interval/cron scheduler process."""
from __future__ import annotations
import asyncio
import logging
from anchor.runtime.scheduler import run_interval_scheduler
from anchor.runtime.settings import AnchorSettings
from anchor.state.relational import RelationalStateStore

def _objective(trigger, now):
    return (f"Scheduled trigger {trigger.id} at {now.isoformat()}", {"trigger_id": str(trigger.id), "scheduled_at": now.isoformat()})

async def _retention_loop(store, settings, stop):
    from anchor.runtime.artifacts import LocalArtifactStore
    from anchor.runtime.retention import enforce_storage_budgets
    artifacts = LocalArtifactStore(settings.artifact_root)
    log = logging.getLogger("anchor.retention")
    while not stop.is_set():
        try:
            result = await asyncio.to_thread(
                enforce_storage_budgets, store, artifacts, trigger="scheduler",
                batch=settings.storage_sweep_batch, max_rounds=settings.storage_sweep_max_rounds)
            if result.get("evicted"):
                log.info("retention sweep evicted %s runs and freed %s bytes",
                         result["evicted"], result["freed_bytes"])
        except Exception:  # never let maintenance kill scheduling
            log.exception("retention sweep failed")
        try:
            await asyncio.wait_for(stop.wait(), timeout=settings.storage_sweep_interval)
        except asyncio.TimeoutError:
            pass


async def serve():
    settings = AnchorSettings()
    url = settings.require_database_url()
    store=RelationalStateStore(url); stop=asyncio.Event()
    tasks = [run_interval_scheduler(store, objective_factory=_objective,
                                    interval=settings.scheduler_interval, stop=stop)]
    if settings.storage_enforce:
        tasks.append(_retention_loop(store, settings, stop))
    try:
        await asyncio.gather(*tasks)
    finally:
        stop.set(); store.close()

def main():
    logging.basicConfig(level=AnchorSettings().log_level)
    try: asyncio.run(serve())
    except KeyboardInterrupt: logging.getLogger("anchor.scheduler").info("scheduler stopped")

if __name__ == "__main__": main()
